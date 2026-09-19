use std::{
    fs,
    process::{Command, Stdio},
};
fn command(home: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_agy-auto-approve"));
    c.env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("AGY_AUTO_APPROVE_PROVIDER")
        .env_remove("AGY_AUTO_APPROVE_EFFORT")
        .env_remove("AGY_AUTO_APPROVE_APPROVER_MODEL")
        .stdin(Stdio::null());
    c
}
#[test]
fn overview_and_xdg_paths_are_host_neutral() {
    let home = tempfile::tempdir().unwrap();
    let out = command(home.path())
        .args(["config", "--json"])
        .env("XDG_CONFIG_HOME", home.path().join("custom"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["hosts"].as_array().unwrap().len(), 3);
    assert_eq!(
        v["file"],
        home.path()
            .join("custom/agy-auto-approve/config.toml")
            .to_str()
            .unwrap()
    );
    assert!(!home.path().join(".gemini").exists());
}
#[test]
fn root_without_terminal_fails_and_flags_do_not_open_tui() {
    let home = tempfile::tempdir().unwrap();
    let out = command(home.path()).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("TUI requires a terminal"));
    let out = command(home.path())
        .args(["--host", "pi"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Choose a subcommand"));
}
#[test]
fn host_group_limit_preserves_quiet_hosts_and_outcomes() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("logs");
    fs::create_dir_all(&logs).unwrap();
    let now = chrono::Utc::now();
    let mut rows = String::new();
    for (id, host) in [("a", "pi"), ("b", "agy-cli"), ("c", "agy-cli")] {
        rows += &format!(
            "{}\n",
            serde_json::json!({"schema_version":3,"id":id,"host":host,"mode":if host=="pi" {"pi"} else {"cli"},"timestamp":now.to_rfc3339(),"event":"hook_result","data":{"stage":"whitelist","output":{"decision":"allow"}}})
        );
    }
    fs::write(
        logs.join(format!("approvals-{}.jsonl", now.format("%Y-%m-%d"))),
        rows,
    )
    .unwrap();
    let out = command(home.path())
        .env("AGY_AUTO_APPROVE_LOG_DIR", &logs)
        .args(["logs", "--limit", "1", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["pi"].as_array().unwrap().len(), 1);
    assert_eq!(v["agy-cli"].as_array().unwrap().len(), 1);
    let out = command(home.path())
        .env("AGY_AUTO_APPROVE_LOG_DIR", &logs)
        .arg("stats")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Total | whitelist:allow | 3 | 3 | 3"));
}

#[test]
fn effective_config_reports_sources_and_independent_paths() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config/agy-auto-approve");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("config.toml"),
        "[approver]\nprovider='pi'\neffort='low'\n[hosts.pi.approver]\nmodel='example/model'\n",
    )
    .unwrap();
    let out = command(home.path())
        .args(["config", "--host", "pi", "--json"])
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("AGY_AUTO_APPROVE_EFFORT", "high")
        .env_remove("AGY_AUTO_APPROVE_LOG_DIR")
        .env_remove("AGY_APPROVER_SOCKET")
        .env_remove("AGY_APPROVER_STATE_DIR")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["reviewer"]["approver_sources"]["provider"], "[approver]");
    assert_eq!(
        v["reviewer"]["approver_sources"]["model"],
        "[hosts.pi.approver]"
    );
    assert_eq!(
        v["reviewer"]["approver_sources"]["effort"],
        "AGY_AUTO_APPROVE_EFFORT"
    );
    assert_eq!(
        v["state_dir"],
        home.path()
            .join("data/agy-auto-approve/hosts/pi/default")
            .to_str()
            .unwrap()
    );
    assert_eq!(
        v["socket"],
        home.path()
            .join("data/agy-auto-approve/runtime/approver-pi.sock")
            .to_str()
            .unwrap()
    );
}
