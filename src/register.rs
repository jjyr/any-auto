use crate::config;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{fs, path::Path};
fn update(path: &Path, dry_run: bool, f: impl FnOnce(&mut Value) -> Result<()>) -> Result<()> {
    let mut data = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .with_context(|| format!("Invalid JSON in {}; leaving it unchanged", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e.into()),
    };
    if !data.is_object() {
        bail!("Expected JSON object in {}", path.display());
    }
    f(&mut data)?;
    if dry_run {
        println!("Would update {}", path.display());
        return Ok(());
    }
    fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec_pretty(&data)?)?;
    fs::rename(tmp, path)?;
    println!("Updated {}", path.display());
    Ok(())
}
fn object_field<'a>(v: &'a mut Value, key: &str) -> Result<&'a mut Value> {
    if v.get(key).is_none() {
        v[key] = json!({});
    }
    if !v[key].is_object() {
        bail!("Expected {key} to be an object");
    }
    Ok(&mut v[key])
}
pub fn register(cli_only: bool, desktop_only: bool) -> Result<()> {
    register_agy(cli_only, desktop_only, false)
}

pub fn preview(cli_only: bool, desktop_only: bool) -> Result<()> {
    register_agy(cli_only, desktop_only, true)
}

fn register_agy(cli_only: bool, desktop_only: bool, dry_run: bool) -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let executable = exe.to_str().context("Executable path is not UTF-8")?;
    let quoted = format!("'{}'", executable.replace('\'', "'\"'\"'"));
    let plugin: Value = serde_json::from_str(include_str!("../agy/plugin.json"))?;
    let plugin_name = plugin["name"]
        .as_str()
        .context("Plugin template requires name")?;
    let mut hooks: Value = serde_json::from_str(include_str!("../agy/hooks.json"))?;
    let entries = hooks[plugin_name]["PreToolUse"]
        .as_array_mut()
        .context("Hook template requires PreToolUse")?;
    for entry in entries {
        for hook in entry["hooks"]
            .as_array_mut()
            .context("Hook template requires hooks")?
        {
            let command = hook["command"]
                .as_str()
                .context("Hook template requires command")?;
            let args = command
                .strip_prefix(&format!("{plugin_name} "))
                .context("Unexpected hook executable in template")?;
            hook["command"] = json!(format!("{quoted} {args}"));
        }
    }
    let mut sidecar: Value =
        serde_json::from_str(include_str!("../agy/sidecars/approver/sidecar.json"))?;
    sidecar["command"] = json!(executable);
    let sidecar_name = sidecar["name"]
        .as_str()
        .context("Sidecar template requires name")?
        .to_owned();
    let base = config::home().join(".gemini/config");
    if !desktop_only {
        update(&base.join("hooks.json"), dry_run, |v| {
            v[plugin_name] = hooks[plugin_name].clone();
            Ok(())
        })?;
        let settings = config::home().join(".gemini/antigravity-cli/settings.json");
        if settings.exists() {
            update(&settings, dry_run, |v| {
                let permissions = object_field(v, "permissions")?;
                if permissions.get("allow").is_none() {
                    permissions["allow"] = json!([]);
                }
                let allow = permissions["allow"]
                    .as_array_mut()
                    .context("permissions.allow must be an array")?;
                for p in "gh npm npx yarn pnpm bun git python python3 pytest cargo go node make docker docker-compose curl cat echo ls mkdir cp touch grep find sh bash zsh head tail mise uv".split_whitespace() {
                    let grant = json!(format!("command({p})"));
                    if !allow.contains(&grant) { allow.push(grant); }
                }
                allow.sort_by_key(Value::to_string);
                Ok(())
            })?;
        }
    }
    if !cli_only {
        update(&base.join("config.json"), dry_run, |v| {
            object_field(v, "sidecars")?[format!("{plugin_name}/{sidecar_name}")] =
                json!({"enabled":true});
            Ok(())
        })?;
        for relative in [
            format!("sidecars/{sidecar_name}/sidecar.json"),
            format!("sidecars/{plugin_name}/{sidecar_name}/sidecar.json"),
        ] {
            update(&base.join(relative), dry_run, |v| {
                *v = sidecar.clone();
                Ok(())
            })?;
        }
    }
    Ok(())
}

/// Install only the Pi integration, preserving unrelated extensions/settings.
pub fn register_pi() -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let base = std::env::var_os("PI_CODING_AGENT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config::home().join(".pi/agent"));
    let path = base.join("extensions/any-auto.ts");
    fs::create_dir_all(path.parent().unwrap())?;
    let source = include_str!("../pi/extensions/any-auto.ts").replace(
        "const executable = \"any-auto\";",
        &format!("const executable = {};", serde_json::to_string(&exe)?),
    );
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    use std::io::Write;
    file.write_all(source.as_bytes())?;
    file.persist(&path)?;
    println!("Installed {}. Run /reload in Pi.", path.display());
    Ok(())
}
