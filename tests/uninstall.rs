use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
};
fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/nonexistent")
        .env("ANY_AUTO_SOCKET", home.join("daemon.sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}
fn ok(home: &Path, args: &[&str]) -> String {
    let output = run(home, args);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}
fn write(home: &Path, name: &str, text: &str) {
    let path = home.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
#[test]
fn selective_removal_dry_run_and_idempotence_preserve_user_data() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    ok(home, &["install", "--agents", "agy-cli,agy-desktop,pi"]);
    write(
        home,
        ".gemini/config/hooks.json",
        r#"{"any-auto":{},"other":{"enabled":true}}"#,
    );
    write(
        home,
        ".gemini/antigravity-cli/settings.json",
        r#"{"permissions":{"allow":["command(git)"]}}"#,
    );
    write(
        home,
        ".config/any-auto/config.toml",
        "[approver]\nprovider='jev'\n",
    );
    write(home, ".local/share/any-auto/history", "history");
    let hooks = fs::read(home.join(".gemini/config/hooks.json")).unwrap();
    let text = ok(home, &["uninstall", "--agents", "agy-cli,pi", "--dry-run"]);
    assert!(text.contains("Dry run"));
    assert_eq!(
        fs::read(home.join(".gemini/config/hooks.json")).unwrap(),
        hooks
    );
    assert!(home.join(".pi/agent/extensions/any-auto.ts").exists());
    ok(home, &["uninstall", "--agents", "agy-cli,pi,agy-cli"]);
    assert!(!home.join(".pi/agent/extensions/any-auto.ts").exists());
    let hooks: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join(".gemini/config/hooks.json")).unwrap()).unwrap();
    assert!(hooks.get("any-auto").is_none());
    assert_eq!(hooks["other"]["enabled"], true);
    assert!(
        home.join(".gemini/config/sidecars/approver/sidecar.json")
            .exists()
    );
    assert!(ok(home, &["uninstall", "--agents", "pi"]).contains("already uninstalled"));
    ok(home, &["uninstall", "--all"]);
    for name in [
        "sidecars/approver/sidecar.json",
        "sidecars/any-auto/approver/sidecar.json",
    ] {
        assert!(!home.join(".gemini/config").join(name).exists());
    }
    assert_eq!(
        fs::read_to_string(home.join(".config/any-auto/config.toml")).unwrap(),
        "[approver]\nprovider='jev'\n"
    );
    assert_eq!(
        fs::read_to_string(home.join(".local/share/any-auto/history")).unwrap(),
        "history"
    );
    assert_eq!(
        fs::read_to_string(home.join(".gemini/antigravity-cli/settings.json")).unwrap(),
        r#"{"permissions":{"allow":["command(git)"]}}"#
    );
    ok(home, &["uninstall", "--all"]);
}
#[test]
fn invalid_or_unowned_desktop_files_fail_before_any_removal() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    ok(home, &["install", "--agents", "agy-cli,agy-desktop,pi"]);
    for data in [
        "broken",
        r#"{"name":"approver","command":"another-tool","args":["daemon","start"]}"#,
    ] {
        write(home, ".gemini/config/sidecars/approver/sidecar.json", data);
        assert!(!run(home, &["uninstall", "--all"]).status.success());
        assert!(home.join(".pi/agent/extensions/any-auto.ts").exists());
        assert!(
            fs::read_to_string(home.join(".gemini/config/hooks.json"))
                .unwrap()
                .contains("any-auto")
        );
    }
}
#[test]
fn nonterminal_requires_explicit_selection_and_respects_custom_pi_directory() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    assert!(!run(home, &["uninstall"]).status.success());
    write(home, "custom/extensions/any-auto.ts", "extension");
    let output = Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .args(["uninstall", "--agents", "pi"])
        .env_clear()
        .env("HOME", home)
        .env("PI_CODING_AGENT_DIR", home.join("custom"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!home.join("custom/extensions/any-auto.ts").exists());
}
#[test]
fn shared_daemon_survives_partial_removal_and_stops_after_last_integration() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    ok(home, &["install", "--agents", "agy-cli,pi"]);
    ok(home, &["daemon", "start"]);
    // Always clean up the daemon if an assertion fails.
    struct Cleanup<'a>(&'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = run(self.0, &["daemon", "stop"]);
        }
    }
    let _cleanup = Cleanup(home);
    ok(home, &["uninstall", "--all", "--dry-run"]);
    assert!(home.join("daemon.sock").exists());
    ok(home, &["uninstall", "--agents", "pi"]);
    assert!(home.join("daemon.sock").exists());
    assert!(ok(home, &["uninstall", "--all"]).contains("Shared daemon stopped"));
    assert!(!home.join("daemon.sock").exists());
}
