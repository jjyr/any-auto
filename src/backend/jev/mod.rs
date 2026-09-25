//! TypeSafe wire validation and normalization into the shared Judgment contract.
pub(crate) mod client;
use super::{Backend, ReviewFuture};
use crate::{audit, config::ReviewerConfig, reviewer::ReviewInput};
#[cfg(test)]
use crate::{config::JevInstructions, policy, reviewer::Assessment};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Instant;

use crate::policy::RUBRIC_VERSION;
use crate::policy::{
    Authorization, AuthorizationProbabilities, Distribution, Judgment, Policy, PolicyProbabilities,
    Probabilities, Review, Risk, RiskProbabilities,
};
use crate::prompts::jev_questions as questions;
#[derive(Deserialize, Serialize)]
enum AnswerType {
    #[serde(rename = "choice")]
    Choice,
}
#[derive(Deserialize, Serialize)]
struct Choice<C, P> {
    #[serde(rename = "type")]
    kind: AnswerType,
    choice: C,
    probabilities: P,
    confidence: f64,
}
impl<C: Copy, P: Distribution<C>> Choice<C, P> {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.confidence.is_finite() && (0.0..=1.0).contains(&self.confidence),
            "Invalid Jev probability or confidence"
        );
        self.probabilities.validate(self.choice)
    }
}
#[derive(Deserialize, Serialize)]
struct Answers {
    risk: Choice<Risk, RiskProbabilities>,
    authorization: Choice<Authorization, AuthorizationProbabilities>,
    policy: Choice<Policy, PolicyProbabilities>,
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
    response.answers.risk.validate()?;
    response.answers.authorization.validate()?;
    response.answers.policy.validate()?;
    Ok(response)
}
fn into_review(response: &Response) -> Result<Review> {
    let a = &response.answers;
    let judgment = Judgment {
        risk: a.risk.choice,
        authorization: a.authorization.choice,
        policy: a.policy.choice,
        rationale: "Jev structured classification".into(),
        probabilities: Some(Probabilities {
            risk: a.risk.probabilities.clone(),
            authorization: a.authorization.probabilities.clone(),
            policy: a.policy.probabilities.clone(),
        }),
    };
    judgment.validate()?;
    Ok(Review {
        judgment,
        metadata: json!({"model":response.model,"answers":a,"rubric_version":RUBRIC_VERSION}),
    })
}
#[cfg(test)]
fn decision(response: &Response, input: &ReviewInput, threshold: f64) -> Assessment {
    policy::decide(into_review(response).unwrap(), input, threshold)
}

pub struct JevBackend {
    config: ReviewerConfig,
    client: client::JevClient,
    evaluation_questions: Option<Value>,
}
impl JevBackend {
    pub fn new(config: ReviewerConfig) -> Result<Self> {
        Ok(Self {
            client: client::JevClient::new(
                &config.approver.base_url,
                config.approver.request_timeout,
            )?,
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
    async fn evaluate(&self, input: &ReviewInput) -> Result<Review> {
        let key = &self.config.approver.api_key;
        ensure!(!key.trim().is_empty(), "Jev API key is empty");
        let questions = self
            .evaluation_questions
            .clone()
            .unwrap_or_else(|| questions(&self.config.approver.instructions));
        let body =
            json!({"model":self.config.approver.model,"state":input.state,"questions":questions});
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
        let mut result = into_review(&response)?;
        if self.evaluation_questions.is_some() {
            result.metadata["rubric_version"] = json!("evaluation");
        }
        use sha2::{Digest, Sha256};
        result.metadata["rubric_hash"] = json!(format!(
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
    const RISK: &[&str] = &["low", "medium", "high", "critical", "unknown"];
    const AUTH: &[&str] = &["high", "medium", "low", "unknown"];
    const POLICY: &[&str] = &["permitted", "prohibited", "needs_confirmation", "unknown"];
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
        assert_eq!(decision(&response, &input, 0.9).outcome.as_str(), "allow");
        assert_eq!(decision(&response, &input, 0.91).outcome.as_str(), "deny");
        assert_eq!(decision(&response, &input, 0.0).outcome.as_str(), "allow");
        assert_eq!(decision(&response, &input, 1.0).outcome.as_str(), "deny");
    }
    fn certain(selected: &str, options: &[&str]) -> Value {
        let probabilities: serde_json::Map<String, Value> = options
            .iter()
            .map(|option| {
                (
                    (*option).into(),
                    json!(if *option == selected { 1.0 } else { 0.0 }),
                )
            })
            .collect();
        choice(selected, probabilities.into())
    }
    #[test]
    fn codex_authorization_matrix_preserves_policy_and_critical_guards() {
        for risk in RISK {
            for auth in AUTH {
                for policy in POLICY {
                    let mut value = response();
                    value["answers"]["risk"] = certain(risk, RISK);
                    value["answers"]["authorization"] = certain(auth, AUTH);
                    value["answers"]["policy"] = certain(policy, POLICY);
                    let parsed = parse(value).unwrap();
                    let allowed = *policy == "permitted"
                        && (matches!(*risk, "low" | "medium")
                            || (*risk == "high" && matches!(*auth, "high" | "medium")));
                    for threshold in [0.0, 0.85, 1.0] {
                        let assessment = decision(&parsed, &input(), threshold);
                        assert_eq!(
                            assessment.outcome.as_str(),
                            if allowed { "allow" } else { "deny" },
                            "risk={risk} auth={auth} policy={policy} threshold={threshold}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn low_medium_do_not_require_an_authorization_probability_gate() {
        for risk in ["low", "medium"] {
            let mut value = response();
            value["answers"]["risk"] = certain(risk, RISK);
            value["answers"]["authorization"] = certain("unknown", AUTH);
            let assessment = decision(&parse(value).unwrap(), &input(), 0.9);
            assert_eq!(assessment.outcome.as_str(), "allow");
            let metadata = assessment.reviewer.unwrap();
            assert_eq!(
                metadata["approval_checks"]["authorization"]["required"],
                false
            );
            assert_eq!(metadata["failed_checks"], json!([]));
        }
    }
    #[test]
    fn high_risk_requires_authorization_and_reports_actual_gates() {
        let mut value = response();
        value["answers"]["risk"] = certain("high", RISK);
        value["answers"]["authorization"] = choice(
            "medium",
            json!({"high":0.03,"medium":0.63,"low":0.27,"unknown":0.07}),
        );
        let assessment = decision(&parse(value.clone()).unwrap(), &input(), 0.85);
        assert_eq!(assessment.outcome.as_str(), "deny");
        let metadata = assessment.reviewer.unwrap();
        assert_eq!(metadata["failed_checks"], json!(["authorization"]));
        assert_eq!(
            metadata["approval_checks"]["authorization"]["required"],
            true
        );
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
            "medium",
            json!({"high":0.35,"medium":0.50,"low":0.10,"unknown":0.05}),
        );
        let parsed = parse(value).unwrap();
        assert_eq!(decision(&parsed, &input(), 0.85).outcome.as_str(), "allow");
        assert_eq!(decision(&parsed, &input(), 0.851).outcome.as_str(), "deny");
    }
    #[test]
    fn permitted_risk_mass_combines_only_when_authorization_supports_it() {
        let mut value = response();
        value["answers"]["risk"] = choice(
            "medium",
            json!({"low":0.07,"medium":0.61,"high":0.30,"critical":0.02,"unknown":0.0}),
        );
        value["answers"]["authorization"] = certain("high", AUTH);
        let parsed = parse(value.clone()).unwrap();
        let allowed = decision(&parsed, &input(), 0.85);
        assert_eq!(allowed.outcome.as_str(), "allow");
        assert_eq!(
            allowed.reviewer.unwrap()["approval_checks"]["risk"]["accepted_choices"],
            json!(["low", "medium", "high"])
        );
        value["answers"]["authorization"] = certain("low", AUTH);
        assert_eq!(
            decision(&parse(value.clone()).unwrap(), &input(), 0.85)
                .outcome
                .as_str(),
            "deny"
        );
        value["answers"]["risk"] = choice(
            "low",
            json!({"low":0.61,"medium":0.39,"high":0.0,"critical":0.0,"unknown":0.0}),
        );
        assert_eq!(
            decision(&parse(value.clone()).unwrap(), &input(), 0.85)
                .outcome
                .as_str(),
            "allow"
        );
        value["answers"]["authorization"] = certain("high", AUTH);
        value["answers"]["risk"] = choice(
            "high",
            json!({"low":0.05,"medium":0.15,"high":0.50,"critical":0.20,"unknown":0.10}),
        );
        assert_eq!(
            decision(&parse(value).unwrap(), &input(), 0.85)
                .outcome
                .as_str(),
            "deny"
        );
    }
    #[test]
    fn authorization_never_overrides_missing_evidence_or_absolute_denial() {
        for risk in ["low", "medium", "high"] {
            let mut value = response();
            value["answers"]["risk"] = certain(risk, RISK);
            value["answers"]["authorization"] = certain("high", AUTH);
            let mut missing = input();
            missing.state["completeness"]["authorization"] = json!("unavailable");
            assert_eq!(
                decision(&parse(value).unwrap(), &missing, 0.0)
                    .outcome
                    .as_str(),
                "deny"
            );
        }
        let mut value = response();
        value["answers"]["risk"] = choice(
            "critical",
            json!({"low":0.10,"medium":0.10,"high":0.20,"critical":0.40,"unknown":0.20}),
        );
        let assessment = decision(&parse(value).unwrap(), &input(), 1.0);
        assert_eq!(assessment.outcome.as_str(), "deny");
        assert_eq!(
            assessment.reviewer.unwrap()["rule_id"],
            "prohibited_or_critical"
        );
    }
    #[test]
    fn approximate_probability_sums_preserve_raw_approval_gates() {
        for (question, option) in [
            ("risk", "low"),
            ("authorization", "medium"),
            ("policy", "permitted"),
        ] {
            for delta in [-0.01, 0.01] {
                let mut value = response();
                if question == "risk" {
                    value["answers"]["risk"] = choice(
                        "low",
                        json!({"low":0.9,"medium":0.0,"high":0.0,"critical":0.1,"unknown":0.0}),
                    );
                    value["answers"]["authorization"] = certain("low", AUTH);
                } else if question == "authorization" {
                    value["answers"]["risk"] = certain("high", RISK);
                }
                let original = value["answers"][question]["probabilities"][option]
                    .as_f64()
                    .unwrap();
                value["answers"][question]["probabilities"][option] = json!(original + delta);
                let parsed = parse(value.clone()).unwrap();
                assert_eq!(
                    serde_json::to_value(&parsed.answers).unwrap(),
                    value["answers"]
                );
                assert_eq!(
                    decision(&parsed, &input(), 0.9).outcome.as_str(),
                    if delta < 0.0 { "deny" } else { "allow" },
                    "{question}: delta={delta}"
                );
            }
        }
    }
    #[test]
    fn malformed_required_answers_fail_validation() {
        for (pointer, bad) in [
            ("/answers/risk/type", json!("noul")),
            ("/answers/risk/choice", json!("allow")),
            ("/answers/risk/choice", json!("high")),
            ("/answers/risk/probabilities/low", json!(1.2)),
            ("/answers/risk/probabilities/low", json!(-0.1)),
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
        value["answers"]["risk"]["probabilities"]["surprise"] = json!({"ignored": true});
        value["answers"]["risk"]["explanation"] = json!("extra answer data");
        value["answers"]["new_question"] = json!(42);
        let parsed = parse(value).unwrap();
        assert_eq!(
            decision(&parsed, &input(), 0.9).outcome,
            policy::Outcome::Allow
        );
        let mut missing = response();
        missing["answers"]["risk"]["probabilities"]
            .as_object_mut()
            .unwrap()
            .remove("unknown");
        assert!(parse(missing).is_err());
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
