use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::time::Duration;

const GITHUB_API_BASE: &str = "https://api.github.com";
const ADNT_ORG: &str = "ADNTIO";
const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
// ADNT Tools Manager OAuth App - supports Device Flow
// Can be overridden via env var ADNT_GITHUB_CLIENT_ID
const DEFAULT_CLIENT_ID: &str = "Ov23lihUc287puYq0CK1";

#[derive(Debug, Deserialize)]
pub struct Repository {
    pub name: String,
    pub clone_url: String,
    pub description: Option<String>,
    pub html_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AdntConfig {
    github_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
pub struct TokenInfo {
    pub scopes: Option<String>,
    pub username: Option<String>,
}

pub struct GitHubClient {
    client: reqwest::Client,
    token: Option<String>,
}

impl GitHubClient {
    pub fn new() -> Self {
        let token = Self::load_github_token();

        // Don't set default headers - we'll add auth per request if token exists
        let client = reqwest::Client::builder()
            .user_agent("adnt-tool-manager")
            .build()
            .expect("Failed to create HTTP client");

        Self { client, token }
    }

    fn load_github_token() -> Option<String> {
        // Priority 1: Environment variable
        if let Ok(token) = env::var("GITHUB_TOKEN") {
            return Some(token);
        }

        // Priority 2: ADNT config file
        if let Some(home) = dirs::home_dir() {
            let config_path = home.join(".config/adnt/config.json");
            if config_path.exists() {
                if let Ok(content) = fs::read_to_string(&config_path) {
                    if let Ok(config) = serde_json::from_str::<AdntConfig>(&content) {
                        if config.github_token.is_some() {
                            return config.github_token;
                        }
                    }
                }
            }
        }

        // Priority 3: Try gh CLI token
        if let Ok(output) = std::process::Command::new("gh")
            .args(&["auth", "token"])
            .output()
        {
            if output.status.success() {
                let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !token.is_empty() {
                    return Some(token);
                }
            }
        }

        None
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    pub fn get_token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Check token scopes and permissions
    pub async fn verify_token(&self) -> Result<TokenInfo> {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No token configured"))?;

        let response = self
            .client
            .get(&format!("{}/user", GITHUB_API_BASE))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;

        let scopes = response
            .headers()
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let user: serde_json::Value = response.json().await?;

        Ok(TokenInfo {
            scopes,
            username: user["login"].as_str().map(|s| s.to_string()),
        })
    }

    pub fn save_token(token: &str) -> Result<()> {
        let home = dirs::home_dir().context("Failed to get home directory")?;
        let config_dir = home.join(".config/adnt");
        let config_path = config_dir.join("config.json");

        fs::create_dir_all(&config_dir).context("Failed to create config directory")?;

        let config = AdntConfig {
            github_token: Some(token.to_string()),
        };

        let content = serde_json::to_string_pretty(&config)?;
        fs::write(&config_path, content)?;

        Ok(())
    }

    /// Initiate GitHub Device Flow OAuth
    pub async fn device_flow_login() -> Result<String> {
        use colored::Colorize;

        let client = reqwest::Client::new();
        let client_id =
            env::var("ADNT_GITHUB_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());

        // Step 1: Request device code
        let device_response: DeviceCodeResponse = client
            .post(GITHUB_DEVICE_CODE_URL)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client_id.as_str()),
                ("scope", "repo read:org"),
            ])
            .send()
            .await?
            .json()
            .await?;

        // Step 2: Display user code and verification URL
        println!("\n{}", "GitHub Authentication".cyan().bold());
        println!("{}", "─".repeat(60).cyan());
        println!("\n{}", "Please visit:".bold());
        println!("  {}", device_response.verification_uri.green().underline());
        println!("\n{}", "And enter code:".bold());
        println!("  {}", device_response.user_code.yellow().bold());
        println!("\n{}", "Waiting for authentication...".dimmed());

        // Try to open browser automatically
        if let Err(_) = open::that(&device_response.verification_uri) {
            println!("{}", "  (Could not open browser automatically)".dimmed());
        }

        // Step 3: Poll for access token
        let interval = Duration::from_secs(device_response.interval);
        let mut attempts = 0;
        let max_attempts = device_response.expires_in / device_response.interval;

        loop {
            if attempts >= max_attempts {
                anyhow::bail!("Authentication timeout - device code expired");
            }

            tokio::time::sleep(interval).await;

            let token_response: AccessTokenResponse = client
                .post(GITHUB_ACCESS_TOKEN_URL)
                .header("Accept", "application/json")
                .form(&[
                    ("client_id", client_id.as_str()),
                    ("device_code", &device_response.device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await?
                .json()
                .await?;

            if let Some(token) = token_response.access_token {
                println!("\n{}", "✓ Authentication successful!".green().bold());
                return Ok(token);
            }

            if let Some(error) = token_response.error {
                match error.as_str() {
                    "authorization_pending" => {
                        // Continue polling
                        attempts += 1;
                    }
                    "slow_down" => {
                        // Slow down polling
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        attempts += 1;
                    }
                    "expired_token" => {
                        anyhow::bail!("Device code expired. Please try again.");
                    }
                    "access_denied" => {
                        anyhow::bail!("Access denied by user.");
                    }
                    _ => {
                        anyhow::bail!("Authentication error: {}", error);
                    }
                }
            }
        }
    }

    async fn fetch_all_repos(&self) -> Result<Vec<Repository>> {
        let mut all_repos = Vec::new();
        let mut page = 1;
        let per_page = 100;

        loop {
            let url = format!(
                "{}/orgs/{}/repos?per_page={}&page={}&sort=updated",
                GITHUB_API_BASE, ADNT_ORG, per_page, page
            );

            let mut request = self.client.get(&url);

            // Add authentication if token is available
            if let Some(token) = &self.token {
                request = request.header("Authorization", format!("Bearer {}", token));
            }

            let response = request
                .send()
                .await
                .context("Failed to fetch repositories from GitHub")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("GitHub API request failed: {} - {}", status, body);
            }

            let repos: Vec<Repository> = response
                .json()
                .await
                .context("Failed to parse GitHub API response")?;

            let fetched_count = repos.len();
            all_repos.extend(repos);

            // If we got fewer than per_page, we're done
            if fetched_count < per_page {
                break;
            }

            page += 1;
        }

        Ok(all_repos)
    }

    pub async fn list_all_repos(&self, _verbose: bool) -> Result<Vec<Repository>> {
        self.fetch_all_repos().await
    }

    pub async fn list_adnt_tools(&self) -> Result<Vec<Repository>> {
        let all_repos = self.fetch_all_repos().await?;

        // Filter repositories that start with "adnt-"
        let adnt_tools: Vec<Repository> = all_repos
            .into_iter()
            .filter(|repo| repo.name.starts_with("adnt-") && repo.name != "adnt")
            .collect();

        Ok(adnt_tools)
    }

    pub async fn get_tool_repo_url(&self, tool_name: &str) -> Result<String> {
        let full_name = format!("adnt-{}", tool_name);
        let tools = self.list_adnt_tools().await?;

        tools
            .into_iter()
            .find(|repo| repo.name == full_name)
            .map(|repo| repo.clone_url)
            .context(format!(
                "Tool '{}' not found in ADNTIO repositories",
                full_name
            ))
    }
}
