use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn hook(agent: &str, payload: &Value) -> (Value, Vec<Value>) {
    let root = tempfile::tempdir().unwrap();
    let logs = root.path().join("logs");
    let mut child = Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .args(["hook", "--agent", agent])
        .env("HOME", root.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env("ANY_AUTO_LOG_DIR", &logs)
        .env("ANY_AUTO_STATE_DIR", root.path().join("state"))
        .env("ANY_AUTO_SILENT", "1")
        .env_remove("ANY_AUTO_REVIEWER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let response = serde_json::from_slice(&out.stdout).unwrap();
    let events = std::fs::read_dir(logs)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|s| s == "jsonl"))
        .flat_map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap()
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect::<Vec<Value>>()
        })
        .collect();
    (response, events)
}

#[test]
fn antigravity_responses_have_only_protocol_fields_but_keep_audit_ids() {
    for agent in ["agy-cli", "agy-desktop"] {
        for (tool, args, decision) in [
            ("view_file", json!({}), "allow"),
            ("run_command", json!({"CommandLine":"rm -rf /"}), "deny"),
        ] {
            let (response, events) = hook(
                agent,
                &json!({"request_id":"protocol-test", "toolCall":{"name":tool,"args":args}}),
            );
            assert_eq!(response["decision"], decision);
            for key in response.as_object().unwrap().keys() {
                assert!(
                    ["decision", "reason", "permissionOverrides"].contains(&key.as_str()),
                    "{agent}: unexpected field {key}: {response}"
                );
            }
            assert!(
                events
                    .iter()
                    .any(|e| e["event"] == "hook_result" && e["id"] == "protocol-test")
            );
        }
    }
}

#[test]
fn pi_responses_preserve_request_id_for_human_confirmation() {
    for (tool, args, decision) in [
        ("read", json!({}), "allow"),
        ("bash", json!({"command":"rm -rf /"}), "deny"),
    ] {
        let (response, events) = hook(
            "pi",
            &json!({"request_id":"pi-test", "builtin_tool":true, "toolCall":{"name":tool,"args":args}}),
        );
        assert_eq!(response["decision"], decision);
        assert_eq!(response["request_id"], "pi-test");
        assert!(
            events
                .iter()
                .any(|e| e["event"] == "hook_result" && e["id"] == "pi-test")
        );
    }
}
