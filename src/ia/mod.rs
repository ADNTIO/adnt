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

//! `adnt ia`: installs AI coding agents wired to the ADNT inference API
//! (LiteLLM, OpenAI-compatible, OIDC authentication through Authentik).

mod hermes;
mod oidc;
mod opencode;

use anyhow::{Context, Result};
use clap::ValueEnum;
use colored::Colorize;
use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

const DEFAULT_VLLM_URL: &str = "https://vllm.k8s.adnt.io";

/// A model exposed by LiteLLM (`id` is the name on the API side).
/// `output` caps the completion so agents never exceed the context window.
struct Model {
    id: &'static str,
    name: &'static str,
    context: u64,
    output: u64,
}

const MODELS: &[Model] = &[
    Model {
        id: "chat",
        name: "Qwen3.5 9B (chat)",
        context: 32768,
        output: 8192,
    },
    Model {
        id: "coder",
        name: "Qwen3-Coder 30B (code)",
        context: 65536,
        output: 8192,
    },
    Model {
        id: "agent",
        name: "Qwen3.6 35B-A3B (agent)",
        context: 262144,
        output: 8192,
    },
    Model {
        id: "longctx",
        name: "Qwen2.5 7B — 1M ctx",
        context: 262144,
        output: 8192,
    },
    Model {
        id: "minimax",
        name: "MiniMax-M2.7 (gros, lent)",
        context: 65536,
        output: 8192,
    },
    Model {
        id: "translate-eurollm",
        name: "EuroLLM 22B (traduction)",
        context: 32768,
        output: 8192,
    },
];

#[derive(Clone, Copy, ValueEnum)]
pub enum Agent {
    /// Hermes Agent (Nous Research)
    Hermes,
    /// OpenCode
    Opencode,
}

/// Installs (or updates) the agent, logs in through Authentik and writes its
/// configuration so that only ADNT models are reachable.
pub async fn install(agent: Agent, model: Option<String>) -> Result<()> {
    // Fail fast on a bad URL override, before touching the agent install
    api_base_url()?;
    oidc::authentik_url()?;

    match agent {
        Agent::Hermes => hermes::install(model).await,
        Agent::Opencode => {
            if model.is_some() {
                anyhow::bail!("--model is only supported for hermes");
            }
            opencode::install().await
        }
    }
}

/// Base URL of the OpenAI-compatible API.
fn api_base_url() -> Result<String> {
    Ok(format!("{}/v1", https_url("VLLM_URL", DEFAULT_VLLM_URL)?))
}

/// Logs in and returns the access token.
async fn login() -> Result<String> {
    let token = oidc::device_login().await?;
    let refresh = if token.refresh_token.is_some() {
        "yes"
    } else {
        "no"
    };
    println!(
        "{}",
        format!("✓ Logged in (refresh token: {})", refresh).green()
    );
    Ok(token.access_token)
}

/// Locates an executable in PATH (including `.exe`/`.cmd` on Windows).
fn find_in_path(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

/// Interpreter of an official installer script.
#[derive(Clone, Copy)]
enum Shell {
    Bash,
    PowerShell,
}

impl Shell {
    /// Command running the script read from stdin with `args`.
    fn command(self, args: &[&str]) -> Command {
        match self {
            Shell::Bash => {
                let mut command = Command::new("bash");
                command.args(["-s", "--"]).args(args);
                command
            }
            Shell::PowerShell => {
                let script = format!(
                    "& ([scriptblock]::Create([Console]::In.ReadToEnd())) {}",
                    args.join(" ")
                );
                let mut command = Command::new("powershell");
                command.args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    script.trim_end(),
                ]);
                command
            }
        }
    }
}

/// Downloads an official installer script and runs it. The script is fully
/// downloaded before running, so a dropped connection never executes a
/// truncated script.
async fn run_installer(url: &str, shell: Shell, args: &[&str]) -> Result<()> {
    let script = reqwest::get(url)
        .await?
        .error_for_status()
        .with_context(|| format!("Failed to download installer {}", url))?
        .text()
        .await?;

    let mut child = shell.command(args).stdin(Stdio::piped()).spawn()?;
    child
        .stdin
        .take()
        .context("Failed to open installer stdin")?
        .write_all(script.as_bytes())?;
    check_installed(child.wait()?, url)
}

fn check_installed(status: ExitStatus, installer: &str) -> Result<()> {
    if !status.success() {
        anyhow::bail!("Installer failed: {}", installer);
    }
    Ok(())
}

/// Makes `name`, staged by its installer in `bin_dir`, reachable from a
/// shell. The installers add `bin_dir` to the user PATH, but the terminal that
/// ran `adnt` (and every terminal spawned by the same parent, e.g. an IDE)
/// keeps the environment it started with: on Windows the user PATH is checked
/// and completed in the registry, then the user is told how to refresh the
/// current session.
fn ensure_on_path(name: &str, bin_dir: &Path) {
    if find_in_path(name).is_some() {
        return;
    }
    if cfg!(windows) {
        match add_to_user_path(bin_dir) {
            Ok(true) => println!(
                "{}",
                format!("Added to user PATH: {}", bin_dir.display()).green()
            ),
            Ok(false) => {}
            Err(err) => println!(
                "{}",
                format!(
                    "Could not update the user PATH ({}): add {} to it manually",
                    err,
                    bin_dir.display()
                )
                .yellow()
            ),
        }
    }
    println!(
        "\n{}",
        format!(
            "⚠ `{}` is not on the PATH of this terminal yet: open a new terminal \
             (restart the IDE if this one was opened from it), or for this session run:",
            name
        )
        .yellow()
    );
    for line in refresh_path_commands(bin_dir) {
        println!("    {}", line);
    }
}

/// Adds `dir` to the user PATH (`HKCU\Environment`) unless already there.
/// Returns whether it was added.
fn add_to_user_path(dir: &Path) -> Result<bool> {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &user_path_script(dir)])
        .output()
        .context("powershell not found")?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "added")
}

/// PowerShell script prepending `dir` to the user PATH if missing, printing
/// `added` or `present`.
fn user_path_script(dir: &Path) -> String {
    let dir = dir.display().to_string().replace('\'', "''");
    format!(
        "$dir = '{}'; \
         $user = [Environment]::GetEnvironmentVariable('Path', 'User'); \
         if (($user -split ';') -contains $dir) {{ 'present' }} \
         else {{ [Environment]::SetEnvironmentVariable('Path', ($dir + ';' + $user), 'User'); 'added' }}",
        dir
    )
}

/// Shell commands putting `dir` on the PATH of the current session.
fn refresh_path_commands(dir: &Path) -> Vec<String> {
    let dir = dir.display();
    if cfg!(windows) {
        vec![
            format!("PowerShell:  $env:Path = \"{};$env:Path\"", dir),
            format!("cmd:         set PATH={};%PATH%", dir),
        ]
    } else {
        vec![format!("export PATH=\"{}:$PATH\"", dir)]
    }
}

/// Reads a URL from the environment (or its default), requiring HTTPS: the
/// access token is sent to it.
fn https_url(var: &str, default: &str) -> Result<String> {
    require_https(var, env::var(var).unwrap_or_else(|_| default.to_string()))
}

fn require_https(var: &str, url: String) -> Result<String> {
    if !url.starts_with("https://") {
        anyhow::bail!("{} must be an https:// URL (got '{}')", var, url);
    }
    Ok(url.trim_end_matches('/').to_string())
}

fn config_home() -> Result<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .context("Could not determine config directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_require_https() {
        assert_eq!(
            require_https("VLLM_URL", "https://vllm.example/".to_string()).unwrap(),
            "https://vllm.example"
        );
        let error = require_https("VLLM_URL", "http://vllm.example".to_string()).unwrap_err();
        assert!(error.to_string().contains("VLLM_URL"));
        assert!(require_https("VLLM_URL", "vllm.example".to_string()).is_err());
    }

    #[test]
    fn test_installer_commands() {
        let bash = Shell::Bash.command(&["--skip-setup"]);
        assert_eq!(bash.get_program(), "bash");
        assert_eq!(
            bash.get_args().collect::<Vec<_>>(),
            ["-s", "--", "--skip-setup"]
        );

        let powershell = Shell::PowerShell.command(&["-SkipSetup"]);
        assert_eq!(powershell.get_program(), "powershell");
        let args: Vec<_> = powershell.get_args().collect();
        assert_eq!(
            args[..4],
            ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]
        );
        assert_eq!(
            args[4],
            "& ([scriptblock]::Create([Console]::In.ReadToEnd())) -SkipSetup"
        );
    }

    #[test]
    fn test_user_path_script() {
        let script = user_path_script(Path::new(r"C:\Users\O'Neil\AppData\Local\hermes\bin"));
        assert!(script.starts_with(r"$dir = 'C:\Users\O''Neil\AppData\Local\hermes\bin';"));
        assert!(script.contains("GetEnvironmentVariable('Path', 'User')"));
        assert!(script.contains("SetEnvironmentVariable('Path', ($dir + ';' + $user), 'User')"));
        assert!(
            !script.contains('"'),
            "no double quotes: passed through -Command"
        );
        assert!(script.contains("'present'") && script.contains("'added'"));
    }

    #[test]
    fn test_refresh_path_commands_name_the_dir() {
        let commands = refresh_path_commands(Path::new("/home/me/.local/bin"));
        assert!(!commands.is_empty());
        assert!(commands
            .iter()
            .all(|line| line.contains("/home/me/.local/bin") && line.contains("PATH")));
    }

    #[test]
    fn test_default_urls_are_https() {
        assert!(require_https("VLLM_URL", DEFAULT_VLLM_URL.to_string()).is_ok());
    }

    #[test]
    fn test_agent_models_fit_their_context() {
        for model in MODELS {
            assert!(model.output < model.context, "{}", model.id);
        }
    }
}
