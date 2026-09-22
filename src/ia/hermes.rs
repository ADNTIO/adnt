// ADNT - Dynamic CLI tool manager for ADNT projects
// Copyright (C) 2025 ADNT Sàrl <info@adnt.io>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Hermes Agent (Nous Research): install/update and "adnt" provider. Only ADNT
//! models stay reachable: no fallback, auxiliary tasks and sub-agents on the main
//! model, remote catalog disabled, credentials of other providers removed.

use super::{api_base_url, find_in_path, login, run_installer, Model, Shell, MODELS};
use crate::secure_fs::write_private;
use anyhow::{Context, Result};
use colored::Colorize;
use serde_norway::{Mapping, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const INSTALL_URL: &str = "https://hermes-agent.nousresearch.com/install.sh";
const INSTALL_URL_WINDOWS: &str = "https://hermes-agent.nousresearch.com/install.ps1";
/// Hermes needs at least 64k of context for agentic use (tools)
const MIN_CONTEXT: u64 = 64000;
const DEFAULT_MODEL: &str = "agent";
const PROVIDER: &str = "adnt";

/// Dummy values written to `.env` by earlier versions to block borrowed
/// credentials. Hermes counts a set provider variable as an explicit opt-in to
/// that provider, so they are removed in favor of Hermes' own switches.
const LEGACY_BLOCKED_ENV: &[&str] = &["COPILOT_GITHUB_TOKEN", "ANTHROPIC_API_KEY"];
const LEGACY_BLOCKED_VALUE: &str = "disabled-adnt-only";

/// Suppresses every Copilot token source (gh CLI and GitHub env vars) for the
/// copilot provider only, through Hermes' own locked auth store API: GH_TOKEN
/// and GITHUB_TOKEN keep working for `gh` and GitHub tools.
const SUPPRESS_COPILOT: &str = "\
from hermes_cli.auth import suppress_credential_source
from hermes_cli.copilot_auth import COPILOT_ENV_VARS
for source in ['gh_cli'] + ['env:' + var for var in COPILOT_ENV_VARS]:
    suppress_credential_source('copilot', source)
";

pub async fn install(model: Option<String>) -> Result<()> {
    let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let agent_models = agent_models().map(|m| m.id).collect::<Vec<_>>();
    if !agent_models.contains(&model.as_str()) {
        anyhow::bail!(
            "Unknown model '{}' for hermes (expected one of: {})",
            model,
            agent_models.join(", ")
        );
    }

    ensure_hermes().await?;
    let access_token = login().await?;

    let home = hermes_home()?;
    write_config(&home.join("config.yaml"), &access_token, &model)?;
    remove_foreign_credentials(&home)?;

    println!(
        "\n{}",
        format!(
            "→ Ready: `hermes` (other models: adnt ia install hermes --model {})",
            agent_models.join("|")
        )
        .green()
    );
    Ok(())
}

fn agent_models() -> impl Iterator<Item = &'static Model> {
    MODELS.iter().filter(|m| m.context >= MIN_CONTEXT)
}

/// `HERMES_HOME`, else the platform default used by Hermes itself:
/// `%LOCALAPPDATA%\hermes` on Windows, `~/.hermes` elsewhere.
fn hermes_home() -> Result<PathBuf> {
    let default = || {
        if cfg!(windows) {
            dirs::data_local_dir().map(|dir| dir.join("hermes"))
        } else {
            dirs::home_dir().map(|home| home.join(".hermes"))
        }
    };
    env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(default)
        .context("Could not determine Hermes home directory")
}

/// `hermes` from PATH, else where the official installers put it (not yet on
/// the PATH of the current shell right after an install).
fn hermes_bin() -> Option<PathBuf> {
    let installed = || {
        if cfg!(windows) {
            hermes_home()
                .ok()
                .map(|home| home.join("hermes-agent/venv/Scripts/hermes.exe"))
        } else {
            dirs::home_dir().map(|home| home.join(".local/bin/hermes"))
        }
    };
    find_in_path("hermes").or_else(|| installed().filter(|path| path.is_file()))
}

async fn ensure_hermes() -> Result<()> {
    if let Some(exe) = hermes_bin() {
        println!("{}", "Updating Hermes Agent...".cyan());
        // A failed update keeps the current version: not fatal
        let _ = Command::new(exe).arg("update").status();
        Ok(())
    } else {
        println!("{}", "Installing Hermes Agent...".cyan());
        // Skip the interactive setup wizard: the config is written by `adnt`
        if cfg!(windows) {
            run_installer(INSTALL_URL_WINDOWS, Shell::PowerShell, &["-SkipSetup"]).await
        } else {
            run_installer(INSTALL_URL, Shell::Bash, &["--skip-setup"]).await
        }
    }
}

/// Writes the "adnt" provider (every model with enough context, selectable
/// through /model) and the default model, keeping the rest of the config.
fn write_config(path: &Path, access_token: &str, model_id: &str) -> Result<()> {
    let mut config: Mapping = match fs::read_to_string(path) {
        Ok(content) => serde_norway::from_str(&content)
            .with_context(|| format!("Invalid YAML in {}", path.display()))?,
        Err(_) => Mapping::new(),
    };
    let base_url = api_base_url()?;

    // Context window = llama-swap server context, per model (auto-detection
    // through LiteLLM falls back to 128k); Hermes finds it through base_url
    let mut models = Mapping::new();
    for model in agent_models() {
        let mut settings = Mapping::new();
        settings.insert("context_length".into(), model.context.into());
        models.insert(model.id.into(), settings.into());
    }
    let mut provider = Mapping::new();
    provider.insert("name".into(), "ADNT".into());
    provider.insert("base_url".into(), base_url.as_str().into());
    provider.insert("api_key".into(), access_token.into());
    provider.insert("default_model".into(), model_id.into());
    provider.insert("models".into(), models.into());
    mapping_entry(&mut config, "providers").insert(PROVIDER.into(), provider.into());

    // Sovereignty: no fallback to another provider, no remote catalog
    // (OpenRouter/Nous model list in /model)
    config.insert("fallback_providers".into(), Value::Sequence(vec![]));
    config.remove("fallback_model");
    mapping_entry(&mut config, "model_catalog").insert("enabled".into(), false.into());
    // Never read or refresh borrowed CLI logins (Claude Code, Codex CLI)
    mapping_entry(&mut config, "auth").insert("adopt_external_logins".into(), false.into());

    // Auxiliary tasks (compression, titles, vision...): "auto" = main model, so
    // ADNT; any override towards another provider is erased
    if let Some(Value::Mapping(auxiliary)) = config.get_mut("auxiliary") {
        for task in auxiliary.values_mut() {
            if let Value::Mapping(task) = task {
                set_all(
                    task,
                    &[
                        ("provider", "auto"),
                        ("model", ""),
                        ("base_url", ""),
                        ("api_key", ""),
                    ],
                );
            }
        }
    }
    // Sub-agents inherit the main model
    if let Some(Value::Mapping(delegation)) = config.get_mut("delegation") {
        set_all(
            delegation,
            &[
                ("provider", ""),
                ("model", ""),
                ("base_url", ""),
                ("api_key", ""),
            ],
        );
    }

    let model = mapping_entry(&mut config, "model");
    // A global model.context_length would override the per-model context on /model
    model.remove("context_length");
    set_all(
        model,
        &[
            ("provider", "custom"),
            ("base_url", &base_url),
            ("default", model_id),
            ("api_key", access_token),
            ("api_mode", "chat_completions"),
        ],
    );

    write_private(path, &serde_norway::to_string(&config)?)?;
    println!(
        "{}",
        format!(
            "✓ Provider '{}' and key written to {}; default model '{}'",
            PROVIDER,
            path.display(),
            model_id
        )
        .green()
    );
    Ok(())
}

/// Returns the mapping stored under `key`, replacing any non-mapping value.
fn mapping_entry<'a>(map: &'a mut Mapping, key: &str) -> &'a mut Mapping {
    let value = map
        .entry(key.into())
        .or_insert_with(|| Mapping::new().into());
    if !value.is_mapping() {
        *value = Mapping::new().into();
    }
    value.as_mapping_mut().unwrap()
}

fn set_all(map: &mut Mapping, entries: &[(&str, &str)]) {
    for (key, value) in entries {
        map.insert((*key).into(), (*value).into());
    }
}

/// Removes every non-ADNT credential from the Hermes pool (OpenRouter, Codex,
/// Copilot borrowed from `gh auth token`...). `hermes auth remove` marks the
/// source as removed, so it is not imported again.
fn remove_foreign_credentials(home: &Path) -> Result<()> {
    remove_legacy_blocked_env(&home.join(".env"))?;

    let Some(exe) = hermes_bin() else {
        return Ok(());
    };
    let output = Command::new(&exe).args(["auth", "list"]).output()?;
    let credentials = foreign_credentials(&String::from_utf8_lossy(&output.stdout));

    let mut removed: Vec<&str> = Vec::new();
    for (provider, id) in &credentials {
        let _ = Command::new(&exe)
            .args(["auth", "remove", provider, id])
            .output();
        if !removed.contains(&provider.as_str()) {
            removed.push(provider);
        }
    }
    removed.sort();
    // Also clears the stored OAuth state (auth.json)
    for provider in &removed {
        let _ = Command::new(&exe)
            .args(["auth", "logout", provider])
            .output();
    }
    let removed = if removed.is_empty() {
        "none".to_string()
    } else {
        removed.join(", ")
    };
    println!(
        "{}",
        format!("✓ Non-ADNT credentials removed: {}", removed).green()
    );

    suppress_copilot_sources(&exe);
    Ok(())
}

/// Runs [`SUPPRESS_COPILOT`] with the Python interpreter of the Hermes install.
/// Not fatal: the ADNT provider works without it.
fn suppress_copilot_sources(exe: &Path) {
    // The `hermes` launcher sits next to its virtualenv interpreter
    // (venv/bin on Unix, venv\Scripts on Windows)
    let python = fs::canonicalize(exe).ok().and_then(|exe| {
        exe.parent()
            .map(|dir| dir.join(format!("python{}", env::consts::EXE_SUFFIX)))
    });
    let status = python.and_then(|python| {
        Command::new(python)
            .args(["-c", SUPPRESS_COPILOT])
            .status()
            .ok()
    });
    if status.is_some_and(|status| status.success()) {
        println!(
            "{}",
            "✓ Copilot token sources (gh CLI, GH_TOKEN, GITHUB_TOKEN) disabled for Hermes".green()
        );
    } else {
        println!(
            "{}",
            "⚠ Could not disable Copilot token sources: run `hermes auth remove copilot` if needed"
                .yellow()
        );
    }
}

/// Parses `hermes auth list` and returns the (provider, id) pairs to remove.
fn foreign_credentials(auth_list: &str) -> Vec<(String, String)> {
    let own_provider = format!("custom:{}", PROVIDER);
    let mut provider: Option<&str> = None;
    let mut credentials = Vec::new();
    for line in auth_list.lines() {
        if let Some(name) = provider_header(line) {
            provider = Some(name);
        } else if let (Some(name), Some(id)) = (provider, credential_id(line)) {
            if name != own_provider {
                credentials.push((name.to_string(), id.to_string()));
            }
        }
    }
    credentials
}

/// Matches a provider header line: `<provider> (<n> credential[s]):`.
fn provider_header(line: &str) -> Option<&str> {
    let (name, rest) = line.split_once(' ')?;
    let count = rest
        .strip_prefix('(')?
        .strip_suffix("credentials):")
        .or_else(|| rest.strip_prefix('(')?.strip_suffix("credential):"))?
        .strip_suffix(' ')?;
    let valid = !name.is_empty() && !count.is_empty() && count.chars().all(|c| c.is_ascii_digit());
    valid.then_some(name)
}

/// Extracts the value of a whole-word `id=` field.
fn credential_id(line: &str) -> Option<&str> {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut search = 0;
    while let Some(offset) = line[search..].find("id=") {
        let start = search + offset;
        let after = &line[start + 3..];
        let id_len = after.find(|c: char| !is_word(c)).unwrap_or(after.len());
        let at_word_start = !line[..start].chars().next_back().is_some_and(is_word);
        if at_word_start && id_len > 0 {
            return Some(&after[..id_len]);
        }
        search = start + 3;
    }
    None
}

/// Removes the dummy values written by earlier versions, keeping any real value.
fn remove_legacy_blocked_env(path: &Path) -> Result<()> {
    let Ok(existing) = fs::read_to_string(path) else {
        return Ok(());
    };
    let is_legacy = |line: &&str| {
        line.split_once('=').is_some_and(|(key, value)| {
            LEGACY_BLOCKED_ENV.contains(&key) && value == LEGACY_BLOCKED_VALUE
        })
    };
    if !existing.lines().any(|line| is_legacy(&line)) {
        return Ok(());
    }
    let lines: Vec<&str> = existing.lines().filter(|line| !is_legacy(line)).collect();
    let contents = if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    };
    write_private(path, &contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure_fs::assert_owner_only;
    use tempfile::tempdir;

    #[test]
    fn test_foreign_credentials() {
        let auth_list = "\
  #0 key  id=orphan (before any provider header)
Credential pool (3 providers):
openrouter (2 credentials):
  #1 key  id=abc123 source=env
  #2 key  id=def_456 source=file
custom:adnt (1 credential):
  #1 key  id=keep01
copilot (1 credential):
  #1 oauth  id=cop9 (gh)
  #2 oauth  client_id=not_an_id
";
        assert_eq!(
            foreign_credentials(auth_list),
            vec![
                ("openrouter".to_string(), "abc123".to_string()),
                ("openrouter".to_string(), "def_456".to_string()),
                ("copilot".to_string(), "cop9".to_string()),
            ]
        );
    }

    #[test]
    fn test_credential_id_is_whole_word() {
        assert_eq!(credential_id("  client_id=nope id=yes"), Some("yes"));
        assert_eq!(credential_id("  no credential here"), None);
    }

    #[test]
    fn test_write_config() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("config.yaml");
        fs::write(
            &path,
            "\
toolsets: [web]
fallback_model: openrouter/foo
model:
  context_length: 128000
model_catalog: true
auxiliary:
  vision:
    provider: openrouter
    model: gpt
  titles: disabled
delegation:
  provider: openrouter
  model: gpt
  max_depth: 2
",
        )
        .unwrap();

        write_config(&path, "jwt", "coder").unwrap();

        let config: Value = serde_norway::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["toolsets"][0], "web");
        assert!(config.get("fallback_model").is_none());
        assert_eq!(config["model"]["default"], "coder");
        assert_eq!(config["model"]["api_key"], "jwt");
        assert!(config["model"].get("context_length").is_none());
        assert_eq!(config["auxiliary"]["vision"]["provider"], "auto");
        assert_eq!(config["auxiliary"]["vision"]["model"], "");
        assert_eq!(config["auxiliary"]["titles"], "disabled");
        assert_eq!(config["delegation"]["provider"], "");
        assert_eq!(config["delegation"]["max_depth"], 2);
        assert_eq!(config["fallback_providers"], Value::Sequence(vec![]));
        assert_eq!(config["model"]["provider"], "custom");
        assert_eq!(config["providers"]["adnt"]["api_key"], "jwt");
        assert_eq!(config["providers"]["adnt"]["default_model"], "coder");
        assert_eq!(config["model_catalog"]["enabled"], false);
        assert_eq!(config["auth"]["adopt_external_logins"], false);
        let models = config["providers"]["adnt"]["models"].as_mapping().unwrap();
        assert!(models.contains_key("agent"));
        assert!(!models.contains_key("chat"));
        assert_eq!(models["agent"]["context_length"], 262144);

        assert_owner_only(&path);
    }

    #[test]
    fn test_write_config_refuses_invalid_yaml() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("config.yaml");
        fs::write(&path, "model: [unclosed\n").unwrap();

        assert!(write_config(&path, "jwt", "agent").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "model: [unclosed\n");
    }

    #[test]
    fn test_remove_legacy_blocked_env() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join(".env");
        fs::write(
            &path,
            "FOO=bar\nCOPILOT_GITHUB_TOKEN=disabled-adnt-only\nANTHROPIC_API_KEY=real\n",
        )
        .unwrap();

        remove_legacy_blocked_env(&path).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "FOO=bar\nANTHROPIC_API_KEY=real\n"
        );

        // Nothing to clean: untouched; missing file: no error
        remove_legacy_blocked_env(&path).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "FOO=bar\nANTHROPIC_API_KEY=real\n"
        );
        remove_legacy_blocked_env(&temp_dir.path().join("missing")).unwrap();
    }
}
