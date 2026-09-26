mod support;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};

struct Agent {
    root: tempfile::TempDir,
}
impl Agent {
    fn new() -> Self {
        let agent = Self {
            root: tempfile::tempdir_in("/tmp").unwrap(),
        };
        let script = r#"#!/bin/sh
printf '%s\n' "$$" >> "$HOME/pi-pids"
printf '%s\n' "$@" >> "$HOME/pi-args"
[ "$ANY_AUTO_REVIEWER" = 1 ] || exit 3
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
  printf '%s\n' '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"{\"risk\":\"low\",\"authorization\":\"high\",\"policy\":\"permitted\",\"rationale\":\"RPC approved\"}"}],"stopReason":"stop","usage":{"input":10,"output":2,"cacheRead":5,"cacheWrite":3}}}'
  printf '%s\n' '{"type":"agent_end","messages":[{"usage":{"input":999}}]}' '{"type":"agent_settled"}'
 ;;
 esac
done
"#;
        fs::write(agent.root.path().join("pi"), script).unwrap();
        fs::set_permissions(
            agent.root.path().join("pi"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        agent
    }
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_any-auto"));
        c.env("HOME", self.root.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_RUNTIME_DIR")
            .env("PATH", self.root.path())
            .env("ANY_AUTO_SOCKET", self.root.path().join("a.sock"))
            .env("ANY_AUTO_LOG_DIR", self.root.path().join("logs"))
            .env("ANY_AUTO_STATE_DIR", self.root.path().join("state"))
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("ANY_AUTO_REVIEWER")
            .env_remove("ANY_AUTO_PROVIDER")
            .env_remove("ANY_AUTO_EFFORT")
            .env_remove("ANY_AUTO_APPROVER_MODEL")
            .env_remove("ANY_AUTO_MODEL")
            .env_remove("ANY_AUTO_CLI_MODEL")
            .env_remove("ANY_AUTO_PROMPT");
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
        self.hook_instance("default", session, name, builtin)
    }
    fn hook_instance(&self, instance: &str, session: &str, name: &str, builtin: bool) -> Value {
        let mut child = self
            .cmd()
            .args(["hook", "--agent", "pi", "--instance", instance])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(json!({"conversationId":session,"builtin_tool":builtin,"authorization":support::authorization(),"toolCall":{"name":name,"args":{"command":"echo hello","path":"a"}},"workspacePaths":["/tmp"]}).to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn config(&self, text: &str) {
        let p = self.root.path().join(".config/any-auto");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.toml"), text).unwrap();
    }
}
impl Drop for Agent {
    fn drop(&mut self) {
        for mode in ["pi", "cli"] {
            let _ = self
                .cmd()
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
fn rpc_reuses_process_isolates_sessions_and_counts_usage_once() {
    let h = Agent::new();
    h.config("[agents.pi.approver]\neffort = 'low'\n");
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
    let stats = h.run(&["stats", "--agent", "pi", "--no-group"]);
    assert!(stats.contains("54"), "{stats}"); // (10 + 5 + 3) * 3
    assert!(stats.contains("6"), "{stats}");
    let logs: Value =
        serde_json::from_str(&h.run(&["logs", "--group-by", "agent", "--json"])).unwrap();
    assert_eq!(logs["pi"].as_array().unwrap().len(), 3);
    let args = fs::read_to_string(h.root.path().join("pi-args")).unwrap();
    for arg in ["rpc", "--no-tools", "--no-extensions", "--no-context-files"] {
        assert!(args.lines().any(|v| v == arg));
    }
}
#[test]
fn pi_builtin_reads_are_fast_but_custom_names_are_reviewed() {
    let h = Agent::new();
    assert_eq!(h.hook("read", "read", true)["decision"], "allow");
    assert!(!h.root.path().join("pi-pids").exists());
    assert_eq!(h.hook("read", "view_file", false)["decision"], "allow");
    assert!(h.root.path().join("pi-pids").exists());
}
#[test]
fn unsupported_effort_fails_before_prompt_and_provider_is_independent_of_agent() {
    let h = Agent::new();
    h.config("[approver]\nprovider='pi'\neffort='high'\n");
    assert_eq!(h.hook("one", "write", true)["decision"], "deny");
    assert!(!h.root.path().join("pi-prompts").exists());
    let c: Value =
        serde_json::from_str(&h.run(&["config", "--agent", "agy-cli", "--json"])).unwrap();
    assert_eq!(c["reviewer"]["approver"]["provider"], "pi");
}
#[test]
fn cancelled_hook_discards_busy_rpc_and_next_review_uses_new_child() {
    let h = Agent::new();
    let path = h.root.path().join("pi");
    let script = fs::read_to_string(&path).unwrap().replace("printf '%s\\n' \"$line\" >> \"$HOME/pi-prompts\"", "printf '%s\\n' \"$line\" >> \"$HOME/pi-prompts\"\n  case \"$line\" in *HANG*) while IFS= read -r ignored; do :; done; exit;; esac");
    fs::write(path, script).unwrap();
    let mut hook = h
        .cmd()
        .args(["hook", "--agent", "pi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    hook.stdin.take().unwrap().write_all(br#"{"authorization":{"availability":"available","latest_user_message":{"id":"fixture-user","role":"user","source":"test","text":"Run the requested development task"},"relevant_prior_messages":[]},"conversationId":"one","toolCall":{"name":"write","args":{"path":"HANG"}},"workspacePaths":[]}"#).unwrap();
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
    check_openai_responses(true);
}

#[test]
fn openai_responses_omits_unconfigured_generation_parameters() {
    check_openai_responses(false);
}

fn check_openai_responses(generation_settings: bool) {
    use std::{io::Read, net::TcpListener};
    let h = Agent::new();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let generation = if generation_settings {
        "temperature=0.6\ntop_p=0.95\nmax_output_tokens=2048\n[agents.pi.approver.openai.llama_cpp]\ntop_k=20\nmin_p=0.0\npresence_penalty=0.0\nrepeat_penalty=1.0\nreasoning_budget_tokens=128\n"
    } else {
        ""
    };
    h.config(&format!("[agents.pi.approver]\nprovider='openai'\n[agents.pi.approver.openai]\nmodel='fixture-model'\nbase_url='http://{}/v1'\napi_key='fixture-secret'\n[agents.pi.approver.openai.common]\neffort='low'\n{generation}", server.local_addr().unwrap()));
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
            let response = json!({"id":format!("resp_{i}"),"model":"fixture-model","status":"completed","reasoning":{"effort":"low"},"usage":{"input_tokens":20,"output_tokens":3,"input_tokens_details":{"cached_tokens":10}},"output":[{"type":"message","content":[{"type":"output_text","text":"{\"risk\":\"low\",\"authorization\":\"high\",\"policy\":\"permitted\",\"rationale\":\"Fixture assessment\"}"}]}]}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
        }
        requests
    });
    // The fixture key is explicitly supplied, never an inherited live credential.
    for _ in 0..2 {
        let mut hook = h
            .cmd()
            .args(["hook", "--agent", "pi"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        hook.stdin.take().unwrap().write_all(br#"{"conversationId":"api","toolCall":{"name":"write","args":{}},"workspacePaths":[],"authorization":{"availability":"available","latest_user_message":{"id":"u1","role":"user","text":"Create the file","source":"current_branch"},"relevant_prior_messages":[]}}"#).unwrap();
        let out = hook.wait_with_output().unwrap();
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["decision"], "allow", "{result}");
    }
    let requests = worker.join().unwrap();
    assert!(requests[0]["previous_response_id"].is_null());
    assert_eq!(requests[1]["previous_response_id"], "resp_1");
    assert_eq!(requests[0]["reasoning"]["effort"], "low");
    assert_eq!(requests[0]["tools"], json!([]));
    for request in &requests {
        assert!(request.get("openai").is_none());
        assert!(request.get("common").is_none());
        assert!(request.get("llama_cpp").is_none());
    }
    for request in &requests {
        for (key, expected) in [
            ("temperature", json!(0.6)),
            ("top_p", json!(0.95)),
            ("top_k", json!(20)),
            ("min_p", json!(0.0)),
            ("presence_penalty", json!(0.0)),
            ("repeat_penalty", json!(1.0)),
            ("max_output_tokens", json!(2048)),
            ("reasoning_budget_tokens", json!(128)),
        ] {
            if generation_settings {
                assert_eq!(request[key], expected, "{key}");
            } else {
                assert!(request.get(key).is_none(), "unexpected {key}");
            }
        }
    }

    let input: Value = serde_json::from_str(requests[0]["input"].as_str().unwrap()).unwrap();
    assert_eq!(
        input["authorization"]["latest_user_message"]["text"],
        "Create the file"
    );
    assert_eq!(input["completeness"]["authorization"], "available");
    let stats = h.run(&["stats", "--no-group"]);
    assert!(stats.contains("40"), "{stats}");
    assert!(
        !h.run(&["logs", "--no-group", "--json"])
            .contains("fixture-secret")
    );
}

#[test]
fn daemon_start_is_shared_across_agents_and_instances() {
    let h = Agent::new();
    let first: Value = serde_json::from_str(&h.run(&["daemon", "start", "--agent", "pi"])).unwrap();
    for args in [
        vec!["daemon", "start", "--agent", "agy-cli"],
        vec!["daemon", "start", "--agent", "pi", "--instance", "second"],
    ] {
        let status: Value = serde_json::from_str(&h.run(&args)).unwrap();
        assert_eq!(status["pid"], first["pid"]);
        assert_eq!(status["socket"], first["socket"]);
    }
    let status: Value = serde_json::from_str(&h.run(&["daemon", "status", "--all"])).unwrap();
    assert_eq!(status["instances"], json!([]));
}

#[test]
fn changed_config_starts_new_rpc_generation() {
    let h = Agent::new();
    h.config("[agents.pi.approver]\nmodel='mock/first'\n");
    assert_eq!(h.hook("one", "write", true)["decision"], "allow");
    h.config("[agents.pi.approver]\nmodel='mock/second'\n");
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
fn logs_use_request_provider_instead_of_daemon_startup_environment() {
    let h = Agent::new();
    h.run(&["daemon", "start", "--agent", "pi"]);
    fs::write(h.root.path().join("agy"), "#!/bin/sh\necho '{\"status\":\"SUCCESS\",\"conversation_id\":\"cli-session\",\"response\":\"{\\\"risk\\\":\\\"low\\\",\\\"authorization\\\":\\\"high\\\",\\\"policy\\\":\\\"permitted\\\",\\\"rationale\\\":\\\"Fixture assessment\\\"}\"}'\n").unwrap();
    fs::set_permissions(h.root.path().join("agy"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut hook = h
        .cmd()
        .env("ANY_AUTO_PROVIDER", "cli")
        .args(["hook", "--agent", "pi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    hook.stdin.take().unwrap().write_all(br#"{"authorization":{"availability":"available","latest_user_message":{"id":"fixture-user","role":"user","source":"test","text":"Run the requested development task"},"relevant_prior_messages":[]},"conversationId":"one","toolCall":{"name":"write","args":{}},"workspacePaths":[]}"#).unwrap();
    let result: Value = serde_json::from_slice(&hook.wait_with_output().unwrap().stdout).unwrap();
    assert_eq!(result["decision"], "allow");
    let records: Value =
        serde_json::from_str(&h.run(&["logs", "--no-group", "--provider", "cli", "--json"]))
            .unwrap();
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["provider"], "cli");
}

#[test]
fn pi_error_message_cannot_allow_even_if_text_contains_allow() {
    let h = Agent::new();
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
fn pi_sessions_are_private_and_idle_children_are_reaped_then_restored() {
    use std::time::{Duration, Instant};
    let h = Agent::new();
    let mut daemon = h
        .cmd()
        .args([
            "daemon",
            "run",
            "--session-idle-timeout",
            "1",
            "--idle-timeout",
            "0",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !h.root.path().join("a.sock").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    for instance in ["default", "work"] {
        assert_eq!(
            h.hook_instance(instance, "same-conversation", "write", false)["decision"],
            "allow"
        );
    }
    let pids: Vec<u32> = fs::read_to_string(h.root.path().join("pi-pids"))
        .unwrap()
        .lines()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    assert_ne!(pids[0], pids[1]);
    let state = any_auto::sessions::directory(&h.root.path().join("state/pi"), "same-conversation")
        .join("reviewer_session.json");
    let saved = fs::read(&state).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status: Value = serde_json::from_str(&h.run(&["daemon", "status"])).unwrap();
        if status["cached_sessions"] == 0 {
            assert_eq!(status["instances"], json!([]));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Idle sessions were not evicted: {status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    for pid in &pids {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(Instant::now() < deadline, "Pi child {pid} was not reaped");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert_eq!(fs::read(&state).unwrap(), saved);
    assert_eq!(
        h.hook("same-conversation", "write", false)["decision"],
        "allow"
    );
    assert_eq!(fs::read(&state).unwrap(), saved);
    assert_eq!(
        fs::read_to_string(h.root.path().join("pi-pids"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    h.run(&["daemon", "stop"]);
    assert!(daemon.wait().unwrap().success());
}

#[test]
fn evaluator_records_backend_prompts_without_production_logs() {
    for provider in ["pi", "cli"] {
        let h = Agent::new();
        h.config(&format!(
            "prompt='Custom classification prompt'\n[approver]\nprovider='{provider}'\n"
        ));
        if provider == "cli" {
            let path = h.root.path().join("agy");
            let response = json!({"status":"SUCCESS","conversation_id":"eval",
                "response":json!({"risk":"low","authorization":"high","policy":"permitted","rationale":"Fixture"}).to_string()});
            fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{response}'\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let suite = h.root.path().join("suite.jsonl");
        let report_path = h.root.path().join("report.json");
        let cases: Vec<_> = (0..2)
            .map(|i| {
                json!({"id":format!("case-{i}"),"reason":"Fixture",
            "expected":{"decision":"allow"},"input":{"authorization":support::authorization(),
                "toolCall":{"name":"write","args":{"path":"fixture.txt"}}}})
                .to_string()
            })
            .collect();
        fs::write(&suite, cases.join("\n")).unwrap();
        let output = h
            .cmd()
            .args(["reviewer-eval", "--agent", "pi", "--repeat", "1", "--suite"])
            .arg(&suite)
            .arg("--output")
            .arg(&report_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
        assert_eq!(report["provider"], provider);
        let expected_prompt = if provider == "cli" {
            any_auto::prompts::agy_session_prompt("Custom classification prompt")
        } else {
            "Custom classification prompt".to_owned()
        };
        assert_eq!(report["prompt"], expected_prompt);
        assert!(report.get("questions").is_none());
        assert!(report.get("questions_hash").is_none());
        if provider == "pi" {
            let args = fs::read_to_string(h.root.path().join("pi-args")).unwrap();
            assert!(args.lines().any(|line| line == expected_prompt));
        }

        assert_eq!(report["completed"], true);
        assert!(
            report["trials"]
                .as_array()
                .unwrap()
                .iter()
                .all(|trial| trial["matched"] == true)
        );
        assert_eq!(
            report["summary"]["backend_calls"],
            if provider == "cli" { 4 } else { 2 }
        );
        assert!(!h.root.path().join("logs").exists());
        assert!(!h.root.path().join("state").exists());
        if provider == "pi" {
            assert_eq!(
                fs::read_to_string(h.root.path().join("pi-pids"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
        }
    }
}

#[test]
fn evaluator_limits_concurrent_trials_and_isolates_sessions() {
    let h = Agent::new();
    let path = h.root.path().join("pi");
    let script = fs::read_to_string(&path).unwrap().replace(
        "count=$((count + 1))",
        "count=$((count + 1))\n  : > \"$HOME/started-$$\"\n  while [ ! -f \"$HOME/release\" ]; do /bin/sleep 0.02; done",
    );
    fs::write(path, script).unwrap();
    let suite = h.root.path().join("suite.jsonl");
    let output = h.root.path().join("report.json");
    let cases: Vec<_> = (0..3)
        .map(|i| {
            json!({"id":format!("case-{i}"),"reason":"Fixture",
        "expected":{"decision":"allow"},"input":{"authorization":support::authorization(),
            "toolCall":{"name":"write","args":{}}}})
            .to_string()
        })
        .collect();
    fs::write(&suite, cases.join("\n")).unwrap();
    let mut child = h
        .cmd()
        .args([
            "reviewer-eval",
            "--agent",
            "pi",
            "--repeat",
            "1",
            "--concurrency",
            "2",
            "--suite",
        ])
        .arg(suite)
        .arg("--output")
        .arg(&output)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let count = || {
        fs::read_dir(h.root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("started-"))
            .count()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while count() < 2 {
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("Two evaluations did not start concurrently");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(count(), 2, "The third trial must remain queued");
    fs::write(h.root.path().join("release"), "").unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(count(), 3);
    let report: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(report["concurrency"], 2);
    assert_eq!(report["summary"]["matched"], 3);
    assert_eq!(
        report["trials"]
            .as_array()
            .unwrap()
            .iter()
            .map(|trial| trial["case_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["case-0", "case-1", "case-2"]
    );
}
