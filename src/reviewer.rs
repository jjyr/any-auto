pub use crate::review_input::ReviewInput;
use crate::{audit, config, parser};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assessment {
    pub outcome: String,
    pub risk_level: String,
    pub user_authorization: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<Value>,
}
impl Assessment {
    pub fn deny(reason: impl std::fmt::Display) -> Self {
        Self {
            outcome: "deny".into(),
            risk_level: "high".into(),
            user_authorization: "unknown".into(),
            rationale: format!("Fail-closed: {reason}"),
            error_stage: Some("reviewer".into()),
            reviewer: None,
        }
    }
}
pub fn parse(raw: &str) -> Assessment {
    if raw.trim().is_empty() {
        return Assessment::deny("LLM review completed without an assessment payload");
    }
    let mut data = serde_json::from_str::<Value>(raw.trim()).ok();
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
    let Some(v) = data.filter(Value::is_object) else {
        return Assessment::deny("Assessment payload was not valid JSON");
    };
    // An invalid, nonempty outcome must not fall back to a more permissive alias.
    let outcome_value = v
        .get("outcome")
        .filter(|value| match value {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64() != Some(0.0),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        })
        .or_else(|| v.get("decision"));
    let outcome = outcome_value
        .and_then(Value::as_str)
        .unwrap_or("deny")
        .trim()
        .to_lowercase();
    let outcome = if matches!(outcome.as_str(), "allow" | "deny") {
        outcome
    } else {
        "deny".into()
    };
    let allow = outcome == "allow";
    Assessment {
        error_stage: None,
        reviewer: None,
        outcome,
        risk_level: v["risk_level"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(if allow { "low" } else { "high" })
            .trim()
            .into(),
        user_authorization: v["user_authorization"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .trim()
            .into(),
        rationale: v["rationale"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .or(v["reason"].as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(if allow {
                "Auto-review returned a low-risk allow decision."
            } else {
                "Auto-review returned a deny decision without a rationale."
            })
            .trim()
            .into(),
    }
}
pub fn script_content(cmd: &str, workspaces: &[Value]) -> String {
    for command in parser::commands(cmd) {
        for token in command.split_whitespace() {
            // Inspect script candidates with the established 4000-character limit.
            if ![
                ".sh", ".py", ".js", ".ts", ".bash", ".zsh", ".rb", ".mjs", ".cjs",
            ]
            .iter()
            .any(|ext| token.ends_with(ext))
            {
                continue;
            }
            let candidate = token.trim_matches(['\'', '"']);
            for ws in workspaces.iter().filter_map(Value::as_str) {
                if let Ok(bytes) = std::fs::read(PathBuf::from(ws).join(candidate)) {
                    let content: String =
                        String::from_utf8_lossy(&bytes).chars().take(4000).collect();
                    return format!(
                        "\n[Extracted Content of Script '{candidate}']:\n```\n{content}\n```\n"
                    );
                }
            }
        }
    }
    String::new()
}
pub struct Bridge {
    backend: Option<Box<dyn crate::backend::Backend>>,
    mode: config::Mode,
    fingerprint: Option<String>,
    settings: Option<config::ReviewerConfig>,
    pub conversation_id: Option<String>,
    path: PathBuf,
    temporary: Option<tempfile::TempDir>,
}
impl Bridge {
    pub fn persistent(mode: config::Mode, directory: PathBuf) -> Self {
        Self::new(mode, directory, None)
    }
    pub fn temporary(mode: config::Mode) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("any-auto-review-")
            .tempdir()?;
        Ok(Self::new(
            mode,
            directory.path().to_path_buf(),
            Some(directory),
        ))
    }
    fn new(mode: config::Mode, directory: PathBuf, temporary: Option<tempfile::TempDir>) -> Self {
        let path = directory.join("reviewer_session.json");
        let data: Value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        let conversation_id = data["conversationId"]
            .as_str()
            .or(data["conversation_id"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        Self {
            backend: None,
            mode,
            fingerprint: data["config_fingerprint"].as_str().map(str::to_owned),
            settings: None,
            temporary,
            conversation_id,
            path,
        }
    }
}
impl Bridge {
    fn configure(&mut self) -> Result<()> {
        use sha2::{Digest, Sha256};
        let config = config::reviewer_config_for(self.mode)?;
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(
                &serde_json::json!({"api_key_hash":format!("{:x}", Sha256::digest(config.approver.api_key.as_bytes())),"approver":config.approver,"prompt":config.prompt,"runtime_fingerprint":crate::context::current().map(|c| c.fingerprint())})
            )?)
        );
        if self.fingerprint.as_ref() != Some(&fingerprint) {
            self.conversation_id = None;
            self.backend = None;
            let _ = std::fs::remove_file(&self.path);
        }
        if self.backend.is_none() {
            let workspace = self
                .path
                .parent()
                .unwrap()
                .join("workspace")
                .join(&fingerprint[..16]);
            // Keep the established agy workspace path; Pi/API sessions live under a config generation.
            let workspace = if config.approver.provider == config::Provider::Cli {
                self.path.parent().unwrap().join("workspace")
            } else {
                workspace
            };
            self.backend = Some(crate::backend::for_config(
                &config,
                workspace,
                self.path.clone(),
                fingerprint.clone(),
                self.conversation_id.clone(),
                self.temporary.is_none(),
            )?);
        }
        self.fingerprint = Some(fingerprint);
        self.settings = Some(config);
        Ok(())
    }
    pub async fn evaluate(&mut self, req: &Value) -> Assessment {
        if let Err(e) = self.configure() {
            return Assessment::deny(format!("Invalid reviewer configuration: {e:#}"));
        }

        let input = ReviewInput::from_request(req);
        let id = &input.request_id;
        audit::record(
            id,
            "reviewer_input",
            json!({"action":input.state["action"],
            "completeness":input.state["completeness"], "authorization_diagnostics":input.authorization_diagnostics, "user_session_id":req["user_session_id"]}),
        );
        let backend = self.backend.as_mut().expect("configured backend");
        let mut assessment = match backend.review(&input).await {
            Ok(value) => value,
            Err(err) => Assessment::deny(format!("Approver review failed: {err:#}")),
        };
        self.conversation_id = backend.conversation_id().map(str::to_owned);
        if let Some(settings) = &self.settings {
            let metadata = assessment.reviewer.get_or_insert_with(|| json!({}));
            metadata["provider"] = json!(settings.approver.provider);
            metadata["requested_model"] = json!(settings.approver.model);
            if metadata["model"].is_null() {
                metadata["model"] = json!(settings.approver.model);
            }
            metadata["effort_requested"] = json!(settings.approver.effort);
        }
        audit::record(id, "reviewer_result", json!({"assessment":assessment}));
        assessment
    }
}
