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
    assert!(h.logs().contains("jev-decision-v2"));
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
