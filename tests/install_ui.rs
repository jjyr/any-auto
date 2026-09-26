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
fn cli_install_enables_turbo_preserving_rules_and_reports_it_before_apply() {
    use serde_json::{Value, json};
    use std::fs;
    for existing in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".gemini/antigravity-cli/settings.json");
        let original = json!({"toolPermission":"request-review", "enableTerminalSandbox":true,
            "permissions":{"allow":["command(custom)"],"deny":["command(secret)"],"ask":["command(review)"]},"custom":42});
        if existing {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, original.to_string()).unwrap();
        }
        let dry = isolated_install(
            home.path(),
            &["install", "--agents", "agy-cli", "--dry-run"],
        );
        assert!(dry.status.success(), "{dry:?}");
        assert!(String::from_utf8_lossy(&dry.stdout).contains("Enable Turbo mode"));
        assert_eq!(path.exists(), existing);
        if existing {
            assert_eq!(fs::read_to_string(&path).unwrap(), original.to_string());
        }
        for _ in 0..2 {
            let output = isolated_install(home.path(), &["install", "--agents", "agy-cli"]);
            assert!(output.status.success(), "{output:?}");
            let actual: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(actual["toolPermission"], "always-proceed");
            assert_eq!(actual["enableTerminalSandbox"], false);
            if existing {
                assert_eq!(actual["permissions"], original["permissions"]);
                assert_eq!(actual["custom"], 42);
            } else {
                assert!(actual.get("permissions").is_none());
            }
        }
    }
}

#[test]
fn malformed_cli_settings_fail_before_installing_hook() {
    use std::fs;
    let home = tempfile::tempdir().unwrap();
    let settings = home.path().join(".gemini/antigravity-cli/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, "broken").unwrap();
    let out = isolated_install(home.path(), &["install", "--agents", "agy-cli"]);
    assert!(!out.status.success());
    assert!(!home.path().join(".gemini/config/hooks.json").exists());
    assert_eq!(fs::read_to_string(settings).unwrap(), "broken");
}

#[test]
fn doctor_checks_turbo_settings_without_modifying_them() {
    use std::fs;
    let cases = [
        (None, false, "cannot read settings"),
        (Some("broken"), false, "cannot read settings"),
        (Some("[]"), false, "expected a JSON object"),
        (Some("{}"), false, "request-review (default)"),
        (
            Some(r#"{"toolPermission":"always-proceed","enableTerminalSandbox":false}"#),
            true,
            "enableTerminalSandbox=false",
        ),
        (
            Some(r#"{"toolPermission":"always-proceed"}"#),
            true,
            "false (default)",
        ),
        (
            Some(r#"{"toolPermission":"always-proceed","enableTerminalSandbox":true}"#),
            false,
            "enableTerminalSandbox=true",
        ),
        (
            Some(r#"{"toolPermission":"request-review","enableTerminalSandbox":false}"#),
            false,
            "toolPermission=request-review",
        ),
        (
            Some(r#"{"toolPermission":"always-proceed","enableTerminalSandbox":"false"}"#),
            false,
            "invalid value",
        ),
        (Some(r#"{"toolPermission":null}"#), false, "invalid value"),
    ];
    for (contents, healthy, diagnostic) in cases {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".gemini/antigravity-cli/settings.json");
        if let Some(contents) = contents {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
        }
        let out = isolated_install(home.path(), &["doctor"]);
        assert!(out.status.success(), "{out:?}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let status = if healthy {
            "✓ CLI Turbo mode:"
        } else {
            "✗ CLI Turbo mode:"
        };
        assert!(stdout.contains(status), "{stdout}");
        assert!(stdout.contains(diagnostic), "{stdout}");
        assert_eq!(stdout.contains("Expected always-proceed with terminal sandbox off. Run: any-auto install --agents agy-cli"), !healthy, "{stdout}");
        assert_eq!(fs::read_to_string(&path).ok().as_deref(), contents);
        assert!(!home.path().join(".gemini/config/hooks.json").exists());
    }
}
