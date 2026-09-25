//! Unified typed reviews; conversational transports remain an internal implementation detail.
mod agentapi;
mod agy;
mod conversational;
pub(crate) mod jev;
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

pub trait SessionTransport: Send + Sync {
    fn create_session<'a>(&'a self, config: &'a ReviewerConfig, id: &'a str) -> BackendFuture<'a>;
    fn send_message<'a>(&'a self, cid: &'a str, payload: &'a str, id: &'a str)
    -> BackendFuture<'a>;
}

pub type ReviewFuture<'a> =
    Pin<Box<dyn Future<Output = Result<crate::policy::Review>> + Send + 'a>>;
pub trait Backend: Send + Sync {
    fn review<'a>(&'a mut self, input: &'a crate::reviewer::ReviewInput) -> ReviewFuture<'a>;
    fn conversation_id(&self) -> Option<&str> {
        None
    }
}

pub fn for_config(
    config: &ReviewerConfig,
    workspace: PathBuf,
    path: PathBuf,
    fingerprint: String,
    conversation_id: Option<String>,
    persistent: bool,
) -> Result<Box<dyn Backend>> {
    let transport: Box<dyn SessionTransport> = match config.approver.provider {
        Provider::Jev => return Ok(Box::new(jev::JevBackend::new(config.clone())?)),
        Provider::Agentapi => Box::new(AgentApiBackend {
            request_timeout: config.approver.request_timeout,
        }),
        Provider::Cli => Box::new(AgyBackend::new(workspace, config.clone())),
        Provider::Pi => Box::new(pi::PiBackend::new(workspace, config.clone())),
        Provider::Openai => Box::new(openai::OpenAiBackend::new(workspace, config.clone())),
    };
    Ok(Box::new(conversational::ConversationalBackend {
        transport,
        config: config.clone(),
        conversation_id,
        path,
        fingerprint,
        persistent,
    }))
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
