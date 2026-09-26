use crate::config;
use std::path::{Path, PathBuf};

pub fn display_path(path: &Path) -> String {
    let home = config::home();
    if let Ok(rest) = path.strip_prefix(&home) {
        format!("~/{}", rest.display())
    } else {
        path.display().to_string()
    }
}

pub fn is_cli_detected() -> bool {
    super::executable("agy")
        || config::home()
            .join(".gemini/antigravity-cli/bin/agy")
            .is_file()
}

pub fn is_desktop_detected() -> bool {
    super::executable("antigravity")
        || [
            PathBuf::from("/Applications/Antigravity.app"),
            config::home().join("Applications/Antigravity.app"),
        ]
        .iter()
        .any(|p| p.is_dir())
}

pub fn is_cli_installed() -> bool {
    let path = config::home().join(".gemini/config/hooks.json");
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| value.get("any-auto").is_some())
}

pub fn is_cli_enabled() -> bool {
    let path = config::home().join(".gemini/config/hooks.json");
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|v| v["any-auto"]["enabled"] == true)
}

pub fn print_cli_turbo_check() {
    let path = config::home().join(".gemini/antigravity-cli/settings.json");
    let settings = (|| -> anyhow::Result<serde_json::Value> {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        anyhow::ensure!(value.is_object(), "expected a JSON object");
        Ok(value)
    })();
    let healthy = match settings {
        Ok(value) => {
            let mode = match value.get("toolPermission") {
                None => "request-review (default)",
                Some(v) => v.as_str().unwrap_or("invalid value"),
            };
            let sandbox = match value.get("enableTerminalSandbox") {
                None => "false (default)",
                Some(serde_json::Value::Bool(false)) => "false",
                Some(serde_json::Value::Bool(true)) => "true",
                Some(_) => "invalid value",
            };
            let healthy = mode == "always-proceed"
                && (value.get("enableTerminalSandbox").is_none()
                    || value["enableTerminalSandbox"] == false);
            println!(
                "  {} CLI Turbo mode: toolPermission={mode}, enableTerminalSandbox={sandbox}",
                if healthy { "✓" } else { "✗" }
            );
            healthy
        }
        Err(error) => {
            println!(
                "  ✗ CLI Turbo mode: cannot read settings from {}: {error}",
                display_path(&path)
            );
            false
        }
    };
    if !healthy {
        println!(
            "    Expected always-proceed with terminal sandbox off. Run: any-auto install --agents agy-cli, then restart agy."
        );
    }
}

pub fn is_desktop_installed() -> bool {
    let path = config::home().join(".gemini/config/config.json");
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| value["sidecars"].get("any-auto/approver").is_some())
}

pub fn is_desktop_enabled() -> bool {
    let path = config::home().join(".gemini/config/config.json");
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|v| v["sidecars"]["any-auto/approver"]["enabled"] == true)
}
