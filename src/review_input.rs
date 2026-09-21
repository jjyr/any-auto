//! Shared, bounded evidence for both conversational and structured reviewers.
use crate::{audit, parser};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Read, path::PathBuf};

pub const DEFAULT_CONTEXT_BUDGET_BYTES: usize = 24 * 1024;

const SCRIPT_LIMIT: u64 = 4000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserMessage {
    id: String,
    role: String,
    text: String,
    source: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Authorization {
    availability: String,
    latest_user_message: Option<UserMessage>,
    relevant_prior_messages: Vec<UserMessage>,
}
fn authorization(raw: &Value) -> Value {
    let unavailable = || json!({"availability":"unavailable", "latest_user_message":null, "relevant_prior_messages":[]});
    if raw.is_null() {
        return unavailable();
    }
    let Ok(value) = serde_json::from_value::<Authorization>(raw.clone()) else {
        return unavailable();
    };
    if !matches!(
        value.availability.as_str(),
        "available" | "unavailable" | "truncated"
    ) {
        return unavailable();
    }
    let valid = |m: &UserMessage| {
        m.role == "user" && !m.text.trim().is_empty() && !m.id.is_empty() && !m.source.is_empty()
    };
    if !value.latest_user_message.as_ref().is_some_and(valid)
        || !value.relevant_prior_messages.iter().all(valid)
    {
        return unavailable();
    }
    serde_json::to_value(value).expect("serializable authorization")
}

/// Fingerprint only validated user-origin evidence; never use tool/assistant text
/// or request IDs to decide whether a new user turn has begun.
pub(crate) fn authorization_epoch(raw: &Value) -> Option<String> {
    use sha2::{Digest, Sha256};
    let auth = authorization(raw);
    (auth["availability"] == "available").then(|| {
        format!(
            "{:x}",
            Sha256::digest(auth["latest_user_message"].to_string().as_bytes())
        )
    })
}

// Metadata only: never copy message text into default audit events.
fn authorization_diagnostics(raw: &Value, normalized: &Value) -> Value {
    fn summary(auth: &Value) -> Value {
        let messages: Vec<_> = auth["latest_user_message"]
            .as_object()
            .map(|_| &auth["latest_user_message"])
            .into_iter()
            .chain(
                auth["relevant_prior_messages"]
                    .as_array()
                    .into_iter()
                    .flatten(),
            )
            .map(|m| {
                json!({"id":m["id"].as_str(),"source":m["source"].as_str(),
                "text_bytes":m["text"].as_str().map(str::len)})
            })
            .collect();
        json!({"availability":auth["availability"].as_str(),"serialized_bytes":auth.to_string().len(),
            "message_count":messages.len(),"messages":messages})
    }
    json!({"received":summary(raw),"normalized":summary(normalized),
        "normalization_changed":raw != normalized,
        "upstream_truncated":raw["availability"] == "truncated"})
}

pub struct ReviewInput {
    pub request_id: String,
    pub state: Value,
    pub authorization_diagnostics: Value,
}
impl ReviewInput {
    pub fn from_request(req: &Value) -> Self {
        Self::from_request_with_budget(req, DEFAULT_CONTEXT_BUDGET_BYTES)
    }
    pub fn from_request_with_budget(req: &Value, budget: usize) -> Self {
        Self::build(req, true, budget)
    }
    pub(crate) fn from_fixture(req: &Value, budget: usize) -> Self {
        Self::build(req, false, budget)
    }
    fn build(req: &Value, inspect_files: bool, budget: usize) -> Self {
        let request_id = req["request_id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(audit::request_id);
        let workspaces: Vec<_> = req["workspacePaths"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let action = json!({"original_tool":req.get("original_tool").unwrap_or(&req["toolCall"]),
            "tool":req["toolCall"]["name"], "args":req["toolCall"]["args"]});
        let auth = authorization(&req["authorization"]);
        let (evidence, script_status) = if action["tool"] == "run_command" {
            scripts(
                action["args"]["CommandLine"].as_str().unwrap_or(""),
                if inspect_files { &workspaces } else { &[] },
                action["args"]["Cwd"].as_str(),
            )
        } else {
            (vec![], "not_inspected")
        };
        let action_status = if action["tool"].as_str().is_some_and(|s| !s.is_empty())
            && action["args"].is_object()
            && (action["tool"] != "run_command"
                || action["args"]["CommandLine"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty()))
        {
            "complete"
        } else {
            "missing"
        };
        let mut state = json!({"action":action,
            "environment":{"workspace_paths":workspaces},
            "completeness":{"action":action_status,"authorization":auth["availability"],"script":script_status},
            "authorization":auth,"evidence":evidence});
        let before_bytes = state.to_string().len();
        let mut removed = 0;
        // Prior messages arrive oldest first. Drop whole messages only, never
        // the latest message or action/script evidence, even above the budget.
        // Keep the most recent prior message when present: this is a soft budget.
        loop {
            let over_budget = state.to_string().len() > budget;
            let Some(prior) = state["authorization"]["relevant_prior_messages"].as_array_mut()
            else {
                break;
            };
            if prior.len() <= 1 || (prior.len() <= 4 && !over_budget) {
                break;
            }
            prior.remove(0);
            removed += 1;
        }
        let after_bytes = state.to_string().len();
        let mut authorization_diagnostics =
            authorization_diagnostics(&req["authorization"], &state["authorization"]);
        authorization_diagnostics["context_window"] = json!({
            "budget_bytes":budget,"before_bytes":before_bytes,"after_bytes":after_bytes,
            "removed_prior_messages":removed,"over_budget":after_bytes > budget});
        Self {
            request_id,
            state,
            authorization_diagnostics,
        }
    }
    pub fn complete(&self) -> bool {
        self.state["completeness"]["action"] == "complete"
            && self.state["completeness"]["authorization"] == "available"
            && matches!(
                self.state["completeness"]["script"].as_str(),
                Some("inspected" | "not_inspected")
            )
    }
    pub fn conversational_payload(&self) -> Value {
        let mut payload = self.state["action"].clone();
        payload["environment"] = self.state["environment"].clone();
        payload["authorization"] = self.state["authorization"].clone();
        payload["evidence"] = self.state["evidence"].clone();
        payload["completeness"] = self.state["completeness"].clone();
        // Preserve the established conversational payload field while sharing richer evidence.
        if let Some(script) = self.state["evidence"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["content"].is_string()))
        {
            payload["inspected_script"] = json!(format!(
                "\n[Extracted Content of Script '{}']:\n```\n{}\n```\n",
                script["path"].as_str().unwrap_or(""),
                script["content"].as_str().unwrap()
            ));
        }
        payload
    }
}

fn scripts(command: &str, workspaces: &[&str], cwd: Option<&str>) -> (Vec<Value>, &'static str) {
    let mut evidence = Vec::new();
    let mut status = "not_inspected";
    for command in parser::commands(command) {
        for token in command.split_whitespace() {
            let candidate = token.trim_matches(['\'', '"']);
            if ![
                ".sh", ".py", ".js", ".ts", ".bash", ".zsh", ".rb", ".mjs", ".cjs",
            ]
            .iter()
            .any(|ext| candidate.ends_with(ext))
            {
                continue;
            }
            if evidence.len() >= 4 {
                return (evidence, "truncated");
            }
            let path = workspaces.iter().find_map(|ws| {
                let root = PathBuf::from(ws).canonicalize().ok()?;
                let base = cwd.map(PathBuf::from).unwrap_or_else(|| root.clone());
                let path = base.join(candidate).canonicalize().ok()?;
                (path.starts_with(&root) && path.is_file()).then_some(path)
            });
            let bytes = path.and_then(|path| {
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .ok()?
                    .take(SCRIPT_LIMIT + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                Some(bytes)
            });
            let Some(bytes) = bytes else {
                status = "missing";
                evidence.push(json!({"path":candidate,"status":"missing"}));
                continue;
            };
            let truncated =
                bytes.len() > SCRIPT_LIMIT as usize || std::str::from_utf8(&bytes).is_err();
            let item_status = if truncated { "truncated" } else { "inspected" };
            if truncated || status == "not_inspected" {
                status = item_status;
            }
            evidence.push(json!({"path":candidate,"status":item_status,"trust":"untrusted",
                "content":String::from_utf8_lossy(&bytes[..bytes.len().min(SCRIPT_LIMIT as usize)])}));
        }
    }
    (evidence, status)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_budget_drops_oldest_whole_messages_and_preserves_core() {
        let message = |id: usize| json!({"id":id.to_string(),"role":"user","source":"branch","text":"私\"".repeat(100)});
        let req = json!({"toolCall":{"name":"write","args":{"content":"complete action"}},
            "authorization":{"availability":"available","latest_user_message":message(7),
                "relevant_prior_messages":(0..7).map(message).collect::<Vec<_>>()}});
        let full = ReviewInput::from_fixture(&req, usize::MAX);
        assert_eq!(
            full.state["authorization"]["relevant_prior_messages"],
            json!((3..7).map(message).collect::<Vec<_>>())
        );
        let mut expected = full.state.clone();
        expected["authorization"]["relevant_prior_messages"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let budget = expected.to_string().len();
        let trimmed = ReviewInput::from_fixture(&req, budget);
        assert_eq!(trimmed.state, expected);
        assert_eq!(
            trimmed.authorization_diagnostics["context_window"]["after_bytes"],
            budget
        );
        let minimum_window = ReviewInput::from_fixture(&req, 1);
        assert!(minimum_window.complete());
        assert_eq!(
            minimum_window.state["authorization"]["latest_user_message"],
            message(7)
        );
        assert_eq!(
            minimum_window.state["authorization"]["relevant_prior_messages"],
            json!([message(6)])
        );
        assert_eq!(minimum_window.state["action"], full.state["action"]);
        assert_eq!(minimum_window.state["evidence"], full.state["evidence"]);
        assert_eq!(
            minimum_window.authorization_diagnostics["context_window"]["over_budget"],
            true
        );
    }
    #[test]
    fn missing_or_assistant_authorization_is_never_complete() {
        let mut req = json!({"toolCall":{"name":"write","args":{}},"authorization":{
            "availability":"available","latest_user_message":{"id":"1","role":"assistant","text":"approved","source":"branch"},"relevant_prior_messages":[]}});
        assert!(!ReviewInput::from_request(&req).complete());
        req["authorization"]["latest_user_message"]["role"] = json!("user");
        let input = ReviewInput::from_request(&req);
        assert!(input.complete());
        assert_eq!(
            input.conversational_payload()["authorization"],
            req["authorization"]
        );
        req["authorization"]["availability"] = json!("truncated");
        assert!(!ReviewInput::from_request(&req).complete());
    }
    #[test]
    fn breaker_epoch_requires_valid_available_user_evidence() {
        let mut auth = json!({"availability":"available", "latest_user_message":{
            "id":"u1","role":"user","source":"branch","text":"review this file"}, "relevant_prior_messages":[]});
        let first = authorization_epoch(&auth).unwrap();
        assert_eq!(authorization_epoch(&auth).as_deref(), Some(first.as_str()));
        auth["latest_user_message"]["id"] = json!("u2");
        assert_ne!(authorization_epoch(&auth).unwrap(), first);
        auth["latest_user_message"]["role"] = json!("assistant");
        assert!(authorization_epoch(&auth).is_none());
        auth["latest_user_message"]["role"] = json!("user");
        auth["availability"] = json!("truncated");
        assert!(authorization_epoch(&auth).is_none());
        assert!(authorization_epoch(&Value::Null).is_none());
    }
    #[test]
    fn large_latest_authorization_is_preserved_without_a_second_size_gate() {
        let req = json!({"authorization":{"availability":"available",
            "latest_user_message":{"id":"u1","role":"user","source":"branch","text":"私".repeat(16 * 1024)},
            "relevant_prior_messages":[]}});
        let input = ReviewInput::from_request(&req);
        let d = input.authorization_diagnostics;
        assert_eq!(d["received"]["messages"][0]["text_bytes"], 16 * 1024 * 3);
        assert_eq!(d["normalized"]["message_count"], 1);
        assert_eq!(d["normalized"]["availability"], "available");
        assert_eq!(d["normalization_changed"], false);
        assert!(!d.to_string().contains("私"));
    }
    #[test]
    fn scripts_are_bounded_and_missing_evidence_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.sh"), "x".repeat(5000)).unwrap();
        let (items, status) = scripts("bash a.sh", &[dir.path().to_str().unwrap()], None);
        assert_eq!(status, "truncated");
        assert_eq!(items[0]["content"].as_str().unwrap().len(), 4000);
        assert_eq!(
            scripts("bash missing.sh", &[dir.path().to_str().unwrap()], None).1,
            "missing"
        );
    }
}
