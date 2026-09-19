use agy_auto_approve::{audit, config, daemon, install, pipeline, stats, upgrade};
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
    /// Host routing/filter. Hooks auto-detect; logs/stats include all hosts unless filtered.
    #[arg(long, visible_alias = "host", global = true, value_enum)]
    mode: Option<config::Mode>,
    /// Isolate a daemon instance, or filter logs/stats to that instance.
    #[arg(long, global = true)]
    instance: Option<String>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Read a PreToolUse JSON payload on stdin; emit exactly one result on stdout.
    Hook,
    /// Record a Pi user confirmation (internal extension protocol).
    HumanResult,
    /// Show rolling model approval usage, grouped by host plus totals by default.
    Stats {
        #[arg(long, value_enum, default_value = "host")]
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
        /// Group recent records (default: host). Follow always uses chronological order.
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
    /// Stop the daemon, forget the cached reviewer conversation, and start again.
    Restart,
    Run {
        #[arg(long, default_value_t = 1800)]
        idle_timeout: u64,
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
            .or_else(|| std::env::var("AGY_AUTO_APPROVE_INSTANCE").ok())
            .unwrap_or_else(|| "default".into()),
    )?;
    config::set_mode(cli.mode.unwrap_or_else(|| {
        if matches!(cli.command, Commands::Hook) {
            config::Mode::for_hook()
        } else {
            config::Mode::Cli
        }
    }));
    match cli.command {
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
            {
                pipeline::Breaker::open(&config::state_dir(), session)?.record("allow")?;
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
                    host: selected_mode.map(|m| m.host().into()),
                    provider: provider.map(|p| p.as_str().into()),
                    instance: cli.instance.clone(),
                    group_by: if follow || no_group {
                        None
                    } else {
                        Some(group_by.unwrap_or(audit::Group::Host))
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
                config::show(json)?;
            }
        }
        Commands::Update { version } => upgrade::update(version.as_deref()).await?,
        Commands::Install { options } => install::run(options)?,
        Commands::Daemon { command } => match command {
            DaemonCommand::Run { idle_timeout } => daemon::run(idle_timeout).await?,
            DaemonCommand::Start => println!("{}", daemon::start().await?),
            DaemonCommand::Status { all: true } => println!("{}", daemon::status_all().await?),
            DaemonCommand::Status { all: false } => {
                match daemon::request(&config::socket_path(), &json!({"action":"status"}), 1).await
                {
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
