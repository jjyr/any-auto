//! One cancellable, persistent RPC child per reviewer conversation.
use super::{Backend, BackendFuture};
use crate::{audit, config::ReviewerConfig};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

pub struct PiBackend {
    workspace: PathBuf,
    config: ReviewerConfig,
    rpc: Mutex<Option<Rpc>>,
}
// Drop cannot await child.wait(). Track asynchronous cleanup tasks so daemon
// shutdown can wait for terminated Pi children to be reaped before exiting.
static CHILD_CLEANUP_TASKS: std::sync::LazyLock<
    std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));
pub(super) async fn reap_children() {
    let tasks = std::mem::take(&mut *CHILD_CLEANUP_TASKS.lock().unwrap());
    for task in tasks {
        let _ = task.await;
    }
}
struct Rpc {
    child: Option<Child>,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    stderr: tokio::task::JoinHandle<()>,
    sequence: u64,
    metadata: Value,
}
impl Drop for Rpc {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let mut cleanup_tasks = CHILD_CLEANUP_TASKS.lock().unwrap();
            cleanup_tasks.retain(|task| !task.is_finished());
            cleanup_tasks.push(tokio::spawn(async move {
                let _ = child.wait().await;
            }));
        }
        self.stderr.abort();
    }
}
impl Rpc {
    async fn read(&mut self) -> Result<Value> {
        let mut bytes = Vec::new();
        loop {
            let chunk = self.output.fill_buf().await?;
            ensure!(!chunk.is_empty(), "Pi RPC closed stdout");
            let end = chunk.iter().position(|b| *b == b'\n').map(|n| n + 1);
            let n = end.unwrap_or(chunk.len());
            ensure!(
                bytes.len() + n <= 4 * 1024 * 1024,
                "Pi RPC frame exceeds 4 MiB"
            );
            bytes.extend_from_slice(&chunk[..n]);
            self.output.consume(n);
            if end.is_some() {
                return serde_json::from_slice(&bytes).context("Invalid Pi RPC JSONL");
            }
        }
    }
    async fn write(&mut self, mut value: Value) -> Result<String> {
        self.sequence += 1;
        let id = format!("rpc-{}", self.sequence);
        value["id"] = json!(id);
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await?;
        self.input.flush().await?;
        Ok(id)
    }
    async fn command(&mut self, value: Value) -> Result<Value> {
        let command = value["type"].clone();
        let id = self.write(value).await?;
        loop {
            let value = self.read().await?;
            if value["type"] == "response" && value["id"] == id {
                ensure!(
                    value["command"] == command && value["success"] == true,
                    "Pi RPC command failed: {value}"
                );
                return Ok(value["data"].clone());
            }
        }
    }
    async fn review(&mut self, payload: &str, request_id: &str) -> Result<(String, Value)> {
        let before = self.command(json!({"type":"get_session_stats"})).await?;
        let id = self
            .write(json!({"type":"prompt","message":payload}))
            .await?;
        let mut accepted = false;
        let mut settled = false;
        let mut message = Value::Null;
        loop {
            let event = self.read().await?;
            match event["type"].as_str() {
                Some("response") if event["id"] == id => {
                    ensure!(
                        event["command"] == "prompt" && event["success"] == true,
                        "Pi rejected prompt: {event}"
                    );
                    accepted = true;
                }
                Some("message_end") if event["message"]["role"] == "assistant" => {
                    message = event["message"].clone();
                }
                Some("tool_execution_start" | "extension_ui_request") => {
                    bail!("Reviewer unexpectedly requested a tool or UI")
                }
                Some("agent_settled") => settled = true,
                _ => {}
            }
            if accepted && settled {
                break;
            }
        }
        let after = self.command(json!({"type":"get_session_stats"})).await?;
        let delta = |key: &str| {
            after["tokens"][key]
                .as_u64()?
                .checked_sub(before["tokens"][key].as_u64()?)
        };
        let usage = (|| {
            Some(crate::usage::Tokens {
                input_tokens: delta("input")?
                    .checked_add(delta("cacheRead")?)?
                    .checked_add(delta("cacheWrite")?)?,
                output_tokens: delta("output")?,
            })
        })();
        audit::record(
            request_id,
            "backend_usage",
            json!({"provider":"pi","usage_delta":usage}),
        );
        ensure!(
            !message.is_null(),
            "Pi settled without an assistant message"
        );
        ensure!(
            message["stopReason"] == "stop",
            "Pi review did not complete: {}",
            message["stopReason"]
        );
        let content = message["content"]
            .as_array()
            .context("Missing Pi assistant content")?;
        ensure!(
            !content.iter().any(|c| c["type"] == "toolCall"),
            "Reviewer returned a tool call"
        );
        let text = content
            .iter()
            .filter(|c| c["type"] == "text")
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        Ok((text, json!(usage)))
    }
}
impl PiBackend {
    pub fn new(workspace: PathBuf, config: ReviewerConfig) -> Self {
        Self {
            workspace,
            config,
            rpc: Mutex::new(None),
        }
    }
    fn session(&self) -> PathBuf {
        self.workspace.join("session.jsonl")
    }
    async fn spawn(&self, id: &str) -> Result<Rpc> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&self.workspace)?;
        std::fs::set_permissions(&self.workspace, std::fs::Permissions::from_mode(0o700))?;
        if self.workspace.join("in-flight").exists() {
            let _ = std::fs::remove_file(self.session());
            let _ = std::fs::remove_file(self.workspace.join("in-flight"));
        }
        // Keep Pi's native auth path and OAuth lock identity. Startup model/thinking
        // options do not persist defaults; RPC setters on Pi 0.84.2 can persist them.
        let source = crate::context::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::config::home().join(".pi/agent"));
        let mut command = Command::new("pi");
        crate::context::apply(&mut command);
        command
            .current_dir(&self.workspace)
            .env("PI_CODING_AGENT_DIR", &source)
            .env("PI_OFFLINE", "1")
            .env("ANY_AUTO_REVIEWER", "1")
            .env("PATH", crate::config::backend_path()?)
            .args([
                "--mode",
                "rpc",
                "--no-tools",
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--no-context-files",
                "--no-themes",
                "--no-approve",
                "--session",
            ])
            .arg(self.session())
            .arg("--system-prompt")
            .arg(&self.config.prompt)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(model) = &self.config.approver.model {
            command.arg("--model").arg(model);
        }
        if let Some(effort) = &self.config.approver.effort {
            command.arg("--thinking").arg(effort);
        }
        let mut child = command
            .spawn()
            .context("Cannot spawn pi RPC (requires Pi 0.84.2 or newer)")?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut errors = child.stderr.take().unwrap();
        let stderr = tokio::spawn(async move {
            let mut bytes = [0; 4096];
            while let Ok(n) = errors.read(&mut bytes).await {
                if n == 0 {
                    break;
                }
            }
        });
        let mut rpc = Rpc {
            child: Some(child),
            input,
            output,
            stderr,
            sequence: 0,
            metadata: Value::Null,
        };
        if let Some(effort) = &self.config.approver.effort {
            let available = rpc
                .command(json!({"type":"get_available_thinking_levels"}))
                .await?;
            ensure!(
                available["levels"]
                    .as_array()
                    .is_some_and(|levels| levels.contains(&json!(effort))),
                "Pi model does not support effort {effort}; available: {available}"
            );
        }
        let state = rpc.command(json!({"type":"get_state"})).await?;
        ensure!(!state["model"].is_null(), "Pi has no configured model");
        if let Some(effort) = &self.config.approver.effort {
            ensure!(
                state["thinkingLevel"] == *effort,
                "Pi changed requested effort"
            );
        }
        audit::record(
            id,
            "backend_ready",
            json!({"provider":"pi","model":state["model"]["id"],"model_provider":state["model"]["provider"],"effort_effective":state["thinkingLevel"]}),
        );
        rpc.metadata = json!({"model": state["model"]["id"], "model_provider":state["model"]["provider"], "effort_effective":state["thinkingLevel"]});
        Ok(rpc)
    }
}
impl Backend for PiBackend {
    fn create_session<'a>(&'a self, _: &'a ReviewerConfig, _: &'a str) -> BackendFuture<'a> {
        Box::pin(async move {
            self.rpc.lock().await.take();
            let _ = std::fs::remove_file(self.session());
            Ok(self.session().to_string_lossy().into_owned())
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
                cid == self.session().to_string_lossy(),
                "Pi session path mismatch"
            );
            let mut slot = self.rpc.lock().await;
            // Taking ownership makes timeout/drop kill the child rather than retain a busy RPC.
            let existing = slot.take();
            let started = Instant::now();
            audit::record(
                id,
                "backend_request",
                json!({"provider":"pi","operation":"prompt"}),
            );
            let result = tokio::time::timeout(Duration::from_secs(20), async {
                let mut rpc = match existing {
                    Some(rpc) => rpc,
                    None => self.spawn(id).await?,
                };
                std::fs::write(self.workspace.join("in-flight"), b"1")?;
                let (text, usage) = rpc.review(payload, id).await?;
                std::fs::remove_file(self.workspace.join("in-flight"))?;
                let metadata = rpc.metadata.clone();
                *slot = Some(rpc);
                Ok::<_, anyhow::Error>((text, usage, metadata))
            })
            .await
            .context("Pi RPC approval timed out")
            .and_then(|v| v);
            match result {
                Ok((text, usage, metadata)) => {
                    audit::record(
                        id,
                        "backend_response",
                        json!({"provider":"pi","stdout":text,"usage_delta":usage,"exit_code":0,"duration_ms":started.elapsed().as_millis(),"model":metadata["model"],"model_provider":metadata["model_provider"],"effort_effective":metadata["effort_effective"]}),
                    );
                    Ok(text)
                }
                Err(e) => {
                    audit::record(
                        id,
                        "backend_error",
                        json!({"provider":"pi","error":e.to_string(),"duration_ms":started.elapsed().as_millis()}),
                    );
                    Err(e)
                }
            }
        })
    }
}
