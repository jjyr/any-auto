//! Shared classification contract and deterministic approval policy.
use crate::reviewer::{Assessment, ReviewInput};
use anyhow::Result;
use serde_json::{Value, json};
mod types;
pub use types::*;

pub const RUBRIC_VERSION: &str = "review-v6";
pub const DECISION_POLICY_VERSION: &str = "policy-decision-v6";

/// Transport metadata is separate from the model's typed classification schema.
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

fn approval_checks(a: &Judgment, input: &ReviewInput, threshold: f64) -> ApprovalChecks {
    let p = a.probabilities.as_ref();
    let passes = |value: Option<f64>| value.is_none_or(|v| v >= threshold);
    let routine_probability = p.map(|p| p.risk.sum(&[Risk::Low, Risk::Medium]));
    let authorization_required =
        !(matches!(a.risk, Risk::Low | Risk::Medium) && passes(routine_probability));
    let risk_choices = if authorization_required {
        vec![Risk::Low, Risk::Medium, Risk::High]
    } else {
        vec![Risk::Low, Risk::Medium]
    };
    let auth_choices = if authorization_required {
        vec![Authorization::High, Authorization::Medium]
    } else {
        Authorization::ALL.to_vec()
    };
    let risk_probability = p.map(|p| p.risk.sum(&risk_choices));
    let auth_probability = p.map(|p| p.authorization.sum(&auth_choices));
    let policy_probability = p.map(|p| p.policy.permitted);
    let threshold = p.map(|_| threshold);
    ApprovalChecks {
        evidence: EvidenceCheck {
            passed: input.complete(),
        },
        risk: ClassificationCheck {
            passed: risk_choices.contains(&a.risk) && passes(risk_probability),
            accepted_choices: risk_choices,
            choice: a.risk,
            probability: risk_probability,
            threshold,
        },
        authorization: AuthorizationCheck {
            required: authorization_required,
            classification: ClassificationCheck {
                passed: !authorization_required
                    || (auth_choices.contains(&a.authorization) && passes(auth_probability)),
                accepted_choices: auth_choices,
                choice: a.authorization,
                probability: auth_probability,
                threshold,
            },
        },
        policy: ClassificationCheck {
            passed: a.policy == Policy::Permitted && passes(policy_probability),
            accepted_choices: vec![Policy::Permitted],
            choice: a.policy,
            probability: policy_probability,
            threshold,
        },
    }
}

fn describe_check<C: std::fmt::Display + serde::Serialize>(
    kind: CheckKind,
    check: &ClassificationCheck<C>,
) -> String {
    let choices = serde_json::to_string(&check.accepted_choices).expect("classification choices");
    if let (Some(probability), Some(threshold)) = (check.probability, check.threshold) {
        format!(
            "{kind}(choice={}, probability={probability:.4}, required>={threshold}, accepted_choices={choices})",
            check.choice
        )
    } else {
        format!(
            "{kind}(choice={}, accepted_choices={choices})",
            check.choice
        )
    }
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
        (Outcome::Deny, RuleId::ProhibitedOrCritical)
    } else if !checks.evidence.passed {
        (Outcome::Deny, RuleId::IncompleteEvidence)
    } else if checks.passed() {
        (Outcome::Allow, RuleId::ApprovalThresholdsMet)
    } else {
        (Outcome::Deny, RuleId::ConfirmationOrUncertainty)
    };
    let failed = checks.failed();
    let failures = failed
        .iter()
        .map(|kind| match kind {
            CheckKind::Evidence => "evidence incomplete".to_owned(),
            CheckKind::Risk => describe_check(*kind, &checks.risk),
            CheckKind::Authorization => describe_check(*kind, &checks.authorization.classification),
            CheckKind::Policy => describe_check(*kind, &checks.policy),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut metadata = review.metadata;
    if !metadata.is_object() {
        metadata = json!({});
    }
    metadata["judgment"] = json!(a);
    metadata["probability_threshold"] = json!(a.probabilities.as_ref().map(|_| threshold));
    metadata["approval_checks"] = json!(checks);
    metadata["failed_checks"] = json!(failed);
    metadata["rule_id"] = json!(rule);
    metadata["decision_policy_version"] = json!(DECISION_POLICY_VERSION);
    if metadata["rubric_version"].is_null() {
        metadata["rubric_version"] = json!(RUBRIC_VERSION);
    }
    Assessment {
        outcome,
        risk_level: a.risk,
        user_authorization: a.authorization,
        rationale: format!(
            "Policy: {outcome}; rule={rule}; risk={}, authorization={}, policy={}; failed_checks=[{failures}]. {}",
            a.risk, a.authorization, a.policy, a.rationale
        ),
        error_stage: None,
        reviewer: Some(metadata),
    }
}

#[cfg(test)]
mod tests;
