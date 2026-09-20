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

pub(crate) const RUBRIC_VERSION: &str = "jev-review-v4";
pub(crate) const DECISION_POLICY_VERSION: &str = "jev-decision-v3";

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
fn approval_checks(a: &Answers, input: &ReviewInput, threshold: f64) -> Value {
    // The selected risk class determines the authorization requirement. A weak
    // low-risk judgment does not silently fall back to the medium-risk branch.
    let medium_risk = a.risk.choice == "medium";
    let risk_choices = if medium_risk {
        vec!["low", "medium"]
    } else {
        vec!["low"]
    };
    let auth_choices = if medium_risk {
        vec!["high"]
    } else {
        vec!["high", "medium"]
    };
    let risk_probability = risk_choices.iter().map(|key| a.risk.p(key)).sum::<f64>();
    let auth_probability = auth_choices
        .iter()
        .map(|key| a.authorization.p(key))
        .sum::<f64>();
    json!({
        "evidence": {"passed": input.complete()},
        "risk": {"passed": risk_choices.contains(&a.risk.choice.as_str()) && risk_probability >= threshold,
            "accepted_choices":risk_choices, "choice": a.risk.choice, "probability": risk_probability, "threshold": threshold},
        "authorization": {"passed": auth_choices.contains(&a.authorization.choice.as_str()) && auth_probability >= threshold,
            "accepted_choices":auth_choices, "choice": a.authorization.choice,
            "probability": auth_probability, "threshold": threshold},
        "policy": {"passed": a.policy.choice == "permitted" && a.policy.p("permitted") >= threshold,
            "accepted_choices":["permitted"], "choice": a.policy.choice, "probability": a.policy.p("permitted"), "threshold": threshold}
    })
}
fn decision(response: &Response, input: &ReviewInput, threshold: f64) -> Assessment {
    let a = &response.answers;
    let checks = approval_checks(a, input, threshold);
    let (outcome, rule) = if (a.policy.choice == "prohibited"
        && a.policy.p("prohibited") >= threshold)
        || (a.risk.choice == "critical" && a.risk.p("critical") >= threshold)
    {
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
    let failures = failed
        .iter()
        .map(|key| {
            let check = &checks[*key];
            if *key == "evidence" {
                "evidence incomplete".to_owned()
            } else {
                format!(
                    "{key}(choice={}, probability={:.4}, required>={threshold}, accepted_choices={})",
                    check["choice"].as_str().unwrap(),
                    check["probability"].as_f64().unwrap(),
                    check["accepted_choices"]
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Assessment {
        outcome: outcome.into(),
        risk_level: a.risk.choice.clone(),
        user_authorization: a.authorization.choice.clone(),
        rationale: format!(
            "Jev: {outcome}; rule={rule}; risk={}, authorization={}, policy={}, probability threshold={threshold}; failed_checks=[{failures}].",
            a.risk.choice, a.authorization.choice, a.policy.choice
        ),
        error_stage: None,
        reviewer: Some(json!({"model":response.model, "answers":a,
            "probability_threshold":threshold,"approval_checks":checks,"failed_checks":failed,"rule_id":rule,"rubric_version":RUBRIC_VERSION,"decision_policy_version":DECISION_POLICY_VERSION})),
    }
}

pub struct JevBackend {
    config: ReviewerConfig,
    client: client::JevClient,
    evaluation_questions: Option<Value>,
}
impl JevBackend {
    pub fn new(config: ReviewerConfig) -> Result<Self> {
        Ok(Self {
            client: client::JevClient::new(&config.approver.base_url)?,
            evaluation_questions: None,
            config,
        })
    }
    pub(crate) fn for_evaluation(
        config: ReviewerConfig,
        questions: Value,
        retries: u32,
    ) -> Result<Self> {
        let mut backend = Self::new(config)?;
        backend.evaluation_questions = Some(questions);
        backend.client.retries = retries;
        backend.client.collect_usage = true;
        Ok(backend)
    }
    async fn evaluate(&self, input: &ReviewInput) -> Result<Assessment> {
        let key = &self.config.approver.api_key;
        ensure!(!key.trim().is_empty(), "Jev API key is empty");
        let questions = self
            .evaluation_questions
            .clone()
            .unwrap_or_else(|| questions(&self.config.approver.instructions));
        let body =
            json!({"model":self.config.approver.model,"state":input.state,"questions":questions});
        ensure!(
            serde_json::to_vec(&body)?.len() <= 24 * 1024,
            "Jev request exceeds 24 KiB evidence budget"
        );
        audit::record(
            &input.request_id,
            "jev_rubric",
            json!({"questions":questions}),
        );
        let snapshot = self.config.approver.diagnostic_snapshot;
        if snapshot {
            audit::record(
                &input.request_id,
                "jev_diagnostic_request",
                json!({"body":body}),
            );
        }
        let started = Instant::now();
        let response = parse(self.client.evaluate(key, &body, &input.request_id).await?)?;
        if snapshot {
            audit::record(
                &input.request_id,
                "jev_diagnostic_response",
                json!({"model":response.model,"answers":response.answers}),
            );
        }
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
        if self.evaluation_questions.is_some() {
            result.reviewer.as_mut().unwrap()["rubric_version"] = json!("evaluation");
        }
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
    fn diagnostics_identify_authorization_threshold_and_choice_failures() {
        let mut value = response();
        value["answers"]["authorization"] = choice(
            "medium",
            json!({"high":0.03,"medium":0.63,"low":0.27,"unknown":0.07}),
        );
        let assessment = decision(&parse(value.clone()).unwrap(), &input(), 0.85);
        assert_eq!(assessment.outcome, "deny");
        let metadata = assessment.reviewer.unwrap();
        assert_eq!(metadata["failed_checks"], json!(["authorization"]));
        assert_eq!(
            metadata["approval_checks"]["authorization"]["probability"],
            0.66
        );
        assert!(
            assessment
                .rationale
                .contains("probability=0.6600, required>=0.85")
        );
        value["answers"]["authorization"] = choice(
            "low",
            json!({"high":0.03,"medium":0.31,"low":0.56,"unknown":0.10}),
        );
        let assessment = decision(&parse(value).unwrap(), &input(), 0.0);
        assert_eq!(assessment.outcome, "deny");
        assert_eq!(
            assessment.reviewer.unwrap()["failed_checks"],
            json!(["authorization"])
        );
        assert!(assessment.rationale.contains("choice=low"));
    }
    fn medium_risk_response() -> Value {
        let mut value = response();
        value["answers"]["risk"] = choice(
            "medium",
            json!({"low":0.25,"medium":0.60,"high":0.10,"critical":0.03,"unknown":0.02}),
        );
        value["answers"]["authorization"] = choice(
            "high",
            json!({"high":0.85,"medium":0.10,"low":0.03,"unknown":0.02}),
        );
        value
    }
    #[test]
    fn medium_risk_requires_explicit_authorization_and_reports_actual_gates() {
        let response = parse(medium_risk_response()).unwrap();
        let assessment = decision(&response, &input(), 0.85);
        assert_eq!(assessment.outcome, "allow");
        let metadata = assessment.reviewer.unwrap();
        assert_eq!(metadata["approval_checks"]["risk"]["probability"], 0.85);
        assert_eq!(
            metadata["approval_checks"]["risk"]["accepted_choices"],
            json!(["low", "medium"])
        );
        assert_eq!(
            metadata["approval_checks"]["authorization"]["probability"],
            0.85
        );
        assert_eq!(
            metadata["approval_checks"]["authorization"]["accepted_choices"],
            json!(["high"])
        );
        assert_eq!(decision(&response, &input(), 0.851).outcome, "deny");
        for auth in [
            choice(
                "high",
                json!({"high":0.84,"medium":0.16,"low":0.0,"unknown":0.0}),
            ),
            choice(
                "medium",
                json!({"high":0.4,"medium":0.6,"low":0.0,"unknown":0.0}),
            ),
        ] {
            let mut value = medium_risk_response();
            value["answers"]["authorization"] = auth;
            let assessment = decision(&parse(value).unwrap(), &input(), 0.85);
            assert_eq!(assessment.outcome, "deny");
            assert_eq!(
                assessment.reviewer.unwrap()["failed_checks"],
                json!(["authorization"])
            );
        }
    }
    #[test]
    fn medium_risk_keeps_evidence_policy_and_unsupported_class_guards() {
        let mut missing = input();
        missing.state["completeness"]["authorization"] = json!("unavailable");
        assert_eq!(
            decision(&parse(medium_risk_response()).unwrap(), &missing, 0.85).outcome,
            "deny"
        );
        for selected in ["prohibited", "needs_confirmation", "unknown"] {
            let mut value = medium_risk_response();
            let mut probabilities =
                json!({"permitted":0.0,"prohibited":0.0,"needs_confirmation":0.0,"unknown":0.0});
            probabilities[selected] = json!(1.0);
            value["answers"]["policy"] = choice(selected, probabilities);
            assert_eq!(
                decision(&parse(value).unwrap(), &input(), 0.85).outcome,
                "deny"
            );
        }
        for selected in ["high", "critical", "unknown"] {
            let mut value = medium_risk_response();
            let mut probabilities =
                json!({"low":0.0,"medium":0.0,"high":0.0,"critical":0.0,"unknown":0.0});
            probabilities[selected] = json!(1.0);
            value["answers"]["risk"] = choice(selected, probabilities);
            assert_eq!(
                decision(&parse(value).unwrap(), &input(), 0.0).outcome,
                "deny"
            );
        }
        let mut value = medium_risk_response();
        value["answers"]["risk"] = choice(
            "low",
            json!({"low":0.6,"medium":0.4,"high":0.0,"critical":0.0,"unknown":0.0}),
        );
        assert_eq!(
            decision(&parse(value).unwrap(), &input(), 0.85).outcome,
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
