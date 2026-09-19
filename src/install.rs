use crate::{config, register};
use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use dialoguer::{Confirm, MultiSelect};
use std::{io::IsTerminal, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Host {
    AgyCli,
    AgyDesktop,
    Pi,
}
impl Host {
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
#[derive(Args, Debug)]
pub struct Options {
    /// Install all detected hosts without prompting.
    #[arg(long, conflicts_with_all = ["hosts", "cli_only", "desktop_only", "pi"])]
    auto: bool,
    /// Select hosts explicitly; comma-separated or repeated.
    #[arg(long, value_enum, value_delimiter = ',', conflicts_with_all = ["cli_only", "desktop_only", "pi"])]
    hosts: Vec<Host>,
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
    let all = [Host::AgyCli, Host::AgyDesktop, Host::Pi];
    // Count process arguments, including global options: only bare install is interactive.
    let interactive = std::env::args_os().count() == 2;
    let mut hosts = options.hosts;
    if options.cli_only {
        hosts.push(Host::AgyCli);
    }
    if options.desktop_only {
        hosts.push(Host::AgyDesktop);
    }
    if options.pi {
        hosts.push(Host::Pi);
    }
    if interactive {
        if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            bail!(
                "Bare install requires a terminal. Use install --auto or install --hosts agy-cli,agy-desktop,pi."
            );
        }
        let labels: Vec<_> = all
            .iter()
            .map(|host| {
                format!(
                    "{} ({})",
                    host.name(),
                    if host.detected() {
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
            .defaults(&all.map(Host::detected))
            .interact_opt()?;
        let Some(selected) = selected else {
            return Ok(());
        };
        hosts = selected.into_iter().map(|i| all[i]).collect();
        if hosts.is_empty() {
            println!("No hosts selected; nothing changed.");
            return Ok(());
        }
    } else if hosts.is_empty() {
        hosts = all.into_iter().filter(|host| host.detected()).collect();
    }
    if hosts.is_empty() {
        bail!("No hosts detected. Use install --hosts to select an integration explicitly.");
    }
    hosts.dedup();
    for host in &hosts {
        println!(
            "Install {} integration{}",
            host.name(),
            if host.detected() {
                ""
            } else {
                " (host not detected)"
            }
        );
    }
    println!("Existing approver settings are preserved; configure them with config --edit.");
    let cli = hosts.contains(&Host::AgyCli);
    let desktop = hosts.contains(&Host::AgyDesktop);
    if cli || desktop {
        register::preview(!desktop, !cli)?;
    }
    if hosts.contains(&Host::Pi) {
        let base = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| config::home().join(".pi/agent"));
        println!(
            "Would install {}",
            base.join("extensions/agy-auto-approve.ts").display()
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
    if hosts.contains(&Host::Pi) {
        register::register_pi()?;
    }
    Ok(())
}
