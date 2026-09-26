use crate::{
    config, daemon,
    install::{Agent, pi_extension},
};
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use dialoguer::{Confirm, MultiSelect};
use serde_json::Value;
use std::{
    fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

#[derive(Args, Debug, Default)]
pub struct Options {
    /// Explicit integrations to remove without prompting.
    #[arg(long, value_enum, value_delimiter = ',', conflicts_with = "all")]
    agents: Vec<Agent>,
    /// Remove all installed integrations without prompting.
    #[arg(long)]
    all: bool,
    /// Preview removal without changing files or stopping the daemon.
    #[arg(long)]
    dry_run: bool,
}
struct Change {
    path: PathBuf,
    before: Vec<u8>,
    after: Option<Vec<u8>>,
}
const AGENTS: [Agent; 3] = [Agent::AgyCli, Agent::AgyDesktop, Agent::Pi];

fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Cannot read {}", path.display())),
    }
}
fn plan(agent: Agent) -> Result<Vec<Change>> {
    let base = config::home().join(".gemini/config");
    let mut changes = Vec::new();
    if agent == Agent::Pi {
        let path = pi_extension();
        if let Some(before) = read(&path)? {
            changes.push(Change {
                path,
                before,
                after: None,
            });
        }
        return Ok(changes);
    }
    let path = base.join(if agent == Agent::AgyCli {
        "hooks.json"
    } else {
        "config.json"
    });
    if let Some(before) = read(&path)? {
        let mut value: Value = serde_json::from_slice(&before)
            .with_context(|| format!("Invalid JSON in {}", path.display()))?;
        let root = value
            .as_object_mut()
            .context("Expected configuration object")?;
        let removed = if agent == Agent::AgyCli {
            root.remove("any-auto").is_some()
        } else if let Some(sidecars) = root.get_mut("sidecars") {
            sidecars
                .as_object_mut()
                .context("Expected sidecars object")?
                .remove("any-auto/approver")
                .is_some()
        } else {
            false
        };
        if removed {
            // Restore native review before removing the CLI approval hook.
            if agent == Agent::AgyCli {
                let settings = config::home().join(".gemini/antigravity-cli/settings.json");
                if let Some(original) = read(&settings)? {
                    let mut value: Value = serde_json::from_slice(&original)
                        .with_context(|| format!("Invalid JSON in {}", settings.display()))?;
                    ensure!(value.is_object(), "Expected CLI settings object");
                    if value["toolPermission"] == "always-proceed" {
                        value["toolPermission"] = Value::String("request-review".into());
                        changes.push(Change {
                            path: settings,
                            before: original,
                            after: Some(serde_json::to_vec_pretty(&value)?),
                        });
                    }
                }
            }
            changes.push(Change {
                path,
                before,
                after: Some(serde_json::to_vec_pretty(&value)?),
            });
        }
    }
    if agent == Agent::AgyDesktop {
        for relative in [
            "sidecars/approver/sidecar.json",
            "sidecars/any-auto/approver/sidecar.json",
        ] {
            let path = base.join(relative);
            if let Some(before) = read(&path)? {
                let value: Value = serde_json::from_slice(&before)
                    .with_context(|| format!("Invalid JSON in {}", path.display()))?;
                let owned = value["name"] == "approver"
                    && value["args"] == serde_json::json!(["daemon", "start"])
                    && value["command"]
                        .as_str()
                        .is_some_and(|s| Path::new(s).file_name().is_some_and(|n| n == "any-auto"));
                if owned {
                    changes.push(Change {
                        path,
                        before,
                        after: None,
                    });
                } else {
                    bail!(
                        "Cannot verify any-auto ownership of {}; leaving integrations unchanged",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(changes)
}

pub async fn run(options: Options) -> Result<()> {
    // Inspect all integrations before writing, including orphaned desktop manifests.
    let plans: Vec<_> = AGENTS
        .into_iter()
        .map(|agent| Ok((agent, plan(agent)?)))
        .collect::<Result<_>>()?;
    let interactive = options.agents.is_empty() && !options.all && !options.dry_run;
    let selected = if interactive {
        ensure!(
            std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
            "Uninstall requires a terminal. Use --agents or --all for noninteractive removal."
        );
        let installed: Vec<_> = plans
            .iter()
            .filter(|(_, changes)| !changes.is_empty())
            .map(|(agent, _)| *agent)
            .collect();
        if installed.is_empty() {
            println!("No integrations installed. Nothing to uninstall.");
            return Ok(());
        }
        println!(
            "Select integrations to uninstall. None are selected by default; empty selection makes no changes."
        );
        let labels: Vec<_> = installed
            .iter()
            .map(|agent| format!("{} (installed)", agent.name()))
            .collect();
        let Some(indices) = MultiSelect::new()
            .with_prompt("Up/Down move, Space toggles, Enter continues, Esc cancels")
            .items(&labels)
            .interact_opt()?
        else {
            println!("Cancelled. No changes made.");
            return Ok(());
        };
        indices
            .into_iter()
            .map(|i| installed[i])
            .collect::<Vec<_>>()
    } else if options.agents.is_empty() {
        AGENTS.to_vec()
    } else {
        options.agents
    };
    if selected.is_empty() {
        println!("Nothing selected. No changes made.");
        return Ok(());
    }
    println!("\nUninstall summary");
    let mut changes = Vec::new();
    let mut removed = Vec::new();
    for (agent, planned) in &plans {
        if !selected.contains(agent) {
            continue;
        }
        if planned.is_empty() {
            println!("{}: already uninstalled", agent.name());
        } else {
            removed.push(*agent);
            println!("Remove {} integration:", agent.name());
            for change in planned {
                println!(
                    "  {} {}",
                    if change.after.is_some() {
                        "Update"
                    } else {
                        "Delete"
                    },
                    change.path.display()
                );
                changes.push(change);
            }
        }
    }
    println!(
        "Keep reviewer configuration, credentials, logs, history, sessions, and the any-auto executable."
    );
    println!("Unselected integrations and existing command permissions are preserved.");
    if selected.contains(&Agent::AgyCli) {
        println!(
            "Before removing the CLI hook, change Turbo (always-proceed) to request-review. Terminal sandbox settings are preserved."
        );
    }
    let none_remaining = plans
        .iter()
        .all(|(agent, planned)| selected.contains(agent) || planned.is_empty());
    if none_remaining {
        println!("No integrations will remain; stop the shared daemon if running.");
    }
    if options.dry_run {
        println!("Dry run: no files changed; daemon left running.");
        return Ok(());
    }
    if interactive
        && !Confirm::new()
            .with_prompt("Continue?")
            .default(false)
            .interact_opt()?
            .unwrap_or(false)
    {
        println!("Cancelled. No changes made.");
        return Ok(());
    }
    // Reject concurrent changes before performing any removal.
    for change in &changes {
        ensure!(
            read(&change.path)?.as_ref() == Some(&change.before),
            "{} changed since preview; retry uninstall",
            change.path.display()
        );
    }
    for change in changes {
        if let Some(after) = &change.after {
            let mut file = tempfile::NamedTempFile::new_in(change.path.parent().unwrap())?;
            file.as_file()
                .set_permissions(fs::metadata(&change.path)?.permissions())?;
            file.write_all(after)?;
            file.persist(&change.path)?;
        } else {
            fs::remove_file(&change.path)?;
        }
    }
    for agent in removed {
        println!(
            "{}: {}",
            agent.name(),
            if agent == Agent::Pi {
                "run /reload or restart Pi to unload the extension."
            } else {
                "restart the agent to unload the integration."
            }
        );
    }
    if none_remaining && config::socket_path().exists() {
        daemon::stop()
            .await
            .context("Integrations removed, but daemon could not be stopped")?;
        println!("Shared daemon stopped.");
    }
    println!("Uninstall complete.");
    Ok(())
}
