use crate::{config, register};
use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use dialoguer::{Confirm, MultiSelect};
use std::{io::IsTerminal, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Agent {
    AgyCli,
    AgyDesktop,
    Pi,
}
impl Agent {
    fn name(self) -> &'static str {
        match self {
            Self::AgyCli => "agy-cli",
            Self::AgyDesktop => "agy-desktop",
            Self::Pi => "pi",
        }
    }
    fn detected(self) -> bool {
        match self {
            Self::AgyCli => {
                executable("agy")
                    || config::home()
                        .join(".gemini/antigravity-cli/bin/agy")
                        .is_file()
            }
            Self::AgyDesktop => {
                executable("antigravity")
                    || [
                        PathBuf::from("/Applications/Antigravity.app"),
                        config::home().join("Applications/Antigravity.app"),
                    ]
                    .iter()
                    .any(|p| p.is_dir())
            }
            Self::Pi => executable("pi"),
        }
    }
}
fn executable(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let path = dir.join(name);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                path.metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                path.is_file()
            }
        })
    })
}
#[derive(Args, Debug, Default)]
pub struct Options {
    /// Install all detected agents without prompting.
    #[arg(long, conflicts_with_all = ["agents", "cli_only", "desktop_only", "pi"])]
    auto: bool,
    /// Select agents explicitly; comma-separated or repeated.
    #[arg(long, value_enum, value_delimiter = ',', conflicts_with_all = ["cli_only", "desktop_only", "pi"])]
    agents: Vec<Agent>,
    #[arg(long, conflicts_with_all = ["desktop_only", "pi"])]
    cli_only: bool,
    #[arg(long, conflicts_with = "pi")]
    desktop_only: bool,
    #[arg(long)]
    pi: bool,
    /// Print selected integrations without writing files (auto-detect by default).
    #[arg(long)]
    dry_run: bool,
}
pub fn run(options: Options) -> Result<()> {
    run_with_interaction(options, std::env::args_os().count() == 2)
}
pub fn wizard() -> Result<()> {
    run_with_interaction(Options::default(), true)
}
fn run_with_interaction(options: Options, interactive: bool) -> Result<()> {
    let all = [Agent::AgyCli, Agent::AgyDesktop, Agent::Pi];
    let mut agents = options.agents;
    if options.cli_only {
        agents.push(Agent::AgyCli);
    }
    if options.desktop_only {
        agents.push(Agent::AgyDesktop);
    }
    if options.pi {
        agents.push(Agent::Pi);
    }
    if interactive {
        if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            bail!(
                "Bare install requires a terminal. Use install --auto or install --agents agy-cli,agy-desktop,pi."
            );
        }
        let labels: Vec<_> = all
            .iter()
            .map(|agent| {
                format!(
                    "{} ({})",
                    agent.name(),
                    if agent.detected() {
                        "detected"
                    } else {
                        "not detected; pre-install"
                    }
                )
            })
            .collect();
        let selected = MultiSelect::new()
            .with_prompt("Install integrations (Space selects, Enter continues)")
            .items(&labels)
            .defaults(&all.map(Agent::detected))
            .interact_opt()?;
        let Some(selected) = selected else {
            return Ok(());
        };
        agents = selected.into_iter().map(|i| all[i]).collect();
        if agents.is_empty() {
            println!("No agents selected; nothing changed.");
            return Ok(());
        }
    } else if agents.is_empty() {
        agents = all.into_iter().filter(|agent| agent.detected()).collect();
    }
    if agents.is_empty() {
        bail!("No agents detected. Use install --agents to select an integration explicitly.");
    }
    agents.dedup();
    for agent in &agents {
        println!(
            "Install {} integration{}",
            agent.name(),
            if agent.detected() {
                ""
            } else {
                " (agent not detected)"
            }
        );
    }
    println!(
        "Existing approver settings are preserved. Use `any-auto config --edit` to change them."
    );
    let cli = agents.contains(&Agent::AgyCli);
    let desktop = agents.contains(&Agent::AgyDesktop);
    if cli || desktop {
        register::preview(!desktop, !cli)?;
    }
    if agents.contains(&Agent::Pi) {
        let base = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| config::home().join(".pi/agent"));
        println!(
            "Would install {}",
            base.join("extensions/any-auto.ts").display()
        );
    }
    if options.dry_run {
        println!("Dry run: no files changed.");
        return Ok(());
    }
    if interactive
        && !Confirm::new()
            .with_prompt("Apply installation?")
            .default(true)
            .interact()?
    {
        return Ok(());
    }
    if cli || desktop {
        register::register(!desktop, !cli)?;
    }
    if agents.contains(&Agent::Pi) {
        register::register_pi()?;
    }
    Ok(())
}

pub fn doctor() -> Result<()> {
    for (agent, mode) in [
        (Agent::AgyCli, config::Mode::Cli),
        (Agent::AgyDesktop, config::Mode::Sidecar),
        (Agent::Pi, config::Mode::Pi),
    ] {
        let base = config::home().join(".gemini/config");
        let installed = match agent {
            Agent::AgyCli => std::fs::read(base.join("hooks.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .is_some_and(|v| v["any-auto"]["enabled"] == true),
            Agent::AgyDesktop => std::fs::read(base.join("config.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .is_some_and(|v| v["sidecars"]["any-auto/approver"]["enabled"] == true),
            Agent::Pi => std::env::var_os("PI_CODING_AGENT_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| config::home().join(".pi/agent"))
                .join("extensions/any-auto.ts")
                .is_file(),
        };
        println!("  integration_installed={installed}");
        let settings = config::reviewer_config_for(mode);
        println!("{}: detected={}", agent.name(), agent.detected());
        match settings {
            Ok(c) => {
                let available = match c.approver.provider {
                    config::Provider::Pi => executable("pi"),
                    config::Provider::Cli => Agent::AgyCli.detected(),
                    config::Provider::Agentapi => {
                        std::env::var_os("ANTIGRAVITY_LS_ADDRESS").is_some()
                    }
                    config::Provider::Openai => {
                        std::env::var(&c.approver.api_key_env).is_ok_and(|v| !v.is_empty())
                    }
                };
                println!(
                    "  approver={} locally_available={} (authentication/model capability not tested)",
                    c.approver.provider.as_str(),
                    available
                );
            }
            Err(e) => println!("  configuration error: {e:#}"),
        }
    }
    Ok(())
}
