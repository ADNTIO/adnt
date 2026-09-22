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

//! OpenCode: install/update, "vllm" provider and "security-review" agent.

use super::{
    api_base_url, check_installed, config_home, find_in_path, login, run_installer, Shell, MODELS,
};
use crate::secure_fs::write_private;
use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::Path;
use std::process::Command;

const PROVIDER_ID: &str = "vllm";
const INSTALL_URL: &str = "https://opencode.ai/install";
const NPM_PACKAGE: &str = "opencode-ai";

/// Read-only static security review (SAST) on the coder model, a strong tool caller.
const SECURITY_AGENT: &str = r#"---
description: Revue de sécurité statique d'une base de code (SAST), en lecture seule
mode: primary
model: vllm/coder
temperature: 0.1
permission:
  edit: deny
---
Tu es un auditeur AppSec spécialisé en revue de code statique (SAST). Analyse la base
de code pour identifier des vulnérabilités, SANS la modifier (lecture seule).

Méthode :
1. Cartographie : langage, frameworks, points d'entrée (HTTP, CLI, désérialisation,
   upload) et zones sensibles (authn/authz, accès données, secrets, exécution).
2. Cherche en priorité : injections (SQL/NoSQL/commande/LDAP), XSS, SSRF, path
   traversal, désérialisation non sûre, IDOR / contournement d'autorisation, secrets
   en dur, crypto faible, SSTI, validation d'entrée manquante, dépendances vulnérables.
3. Lis réellement les fichiers (outils read/grep/glob) ; suis les flux source→sink
   pour confirmer l'exploitabilité et éviter les faux positifs.
4. Pour chaque finding : `fichier:ligne`, sévérité (Critical/High/Medium/Low), vecteur,
   impact, correctif concret (exemple de code) et référence CWE.
5. Termine par une synthèse priorisée. Ne modifie aucun fichier.
"#;

pub async fn install() -> Result<()> {
    ensure_opencode().await?;
    let access_token = login().await?;

    let config_dir = config_home()?.join("opencode");
    write_config(&config_dir.join("opencode.json"), &access_token)?;
    write_agent(&config_dir.join("agents/security-review.md"))?;

    println!(
        "\n{}",
        "→ Ready: `opencode` → /models (chat/coder); SAST agent: `@security-review`".green()
    );
    Ok(())
}

async fn ensure_opencode() -> Result<()> {
    if let Some(exe) = find_in_path("opencode") {
        println!("{}", "Updating OpenCode...".cyan());
        // A failed upgrade keeps the current version: not fatal
        let _ = Command::new(exe).arg("upgrade").status();
        Ok(())
    } else {
        println!("{}", "Installing OpenCode...".cyan());
        if cfg!(windows) {
            // The install script is Unix-only: use the official npm package
            let npm = find_in_path("npm")
                .context("npm is required to install OpenCode on Windows (https://nodejs.org)")?;
            let status = Command::new(npm)
                .args(["install", "-g", NPM_PACKAGE])
                .status()?;
            check_installed(status, "npm install -g opencode-ai")
        } else {
            run_installer(INSTALL_URL, Shell::Bash, &[]).await
        }
    }
}

/// Writes the "vllm" provider into opencode.json, keeping the rest of the config.
fn write_config(path: &Path, access_token: &str) -> Result<()> {
    let mut config = read_object(path)?;
    config
        .entry("$schema")
        .or_insert_with(|| json!("https://opencode.ai/config.json"));
    // Sovereignty: ADNT models only. Without an allow-list OpenCode exposes its free
    // "Zen" models (prompts sent to opencode.ai). Session sharing (/share) disabled.
    config.insert("enabled_providers".into(), json!([PROVIDER_ID]));
    config.insert("share".into(), json!("disabled"));
    // No auto-update (install runs `opencode upgrade`); webfetch needs confirmation
    // so a manipulated model cannot exfiltrate through a URL without consent.
    config.insert("autoupdate".into(), json!(false));
    object_entry(&mut config, "permission").insert("webfetch".into(), json!("ask"));

    let provider = object_entry(object_entry(&mut config, "provider"), PROVIDER_ID);
    let models: Map<String, Value> = MODELS
        .iter()
        .map(|model| {
            let limit = json!({ "context": model.context, "output": model.output });
            (
                model.id.to_string(),
                json!({ "name": model.name, "limit": limit }),
            )
        })
        .collect();
    provider.insert("npm".into(), json!("@ai-sdk/openai-compatible"));
    provider.insert("name".into(), json!("vLLM ADNT"));
    provider.insert("models".into(), Value::Object(models));
    // The openai-compatible provider only sends the Authorization header when
    // apiKey is set in the options
    let options = object_entry(provider, "options");
    options.insert("baseURL".into(), json!(api_base_url()?));
    options.insert("apiKey".into(), json!(access_token));

    write_private(path, &(serde_json::to_string_pretty(&config)? + "\n"))?;
    println!(
        "{}",
        format!(
            "✓ Provider '{}' and key written to {}",
            PROVIDER_ID,
            path.display()
        )
        .green()
    );
    Ok(())
}

fn write_agent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, SECURITY_AGENT)?;
    println!(
        "{}",
        format!("✓ Agent 'security-review' written to {}", path.display()).green()
    );
    Ok(())
}

/// Reads a JSON object (empty if the file is missing). Fails on invalid JSON
/// (e.g. JSONC comments) rather than overwriting the user's settings.
fn read_object(path: &Path) -> Result<Map<String, Value>> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).with_context(|| {
            format!(
                "Invalid JSON in {} (comments are not supported)",
                path.display()
            )
        }),
        Err(_) => Ok(Map::new()),
    }
}

/// Returns the object stored under `key`, replacing any non-object value.
fn object_entry<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let value = map.entry(key).or_insert_with(|| json!({}));
    if !value.is_object() {
        *value = json!({});
    }
    value.as_object_mut().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure_fs::assert_owner_only;
    use tempfile::tempdir;

    #[test]
    fn test_write_config_keeps_user_settings() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("opencode.json");
        fs::write(
            &path,
            r#"{"theme": "dark", "provider": {"vllm": {"options": {"timeout": 5}}}}"#,
        )
        .unwrap();

        write_config(&path, "jwt").unwrap();

        let config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["enabled_providers"], json!(["vllm"]));
        let options = &config["provider"]["vllm"]["options"];
        assert_eq!(options["timeout"], 5);
        assert_eq!(options["apiKey"], "jwt");
        assert_eq!(
            config["provider"]["vllm"]["models"]["coder"]["limit"]["output"],
            8192
        );

        assert_owner_only(&path);
    }

    #[test]
    fn test_write_config_refuses_invalid_json() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("opencode.json");
        let jsonc = "{\n  // comment\n  \"theme\": \"dark\",\n}\n";
        fs::write(&path, jsonc).unwrap();

        assert!(write_config(&path, "jwt").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), jsonc);
    }

    #[test]
    fn test_write_agent() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("agents/security-review.md");

        write_agent(&path).unwrap();

        let agent = fs::read_to_string(&path).unwrap();
        assert!(agent.starts_with("---\n"));
        assert!(agent.contains("model: vllm/coder"));
        assert!(agent.contains("edit: deny"));
    }
}
