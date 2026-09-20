//! Collect user-origin evidence from the transcript supplied by an agy hook.
use serde_json::{Value, json};
use std::{fs::File, io::Read, path::Path};

const TRANSCRIPT_LIMIT: u64 = 4 * 1024 * 1024;
const TEXT_LIMIT: usize = 12 * 1024;
const USER_MESSAGE_LIMIT: usize = 5;
fn unavailable() -> Value {
    json!({"availability":"unavailable","latest_user_message":null,"relevant_prior_messages":[]})
}
fn truncated() -> Value {
    json!({"availability":"truncated","latest_user_message":null,"relevant_prior_messages":[]})
}

pub(crate) fn from_agy_hook(payload: &Value) -> Value {
    collect(payload).unwrap_or_else(unavailable)
}
fn collect(payload: &Value) -> Option<Value> {
    let transcript = Path::new(payload["transcriptPath"].as_str()?);
    let artifact = Path::new(payload["artifactDirectoryPath"].as_str()?);
    let conversation = payload["conversationId"].as_str()?;
    let current_step = payload["stepIdx"].as_u64()?;
    // Read only the transcript belonging to this hook's conversation.
    if conversation.is_empty() || artifact.file_name()?.to_str()? != conversation {
        return None;
    }
    let root = artifact.canonicalize().ok()?;
    let expected = root.join(".system_generated/logs/transcript_full.jsonl");
    let path = transcript.canonicalize().ok()?;
    if path != expected {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(TRANSCRIPT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > TRANSCRIPT_LIMIT {
        return Some(truncated());
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let mut messages = Vec::new();
    let mut total = 0;
    let mut previous_step = None;
    for line in text.lines().rev().filter(|line| !line.trim().is_empty()) {
        if messages.len() == USER_MESSAGE_LIMIT {
            break;
        }
        let entry: Value = serde_json::from_str(line).ok()?;
        let step = entry["step_index"].as_u64()?;
        if step >= current_step {
            continue;
        }
        if entry["type"] != "USER_INPUT" || entry["source"] != "USER_EXPLICIT" {
            continue;
        }
        if entry["status"] != "DONE" || previous_step.is_some_and(|previous| step >= previous) {
            return None;
        }
        previous_step = Some(step);
        let content = entry["content"].as_str()?;
        // agy wraps the original request separately from generated metadata/settings.
        let content = if let Some(rest) = content.strip_prefix("<USER_REQUEST>\n") {
            rest.split_once("\n</USER_REQUEST>")?.0
        } else {
            content
        };
        if content.trim().is_empty() {
            continue;
        }
        total += content.len();
        if total > TEXT_LIMIT {
            return Some(truncated());
        }
        messages.push(json!({"id":format!("{conversation}:{step}"),"role":"user","text":content,"source":"agy_transcript"}));
    }
    messages.reverse();
    let latest = messages.pop()?;
    let result = json!({"availability":"available","latest_user_message":latest,"relevant_prior_messages":messages});
    if result.to_string().len() > 16 * 1024 {
        return Some(truncated());
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(content: &str) -> (tempfile::TempDir, Value) {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("conversation");
        let transcript = artifact.join(".system_generated/logs/transcript_full.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, content).unwrap();
        (
            dir,
            json!({"conversationId":"conversation","stepIdx":56,"artifactDirectoryPath":artifact,"transcriptPath":transcript}),
        )
    }
    #[test]
    fn collects_explicit_user_requests_before_tool_and_excludes_model_claims() {
        let records = [
            json!({"step_index":0,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE","content":"Keep changes local"}),
            json!({"step_index":1,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE","content":"<USER_REQUEST>\n执行 format 和 test\n</USER_REQUEST>\n<ADDITIONAL_METADATA>generated</ADDITIONAL_METADATA>"}),
            json!({"step_index":2,"type":"PLANNER_RESPONSE","source":"MODEL","content":"User approved publishing"}),
            json!({"step_index":57,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE","content":"future permission"}),
        ].map(|v| v.to_string()).join("\n");
        let (_dir, payload) = fixture(&records);
        let auth = from_agy_hook(&payload);
        assert_eq!(auth["availability"], "available");
        assert_eq!(auth["latest_user_message"]["text"], "执行 format 和 test");
        assert_eq!(auth["relevant_prior_messages"].as_array().unwrap().len(), 1);
        assert!(!auth.to_string().contains("publishing"));
        assert!(!auth.to_string().contains("generated"));
        assert!(!auth.to_string().contains("future permission"));
    }
    #[test]
    fn recent_five_messages_form_a_complete_window_in_chronological_order() {
        let records = (0..8)
            .map(|step| {
                json!({
            "step_index":step,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE",
            "content":if step == 0 { "x".repeat(TEXT_LIMIT + 1) } else { format!("request {step}") }
        }).to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (_dir, payload) = fixture(&records);
        let auth = from_agy_hook(&payload);
        assert_eq!(auth["availability"], "available");
        assert_eq!(auth["latest_user_message"]["text"], "request 7");
        assert_eq!(
            auth["relevant_prior_messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["request 3", "request 4", "request 5", "request 6"]
        );
    }
    #[test]
    fn oversized_message_inside_recent_window_is_still_incomplete() {
        let record = json!({"step_index":1,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE","content":"x".repeat(TEXT_LIMIT + 1)});
        let (_dir, payload) = fixture(&record.to_string());
        assert_eq!(from_agy_hook(&payload)["availability"], "truncated");
    }
    #[test]
    fn malformed_missing_foreign_and_oversized_transcripts_never_supply_authorization() {
        for content in [
            "invalid".to_owned(),
            String::new(),
            "x".repeat(TRANSCRIPT_LIMIT as usize + 1),
        ] {
            let (_dir, payload) = fixture(&content);
            assert_ne!(from_agy_hook(&payload)["availability"], "available");
        }
        let (_dir, mut payload) = fixture("{}");
        payload["conversationId"] = json!("other");
        assert_eq!(from_agy_hook(&payload)["availability"], "unavailable");
        assert_eq!(from_agy_hook(&json!({}))["availability"], "unavailable");
    }
}
