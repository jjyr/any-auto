//! Convert cumulative CLI counters into independently readable per-call usage.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Write, path::Path};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Tokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Baseline {
    conversation_id: String,
    turn: u64,
    tokens: Tokens,
}
impl Baseline {
    fn from_response(v: &Value) -> Option<Self> {
        Some(Self {
            conversation_id: v["conversation_id"]
                .as_str()
                .filter(|s| !s.is_empty())?
                .into(),
            turn: v["num_turns"].as_u64().filter(|n| *n > 0)?,
            tokens: serde_json::from_value(v["usage"].clone()).ok()?,
        })
    }
    fn delta(&self, previous: Option<&Self>, fresh: bool) -> Option<Tokens> {
        if fresh && self.turn == 1 {
            return Some(self.tokens);
        }
        let previous = previous?;
        if previous.conversation_id != self.conversation_id
            || previous.turn.checked_add(1) != Some(self.turn)
        {
            return None;
        }
        Some(Tokens {
            input_tokens: self
                .tokens
                .input_tokens
                .checked_sub(previous.tokens.input_tokens)?,
            output_tokens: self
                .tokens
                .output_tokens
                .checked_sub(previous.tokens.output_tokens)?,
        })
    }
}

pub(crate) fn save(path: &Path, state: &Value) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer(&mut file, state)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

/// Bridges serialize calls within a session. Failed persistence never changes a decision;
/// the next call detects a missing round and reports unknown rather than overcounting.
pub(crate) fn record(path: &Path, cid: Option<&str>, raw: &str) -> Option<Tokens> {
    let value: Value = serde_json::from_str(raw).ok()?;
    if value["status"] != "SUCCESS" {
        return None;
    }
    let current = Baseline::from_response(&value)?;
    if cid.is_some_and(|cid| cid != current.conversation_id) {
        return None;
    }
    let mut state: Value = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    let previous: Option<Baseline> = serde_json::from_value(state["usage_baseline"].clone()).ok();
    if cid.is_some()
        && previous.as_ref().is_some_and(|previous| {
            previous.conversation_id == current.conversation_id && previous.turn >= current.turn
        })
    {
        return None;
    }
    let delta = current.delta(previous.as_ref(), cid.is_none());
    if !state.is_object() || state["conversationId"] != current.conversation_id {
        state = json!({});
    }
    state["conversationId"] = json!(current.conversation_id);
    state["usage_baseline"] = json!(current);
    if let Err(error) = save(path, &state) {
        eprintln!("agy-auto-approve: unable to save token baseline: {error}");
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(turn: u64, input: u64, output: u64) -> String {
        json!({"status":"SUCCESS","conversation_id":"session","num_turns":turn,
            "usage":{"input_tokens":input,"output_tokens":output}})
        .to_string()
    }
    #[test]
    fn first_turn_resume_restart_gaps_and_counter_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer_session.json");
        assert_eq!(
            record(&path, None, &response(1, 100, 20)),
            Some(Tokens {
                input_tokens: 100,
                output_tokens: 20
            })
        );
        assert_eq!(
            record(&path, Some("session"), &response(2, 140, 25)),
            Some(Tokens {
                input_tokens: 40,
                output_tokens: 5
            })
        );
        // Calls read the persisted baseline, including after daemon restart.
        assert_eq!(
            record(&path, Some("session"), &response(3, 190, 35)),
            Some(Tokens {
                input_tokens: 50,
                output_tokens: 10
            })
        );
        assert_eq!(record(&path, Some("session"), &response(3, 190, 35)), None);
        assert_eq!(record(&path, Some("session"), &response(5, 250, 45)), None);
        assert_eq!(
            record(&path, Some("session"), &response(6, 270, 48)),
            Some(Tokens {
                input_tokens: 20,
                output_tokens: 3
            })
        );
        assert_eq!(record(&path, Some("session"), &response(7, 1, 1)), None);
        assert_eq!(
            record(&path, Some("session"), &response(8, 5, 2)),
            Some(Tokens {
                input_tokens: 4,
                output_tokens: 1
            })
        );
    }
    #[test]
    fn missing_baseline_wrong_session_and_incomplete_usage_are_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer_session.json");
        assert_eq!(record(&path, Some("session"), &response(10, 100, 20)), None);
        assert_eq!(record(&path, Some("other"), &response(11, 150, 25)), None);
        assert_eq!(record(&path, Some("session"), "{}"), None);
        assert_eq!(
            record(&path, Some("session"), &response(11, 150, 25)),
            Some(Tokens {
                input_tokens: 50,
                output_tokens: 5
            })
        );
    }
}
