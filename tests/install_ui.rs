use std::process::{Command, Stdio};

#[test]
fn bare_install_without_terminal_fails_without_writes() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .arg("install")
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_RUNTIME_DIR")
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
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_RUNTIME_DIR")
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
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_RUNTIME_DIR")
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

fn isolated_install(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/nonexistent")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn pi_update_preserves_configuration_and_unselected_agents() {
    use std::fs;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".config/any-auto");
    fs::create_dir_all(&path).unwrap();
    let configuration = "[agents.pi.approver]\nprovider='jev'\nprobability_threshold=0.95\n";
    fs::write(path.join("config.toml"), configuration).unwrap();
    let gemini = home.path().join(".gemini/config");
    fs::create_dir_all(&gemini).unwrap();
    fs::write(gemini.join("hooks.json"), "{\"unrelated\":true}").unwrap();
    let extension = home.path().join(".pi/agent/extensions/any-auto.ts");
    fs::create_dir_all(extension.parent().unwrap()).unwrap();
    fs::write(&extension, "old extension").unwrap();
    let out = isolated_install(home.path(), &["install", "--pi"]);
    assert!(out.status.success(), "{out:?}");
    assert_ne!(fs::read_to_string(extension).unwrap(), "old extension");
    assert_eq!(
        fs::read_to_string(path.join("config.toml")).unwrap(),
        configuration
    );
    assert_eq!(
        fs::read_to_string(gemini.join("hooks.json")).unwrap(),
        "{\"unrelated\":true}"
    );
}

#[test]
fn invalid_reviewer_config_never_writes() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".config/any-auto");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("config.toml"),
        "[approver]\nprovider='not-a-provider'\n",
    )
    .unwrap();
    let out = isolated_install(home.path(), &["install", "--agents", "agy-cli,pi"]);
    assert!(!out.status.success());
    assert!(!home.path().join(".gemini").exists());
    assert!(!home.path().join(".pi").exists());
}

#[test]
fn install_warns_on_other_pre_tool_use_hooks() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".gemini/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("hooks.json"),
        r#"{
            "orca-status": {
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "./orca.sh" }] }]
            }
        }"#,
    )
    .unwrap();

    let out = isolated_install(
        home.path(),
        &["install", "--agents", "agy-cli", "--dry-run"],
    );
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("! PreToolUse hooks: other active hook(s) detected:"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("orca-status"), "stdout was: {stdout}");
    assert!(
        stdout.contains("https://github.com/jjyr/any-auto/issues/19"),
        "stdout was: {stdout}"
    );
}

#[test]
fn doctor_warns_on_other_pre_tool_use_hooks() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".gemini/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("hooks.json"),
        r#"{
            "orca-status": {
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "./orca.sh" }] }]
            }
        }"#,
    )
    .unwrap();

    let out = isolated_install(home.path(), &["doctor"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("! PreToolUse hooks: other active hook(s) detected:"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("orca-status"), "stdout was: {stdout}");
    assert!(
        stdout.contains("https://github.com/jjyr/any-auto/issues/19"),
        "stdout was: {stdout}"
    );
}
