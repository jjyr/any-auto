use any_auto::{audit, config, daemon, install, pipeline, stats, ui, uninstall, upgrade};
use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::io::Read;

#[derive(Parser)]
#[command(
    version,
    about = "Antigravity and Pi approval hooks and reviewer daemons"
)]
struct Cli {
    /// Agent routing/filter. Hooks auto-detect; logs/stats include all agents unless filtered.
    #[arg(long = "agent", value_name = "AGENT", global = true, value_enum)]
    mode: Option<config::Mode>,
    /// Isolate a daemon instance, or filter logs/stats to that instance.
    #[arg(long, global = true)]
    instance: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}
#[derive(Subcommand)]
enum Commands {
    /// Read a PreToolUse JSON payload on stdin; emit exactly one result on stdout.
    Hook,
    /// Record completion of an agy tool step (internal PostToolUse protocol).
    PostTool,
    /// Record a Pi user confirmation (internal extension protocol).
    HumanResult,
    /// Check agent detection and local reviewer readiness without model requests.
    Doctor,
    /// Evaluate fixture suites without executing actions or touching approval state.
    ReviewerEval {
        #[command(flatten)]
        options: any_auto::reviewer_eval::Options,
    },
    /// Show rolling model approval usage, grouped by agent plus totals by default.
    Stats {
        #[arg(long, value_enum, default_value = "agent")]
        group_by: audit::Group,
        #[arg(long)]
        no_group: bool,
        #[arg(long, value_enum)]
        provider: Option<config::Provider>,
    },
    /// Show global reviewer settings, or edit the global TOML file.
    Config {
        #[arg(long, conflicts_with = "json")]
        edit: bool,
        #[arg(long)]
        json: bool,
    },
    /// List approval logs, or inspect the full input/output trace of one approval.
    #[command(args_conflicts_with_subcommands = true)]
    Logs {
        #[command(subcommand)]
        command: Option<LogsCommand>,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..))]
        limit: u32,
        #[arg(long, value_parser = ["allow", "deny", "ask", "force_ask"])]
        decision: Option<String>,
        #[arg(long)]
        tool: Option<String>,
        #[arg(long)]
        conversation: Option<String>,
        /// Print grouped JSON (an array with --no-group, JSON Lines with --follow).
        #[arg(long)]
        json: bool,
        /// Print recent approvals, then follow newly completed approvals until Ctrl-C.
        #[arg(short, long)]
        follow: bool,
        /// Group recent records (default: agent). Follow always uses chronological order.
        #[arg(long, value_enum, conflicts_with_all = ["follow", "no_group"])]
        group_by: Option<audit::Group>,
        /// Merge all matching records into one chronological list.
        #[arg(long)]
        no_group: bool,
        #[arg(long, value_enum)]
        provider: Option<config::Provider>,
    },
    /// Start, inspect, stop, or run the approval daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Upgrade the executable and refresh installed plugin configuration.
    Update {
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove selected integrations, preserving configuration and history.
    Uninstall {
        #[command(flatten)]
        options: uninstall::Options,
    },
    /// Install integrations: no arguments opens the terminal wizard.
    Install {
        #[command(flatten)]
        options: install::Options,
    },
}
#[derive(Subcommand)]
enum DaemonCommand {
    Start,
    Status {
        #[arg(long)]
        all: bool,
    },
    Stop,
    /// Restart the shared daemon, preserving persisted reviewer sessions.
    Restart,
    /// Clear one agent/instance reviewer cache, preserving circuit breakers.
    Reset,
    Run {
        #[arg(long, default_value_t = 1800)]
        idle_timeout: u64,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..))]
        session_idle_timeout: u64,
    },
}
#[derive(Subcommand)]
enum LogsCommand {
    /// Show all events for an approval ID, including backend input/output.
    Show { id: String },
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let selected_mode = cli.mode;
    config::set_instance(
        cli.instance
            .clone()
            .or_else(|| std::env::var("ANY_AUTO_INSTANCE").ok())
            .unwrap_or_else(|| "default".into()),
    )?;
    config::set_mode(cli.mode.unwrap_or_else(|| {
        if matches!(cli.command, Some(Commands::Hook | Commands::PostTool)) {
            config::Mode::for_hook()
        } else {
            config::Mode::Cli
        }
    }));
    let Some(command) = cli.command else {
        anyhow::ensure!(
            std::env::args_os().count() == 1,
            "Choose a subcommand; see --help"
        );
        return ui::run().await;
    };
    match command {
        Commands::Doctor => install::doctor()?,
        Commands::ReviewerEval { options } => any_auto::reviewer_eval::run(options).await?,
        Commands::HumanResult => {
            let mut bytes = Vec::new();
            std::io::stdin().take(65537).read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() <= 65536, "Human result too large");
            let value: Value = serde_json::from_slice(&bytes)?;
            let id = value["request_id"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow::anyhow!("Missing request id"))?;
            anyhow::ensure!(value["allowed"].is_boolean(), "Missing human decision");
            if value["allowed"] == true
                && let Some(session) = value["conversation_id"].as_str().filter(|s| !s.is_empty())
                && pipeline::Breaker::open(&config::state_dir(), session)?.human_result(id, true)?
            {
                audit::record(
                    id,
                    "circuit_breaker_reset",
                    json!({"reason":"human_approval", "conversation_id":session}),
                );
            }
            audit::record(id, "human_result", value.clone());
        }
        Commands::Stats {
            group_by,
            no_group,
            provider,
        } => stats::print(
            selected_mode,
            provider.map(|p| p.as_str()),
            cli.instance.as_deref(),
            (!no_group).then_some(group_by),
        )?,
        Commands::PostTool => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() <= 1024 * 1024, "PostToolUse input too large");
            let payload: Value = serde_json::from_slice(&bytes)?;
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                pipeline::post_tool(&payload),
            )
            .await??;
            println!("{{}}");
        }
        Commands::Hook => {
            let mut bytes = Vec::new();
            let parsed = std::io::stdin()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .ok()
                .filter(|_| bytes.len() <= 1024 * 1024)
                .and_then(|_| serde_json::from_slice::<Value>(&bytes).ok())
                .filter(|v| {
                    v.is_object()
                        && v["toolCall"].is_object()
                        && v["toolCall"]["name"].is_string()
                        && v["toolCall"]["args"].is_object()
                });
            let result = match parsed {
                Some(payload) => pipeline::evaluate(&payload).await,
                None => {
                    let id = audit::request_id();
                    audit::record(
                        &id,
                        "hook_input",
                        json!({"raw_input":String::from_utf8_lossy(&bytes),
                        "truncated":bytes.len() > 1024*1024}),
                    );
                    let output =
                        pipeline::result("ask", "Failed to parse hook stdin payload.", "", None);
                    audit::record(
                        &id,
                        "hook_result",
                        json!({"tool":"", "conversation_id":"default",
                        "stage":"invalid_input", "output":output, "duration_ms":0}),
                    );
                    output
                }
            };
            println!("{result}");
        }
        Commands::Logs {
            command,
            limit,
            decision,
            tool,
            conversation,
            json,
            follow,
            group_by,
            no_group,
            provider,
        } => match command {
            Some(LogsCommand::Show { id }) => {
                println!("{}", serde_json::to_string_pretty(&audit::show(&id)?)?)
            }
            None => {
                let filter = audit::Filter {
                    limit: limit as usize,
                    decision,
                    tool,
                    conversation,
                    agent: selected_mode.map(|m| m.agent().into()),
                    provider: provider.map(|p| p.as_str().into()),
                    instance: cli.instance.clone(),
                    group_by: if follow || no_group {
                        None
                    } else {
                        Some(group_by.unwrap_or(audit::Group::Agent))
                    },
                };
                if follow {
                    audit::follow(&filter, json).await?;
                } else {
                    audit::print_list(&filter, json)?;
                }
            }
        },
        Commands::Config { edit, json } => {
            if edit {
                config::edit()?;
            } else {
                config::overview(json, selected_mode)?;
            }
        }
        Commands::Update { version } => upgrade::update(version.as_deref()).await?,
        Commands::Install { options } => install::run(options)?,
        Commands::Uninstall { options } => uninstall::run(options).await?,
        Commands::Daemon { command } => match command {
            DaemonCommand::Run {
                idle_timeout,
                session_idle_timeout,
            } => daemon::run(idle_timeout, session_idle_timeout).await?,
            DaemonCommand::Reset => {
                anyhow::ensure!(selected_mode.is_some(), "reset requires --agent");
                println!("{}", daemon::reset().await?);
            }
            DaemonCommand::Start => println!("{}", daemon::start().await?),
            DaemonCommand::Status { all: true } => println!("{}", daemon::status_all().await?),
            DaemonCommand::Status { all: false } => {
                match daemon::status(selected_mode, cli.instance.as_deref()).await {
                    Ok(v) if v["status"] == "running" => println!("{v}"),
                    _ => {
                        println!(
                            "{}",
                            json!({"status":"stopped","mode":config::mode(),"socket":config::socket_path()})
                        );
                        std::process::exit(1);
                    }
                }
            }
            DaemonCommand::Stop => {
                daemon::stop().await?;
                println!("{}", json!({"status":"stopped", "mode":config::mode()}));
            }
            DaemonCommand::Restart => println!("{}", daemon::restart().await?),
        },
    }
    Ok(())
}
