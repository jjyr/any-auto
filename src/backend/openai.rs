//! Direct Responses API backend; no tools are supplied to the model.
use super::{BackendFuture, SessionTransport};
use crate::{audit, config::ReviewerConfig};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct OpenAiBackend {
    workspace: PathBuf,
    config: ReviewerConfig,
}
impl OpenAiBackend {
    pub fn new(workspace: PathBuf, config: ReviewerConfig) -> Self {
        Self { workspace, config }
    }
    fn state(&self) -> PathBuf {
        self.workspace.join("response.json")
    }
    async fn review(&self, payload: &str, id: &str) -> Result<String> {
        let key = &self.config.approver.api_key;
        ensure!(!key.trim().is_empty(), "OpenAI approver API key is empty");
        let endpoint = format!(
            "{}/responses",
            self.config.approver.base_url.trim_end_matches('/')
        );
        let url = reqwest::Url::parse(&endpoint)?;
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "API base URL must not contain credentials, query, or fragment"
        );
        ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
            "API URL requires HTTPS except for localhost"
        );
        let mut body = json!({"model":self.config.approver.model,"instructions":self.config.prompt,"input":payload,"tools":[],"store":true});
        if let Some(effort) = &self.config.approver.effort {
            body["reasoning"] = json!({"effort":effort});
        }
        if let Ok(bytes) = std::fs::read(self.state()) {
            let previous: Value = serde_json::from_slice(&bytes)?;
            if let Some(id) = previous["id"].as_str() {
                body["previous_response_id"] = json!(id);
            }
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut response = client.post(url).bearer_auth(key).json(&body).send().await?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                "OpenAI response exceeds 4 MiB"
            );
            bytes.extend_from_slice(&chunk);
        }
        // Do not echo arbitrary HTTP bodies or authentication headers into logs.
        ensure!(
            status.is_success(),
            "OpenAI Responses request failed with HTTP {status}"
        );
        let value: Value = serde_json::from_slice(&bytes)?;
        let usage = &value["usage"];
        let tokens = match (
            usage["input_tokens"].as_u64(),
            usage["output_tokens"].as_u64(),
        ) {
            (Some(input), Some(output)) => json!({"input_tokens":input,"output_tokens":output}),
            _ => Value::Null,
        };
        audit::record(
            id,
            "backend_usage",
            json!({"provider":"openai","usage_delta":tokens}),
        );
        ensure!(
            value["status"] == "completed",
            "OpenAI response incomplete or failed"
        );
        let output = value["output"]
            .as_array()
            .context("Missing OpenAI output")?;
        ensure!(
            output
                .iter()
                .all(|v| matches!(v["type"].as_str(), Some("message" | "reasoning"))),
            "Unexpected OpenAI output type"
        );
        let text = output
            .iter()
            .filter(|v| v["type"] == "message")
            .filter_map(|v| v["content"].as_array())
            .flatten()
            .filter(|v| v["type"] == "output_text")
            .filter_map(|v| v["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        ensure!(!text.is_empty(), "OpenAI response has no assessment text");
        ensure!(value["id"].is_string(), "Missing OpenAI response ID");
        std::fs::create_dir_all(&self.workspace)?;
        crate::usage::save(&self.state(), &json!({"id":value["id"]}))?;
        audit::record(
            id,
            "backend_response",
            json!({"provider":"openai","stdout":text,"usage_delta":tokens,"exit_code":0,"model":value["model"],"effort_effective":value["reasoning"]["effort"]}),
        );
        Ok(text)
    }
}
impl SessionTransport for OpenAiBackend {
    fn create_session<'a>(&'a self, _: &'a ReviewerConfig, _: &'a str) -> BackendFuture<'a> {
        Box::pin(async move {
            let _ = std::fs::remove_file(self.state());
            Ok(self.workspace.to_string_lossy().into_owned())
        })
    }
    fn send_message<'a>(
        &'a self,
        cid: &'a str,
        payload: &'a str,
        id: &'a str,
    ) -> BackendFuture<'a> {
        Box::pin(async move {
            ensure!(
                cid == self.workspace.to_string_lossy(),
                "OpenAI session mismatch"
            );
            let started = Instant::now();
            audit::record(id, "backend_request", json!({"provider":"openai"}));
            let result = self.review(payload, id).await;
            if let Err(e) = &result {
                audit::record(
                    id,
                    "backend_error",
                    json!({"provider":"openai","error":e.to_string(),"duration_ms":started.elapsed().as_millis()}),
                );
            }
            result
        })
    }
}
