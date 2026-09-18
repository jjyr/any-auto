use agy_auto_approve::{audit, config, daemon, pipeline, register, stats, upgrade};
use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::io::Read;

#[derive(Parser)]
#[command(version, about = "Antigravity approval hook and daemon management")]
struct Cli {
    /// Backend mode. Hooks auto-detect; stats includes all modes; other commands default to cli.
    #[arg(long, global = true, value_enum)]
    mode: Option<config::Mode>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Read a PreToolUse JSON payload on stdin; emit exactly one result on stdout.
    Hook,
    /// Show rolling 24-hour, 7-day and 30-day model approval usage (all modes by default).
    Stats,
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
        /// Print JSON summaries (an array normally, JSON Lines with --follow).
        #[arg(long)]
        json: bool,
        /// Print recent approvals, then follow newly completed approvals until Ctrl-C.
        #[arg(short, long)]
        follow: bool,
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
    /// Register this executable for CLI hooks and Desktop sidecars (both by default).
    Install {
        #[arg(long, conflicts_with = "desktop_only")]
        cli_only: bool,
        #[arg(long)]
        desktop_only: bool,
    },
}
#[derive(Subcommand)]
enum DaemonCommand {
    Start,
    Status,
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
    /// Show all recorded events for an exact approval ID, including agentapi input/output.
    Show { id: String },
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let selected_mode = cli.mode;
    config::set_mode(cli.mode.unwrap_or_else(|| {
        if matches!(cli.command, Commands::Hook) {
            config::Mode::for_hook()
        } else {
            config::Mode::Cli
        }
    }));
    match cli.command {
        Commands::Stats => stats::print(selected_mode)?,
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
        Commands::Install {
            cli_only,
            desktop_only,
        } => register::register(cli_only, desktop_only)?,
        Commands::Daemon { command } => match command {
            DaemonCommand::Run { idle_timeout } => daemon::run(idle_timeout).await?,
            DaemonCommand::Start => println!("{}", daemon::start().await?),
            DaemonCommand::Status => {
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
