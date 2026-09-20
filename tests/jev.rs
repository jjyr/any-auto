//! End-to-end Jev reviews through hooks, the shared daemon, and audit/statistics.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

struct Harness {
    root: tempfile::TempDir,
}
impl Harness {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir_in("/tmp").unwrap(),
        }
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_any-auto"));
        c.env_clear()
            .env("HOME", self.root.path())
            .env("PATH", "/usr/bin:/bin")
            .env("ANY_AUTO_SOCKET", self.root.path().join("a.sock"))
            .env("ANY_AUTO_LOG_DIR", self.root.path().join("logs"))
            .env("ANY_AUTO_STATE_DIR", self.root.path().join("state"))
            .env("NO_PROXY", "*");
        c
    }
    fn configure(&self, url: &str, extra: &str) {
        let path = self.root.path().join(".config/any-auto");
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("config.toml"),
            format!("[approver]\nprovider='jev'\nbase_url='{url}'\napi_key='fixture-key'\n{extra}"),
        )
        .unwrap();
    }
    fn hook(&self, req: &Value, key: &str) -> Value {
        let mut child = self
            .command()
            .env("ANY_AUTO_API_KEY", key)
            .args(["hook", "--agent", "pi"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(req.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn submit(&self, args: &[&str], payload: &Value) -> Value {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if out.stdout.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&out.stdout).unwrap()
        }
    }
    fn output(&self, args: &[&str]) -> String {
        let out = self.command().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn logs(&self) -> String {
        fs::read_dir(self.root.path().join("logs"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|s| s == "jsonl"))
            .map(|e| fs::read_to_string(e.path()).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self
            .command()
            .args(["daemon", "stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
fn request() -> Value {
    json!({"conversationId":"jev-session","toolCall":{"name":"write","args":{"path":"test.txt","content":"hello"}},"workspacePaths":["/tmp"],"authorization":{
        "availability":"available","latest_user_message":{"id":"user-1","role":"user","text":"private-user-context: create test.txt","source":"current_branch"},
        "relevant_prior_messages":[{"id":"user-0","role":"user","text":"private-prior-context: keep changes local","source":"current_branch"}]}})
}
fn response() -> Value {
    json!({"model":"jev-fixture","answers":{
        "risk":{"type":"choice","choice":"low","probabilities":{"low":0.9,"medium":0.1,"high":0.0,"critical":0.0,"unknown":0.0},"confidence":0.5},
        "authorization":{"type":"choice","choice":"high","probabilities":{"high":0.9,"medium":0.1,"low":0.0,"unknown":0.0},"confidence":0.5},
        "policy":{"type":"choice","choice":"permitted","probabilities":{"permitted":0.9,"prohibited":0.0,"needs_confirmation":0.1,"unknown":0.0},"confidence":0.5}
    },"usage":{"input_tokens":20,"output_tokens":3}})
}
struct Reply {
    status: u16,
    headers: String,
    body: String,
}
fn ok() -> Reply {
    Reply {
        status: 200,
        headers: String::new(),
        body: response().to_string(),
    }
}
fn server(replies: Vec<Reply>) -> (String, std::thread::JoinHandle<Vec<(String, Value)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for reply in replies {
            let deadline = Instant::now() + Duration::from_secs(12);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "Timed out waiting for Jev request"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            let headers = String::from_utf8(bytes).unwrap();
            assert!(headers.starts_with("POST /v1/systemone "));
            let length: usize = headers
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            requests.push((headers, serde_json::from_slice(&body).unwrap()));
            let _ = write!(
                stream,
                "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                reply.status,
                reply.body.len(),
                reply.headers,
                reply.body
            );
        }
        requests
    });
    (url, worker)
}

#[test]
fn jev_stateless_configuration_context_credentials_and_statistics() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok(), ok(), ok()]);
    h.configure(&url,"[approver.instructions]\nrisk='Custom risk instruction: assess actual operational effects.'\n");
    assert_eq!(h.hook(&request(), "first-key")["decision"], "allow");
    let mut missing = request();
    missing.as_object_mut().unwrap().remove("authorization");
    assert_eq!(h.hook(&missing, "second-key")["decision"], "deny");
    h.configure(&url, "probability_threshold=0.95\n");
    assert_eq!(h.hook(&request(), "second-key")["decision"], "deny");
    let requests = worker.join().unwrap();
    assert!(
        requests[0]
            .0
            .to_lowercase()
            .contains("authorization: bearer first-key")
    );
    assert!(
        requests[1]
            .0
            .to_lowercase()
            .contains("authorization: bearer second-key")
    );
    assert_eq!(
        requests[0].1["state"]["authorization"],
        request()["authorization"]
    );
    assert!(
        requests[0].1["questions"]["risk"]["instructions"]
            .as_str()
            .unwrap()
            .starts_with("Custom risk instruction")
    );
    assert!(
        !requests[2].1["questions"]["risk"]["instructions"]
            .as_str()
            .unwrap()
            .starts_with("Custom risk instruction")
    );
    for (_, body) in &requests {
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("probability_threshold").is_none());
        assert!(body.get("messages").is_none());
    }
    let logs = h.logs();
    for secret in [
        "first-key",
        "second-key",
        "private-user-context",
        "private-prior-context",
    ] {
        assert!(!logs.contains(secret), "{secret} leaked into audit");
    }
    assert!(logs.contains("rubric_hash"));
    assert!(logs.contains("jev-fixture"));
    assert!(!logs.contains("reviewer_session"));
    assert!(!logs.contains("reviewer_retry"));
    let stats = h.output(&["stats", "--provider", "jev", "--no-group"]);
    assert!(stats.contains("60"), "{stats}");
    fn no_sessions(path: &std::path::Path) {
        if !path.exists() {
            return;
        }
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            assert_ne!(entry.file_name(), "reviewer_session.json");
            if entry.path().is_dir() {
                no_sessions(&entry.path());
            }
        }
    }
    no_sessions(&h.root.path().join("state"));
}

#[test]
fn jev_retries_overload_but_not_auth_or_schema_errors() {
    let h = Harness::new();
    let (url, worker) = server(vec![
        Reply {
            status: 429,
            headers: "Retry-After: 0\r\n".into(),
            body: "private-server-error".into(),
        },
        Reply {
            status: 529,
            headers: "Retry-After: 0\r\n".into(),
            body: String::new(),
        },
        ok(),
        Reply {
            status: 401,
            headers: String::new(),
            body: "private-server-error".into(),
        },
        Reply {
            status: 200,
            headers: String::new(),
            body: "{\"answers\":{}}".into(),
        },
    ]);
    h.configure(&url, "");
    assert_eq!(h.hook(&request(), "fixture-key")["decision"], "allow");
    assert_eq!(h.hook(&request(), "fixture-key")["decision"], "deny");
    assert_eq!(h.hook(&request(), "fixture-key")["decision"], "deny");
    assert_eq!(worker.join().unwrap().len(), 5);
    assert!(!h.logs().contains("private-server-error"));
}

#[test]
fn jev_retry_after_budget_and_redirects_fail_closed_without_following() {
    for reply in [
        Reply {
            status: 429,
            headers: "Retry-After: 60\r\n".into(),
            body: String::new(),
        },
        Reply {
            status: 302,
            headers: "Location: http://127.0.0.1:1/leak\r\n".into(),
            body: String::new(),
        },
        Reply {
            status: 422,
            headers: String::new(),
            body: String::new(),
        },
    ] {
        let h = Harness::new();
        let (url, worker) = server(vec![reply]);
        h.configure(&url, "");
        let started = Instant::now();
        assert_eq!(h.hook(&request(), "fixture-key")["decision"], "deny");
        assert!(started.elapsed() < Duration::from_secs(8));
        assert_eq!(worker.join().unwrap().len(), 1);
    }
}

#[test]
fn environment_only_configuration_reaches_the_daemon() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok()]);
    let mut child = h
        .command()
        .env("ANY_AUTO_PROVIDER", "jev")
        .env("ANY_AUTO_BASE_URL", url)
        .env("ANY_AUTO_API_KEY", "fixture-key")
        .args(["hook", "--agent", "pi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request().to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["decision"], "allow", "{v}");
    assert_eq!(worker.join().unwrap().len(), 1);
}

fn stalled_server() -> (
    String,
    std::sync::mpsc::Receiver<()>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (send, ready) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut stream = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("{e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(24)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut byte = [0];
        while !bytes.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
        }
        let headers = String::from_utf8(bytes).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|l| {
                l.to_lowercase()
                    .strip_prefix("content-length:")
                    .map(|s| s.trim().parse().unwrap())
            })
            .unwrap();
        stream.read_exact(&mut vec![0; length]).unwrap();
        let _ = send.send(());
        // No response: cancellation or the total deadline must close this socket.
        match stream.read(&mut byte) {
            Ok(0) => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            other => panic!("Request was not cancelled: {other:?}"),
        }
    });
    (url, ready, worker)
}

#[test]
fn cancelling_hook_cancels_jev_http_and_next_review_can_complete() {
    let h = Harness::new();
    let (url, ready, worker) = stalled_server();
    h.configure(&url, "");
    let mut child = h
        .command()
        .args(["hook", "--agent", "pi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request().to_string().as_bytes())
        .unwrap();
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = Instant::now();
    child.kill().unwrap();
    child.wait().unwrap();
    worker.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let (url, worker) = server(vec![ok()]);
    h.configure(&url, "");
    assert_eq!(h.hook(&request(), "fixture-key")["decision"], "allow");
    worker.join().unwrap();
}

#[test]
fn stalled_response_is_bounded_by_total_review_deadline() {
    let h = Harness::new();
    let (url, _ready, worker) = stalled_server();
    h.configure(&url, "");
    let started = Instant::now();
    let result = h.hook(&request(), "fixture-key");
    assert_eq!(result["decision"], "deny");
    assert!(
        result["reason"]
            .as_str()
            .unwrap()
            .contains("Jev review deadline exceeded"),
        "{result}"
    );
    assert!(started.elapsed() < Duration::from_secs(26));
    worker.join().unwrap();
}

#[test]
fn oversized_response_and_missing_usage_are_handled_without_fabrication() {
    let h = Harness::new();
    let mut no_usage = response();
    no_usage.as_object_mut().unwrap().remove("usage");
    let (url, worker) = server(vec![
        Reply {
            status: 200,
            headers: String::new(),
            body: " ".repeat(4 * 1024 * 1024 + 1),
        },
        Reply {
            status: 200,
            headers: String::new(),
            body: no_usage.to_string(),
        },
    ]);
    h.configure(&url, "");
    assert_eq!(h.hook(&request(), "fixture-key")["decision"], "deny");
    assert_eq!(h.hook(&request(), "fixture-key")["decision"], "allow");
    worker.join().unwrap();
    assert!(
        h.output(&["stats", "--provider", "jev", "--no-group"])
            .contains("N/A")
    );
}

#[test]
fn oversized_action_and_missing_credentials_fail_before_network() {
    let h = Harness::new();
    h.configure("http://127.0.0.1:1/v1", "");
    let mut req = request();
    req["toolCall"]["args"]["content"] = json!("x".repeat(25 * 1024));
    let result = h.hook(&req, "fixture-key");
    assert_eq!(result["decision"], "deny");
    assert!(result["reason"].as_str().unwrap().contains("24 KiB"));
    let result = h.hook(&request(), "");
    assert_eq!(result["decision"], "deny");
    assert!(
        result["reason"]
            .as_str()
            .unwrap()
            .contains("API key is empty")
    );
    assert!(!h.logs().contains("backend_request"));
}

#[test]
fn jev_command_surface_supports_configuration_diagnostics_lifecycle_and_queries() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok()]);
    h.configure(&url, "probability_threshold=0.85\n[approver.instructions]\nrisk='Assess actual operational risk. Use unknown for missing evidence.'\n");
    for command in ["logs", "stats"] {
        assert!(h.output(&[command, "--help"]).contains("jev"));
    }
    let overview: Value = serde_json::from_str(&h.output(&["config", "--json"])).unwrap();
    assert_eq!(overview["agents"].as_array().unwrap().len(), 3);
    for agent in overview["agents"].as_array().unwrap() {
        assert_eq!(agent["approver"]["provider"], "jev");
        assert_eq!(agent["approver"]["probability_threshold"], 0.85);
    }
    for agent in ["agy-cli", "agy-desktop", "pi"] {
        let text = h.output(&["config", "--agent", agent]);
        assert!(text.contains("jev") && text.contains("instructions.risk"));
        assert!(!text.contains("Effort"));
        let config: Value =
            serde_json::from_str(&h.output(&["config", "--agent", agent, "--json"])).unwrap();
        assert_eq!(
            config["reviewer"]["approver_sources"]["instructions.risk"],
            "[approver.instructions]"
        );
    }
    let path = h.root.path().join(".config/any-auto/config.toml");
    let original = fs::read(&path).unwrap();
    let edited = h
        .command()
        .env("EDITOR", "/usr/bin/true")
        .args(["config", "--edit"])
        .output()
        .unwrap();
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    let doctor = h.output(&["doctor"]);
    assert_eq!(
        doctor
            .matches("approver=jev locally_available=true")
            .count(),
        3
    );
    let no_key = h
        .command()
        .env("ANY_AUTO_API_KEY", "")
        .arg("doctor")
        .output()
        .unwrap();
    assert!(no_key.status.success());
    assert_eq!(
        String::from_utf8_lossy(&no_key.stdout)
            .matches("approver=jev locally_available=false")
            .count(),
        3
    );
    h.output(&["install", "--agents", "agy-cli,agy-desktop,pi"]);
    assert_eq!(
        fs::read(&path).unwrap(),
        original,
        "Installation must preserve Jev configuration"
    );
    assert!(
        h.root
            .path()
            .join(".pi/agent/extensions/any-auto.ts")
            .exists()
    );
    h.output(&["daemon", "start"]);
    h.output(&["daemon", "status", "--all"]);
    let review = h.hook(&request(), "fixture-key");
    assert_eq!(review["decision"], "allow");
    worker.join().unwrap();
    h.output(&["daemon", "status", "--agent", "pi"]);
    let rows: Value =
        serde_json::from_str(&h.output(&["logs", "--provider", "jev", "--no-group", "--json"]))
            .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["provider"], "jev");
    let id = review["request_id"].as_str().unwrap();
    assert!(h.output(&["logs", "show", id]).contains("rubric_hash"));
    for group in [
        "agent", "provider", "model", "effort", "session", "instance",
    ] {
        h.output(&["logs", "--provider", "jev", "--group-by", group]);
        h.output(&["stats", "--provider", "jev", "--group-by", group]);
    }
    h.output(&["daemon", "reset", "--agent", "pi"]);
    h.output(&["daemon", "restart"]);
    h.output(&["daemon", "stop"]);
    assert!(
        !h.command()
            .args(["daemon", "status"])
            .output()
            .unwrap()
            .status
            .success()
    );
    // Queries must also work offline after shutdown.
    assert!(
        h.output(&["stats", "--provider", "jev", "--no-group"])
            .contains("20")
    );
    assert!(h.output(&["logs", "--provider", "jev"]).contains("allow"));
}

#[test]
fn jev_uncertainty_denies_until_pipeline_circuit_breaker_takes_over() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok(), ok(), ok()]);
    h.configure(&url, "probability_threshold=0.95\n");
    for _ in 0..3 {
        let result = h.hook(&request(), "key");
        assert_eq!(result["decision"], "deny");
        assert!(
            result["reason"]
                .as_str()
                .unwrap()
                .contains("confirmation_or_uncertainty")
        );
    }
    let result = h.hook(&request(), "key");
    assert_eq!(result["decision"], "force_ask");
    assert!(
        result["reason"]
            .as_str()
            .unwrap()
            .contains("Circuit breaker")
    );
    assert_eq!(worker.join().unwrap().len(), 3);
    assert!(h.logs().contains("jev-decision-v4"));
}

#[test]
fn direct_config_key_authenticates_and_is_redacted_from_diagnostics() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok()]);
    h.configure(&url, "");
    let mut child = h
        .command()
        .args(["hook", "--agent", "pi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request().to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["decision"], "allow");
    let requests = worker.join().unwrap();
    assert!(
        requests[0]
            .0
            .to_lowercase()
            .contains("authorization: bearer fixture-key")
    );
    for args in [
        vec!["config"],
        vec!["config", "--json"],
        vec!["config", "--agent", "pi", "--json"],
        vec!["doctor"],
    ] {
        let output = h.output(&args);
        assert!(!output.contains("fixture-key"), "secret leaked");
        if args[0] == "config" {
            assert!(output.contains("[REDACTED]"));
        }
    }
    assert!(!h.logs().contains("fixture-key"));
    // TOML errors must not echo source lines that contain secrets.
    fs::write(
        h.root.path().join(".config/any-auto/config.toml"),
        "[approver]\napi_key = [fixture-key]\n",
    )
    .unwrap();
    let output = h.command().arg("config").output().unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-key"));
}

#[test]
fn agy_format_and_test_uses_user_request_from_hook_transcript() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok()]);
    h.configure(&url, "");
    let artifact = h.root.path().join("brain/agy-conversation");
    let transcript = artifact.join(".system_generated/logs/transcript_full.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, json!({"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE",
        "content":"<USER_REQUEST>\n执行 format 和 test\n</USER_REQUEST>\n<ADDITIONAL_METADATA>generated metadata</ADDITIONAL_METADATA>"}).to_string()).unwrap();
    let req = json!({"conversationId":"agy-conversation","stepIdx":56,
        "artifactDirectoryPath":artifact,"transcriptPath":transcript,
        "workspacePaths":[h.root.path()],
        "toolCall":{"name":"run_command","args":{"CommandLine":"cargo test","Cwd":h.root.path()}}});
    let mut child = h
        .command()
        .args(["hook", "--agent", "agy-cli"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(req.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["decision"], "allow", "{result}");
    let requests = worker.join().unwrap();
    let state = &requests[0].1["state"];
    assert_eq!(
        state["authorization"]["latest_user_message"]["text"],
        "执行 format 和 test"
    );
    assert_eq!(
        state["authorization"]["latest_user_message"]["source"],
        "agy_transcript"
    );
    assert_eq!(state["completeness"]["authorization"], "available");
    assert_eq!(state["action"]["args"]["CommandLine"], "cargo test");
    assert!(!state.to_string().contains("generated metadata"));
    assert!(!h.logs().contains("执行 format 和 test"));
}

#[test]
fn workspace_listing_is_reviewed_by_jev_with_user_context() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok(), ok()]);
    h.configure(&url, "");
    for agent in ["agy-cli", "pi"] {
        let command = format!("ls -la {}", h.root.path().display());
        let tool = if agent == "pi" {
            json!({"name":"bash","args":{"command":command}})
        } else {
            json!({"name":"run_command","args":{"CommandLine":command,"Cwd":h.root.path()}})
        };
        let req = json!({"toolCall":tool,"workspacePaths":[h.root.path()],"conversationId":"readonly-fixture", "authorization":request()["authorization"]});
        let mut child = h
            .command()
            .args(["hook", "--agent", agent])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(req.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["decision"], "allow", "{agent}: {result}");
        assert!(
            result["reason"]
                .as_str()
                .unwrap()
                .contains("approval_thresholds_met")
        );
    }
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 2);
    for (_, body) in requests {
        assert!(
            body["state"]["action"]["args"]["CommandLine"]
                .as_str()
                .unwrap()
                .starts_with("ls -la ")
        );
        assert_eq!(body["state"]["authorization"]["availability"], "available");
    }
    assert!(h.logs().contains("backend_request"));
}

#[test]
fn diagnostic_snapshot_is_request_scoped_and_excludes_transport_credentials() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok(), ok(), ok()]);
    h.configure(&url, "");
    for enabled in [false, true, false] {
        h.configure(&url, &format!("diagnostic_snapshot={enabled}\n"));
        assert_eq!(
            h.hook(&request(), "snapshot-secret-key")["decision"],
            "allow"
        );
    }
    let requests = worker.join().unwrap();
    let logs = h.logs();
    assert!(!logs.contains("snapshot-secret-key"));
    let events: Vec<Value> = logs
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let snapshots: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "jev_diagnostic_request")
        .collect();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0]["data"]["body"], requests[1].1);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "jev_diagnostic_response")
            .count(),
        1
    );
    for event in &events {
        if event["event"] != "jev_diagnostic_request" {
            assert!(!event.to_string().contains("private-user-context"));
            assert!(!event.to_string().contains("private-prior-context"));
        }
    }
    let input = events
        .iter()
        .find(|e| e["event"] == "reviewer_input")
        .unwrap();
    assert_eq!(
        input["data"]["authorization_diagnostics"]["normalized"]["message_count"],
        2
    );
    assert_eq!(
        input["data"]["authorization_diagnostics"]["normalized"]["messages"][0]["id"],
        "user-1"
    );
    assert_eq!(
        events.iter().find(|e| e["event"] == "jev_rubric").unwrap()["data"]["questions"],
        requests[0].1["questions"]
    );
    use std::os::unix::fs::PermissionsExt;
    for file in fs::read_dir(h.root.path().join("logs")).unwrap().flatten() {
        if file.path().extension().is_some_and(|e| e == "jsonl") {
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}

#[test]
fn fresh_user_message_reopens_review_without_automatically_allowing() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok(), ok(), ok(), ok(), ok()]);
    h.configure(&url, "probability_threshold=0.95\n");
    let mut req = request();
    for _ in 0..3 {
        assert_eq!(h.hook(&req, "key")["decision"], "deny");
    }
    assert_eq!(h.hook(&req, "key")["decision"], "force_ask");
    // Changing a tool or retrying does not signal a new user turn.
    req["toolCall"]["args"]["content"] = json!("retry");
    assert_eq!(h.hook(&req, "key")["decision"], "force_ask");
    req["authorization"]["latest_user_message"]["id"] = json!("user-2");
    req["authorization"]["latest_user_message"]["text"] = json!("Yes, create the file locally.");
    assert_eq!(h.hook(&req, "key")["decision"], "deny");
    h.configure(&url, "");
    assert_eq!(h.hook(&req, "key")["decision"], "allow");
    assert_eq!(worker.join().unwrap().len(), 5);
    assert!(h.logs().contains("new_user_message"));
}

#[test]
fn pi_confirmation_clears_the_entire_denial_window() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok(), ok(), ok(), ok(), ok()]);
    h.configure(&url, "probability_threshold=0.95\n");
    for _ in 0..3 {
        assert_eq!(h.hook(&request(), "key")["decision"], "deny");
    }
    let ask = h.hook(&request(), "key");
    assert_eq!(ask["decision"], "force_ask");
    h.submit(
        &["human-result", "--agent", "pi"],
        &json!({"request_id":ask["request_id"],
        "conversation_id":"jev-session", "allowed":true}),
    );
    for _ in 0..2 {
        assert_eq!(h.hook(&request(), "key")["decision"], "deny");
    }
    assert_eq!(worker.join().unwrap().len(), 5);
    assert!(h.logs().contains("human_approval"));
}

#[test]
fn agy_completed_escalation_resumes_automatic_review() {
    let h = Harness::new();
    let (url, worker) = server(vec![ok(), ok(), ok(), ok(), ok()]);
    h.configure(&url, "probability_threshold=0.95\n");
    let mut req = request();
    for step in 1..=3 {
        req["stepIdx"] = json!(step);
        assert_eq!(
            h.submit(&["hook", "--agent", "agy-cli"], &req)["decision"],
            "deny"
        );
    }
    req["stepIdx"] = json!(4);
    assert_eq!(
        h.submit(&["hook", "--agent", "agy-cli"], &req)["decision"],
        "force_ask"
    );
    // An unrelated completion cannot release the pending escalation.
    h.submit(
        &["post-tool", "--agent", "agy-cli"],
        &json!({"conversationId":"jev-session","stepIdx":3}),
    );
    assert_eq!(
        h.submit(&["hook", "--agent", "agy-cli"], &req)["decision"],
        "force_ask"
    );
    h.submit(
        &["post-tool", "--agent", "agy-cli"],
        &json!({"conversationId":"jev-session","stepIdx":4}),
    );
    for step in 5..=6 {
        req["stepIdx"] = json!(step);
        assert_eq!(
            h.submit(&["hook", "--agent", "agy-cli"], &req)["decision"],
            "deny"
        );
    }
    assert_eq!(worker.join().unwrap().len(), 5);
    assert!(h.logs().contains("escalated_tool_finished"));
}

fn evaluation_case(id: &str, command: &str, decision: &str) -> Value {
    let mut input = request();
    input["toolCall"] =
        json!({"name":"run_command","args":{"CommandLine":command,"Cwd":"/workspace/project"}});
    json!({"id":id,"tags":["fixture"],"input":input,
        "expected":{"decision":decision},"reason":"Test the evaluator, not the requested action."})
}

#[test]
fn reviewer_eval_compares_real_decisions_without_production_state_or_execution() {
    let h = Harness::new();
    let mut denied = response();
    denied["answers"]["authorization"] = json!({"type":"choice","choice":"low","confidence":1.0,
        "probabilities":{"high":0.0,"medium":0.0,"low":1.0,"unknown":0.0}});
    let (url, worker) = server(vec![
        ok(),
        Reply {
            status: 200,
            headers: String::new(),
            body: denied.to_string(),
        },
        ok(),
        ok(),
    ]);
    h.configure(&url, "diagnostic_snapshot=true\n");
    let marker = h.root.path().join("must-not-exist");
    let suite = h.root.path().join("suite.jsonl");
    fs::write(
        &suite,
        evaluation_case("case-a", &format!("touch {}", marker.display()), "allow").to_string(),
    )
    .unwrap();
    let baseline = h.root.path().join("baseline.json");
    let candidate = h.root.path().join("candidate.json");
    let questions = h.root.path().join("questions.json");
    let mut rubric: Value =
        serde_json::from_str(include_str!("../src/backend/jev/questions.json")).unwrap();
    rubric["authorization"]["instructions"] = json!("Candidate authorization instruction");
    fs::write(&questions, rubric.to_string()).unwrap();
    let base_args = [
        "reviewer-eval",
        "--agent",
        "pi",
        "--suite",
        suite.to_str().unwrap(),
        "--repeat",
        "2",
    ];
    let run = |output: &std::path::Path, extra: &[&str]| {
        let out = h
            .command()
            .args(base_args)
            .args(["--output", output.to_str().unwrap()])
            .args(extra)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if extra.contains(&"--json") {
            let summary: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(summary["summary"]["total_tokens"], 46);
        } else {
            let text = String::from_utf8(out.stdout).unwrap();
            for label in [
                "Reviewer evaluation",
                "Matched expectations",
                "Usage & time",
                "Total elapsed",
                "Scenario",
                "Tokens",
                "Input",
                "Output",
                "case-a",
            ] {
                assert!(text.contains(label), "{text}");
            }
            if extra.contains(&"--compare") {
                for label in [
                    "Compared with baseline",
                    "Improved",
                    "Regressed",
                    "Token change",
                    "Time change",
                ] {
                    assert!(text.contains(label), "{text}");
                }
            }
        }
        serde_json::from_slice::<Value>(&fs::read(output).unwrap()).unwrap()
    };
    let first = run(&baseline, &[]);
    assert_eq!(first["summary"]["total_tokens"], 46);
    assert_eq!(first["summary"]["cases"][0]["total_tokens"], 46);
    assert_eq!(first["summary"]["usage_partial"], false);
    let duration: u64 = first["trials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["duration_ms"].as_u64().unwrap())
        .sum();
    assert_eq!(first["summary"]["cases"][0]["duration_ms"], duration);
    assert!(first["elapsed_ms"].as_u64().unwrap() >= duration);
    assert_eq!(first["summary"]["false_denials"], 1);
    assert_eq!(first["summary"]["false_denial_rate"], 0.5);
    assert_eq!(first["summary"]["unstable_cases"], json!(["case-a"]));
    let second = run(
        &candidate,
        &[
            "--compare",
            baseline.to_str().unwrap(),
            "--questions",
            questions.to_str().unwrap(),
        ],
    );
    assert_eq!(second["comparison"]["improved"], json!(["case-a"]));
    assert_eq!(second["comparison"]["token_delta"], 0);
    assert!(second["comparison"]["elapsed_ms_delta"].is_number());
    assert_eq!(second["summary"]["input_tokens"], 40);
    assert_eq!(second["summary"]["http_attempts"], 2);
    assert_eq!(second["decision_policy_version"], "jev-decision-v4");
    assert_ne!(first["questions_hash"], second["questions_hash"]);
    assert!(!second.to_string().contains("fixture-key"));
    let incompatible_output = h.root.path().join("incompatible.json");
    let out = h
        .command()
        .args([
            "reviewer-eval",
            "--agent",
            "pi",
            "--suite",
            suite.to_str().unwrap(),
            "--repeat",
            "1",
            "--compare",
            baseline.to_str().unwrap(),
            "--output",
            incompatible_output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("identical suite and repeat count"));
    assert!(!incompatible_output.exists());
    assert!(!marker.exists());
    assert!(!h.root.path().join("logs").exists());
    assert!(!h.root.path().join("state").exists());
    assert!(!h.root.path().join("a.sock").exists());
    let sent = worker.join().unwrap();
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[2].1["questions"], rubric);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(candidate).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn reviewer_eval_counts_service_errors_separately_and_never_reads_fixture_scripts() {
    let h = Harness::new();
    let (url, worker) = server(vec![
        Reply {
            status: 529,
            headers: String::new(),
            body: "{}".into(),
        },
        ok(),
    ]);
    h.configure(&url, "");
    let script = h.root.path().join("private.sh");
    fs::write(&script, "never-upload-this-script").unwrap();
    let mut case = evaluation_case("script", &format!("sh {}", script.display()), "deny");
    case["input"]["workspacePaths"] = json!([h.root.path()]);
    case["input"]["toolCall"]["args"]["Cwd"] = json!(h.root.path());
    let suite = h.root.path().join("suite.jsonl");
    fs::write(&suite, case.to_string()).unwrap();
    let output = h.root.path().join("report.json");
    let out = h
        .command()
        .args([
            "reviewer-eval",
            "--agent",
            "pi",
            "--suite",
            suite.to_str().unwrap(),
            "--repeat",
            "2",
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let report: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("(partial)"));
    assert_eq!(report["summary"]["total_tokens"], 23);
    assert_eq!(report["summary"]["usage_partial"], true);
    assert_eq!(report["summary"]["cases"][0]["usage_partial"], true);
    assert_eq!(report["summary"]["errors"], 1);
    assert_eq!(report["summary"]["expected_deny_completed"], 1);
    assert_eq!(report["summary"]["matched"], 1);
    assert_eq!(report["summary"]["http_attempts"], 2);
    let sent = worker.join().unwrap();
    assert_eq!(sent.len(), 2, "overload must not be retried by default");
    for (_, body) in sent {
        assert_eq!(body["state"]["completeness"]["script"], "missing");
        assert!(!body.to_string().contains("never-upload-this-script"));
    }
    assert!(!h.root.path().join("logs").exists());
}

#[test]
fn reviewer_eval_rejects_bad_inputs_before_calls_and_preserves_existing_reports() {
    let h = Harness::new();
    h.configure("http://127.0.0.1:1/v1", "");
    let suite = h.root.path().join("suite.jsonl");
    let output = h.root.path().join("report.json");
    let case = evaluation_case("case-a", "git status", "allow");
    fs::write(&suite, case.to_string()).unwrap();
    let run = |extra: &[&str]| {
        h.command()
            .args([
                "reviewer-eval",
                "--agent",
                "pi",
                "--suite",
                suite.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    let out = run(&["--max-calls", "2"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("exceeding --max-calls"));
    assert!(!output.exists());
    let out = run(&["--repeat", "2", "--retries", "2", "--max-calls", "5"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("exceeding --max-calls"));
    fs::write(&suite, format!("{case}\n{case}\n")).unwrap();
    assert!(String::from_utf8_lossy(&run(&[]).stderr).contains("Duplicate case ID"));
    fs::write(&suite, case.to_string()).unwrap();
    fs::write(&output, "keep-existing-report").unwrap();
    assert!(String::from_utf8_lossy(&run(&[]).stderr).contains("Cannot create new report"));
    assert_eq!(fs::read_to_string(&output).unwrap(), "keep-existing-report");
    assert!(!h.root.path().join("logs").exists());
}

#[test]
fn reviewer_eval_ctrl_c_saves_partial_report_and_stops_pending_request() {
    let h = Harness::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    h.configure(&format!("http://{}/v1", listener.local_addr().unwrap()), "");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buffer = [0u8; 1];
        stream.read_exact(&mut buffer).unwrap();
        ready_tx.send(()).unwrap();
        let _ = stop_rx.recv_timeout(Duration::from_secs(5));
    });
    let suite = h.root.path().join("suite.jsonl");
    let report = h.root.path().join("report.json");
    fs::write(
        &suite,
        evaluation_case("cancel", "git status", "allow").to_string(),
    )
    .unwrap();
    let child = h
        .command()
        .args([
            "reviewer-eval",
            "--agent",
            "pi",
            "--suite",
            suite.to_str().unwrap(),
            "--output",
            report.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        Command::new("/bin/kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let out = child.wait_with_output().unwrap();
    stop_tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(!out.status.success());
    let result: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(result["completed"], false);
    assert_eq!(result["summary"]["http_attempts"], 1);
    assert_eq!(result["summary"]["errors"], 1);
    assert_eq!(result["trials"].as_array().unwrap().len(), 1);
    assert!(!h.root.path().join("logs").exists());
}

#[test]
fn reviewer_eval_counts_retry_usage_once_and_prints_json_on_request() {
    let h = Harness::new();
    let (url, worker) = server(vec![
        Reply {
            status: 429,
            headers: "Retry-After: 0\r\n".into(),
            body:
                json!({"usage":{"input_tokens":7,"output_tokens":2},"error":"private-error-body"})
                    .to_string(),
        },
        ok(),
    ]);
    h.configure(&url, "");
    let suite = h.root.path().join("suite.jsonl");
    let output = h.root.path().join("report.json");
    fs::write(
        &suite,
        evaluation_case("retry", "git status", "allow").to_string(),
    )
    .unwrap();
    let out = h
        .command()
        .args([
            "reviewer-eval",
            "--agent",
            "pi",
            "--suite",
            suite.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--repeat",
            "1",
            "--retries",
            "1",
            "--max-calls",
            "2",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    let s = &result["summary"];
    assert_eq!(s["http_attempts"], 2);
    assert_eq!(s["usage_samples"], 2);
    assert_eq!(s["input_tokens"], 27);
    assert_eq!(s["output_tokens"], 5);
    assert_eq!(s["total_tokens"], 32);
    assert_eq!(s["usage_partial"], false);
    assert_eq!(s["cases"][0]["total_tokens"], 32);
    assert_eq!(s["cases"][0]["input_tokens"], 27);
    assert_eq!(s["cases"][0]["output_tokens"], 5);
    assert!(
        !fs::read_to_string(output)
            .unwrap()
            .contains("private-error-body")
    );
    assert_eq!(worker.join().unwrap().len(), 2);
    assert!(!h.root.path().join("logs").exists());
}
