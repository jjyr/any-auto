use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

struct Sandbox {
    dir: tempfile::TempDir,
    socket: PathBuf,
}
impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("agy-rs-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("a.sock");
        Self { dir, socket }
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_any-auto"));
        c.args(["--agent", "agy-desktop"])
            .env("ANY_AUTO_PROVIDER", "agentapi")
            .env("HOME", self.dir.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("ANY_AUTO_MODEL")
            .env_remove("ANY_AUTO_PROMPT")
            .env("ANY_AUTO_SOCKET", self.dir.path().join("a.sock"))
            .env("ANY_AUTO_STATE_DIR", self.dir.path().join("state"))
            .env("ANY_AUTO_LOG_DIR", self.dir.path().join("logs"))
            .env("ANY_AUTO_SILENT", "1")
            .env("PATH", self.dir.path())
            .current_dir(self.dir.path());
        c
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        self.command().args(args).output().unwrap()
    }
    fn hook(&self, payload: &str) -> Value {
        let mut c = self
            .command()
            .arg("hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = c.wait_with_output().unwrap();
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn mock(&self, body: &str) {
        let p = self.dir.path().join("agentapi");
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = self.run(&["daemon", "stop"]);
    }
}
fn payload(command: &str) -> String {
    json!({"conversationId":"test", "toolCall":{"name":"run_command","args":{"CommandLine":command}}, "workspacePaths":[]}).to_string()
}

#[test]
fn agentapi_cli_fallback_preserves_agent_path_priority() {
    for agent_available in [false, true] {
        let s = Sandbox::new();
        let fallback = s.dir.path().join(".gemini/antigravity-cli/bin");
        fs::create_dir_all(&fallback).unwrap();
        s.mock(
            r#"
case "$1" in
  new-conversation) echo '{"conversationId":"fallback"}';;
  send-message) echo '{"outcome":"allow","rationale":"CLI fallback"}';;
esac
"#,
        );
        fs::rename(s.dir.path().join("agentapi"), fallback.join("agentapi")).unwrap();
        if agent_available {
            s.mock(
                r#"
case "$1" in
  new-conversation) echo '{"conversationId":"agent"}';;
  send-message) echo '{"outcome":"deny","rationale":"agent reviewer decision"}';;
esac
"#,
            );
        }
        let input = json!({"conversationId":"path-test", "toolCall":{
            "name":"run_command", "args":{"CommandLine":"git status", "Cwd":"/tmp/work space"}}});
        let output = s.hook(&input.to_string());
        assert_eq!(
            output["decision"],
            if agent_available { "deny" } else { "allow" }
        );
        assert!(
            output["reason"]
                .as_str()
                .unwrap()
                .contains(if agent_available {
                    "agent reviewer decision"
                } else {
                    "CLI fallback"
                })
        );
        let records: Value =
            serde_json::from_slice(&s.run(&["logs", "--no-group", "--json"]).stdout).unwrap();
        assert_eq!(records[0]["command"], "git status");
        assert_eq!(records[0]["cwd"], "/tmp/work space");
        assert_eq!(records[0]["stage"], "reviewer");
        let plain = s.run(&["logs"]);
        let plain = String::from_utf8(plain.stdout).unwrap();
        assert!(plain.contains("command: git status"));
        assert!(plain.contains("cwd: /tmp/work space"));
        let trace: Value = serde_json::from_slice(
            &s.run(&["logs", "show", records[0]["id"].as_str().unwrap()])
                .stdout,
        )
        .unwrap();
        let request = trace["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["event"] == "backend_request")
            .unwrap();
        assert_eq!(
            request["data"]["search_path"],
            json!([s.dir.path(), fallback])
        );
    }
}

#[test]
fn agentapi_failure_includes_stdout_and_stderr() {
    for stderr in ["", "transport failed"] {
        let s = Sandbox::new();
        s.mock(&format!(
            "echo '{{\"error\":\"ANTIGRAVITY_LS_ADDRESS is not set\"}}'\necho '{stderr}' >&2\nexit 1"
        ));
        let output = s.hook(&payload("git status"));
        assert_eq!(output["decision"], "deny");
        let reason = output["reason"].as_str().unwrap();
        assert!(reason.contains("ANTIGRAVITY_LS_ADDRESS is not set"));
        assert!(reason.contains(stderr));
        let records: Value =
            serde_json::from_slice(&s.run(&["logs", "--no-group", "--json"]).stdout).unwrap();
        assert_eq!(records[0]["stage"], "reviewer_error");
    }
}

#[test]
fn missing_agentapi_logs_search_context_and_infrastructure_failure() {
    let s = Sandbox::new();
    let output = s.hook(&payload("git status"));
    assert_eq!(output["decision"], "deny");
    assert!(
        output["reason"]
            .as_str()
            .unwrap()
            .contains("agentapi new-conversation")
    );
    let records: Value =
        serde_json::from_slice(&s.run(&["logs", "--no-group", "--json"]).stdout).unwrap();
    assert_eq!(records[0]["stage"], "reviewer_error");
    let trace: Value = serde_json::from_slice(
        &s.run(&["logs", "show", records[0]["id"].as_str().unwrap()])
            .stdout,
    )
    .unwrap();
    let events = trace["events"].as_array().unwrap();
    let error = events
        .iter()
        .find(|e| e["event"] == "backend_error")
        .unwrap();
    assert_eq!(error["data"]["operation"], "new-conversation");
    assert_eq!(error["data"]["stage"], "spawn");
    assert!(!events.iter().any(|e| e["event"] == "backend_response"));
}
#[test]
fn lifecycle_auto_spawn_fail_closed_and_breaker() {
    let s = Sandbox::new();
    assert!(!s.run(&["daemon", "status"]).status.success());
    let out = s.hook(&json!({"toolCall":{"name":"view_file","args":{}}}).to_string());
    assert_eq!(out["decision"], "allow");
    assert!(!s.socket.exists());
    assert_eq!(s.hook(&payload("echo $(rm -rf /)"))["decision"], "deny");
    assert!(!s.socket.exists());
    assert_eq!(s.hook("broken")["decision"], "ask");
    for _ in 0..3 {
        assert_eq!(s.hook(&payload("cargo test"))["decision"], "deny");
    }
    assert_eq!(s.hook(&payload("cargo test"))["decision"], "force_ask");
    let status = s.run(&["daemon", "status"]);
    assert!(status.status.success());
    let v: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(v["evaluations"], 3);
    let start: Value = serde_json::from_slice(&s.run(&["daemon", "start"]).stdout).unwrap();
    assert_eq!(start["pid"], v["pid"]);
    assert!(s.run(&["daemon", "stop"]).status.success());
    assert!(!s.socket.exists());
}
#[test]
fn session_reuse_and_cached_session_recovery() {
    let s = Sandbox::new();
    s.mock(r#"
case "$1" in
  new-conversation) echo new >> "$HOME/calls"; echo '{"conversationId":"fresh"}';;
  send-message) echo send >> "$HOME/calls"; if [ "$2" = expired ]; then exit 1; fi; printf '%s\n' '{"response":"```json\n{\"outcome\":\"allow\"}\n```"}';;
esac
"#);
    assert_eq!(s.hook(&payload("git status"))["decision"], "allow");
    assert!(s.run(&["daemon", "stop"]).status.success());
    let path = any_auto::sessions::directory(&s.dir.path().join("state/sidecar"), "test")
        .join("reviewer_session.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["conversationId"] = json!("expired");
    fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
    fs::write(s.dir.path().join("calls"), "").unwrap();
    for _ in 0..2 {
        let out = s.hook(&payload("git status && gh pr list"));
        assert_eq!(out["decision"], "allow", "{out}");
        assert_eq!(
            out["permissionOverrides"],
            json!(["command(git status)", "command(gh pr list)"])
        );
    }
    assert_eq!(
        fs::read_to_string(s.dir.path().join("calls")).unwrap(),
        "send\nnew\nsend\nsend\n"
    );
}
#[test]
fn idle_shutdown_and_socket_collision() {
    let s = Sandbox::new();
    fs::write(&s.socket, "do not remove").unwrap();
    assert!(
        !s.run(&["daemon", "run", "--idle-timeout", "1"])
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(&s.socket).unwrap(), "do not remove");
    fs::remove_file(&s.socket).unwrap();
    let mut child = s
        .command()
        .args(["daemon", "run", "--idle-timeout", "1"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..150 {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            assert!(!s.socket.exists());
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    panic!("idle daemon did not exit");
}
#[test]
fn registration_preserves_configuration_and_uses_absolute_binary() {
    let s = Sandbox::new();
    let config = s.dir.path().join(".gemini/config");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("hooks.json"), r#"{"other":{"enabled":true}}"#).unwrap();
    let out = s.run(&["install", "--agents", "agy-cli,agy-desktop"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&fs::read(config.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(v["other"]["enabled"], true);
    let command = v["any-auto"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains(env!("CARGO_BIN_EXE_any-auto")));
    assert!(!command.contains("python"));
    let post_command = v["any-auto"]["PostToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(post_command.contains(env!("CARGO_BIN_EXE_any-auto")));
    assert!(post_command.ends_with(" post-tool"));
    assert!(
        s.run(&["install", "--agents", "agy-cli,agy-desktop"])
            .status
            .success()
    );
    fs::write(config.join("hooks.json"), "invalid JSON").unwrap();
    assert!(!s.run(&["install", "--cli-only"]).status.success());
    assert_eq!(
        fs::read_to_string(config.join("hooks.json")).unwrap(),
        "invalid JSON"
    );
}

#[test]
fn concurrent_starts_and_status_during_review() {
    let s = Sandbox::new();
    s.mock(
        r#"
case "$1" in
  new-conversation) echo '{"conversationId":"shared"}';;
  send-message) /bin/sleep 1; echo '{"outcome":"allow"}';;
esac
"#,
    );
    let starters: Vec<_> = (0..5)
        .map(|_| {
            s.command()
                .args(["daemon", "start"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in starters {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut hook = s
        .command()
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hook.stdin
        .take()
        .unwrap()
        .write_all(payload("cargo test").as_bytes())
        .unwrap();
    let mut observed = false;
    for _ in 0..50 {
        let out = s.run(&["daemon", "status"]);
        assert!(out.status.success());
        let status: Value = serde_json::from_slice(&out.stdout).unwrap();
        if status["active_evaluations"] == 1 {
            observed = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(observed, "status must remain responsive during model call");
    let out = hook.wait_with_output().unwrap();
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["decision"], "allow");
}

#[test]
fn prompt_precedence_and_script_inspection() {
    for (workspace, environment, expected) in [
        (false, false, "global"),
        (true, false, "global"),
        (true, true, "environment"),
    ] {
        let s = Sandbox::new();
        let global = s.dir.path().join(".config/any-auto");
        fs::create_dir_all(&global).unwrap();
        fs::write(global.join("config.toml"), "prompt = 'global'").unwrap();
        if workspace {
            fs::create_dir_all(s.dir.path().join(".agents")).unwrap();
            fs::write(
                s.dir.path().join(".agents/any-auto-prompt.txt"),
                "workspace",
            )
            .unwrap();
        }
        s.mock(
            r#"
case "$1" in
  new-conversation) printf '%s' "$3" > "$HOME/prompt"; echo '{"conversationId":"fresh"}';;
  send-message) printf '%s' "$3" > "$HOME/action"; echo '{"outcome":"allow"}';;
esac
"#,
        );
        fs::write(s.dir.path().join("build.sh"), "echo inspected").unwrap();
        let mut cmd = s.command();
        cmd.env_remove("ANY_AUTO_PROMPT");
        if environment {
            cmd.env("ANY_AUTO_PROMPT", "environment");
        }
        let mut child = cmd
            .arg("hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut v: Value = serde_json::from_str(&payload("bash build.sh")).unwrap();
        v["workspacePaths"] = json!([s.dir.path()]);
        v["toolCall"]["args"]["Cwd"] = json!(s.dir.path());
        child
            .stdin
            .take()
            .unwrap()
            .write_all(v.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap()["decision"],
            "allow"
        );
        assert_eq!(
            fs::read_to_string(s.dir.path().join("prompt")).unwrap(),
            expected
        );
        let action: Value =
            serde_json::from_slice(&fs::read(s.dir.path().join("action")).unwrap()).unwrap();
        assert!(
            action["inspected_script"]
                .as_str()
                .unwrap()
                .contains("echo inspected")
        );
    }
}

#[test]
fn malformed_ipc_and_stale_socket_recovery() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::{UnixListener, UnixStream},
    };
    let s = Sandbox::new();
    drop(UnixListener::bind(&s.socket).unwrap());
    assert!(s.run(&["daemon", "start"]).status.success());
    let mut stream = UnixStream::connect(&s.socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(b"invalid JSON\n").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let result: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(result["assessment"]["outcome"], "deny");
    assert!(s.run(&["daemon", "status"]).status.success());
    assert_eq!(
        fs::metadata(&s.socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn non_string_agentapi_response_fails_closed() {
    let s = Sandbox::new();
    s.mock(
        r#"
case "$1" in
  new-conversation) echo '{"conversationId":"fresh"}';;
  send-message) echo '{"response":{"outcome":"allow"}}';;
esac
"#,
    );
    assert_eq!(s.hook(&payload("cargo test"))["decision"], "deny");
}

#[test]
fn logs_contains_full_review_trace_and_filters() {
    let s = Sandbox::new();
    s.mock(r#"
case "$1" in
  new-conversation) echo '{"conversationId":"history-session"}';;
  send-message) printf '%s\n' '{"response":"{\"outcome\":\"allow\",\"risk_level\":\"low\",\"user_authorization\":\"high\",\"rationale\":\"User requested a test run\"}"}';;
esac
"#);
    let input = payload("cargo test --locked");
    let output = s.hook(&input);
    assert_eq!(output["decision"], "allow");
    let readonly = json!({"conversationId":"readonly", "toolCall":{"name":"view_file","args":{"AbsolutePath":"/tmp/example"}}});
    s.hook(&readonly.to_string());
    assert!(s.run(&["daemon", "stop"]).status.success());
    let list = s.run(&[
        "logs",
        "--no-group",
        "--json",
        "--tool",
        "run_command",
        "--decision",
        "allow",
        "--conversation",
        "test",
    ]);
    assert!(list.status.success());
    let records: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 1);
    let id = records[0]["id"].as_str().unwrap();
    let detail = s.run(&["logs", "show", id]);
    assert!(detail.status.success());
    let trace: Value = serde_json::from_slice(&detail.stdout).unwrap();
    let events = trace["events"].as_array().unwrap();
    assert_eq!(events[0]["event"], "hook_input");
    assert_eq!(
        events[0]["data"]["input"],
        serde_json::from_str::<Value>(&input).unwrap()
    );
    assert_eq!(events.last().unwrap()["data"]["output"], output);
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "backend_request" && e["data"]["args"][0] == "new-conversation")
    );
    assert!(events.iter().any(|e| {
        e["event"] == "backend_request"
            && e["data"]["args"][2]
                .as_str()
                .unwrap_or("")
                .contains("cargo test --locked")
    }));
    assert!(events.iter().any(|e| {
        e["event"] == "backend_response"
            && e["data"]["stdout"]
                .as_str()
                .unwrap_or("")
                .contains("User requested a test run")
    }));
    assert!(events.iter().any(|e| e["event"] == "assessment"
        && e["data"]["assessment"]["risk_level"] == "low"
        && e["data"]["assessment"]["user_authorization"] == "high"));
    let latest: Value = serde_json::from_slice(
        &s.run(&["logs", "--no-group", "--json", "--limit", "1"])
            .stdout,
    )
    .unwrap();
    assert_eq!(latest[0]["tool"], "view_file");
    assert_eq!(latest[0]["stage"], "whitelist");
    assert!(!s.run(&["logs", "show", "missing"]).status.success());
    assert!(!s.run(&["logs", "--limit", "0"]).status.success());
    assert!(!s.socket.exists(), "logs must not spawn a daemon");
    let path =
        any_auto::audit::daily_path(&s.dir.path().join("logs"), chrono::Utc::now().date_naive());
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn logs_records_failures_and_concurrent_hooks() {
    let s = Sandbox::new();
    assert_eq!(
        serde_json::from_slice::<Value>(&s.run(&["logs", "--no-group", "--json"]).stdout).unwrap(),
        json!([])
    );
    let input = json!({"toolCall":{"name":"view_file","args":{}}}).to_string();
    let mut children = Vec::new();
    for _ in 0..12 {
        let mut child = s
            .command()
            .arg("hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        children.push(child);
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    s.hook("bad JSON");
    assert_eq!(s.hook(&payload("cargo test"))["decision"], "deny");
    let records: Value =
        serde_json::from_slice(&s.run(&["logs", "--no-group", "--json"]).stdout).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 14);
    let id = records[0]["id"].as_str().unwrap();
    let trace: Value = serde_json::from_slice(&s.run(&["logs", "show", id]).stdout).unwrap();
    assert!(
        trace["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "backend_error")
    );
    assert_eq!(records[1]["stage"], "invalid_input");
    let mut ids = std::collections::HashSet::new();
    for record in records.as_array().unwrap() {
        assert!(ids.insert(record["id"].as_str().unwrap()));
    }
}

#[test]
fn installer_handles_prebuilt_binary_and_paths_with_spaces() {
    let s = Sandbox::new();
    let install_dir = s.dir.path().join("bin with 'quote");
    fs::create_dir_all(&install_dir).unwrap();
    let binary = install_dir.join("any-auto");
    fs::copy(env!("CARGO_BIN_EXE_any-auto"), &binary).unwrap();
    let install = |flags: &[&str]| {
        Command::new(&binary)
            .arg("install")
            .args(flags)
            .env("HOME", s.dir.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_RUNTIME_DIR")
            .output()
            .unwrap()
    };
    let out = install(&["--cli-only"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hooks: Value =
        serde_json::from_slice(&fs::read(s.dir.path().join(".gemini/config/hooks.json")).unwrap())
            .unwrap();
    let hook_command = hooks["any-auto"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let mut child = Command::new("/bin/sh")
        .args(["-c", hook_command])
        .env("HOME", s.dir.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env("ANY_AUTO_LOG_DIR", s.dir.path().join("logs"))
        .env("ANY_AUTO_SILENT", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"toolCall\":{\"name\":\"view_file\",\"args\":{}}}")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["decision"],
        "allow"
    );
    assert!(!install(&["--cli-only", "--desktop-only"]).status.success());
}

struct LogFollower {
    child: std::process::Child,
    records: std::sync::mpsc::Receiver<Value>,
}
impl LogFollower {
    fn new(s: &Sandbox, args: &[&str]) -> Self {
        use std::io::BufRead;
        let mut child = s
            .command()
            .arg("logs")
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, records) = std::sync::mpsc::channel();
        thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let Ok(record) = serde_json::from_str(&line) else {
                    break;
                };
                if tx.send(record).is_err() {
                    break;
                }
            }
        });
        Self { child, records }
    }
    fn next(&self) -> Value {
        self.records
            .recv_timeout(Duration::from_secs(3))
            .expect("Expected live log record")
    }
}
impl Drop for LogFollower {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
#[test]
fn logs_follow_snapshot_filters_partial_lines_and_rotation() {
    let s = Sandbox::new();
    let readonly = json!({"toolCall":{"name":"view_file","args":{}}}).to_string();
    s.hook(&readonly);
    s.hook(&readonly);
    let latest: Value = serde_json::from_slice(
        &s.run(&["logs", "--no-group", "--json", "--limit", "1"])
            .stdout,
    )
    .unwrap();
    let follower = LogFollower::new(&s, &["-f", "--json", "--limit", "1", "--tool", "view_file"]);
    assert_eq!(follower.next()["id"], latest[0]["id"]);
    s.hook("invalid input");
    s.hook(&readonly);
    let next = follower.next();
    assert_eq!(next["tool"], "view_file");
    assert_ne!(next["id"], latest[0]["id"]);
    let log =
        any_auto::audit::daily_path(&s.dir.path().join("logs"), chrono::Utc::now().date_naive());
    let event = json!({"schema_version":3, "agent":"agy-desktop", "event":"hook_result", "id":"partial", "data":{"tool":"view_file", "output":{"decision":"allow"}}}).to_string();
    let split = event.len() / 2;
    let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
    file.write_all(&event.as_bytes()[..split]).unwrap();
    assert!(
        follower
            .records
            .recv_timeout(Duration::from_millis(400))
            .is_err()
    );
    writeln!(file, "{}", &event[split..]).unwrap();
    assert_eq!(follower.next()["id"], "partial");
    fs::rename(&log, log.with_extension("old")).unwrap();
    fs::write(&log, format!("{}\n", event.replace("partial", "rotated"))).unwrap();
    assert_eq!(follower.next()["id"], "rotated");
    fs::write(&log, format!("{}\n", event.replace("partial", "short"))).unwrap();
    assert_eq!(follower.next()["id"], "short");
    assert!(!s.socket.exists());
}
#[test]
fn logs_follow_waits_for_creation_and_exits_on_ctrl_c() {
    let s = Sandbox::new();
    let mut follower = LogFollower::new(&s, &["--follow", "--json"]);
    assert!(
        follower
            .records
            .recv_timeout(Duration::from_millis(300))
            .is_err()
    );
    s.hook("invalid input");
    assert_eq!(follower.next()["decision"], "ask");
    assert!(
        Command::new("/bin/kill")
            .args(["-INT", &follower.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    for _ in 0..100 {
        if let Some(status) = follower.child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("logs --follow did not stop on Ctrl-C");
}

#[test]
fn concurrent_reviews_share_session_across_daemon_restart() {
    let s = Sandbox::new();
    s.mock(
        r#"
case "$1" in
  new-conversation) echo new >> "$HOME/calls"; echo '{"conversationId":"persistent"}';;
  send-message) echo send >> "$HOME/calls"; echo '{"outcome":"allow"}';;
esac
"#,
    );
    let mut children = Vec::new();
    for _ in 0..10 {
        let mut v: Value = serde_json::from_str(&payload("cargo test")).unwrap();
        v["conversationId"] = "test".into();
        let mut child = s
            .command()
            .arg("hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(v.to_string().as_bytes())
            .unwrap();
        children.push(child);
    }
    for child in children {
        let out = child.wait_with_output().unwrap();
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["decision"], "allow", "{result}");
    }
    assert!(s.run(&["daemon", "stop"]).status.success());
    assert_eq!(s.hook(&payload("git status"))["decision"], "allow");
    let calls = fs::read_to_string(s.dir.path().join("calls")).unwrap();
    assert_eq!(calls.lines().filter(|line| *line == "new").count(), 1);
    assert_eq!(calls.lines().filter(|line| *line == "send").count(), 11);
}

#[test]
fn registration_modes_preserve_existing_permissions() {
    for mode in ["--cli-only", "--desktop-only"] {
        let s = Sandbox::new();
        let cli = s.dir.path().join(".gemini/antigravity-cli");
        fs::create_dir_all(&cli).unwrap();
        let settings = json!({"permissions":{"allow":["file(/custom)","command(cargo)"],"deny":["command(secret)"]},"custom":true});
        fs::write(cli.join("settings.json"), settings.to_string()).unwrap();
        let out = s.run(&["install", mode]);
        assert!(out.status.success());
        let actual: Value =
            serde_json::from_slice(&fs::read(cli.join("settings.json")).unwrap()).unwrap();
        if mode == "--desktop-only" {
            assert_eq!(actual, settings);
            assert!(!s.dir.path().join(".gemini/config/hooks.json").exists());
            let manifest: Value = serde_json::from_slice(
                &fs::read(
                    s.dir
                        .path()
                        .join(".gemini/config/sidecars/any-auto/approver/sidecar.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(manifest["args"], json!(["daemon", "start"]));
        } else {
            assert!(
                actual["permissions"]["allow"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("file(/custom)"))
            );
            assert_eq!(
                actual["permissions"]["deny"],
                settings["permissions"]["deny"]
            );
            assert_eq!(actual["custom"], true);
            assert!(!s.dir.path().join(".gemini/config/config.json").exists());
        }
    }
}

#[test]
fn global_config_edit_and_precedence() {
    let s = Sandbox::new();
    let out = s.run(&["config", "--json"]);
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["reviewer"]["model"].is_null());
    assert!(!s.dir.path().join(".config/any-auto").exists());
    let editor = s.dir.path().join("editor with spaces");
    fs::write(&editor, "#!/bin/sh\n[ \"$1\" = --wait ] || exit 5\nprintf 'model = \"pro\"\nprompt = \"custom prompt\"\n' > \"$2\"\n").unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();
    let out = s
        .command()
        .args(["config", "--edit"])
        .env("VISUAL", format!("'{}' --wait", editor.display()))
        .env("EDITOR", "/bin/false")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = s
        .command()
        .args(["config", "--json"])
        .env("ANY_AUTO_MODEL", "flash")
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["reviewer"]["model"], "flash");
    assert_eq!(value["reviewer"]["model_source"], "ANY_AUTO_MODEL");
    assert_eq!(value["reviewer"]["prompt"], "custom prompt");
    assert!(!s.run(&["config", "--local"]).status.success());
    assert!(!s.run(&["config", "--edit", "--json"]).status.success());
    assert!(
        !s.command()
            .args(["config", "--edit"])
            .env("VISUAL", "/bin/false")
            .output()
            .unwrap()
            .status
            .success()
    );
    fs::write(
        s.dir.path().join(".config/any-auto/config.toml"),
        "model = 'unsupported'",
    )
    .unwrap();
    assert!(!s.run(&["config"]).status.success());
    assert!(!s.socket.exists());
}

#[test]
fn reset_clears_session_and_applies_model_and_prompt() {
    let s = Sandbox::new();
    s.mock(
        r#"
case "$1" in
  new-conversation) printf '%s\n' "$@" >> "$HOME/created"; echo '{"conversationId":"reviewer"}';;
  send-message) echo '{"outcome":"allow"}';;
esac
"#,
    );
    // Restart also starts a stopped daemon and removes a persisted session.
    fs::create_dir_all(any_auto::sessions::directory(
        &s.dir.path().join("state/sidecar"),
        "test",
    ))
    .unwrap();
    fs::write(
        any_auto::sessions::directory(&s.dir.path().join("state/sidecar"), "test")
            .join("reviewer_session.json"),
        r#"{"conversationId":"old"}"#,
    )
    .unwrap();
    assert!(s.run(&["daemon", "reset"]).status.success());
    assert!(
        !any_auto::sessions::directory(&s.dir.path().join("state/sidecar"), "test")
            .join("reviewer_session.json")
            .exists()
    );
    assert_eq!(s.hook(&payload("cargo test"))["decision"], "allow");
    let before = fs::read_to_string(s.dir.path().join("created")).unwrap();
    assert!(!before.contains("--model="));
    fs::create_dir_all(s.dir.path().join(".config/any-auto")).unwrap();
    fs::write(
        s.dir.path().join(".config/any-auto/config.toml"),
        "model = 'pro'\nprompt = 'new prompt'\n",
    )
    .unwrap();
    assert_eq!(s.hook(&payload("cargo test"))["decision"], "allow");
    let changed = fs::read_to_string(s.dir.path().join("created")).unwrap();
    assert_eq!(
        &changed[before.len()..],
        "new-conversation\n--title=Guardian Approver Session\n--model=pro\nnew prompt\n"
    );
    assert!(s.run(&["daemon", "stop"]).status.success());
    assert_eq!(s.hook(&payload("cargo test"))["decision"], "allow");
    assert_eq!(
        fs::read_to_string(s.dir.path().join("created")).unwrap(),
        changed
    );
    assert!(s.run(&["daemon", "reset"]).status.success());
    assert_eq!(s.hook(&payload("cargo test"))["decision"], "allow");
    let after = fs::read_to_string(s.dir.path().join("created")).unwrap();
    assert_eq!(
        &after[changed.len()..],
        "new-conversation\n--title=Guardian Approver Session\n--model=pro\nnew prompt\n"
    );
    fs::write(
        s.dir.path().join(".config/any-auto/config.toml"),
        "model = 'bad'\n",
    )
    .unwrap();
    assert!(s.run(&["daemon", "restart"]).status.success());
    assert_eq!(s.hook(&payload("cargo test"))["decision"], "deny");
    assert_eq!(
        fs::read_to_string(s.dir.path().join("created")).unwrap(),
        after
    );
}

#[test]
fn old_global_text_files_are_ignored_when_reading_and_creating_config() {
    let s = Sandbox::new();
    let global = s.dir.path().join(".config/any-auto");
    fs::create_dir_all(&global).unwrap();
    fs::write(global.join("any-auto-prompt.txt"), "legacy prompt").unwrap();
    fs::write(global.join("any-auto-model.txt"), "legacy model").unwrap();
    let out = s.run(&["config", "--json"]);
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["reviewer"]["model"].is_null());
    assert_eq!(
        value["reviewer"]["prompt"],
        include_str!("../src/prompt.txt")
    );
    assert_eq!(value["reviewer"]["prompt_source"], "default");
    let out = s
        .command()
        .args(["config", "--edit"])
        .env("VISUAL", "/usr/bin/true")
        .output()
        .unwrap();
    assert!(out.status.success());
    let file: toml::Value =
        toml::from_str(&fs::read_to_string(global.join("config.toml")).unwrap()).unwrap();
    assert!(file.get("model").is_none());
    assert!(file.get("prompt").is_none());
}
