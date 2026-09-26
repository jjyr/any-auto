use crate::config;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OtherPreToolUseHook {
    pub name: String,
    pub source: PathBuf,
}

pub fn display_path(path: &Path) -> String {
    let home = config::home();
    if let Ok(rest) = path.strip_prefix(&home) {
        format!("~/{}", rest.display())
    } else {
        path.display().to_string()
    }
}

pub fn is_cli_detected() -> bool {
    super::executable("agy")
        || config::home()
            .join(".gemini/antigravity-cli/bin/agy")
            .is_file()
}

pub fn is_desktop_detected() -> bool {
    super::executable("antigravity")
        || [
            PathBuf::from("/Applications/Antigravity.app"),
            config::home().join("Applications/Antigravity.app"),
        ]
        .iter()
        .any(|p| p.is_dir())
}

pub fn is_cli_installed() -> bool {
    let path = config::home().join(".gemini/config/hooks.json");
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| value.get("any-auto").is_some())
}

pub fn is_cli_enabled() -> bool {
    let path = config::home().join(".gemini/config/hooks.json");
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|v| v["any-auto"]["enabled"] == true)
}

pub fn is_desktop_installed() -> bool {
    let path = config::home().join(".gemini/config/config.json");
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| value["sidecars"].get("any-auto/approver").is_some())
}

pub fn is_desktop_enabled() -> bool {
    let path = config::home().join(".gemini/config/config.json");
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|v| v["sidecars"]["any-auto/approver"]["enabled"] == true)
}

fn check_hooks_file(
    path: &Path,
    results: &mut Vec<OtherPreToolUseHook>,
    seen: &mut HashSet<(String, PathBuf)>,
) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    let Some(obj) = val.as_object() else {
        return;
    };

    for (name, hook_val) in obj {
        if name == "any-auto" {
            continue;
        }
        let is_enabled = match hook_val.get("enabled") {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => s != "false",
            _ => true,
        };
        if !is_enabled {
            continue;
        }

        let Some(pre_tool_use) = hook_val.get("PreToolUse").and_then(|v| v.as_array()) else {
            continue;
        };

        let has_hooks = pre_tool_use.iter().any(|entry| {
            if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
                !hooks.is_empty()
            } else {
                entry.get("command").is_some()
            }
        });

        if !has_hooks {
            continue;
        }

        let is_any_auto_command = pre_tool_use.iter().any(|entry| {
            if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.contains("any-auto"))
                })
            } else {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("any-auto"))
            }
        });

        if !is_any_auto_command && seen.insert((name.clone(), path.to_path_buf())) {
            results.push(OtherPreToolUseHook {
                name: name.clone(),
                source: path.to_path_buf(),
            });
        }
    }
}

pub fn detect_other_pre_tool_use_hooks_in(
    global_dir: &Path,
    workspace_dir: Option<&Path>,
) -> Vec<OtherPreToolUseHook> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    let global_hooks = global_dir.join("hooks.json");
    if global_hooks.is_file() {
        check_hooks_file(&global_hooks, &mut results, &mut seen);
    }

    if let Some(start_dir) = workspace_dir {
        let home = config::home();
        let mut current = start_dir.to_path_buf();
        loop {
            if current == home {
                break;
            }
            for candidate in [
                ".agents/hooks.json",
                ".agent/hooks.json",
                "_agents/hooks.json",
                "_agent/hooks.json",
            ] {
                let path = current.join(candidate);
                if path.is_file() && path != global_hooks {
                    check_hooks_file(&path, &mut results, &mut seen);
                }
            }
            if current.join(".git").exists() {
                break;
            }
            if !current.pop() {
                break;
            }
        }
    }

    results
}

pub fn detect_other_pre_tool_use_hooks() -> Vec<OtherPreToolUseHook> {
    let global_dir = config::home().join(".gemini/config");
    let current_dir = std::env::current_dir().ok();
    detect_other_pre_tool_use_hooks_in(&global_dir, current_dir.as_deref())
}

pub fn print_pre_tool_use_hooks_check() {
    let other_hooks = detect_other_pre_tool_use_hooks();
    if other_hooks.is_empty() {
        println!("  ✓ PreToolUse hooks: clean");
    } else {
        println!("  ! PreToolUse hooks: other active hook(s) detected:");
        for hook in &other_hooks {
            println!("    - \"{}\" ({})", hook.name, display_path(&hook.source));
        }
        println!(
            "    Note: If other hooks return 'ask', Antigravity will still prompt for manual confirmation."
        );
        println!("    See: https://github.com/jjyr/any-auto/issues/19");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detects_no_hooks_when_file_absent() {
        let dir = tempdir().unwrap();
        let hooks = detect_other_pre_tool_use_hooks_in(dir.path(), None);
        assert!(hooks.is_empty());
    }

    #[test]
    fn ignores_any_auto_only() {
        let dir = tempdir().unwrap();
        let json = serde_json::json!({
            "any-auto": {
                "enabled": true,
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "any-auto hook" }] }]
            }
        });
        std::fs::write(dir.path().join("hooks.json"), json.to_string()).unwrap();
        let hooks = detect_other_pre_tool_use_hooks_in(dir.path(), None);
        assert!(hooks.is_empty());
    }

    #[test]
    fn ignores_disabled_hooks() {
        let dir = tempdir().unwrap();
        let json = serde_json::json!({
            "safety": {
                "enabled": false,
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "./safety.sh" }] }]
            },
            "security": {
                "enabled": "false",
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "./sec.sh" }] }]
            }
        });
        std::fs::write(dir.path().join("hooks.json"), json.to_string()).unwrap();
        let hooks = detect_other_pre_tool_use_hooks_in(dir.path(), None);
        assert!(hooks.is_empty());
    }

    #[test]
    fn ignores_hooks_without_pre_tool_use() {
        let dir = tempdir().unwrap();
        let json = serde_json::json!({
            "linter": {
                "PostToolUse": [{ "matcher": "*", "hooks": [{ "command": "./lint.sh" }] }],
                "PreInvocation": [{ "command": "./prompt.sh" }]
            },
            "empty": {
                "PreToolUse": []
            }
        });
        std::fs::write(dir.path().join("hooks.json"), json.to_string()).unwrap();
        let hooks = detect_other_pre_tool_use_hooks_in(dir.path(), None);
        assert!(hooks.is_empty());
    }

    #[test]
    fn detects_active_pre_tool_use_hooks() {
        let dir = tempdir().unwrap();
        let json = serde_json::json!({
            "any-auto": {
                "enabled": true,
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "any-auto hook" }] }]
            },
            "orca-status": {
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "./orca.sh" }] }]
            }
        });
        std::fs::write(dir.path().join("hooks.json"), json.to_string()).unwrap();
        let hooks = detect_other_pre_tool_use_hooks_in(dir.path(), None);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name, "orca-status");
        assert_eq!(hooks[0].source, dir.path().join("hooks.json"));
    }

    #[test]
    fn detects_workspace_hooks() {
        let global_dir = tempdir().unwrap();
        let workspace_dir = tempdir().unwrap();
        let ws_agents = workspace_dir.path().join(".agents");
        std::fs::create_dir_all(&ws_agents).unwrap();
        let json = serde_json::json!({
            "workspace-guard": {
                "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "./guard.sh" }] }]
            }
        });
        std::fs::write(ws_agents.join("hooks.json"), json.to_string()).unwrap();

        let hooks =
            detect_other_pre_tool_use_hooks_in(global_dir.path(), Some(workspace_dir.path()));
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name, "workspace-guard");
        assert_eq!(hooks[0].source, ws_agents.join("hooks.json"));
    }
}
