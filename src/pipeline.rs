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
    pub fn reset(&mut self) -> Result<()> {
        self.state["consecutive_denials"] = json!(0);
        self.state["history"] = json!([]);
        self.state
            .as_object_mut()
            .unwrap()
            .remove("pending_escalation");
        self.save()
    }
    fn observe_user(&mut self, epoch: &str) -> Result<bool> {
        if self.state["authorization_epoch"] == epoch {
            return Ok(false);
        }
        let had_denials = self.state["history"]
            .as_array()
            .is_some_and(|history| history.iter().any(|v| v == "deny"))
            || self.state["consecutive_denials"].as_u64().unwrap_or(0) > 0;
        self.state["authorization_epoch"] = json!(epoch);
        self.reset()?;
        Ok(had_denials)
    }
    fn escalate(&mut self, id: &str, step: Option<u64>) -> Result<()> {
        self.state["pending_escalation"] = json!({"request_id":id,"step":step});
        self.save()
    }
    pub fn human_result(&mut self, id: &str, allowed: bool) -> Result<bool> {
        if !allowed || self.state["pending_escalation"]["request_id"] != id {
            return Ok(false);
        }
        self.reset()?;
        Ok(true)
    }
    fn completed_step(&mut self, step: u64) -> Result<Option<String>> {
        if self.state["pending_escalation"]["step"].as_u64() != Some(step) {
            return Ok(None);
        }
        let id = self.state["pending_escalation"]["request_id"]
            .as_str()
            .map(str::to_owned);
        self.reset()?;
        Ok(id)
    }
    fn save(&self) -> Result<()> {
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(&self.state)?)?;
        fs::rename(tmp, &self.path)?;
        Ok(())
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
        self.save()
    }
}
pub fn result(decision: &str, reason: &str, tool: &str, grants: Option<Vec<String>>) -> Value {
    let tag = match decision {
        "allow" => "ALLOWED",
        "deny" => "DENIED",
        _ => "REVIEW REQUIRED",
    };
    let reason = if reason.trim().starts_with("[any-auto") {
        reason.trim().into()
    } else {
        format!("[any-auto: {tag}] {}", reason.trim())
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
    if std::env::var("ANY_AUTO_SILENT")
        .unwrap_or_default()
        .is_empty()
    {
        let color = match decision {
            "allow" => 32,
            "deny" => 31,
            _ => 33,
        };
        eprintln!(
            "\x1b[{color}m[any-auto: {}]\x1b[0m {tool} -> {reason}",
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
    audit::record(
        &id,
        "hook_input",
        json!({"input":audit::without_authorization(payload.clone())}),
    );
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
    // Antigravity parses hook responses with strict protojson. Only Pi's
    // extension accepts this correlation field for human confirmation.
    if config::mode() == config::Mode::Pi {
        output["request_id"] = json!(id);
    }
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
    if std::env::var_os("ANY_AUTO_REVIEWER").is_some() {
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
    if let Some(b) = breaker.as_mut() {
        let update = (|| -> Result<Option<String>> {
            if let Some(epoch) = crate::review_input::authorization_epoch(&payload["authorization"])
                && b.observe_user(&epoch)?
            {
                audit::record(
                    id,
                    "circuit_breaker_reset",
                    json!({"reason":"new_user_message",
                    "conversation_id":conversation_id(payload)}),
                );
            }
            let reason = b.tripped();
            if reason.is_some() {
                b.escalate(id, payload["stepIdx"].as_u64())?;
            }
            Ok(reason)
        })();
        match update {
            Ok(Some(reason)) => {
                *stage = "circuit_breaker";
                return result("force_ask", &reason, tool, None);
            }
            Ok(None) => {}
            Err(error) => {
                *stage = "state_error";
                return result(
                    "deny",
                    &format!("Cannot update circuit breaker: {error}"),
                    tool,
                    None,
                );
            }
        }
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
        .map(|b| b.record(assessment.outcome.as_str()))
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
    let grants = (assessment.outcome == crate::policy::Outcome::Allow)
        .then(|| parser::overrides(tool, args));
    result(
        assessment.outcome.as_str(),
        &assessment.rationale,
        tool,
        grants,
    )
}

/// Agy's PostToolUse callback identifies a completed step, not an approval.
/// Only the exact step that previously received force_ask may end the pause.
pub async fn post_tool(payload: &Value) -> Result<()> {
    if config::mode() == config::Mode::Pi {
        return Ok(());
    }
    let Some(session) = crate::sessions::user_session_id(payload) else {
        return Ok(());
    };
    let Some(step) = payload["stepIdx"].as_u64() else {
        return Ok(());
    };
    let mut breaker = Breaker::open_for_review(&config::state_dir(), session).await?;
    if let Some(id) = breaker.completed_step(step)? {
        audit::record(
            &id,
            "circuit_breaker_reset",
            json!({"reason":"escalated_tool_finished",
            "conversation_id":session,"step":step}),
        );
    }
    Ok(())
}

fn normalize(payload: &Value) -> Value {
    let mut value = payload.clone();
    if config::mode() != config::Mode::Pi
        && value["authorization"].is_null()
        && value["transcriptPath"].is_string()
    {
        let (auth, diagnostics) = crate::authorization::from_agy_hook_with_diagnostics(payload);
        value["authorization"] = auth;
        value["authorization_collection"] = diagnostics;
    }
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
    #[test]
    fn breaker_resets_entire_window_only_on_new_user_or_matching_completion() {
        let root = tempfile::tempdir().unwrap();
        let mut b = Breaker::open(root.path(), "session").unwrap();
        assert!(!b.observe_user("user-1").unwrap());
        for _ in 0..3 {
            b.record("deny").unwrap();
        }
        assert!(!b.observe_user("user-1").unwrap());
        assert!(b.tripped().is_some());
        b.escalate("request-1", Some(10)).unwrap();
        assert!(!b.human_result("other", true).unwrap());
        assert!(!b.human_result("request-1", false).unwrap());
        assert!(b.completed_step(11).unwrap().is_none());
        assert!(b.tripped().is_some());
        assert!(b.human_result("request-1", true).unwrap());
        b.record("deny").unwrap();
        assert!(b.tripped().is_none(), "old rolling window must be gone");
        assert!(b.observe_user("user-2").unwrap());
        for _ in 0..3 {
            b.record("deny").unwrap();
        }
        b.escalate("request-2", Some(20)).unwrap();
        drop(b);
        let mut b = Breaker::open(root.path(), "session").unwrap();
        assert_eq!(b.completed_step(20).unwrap().as_deref(), Some("request-2"));
        assert!(
            b.completed_step(20).unwrap().is_none(),
            "callback is consumed once"
        );
        b.record("deny").unwrap();
        assert!(b.tripped().is_none());
    }
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
