use any_auto::{config::Mode, sessions::directory};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

struct Agent {
    root: tempfile::TempDir,
}
impl Agent {
    fn new() -> Self {
        let agent = Self {
            root: tempfile::tempdir_in("/tmp").unwrap(),
        };
        for backend in ["agentapi", "agy"] {
            let script = format!(
                "#!/bin/sh\nbackend={backend}\n{}",
                r#"
cid=
if [ "$backend" = agentapi ]; then
 if [ "$1" = send-message ]; then cid=$2; payload=$3; fi
else
 payload=$2
 while [ "$#" -gt 0 ]; do
  if [ "$1" = --conversation ]; then shift; cid=$1; fi
  shift
 done
fi
if [ -z "$cid" ]; then
 cid="$backend-$$"
 printf 'new %s %s\n' "$cid" "$PWD" >> "$HOME/events"
 if [ -f "$HOME/block-new" ]; then read -r release < "$HOME/gate-new"; fi
 if [ "$backend" = agentapi ]; then
  printf '{"conversationId":"%s"}\n' "$cid"
 else
  printf '{"conversation_id":"%s","status":"SUCCESS","response":"READY"}\n' "$cid"
 fi
 exit 0
fi
case "$payload" in
 *'"CommandLine":"A1"'*) tag=A1;;
 *'"CommandLine":"A2"'*) tag=A2;;
 *'"CommandLine":"B1"'*) tag=B1;;
 *'"CommandLine":"DENY"'*) tag=DENY;;
 *) tag=other;;
esac
printf 'send %s %s\n' "$cid" "$tag" >> "$HOME/events"
if [ -f "$HOME/fail-$cid" ]; then echo '{"error":"expired"}'; exit 1; fi
if [ "$tag" = A1 ] && [ -f "$HOME/block-A1" ]; then read -r release < "$HOME/gate-A1"; fi
outcome=allow
[ "$tag" = DENY ] && outcome=deny
response='"{\"outcome\":\"'"$outcome"'\",\"rationale\":\"'"$cid"'\"}"'
printf '{"conversation_id":"%s","status":"SUCCESS","response":%s}\n' "$cid" "$response"
"#
            );
            let path = agent.root.path().join(backend);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        agent
    }
    fn command(&self, mode: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_any-auto"));
        c.args([
            "--agent",
            match mode {
                "cli" => "agy-cli",
                "sidecar" => "agy-desktop",
                other => other,
            },
        ])
        .env(
            "ANY_AUTO_PROVIDER",
            if mode == "sidecar" { "agentapi" } else { "cli" },
        )
        .env("HOME", self.root.path())
        .env("PATH", self.root.path())
        .env("ANY_AUTO_SOCKET", self.root.path().join("a.sock"))
        .env("ANY_AUTO_STATE_DIR", self.root.path().join("state"))
        .env("ANY_AUTO_LOG_DIR", self.root.path().join("logs"))
        .env("ANY_AUTO_SILENT", "1")
        .env_remove("ANY_AUTO_REVIEWER")
        .env_remove("ANY_AUTO_MODEL")
        .env_remove("ANY_AUTO_CLI_MODEL")
        .env_remove("ANY_AUTO_PROMPT")
        .current_dir(self.root.path());
        c
    }
    fn run(&self, mode: &str, op: &str) -> Value {
        let out = self.command(mode).args(["daemon", op]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn ipc(&self, mode: &str, session: Option<&str>, tag: &str) -> UnixStream {
        let mut socket = UnixStream::connect(self.root.path().join("a.sock")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(socket, "{}", json!({"action":"evaluate", "context":{"mode":mode,"instance":"default","environment":{"PATH":self.root.path().to_str().unwrap()}}, "mode":mode, "request_id":format!("{mode}-{tag}"), "user_session_id":session, "toolCall":{"name":"run_command","args":{"CommandLine":tag}}})).unwrap();
        socket
    }
    fn finish(socket: UnixStream) -> Value {
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()["assessment"].clone()
    }
    fn review(&self, mode: &str, session: Option<&str>) -> String {
        let a = Self::finish(self.ipc(mode, session, "B1"));
        assert_eq!(a["outcome"], "allow", "{a}");
        a["rationale"].as_str().unwrap().to_owned()
    }
    fn hook(&self, mode: &str, id: Option<&str>, key: &str, tag: &str) -> Value {
        let mut input = json!({"toolCall":{"name":"run_command","args":{"CommandLine":tag}}});
        if let Some(id) = id {
            input[key] = id.into();
        }
        let mut c = self
            .command(mode)
            .arg("hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        serde_json::from_slice(&c.wait_with_output().unwrap().stdout).unwrap()
    }
    fn events(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.root.path().join("events"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_whitespace().map(str::to_owned).collect())
            .collect()
    }
    fn wait(&self, predicate: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(
                Instant::now() < end,
                "Timed out; events: {:?}",
                self.events()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn gate(&self, name: &str) {
        fs::write(self.root.path().join(format!("block-{name}")), "").unwrap();
        assert!(
            Command::new("mkfifo")
                .arg(self.root.path().join(format!("gate-{name}")))
                .status()
                .unwrap()
                .success()
        );
    }
    fn release(&self, name: &str) {
        fs::remove_file(self.root.path().join(format!("block-{name}"))).unwrap();
        // O_RDWR avoids blocking the controller if a regression killed the reader.
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.root.path().join(format!("gate-{name}")))
            .unwrap()
            .write_all(b"go\n")
            .unwrap();
    }
    fn session_file(&self, mode: &str, id: &str) -> PathBuf {
        directory(&self.root.path().join("state").join(mode), id).join("reviewer_session.json")
    }
}
impl Drop for Agent {
    fn drop(&mut self) {
        for mode in ["cli", "sidecar"] {
            let _ = self.command(mode).args(["daemon", "stop"]).output();
        }
    }
}

#[test]
fn concurrent_first_requests_share_only_their_own_session() {
    for mode in ["cli", "sidecar"] {
        let h = Agent::new();
        h.gate("new");
        h.gate("A1");
        h.run(mode, "start");
        let a1 = h.ipc(mode, Some("A"), "A1");
        h.wait(|| h.events().iter().any(|e| e[0] == "new"));
        let a2 = h.ipc(mode, Some("A"), "A2");
        h.wait(|| h.run(mode, "status")["active_evaluations"] == 2);
        assert_eq!(h.events().iter().filter(|e| e[0] == "new").count(), 1);
        h.release("new");
        h.wait(|| h.events().iter().any(|e| e[0] == "send" && e[2] == "A1"));
        // B must finish while A1 is blocked by a FIFO, not merely be faster than A.
        let b = Agent::finish(h.ipc(mode, Some("B"), "B1"));
        assert_eq!(b["outcome"], "allow");
        assert!(!h.events().iter().any(|e| e[0] == "send" && e[2] == "A2"));
        h.release("A1");
        let a1 = Agent::finish(a1);
        let a2 = Agent::finish(a2);
        assert_eq!(a1["outcome"], "allow");
        assert_eq!(a2["outcome"], "allow");
        assert_eq!(a1["rationale"], a2["rationale"]);
        assert_ne!(a1["rationale"], b["rationale"]);
        assert_eq!(h.events().iter().filter(|e| e[0] == "new").count(), 2);
        assert_eq!(h.run(mode, "status")["cached_sessions"], 2);
    }
}

#[test]
fn sessions_restore_retry_and_restart_with_mode_isolation() {
    let h = Agent::new();
    let mut originals = Vec::new();
    for mode in ["cli", "sidecar"] {
        h.run(mode, "start");
        let a = h.review(mode, Some("A"));
        let b = h.review(mode, Some("B"));
        assert_ne!(a, b);
        h.run(mode, "stop");
        h.run(mode, "start");
        assert_eq!(h.review(mode, Some("A")), a);
        assert_eq!(h.review(mode, Some("B")), b);
        fs::write(h.root.path().join(format!("fail-{a}")), "").unwrap();
        let new_a = h.review(mode, Some("A"));
        assert_ne!(new_a, a);
        assert_eq!(h.review(mode, Some("B")), b);
        originals.push((new_a, b));
    }
    assert_ne!(originals[0].0, originals[1].0);
    h.run("cli", "reset");
    for id in ["A", "B"] {
        assert!(!h.session_file("cli", id).exists());
        assert!(h.session_file("sidecar", id).exists());
    }
    assert_eq!(h.review("sidecar", Some("A")), originals[1].0);
    assert_eq!(h.review("sidecar", Some("B")), originals[1].1);
    assert_ne!(h.review("cli", Some("A")), originals[0].0);
    assert_ne!(h.review("cli", Some("B")), originals[0].1);
    let cli_workspaces: Vec<_> = h
        .events()
        .into_iter()
        .filter(|e| e[0] == "new" && e[1].starts_with("agy-"))
        .map(|e| e[2].clone())
        .collect();
    assert_ne!(cli_workspaces[0], cli_workspaces[1]);
}

#[test]
fn hook_aliases_special_ids_and_missing_ids_do_not_share_context() {
    for mode in ["cli", "sidecar"] {
        let h = Agent::new();
        // Different raw IDs used to collide when punctuation was replaced by underscores.
        let a = h.hook(mode, Some("../../用户/a"), "conversationId", "B1");
        let again = h.hook(mode, Some("../../用户/a"), "conversation_id", "B1");
        let b = h.hook(mode, Some("../../用户?a"), "conversationId", "B1");
        assert_eq!(a["decision"], "allow");
        assert_eq!(a["reason"], again["reason"]);
        assert_ne!(a["reason"], b["reason"]);
        for id in ["../../用户/a", "../../用户?a"] {
            let file = h.session_file(mode, id);
            assert!(file.exists());
            let hash = file
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
            assert_eq!(hash.len(), 64);
            assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        }
        for _ in 0..3 {
            assert_eq!(
                h.hook(mode, Some("../../用户/a"), "conversationId", "DENY")["decision"],
                "deny"
            );
        }
        assert_eq!(
            h.hook(mode, Some("../../用户/a"), "conversationId", "DENY")["decision"],
            "force_ask"
        );
        assert_eq!(
            h.hook(mode, Some("../../用户?a"), "conversationId", "B1")["decision"],
            "allow"
        );
        let first = h.hook(mode, None, "conversationId", "B1");
        let second = h.hook(mode, Some(" "), "conversationId", "B1");
        assert_eq!(first["decision"], "allow");
        assert_eq!(second["decision"], "allow");
        assert_ne!(first["reason"], second["reason"]);
        assert_eq!(h.run(mode, "status")["cached_sessions"], 2);
        assert_eq!(
            fs::read_dir(h.root.path().join("state").join(mode).join("sessions"))
                .unwrap()
                .count(),
            2
        );
        if mode == "cli" {
            for event in h.events().iter().rev().filter(|e| e[0] == "new").take(2) {
                assert!(
                    !PathBuf::from(&event[2]).parent().unwrap().exists(),
                    "Temporary workspace leaked"
                );
            }
        }
    }
}

#[test]
fn legacy_shared_session_is_not_loaded() {
    for mode in [Mode::Cli, Mode::Sidecar] {
        let h = Agent::new();
        let base = h.root.path().join("state").join(mode.as_str());
        fs::create_dir_all(&base).unwrap();
        fs::write(
            base.join("reviewer_session.json"),
            r#"{"conversationId":"legacy-shared"}"#,
        )
        .unwrap();
        h.run(mode.as_str(), "start");
        assert_ne!(h.review(mode.as_str(), Some("A")), "legacy-shared");
    }
}

#[test]
fn backend_timeout_releases_session_for_next_request() {
    let threads: Vec<_> = ["cli", "sidecar"]
        .into_iter()
        .map(|mode| {
            std::thread::spawn(move || {
                let h = Agent::new();
                h.gate("A1");
                h.run(mode, "start");
                let request = h.ipc(mode, Some("A"), "A1");
                request
                    .set_read_timeout(Some(Duration::from_secs(26)))
                    .unwrap();
                let result = Agent::finish(request);
                assert_eq!(result["outcome"], "deny", "{result}");
                assert!(
                    result["rationale"].as_str().unwrap().contains("timed out"),
                    "{result}"
                );
                assert_eq!(h.run(mode, "status")["active_evaluations"], 0);
                h.release("A1");
                h.review(mode, Some("A"));
                assert_eq!(h.events().iter().filter(|e| e[0] == "new").count(), 1);
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}
