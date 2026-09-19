use crate::{audit, config};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{process::Stdio, time::Duration};
use tokio::process::Command;

pub(super) async fn call(
    program: &str,
    command: Command,
    args: &[&str],
    id: &str,
) -> Result<String> {
    call_with_usage(program, command, args, id, |_| None).await
}

pub(super) async fn call_with_usage(
    program: &str,
    mut command: Command,
    args: &[&str],
    id: &str,
    usage: impl FnOnce(&str) -> Option<crate::usage::Tokens>,
) -> Result<String> {
    let started = std::time::Instant::now();
    let path = config::backend_path().context("Cannot construct backend search PATH")?;
    let operation = args.first().copied().unwrap_or("unknown");
    audit::record(
        id,
        "backend_request",
        json!({"command":program, "args":args, "operation":operation,
                "daemon_pid":std::process::id(),
                "search_path":std::env::split_paths(&path).collect::<Vec<_>>(),
                "fallback_directory":config::home().join(".gemini/antigravity-cli/bin")}),
    );
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        command.env("PATH", &path);
        let child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ("spawn", e))?;
        child.wait_with_output().await.map_err(|e| ("output", e))
    })
    .await;
    let output = match result {
        Ok(Ok(output)) => output,
        error => {
            let (stage, message) = match error {
                Ok(Err((stage, e))) => (
                    stage,
                    format!(
                        "{program} {operation} failed during {stage} (inherited PATH plus CLI fallback): {e}"
                    ),
                ),
                Err(_) => ("timeout", format!("{program} {operation} timed out")),
                _ => unreachable!(),
            };
            audit::record(
                id,
                "backend_error",
                json!({"error":message,"stage":stage,"operation":operation,"duration_ms":started.elapsed().as_millis()}),
            );
            bail!("{message}");
        }
    };
    let raw = String::from_utf8_lossy(&output.stdout);
    let usage_delta = if output.status.success() {
        usage(&raw)
    } else {
        None
    };
    audit::record(
        id,
        "backend_response",
        json!({"stdout":raw, "usage_delta":usage_delta,
            "stderr":String::from_utf8_lossy(&output.stderr), "exit_code":output.status.code(),
            "duration_ms":started.elapsed().as_millis()}),
    );
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let details = [stderr.trim(), stdout.trim()]
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "{program} {operation} exited with {}: {}",
            output.status,
            if details.is_empty() {
                "no stdout or stderr"
            } else {
                &details
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}
