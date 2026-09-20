//! Structured judgments mapped to the same Assessment as conversational backends.
pub(crate) mod client;
use super::{Backend, ReviewFuture};
use crate::{
    audit,
    config::{JevInstructions, ReviewerConfig},
    reviewer::{Assessment, ReviewInput},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

const RISK: &[&str] = &["low", "medium", "high", "critical", "unknown"];
const AUTH: &[&str] = &["high", "medium", "low", "unknown"];
const POLICY: &[&str] = &["permitted", "prohibited", "needs_confirmation", "unknown"];

pub(crate) fn questions(custom: &JevInstructions) -> Value {
    let mut questions: Value =
        serde_json::from_str(include_str!("questions.json")).expect("built-in questions");
    for (name, replacement) in [
        ("risk", &custom.risk),
        ("authorization", &custom.authorization),
        ("policy", &custom.policy),
    ] {
        if let Some(value) = replacement {
            questions[name]["instructions"] = json!(value);
        }
    }
    questions
}
#[derive(Deserialize, Serialize)]
struct Choice {
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
}
impl Choice {
    fn validate(&self, options: &[&str]) -> Result<()> {
        ensure!(
            self.kind == "choice" && options.contains(&self.choice.as_str()),
            "Invalid Jev answer type or choice"
        );
        ensure!(
            self.probabilities.len() == options.len()
                && options.iter().all(|o| self.probabilities.contains_key(*o)),
            "Invalid Jev probability options"
        );
        ensure!(
            self.confidence.is_finite()
                && (0.0..=1.0).contains(&self.confidence)
                && self
                    .probabilities
                    .values()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "Invalid Jev probability or confidence"
        );
        ensure!(
            (self.probabilities.values().sum::<f64>() - 1.0).abs() <= 1e-3,
            "Jev probabilities do not sum to one"
        );
        let selected = self.p(&self.choice);
        ensure!(
            self.probabilities.values().all(|v| *v <= selected + 1e-9),
            "Jev choice does not match maximum probability"
        );
        Ok(())
    }
    fn p(&self, key: &str) -> f64 {
        self.probabilities[key]
    }
}
#[derive(Deserialize, Serialize)]
struct Answers {
    risk: Choice,
    authorization: Choice,
    policy: Choice,
}
#[derive(Deserialize)]
struct Response {
    model: String,
    answers: Answers,
    #[serde(default)]
    usage: Value,
}
fn parse(value: Value) -> Result<Response> {
    // Do not include serde errors containing arbitrary server-controlled values in logs.
    let response: Response = serde_json::from_value(value)
        .map_err(|_| anyhow::anyhow!("Invalid Jev response schema"))?;
    ensure!(
        !response.model.trim().is_empty() && response.model.len() <= 256,
        "Invalid Jev response model"
    );
    response.answers.risk.validate(RISK)?;
    response.answers.authorization.validate(AUTH)?;
    response.answers.policy.validate(POLICY)?;
    Ok(response)
}
fn decision(response: &Response, input: &ReviewInput, threshold: f64) -> Assessment {
    let a = &response.answers;
    let (outcome, rule) = if (a.policy.choice == "prohibited"
        && a.policy.p("prohibited") >= threshold)
        || (a.risk.choice == "critical" && a.risk.p("critical") >= threshold)
    {
        ("deny", "prohibited_or_critical")
    } else if !input.complete() {
        ("deny", "incomplete_evidence")
    } else if a.risk.choice == "low"
        && a.risk.p("low") >= threshold
        && matches!(a.authorization.choice.as_str(), "high" | "medium")
        && a.authorization.p("high") + a.authorization.p("medium") >= threshold
        && a.policy.choice == "permitted"
        && a.policy.p("permitted") >= threshold
    {
        ("allow", "approval_thresholds_met")
    } else {
        ("deny", "confirmation_or_uncertainty")
    };
    Assessment {
        outcome: outcome.into(),
        risk_level: a.risk.choice.clone(),
        user_authorization: a.authorization.choice.clone(),
        rationale: format!(
            "Jev: {outcome}; rule={rule}; risk={}, authorization={}, policy={}, probability threshold={threshold}.",
            a.risk.choice, a.authorization.choice, a.policy.choice
        ),
        error_stage: None,
        reviewer: Some(json!({"model":response.model, "answers":a,
            "probability_threshold":threshold,"rule_id":rule,"rubric_version":"jev-review-v1","decision_policy_version":"jev-decision-v2"})),
    }
}

pub struct JevBackend {
    config: ReviewerConfig,
    client: client::JevClient,
}
impl JevBackend {
    pub fn new(config: ReviewerConfig) -> Result<Self> {
        Ok(Self {
            client: client::JevClient::new(&config.approver.base_url)?,
            config,
        })
    }
    async fn evaluate(&self, input: &ReviewInput) -> Result<Assessment> {
        let key = &self.config.approver.api_key;
        ensure!(!key.trim().is_empty(), "Jev API key is empty");
        let questions = questions(&self.config.approver.instructions);
        let body =
            json!({"model":self.config.approver.model,"state":input.state,"questions":questions});
        ensure!(
            serde_json::to_vec(&body)?.len() <= 24 * 1024,
            "Jev request exceeds 24 KiB evidence budget"
        );
        let started = Instant::now();
        let response = parse(self.client.evaluate(key, &body, &input.request_id).await?)?;
        let tokens = match (
            response.usage["input_tokens"].as_u64(),
            response.usage["output_tokens"].as_u64(),
        ) {
            (Some(i), Some(o)) => json!({"input_tokens":i,"output_tokens":o}),
            _ => Value::Null,
        };
        audit::record(
            &input.request_id,
            "backend_response",
            json!({"provider":"jev","model":response.model,
            "usage_delta":tokens,"usage_valid":!tokens.is_null(),"exit_code":0,"duration_ms":started.elapsed().as_millis()}),
        );
        let mut result = decision(&response, input, self.config.approver.probability_threshold);
        use sha2::{Digest, Sha256};
        result.reviewer.as_mut().unwrap()["rubric_hash"] = json!(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&questions)?)
        ));
        Ok(result)
    }
}
impl Backend for JevBackend {
    fn review<'a>(&'a mut self, input: &'a ReviewInput) -> ReviewFuture<'a> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self.evaluate(input).await;
            if let Err(error) = &result {
                audit::record(
                    &input.request_id,
                    "backend_error",
                    json!({"provider":"jev","error":error.to_string(),"duration_ms":started.elapsed().as_millis()}),
                );
            }
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn choice(selected: &str, probabilities: Value) -> Value {
        json!({"type":"choice","choice":selected,"probabilities":probabilities,"confidence":0.1})
    }
    fn response() -> Value {
        json!({"model":"jev-fixture","answers":{
            "risk":choice("low",json!({"low":0.9,"medium":0.05,"high":0.03,"critical":0.01,"unknown":0.01})),
            "authorization":choice("medium",json!({"high":0.4,"medium":0.5,"low":0.05,"unknown":0.05})),
            "policy":choice("permitted",json!({"permitted":0.9,"prohibited":0.03,"needs_confirmation":0.03,"unknown":0.04}))
        },"usage":{"input_tokens":20,"output_tokens":3}})
    }
    fn input() -> ReviewInput {
        ReviewInput::from_request(
            &json!({"toolCall":{"name":"write","args":{}},"authorization":{
            "availability":"available","latest_user_message":{"id":"1","role":"user","text":"Fix the bug","source":"current_branch"},"relevant_prior_messages":[]}}),
        )
    }
    #[test]
    fn threshold_is_configurable_and_confidence_is_not_a_hidden_gate() {
        let response = parse(response()).unwrap();
        let input = input();
        assert_eq!(decision(&response, &input, 0.9).outcome, "allow");
        assert_eq!(decision(&response, &input, 0.91).outcome, "deny");
        assert_eq!(decision(&response, &input, 0.0).outcome, "allow");
        assert_eq!(decision(&response, &input, 1.0).outcome, "deny");
    }
    #[test]
    fn unavailable_evidence_and_unknown_results_cannot_allow_even_at_zero() {
        let mut input = input();
        input.state["completeness"]["authorization"] = json!("unavailable");
        assert_eq!(
            decision(&parse(response()).unwrap(), &input, 0.0).outcome,
            "deny"
        );
        for (question, selected) in [
            ("risk", "unknown"),
            ("authorization", "unknown"),
            ("policy", "needs_confirmation"),
        ] {
            let mut value = response();
            let probabilities = value["answers"][question]["probabilities"]
                .as_object_mut()
                .unwrap();
            for (key, v) in probabilities {
                *v = json!(if key == selected { 1.0 } else { 0.0 });
            }
            value["answers"][question]["choice"] = json!(selected);
            assert_eq!(
                decision(&parse(value).unwrap(), &self::input(), 0.0).outcome,
                "deny"
            );
        }
    }
    #[test]
    fn prohibited_decision_precedes_missing_evidence() {
        let mut value = response();
        value["answers"]["policy"] = choice(
            "prohibited",
            json!({"permitted":0.0,"prohibited":1.0,"needs_confirmation":0.0,"unknown":0.0}),
        );
        let mut input = input();
        input.state["completeness"]["authorization"] = json!("unavailable");
        assert_eq!(
            decision(&parse(value).unwrap(), &input, 0.9).outcome,
            "deny"
        );
    }
    #[test]
    fn malformed_answers_fail_strict_validation() {
        for (pointer, bad) in [
            ("/answers/risk/type", json!("noul")),
            ("/answers/risk/choice", json!("allow")),
            ("/answers/risk/choice", json!("high")),
            ("/answers/risk/probabilities/low", json!(1.2)),
            ("/answers/risk/probabilities/low", json!(0.8)),
            ("/answers/risk/probabilities/low", Value::Null),
            ("/answers/risk/confidence", json!(-0.1)),
            ("/answers/policy", Value::Null),
            ("/model", json!("")),
        ] {
            let mut value = response();
            *value.pointer_mut(pointer).unwrap() = bad;
            assert!(parse(value).is_err(), "{pointer}");
        }
        let mut value = response();
        value["answers"]["risk"]["probabilities"]["surprise"] = json!(0.0);
        assert!(parse(value).is_err());
        let mut value = response();
        value.as_object_mut().unwrap().remove("usage");
        value["extra_metadata"] = json!(true);
        assert!(parse(value).is_ok());
    }
    #[test]
    fn custom_instructions_replace_only_the_named_instruction() {
        let original = questions(&JevInstructions::default());
        let custom = questions(&JevInstructions {
            risk: Some("Custom risk instruction".into()),
            ..Default::default()
        });
        assert_eq!(custom["risk"]["instructions"], "Custom risk instruction");
        assert_eq!(custom["risk"]["criteria"], original["risk"]["criteria"]);
        assert_eq!(custom["policy"], original["policy"]);
    }
}
