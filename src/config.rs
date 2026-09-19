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
    Agentapi,
}
impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Cli => "cli",
            Self::Openai => "openai",
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
    api_key_env: Option<String>,
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
#[derive(Clone, serde::Serialize)]
pub struct ApproverConfig {
    pub provider: Provider,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub base_url: String,
    pub api_key_env: String,
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
        Mode::Sidecar => Provider::Agentapi,
        Mode::Pi => Provider::Pi,
    };
    let agent = match mode {
        Mode::Cli => file.agents.cli.approver,
        Mode::Sidecar => file.agents.sidecar.approver,
        Mode::Pi => file.agents.pi.approver,
    };
    let mut sources = std::collections::BTreeMap::new();
    for key in ["provider", "model", "effort", "base_url", "api_key_env"] {
        sources.insert(key.to_owned(), "default".to_owned());
    }
    let common = serde_json::to_value(&file.approver)?;
    for (key, value) in common.as_object().unwrap() {
        if !value.is_null() {
            sources.insert(key.clone(), "[approver]".into());
        }
    }
    let agent_values = serde_json::to_value(&agent)?;
    let mut settings = file.approver;
    if agent.provider.is_some() && agent.provider != settings.provider.or(Some(defaults)) {
        settings = ApproverSettings::default();
        sources.values_mut().for_each(|s| *s = "default".into());
    }
    for (key, value) in agent_values.as_object().unwrap() {
        if !value.is_null() {
            sources.insert(key.clone(), format!("[agents.{}.approver]", mode.agent()));
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
    if agent.api_key_env.is_some() {
        settings.api_key_env = agent.api_key_env;
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
            settings.api_key_env = None;
            sources.values_mut().for_each(|s| *s = "default".into());
        }
        settings.provider = Some(provider);
        sources.insert("provider".into(), "ANY_AUTO_PROVIDER".into());
    }
    let provider = settings.provider.unwrap_or(defaults);
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
            Provider::Agentapi => false,
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
    let approver = ApproverConfig {
        provider,
        model: (!selected_model.trim().is_empty()).then(|| selected_model.trim().into()),
        effort,
        base_url: settings
            .base_url
            .unwrap_or_else(|| "https://api.openai.com/v1".into()),
        api_key_env: settings
            .api_key_env
            .unwrap_or_else(|| "OPENAI_API_KEY".into()),
    };
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
        println!(
            "Model: {} ({})",
            config.model.as_deref().unwrap_or("agent default"),
            config.model_source
        );
        println!(
            "CLI model: {} ({})",
            config.cli_model.as_deref().unwrap_or("agent default"),
            config.cli_model_source
        );
        println!("Agent: {}", mode().agent());
        println!("Approver: {}", serde_json::to_string(&config.approver)?);
        println!(
            "Sources: {}",
            serde_json::to_string(&config.approver_sources)?
        );
        println!("Prompt ({}):\n{}", config.prompt_source, config.prompt);
        println!(
            "Socket: {}\nState: {}\nLogs: {}",
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
        resolve_config(agent, toml::from_str(text)?, false)?;
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
        .map(|agent| {
            reviewer_config_for(agent)
                .map(|c| serde_json::json!({"agent":agent.agent(), "approver":c.approver, "sources":c.approver_sources}))
        })
        .collect::<anyhow::Result<_>>()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"file":config_path(),"agents":agents})
            )?
        );
    } else {
        println!("Config: {}", config_path().display());
        for agent in agents {
            println!(
                "{}: {}",
                agent["agent"].as_str().unwrap(),
                agent["approver"]
            );
            println!("  sources: {}", agent["sources"]);
        }
        println!("Precedence: defaults < [approver] < [agents.NAME.approver] < environment");
    }
    Ok(())
}
