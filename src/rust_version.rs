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
use std::cmp::Ordering;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Represents a semantic version (major.minor.patch)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Parse a version string like "1.70.0" or "1.70"
    pub fn parse(version_str: &str) -> Result<Self> {
        let version_str = version_str.trim();
        let parts: Vec<&str> = version_str.split('.').collect();

        if parts.is_empty() || parts.len() > 3 {
            anyhow::bail!("Invalid version format: {}", version_str);
        }

        let major = parts[0]
            .parse::<u32>()
            .context(format!("Invalid major version: {}", parts[0]))?;

        let minor = if parts.len() > 1 {
            parts[1]
                .parse::<u32>()
                .context(format!("Invalid minor version: {}", parts[1]))?
        } else {
            0
        };

        let patch = if parts.len() > 2 {
            // Handle cases like "1.70.0-nightly" - only take the numeric part
            let patch_str = parts[2].split('-').next().unwrap_or(parts[2]);
            patch_str
                .parse::<u32>()
                .context(format!("Invalid patch version: {}", patch_str))?
        } else {
            0
        };

        Ok(Version {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.major.cmp(&other.major) {
            Ordering::Equal => match self.minor.cmp(&other.minor) {
                Ordering::Equal => self.patch.cmp(&other.patch),
                ord => ord,
            },
            ord => ord,
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Get the currently installed Rust version by running `rustc --version`
pub fn get_installed_rust_version() -> Result<Version> {
    let output = Command::new("rustc")
        .args(["--version"])
        .output()
        .context("Failed to run 'rustc --version'. Is Rust installed?")?;

    if !output.status.success() {
        anyhow::bail!(
            "rustc --version failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let version_output = String::from_utf8_lossy(&output.stdout);
    // Output format: "rustc 1.70.0 (90c541806 2023-05-31)"
    let version_str = version_output
        .split_whitespace()
        .nth(1)
        .context("Could not parse rustc version output")?;

    Version::parse(version_str)
}

/// Read the minimum required Rust version from a tool's Cargo.toml
/// Looks for `rust-version` field under `[package]`
pub fn get_required_rust_version(tool_path: &Path) -> Result<Option<Version>> {
    let cargo_toml_path = tool_path.join("Cargo.toml");

    if !cargo_toml_path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&cargo_toml_path)
        .context(format!("Failed to read {:?}", cargo_toml_path))?;

    // Parse Cargo.toml using the toml crate
    let toml_table: toml::Table = content
        .parse()
        .context(format!("Failed to parse {:?} as TOML", cargo_toml_path))?;

    // Look for package.rust-version field
    if let Some(rust_version) = toml_table
        .get("package")
        .and_then(|p| p.get("rust-version"))
        .and_then(|v| v.as_str())
    {
        return Ok(Some(Version::parse(rust_version)?));
    }

    Ok(None)
}

/// Check if the installed Rust version meets the minimum requirement
/// Returns Ok(()) if version is sufficient, or an error with upgrade prompt if not
pub fn check_rust_version(tool_path: &Path, tool_name: &str) -> Result<()> {
    let installed_version = get_installed_rust_version()?;

    if let Some(required_version) = get_required_rust_version(tool_path)? {
        if installed_version < required_version {
            println!(
                "\n{}",
                format!(
                    "⚠ Rust version {} is required by '{}', but you have {}",
                    required_version, tool_name, installed_version
                )
                .yellow()
                .bold()
            );

            // Ask user if they want to upgrade
            print!(
                "{}",
                "Would you like to update Rust using 'rustup update'? [Y/n] "
                    .cyan()
                    .bold()
            );
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim().to_lowercase();

            if input.is_empty() || input == "y" || input == "yes" {
                println!("{}", "Updating Rust toolchain...".cyan());

                let status = Command::new("rustup")
                    .args(["update"])
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status()
                    .context("Failed to run 'rustup update'. Is rustup installed?")?;

                if !status.success() {
                    anyhow::bail!("rustup update failed");
                }

                // Re-check version after update
                let new_version = get_installed_rust_version()?;
                if new_version < required_version {
                    anyhow::bail!(
                        "After update, Rust version {} still does not meet the requirement of {}. \
                         Please install Rust {} or later manually.",
                        new_version,
                        required_version,
                        required_version
                    );
                }

                println!(
                    "{}",
                    format!("✓ Rust updated to version {}", new_version).green()
                );
            } else {
                anyhow::bail!(
                    "Rust version {} is required. Please run 'rustup update' manually or install Rust {} or later.",
                    required_version,
                    required_version
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_version_parse_full() {
        let v = Version::parse("1.70.0").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 70);
        assert_eq!(v.patch, 0);
    }

    #[test]
    fn test_version_parse_two_parts() {
        let v = Version::parse("1.70").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 70);
        assert_eq!(v.patch, 0);
    }

    #[test]
    fn test_version_parse_with_nightly() {
        let v = Version::parse("1.80.0-nightly").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 80);
        assert_eq!(v.patch, 0);
    }

    #[test]
    fn test_version_parse_with_whitespace() {
        let v = Version::parse("  1.75.0  ").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 75);
        assert_eq!(v.patch, 0);
    }

    #[test]
    fn test_version_parse_invalid() {
        assert!(Version::parse("invalid").is_err());
        assert!(Version::parse("1.a.0").is_err());
    }

    #[test]
    fn test_version_comparison() {
        let v1 = Version::parse("1.70.0").unwrap();
        let v2 = Version::parse("1.70.1").unwrap();
        let v3 = Version::parse("1.71.0").unwrap();
        let v4 = Version::parse("2.0.0").unwrap();
        let v5 = Version::parse("1.70.0").unwrap();

        assert!(v1 < v2);
        assert!(v2 < v3);
        assert!(v3 < v4);
        assert!(v1 == v5);
        assert!(v4 > v1);
    }

    #[test]
    fn test_version_display() {
        let v = Version::parse("1.70.5").unwrap();
        assert_eq!(format!("{}", v), "1.70.5");
    }

    #[test]
    fn test_get_installed_rust_version() {
        // This test will only pass if Rust is installed
        let result = get_installed_rust_version();
        assert!(result.is_ok());
        let version = result.unwrap();
        // Rust version should be at least 1.0.0
        assert!(version.major >= 1);
    }

    #[test]
    fn test_get_required_rust_version_with_rust_version() {
        let temp_dir = tempdir().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");

        fs::write(
            &cargo_toml,
            "[package]\nname = \"test-tool\"\nversion = \"0.1.0\"\nedition = \"2021\"\nrust-version = \"1.70.0\"\n",
        )
        .unwrap();

        let result = get_required_rust_version(temp_dir.path()).unwrap();
        assert!(result.is_some());
        let version = result.unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 70);
        assert_eq!(version.patch, 0);
    }

    #[test]
    fn test_get_required_rust_version_without_rust_version() {
        let temp_dir = tempdir().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");

        fs::write(
            &cargo_toml,
            "[package]\nname = \"test-tool\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let result = get_required_rust_version(temp_dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_required_rust_version_no_cargo_toml() {
        let temp_dir = tempdir().unwrap();
        let result = get_required_rust_version(temp_dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_required_rust_version_two_parts() {
        let temp_dir = tempdir().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");

        fs::write(
            &cargo_toml,
            "[package]\nname = \"test-tool\"\nversion = \"0.1.0\"\nrust-version = \"1.75\"\n",
        )
        .unwrap();

        let result = get_required_rust_version(temp_dir.path()).unwrap();
        assert!(result.is_some());
        let version = result.unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 75);
        assert_eq!(version.patch, 0);
    }
}
