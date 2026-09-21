pub use crate::review_input::ReviewInput;
use crate::{audit, config};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assessment {
    pub outcome: crate::policy::Outcome,
    pub risk_level: crate::policy::Risk,
    pub user_authorization: crate::policy::Authorization,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<Value>,
}
impl Assessment {
    pub fn deny(reason: impl std::fmt::Display) -> Self {
        Self {
            outcome: crate::policy::Outcome::Deny,
            risk_level: crate::policy::Risk::High,
            user_authorization: crate::policy::Authorization::Unknown,
            rationale: format!("Fail-closed: {reason}"),
            error_stage: Some("reviewer".into()),
            reviewer: None,
        }
    }
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
                &serde_json::json!({"api_key_hash":format!("{:x}", Sha256::digest(config.approver.api_key.as_bytes())),"approver":config.approver,"prompt":config.prompt,"policy_version":crate::policy::DECISION_POLICY_VERSION,"runtime_fingerprint":crate::context::current().map(|c| c.fingerprint())})
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

        let input = ReviewInput::from_request_with_budget(
            req,
            self.settings
                .as_ref()
                .unwrap()
                .approver
                .context_budget_bytes,
        );
        let id = &input.request_id;
        audit::record(
            id,
            "reviewer_input",
            json!({"action":input.state["action"],
            "completeness":input.state["completeness"], "authorization_diagnostics":input.authorization_diagnostics, "user_session_id":req["user_session_id"]}),
        );
        let backend = self.backend.as_mut().expect("configured backend");
        let mut assessment = match backend.review(&input).await {
            Ok(value) => crate::policy::decide(
                value,
                &input,
                self.settings
                    .as_ref()
                    .unwrap()
                    .approver
                    .probability_threshold,
            ),
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
