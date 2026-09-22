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

use anyhow::{Context, Result};
use base64::prelude::*;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

use crate::secure_fs::private_dir;

use crate::github::GitHubClient;

#[derive(Debug, Serialize, Deserialize, Default)]
struct ToolsState {
    tools: HashMap<String, ToolInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolInfo {
    repo_url: String,
    last_commit: String,
    installed_at: String,
}

pub struct ToolManager {
    tools_dir: PathBuf,
    state_file: PathBuf,
    state: ToolsState,
    github_client: GitHubClient,
}

impl ToolManager {
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("Failed to get home directory")?;
        let tools_dir = home.join(".adnt").join("tools");
        let state_file = home.join(".adnt").join("state.json");

        private_dir(&home.join(".adnt")).context("Failed to create adnt directory")?;
        fs::create_dir_all(&tools_dir).context("Failed to create tools directory")?;
        scrub_legacy_credentials(&tools_dir);

        let state = if state_file.exists() {
            let content = fs::read_to_string(&state_file)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            ToolsState::default()
        };

        Ok(Self {
            tools_dir,
            state_file,
            state,
            github_client: GitHubClient::new(),
        })
    }

    fn save_state(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.state_file, content)?;
        Ok(())
    }

    /// Remove a tool's cached artifacts from disk and state
    pub fn remove_tool(&mut self, tool_name: &str) -> Result<()> {
        validate_tool_name(tool_name)?;
        let full_tool_name = format!("adnt-{}", tool_name);
        let tool_path = self.tools_dir.join(&full_tool_name);

        let dir_exists = tool_path.exists();
        let in_state = self.state.tools.contains_key(&full_tool_name);

        if !dir_exists && !in_state {
            println!(
                "{}",
                format!("Tool '{}' is not installed.", full_tool_name).yellow()
            );
            return Ok(());
        }

        if dir_exists {
            fs::remove_dir_all(&tool_path)
                .context(format!("Failed to remove tool directory: {:?}", tool_path))?;
        }

        if in_state {
            self.state.tools.remove(&full_tool_name);
            self.save_state()?;
        }

        println!(
            "{}",
            format!("✓ Removed '{}' from cache.", full_tool_name).green()
        );

        Ok(())
    }

    /// Build a git command authenticated with the GitHub token, if any.
    /// The token is passed as environment-scoped config so it never ends up
    /// on the command line or in the repository's `.git/config`.
    fn git_command(&self) -> Command {
        let mut cmd = Command::new("git");
        if let Some(token) = self.github_client.get_token() {
            let credentials = BASE64_STANDARD.encode(format!("x-access-token:{}", token));
            cmd.env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
                .env(
                    "GIT_CONFIG_VALUE_0",
                    format!("Authorization: Basic {}", credentials),
                );
        }
        cmd
    }

    async fn get_latest_commit(&self, repo_path: &Path) -> Result<String> {
        let output = Command::new("git")
            .args(["-C", repo_path.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .await?;

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    async fn clone_repo(&self, repo_url: &str, dest: &Path) -> Result<()> {
        let output = self
            .git_command()
            .args(["clone", repo_url, dest.to_str().unwrap()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to clone repository: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }

    async fn update_repo(&self, repo_path: &Path) -> Result<()> {
        let repo = repo_path.to_str().unwrap();

        let output = self
            .git_command()
            .args(["-C", repo, "pull", "--ff-only"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to update repository: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }

    async fn build_tool(&self, repo_path: &Path) -> Result<()> {
        let output = Command::new("cargo")
            .args(["build", "--release"])
            .current_dir(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to build tool: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }

    async fn run_binary(&self, repo_path: &Path, tool_name: &str, args: Vec<String>) -> Result<()> {
        let binary_name = format!("{}{}", tool_name, std::env::consts::EXE_SUFFIX);
        let binary_path = repo_path.join("target/release").join(binary_name);

        let status = Command::new(binary_path).args(&args).status().await?;

        if !status.success() {
            anyhow::bail!("Tool execution failed");
        }

        Ok(())
    }

    pub async fn list_available_tools(&self, verbose: bool) -> Result<()> {
        println!("{}", "Fetching available ADNT tools from GitHub...".cyan());

        let all_repos = self.github_client.list_all_repos(verbose).await?;
        let tools = self.github_client.list_adnt_tools().await?;

        if verbose {
            println!("\n{}", "All repositories from ADNTIO:".yellow().bold());
            println!("{}", "─".repeat(80).cyan());
            for repo in &all_repos {
                let is_adnt = if repo.name.starts_with("adnt-") && repo.name != "adnt" {
                    "✓ ADNT tool".green()
                } else {
                    "".dimmed()
                };
                println!("  {} {}", repo.name.cyan(), is_adnt);
            }
            println!(
                "\n{}",
                format!("Total repositories: {}", all_repos.len()).dimmed()
            );
            println!("{}", format!("ADNT tools found: {}", tools.len()).dimmed());
        }

        if tools.is_empty() {
            println!("\n{}", "No ADNT tools found.".yellow());
            return Ok(());
        }

        println!("\n{}", "Available ADNT tools:".green().bold());
        println!("{}", "─".repeat(80).cyan());

        for repo in tools {
            let tool_name = repo.name.strip_prefix("adnt-").unwrap_or(&repo.name);
            let installed = if self.tools_dir.join(&repo.name).exists() {
                "✓ installed".green()
            } else {
                "not installed".dimmed()
            };

            println!(
                "  {} {} - {}",
                tool_name.cyan().bold(),
                installed,
                repo.description.as_deref().unwrap_or("No description")
            );
            println!("    {}", repo.html_url.dimmed());
        }

        println!("\n{}", "Usage:".bold());
        println!("  adnt run <tool-name> [args]");
        println!("  adnt run <tool-name> --force [args]  (force update)");

        Ok(())
    }

    pub async fn run_tool(
        &mut self,
        tool_name: &str,
        repo_url: Option<&str>,
        args: Vec<String>,
        force_update: bool,
    ) -> Result<()> {
        validate_tool_name(tool_name)?;
        let tool_path = self.tools_dir.join(format!("adnt-{}", tool_name));
        let full_tool_name = format!("adnt-{}", tool_name);

        if !tool_path.exists() {
            println!(
                "{}",
                format!("Tool '{}' not found. Installing...", full_tool_name).yellow()
            );

            let repo_url = self.resolve_repo_url(tool_name, repo_url).await?;

            let start = Instant::now();
            let pb = spinner("{spinner:.green} {msg}");

            pb.set_message("Cloning repository...");
            self.clone_repo(&repo_url, &tool_path).await?;

            pb.set_message("Building tool...");
            self.build_tool(&tool_path).await?;

            let commit = self.get_latest_commit(&tool_path).await?;
            self.record_install(full_tool_name.clone(), repo_url, commit)?;

            pb.finish_and_clear();
            let duration = start.elapsed();
            println!(
                "{}",
                format!("✓ Installation completed in {:.2}s", duration.as_secs_f64()).green()
            );
        } else if force_update {
            // Reuse the URL recorded at install time to avoid a GitHub API call
            let repo_url = match self.state.tools.get(&full_tool_name) {
                Some(info) => info.repo_url.clone(),
                None => self.resolve_repo_url(tool_name, repo_url).await?,
            };

            let start = Instant::now();
            let pb = spinner("{spinner:.cyan} {msg}");

            pb.set_message("Force updating...");
            let previous_commit = self.get_latest_commit(&tool_path).await?;
            self.update_repo(&tool_path).await?;

            pb.set_message("Building tool...");
            self.build_tool(&tool_path).await?;

            let commit = self.get_latest_commit(&tool_path).await?;
            let up_to_date = commit == previous_commit;
            self.record_install(full_tool_name.clone(), repo_url, commit)?;

            pb.finish_and_clear();
            let duration = start.elapsed();
            let message = if up_to_date {
                "✓ Tool is up to date, rebuilt"
            } else {
                "✓ Update completed"
            };
            println!(
                "{}",
                format!("{} in {:.2}s", message, duration.as_secs_f64()).green()
            );
        }

        // Run the tool
        println!("\n{}", format!("Running {}...", full_tool_name).cyan());
        println!("{}", "─".repeat(50).cyan());

        self.run_binary(&tool_path, &full_tool_name, args).await?;

        Ok(())
    }

    async fn resolve_repo_url(&self, tool_name: &str, repo_url: Option<&str>) -> Result<String> {
        match repo_url {
            Some(url) => Ok(url.to_string()),
            None => self.github_client.get_tool_repo_url(tool_name).await,
        }
    }

    fn record_install(&mut self, name: String, repo_url: String, commit: String) -> Result<()> {
        self.state.tools.insert(
            name,
            ToolInfo {
                repo_url,
                last_commit: commit,
                installed_at: chrono::Local::now().to_rfc3339(),
            },
        );
        self.save_state()
    }

    /// Update adnt itself to the latest version from GitHub
    pub async fn self_update(&self) -> Result<()> {
        const ADNT_REPO_URL: &str = "https://github.com/ADNTIO/adnt";

        println!("{}", "Checking for adnt updates...".cyan());

        // Query the default branch (authenticated through git_command)
        let default_branch_output = self
            .git_command()
            .args(["ls-remote", "--symref", ADNT_REPO_URL, "HEAD"])
            .output()
            .await
            .context("Failed to query remote repository")?;

        if !default_branch_output.status.success() {
            anyhow::bail!(
                "Failed to query remote repository: {}",
                String::from_utf8_lossy(&default_branch_output.stderr)
            );
        }

        let ls_remote_output = String::from_utf8_lossy(&default_branch_output.stdout);

        // Parse output to get the latest commit hash
        // Format: "ref: refs/heads/main\tHEAD\n<commit_hash>\tHEAD\n"
        let remote_commit = ls_remote_output
            .lines()
            .find(|line| !line.starts_with("ref:") && line.ends_with("HEAD"))
            .and_then(|line| line.split_whitespace().next())
            .context("Failed to parse remote commit hash")?
            .to_string();

        // Get the current local commit hash by querying the installed version
        // We'll use `cargo install --list` to find the installed version, or check if we can get
        // the commit from our own binary's build info
        let local_commit = self.get_local_adnt_commit().await?;

        // Helper function to safely truncate commit hash for display
        fn short_hash(hash: &str) -> &str {
            let len = 7.min(hash.len());
            &hash[..len]
        }

        // Compare commits by checking if one is a prefix of the other
        // This handles the case where cargo install --list returns a short hash (8 chars)
        // while git ls-remote returns the full 40-character hash
        let is_same_commit = if local_commit == "unknown" {
            false
        } else {
            let min_len = local_commit.len().min(remote_commit.len());
            local_commit[..min_len] == remote_commit[..min_len]
        };

        if is_same_commit {
            println!(
                "{}",
                format!(
                    "✓ adnt is already up to date ({})",
                    short_hash(&remote_commit)
                )
                .green()
            );
            return Ok(());
        }

        println!(
            "{}",
            format!(
                "Update available: {} → {}",
                short_hash(&local_commit),
                short_hash(&remote_commit)
            )
            .yellow()
        );

        println!("{}", "Updating adnt...".cyan());

        // Update using cargo install --git
        let install_output = Command::new("cargo")
            .args(["install", "--git", ADNT_REPO_URL, "--force"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run cargo install")?;

        if !install_output.status.success() {
            anyhow::bail!(
                "Failed to update adnt: {}",
                String::from_utf8_lossy(&install_output.stderr)
            );
        }

        println!(
            "{}",
            format!("Updated to {}", short_hash(&remote_commit)).green()
        );

        Ok(())
    }

    /// Get the commit hash of the currently installed adnt
    async fn get_local_adnt_commit(&self) -> Result<String> {
        const MARKER: &str = "ADNTIO/adnt#";

        // Try to get commit from cargo install --list
        let list_output = Command::new("cargo")
            .args(["install", "--list"])
            .output()
            .await
            .context("Failed to run cargo install --list")?;

        if list_output.status.success() {
            let list_str = String::from_utf8_lossy(&list_output.stdout);
            // Look for adnt entry, format is typically:
            // adnt v0.1.0 (https://github.com/ADNTIO/adnt#<commit>):
            for line in list_str.lines() {
                if line.starts_with("adnt ") && line.contains(MARKER) {
                    // Extract commit hash after the # and before the ):
                    if let Some(hash_start) = line.find(MARKER) {
                        let after_hash = &line[hash_start + MARKER.len()..];
                        if let Some(end) = after_hash.find(')') {
                            return Ok(after_hash[..end].to_string());
                        }
                    }
                }
            }
        }

        // If we can't determine the local commit, return a placeholder that will trigger an update
        // This handles the case where adnt was installed from a local path or other source
        Ok("unknown".to_string())
    }

    #[cfg(test)]
    /// Creates a new ToolManager with custom paths for testing purposes.
    /// This bypasses the default home directory paths to allow isolated testing.
    fn new_with_paths(tools_dir: PathBuf, state_file: PathBuf) -> Result<Self> {
        fs::create_dir_all(&tools_dir).context("Failed to create tools directory")?;

        let state = if state_file.exists() {
            let content = fs::read_to_string(&state_file)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            ToolsState::default()
        };

        Ok(Self {
            tools_dir,
            state_file,
            state,
            github_client: GitHubClient::new(),
        })
    }
}

/// Rejects names that could escape the tools directory (e.g. `../..`).
fn validate_tool_name(tool_name: &str) -> Result<()> {
    let valid = !tool_name.is_empty()
        && !tool_name.starts_with('.')
        && tool_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid {
        anyhow::bail!("Invalid tool name '{}'", tool_name);
    }
    Ok(())
}

/// Older versions embedded the GitHub token in the remote URL of every clone:
/// strip it so it no longer sits in clear in `.git/config`.
fn scrub_legacy_credentials(tools_dir: &Path) {
    let Ok(entries) = fs::read_dir(tools_dir) else {
        return;
    };
    for repo in entries.flatten().map(|entry| entry.path()) {
        let has_credentials = fs::read_to_string(repo.join(".git/config"))
            .is_ok_and(|config| config.contains("@github.com"));
        if !has_credentials {
            continue;
        }
        let Ok(output) = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["remote", "get-url", "origin"])
            .output()
        else {
            continue;
        };
        let current_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(clean_url) = strip_github_credentials(&current_url) {
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["remote", "set-url", "origin", &clean_url])
                .output();
        }
    }
}

/// Returns the URL without credentials if it is a GitHub HTTPS URL carrying any.
fn strip_github_credentials(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let (userinfo, path) = rest.split_once('@')?;
    if userinfo.contains('/') || !path.starts_with("github.com/") {
        return None;
    }
    Some(format!("https://{}", path))
}

fn spinner(template: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template(template).unwrap());
    pb
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn load_saved_state(state_file: &Path) -> ToolsState {
        let content = fs::read_to_string(state_file).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn test_remove_tool_not_installed() {
        let temp_dir = tempdir().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        let state_file = temp_dir.path().join("state.json");

        let mut manager = ToolManager::new_with_paths(tools_dir, state_file).unwrap();

        // Should succeed without error when tool doesn't exist
        manager.remove_tool("nonexistent").unwrap();
    }

    #[test]
    fn test_remove_tool_installed() {
        let temp_dir = tempdir().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        let state_file = temp_dir.path().join("state.json");

        let mut manager =
            ToolManager::new_with_paths(tools_dir.clone(), state_file.clone()).unwrap();

        // Create a fake tool directory
        let tool_dir = tools_dir.join("adnt-test-app");
        fs::create_dir_all(&tool_dir).unwrap();
        fs::write(tool_dir.join("dummy.txt"), "test content").unwrap();

        // Add tool to state
        manager.state.tools.insert(
            "adnt-test-app".to_string(),
            ToolInfo {
                repo_url: "https://github.com/test/repo".to_string(),
                last_commit: "abc123".to_string(),
                installed_at: "2024-01-01T00:00:00Z".to_string(),
            },
        );

        // Remove the tool
        manager.remove_tool("test-app").unwrap();

        // Verify directory is removed
        assert!(!tool_dir.exists());

        // Verify in-memory state is updated
        assert!(!manager.state.tools.contains_key("adnt-test-app"));

        // Verify state is persisted to disk
        assert!(!load_saved_state(&state_file)
            .tools
            .contains_key("adnt-test-app"));
    }

    #[test]
    fn test_remove_tool_in_state_but_no_directory() {
        let temp_dir = tempdir().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        let state_file = temp_dir.path().join("state.json");

        let mut manager = ToolManager::new_with_paths(tools_dir, state_file.clone()).unwrap();

        // Add tool to state but don't create directory
        manager.state.tools.insert(
            "adnt-orphan-app".to_string(),
            ToolInfo {
                repo_url: "https://github.com/test/repo".to_string(),
                last_commit: "abc123".to_string(),
                installed_at: "2024-01-01T00:00:00Z".to_string(),
            },
        );

        // Remove the tool - should clean up state even without directory
        manager.remove_tool("orphan-app").unwrap();

        // Verify in-memory state is updated
        assert!(!manager.state.tools.contains_key("adnt-orphan-app"));

        // Verify state is persisted to disk
        assert!(!load_saved_state(&state_file)
            .tools
            .contains_key("adnt-orphan-app"));
    }

    #[test]
    fn test_strip_github_credentials() {
        assert_eq!(
            strip_github_credentials("https://oauth2:ghp_secret@github.com/ADNTIO/adnt-x.git"),
            Some("https://github.com/ADNTIO/adnt-x.git".to_string())
        );
        assert_eq!(
            strip_github_credentials("https://github.com/ADNTIO/adnt-x.git"),
            None
        );
        assert_eq!(
            strip_github_credentials("https://user@gitlab.com/ADNTIO/adnt-x.git"),
            None
        );
        assert_eq!(
            strip_github_credentials("git@github.com:ADNTIO/adnt-x.git"),
            None
        );
    }

    #[test]
    fn test_validate_tool_name() {
        assert!(validate_tool_name("net-edge").is_ok());
        assert!(validate_tool_name("my_tool.v2").is_ok());
        assert!(validate_tool_name("").is_err());
        assert!(validate_tool_name("..").is_err());
        assert!(validate_tool_name("net-edge/../../..").is_err());
    }

    #[test]
    fn test_remove_tool_rejects_path_traversal() {
        let temp_dir = tempdir().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        let state_file = temp_dir.path().join("state.json");
        let victim = temp_dir.path().join("victim");
        fs::create_dir_all(&victim).unwrap();

        let mut manager = ToolManager::new_with_paths(tools_dir, state_file).unwrap();

        assert!(manager.remove_tool("x/../../victim").is_err());
        assert!(victim.exists());
    }

    #[test]
    fn test_record_install_persists_state() {
        let temp_dir = tempdir().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        let state_file = temp_dir.path().join("state.json");

        let mut manager =
            ToolManager::new_with_paths(tools_dir.clone(), state_file.clone()).unwrap();
        manager
            .record_install(
                "adnt-demo".to_string(),
                "https://github.com/ADNTIO/adnt-demo.git".to_string(),
                "abc123".to_string(),
            )
            .unwrap();

        let saved = load_saved_state(&state_file);
        let info = &saved.tools["adnt-demo"];
        assert_eq!(info.repo_url, "https://github.com/ADNTIO/adnt-demo.git");
        assert_eq!(info.last_commit, "abc123");
        assert!(chrono::DateTime::parse_from_rfc3339(&info.installed_at).is_ok());

        // A new manager reads the recorded install back
        let reloaded = ToolManager::new_with_paths(tools_dir, state_file).unwrap();
        assert!(reloaded.state.tools.contains_key("adnt-demo"));
    }

    #[test]
    fn test_scrub_legacy_credentials() {
        let temp_dir = tempdir().unwrap();
        let git = |repo: &Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        let legacy = temp_dir.path().join("adnt-legacy");
        let clean = temp_dir.path().join("adnt-clean");
        for (repo, url) in [
            (
                &legacy,
                "https://oauth2:ghp_secret@github.com/ADNTIO/adnt-legacy.git",
            ),
            (&clean, "https://github.com/ADNTIO/adnt-clean.git"),
        ] {
            fs::create_dir_all(repo).unwrap();
            git(repo, &["init", "-q"]);
            git(repo, &["remote", "add", "origin", url]);
        }
        // Not a git repository: ignored
        fs::create_dir_all(temp_dir.path().join("not-a-repo")).unwrap();

        scrub_legacy_credentials(temp_dir.path());

        assert_eq!(
            git(&legacy, &["remote", "get-url", "origin"]),
            "https://github.com/ADNTIO/adnt-legacy.git"
        );
        assert!(!fs::read_to_string(legacy.join(".git/config"))
            .unwrap()
            .contains("ghp_secret"));
        assert_eq!(
            git(&clean, &["remote", "get-url", "origin"]),
            "https://github.com/ADNTIO/adnt-clean.git"
        );
    }
}
