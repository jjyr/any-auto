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
fn summary_distinguishes_defaults_overrides_and_updates_and_preserves_unselected_agents() {
    use std::fs;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".config/any-auto");
    fs::create_dir_all(&path).unwrap();
    let configuration = "[agents.pi.approver]\nprovider='jev'\nprobability_threshold=0.95\n";
    fs::write(path.join("config.toml"), configuration).unwrap();
    let gemini = home.path().join(".gemini/config");
    fs::create_dir_all(&gemini).unwrap();
    fs::write(gemini.join("hooks.json"), "{\"unrelated\":true}").unwrap();
    let out = isolated_install(
        home.path(),
        &["install", "--agents", "agy-cli,pi,agy-cli", "--dry-run"],
    );
    assert!(out.status.success(), "{:?}", out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Installation summary"));
    assert_eq!(text.matches("Install agy-cli integration").count(), 1);
    assert!(text.contains("Reviewer: cli (default)"));
    assert!(text.contains("Reviewer: jev (existing configuration: [agents.pi.approver])"));
    assert!(text.contains("Probability threshold: 0.95"));
    assert!(text.contains("API credentials are not requested or changed"));
    assert!(!home.path().join(".pi").exists());
    let out = isolated_install(home.path(), &["install", "--pi"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Pi: run /reload"));
    assert!(!text.contains("Antigravity Desktop: restart"));
    assert!(!text.contains("Antigravity CLI: restart"));
    assert_eq!(
        fs::read_to_string(path.join("config.toml")).unwrap(),
        configuration
    );
    assert_eq!(
        fs::read_to_string(gemini.join("hooks.json")).unwrap(),
        "{\"unrelated\":true}"
    );
    let out = isolated_install(home.path(), &["install", "--pi", "--dry-run"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("Update pi integration"));
}

#[test]
fn summary_shows_environment_override_and_invalid_config_never_writes() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_any-auto"))
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/nonexistent")
        .env("ANY_AUTO_PROVIDER", "jev")
        .args(["install", "--pi", "--dry-run"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("Reviewer: jev (environment override: ANY_AUTO_PROVIDER)")
    );
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
