//! Session-oriented backend interface, independent of executable and response format.
mod agentapi;
mod agy;
mod openai;
mod pi;
mod process;

use crate::config::{Provider, ReviewerConfig};
pub use agentapi::AgentApiBackend;
pub use agy::AgyBackend;
use anyhow::{Context, Result};
use serde_json::Value;
use std::{future::Future, path::PathBuf, pin::Pin};

// Boxed Send futures keep the async interface usable through a trait object.
pub type BackendFuture<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;

pub trait Backend: Send + Sync {
    fn create_session<'a>(&'a self, config: &'a ReviewerConfig, id: &'a str) -> BackendFuture<'a>;
    fn send_message<'a>(&'a self, cid: &'a str, payload: &'a str, id: &'a str)
    -> BackendFuture<'a>;
}

pub fn for_config(config: &ReviewerConfig, workspace: PathBuf) -> Box<dyn Backend> {
    match config.approver.provider {
        Provider::Agentapi => Box::new(AgentApiBackend),
        Provider::Cli => Box::new(AgyBackend::new(workspace, config.clone())),
        Provider::Pi => Box::new(pi::PiBackend::new(workspace, config.clone())),
        Provider::Openai => Box::new(openai::OpenAiBackend::new(workspace, config.clone())),
    }
}

fn conversation_id(v: &Value) -> Result<String> {
    v["conversationId"]
        .as_str()
        .or(v["conversation_id"].as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .context("No conversationId in backend output")
}

pub async fn reap_children() {
    pi::reap_children().await;
}
