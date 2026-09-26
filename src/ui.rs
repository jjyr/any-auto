use crate::{audit, config, install, stats};
use anyhow::Result;
use dialoguer::{Confirm, Input, Password, Select};
use std::io::IsTerminal;

pub fn stage_settings(agents: &[config::Mode]) -> Result<Option<String>> {
    if !Confirm::new()
        .with_prompt("Customize approver settings?")
        .default(false)
        .interact()?
    {
        return Ok(None);
    }
    let text = match std::fs::read_to_string(config::config_path()) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid configuration TOML"))?;
    for agent in agents {
        println!(
            "Configure {} (environment overrides still take precedence)",
            agent.agent()
        );
        let providers = [
            "Keep existing/default",
            "pi",
            "cli",
            "openai",
            "agentapi",
            "jev",
        ];
        let Some(choice) = Select::new()
            .with_prompt("Approver backend")
            .items(&providers)
            .default(0)
            .interact_opt()?
        else {
            return Ok(None);
        };
        if choice == 0 {
            continue;
        }
        let provider = providers[choice];
        let model: String = Input::new()
            .with_prompt("Model (blank: backend default; OpenAI requires a model)")
            .allow_empty(true)
            .interact_text()?;
        let levels: &[&str] = match provider {
            "pi" => &[
                "default", "off", "minimal", "low", "medium", "high", "xhigh", "max",
            ],
            "cli" => &["default", "low", "medium", "high"],
            "openai" => &[
                "default", "none", "minimal", "low", "medium", "high", "xhigh", "max",
            ],
            _ => &["default"],
        };
        let effort = if levels.len() > 1 {
            println!("Model-specific effort capability will be validated during review.");
            let Some(i) = Select::new()
                .with_prompt("Effort")
                .items(levels)
                .default(0)
                .interact_opt()?
            else {
                return Ok(None);
            };
            levels[i]
        } else {
            "default"
        };
        let mut backend_settings = toml_edit::Table::new();
        // Explicit empty overrides prevent inheriting common model/effort.
        backend_settings["model"] = toml_edit::value(model);
        let effort = toml_edit::value(if effort == "default" { "" } else { effort });
        if provider == "openai" {
            let mut common = toml_edit::Table::new();
            common["effort"] = effort;
            backend_settings["common"] = toml_edit::Item::Table(common);
        } else {
            backend_settings["effort"] = effort;
        }
        if matches!(provider, "openai" | "jev") {
            let url: String = Input::new()
                .with_prompt("API base URL (including /v1)")
                .default(
                    if provider == "jev" {
                        "https://api.typesafe.ai/v1"
                    } else {
                        "https://api.openai.com/v1"
                    }
                    .into(),
                )
                .interact_text()?;
            let key = Password::new().with_prompt("API key").interact()?;
            backend_settings["base_url"] = toml_edit::value(url);
            backend_settings["api_key"] = toml_edit::value(key);
        }
        let mut settings = if provider == "openai" {
            let mut settings = toml_edit::Table::new();
            settings["openai"] = toml_edit::Item::Table(backend_settings);
            println!(
                "Optional sampling and thinking budgets: edit openai.common / openai.llama_cpp with any-auto config --edit."
            );
            settings
        } else {
            backend_settings
        };
        settings["provider"] = toml_edit::value(provider);
        if provider == "jev" {
            let threshold: f64 = Input::new()
                .with_prompt("Probability threshold (0 to 1)")
                .default(0.9)
                .validate_with(|v: &f64| {
                    if v.is_finite() && (0.0..=1.0).contains(v) {
                        Ok(())
                    } else {
                        Err("Expected a finite number from 0 to 1")
                    }
                })
                .interact_text()?;
            settings["probability_threshold"] = toml_edit::value(threshold);
            println!(
                "Optional custom risk/authorization/policy instructions can be edited with any-auto config --edit."
            );
        }
        if doc.get("agents").is_none() {
            doc["agents"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if doc["agents"].get(agent.agent()).is_none() {
            doc["agents"][agent.agent()] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["agents"][agent.agent()]["approver"] = toml_edit::Item::Table(settings);
    }
    let proposed = doc.to_string();
    if proposed == text {
        return Ok(None);
    }
    let text = proposed;
    config::validate_text(&text)?;
    println!(
        "Proposed configuration: {}\n{}",
        config::config_path().display(),
        redacted_preview(&text)?
    );
    Ok(Some(text))
}
pub async fn run() -> Result<()> {
    anyhow::ensure!(
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        "TUI requires a terminal. Use --help or a subcommand."
    );
    loop {
        let Some(choice) = Select::new()
            .with_prompt("any-auto")
            .items(&[
                "Agent readiness",
                "Install integrations",
                "Configure approvers",
                "Logs",
                "Statistics",
                "Uninstall integrations",
                "Exit",
            ])
            .default(0)
            .interact_opt()?
        else {
            return Ok(());
        };
        let result = match choice {
            0 => install::doctor(),
            1 => install::wizard(),
            2 => configure(),
            3 => audit::print_list(
                &audit::Filter {
                    limit: 20,
                    group_by: Some(audit::Group::Agent),
                    ..Default::default()
                },
                false,
            ),
            4 => stats::print(None, None, None, Some(audit::Group::Agent)),
            5 => crate::uninstall::run(Default::default()).await,
            _ => return Ok(()),
        };
        if let Err(e) = result {
            eprintln!("{e:#}");
        }
    }
}
fn configure() -> Result<()> {
    if let Some(text) =
        stage_settings(&[config::Mode::Cli, config::Mode::Sidecar, config::Mode::Pi])?
        && Confirm::new()
            .with_prompt("Save configuration?")
            .default(false)
            .interact()?
    {
        config::save_text(&text)?;
    }
    Ok(())
}

fn redacted_preview(text: &str) -> Result<String> {
    fn redact(value: &mut toml::Value) {
        if let Some(table) = value.as_table_mut() {
            for (key, value) in table {
                if key == "api_key" {
                    *value = toml::Value::String("[REDACTED]".into());
                } else {
                    redact(value);
                }
            }
        }
    }
    let mut value: toml::Value =
        toml::from_str(text).map_err(|_| anyhow::anyhow!("Invalid configuration TOML"))?;
    redact(&mut value);
    Ok(toml::to_string_pretty(&value)?)
}
