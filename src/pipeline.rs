use crate::{audit, config, daemon, parser, reviewer::Assessment};
use anyhow::Result;
use fs2::FileExt;
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::LazyLock,
};

pub fn blacklist(command: &str) -> Option<String> {
    static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [
        r"(?:^|[;&|\n`$()])\s*rm\s+-[rfRF]*\s+/(?:\*|\s*$)",
        r"(?:^|[;&|\n`$()])\s*rm\s+-[rfRF]*\s+~(?:/.*|\s*$)",
        r"(?:^|[;&|\n`$()])\s*rm\s+-[rfRF]*\s+\$HOME(?:/.*|\s*$)",
        r"(?:^|[;&|\n`$()])\s*rm\s+-[rfRF]*\s+/(?:etc|usr|var|bin|System|boot|sbin|Users|home)(?:/|\s+|$)",
        r"(?:^|[;&|\n`$()])\s*rm\s+-[rfRF]*\s+(?:.*/)?\.git(?:/|\s+|$)",
        r"\bmkfs\b", r"\bfdisk\b", r"\bdd\s+if=", r":\(\)\{\s*:\|:&\s*\};:",
        r"(?:^|[;&|\n`$()])\s*chmod\s+-[rwxRWX0-7]*\s+777\s+/",
    ].iter().map(|s| Regex::new(s).unwrap()).collect()
    });
    for cmd in std::iter::once(command.to_string()).chain(parser::commands(command)) {
        for pattern in PATTERNS.iter() {
            if pattern.is_match(&cmd) {
                return Some(format!(
                    "Blocked by hard blacklist: matched pattern '{}'",
                    pattern.as_str()
                ));
            }
        }
    }
    None
}
pub fn read_only(tool: &str) -> bool {
    matches!(
        tool,
        "view_file"
            | "grep_search"
            | "find_by_name"
            | "list_dir"
            | "read_url_content"
            | "search_web"
            | "read_browser_page"
    )
}
fn lock_busy(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
}
pub struct Breaker {
    path: PathBuf,
    _lock: fs::File,
    state: Value,
}
impl Breaker {
    pub fn open(dir: &Path, cid: &str) -> Result<Self> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match Self::try_open(dir, cid) {
                Err(e) if lock_busy(&e) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                result => return result,
            }
        }
    }
    async fn open_for_review(dir: &Path, cid: &str) -> Result<Self> {
        loop {
            match Self::try_open(dir, cid) {
                Err(e) if lock_busy(&e) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await
                }
                result => return result,
            }
        }
    }
    fn try_open(dir: &Path, cid: &str) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let cid = format!("{:x}", Sha256::digest(cid.as_bytes()));
        let path = dir.join(format!("cb_{cid}.json"));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))?;
        lock.try_lock_exclusive()?;
        let state = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({"consecutive_denials":0,"history":[]}));
        Ok(Self {
            path,
            _lock: lock,
            state,
        })
    }
    pub fn tripped(&self) -> Option<String> {
        let n = self.state["consecutive_denials"].as_u64().unwrap_or(0);
        if n >= 3 {
            return Some(format!(
                "Circuit breaker tripped: {n} consecutive denials exceeded threshold (3). Halting loop to prompt user."
            ));
        }
        if let Some(h) = self.state["history"].as_array() {
            let denials = h.iter().rev().take(5).filter(|v| **v == "deny").count();
            if h.len() >= 5 && denials >= 4 && h.last() == Some(&json!("deny")) {
                return Some(format!(
                    "Circuit breaker tripped: {denials}/5 denials in recent window. Halting loop to prompt user."
                ));
            }
        }
        None
    }
    pub fn record(&mut self, decision: &str) -> Result<()> {
        self.state["consecutive_denials"] = if decision == "deny" {
            self.state["consecutive_denials"].as_u64().unwrap_or(0) + 1
        } else {
            0
        }
        .into();
        let mut h = self.state["history"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        h.push(decision.into());
        if h.len() > 5 {
            h.drain(..h.len() - 5);
        }
        self.state["history"] = h.into();
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(&self.state)?)?;
        fs::rename(tmp, &self.path)?;
        Ok(())
    }
}
pub fn result(decision: &str, reason: &str, tool: &str, grants: Option<Vec<String>>) -> Value {
    let tag = match decision {
        "allow" => "ALLOWED",
        "deny" => "DENIED",
        _ => "REVIEW REQUIRED",
    };
    let reason = if reason.trim().starts_with("[agy-auto-approve") {
        reason.trim().into()
    } else {
        format!("[agy-auto-approve: {tag}] {}", reason.trim())
    };
    let dir = config::log_dir();
    if fs::create_dir_all(&dir).is_ok()
        && let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("auto-approve.log"))
    {
        let _ = writeln!(
            f,
            "[{}] [{:<5}] mode={} tool={} | reason={}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            decision.to_uppercase(),
            config::mode().as_str(),
            tool,
            reason
        );
    }
    if std::env::var("AGY_AUTO_APPROVE_SILENT")
        .unwrap_or_default()
        .is_empty()
    {
        let color = match decision {
            "allow" => 32,
            "deny" => 31,
            _ => 33,
        };
        eprintln!(
            "\x1b[{color}m[agy-auto-approve: {}]\x1b[0m {tool} -> {reason}",
            decision.to_uppercase()
        );
    }
    let mut v = json!({"decision": decision, "reason": reason});
    if let Some(grants) = grants {
        v["permissionOverrides"] = grants.into();
    }
    v
}
pub async fn evaluate(payload: &Value) -> Value {
    let id = payload["request_id"]
        .as_str()
        .filter(|s| s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(str::to_owned)
        .unwrap_or_else(audit::request_id);
    let payload = &normalize(payload);
    let started = std::time::Instant::now();
    audit::record(&id, "hook_input", json!({"input":payload}));
    let mut stage = "reviewer";
    let mut reviewer = Value::Null;
    // Includes waiting for this user's circuit breaker lock and daemon startup/review.
    let mut output = match tokio::time::timeout(
        std::time::Duration::from_secs(28),
        evaluate_inner(payload, &id, &mut stage, &mut reviewer),
    )
    .await
    {
        Ok(output) => output,
        Err(_) => {
            stage = "reviewer_error";
            result(
                "deny",
                "Fail-closed: Approval deadline exceeded",
                payload["toolCall"]["name"].as_str().unwrap_or(""),
                None,
            )
        }
    };
    output["request_id"] = json!(id);
    audit::record(
        &id,
        "hook_result",
        json!({"tool":payload.get("original_tool").unwrap_or(&payload["toolCall"])["name"],
        "command":payload["toolCall"]["args"]["CommandLine"],
        "cwd":payload["toolCall"]["args"]["Cwd"],
        "hook_pid":std::process::id(),
        "conversation_id":conversation_id(payload), "output":output, "stage":stage,
        "duration_ms":started.elapsed().as_millis(), "reviewer":reviewer}),
    );
    output
}
fn conversation_id(payload: &Value) -> &str {
    crate::sessions::user_session_id(payload).unwrap_or("default")
}
async fn evaluate_inner(
    payload: &Value,
    id: &str,
    stage: &mut &'static str,
    reviewer: &mut Value,
) -> Value {
    let tool = payload["toolCall"]["name"].as_str().unwrap_or("");
    if std::env::var_os("AGY_AUTO_APPROVE_REVIEWER").is_some() {
        *stage = "reviewer_recursion";
        return result(
            "deny",
            "Approval reviewers may not invoke tools.",
            tool,
            None,
        );
    }
    let args = &payload["toolCall"]["args"];
    if read_only(tool) {
        *stage = "whitelist";
        return result(
            "allow",
            "Read-only tool automatically approved.",
            tool,
            Some(vec![]),
        );
    }
    if tool == "run_command"
        && let Some(reason) = blacklist(args["CommandLine"].as_str().unwrap_or(""))
    {
        *stage = "blacklist";
        return result("deny", &reason, tool, None);
    }
    let breaker_result = match crate::sessions::user_session_id(payload) {
        Some(cid) => Breaker::open_for_review(&config::state_dir(), cid)
            .await
            .map(Some),
        None => Ok(None),
    };
    let mut breaker = match breaker_result {
        Ok(b) => b,
        Err(e) => {
            *stage = "state_error";
            return result(
                "deny",
                &format!("Fail-closed: cannot open circuit breaker state: {e}"),
                tool,
                None,
            );
        }
    };
    if let Some(reason) = breaker.as_ref().and_then(Breaker::tripped) {
        *stage = "circuit_breaker";
        return result("force_ask", &reason, tool, None);
    }
    let assessment = match daemon::review_traced(payload, id).await {
        Ok(a) => a,
        Err(e) => {
            *stage = "reviewer_error";
            Assessment::deny(format!("Approver daemon/backend is unavailable: {e}"))
        }
    };
    *reviewer = assessment.reviewer.clone().unwrap_or(Value::Null);
    audit::record(id, "assessment", json!({"assessment":assessment}));
    if assessment.error_stage.is_some() {
        *stage = "reviewer_error";
    }
    if let Err(e) = breaker
        .as_mut()
        .map(|b| b.record(&assessment.outcome))
        .transpose()
    {
        *stage = "state_error";
        return result(
            "deny",
            &format!("Fail-closed: cannot persist circuit breaker: {e}"),
            tool,
            None,
        );
    }
    let grants = (assessment.outcome == "allow").then(|| parser::overrides(tool, args));
    result(&assessment.outcome, &assessment.rationale, tool, grants)
}

fn normalize(payload: &Value) -> Value {
    let mut value = payload.clone();
    if config::mode() == config::Mode::Pi {
        value["original_tool"] = payload["toolCall"].clone();
        let name = payload["toolCall"]["name"].as_str().unwrap_or("");
        if name == "bash" {
            value["toolCall"]["name"] = json!("run_command");
            value["toolCall"]["args"]["CommandLine"] =
                payload["toolCall"]["args"]["command"].clone();
            value["toolCall"]["args"]["Cwd"] = payload["workspacePaths"][0].clone();
        } else if payload["builtin_tool"] == true {
            let mapped = match name {
                "read" => "view_file",
                "grep" => "grep_search",
                "find" => "find_by_name",
                "ls" => "list_dir",
                _ => name,
            };
            value["toolCall"]["name"] = json!(mapped);
        } else {
            value["toolCall"]["name"] = json!(format!("pi:{name}"));
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_breaker_wait_does_not_retain_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = Breaker::open(root.path(), "user").unwrap();
        owner.record("deny").unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(5),
                Breaker::open_for_review(root.path(), "user")
            )
            .await
            .is_err()
        );
        drop(owner);
        let restored = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            Breaker::open_for_review(root.path(), "user"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(restored.state["consecutive_denials"], 1);
    }
}
