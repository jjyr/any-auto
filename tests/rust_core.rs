use any_auto::{
    parser,
    pipeline::{Breaker, blacklist, read_only},
    policy::parse,
};
use serde_json::json;

#[test]
fn bash_and_permission_overrides() {
    for (command, expected) in [
        (
            "mise run local:down 2>&1; sleep 2; mise run test:e2e 2>&1",
            vec![
                "mise run local:down 2>&1",
                "sleep 2",
                "mise run test:e2e 2>&1",
            ],
        ),
        (
            "cargo build -p agent && (cd tests/deploy && cargo build)",
            vec!["cargo build -p agent", "cd tests/deploy", "cargo build"],
        ),
        (
            "REMOVE_OLD_STATE=y PATH=\"/opt/bin:$PATH\" ./start.sh",
            vec!["./start.sh"],
        ),
        (
            "echo $(which python) && echo `whoami`",
            vec![
                "echo $(which python)",
                "which python",
                "echo `whoami`",
                "whoami",
            ],
        ),
        (
            "cat << 'EOF' > test.ts\nimport foo from 'bar';\nEOF\nnpm test",
            vec![
                "cat << 'EOF' > test.ts\nimport foo from 'bar';\nEOF",
                "npm test",
            ],
        ),
        (
            "$( (cmd1 | cmd2) > /dev/null )",
            vec!["$( (cmd1 | cmd2) > /dev/null )", "cmd1", "cmd2"],
        ),
    ] {
        assert_eq!(parser::commands(command), expected, "{command}");
    }
    assert_eq!(
        parser::overrides(
            "run_command",
            &json!({"CommandLine":"gh pr create --title hi"})
        ),
        ["command(gh pr create)"]
    );
    assert_eq!(
        parser::overrides("edit_file", &json!({"TargetFile":"/tmp/test"})),
        ["file(/tmp/test)"]
    );
    let text = format!("echo {}", "中".repeat(100));
    assert_eq!(
        parser::overrides("run_command", &json!({"CommandLine":text})),
        [format!(
            "command({})",
            text.chars().take(80).collect::<String>()
        )]
    );
}
#[test]
fn deterministic_policies() {
    for cmd in [
        "rm -rf /",
        "rm -rf /*",
        "rm -rf ~",
        "rm -rf /path/.git",
        "mkfs.ext4 /dev/sda1",
        ":(){ :|:& };:",
        "echo $(rm -rf /)",
        "(rm -rf /)",
        "`rm -rf /`",
        "echo $(rm -rf $HOME)",
    ] {
        assert!(blacklist(cmd).is_some(), "{cmd}");
    }
    for cmd in [
        "ls -la",
        "git status",
        "cargo build",
        "echo 'Hello world'",
        "arm-none-eabi-gcc main.c",
    ] {
        assert!(blacklist(cmd).is_none(), "{cmd}");
    }
    assert!(read_only("view_file"));
    assert!(!read_only("run_command"));
}
#[test]
fn tolerant_review_fails_closed() {
    for raw in [
        "",
        " ",
        "not JSON",
        "[]",
        "null",
        "{}",
        "{\"outcome\":\"maybe\"}",
    ] {
        assert!(parse(raw).is_err());
    }
    let raw = r#"text ```json
{"risk":"low","authorization":"medium","policy":"permitted","rationale":"test"}
``` trailing"#;
    assert_eq!(parse(raw).unwrap().risk, any_auto::policy::Risk::Low);
}
#[test]
fn breaker_persistence_and_window() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut b = Breaker::open(dir.path(), "session").unwrap();
        for _ in 0..3 {
            assert!(b.tripped().is_none());
            b.record("deny").unwrap();
        }
        assert!(b.tripped().is_some());
    }
    let mut b = Breaker::open(dir.path(), "session").unwrap();
    assert!(b.tripped().is_some());
    b.record("allow").unwrap();
    assert!(b.tripped().is_none());
    b.record("deny").unwrap();
    assert!(b.tripped().unwrap().contains("4/5"));
    assert!(
        Breaker::open(dir.path(), "other")
            .unwrap()
            .tripped()
            .is_none()
    );
}

#[test]
fn outcome_aliases_cannot_replace_structured_judgments() {
    for value in [
        json!(42),
        json!(true),
        json!(null),
        json!("allow"),
        json!([]),
    ] {
        assert!(parse(&json!({"outcome":value,"decision":"allow"}).to_string()).is_err());
    }
}

#[test]
fn shell_syntax_regressions() {
    for (command, expected) in [
        ("A=1", vec![]),
        ("A=1\nB=2\necho ready", vec!["echo ready"]),
        ("A=1 \\\n B=2 \\\n ./build.sh", vec!["./build.sh"]),
        (
            "echo <(cat /tmp/x)",
            vec!["echo <(cat /tmp/x)", "cat /tmp/x"],
        ),
        ("for x in a b; do echo \"$x\"; done", vec!["echo \"$x\""]),
        ("if test -f x; then cat x; fi", vec!["test -f x", "cat x"]),
        (
            "cat output.log | grep ERROR | wc -l",
            vec!["cat output.log", "grep ERROR", "wc -l"],
        ),
        ("killall worker || true", vec!["killall worker", "true"]),
        (
            "git commit -m 'semicolon ; and && stay quoted'",
            vec!["git commit -m 'semicolon ; and && stay quoted'"],
        ),
        (
            "echo 'import sys;sys.exit(0)' > /tmp/helper.py",
            vec!["echo 'import sys;sys.exit(0)' > /tmp/helper.py"],
        ),
    ] {
        assert_eq!(parser::commands(command), expected, "{command}");
    }
    assert_eq!(
        parser::overrides("run_command", &json!({"CommandLine":"A=1"})),
        ["command(A=1)"]
    );
    for command in [
        "cat << EOF > file.txt\nline 1\nEOF",
        "cat << \"DELIM\" > file.txt\nline 2\nDELIM",
        "cat <<- 'EOF' > file.txt\n\tline 3\n\tEOF",
        "cat <<'EOF' >> main.css\n.foo { color: red; }\nEOF",
    ] {
        assert_eq!(parser::commands(command), [command]);
    }
    assert_eq!(parser::clean("echo (foo)"), "echo (foo)");
    assert!(parser::commands("").is_empty());
    for (command, grant) in [
        ("gh", "command(gh)"),
        ("gh --help", "command(gh)"),
        ("gh pr --help", "command(gh pr)"),
        ("gh run view 123 --log", "command(gh run view)"),
    ] {
        assert_eq!(
            parser::overrides("run_command", &json!({"CommandLine":command})),
            [grant]
        );
    }
    for tool in ["write_to_file", "replace_file_content", "edit_file"] {
        assert_eq!(
            parser::overrides(tool, &json!({"target_file":"/tmp/file"})),
            ["file(/tmp/file)"]
        );
        assert!(parser::overrides(tool, &json!({})).is_empty());
    }
}

#[test]
fn backends_cannot_request_human_override() {
    for outcome in ["allow", "deny", "ask", "force_ask", "unknown"] {
        for field in ["outcome", "decision"] {
            assert!(parse(&json!({field:outcome}).to_string()).is_err());
        }
    }
}
