use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};

struct Agent {
    dir: tempfile::TempDir,
}
impl Agent {
    fn new() -> Self {
        let agent = Self {
            dir: tempfile::tempdir_in("/tmp").unwrap(),
        };
        let config_dir = agent.dir.path().join(".config/any-auto");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.toml"),
            "[agents.agy-desktop.approver]\nprovider = \"agentapi\"\n",
        )
        .unwrap();
        agent.mock("agentapi", r#"
case "$1" in
 new-conversation) echo '{"conversationId":"sidecar-session"}';;
 send-message) [ "$2" = sidecar-session ] || exit 2; echo '{"response":"{\"outcome\":\"allow\",\"rationale\":\"sidecar\"}"}';;
 *) exit 3;;
esac
"#);
        agent.mock("agy", r#"
[ "$ANY_AUTO_REVIEWER" = 1 ] || exit 4
[ -z "$ANTIGRAVITY_LS_ADDRESS" ] || exit 5
case "$PWD" in */state/cli/sessions/*/workspace) ;; *) exit 6;; esac
[ "$1" = -p ] || exit 7
printf '%s\n' "$@" >> "$HOME/agy-args"
resume=no
while [ "$#" -gt 0 ]; do
 if [ "$1" = --conversation ]; then shift; [ "$1" = cli-session ] || exit 8; resume=yes; fi
 shift
done
if [ "$resume" = no ]; then
 echo new >> "$HOME/agy-calls"
 echo '{"status":"SUCCESS","conversation_id":"cli-session","response":"READY"}'
else
 echo send >> "$HOME/agy-calls"
 # Even read-only tools must not recurse into the reviewer or be auto-allowed.
 printf '%s' '{"toolCall":{"name":"view_file","args":{}}}' | "$APPROVER_TEST_BIN" hook > "$HOME/nested-result"
 echo '{"status":"SUCCESS","conversation_id":"cli-session","response":"{\"outcome\":\"allow\",\"rationale\":\"cli\"}"}'
fi
"#);
        agent
    }
    fn mock(&self, name: &str, script: &str) {
        let path = self.dir.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_any-auto"));
        cmd.env("HOME", self.dir.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_RUNTIME_DIR")
            .env("PATH", self.dir.path())
            .env("ANY_AUTO_SOCKET", self.dir.path().join("a.sock"))
            .env("ANY_AUTO_STATE_DIR", self.dir.path().join("state"))
            .env("ANY_AUTO_LOG_DIR", self.dir.path().join("logs"))
            .env("ANY_AUTO_SILENT", "1")
            .env("APPROVER_TEST_BIN", env!("CARGO_BIN_EXE_any-auto"))
            .env_remove("ANY_AUTO_REVIEWER")
            .env_remove("ANY_AUTO_MODEL")
            .env_remove("ANY_AUTO_CLI_MODEL")
            .env_remove("ANY_AUTO_PROMPT")
            .env_remove("ANTIGRAVITY_LS_ADDRESS");
        cmd
    }
    fn run(&self, args: &[&str]) -> Value {
        let out = self.command().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn hook(&self, mode: Option<&str>, agent_env: bool) -> Value {
        let mut cmd = self.command();
        cmd.arg("hook");
        if let Some(mode) = mode {
            cmd.args([
                "--agent",
                match mode {
                    "cli" => "agy-cli",
                    "sidecar" => "agy-desktop",
                    other => other,
                },
            ]);
        }
        if agent_env {
            cmd.env("ANTIGRAVITY_LS_ADDRESS", "localhost:1234");
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(json!({"conversationId":"same-user-conversation", "toolCall":{"name":"run_command","args":{"CommandLine":"git log -n 5 --oneline"}}}).to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
impl Drop for Agent {
    fn drop(&mut self) {
        for mode in ["cli", "sidecar"] {
            let _ = self
                .command()
                .args([
                    "daemon",
                    "stop",
                    "--agent",
                    match mode {
                        "cli" => "agy-cli",
                        "sidecar" => "agy-desktop",
                        other => other,
                    },
                ])
                .output();
        }
    }
}

#[test]
fn shared_daemon_routes_agents_and_resets_independently() {
    let h = Agent::new();
    fs::create_dir_all(h.dir.path().join("state")).unwrap();
    fs::write(
        h.dir.path().join("state/reviewer_session.json"),
        r#"{"conversationId":"legacy"}"#,
    )
    .unwrap();
    assert!(
        h.hook(None, false)["reason"]
            .as_str()
            .unwrap()
            .contains("cli")
    );
    assert!(
        h.hook(None, true)["reason"]
            .as_str()
            .unwrap()
            .contains("sidecar")
    );
    let cli = h.run(&["daemon", "status"]);
    let sidecar = h.run(&["daemon", "status", "--agent", "agy-desktop"]);
    assert_eq!(cli["instances"].as_array().unwrap().len(), 2);
    assert_eq!(sidecar["instances"][0]["agent"], "agy-desktop");
    assert_eq!(cli["pid"], sidecar["pid"]);
    assert_eq!(cli["socket"], sidecar["socket"]);
    assert_eq!(h.run(&["daemon", "start"])["pid"], cli["pid"]);
    assert_eq!(h.hook(Some("cli"), true)["decision"], "allow");
    assert_eq!(h.hook(Some("sidecar"), false)["decision"], "allow");
    assert_eq!(
        fs::read_to_string(h.dir.path().join("agy-calls")).unwrap(),
        "new\nsend\nsend\n"
    );
    let nested: Value =
        serde_json::from_slice(&fs::read(h.dir.path().join("nested-result")).unwrap()).unwrap();
    assert_eq!(nested["decision"], "deny");
    for mode in ["cli", "sidecar"] {
        assert!(h.dir.path().join(format!("state/{mode}/sessions")).exists());
    }
    h.run(&["daemon", "reset", "--agent", "agy-cli"]);
    assert_eq!(
        h.run(&["daemon", "status", "--agent", "agy-desktop"])["pid"],
        sidecar["pid"]
    );
    assert!(
        !any_auto::sessions::directory(&h.dir.path().join("state/cli"), "same-user-conversation")
            .join("reviewer_session.json")
            .exists()
    );
    assert!(
        any_auto::sessions::directory(
            &h.dir.path().join("state/sidecar"),
            "same-user-conversation"
        )
        .join("reviewer_session.json")
        .exists()
    );
    assert_eq!(h.hook(None, false)["decision"], "allow");
    let records = h.run(&["logs", "--no-group", "--json"]);
    assert!(
        records
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["mode"] == "cli")
    );
    assert!(
        records
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["mode"] == "sidecar")
    );
}

#[test]
fn cli_errors_fail_closed_without_switching_backend_and_breakers_are_separate() {
    for body in [
        "echo '{\"error\":\"agy unavailable\"}'; exit 1",
        "echo '{\"status\":\"ERROR\",\"conversation_id\":\"bad\",\"response\":\"allow\"}'",
        "echo '{\"status\":\"SUCCESS\",\"response\":\"missing session\"}'",
        "echo '{\"status\":\"SUCCESS\",\"conversation_id\":\"bad\",\"response\":{\"outcome\":\"allow\"}}'",
    ] {
        let h = Agent::new();
        h.mock("agy", body);
        for _ in 0..3 {
            assert_eq!(h.hook(None, false)["decision"], "deny");
        }
        assert_eq!(h.hook(None, false)["decision"], "force_ask");
        assert_eq!(h.hook(None, true)["decision"], "allow");
    }
}

#[test]
fn cli_model_and_prompt_are_passed_as_single_arguments() {
    let h = Agent::new();
    let config = h.dir.path().join(".config/any-auto");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("config.toml"), "model = 'pro'\ncli_model = 'gemini-3.8-flash-high'\nprompt = 'custom $(do-not-execute) prompt'\n").unwrap();
    assert_eq!(h.hook(None, false)["decision"], "allow");
    let args = fs::read_to_string(h.dir.path().join("agy-args")).unwrap();
    assert!(args.contains("custom $(do-not-execute) prompt"));
    assert!(args.contains("--model\ngemini-3.8-flash-high\n"));
    assert!(!args.contains("--model\npro\n"));
    assert!(args.contains("--disable-slash-commands\n--output-format\njson"));
}

#[test]
fn cli_cached_session_failure_recreates_only_cli_session() {
    let h = Agent::new();
    assert_eq!(h.hook(None, false)["decision"], "allow");
    h.run(&["daemon", "stop", "--agent", "agy-cli"]);
    let path =
        any_auto::sessions::directory(&h.dir.path().join("state/cli"), "same-user-conversation")
            .join("reviewer_session.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["conversationId"] = json!("expired");
    fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(h.hook(None, true)["decision"], "allow");
    assert_eq!(h.hook(None, false)["decision"], "allow");
    let records = h.run(&["logs", "--no-group", "--json"]);
    let trace = h.run(&["logs", "show", records[0]["id"].as_str().unwrap()]);
    assert!(
        trace["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "reviewer_retry")
    );
    assert_eq!(
        fs::read_to_string(h.dir.path().join("agy-calls")).unwrap(),
        "new\nsend\nnew\nsend\n"
    );
    assert!(
        any_auto::sessions::directory(
            &h.dir.path().join("state/sidecar"),
            "same-user-conversation"
        )
        .join("reviewer_session.json")
        .exists()
    );
}

#[test]
fn ipc_rejects_mode_mismatch() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
    };
    let h = Agent::new();
    h.run(&["daemon", "start"]);
    let mut stream = UnixStream::connect(h.dir.path().join("a.sock")).unwrap();
    stream
        .write_all(b"{\"action\":\"evaluate\",\"mode\":\"sidecar\"}\n")
        .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["assessment"]["outcome"], "deny");
    assert!(!h.dir.path().join("agy-calls").exists());
}

#[test]
fn stats_tracks_initialization_resume_and_persisted_baseline_after_restart() {
    let h = Agent::new();
    h.mock("agy", r#"
resume=no
while [ "$#" -gt 0 ]; do
 if [ "$1" = --conversation ]; then resume=yes; fi
 shift
done
if [ "$resume" = no ]; then turn=1; else read -r turn < "$HOME/turn"; turn=$((turn + 1)); fi
printf '%s\n' "$turn" > "$HOME/turn"
printf '{"status":"SUCCESS","conversation_id":"usage-session","num_turns":%s,"usage":{"input_tokens":%s,"output_tokens":%s},"response":"%s"}\n' "$turn" "$((turn * 100))" "$((turn * 10))" '{\"outcome\":\"allow\"}'
"#);
    assert_eq!(h.hook(Some("cli"), false)["decision"], "allow");
    assert_eq!(h.hook(Some("cli"), false)["decision"], "allow");
    h.run(&["daemon", "stop", "--agent", "agy-cli"]);
    assert_eq!(h.hook(Some("cli"), false)["decision"], "allow");
    let out = h
        .command()
        .args(["stats", "--agent", "agy-cli", "--no-group"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let table = String::from_utf8(out.stdout).unwrap();
    assert!(!table.contains("Outcomes"));
    for line in table.lines().filter(|line| line.contains("Last ")) {
        let cells: Vec<_> = line
            .split('│')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(&cells[1..4], &["3", "400", "40"]);
        assert_eq!(&cells[5..7], &["133", "13"]);
    }
    let path =
        any_auto::audit::daily_path(&h.dir.path().join("logs"), chrono::Utc::now().date_naive());
    assert!(!h.dir.path().join("logs/approvals.jsonl").exists());
    let events: Vec<Value> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let total_ms: u64 = events
        .iter()
        .filter(|event| event["event"] == "hook_result" && event["data"]["stage"] == "reviewer")
        .map(|event| event["data"]["duration_ms"].as_u64().unwrap())
        .sum();
    let expected_average = format!("{:.1}s", total_ms as f64 / 3.0 / 1000.0);
    for line in table.lines().filter(|line| line.contains("Last ")) {
        let cells: Vec<_> = line
            .split('│')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(cells[7], expected_average);
    }
    let responses: Vec<_> = events
        .iter()
        .filter(|event| event["event"] == "backend_response")
        .collect();
    assert_eq!(responses.len(), 4);
    for response in responses {
        assert_eq!(
            response["data"]["usage_delta"],
            json!({"input_tokens":100,"output_tokens":10})
        );
    }
    assert_eq!(h.hook(Some("sidecar"), false)["decision"], "allow");
    let all = h.command().args(["stats", "--no-group"]).output().unwrap();
    let all = String::from_utf8(all.stdout).unwrap();
    assert!(all.contains("N/A"));
    for line in all.lines().filter(|line| line.contains("Last ")) {
        let cells: Vec<_> = line
            .split('│')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(cells[1], "4");
    }
    let cli = h
        .command()
        .args(["stats", "--agent", "agy-cli", "--no-group"])
        .output()
        .unwrap();
    assert!(!String::from_utf8(cli.stdout).unwrap().contains("N/A"));
}

#[test]
fn shared_daemon_keeps_desktop_connection_environments_private() {
    let h = Agent::new();
    h.mock(
        "agentapi",
        r#"
case "$ANTIGRAVITY_LS_ADDRESS:$ANTIGRAVITY_CSRF_TOKEN" in
 connection-one:secret-one) cid=one;;
 connection-two:secret-two) cid=two;;
 *) exit 8;;
esac
case "$1" in
 new-conversation) printf '{"conversationId":"%s"}\n' "$cid";;
 send-message) [ "$2" = "$cid" ] || exit 9; printf '{"outcome":"allow","rationale":"%s"}\n' "$cid";;
esac
"#,
    );
    let pid = h.run(&["daemon", "start"])["pid"].clone();
    let mut children = Vec::new();
    for (instance, address, token) in [
        ("one", "connection-one", "secret-one"),
        ("two", "connection-two", "secret-two"),
    ] {
        let mut child = h
            .command()
            .args(["hook", "--agent", "agy-desktop", "--instance", instance])
            .env("ANTIGRAVITY_LS_ADDRESS", address)
            .env("ANTIGRAVITY_CSRF_TOKEN", token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(br#"{"conversationId":"same","toolCall":{"name":"run_command","args":{"CommandLine":"cargo test"}},"workspacePaths":[]}"#).unwrap();
        children.push(child);
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["decision"], "allow", "{value}");
    }
    let status = h.run(&["daemon", "status"]);
    assert_eq!(status["pid"], pid);
    assert_eq!(status["instances"].as_array().unwrap().len(), 2);
    let logs = h.run(&["logs", "--no-group", "--json"]);
    assert!(!logs.to_string().contains("secret-one"));
    assert!(!logs.to_string().contains("secret-two"));
    assert_eq!(logs.as_array().unwrap().len(), 2);
}
