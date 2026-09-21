use std::{env, fs, path::PathBuf};

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    #[value(name = "agy-cli")]
    Cli,
    #[value(name = "agy-desktop")]
    Sidecar,
    Pi,
}
impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Sidecar => "sidecar",
            Self::Pi => "pi",
        }
    }
    pub fn agent(self) -> &'static str {
        match self {
            Self::Cli => "agy-cli",
            Self::Sidecar => "agy-desktop",
            Self::Pi => "pi",
        }
    }
    pub fn for_hook() -> Self {
        if env::var_os("ANTIGRAVITY_LS_ADDRESS").is_some_and(|v| !v.is_empty()) {
            Self::Sidecar
        } else {
            Self::Cli
        }
    }
}
// Selected once at the process boundary; never mutate the daemon environment.
static MODE: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
pub fn set_mode(mode: Mode) {
    MODE.set(mode).expect("mode already selected");
}
pub fn mode() -> Mode {
    crate::context::current()
        .map(|c| c.mode)
        .unwrap_or_else(|| *MODE.get_or_init(Mode::default))
}

static INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
pub fn set_instance(value: String) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
        "Instance must contain 1-64 letters, digits, hyphens or underscores"
    );
    INSTANCE
        .set(value)
        .map_err(|_| anyhow::anyhow!("instance already selected"))
}
pub fn instance() -> String {
    crate::context::current()
        .map(|c| c.instance.clone())
        .unwrap_or_else(|| INSTANCE.get().cloned().unwrap_or_else(|| "default".into()))
}
fn instance_suffix() -> String {
    use sha2::{Digest, Sha256};
    if instance() == "default" {
        String::new()
    } else {
        format!("-{:x}", Sha256::digest(instance().as_bytes()))[..13].into()
    }
}
pub fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME must be set")
}
/// Preserve the agent-injected PATH and append the CLI shim directory as a fallback.
/// Only applied to reviewer child processes, never to the daemon's global environment.
pub fn backend_path() -> anyhow::Result<std::ffi::OsString> {
    let mut paths: Vec<PathBuf> = crate::context::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    let fallback = home().join(".gemini/antigravity-cli/bin");
    if !paths.contains(&fallback) {
        paths.push(fallback);
    }
    Ok(env::join_paths(paths)?)
}
pub fn log_dir() -> PathBuf {
    env::var_os("ANY_AUTO_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir().join("logs"))
}
pub fn state_dir() -> PathBuf {
    state_dir_for(mode())
}
pub fn state_dir_for(mode: Mode) -> PathBuf {
    if let Some(base) = env::var_os("ANY_AUTO_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(base).join(format!("{}{}", mode.as_str(), instance_suffix()));
    }
    if env::var_os("ANY_AUTO_LOG_DIR").is_some_and(|v| !v.is_empty()) {
        return log_dir()
            .join("state")
            .join(format!("{}{}", mode.as_str(), instance_suffix()));
    }
    data_dir()
        .join("agents")
        .join(mode.agent())
        .join(instance())
}
pub fn socket_path() -> PathBuf {
    socket_path_for(mode())
}
pub fn socket_path_for(_mode: Mode) -> PathBuf {
    env::var_os("ANY_AUTO_SOCKET")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_dir().join("approver.sock"))
}

fn xdg(name: &str, fallback: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
        .join("any-auto")
}
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}
pub fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("any-auto"))
        .unwrap_or_else(data_dir)
        .join("runtime")
}
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_model: Option<String>,
    #[serde(default)]
    approver: ApproverSettings,
    #[serde(default)]
    agents: Agents,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Pi,
    Cli,
    Openai,
    Jev,
    Agentapi,
}
impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Cli => "cli",
            Self::Openai => "openai",
            Self::Jev => "jev",
            Self::Agentapi => "agentapi",
        }
    }
}
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproverSettings {
    provider: Option<Provider>,
    model: Option<String>,
    effort: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    context_budget_bytes: Option<usize>,
    probability_threshold: Option<f64>,
    diagnostic_snapshot: Option<bool>,
    instructions: Option<JevInstructions>,
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentSettings {
    #[serde(default)]
    approver: ApproverSettings,
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Agents {
    #[serde(rename = "agy-cli", default)]
    cli: AgentSettings,
    #[serde(rename = "agy-desktop", default)]
    sidecar: AgentSettings,
    #[serde(default)]
    pi: AgentSettings,
}
/// Per-question instruction replacements. Answer options and local gates stay fixed.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JevInstructions {
    pub risk: Option<String>,
    pub authorization: Option<String>,
    pub policy: Option<String>,
}
impl JevInstructions {
    fn merge(&mut self, other: Self) {
        if other.risk.is_some() {
            self.risk = other.risk;
        }
        if other.authorization.is_some() {
            self.authorization = other.authorization;
        }
        if other.policy.is_some() {
            self.policy = other.policy;
        }
    }
}
#[derive(Clone, serde::Serialize)]
pub struct ApproverConfig {
    pub provider: Provider,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub base_url: String,
    #[serde(serialize_with = "serialize_api_key")]
    pub api_key: String,
    pub context_budget_bytes: usize,
    pub probability_threshold: f64,
    pub diagnostic_snapshot: bool,
    pub instructions: JevInstructions,
}
#[derive(Clone, serde::Serialize)]
pub struct ReviewerConfig {
    pub approver: ApproverConfig,
    pub approver_sources: std::collections::BTreeMap<String, String>,
    pub model: Option<String>,
    pub prompt: String,
    pub cli_model: Option<String>,
    pub cli_model_source: String,
    pub model_source: String,
    pub prompt_source: String,
}

fn read_optional(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    use anyhow::Context;
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Cannot read {}", path.display())),
    }
}

fn file_config() -> anyhow::Result<FileConfig> {
    use anyhow::Context;
    read_optional(&config_path())?
        .map(|text| {
            toml::from_str(&text)
                .map_err(|_| anyhow::anyhow!("Invalid TOML or unsupported configuration field"))
                .with_context(|| format!("Invalid configuration: {}", config_path().display()))
        })
        .unwrap_or_else(|| Ok(FileConfig::default()))
}

fn resolve(
    name: &str,
    value: Option<String>,
    default: &str,
    environment: bool,
) -> anyhow::Result<(String, String)> {
    let variable = format!("ANY_AUTO_{}", name.to_uppercase());
    if let Ok(value) = crate::context::var(&variable)
        && environment
        && !value.is_empty()
    {
        return Ok((value, variable));
    }
    if let Some(value) = value {
        return Ok((value, config_path().display().to_string()));
    }
    Ok((default.into(), "default".into()))
}

pub fn reviewer_config() -> anyhow::Result<ReviewerConfig> {
    reviewer_config_for(mode())
}
pub fn reviewer_config_for(mode: Mode) -> anyhow::Result<ReviewerConfig> {
    resolve_config(mode, file_config()?, true)
}
fn resolve_config(
    mode: Mode,
    file: FileConfig,
    environment: bool,
) -> anyhow::Result<ReviewerConfig> {
    let (model, model_source) = resolve("model", file.model, "", environment)?;
    let model = model.trim();
    anyhow::ensure!(
        matches!(model, "" | "flash_lite" | "flash" | "pro"),
        "Invalid model {model:?} from {model_source}; expected flash_lite, flash, pro, or an empty string for the agent default"
    );
    let (prompt, prompt_source) = resolve(
        "prompt",
        file.prompt,
        include_str!("prompt.txt"),
        environment,
    )?;
    let (cli_model, cli_model_source) = resolve("cli_model", file.cli_model, "", environment)?;
    let defaults = match mode {
        Mode::Cli => Provider::Cli,
        Mode::Sidecar => Provider::Cli,
        Mode::Pi => Provider::Pi,
    };
    let agent = match mode {
        Mode::Cli => file.agents.cli.approver,
        Mode::Sidecar => file.agents.sidecar.approver,
        Mode::Pi => file.agents.pi.approver,
    };
    let mut sources = std::collections::BTreeMap::new();
    for key in [
        "provider",
        "model",
        "effort",
        "base_url",
        "api_key",
        "context_budget_bytes",
        "probability_threshold",
        "diagnostic_snapshot",
        "instructions",
        "instructions.risk",
        "instructions.authorization",
        "instructions.policy",
    ] {
        sources.insert(key.to_owned(), "default".to_owned());
    }
    let common = serde_json::to_value(&file.approver)?;
    for (key, value) in common.as_object().unwrap() {
        if !value.is_null() {
            sources.insert(key.clone(), "[approver]".into());
            if key == "instructions" {
                for (name, text) in value.as_object().unwrap() {
                    if !text.is_null() {
                        sources.insert(
                            format!("instructions.{name}"),
                            "[approver.instructions]".into(),
                        );
                    }
                }
            }
        }
    }
    let agent_values = serde_json::to_value(&agent)?;
    let mut settings = file.approver;
    if agent.provider.is_some() && agent.provider != settings.provider.or(Some(defaults)) {
        settings = ApproverSettings {
            context_budget_bytes: settings.context_budget_bytes,
            ..Default::default()
        };
        sources
            .iter_mut()
            .filter(|(key, _)| key.as_str() != "context_budget_bytes")
            .for_each(|(_, s)| *s = "default".into());
    }
    for (key, value) in agent_values.as_object().unwrap() {
        if !value.is_null() {
            sources.insert(key.clone(), format!("[agents.{}.approver]", mode.agent()));
            if key == "instructions" {
                for (name, text) in value.as_object().unwrap() {
                    if !text.is_null() {
                        sources.insert(
                            format!("instructions.{name}"),
                            format!("[agents.{}.approver.instructions]", mode.agent()),
                        );
                    }
                }
            }
        }
    }
    if agent.provider.is_some() {
        settings.provider = agent.provider;
    }
    if agent.model.is_some() {
        settings.model = agent.model;
    }
    if agent.effort.is_some() {
        settings.effort = agent.effort;
    }
    if agent.base_url.is_some() {
        settings.base_url = agent.base_url;
    }
    if agent.api_key.is_some() {
        settings.api_key = agent.api_key;
    }
    if agent.context_budget_bytes.is_some() {
        settings.context_budget_bytes = agent.context_budget_bytes;
    }
    if agent.probability_threshold.is_some() {
        settings.probability_threshold = agent.probability_threshold;
    }
    if agent.diagnostic_snapshot.is_some() {
        settings.diagnostic_snapshot = agent.diagnostic_snapshot;
    }
    if let Some(instructions) = agent.instructions {
        settings
            .instructions
            .get_or_insert_with(Default::default)
            .merge(instructions);
    }
    if let Ok(value) = crate::context::var("ANY_AUTO_PROVIDER")
        && environment
        && !value.is_empty()
    {
        let provider: Provider = serde_json::from_value(serde_json::json!(value))?;
        if provider != settings.provider.unwrap_or(defaults) {
            settings.model = None;
            settings.effort = None;
            settings.base_url = None;
            settings.api_key = None;
            settings.probability_threshold = None;
            settings.diagnostic_snapshot = None;
            settings.instructions = None;
            sources
                .iter_mut()
                .filter(|(key, _)| key.as_str() != "context_budget_bytes")
                .for_each(|(_, s)| *s = "default".into());
        }
        settings.provider = Some(provider);
        sources.insert("provider".into(), "ANY_AUTO_PROVIDER".into());
    }
    let provider = settings.provider.unwrap_or(defaults);
    for (field, variable) in [
        ("base_url", "ANY_AUTO_BASE_URL"),
        ("api_key", "ANY_AUTO_API_KEY"),
    ] {
        if let Ok(value) = crate::context::var(variable)
            && environment
            && (field == "api_key" || !value.is_empty())
        {
            if field == "base_url" {
                settings.base_url = Some(value);
            } else {
                settings.api_key = Some(value);
            }
            sources.insert(field.into(), variable.into());
        }
    }
    anyhow::ensure!(
        provider == Provider::Jev
            || (settings.probability_threshold.is_none()
                && settings.instructions.is_none()
                && settings.diagnostic_snapshot.is_none()),
        "probability_threshold, diagnostic_snapshot and instructions are only supported by the Jev backend"
    );
    let probability_threshold = settings.probability_threshold.unwrap_or(0.9);
    anyhow::ensure!(
        probability_threshold.is_finite() && (0.0..=1.0).contains(&probability_threshold),
        "Jev probability_threshold must be a finite number between 0 and 1"
    );
    let mut instructions = settings.instructions.unwrap_or_default();
    for value in [
        &instructions.risk,
        &instructions.authorization,
        &instructions.policy,
    ]
    .into_iter()
    .flatten()
    {
        anyhow::ensure!(
            !value.trim().is_empty() && value.len() <= 4096,
            "Jev instructions must contain 1-4096 bytes of nonblank text"
        );
    }
    anyhow::ensure!(
        provider != Provider::Jev || prompt_source == "default",
        "Jev uses approver.instructions, not prompt/ANY_AUTO_PROMPT"
    );
    if provider == Provider::Jev {
        let effective = crate::backend::jev::questions(&instructions);
        instructions.risk = effective["risk"]["instructions"]
            .as_str()
            .map(str::to_owned);
        instructions.authorization = effective["authorization"]["instructions"]
            .as_str()
            .map(str::to_owned);
        instructions.policy = effective["policy"]["instructions"]
            .as_str()
            .map(str::to_owned);
    }
    let legacy_model = match provider {
        Provider::Cli => cli_model.trim(),
        Provider::Agentapi => model,
        _ => "",
    };
    if settings.model.is_none() && !legacy_model.is_empty() {
        sources.insert(
            "model".into(),
            match provider {
                Provider::Cli => cli_model_source.clone(),
                _ => model_source.clone(),
            },
        );
    }
    for (key, var) in [
        ("model", "ANY_AUTO_APPROVER_MODEL"),
        ("effort", "ANY_AUTO_EFFORT"),
    ] {
        if environment && crate::context::var(var).is_ok_and(|v| !v.is_empty()) {
            sources.insert(key.into(), var.into());
        }
    }
    let selected_model = crate::context::var("ANY_AUTO_APPROVER_MODEL")
        .ok()
        .filter(|v| environment && !v.is_empty())
        .or(settings.model)
        .unwrap_or_else(|| legacy_model.into());
    let selected_model = if provider == Provider::Jev && selected_model.trim().is_empty() {
        "jev-1.13.0".into()
    } else {
        selected_model
    };
    let effort = crate::context::var("ANY_AUTO_EFFORT")
        .ok()
        .filter(|v| environment && !v.is_empty())
        .or(settings.effort)
        .filter(|v| !v.trim().is_empty());
    if let Some(effort) = &effort {
        let supported = match provider {
            Provider::Cli => matches!(effort.as_str(), "low" | "medium" | "high"),
            Provider::Pi => matches!(
                effort.as_str(),
                "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            ),
            Provider::Openai => matches!(
                effort.as_str(),
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            ),
            Provider::Agentapi | Provider::Jev => false,
        };
        anyhow::ensure!(
            supported,
            "Unsupported effort {effort:?} for {}",
            provider.as_str()
        );
    }
    if provider == Provider::Agentapi {
        anyhow::ensure!(
            matches!(selected_model.trim(), "" | "flash_lite" | "flash" | "pro"),
            "Invalid agentapi model tier"
        );
    }
    anyhow::ensure!(
        provider != Provider::Openai || !selected_model.trim().is_empty(),
        "OpenAI approver requires model"
    );
    let context_budget_bytes = settings
        .context_budget_bytes
        .unwrap_or(crate::review_input::DEFAULT_CONTEXT_BUDGET_BYTES);
    anyhow::ensure!(
        context_budget_bytes > 0,
        "context_budget_bytes must be positive"
    );
    let approver = ApproverConfig {
        context_budget_bytes,
        provider,
        model: (!selected_model.trim().is_empty()).then(|| selected_model.trim().into()),
        effort,
        probability_threshold,
        diagnostic_snapshot: settings.diagnostic_snapshot.unwrap_or(false),
        instructions,
        base_url: settings.base_url.unwrap_or_else(|| {
            if provider == Provider::Jev {
                "https://api.typesafe.ai/v1"
            } else {
                "https://api.openai.com/v1"
            }
            .into()
        }),
        api_key: settings.api_key.unwrap_or_default(),
    };
    if provider == Provider::Jev {
        crate::backend::jev::client::endpoint(&approver.base_url)?;
    }
    anyhow::ensure!(
        !approver.api_key.contains(['\r', '\n', '\0']),
        "Invalid API key: control characters are not allowed"
    );
    Ok(ReviewerConfig {
        approver,
        approver_sources: sources,
        cli_model: (!cli_model.trim().is_empty()).then(|| cli_model.trim().to_owned()),
        cli_model_source,
        model: if model.is_empty() {
            None
        } else {
            Some(model.into())
        },
        prompt,
        model_source,
        prompt_source,
    })
}

fn print_approver(
    agent: &str,
    approver: &ApproverConfig,
    sources: &std::collections::BTreeMap<String, String>,
) {
    println!("\n{agent}");
    let row = |label: &str, key: &str, value: &str| {
        let source = sources.get(key).map(String::as_str).unwrap_or("default");
        if source == "default" {
            println!("  {label:<10} {value}");
        } else {
            println!("  {label:<10} {value}  (from {source})");
        }
    };
    row("Approver", "provider", approver.provider.as_str());
    row(
        "Model",
        "model",
        approver.model.as_deref().unwrap_or("default"),
    );
    if approver.provider != Provider::Jev {
        row(
            "Effort",
            "effort",
            approver.effort.as_deref().unwrap_or("default"),
        );
    }
    if matches!(approver.provider, Provider::Openai | Provider::Jev) {
        row("Base URL", "base_url", &approver.base_url);
        row(
            "API key",
            "api_key",
            if approver.api_key.trim().is_empty() {
                "not configured"
            } else {
                "[REDACTED]"
            },
        );
    }
    row(
        "Context budget bytes",
        "context_budget_bytes",
        &approver.context_budget_bytes.to_string(),
    );
    if approver.provider == Provider::Jev {
        row(
            "Threshold",
            "probability_threshold",
            &approver.probability_threshold.to_string(),
        );
        row(
            "Diagnostic snapshot",
            "diagnostic_snapshot",
            &approver.diagnostic_snapshot.to_string(),
        );
        let defaults = crate::backend::jev::questions(&approver.instructions);
        for key in ["risk", "authorization", "policy"] {
            let field = format!("instructions.{key}");
            row(
                &field,
                &field,
                defaults[key]["instructions"].as_str().unwrap(),
            );
        }
    }
}

fn serialize_api_key<S: serde::Serializer>(key: &str, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(if key.trim().is_empty() {
        ""
    } else {
        "[REDACTED]"
    })
}

pub fn show(json: bool) -> anyhow::Result<()> {
    let config = reviewer_config()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "file": config_path(), "reviewer": config, "mode": mode(), "agent":mode().agent(), "instance":instance(),
                "socket": socket_path(), "state_dir": state_dir(), "log_dir": log_dir()
            }))?
        );
    } else {
        println!("Config: {}", config_path().display());
        print_approver(mode().agent(), &config.approver, &config.approver_sources);
        println!("\n  Instance  {}", instance());
        println!(
            "  Prompt    {} ({} lines; use --json for full text)",
            config.prompt_source,
            config.prompt.lines().count()
        );
        println!(
            "  Socket    {}\n  State     {}\n  Logs      {}",
            socket_path().display(),
            state_dir().display(),
            log_dir().display()
        );
        println!(
            "File changes apply on the next review with a new configuration generation. Caller environment changes apply on the next review."
        );
    }
    Ok(())
}

pub fn edit() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = config_path();
    fs::create_dir_all(path.parent().unwrap())?;
    // Start with built-in defaults; environment overrides remain temporary.
    if !path.exists() {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        writeln!(
            file,
            "# Optional common settings; omit to use each agent's defaults.\n# [approver]\n# provider = \"pi\"\n# model = \"provider/model-id\"\n# effort = \"low\"\n\n# Optional per-agent override:\n# [agents.pi.approver]\n# provider = \"pi\"\n# effort = \"low\"\n\n# A top-level prompt string can replace the built-in review policy.\n# Environment variables override file settings."
        )?;
    }
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|key| env::var(key).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("exec {editor} \"$1\""))
        .arg("any-auto-config-editor")
        .arg(&path)
        .status()
        .context("Cannot launch configuration editor")?;
    anyhow::ensure!(status.success(), "Editor exited with {status}");
    let file = file_config()?;
    if let Some(model) = file.model {
        anyhow::ensure!(
            matches!(model.trim(), "" | "flash_lite" | "flash" | "pro"),
            "Invalid model in {}; expected flash_lite, flash, pro, or empty",
            path.display()
        );
    }
    validate_text(&fs::read_to_string(&path)?)?;
    eprintln!(
        "Saved configuration. File changes apply on the next review. Caller environment changes apply on the next review."
    );
    Ok(())
}

/// Validate all agents before saving a staged configuration.
pub fn validate_text(text: &str) -> anyhow::Result<()> {
    for agent in [Mode::Cli, Mode::Sidecar, Mode::Pi] {
        resolve_config(
            agent,
            toml::from_str(text)
                .map_err(|_| anyhow::anyhow!("Invalid TOML or unsupported configuration field"))?,
            false,
        )?;
    }
    Ok(())
}
pub fn save_text(text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    validate_text(text)?;
    let path = config_path();
    fs::create_dir_all(path.parent().unwrap())?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    file.write_all(text.as_bytes())?;
    file.persist(path)?;
    Ok(())
}
pub fn overview(json: bool, selected: Option<Mode>) -> anyhow::Result<()> {
    if selected.is_some() {
        return show(json);
    }
    let agents: Vec<_> = [Mode::Cli, Mode::Sidecar, Mode::Pi]
        .into_iter()
        .map(|agent| reviewer_config_for(agent).map(|c| (agent, c)))
        .collect::<anyhow::Result<_>>()?;
    if json {
        let agents: Vec<_> = agents.iter().map(|(agent, c)| serde_json::json!({"agent":agent.agent(), "approver":c.approver, "sources":c.approver_sources})).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"file":config_path(),"agents":agents})
            )?
        );
    } else {
        println!("Config: {}", config_path().display());
        for (agent, config) in agents {
            print_approver(agent.agent(), &config.approver, &config.approver_sources);
        }
        println!("\nEdit: any-auto config --edit");
        println!("Precedence: defaults < [approver] < [agents.NAME.approver] < environment");
    }
    Ok(())
}

#[cfg(test)]
mod jev_tests {
    use super::*;
    fn resolve(text: &str, mode: Mode) -> anyhow::Result<ReviewerConfig> {
        resolve_config(mode, toml::from_str(text)?, false)
    }
    #[test]
    fn common_context_budget_defaults_overrides_and_validation() {
        assert_eq!(
            resolve("", Mode::Pi).unwrap().approver.context_budget_bytes,
            24576
        );
        let common = "[approver]\nprovider='jev'\ncontext_budget_bytes=32768\n[agents.pi.approver]\nprovider='pi'\n";
        let c = resolve(common, Mode::Pi).unwrap();
        assert_eq!(c.approver.context_budget_bytes, 32768);
        assert_eq!(c.approver_sources["context_budget_bytes"], "[approver]");
        let c = resolve(&format!("{common}context_budget_bytes=65536\n"), Mode::Pi).unwrap();
        assert_eq!(c.approver.context_budget_bytes, 65536);
        assert_eq!(
            c.approver_sources["context_budget_bytes"],
            "[agents.pi.approver]"
        );
        for value in ["0", "-1", "1.5"] {
            assert!(
                resolve(
                    &format!("[approver]\ncontext_budget_bytes={value}"),
                    Mode::Pi
                )
                .is_err()
            );
        }
    }
    #[test]
    fn jev_defaults_and_instruction_inheritance() {
        let c = resolve("[approver]\nprovider='jev'\n", Mode::Pi).unwrap();
        assert_eq!(c.approver.probability_threshold, 0.9);
        assert_eq!(c.approver.model.as_deref(), Some("jev-1.13.0"));
        assert!(c.approver.api_key.is_empty());
        assert_eq!(c.approver.base_url, "https://api.typesafe.ai/v1");
        let text = "[approver]\nprovider='jev'\nprobability_threshold=0.8\n[approver.instructions]\nrisk='common risk'\npolicy='common policy'\n[agents.pi.approver]\nprobability_threshold=0.95\n[agents.pi.approver.instructions]\nrisk='pi risk'\n";
        let c = resolve(text, Mode::Pi).unwrap();
        assert_eq!(c.approver.probability_threshold, 0.95);
        assert_eq!(c.approver.instructions.risk.as_deref(), Some("pi risk"));
        assert_eq!(
            c.approver.instructions.policy.as_deref(),
            Some("common policy")
        );
        let c = resolve(text, Mode::Cli).unwrap();
        assert_eq!(c.approver.instructions.risk.as_deref(), Some("common risk"));
        let c = resolve(
            &format!("{text}\n[agents.agy-cli.approver]\nprovider='cli'\n"),
            Mode::Cli,
        )
        .unwrap();
        assert!(c.approver.instructions.risk.is_none());
    }
    #[test]
    fn diagnostic_snapshot_defaults_inherits_and_overrides() {
        assert!(
            !resolve("[approver]\nprovider='jev'", Mode::Pi)
                .unwrap()
                .approver
                .diagnostic_snapshot
        );
        let text = "[approver]\nprovider='jev'\ndiagnostic_snapshot=true\n[agents.pi.approver]\ndiagnostic_snapshot=false\n";
        assert!(
            resolve(text, Mode::Cli)
                .unwrap()
                .approver
                .diagnostic_snapshot
        );
        let pi = resolve(text, Mode::Pi).unwrap();
        assert!(!pi.approver.diagnostic_snapshot);
        assert_eq!(
            pi.approver_sources["diagnostic_snapshot"],
            "[agents.pi.approver]"
        );
        assert!(
            !resolve(
                &format!("{text}\n[agents.agy-cli.approver]\nprovider='cli'"),
                Mode::Cli
            )
            .unwrap()
            .approver
            .diagnostic_snapshot
        );
        assert!(
            resolve(
                "[approver]\nprovider='jev'\ndiagnostic_snapshot='true'",
                Mode::Pi
            )
            .is_err()
        );
        assert!(
            resolve(
                "[approver]\nprovider='pi'\ndiagnostic_snapshot=true",
                Mode::Pi
            )
            .is_err()
        );
    }
    #[test]
    fn direct_keys_inherit_override_and_reset_with_provider() {
        let text = "[approver]\nprovider='jev'\napi_key='common-secret'\n[agents.pi.approver]\napi_key='pi-secret'\n[agents.agy-desktop.approver]\nprovider='openai'\nmodel='fixture'\n";
        assert_eq!(
            resolve(text, Mode::Cli).unwrap().approver.api_key,
            "common-secret"
        );
        let pi = resolve(text, Mode::Pi).unwrap();
        assert_eq!(pi.approver.api_key, "pi-secret");
        assert!(!serde_json::to_string(&pi).unwrap().contains("pi-secret"));
        assert!(
            resolve(text, Mode::Sidecar)
                .unwrap()
                .approver
                .api_key
                .is_empty()
        );
        assert!(validate_text("[approver]\napi_key_env='OLD_KEY'").is_err());
    }
    #[test]
    fn reject_invalid_jev_configuration() {
        for text in [
            "[approver]\nprovider='jev'\nprobability_threshold=nan",
            "[approver]\nprovider='jev'\nprobability_threshold=1.1",
            "[approver]\nprovider='jev'\nprobability_threshold=-0.1",
            "[approver]\nprovider='pi'\nprobability_threshold=0.9",
            "[approver]\nprovider='jev'\neffort='low'",
            "prompt='custom'\n[approver]\nprovider='jev'",
            "[approver]\nprovider='jev'\nbase_url='http://example.com/v1'",
            "[approver]\nprovider='jev'\napi_key='bad\nkey'",
            "[approver]\nprovider='jev'\n[approver.instructions]\nrisk=' '",
            "[approver]\nprovider='jev'\n[approver.instructions]\nunknown='bad'",
        ] {
            assert!(resolve(text, Mode::Pi).is_err(), "{text}");
        }
        for threshold in [0, 1] {
            assert!(
                resolve(
                    &format!("[approver]\nprovider='jev'\nprobability_threshold={threshold}"),
                    Mode::Pi
                )
                .is_ok()
            );
        }
    }
}
