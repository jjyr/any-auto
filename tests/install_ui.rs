use std::process::{Command, Stdio};

#[test]
fn bare_install_without_terminal_fails_without_writes() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .arg("install")
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--auto"));
    assert!(!home.path().join(".gemini").exists());
}

#[test]
fn explicit_dry_run_never_writes_or_prompts() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .args(["install", "--agents", "agy-cli,agy-desktop,pi", "--dry-run"])
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    for agent in ["agy-cli", "agy-desktop", "pi"] {
        assert!(stdout.contains(&format!("Install {agent} integration")));
    }
    assert!(!home.path().join(".gemini").exists());
    assert!(!home.path().join(".pi").exists());
}

#[test]
fn invalid_desktop_configuration_does_not_partially_install_cli() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".gemini/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.json"), r#"{"sidecars":false}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .args(["install", "--agents", "agy-cli,agy-desktop"])
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!config.join("hooks.json").exists());
    assert_eq!(
        std::fs::read_to_string(config.join("config.json")).unwrap(),
        r#"{"sidecars":false}"#
    );
}
