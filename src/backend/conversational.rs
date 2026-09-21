//! Shared conversation lifecycle for the existing session transports.
use super::{Backend, ReviewFuture, SessionTransport};
use crate::{audit, config::ReviewerConfig, policy, reviewer::ReviewInput};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct ConversationalBackend {
    pub transport: Box<dyn SessionTransport>,
    pub config: ReviewerConfig,
    pub conversation_id: Option<String>,
    pub path: PathBuf,
    pub fingerprint: String,
    pub persistent: bool,
}
impl ConversationalBackend {
    async fn session(&mut self, id: &str) -> Result<String> {
        if let Some(cid) = &self.conversation_id {
            audit::record(
                id,
                "reviewer_session",
                json!({"conversation_id":cid,"reused":true}),
            );
            return Ok(cid.clone());
        }
        let config = &self.config;
        let cid = self.transport.create_session(config, id).await?;
        audit::record(
            id,
            "reviewer_session",
            json!({"conversation_id":cid,"reused":false}),
        );
        if self.persistent {
            std::fs::create_dir_all(self.path.parent().unwrap()).with_context(|| {
                format!(
                    "Cannot create reviewer state directory for {}",
                    self.path.display()
                )
            })?;
            let mut state: Value = std::fs::read(&self.path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            if state["conversationId"] != cid {
                state = json!({});
            }
            state["conversationId"] = cid.clone().into();
            state["config_fingerprint"] = serde_json::json!(self.fingerprint);
            crate::usage::save(&self.path, &state).with_context(|| {
                format!("Cannot save reviewer session to {}", self.path.display())
            })?;
        }
        self.conversation_id = Some(cid.clone());
        Ok(cid)
    }
    async fn send(&mut self, payload: &str, id: &str) -> Result<String> {
        let cid = self.session(id).await?;
        self.transport.send_message(&cid, payload, id).await
    }
}
impl Backend for ConversationalBackend {
    fn conversation_id(&self) -> Option<&str> {
        self.conversation_id.as_deref()
    }
    fn review<'a>(&'a mut self, input: &'a ReviewInput) -> ReviewFuture<'a> {
        Box::pin(async move {
            let id = &input.request_id;
            let cached = self.conversation_id.is_some();
            let payload = input.conversational_payload().to_string();
            let mut result = self.send(&payload, id).await;
            if result.is_err() && cached {
                audit::record(
                    id,
                    "reviewer_retry",
                    json!({"reason":"Cached session failed; recreating conversation"}),
                );
                self.conversation_id = None;
                let _ = std::fs::remove_file(&self.path);
                result = self.send(&payload, id).await;
            }
            result.and_then(|raw| {
                Ok(policy::Review {
                    judgment: policy::parse(&raw)?,
                    metadata: json!({}),
                })
            })
        })
    }
}
