use clap::{Parser, Subcommand};
use anyhow::Result;
use colored::Colorize;

mod tool_manager;
mod github;

use tool_manager::ToolManager;
use github::GitHubClient;

#[derive(Parser)]
#[command(name = "adnt")]
#[command(about = "ADNT tools manager", long_about = None)]
struct Cli {
    /// Force update even if tool is up to date
    #[arg(short, long, global = true)]
    force: bool,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all available ADNT tools from GitHub
    List,

    /// Run any ADNT tool by name (e.g., adnt run net-edge [args])
    Run {
        /// Tool name (without 'adnt-' prefix)
        tool: String,

        /// Arguments to pass to the tool
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "ARGS")]
        args: Vec<String>,
    },

    /// Remove an app from cache to force clean reinstall or free disk space
    Rm {
        /// Tool name (without 'adnt-' prefix)
        tool: String,
    },

    /// Configure ADNT settings
    Config {
        #[command(subcommand)]
        subcommand: ConfigCommands,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Login to GitHub interactively (OAuth Device Flow)
    Login,

    /// Set GitHub token for API authentication
    SetToken {
        /// GitHub personal access token (leave empty to enter securely)
        token: Option<String>,
    },

    /// Show current configuration status
    Status,
}

async fn handle_config_command(subcommand: ConfigCommands) -> Result<()> {
    match subcommand {
        ConfigCommands::Login => {
            use std::env;

            // Warn if using default client ID
            if env::var("ADNT_GITHUB_CLIENT_ID").is_err() {
                println!("{}", "⚠ Warning: Using default GitHub CLI OAuth client".yellow());
                println!("{}", "  This may not grant access to private repositories.".yellow());
                println!("\n{}", "For private repo access, create a custom OAuth App:".dimmed());
                println!("{}", "  See QUICK_FIX.md or OAUTH_SETUP.md for instructions".dimmed());
                println!("{}", "  Or set ADNT_GITHUB_CLIENT_ID environment variable".dimmed());
                println!();
            }

            println!("{}", "Starting GitHub OAuth login...".cyan());
            let token = GitHubClient::device_flow_login().await?;
            GitHubClient::save_token(&token)?;
            println!("{}", "✓ Token saved successfully".green());
            println!("{}", format!("  Config file: ~/.config/adnt/config.json").dimmed());

            // Verify token scopes immediately
            println!("\n{}", "Verifying token scopes...".dimmed());
            let client = GitHubClient::new();
            match client.verify_token().await {
                Ok(info) => {
                    if let Some(scopes) = info.scopes {
                        if scopes.is_empty() {
                            println!("{}", "✗ Token has no scopes - private repos won't be accessible!".red());
                            println!("\n{}", "To fix this, you must create a custom OAuth App:".yellow().bold());
                            println!("{}", "  1. See QUICK_FIX.md for step-by-step guide".yellow());
                            println!("{}", "  2. Or create manual token: adnt config set-token".yellow());
                        } else {
                            println!("{}", format!("✓ Token scopes: {}", scopes).green());
                        }
                    }
                }
                Err(_) => {
                    println!("{}", "Could not verify token scopes".dimmed());
                }
            }
        }
        ConfigCommands::SetToken { token } => {
            let token = if let Some(t) = token {
                t
            } else {
                // Read token securely from stdin
                use std::io::{self, Write};
                print!("Enter GitHub token: ");
                io::stdout().flush()?;

                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                input.trim().to_string()
            };

            if token.is_empty() {
                println!("{}", "Error: Token cannot be empty".red());
                return Ok(());
            }

            GitHubClient::save_token(&token)?;
            println!("{}", "✓ GitHub token saved successfully".green());
            println!("{}", format!("  Config file: ~/.config/adnt/config.json").dimmed());
        }
        ConfigCommands::Status => {
            let client = GitHubClient::new();

            println!("{}", "ADNT Configuration Status".cyan().bold());
            println!("{}", "─".repeat(50).cyan());

            if client.has_token() {
                println!("{} {}", "GitHub Token:".bold(), "✓ Configured".green());

                // Try to determine source
                if std::env::var("GITHUB_TOKEN").is_ok() {
                    println!("{} {}", "  Source:".dimmed(), "Environment variable (GITHUB_TOKEN)".dimmed());
                } else if let Some(home) = dirs::home_dir() {
                    let config_path = home.join(".config/adnt/config.json");
                    if config_path.exists() {
                        println!("{} {}", "  Source:".dimmed(), "Config file (~/.config/adnt/config.json)".dimmed());
                    }
                } else if std::process::Command::new("gh").args(&["auth", "status"]).output().is_ok() {
                    println!("{} {}", "  Source:".dimmed(), "GitHub CLI (gh)".dimmed());
                }

                // Verify token and show scopes
                println!("\n{}", "Verifying token...".dimmed());
                match client.verify_token().await {
                    Ok(info) => {
                        if let Some(username) = info.username {
                            println!("{} {}", "  User:".bold(), username.green());
                        }
                        if let Some(scopes) = info.scopes {
                            if scopes.is_empty() {
                                println!("{} {}", "  Scopes:".bold(), "(none)".red());
                            } else {
                                println!("{} {}", "  Scopes:".bold(), scopes.cyan());
                            }

                            // Check for required scopes
                            let has_repo = scopes.contains("repo") || scopes.contains("public_repo");
                            let has_org = scopes.contains("read:org");

                            if scopes.is_empty() {
                                println!("\n{}", "✗ Token has NO scopes - it cannot access any repositories!".red().bold());
                            } else {
                                if !has_repo {
                                    println!("\n{}", "⚠ Warning: Missing 'repo' scope - private repositories not accessible".yellow());
                                }
                                if !has_org {
                                    println!("{}", "⚠ Warning: Missing 'read:org' scope - organization access limited".yellow());
                                }
                            }

                            if !has_repo || !has_org {
                                println!("\n{}", "To access private repositories, authenticate with correct scopes:".bold());
                                println!("{}", "  Option 1 (Recommended): adnt config login".green());
                                println!("{}", "  Option 2: Create token manually:".dimmed());
                                println!("{}", "    - Go to https://github.com/settings/tokens".dimmed());
                                println!("{}", "    - Generate token with 'repo' and 'read:org' scopes".dimmed());
                                println!("{}", "    - Run: adnt config set-token".dimmed());
                                println!("\n{}", "For custom OAuth app (better for orgs):".dimmed());
                                println!("{}", "  See OAUTH_SETUP.md for instructions".dimmed());
                            }
                        } else {
                            println!("{}", "  Scopes: Unknown (classic token or legacy format)".dimmed());
                        }
                    }
                    Err(e) => {
                        println!("{} {}", "  Verification failed:".red(), e.to_string().dimmed());
                    }
                }
            } else {
                println!("{} {}", "GitHub Token:".bold(), "✗ Not configured".yellow());
                println!("\n{}", "You can authenticate using:".dimmed());
                println!("{}", "  adnt config login           (recommended - interactive OAuth)".dimmed());
                println!("{}", "  adnt config set-token       (manual token entry)".dimmed());
                println!("\n{}", "Or use one of these methods:".dimmed());
                println!("{}", "  1. Set GITHUB_TOKEN environment variable".dimmed());
                println!("{}", "  2. Authenticate with GitHub CLI: gh auth login".dimmed());
                println!("\n{}", "Note: A token is not required for public repositories.".yellow());
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut manager = ToolManager::new()?;

    match cli.command {
        Commands::List => {
            manager.list_available_tools(cli.verbose).await?;
        }
        Commands::Run { tool, args } => {
            manager.run_tool(&tool, None, args, cli.force).await?;
        }
        Commands::Rm { tool } => {
            manager.remove_tool(&tool)?;
        }
        Commands::Config { subcommand } => {
            handle_config_command(subcommand).await?;
        }
    }

    Ok(())
}
