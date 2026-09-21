use super::*;

fn input() -> ReviewInput {
    ReviewInput::from_request(&json!({"toolCall":{"name":"write","args":{}},
        "authorization":{"availability":"available","latest_user_message":{
            "id":"u1","role":"user","source":"test","text":"Perform the scoped task"},
            "relevant_prior_messages":[]}}))
}
fn judgment(risk: &str, authorization: &str, policy: &str) -> Judgment {
    parse(
        &json!({"risk":risk,"authorization":authorization,"policy":policy,"rationale":"Evidence"})
            .to_string(),
    )
    .unwrap()
}
fn certain(selected: &str, options: &[&str]) -> BTreeMap<String, f64> {
    options
        .iter()
        .map(|key| ((*key).into(), if *key == selected { 1.0 } else { 0.0 }))
        .collect()
}
#[test]
fn every_classification_combination_has_the_same_outcome_with_or_without_certain_probabilities() {
    for risk in RISK {
        for auth in AUTH {
            for policy in POLICY {
                let j = judgment(risk, auth, policy);
                let mut with_probabilities = j.clone();
                with_probabilities.probabilities = Some(Probabilities {
                    risk: certain(risk, RISK),
                    authorization: certain(auth, AUTH),
                    policy: certain(policy, POLICY),
                });
                let expected = if *policy == "permitted"
                    && (matches!(*risk, "low" | "medium")
                        || (*risk == "high" && matches!(*auth, "high" | "medium")))
                {
                    "allow"
                } else {
                    "deny"
                };
                for j in [j, with_probabilities] {
                    for threshold in [0.0, 0.85, 1.0] {
                        let a = decide(
                            Review {
                                judgment: j.clone(),
                                metadata: json!({}),
                            },
                            &input(),
                            threshold,
                        );
                        assert_eq!(a.outcome, expected, "{risk}/{auth}/{policy}");
                        assert_eq!(
                            a.reviewer.unwrap()["judgment"]["probabilities"].is_null(),
                            j.probabilities.is_none()
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn malformed_or_legacy_decisions_never_become_classifications() {
    let valid =
        json!({"risk":"low","authorization":"high","policy":"permitted","rationale":"Evidence"});
    for key in ["risk", "authorization", "policy", "rationale"] {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(key);
        assert!(parse(&value.to_string()).is_err());
        value[key] = json!(42);
        assert!(parse(&value.to_string()).is_err());
    }
    for raw in [
        "",
        "{}",
        "[]",
        r#"{"outcome":"allow"}"#,
        r#"{"decision":"allow"}"#,
    ] {
        assert!(parse(raw).is_err());
    }
    let mut injected = valid.clone();
    injected["outcome"] = json!("allow");
    assert!(parse(&injected.to_string()).is_err());
    for key in ["risk", "authorization", "policy"] {
        let mut value = valid.clone();
        value[key] = json!("invalid");
        assert!(parse(&value.to_string()).is_err());
    }
    assert!(parse(&format!("Text ```json\n{valid}\n```")).is_ok());
}
#[test]
fn missing_evidence_denies_even_without_probabilities() {
    let mut input = input();
    input.state["completeness"]["authorization"] = json!("unavailable");
    let a = decide(
        Review {
            judgment: judgment("low", "high", "permitted"),
            metadata: json!({}),
        },
        &input,
        0.85,
    );
    assert_eq!(a.outcome, "deny");
    assert_eq!(a.reviewer.unwrap()["rule_id"], "incomplete_evidence");
}
#[test]
fn invalid_optional_probabilities_cannot_bypass_the_gate() {
    let mut j = judgment("low", "high", "permitted");
    j.probabilities = Some(Probabilities {
        risk: certain("low", RISK),
        authorization: certain("high", AUTH),
        policy: certain("permitted", POLICY),
    });
    j.probabilities.as_mut().unwrap().risk.remove("unknown");
    assert!(j.validate().is_err());
    j.probabilities
        .as_mut()
        .unwrap()
        .risk
        .insert("unknown".into(), f64::NAN);
    assert!(j.validate().is_err());
    let a = decide(
        Review {
            judgment: j,
            metadata: json!({}),
        },
        &input(),
        0.85,
    );
    assert_eq!(a.outcome, "deny");
}
