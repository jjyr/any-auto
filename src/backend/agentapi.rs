use super::{BackendFuture, SessionTransport, conversation_id, process};
use crate::config::ReviewerConfig;
use anyhow::Context;
use serde_json::Value;
use tokio::process::Command;

pub struct AgentApiBackend {
    pub request_timeout: u64,
}

impl SessionTransport for AgentApiBackend {
    fn create_session<'a>(&'a self, config: &'a ReviewerConfig, id: &'a str) -> BackendFuture<'a> {
        Box::pin(async move {
            let model = config
                .approver
                .model
                .as_ref()
                .map(|m| format!("--model={m}"));
            let mut args = vec!["new-conversation", "--title=Guardian Approver Session"];
            if let Some(model) = &model {
                args.push(model);
            }
            args.push(&config.prompt);
            let raw = process::call(
                "agentapi",
                Command::new("agentapi"),
                &args,
                id,
                self.request_timeout,
            )
            .await?;
            let v: Value = serde_json::from_str(&raw).context("Invalid session response JSON")?;
            conversation_id(&v)
        })
    }
    fn send_message<'a>(
        &'a self,
        cid: &'a str,
        payload: &'a str,
        id: &'a str,
    ) -> BackendFuture<'a> {
        Box::pin(async move {
            let raw = process::call(
                "agentapi",
                Command::new("agentapi"),
                &["send-message", cid, payload],
                id,
                self.request_timeout,
            )
            .await?;
            if let Ok(v) = serde_json::from_str::<Value>(&raw)
                && let Some(response) = v.get("response")
            {
                return response
                    .as_str()
                    .map(str::to_owned)
                    .context("Backend response must be a string");
            }
            Ok(raw)
        })
    }
}
