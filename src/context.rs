//! Request-local routing and backend environment. Never mutate process environment.
use crate::config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::OsString, sync::Arc};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestContext {
    pub mode: config::Mode,
    pub instance: String,
    pub environment: BTreeMap<String, String>,
}
tokio::task_local! { static CURRENT: Arc<RequestContext>; }
pub fn current() -> Option<Arc<RequestContext>> {
    CURRENT.try_with(Arc::clone).ok()
}
pub async fn scope<F: std::future::Future>(context: Arc<RequestContext>, future: F) -> F::Output {
    CURRENT.scope(context, future).await
}
fn relevant(key: &str) -> bool {
    matches!(
        key,
        "PATH"
            | "PI_CODING_AGENT_DIR"
            | "HF_TOKEN"
            | "COPILOT_GITHUB_TOKEN"
            | "CLOUDFLARE_ACCOUNT_ID"
            | "GOOGLE_APPLICATION_CREDENTIALS"
            | "AWS_PROFILE"
            | "AWS_REGION"
            | "AWS_DEFAULT_REGION"
            | "AWS_ACCESS_KEY_ID"
            | "AWS_SECRET_ACCESS_KEY"
            | "AWS_SESSION_TOKEN"
            | "HTTPS_PROXY"
            | "HTTP_PROXY"
            | "ALL_PROXY"
            | "NO_PROXY"
            | "https_proxy"
            | "http_proxy"
            | "all_proxy"
            | "no_proxy"
    ) || key.starts_with("ANTIGRAVITY_")
        || key.starts_with("AWS_")
        || key.starts_with("GOOGLE_CLOUD_")
        || key.starts_with("AZURE_OPENAI_")
        || matches!(
            key,
            "ANY_AUTO_PROVIDER"
                | "ANY_AUTO_APPROVER_MODEL"
                | "ANY_AUTO_EFFORT"
                | "ANY_AUTO_MODEL"
                | "ANY_AUTO_CLI_MODEL"
                | "ANY_AUTO_PROMPT"
                | "ANY_AUTO_BASE_URL"
                | "ANY_AUTO_API_KEY"
        )
        || key.ends_with("_API_KEY")
        || key.ends_with("_AUTH_TOKEN")
        || key.ends_with("_OAUTH_TOKEN")
}
impl RequestContext {
    pub fn capture() -> Result<Self> {
        let mut environment: BTreeMap<_, _> =
            std::env::vars().filter(|(key, _)| relevant(key)).collect();
        let config = config::reviewer_config()?;
        if config.approver.provider != config::Provider::Agentapi {
            environment.retain(|k, _| !k.starts_with("ANTIGRAVITY_"));
        }
        Ok(Self {
            mode: config::mode(),
            instance: config::instance(),
            environment,
        })
    }
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.instance.is_empty()
                && self.instance.len() <= 64
                && self
                    .instance
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
            "Invalid instance"
        );
        anyhow::ensure!(
            self.environment
                .keys()
                .all(|k| !k.is_empty() && !k.contains(['=', '\0']))
                && self.environment.values().all(|v| !v.contains('\0')),
            "Invalid environment context"
        );
        Ok(())
    }
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).expect("serializable context"))
        )
    }
}
pub fn var_os(key: &str) -> Option<OsString> {
    match current() {
        Some(c) => c.environment.get(key).map(OsString::from),
        None => std::env::var_os(key),
    }
}
pub fn var(key: &str) -> Result<String, std::env::VarError> {
    var_os(key)
        .ok_or(std::env::VarError::NotPresent)?
        .into_string()
        .map_err(std::env::VarError::NotUnicode)
}
pub fn apply(command: &mut tokio::process::Command) {
    if let Some(context) = current() {
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(relevant) {
                command.env_remove(key);
            }
        }
        command.envs(&context.environment);
    }
}
