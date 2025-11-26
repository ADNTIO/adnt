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
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

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

    fn save_state(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.state_file, content)?;
        Ok(())
    }

    /// Remove a tool's cached artifacts from disk and state
    pub fn remove_tool(&mut self, tool_name: &str) -> Result<()> {
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

    /// Convert a GitHub HTTPS URL to an authenticated URL using the OAuth token
    fn get_authenticated_url(&self, repo_url: &str) -> String {
        if let Some(token) = self.github_client.get_token() {
            // Convert https://github.com/... to https://oauth2:TOKEN@github.com/...
            if repo_url.starts_with("https://github.com/") {
                return repo_url.replace(
                    "https://github.com/",
                    &format!("https://oauth2:{}@github.com/", token),
                );
            }
        }
        // Return original URL if no token or not a GitHub HTTPS URL
        repo_url.to_string()
    }

    async fn get_latest_commit(&self, repo_path: &Path) -> Result<String> {
        let output = Command::new("git")
            .args(["-C", repo_path.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .await?;

        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    async fn get_remote_commit(&self, repo_url: &str) -> Result<String> {
        // Use authenticated URL if we have a token
        let remote_url = self.get_authenticated_url(repo_url);

        let output = Command::new("git")
            .args(["ls-remote", &remote_url, "HEAD"])
            .output()
            .await?;

        let stdout = String::from_utf8(output.stdout)?;
        let commit = stdout
            .split_whitespace()
            .next()
            .context("No commit found")?;
        Ok(commit.to_string())
    }

    async fn clone_repo(&self, repo_url: &str, dest: &Path) -> Result<()> {
        // Use authenticated URL if we have a token
        let clone_url = self.get_authenticated_url(repo_url);

        let output = Command::new("git")
            .args(["clone", &clone_url, dest.to_str().unwrap()])
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
        // If we have a token, update the remote URL to use authentication
        if self.github_client.has_token() {
            // Get current remote URL
            let output = Command::new("git")
                .args([
                    "-C",
                    repo_path.to_str().unwrap(),
                    "remote",
                    "get-url",
                    "origin",
                ])
                .output()
                .await?;

            if output.status.success() {
                let current_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let auth_url = self.get_authenticated_url(&current_url);

                // Update remote URL with authentication
                Command::new("git")
                    .args([
                        "-C",
                        repo_path.to_str().unwrap(),
                        "remote",
                        "set-url",
                        "origin",
                        &auth_url,
                    ])
                    .output()
                    .await?;
            }
        }

        let output = Command::new("git")
            .args(["-C", repo_path.to_str().unwrap(), "pull", "--ff-only"])
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

    async fn run_binary(
        &self,
        repo_path: &Path,
        tool_name: &str,
        args: Vec<String>,
    ) -> Result<()> {
        let binary_path = repo_path.join("target/release").join(tool_name);

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
        let tool_path = self.tools_dir.join(format!("adnt-{}", tool_name));
        let full_tool_name = format!("adnt-{}", tool_name);

        // Get repo URL from GitHub if not provided
        let repo_url = if let Some(url) = repo_url {
            url.to_string()
        } else {
            self.github_client.get_tool_repo_url(tool_name).await?
        };

        if !tool_path.exists() {
            println!(
                "{}",
                format!("Tool '{}' not found. Installing...", full_tool_name).yellow()
            );

            let start = Instant::now();
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.green} {msg}")
                    .unwrap(),
            );

            pb.set_message("Cloning repository...");
            self.clone_repo(&repo_url, &tool_path).await?;

            pb.set_message("Building tool...");
            self.build_tool(&tool_path).await?;

            let commit = self.get_latest_commit(&tool_path).await?;

            self.state.tools.insert(
                full_tool_name.clone(),
                ToolInfo {
                    repo_url: repo_url.clone(),
                    last_commit: commit,
                    installed_at: chrono::Local::now().to_rfc3339(),
                },
            );
            self.save_state()?;

            pb.finish_and_clear();
            let duration = start.elapsed();
            println!(
                "{}",
                format!("✓ Installation completed in {:.2}s", duration.as_secs_f64()).green()
            );
        } else {
            // Check for updates
            if force_update {
                let pb: ProgressBar = ProgressBar::new_spinner();
                pb.set_style(
                    ProgressStyle::default_spinner()
                        .template("{spinner:.cyan} {msg}")
                        .unwrap(),
                );
                pb.set_message("Checking for updates...");

                let local_commit = self.get_latest_commit(&tool_path).await?;
                let remote_commit = self.get_remote_commit(&repo_url).await?;

                if local_commit != remote_commit {
                    if force_update {
                        pb.set_message("Force updating...");
                    } else {
                        pb.set_message("Update available. Updating...");
                    }
                    let start = Instant::now();

                    self.update_repo(&tool_path).await?;
                    self.build_tool(&tool_path).await?;

                    self.state.tools.insert(
                        full_tool_name.clone(),
                        ToolInfo {
                            repo_url: repo_url.clone(),
                            last_commit: remote_commit,
                            installed_at: chrono::Local::now().to_rfc3339(),
                        },
                    );
                    self.save_state()?;

                    pb.finish_and_clear();
                    let duration = start.elapsed();
                    println!(
                        "{}",
                        format!("✓ Update completed in {:.2}s", duration.as_secs_f64()).green()
                    );
                }
            }
        }

        // Run the tool
        println!("\n{}", format!("Running {}...", full_tool_name).cyan());
        println!("{}", "─".repeat(50).cyan());

        self.run_binary(&tool_path, &full_tool_name, args).await?;

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_remove_tool_not_installed() {
        let temp_dir = tempdir().unwrap();
        let tools_dir = temp_dir.path().join("tools");
        let state_file = temp_dir.path().join("state.json");

        let mut manager = ToolManager::new_with_paths(tools_dir, state_file).unwrap();

        // Should succeed without error when tool doesn't exist
        let result = manager.remove_tool("nonexistent");
        assert!(result.is_ok());
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
        let result = manager.remove_tool("test-app");
        assert!(result.is_ok());

        // Verify directory is removed
        assert!(!tool_dir.exists());

        // Verify in-memory state is updated
        assert!(!manager.state.tools.contains_key("adnt-test-app"));

        // Verify state is persisted to disk
        let state_content = fs::read_to_string(&state_file).unwrap();
        let saved_state: ToolsState = serde_json::from_str(&state_content).unwrap();
        assert!(!saved_state.tools.contains_key("adnt-test-app"));
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
        let result = manager.remove_tool("orphan-app");
        assert!(result.is_ok());

        // Verify in-memory state is updated
        assert!(!manager.state.tools.contains_key("adnt-orphan-app"));

        // Verify state is persisted to disk
        let state_content = fs::read_to_string(&state_file).unwrap();
        let saved_state: ToolsState = serde_json::from_str(&state_content).unwrap();
        assert!(!saved_state.tools.contains_key("adnt-orphan-app"));
    }
}
