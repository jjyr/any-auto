use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};

struct Host {
    root: tempfile::TempDir,
}
impl Host {
    fn new() -> Self {
        let host = Self {
            root: tempfile::tempdir_in("/tmp").unwrap(),
        };
        let script = r#"#!/bin/sh
printf '%s\n' "$$" >> "$HOME/pi-pids"
printf '%s\n' "$@" >> "$HOME/pi-args"
[ "$AGY_AUTO_APPROVE_REVIEWER" = 1 ] || exit 3
[ "$PI_CODING_AGENT_DIR" = "$HOME/.pi/agent" ] || exit 4
count=0
while IFS= read -r line; do
 id=$(printf '%s' "$line" | /usr/bin/sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
 case "$line" in
 *'"type":"get_available_thinking_levels"'*) printf '{"type":"response","id":"%s","command":"get_available_thinking_levels","success":true,"data":{"levels":["off","low"]}}\n' "$id";;
 *'"type":"set_thinking_level"'*) exit 99; printf '{"type":"response","id":"%s","command":"set_thinking_level","success":true}\n' "$id";;
 *'"type":"get_state"'*) printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"model":{"id":"mock-model","provider":"mock"},"thinkingLevel":"low"}}\n' "$id";;
 *'"type":"get_session_stats"'*) printf '{"type":"response","id":"%s","command":"get_session_stats","success":true,"data":{"tokens":{"input":%s,"output":%s,"cacheRead":%s,"cacheWrite":%s}}}\n' "$id" "$((count * 10))" "$((count * 2))" "$((count * 5))" "$((count * 3))";;
 *'"type":"prompt"'*)
  count=$((count + 1))
  printf '%s\n' "$line" >> "$HOME/pi-prompts"
  printf '{"type":"response","id":"unrelated","command":"prompt","success":false}\n'
  printf '{"type":"response","id":"%s","command":"prompt","success":true}\n' "$id"
  printf '%s\n' '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"{\"outcome\":\"allow\",\"rationale\":\"RPC approved\"}"}],"stopReason":"stop","usage":{"input":10,"output":2,"cacheRead":5,"cacheWrite":3}}}'
  printf '%s\n' '{"type":"agent_end","messages":[{"usage":{"input":999}}]}' '{"type":"agent_settled"}'
 ;;
 esac
done
"#;
        fs::write(host.root.path().join("pi"), script).unwrap();
        fs::set_permissions(
            host.root.path().join("pi"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        host
    }
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_agy-auto-approve"));
        c.env("HOME", self.root.path())
            .env("PATH", self.root.path())
            .env("AGY_APPROVER_SOCKET", self.root.path().join("a.sock"))
            .env("AGY_AUTO_APPROVE_LOG_DIR", self.root.path().join("logs"))
            .env("AGY_APPROVER_STATE_DIR", self.root.path().join("state"))
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("AGY_AUTO_APPROVE_REVIEWER")
            .env_remove("AGY_AUTO_APPROVE_PROVIDER")
            .env_remove("AGY_AUTO_APPROVE_EFFORT")
            .env_remove("AGY_AUTO_APPROVE_APPROVER_MODEL")
            .env_remove("AGY_AUTO_APPROVE_MODEL")
            .env_remove("AGY_AUTO_APPROVE_CLI_MODEL")
            .env_remove("AGY_AUTO_APPROVE_PROMPT");
        c
    }
    fn run(&self, args: &[&str]) -> String {
        let out = self.cmd().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn hook(&self, session: &str, name: &str, builtin: bool) -> Value {
        let mut child = self
            .cmd()
            .args(["hook", "--host", "pi"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(json!({"conversationId":session,"builtin_tool":builtin,"toolCall":{"name":name,"args":{"command":"echo hello","path":"a"}},"workspacePaths":["/tmp"]}).to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn config(&self, text: &str) {
        let p = self.root.path().join(".gemini/config");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("agy-auto-approve.toml"), text).unwrap();
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        for mode in ["pi", "cli"] {
            let _ = self.cmd().args(["daemon", "stop", "--mode", mode]).output();
        }
    }
}

#[test]
fn rpc_reuses_process_isolates_sessions_and_counts_usage_once() {
    let h = Host::new();
    h.config("[hosts.pi.approver]\neffort = 'low'\n");
    assert_eq!(h.hook("one", "bash", true)["decision"], "allow");
    assert_eq!(h.hook("one", "write", true)["decision"], "allow");
    assert_eq!(
        fs::read_to_string(h.root.path().join("pi-pids"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(h.hook("two", "write", true)["decision"], "allow");
    assert_eq!(
        fs::read_to_string(h.root.path().join("pi-pids"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let stats = h.run(&["stats", "--host", "pi", "--no-group"]);
    assert!(stats.contains("54"), "{stats}"); // (10 + 5 + 3) * 3
    assert!(stats.contains("6"), "{stats}");
    let logs: Value =
        serde_json::from_str(&h.run(&["logs", "--group-by", "host", "--json"])).unwrap();
    assert_eq!(logs["pi"].as_array().unwrap().len(), 3);
    let args = fs::read_to_string(h.root.path().join("pi-args")).unwrap();
    for arg in ["rpc", "--no-tools", "--no-extensions", "--no-context-files"] {
        assert!(args.lines().any(|v| v == arg));
    }
}
#[test]
fn pi_builtin_reads_are_fast_but_custom_names_are_reviewed() {
    let h = Host::new();
    assert_eq!(h.hook("read", "read", true)["decision"], "allow");
    assert!(!h.root.path().join("pi-pids").exists());
    assert_eq!(h.hook("read", "view_file", false)["decision"], "allow");
    assert!(h.root.path().join("pi-pids").exists());
}
#[test]
fn unsupported_effort_fails_before_prompt_and_provider_is_independent_of_host() {
    let h = Host::new();
    h.config("[approver]\nprovider='pi'\neffort='high'\n");
    assert_eq!(h.hook("one", "write", true)["decision"], "deny");
    assert!(!h.root.path().join("pi-prompts").exists());
    let c: Value =
        serde_json::from_str(&h.run(&["config", "--host", "agy-cli", "--json"])).unwrap();
    assert_eq!(c["reviewer"]["approver"]["provider"], "pi");
}
#[test]
fn host_override_resets_foreign_model_and_install_only_touches_pi() {
    let h = Host::new();
    h.config("[approver]\nprovider='openai'\nmodel='foreign'\neffort='high'\n[hosts.pi.approver]\nprovider='pi'\n");
    let c: Value = serde_json::from_str(&h.run(&["config", "--host", "pi", "--json"])).unwrap();
    assert!(c["reviewer"]["approver"]["model"].is_null());
    assert!(c["reviewer"]["approver"]["effort"].is_null());
    h.run(&["install", "--pi"]);
    let ext = fs::read_to_string(
        h.root
            .path()
            .join(".pi/agent/extensions/agy-auto-approve.ts"),
    )
    .unwrap();
    assert!(ext.contains(env!("CARGO_BIN_EXE_agy-auto-approve")));
    assert!(!h.root.path().join(".gemini/config/hooks.json").exists());
}

#[test]
fn cancelled_hook_discards_busy_rpc_and_next_review_uses_new_child() {
    let h = Host::new();
    let path = h.root.path().join("pi");
    let script = fs::read_to_string(&path).unwrap().replace("printf '%s\\n' \"$line\" >> \"$HOME/pi-prompts\"", "printf '%s\\n' \"$line\" >> \"$HOME/pi-prompts\"\n  case \"$line\" in *HANG*) while IFS= read -r ignored; do :; done; exit;; esac");
    fs::write(path, script).unwrap();
    let mut hook = h
        .cmd()
        .args(["hook", "--host", "pi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    hook.stdin.take().unwrap().write_all(br#"{"conversationId":"one","toolCall":{"name":"write","args":{"path":"HANG"}},"workspacePaths":[]}"#).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !h.root.path().join("pi-prompts").exists() {
        assert!(std::time::Instant::now() < deadline, "RPC never started");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    hook.kill().unwrap();
    hook.wait().unwrap();
    assert_eq!(h.hook("one", "write", true)["decision"], "allow");
    assert_eq!(
        fs::read_to_string(h.root.path().join("pi-pids"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[test]
fn openai_responses_reuses_response_id_and_normalizes_usage() {
    use std::{io::Read, net::TcpListener};
    let h = Host::new();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    h.config(&format!("[hosts.pi.approver]\nprovider='openai'\nmodel='fixture-model'\neffort='low'\nbase_url='http://{}/v1'\napi_key_env='APPROVER_FIXTURE_KEY'\n", server.local_addr().unwrap()));
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for i in 1..=2 {
            let (mut stream, _) = server.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            let headers = String::from_utf8(bytes).unwrap();
            assert!(headers.starts_with("POST /v1/responses "));
            assert!(
                headers
                    .to_lowercase()
                    .contains("authorization: bearer fixture-secret")
            );
            let n: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; n];
            stream.read_exact(&mut body).unwrap();
            requests.push(serde_json::from_slice::<Value>(&body).unwrap());
            let response = json!({"id":format!("resp_{i}"),"model":"fixture-model","status":"completed","reasoning":{"effort":"low"},"usage":{"input_tokens":20,"output_tokens":3,"input_tokens_details":{"cached_tokens":10}},"output":[{"type":"message","content":[{"type":"output_text","text":"{\"outcome\":\"allow\"}"}]}]}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
        }
        requests
    });
    // The fixture key is explicitly supplied, never an inherited live credential.
    for _ in 0..2 {
        let mut hook = h
            .cmd()
            .env("APPROVER_FIXTURE_KEY", "fixture-secret")
            .args(["hook", "--host", "pi"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        hook.stdin.take().unwrap().write_all(br#"{"conversationId":"api","toolCall":{"name":"write","args":{}},"workspacePaths":[]}"#).unwrap();
        let out = hook.wait_with_output().unwrap();
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["decision"], "allow", "{result}");
    }
    let requests = worker.join().unwrap();
    assert!(requests[0]["previous_response_id"].is_null());
    assert_eq!(requests[1]["previous_response_id"], "resp_1");
    assert_eq!(requests[0]["reasoning"]["effort"], "low");
    assert_eq!(requests[0]["tools"], json!([]));
    let stats = h.run(&["stats", "--no-group"]);
    assert!(stats.contains("40"), "{stats}");
    assert!(
        !h.run(&["logs", "--no-group", "--json"])
            .contains("fixture-secret")
    );
}

#[test]
fn daemon_instances_coexist_and_status_all_finds_them() {
    let h = Host::new();
    h.run(&["daemon", "start", "--host", "pi"]);
    h.run(&["daemon", "start", "--host", "agy-cli"]);
    h.run(&["daemon", "start", "--host", "pi", "--instance", "second"]);
    let statuses: Value = serde_json::from_str(&h.run(&["daemon", "status", "--all"])).unwrap();
    assert_eq!(statuses.as_array().unwrap().len(), 3);
    assert!(
        statuses
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["instance"] == "second")
    );
    h.run(&["daemon", "stop", "--host", "pi", "--instance", "second"]);
    assert_eq!(h.hook("one", "write", true)["decision"], "allow");
}

#[test]
fn changed_config_starts_new_rpc_generation() {
    let h = Host::new();
    h.config("[hosts.pi.approver]\nmodel='mock/first'\n");
    assert_eq!(h.hook("one", "write", true)["decision"], "allow");
    h.config("[hosts.pi.approver]\nmodel='mock/second'\n");
    assert_eq!(h.hook("one", "write", true)["decision"], "allow");
    assert_eq!(
        fs::read_to_string(h.root.path().join("pi-pids"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let args = fs::read_to_string(h.root.path().join("pi-args")).unwrap();
    assert!(args.contains("mock/first"));
    assert!(args.contains("mock/second"));
}

#[test]
fn logs_use_running_daemon_provider_not_new_hook_environment() {
    let h = Host::new();
    h.run(&["daemon", "start", "--host", "pi"]);
    let mut hook = h
        .cmd()
        .env("AGY_AUTO_APPROVE_PROVIDER", "cli")
        .args(["hook", "--host", "pi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    hook.stdin.take().unwrap().write_all(br#"{"conversationId":"one","toolCall":{"name":"write","args":{}},"workspacePaths":[]}"#).unwrap();
    let result: Value = serde_json::from_slice(&hook.wait_with_output().unwrap().stdout).unwrap();
    assert_eq!(result["decision"], "allow");
    let records: Value =
        serde_json::from_str(&h.run(&["logs", "--no-group", "--provider", "pi", "--json"]))
            .unwrap();
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["provider"], "pi");
}

#[test]
fn pi_error_message_cannot_allow_even_if_text_contains_allow() {
    let h = Host::new();
    let path = h.root.path().join("pi");
    let script = fs::read_to_string(&path)
        .unwrap()
        .replace("\"stopReason\":\"stop\"", "\"stopReason\":\"error\"");
    fs::write(path, script).unwrap();
    assert_eq!(h.hook("one", "write", true)["decision"], "deny");
    let records: Value = serde_json::from_str(&h.run(&["logs", "--no-group", "--json"])).unwrap();
    assert_eq!(records[0]["stage"], "reviewer_error");
}

#[test]
fn logs_and_stats_default_to_host_groups() {
    let h = Host::new();
    h.hook("one", "write", true);
    let grouped: Value = serde_json::from_str(&h.run(&["logs", "--json"])).unwrap();
    assert_eq!(grouped["pi"].as_array().unwrap().len(), 1);
    let plain = h.run(&["logs"]);
    assert!(plain.starts_with("host: pi\n"), "{plain}");
    let stats = h.run(&["stats"]);
    assert!(stats.starts_with("host: pi\n"), "{stats}");
    assert!(stats.contains("\nTotal\n"), "{stats}");
    let merged: Value = serde_json::from_str(&h.run(&["logs", "--no-group", "--json"])).unwrap();
    assert_eq!(merged.as_array().unwrap().len(), 1);
}
