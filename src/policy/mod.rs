//! Shared classification contract and deterministic approval policy.
use crate::reviewer::{Assessment, ReviewInput};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const RUBRIC_VERSION: &str = "review-v6";
pub const DECISION_POLICY_VERSION: &str = "policy-decision-v6";
pub const RISK: &[&str] = &["low", "medium", "high", "critical", "unknown"];
pub const AUTH: &[&str] = &["high", "medium", "low", "unknown"];
pub const POLICY: &[&str] = &["permitted", "prohibited", "needs_confirmation", "unknown"];

macro_rules! classification {
    ($name:ident { $($variant:ident => $wire:literal),+ }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $wire)] $variant),+ }
        impl $name {
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $wire),+ } }
        }
    };
}
classification!(Risk { Low => "low", Medium => "medium", High => "high", Critical => "critical", Unknown => "unknown" });
classification!(Authorization { High => "high", Medium => "medium", Low => "low", Unknown => "unknown" });
classification!(Policy { Permitted => "permitted", Prohibited => "prohibited", NeedsConfirmation => "needs_confirmation", Unknown => "unknown" });

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probabilities {
    pub risk: BTreeMap<String, f64>,
    pub authorization: BTreeMap<String, f64>,
    pub policy: BTreeMap<String, f64>,
}

/// Model judgments are classifications, never execution decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judgment {
    pub risk: Risk,
    pub authorization: Authorization,
    pub policy: Policy,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probabilities: Option<Probabilities>,
}
impl Judgment {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.rationale.trim().is_empty(),
            "Missing judgment rationale"
        );
        if let Some(p) = &self.probabilities {
            for (values, options, selected) in [
                (&p.risk, RISK, self.risk.as_str()),
                (&p.authorization, AUTH, self.authorization.as_str()),
                (&p.policy, POLICY, self.policy.as_str()),
            ] {
                ensure!(
                    values.len() == options.len()
                        && options.iter().all(|key| values.contains_key(*key)),
                    "Invalid probability options"
                );
                ensure!(
                    values
                        .values()
                        .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                    "Invalid probability value"
                );
                ensure!(
                    values.values().all(|v| *v <= values[selected] + 1e-9),
                    "Choice does not match maximum probability"
                );
                // Official SDK semantics: preserve approximate sums without normalization.
            }
        }
        Ok(())
    }
}

/// Transport metadata is separate from the model's strict JSON schema.
pub struct Review {
    pub judgment: Judgment,
    pub metadata: Value,
}

/// Tolerate JSON fences/prose, but never infer missing classifications from allow/deny.
pub fn parse(raw: &str) -> Result<Judgment> {
    let raw = raw.trim();
    let mut data = serde_json::from_str::<Value>(raw).ok();
    if data.is_none() {
        let fence = regex::Regex::new(r"(?s)```(?:json)?\s*(.*?)\s*```").unwrap();
        if let Some(c) = fence.captures(raw) {
            data = serde_json::from_str(c[1].trim()).ok();
        }
    }
    if data.is_none()
        && let (Some(a), Some(b)) = (raw.find('{'), raw.rfind('}'))
        && a < b
    {
        data = serde_json::from_str(&raw[a..=b]).ok();
    }
    let judgment: Judgment = serde_json::from_value(data.unwrap_or(Value::Null)).map_err(|_| {
        anyhow::anyhow!(
            "Invalid structured judgment: risk, authorization, policy and rationale are required"
        )
    })?;
    judgment.validate()?;
    Ok(judgment)
}

fn approval_checks(a: &Judgment, input: &ReviewInput, threshold: f64) -> Value {
    let p = a.probabilities.as_ref();
    let sum = |values: &BTreeMap<String, f64>, keys: &[&str]| {
        keys.iter().map(|k| values[*k]).sum::<f64>()
    };
    let passes = |probability: Option<f64>| probability.is_none_or(|v| v >= threshold);
    let routine_probability = p.map(|p| sum(&p.risk, &["low", "medium"]));
    let routine_allowed = matches!(a.risk, Risk::Low | Risk::Medium) && passes(routine_probability);
    let authorization_required = !routine_allowed;
    let risk_choices = if authorization_required {
        vec!["low", "medium", "high"]
    } else {
        vec!["low", "medium"]
    };
    let auth_choices = if authorization_required {
        vec!["high", "medium"]
    } else {
        vec!["high", "medium", "low", "unknown"]
    };
    let risk_probability = p.map(|p| sum(&p.risk, &risk_choices));
    let auth_probability = p.map(|p| sum(&p.authorization, &auth_choices));
    let policy_probability = p.map(|p| p.policy["permitted"]);
    let threshold = p.map(|_| threshold);
    json!({
        "evidence":{"passed":input.complete()},
        "risk":{"passed":risk_choices.contains(&a.risk.as_str()) && passes(risk_probability),
            "accepted_choices":risk_choices,"choice":a.risk,"probability":risk_probability,"threshold":threshold},
        "authorization":{"passed":!authorization_required || (auth_choices.contains(&a.authorization.as_str()) && passes(auth_probability)),
            "required":authorization_required,"accepted_choices":auth_choices,"choice":a.authorization,"probability":auth_probability,"threshold":threshold},
        "policy":{"passed":a.policy == Policy::Permitted && passes(policy_probability),
            "accepted_choices":["permitted"],"choice":a.policy,"probability":policy_probability,"threshold":threshold}
    })
}

/// All reviewed backends pass through this function; optional probabilities add a gate.
pub fn decide(review: Review, input: &ReviewInput, threshold: f64) -> Assessment {
    let a = review.judgment;
    if let Err(error) = a.validate() {
        return Assessment::deny(error);
    }
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Assessment::deny("Invalid probability threshold");
    }
    let checks = approval_checks(&a, input, threshold);
    let (outcome, rule) = if a.policy == Policy::Prohibited || a.risk == Risk::Critical {
        ("deny", "prohibited_or_critical")
    } else if !input.complete() {
        ("deny", "incomplete_evidence")
    } else if ["risk", "authorization", "policy"]
        .iter()
        .all(|key| checks[*key]["passed"] == true)
    {
        ("allow", "approval_thresholds_met")
    } else {
        ("deny", "confirmation_or_uncertainty")
    };
    let failed: Vec<_> = ["evidence", "risk", "authorization", "policy"]
        .into_iter()
        .filter(|key| checks[*key]["passed"] == false)
        .collect();
    let failures = failed.iter().map(|key| {
        let check = &checks[*key];
        if *key == "evidence" { "evidence incomplete".to_owned() }
        else if let Some(probability) = check["probability"].as_f64() {
            format!("{key}(choice={}, probability={probability:.4}, required>={threshold}, accepted_choices={})",
                check["choice"].as_str().unwrap(), check["accepted_choices"])
        } else { format!("{key}(choice={}, accepted_choices={})", check["choice"].as_str().unwrap(), check["accepted_choices"]) }
    }).collect::<Vec<_>>().join(", ");
    let mut metadata = review.metadata;
    if !metadata.is_object() {
        metadata = json!({});
    }
    metadata["judgment"] = json!(a);
    metadata["probability_threshold"] = json!(a.probabilities.as_ref().map(|_| threshold));
    metadata["approval_checks"] = checks;
    metadata["failed_checks"] = json!(failed);
    metadata["rule_id"] = json!(rule);
    metadata["decision_policy_version"] = json!(DECISION_POLICY_VERSION);
    if metadata["rubric_version"].is_null() {
        metadata["rubric_version"] = json!(RUBRIC_VERSION);
    }
    Assessment {
        outcome: outcome.into(),
        risk_level: a.risk.as_str().into(),
        user_authorization: a.authorization.as_str().into(),
        rationale: format!(
            "Policy: {outcome}; rule={rule}; risk={}, authorization={}, policy={}; failed_checks=[{failures}]. {}",
            a.risk.as_str(),
            a.authorization.as_str(),
            a.policy.as_str(),
            a.rationale
        ),
        error_stage: None,
        reviewer: Some(metadata),
    }
}

#[cfg(test)]
mod tests;
