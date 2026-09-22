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

//! End-to-end smoke tests of the `adnt` binary. Each test runs it with an
//! isolated HOME and a minimal PATH, so no real install, token or agent is
//! touched and no network access happens.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::{tempdir, TempDir};

fn adnt(home: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_adnt"));
    command.env_clear();
    // Keep coverage instrumentation (cargo llvm-cov) working through env_clear
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
        .args(args)
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        // Keeps the GitHub client from falling back to `gh auth token`
        .env("GITHUB_TOKEN", "dummy")
        .envs(envs.iter().copied())
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Arguments, environment overrides and expected error message.
type FailureCase<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str);

fn home() -> TempDir {
    tempdir().unwrap()
}

#[test]
fn help_lists_commands() {
    let home = home();
    let output = adnt(home.path(), &["--help"], &[]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for command in ["list", "run", "rm", "update", "config", "ia"] {
        assert!(
            stdout.contains(command),
            "missing '{}' in:\n{}",
            command,
            stdout
        );
    }
}

#[test]
fn rm_uninstalled_tool_creates_private_adnt_dir() {
    let home = home();
    let output = adnt(home.path(), &["rm", "demo"], &[]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("not installed"));
    let mode = std::fs::metadata(home.path().join(".adnt"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn rm_rejects_path_traversal() {
    let home = home();
    std::fs::write(home.path().join("keep.txt"), "data").unwrap();

    let output = adnt(home.path(), &["rm", "x/../../.."], &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("Invalid tool name"));
    assert!(home.path().join("keep.txt").exists());
}

#[test]
fn ia_install_rejects_bad_arguments_before_installing() {
    let home = home();
    let cases: &[FailureCase] = &[
        (
            &["ia", "install", "hermes"],
            &[("VLLM_URL", "http://vllm.test")],
            "VLLM_URL must be an https:// URL",
        ),
        (
            &["ia", "install", "opencode"],
            &[("AUTHENTIK_URL", "http://auth.test")],
            "AUTHENTIK_URL must be an https:// URL",
        ),
        (
            &["ia", "install", "opencode", "--model", "coder"],
            &[],
            "--model is only supported for hermes",
        ),
        (
            &["ia", "install", "hermes", "--model", "chat"],
            &[],
            "Unknown model 'chat'",
        ),
        (&["ia", "install", "claude"], &[], "invalid value 'claude'"),
    ];

    for (args, envs, expected) in cases {
        let output = adnt(home.path(), args, envs);
        assert!(!output.status.success(), "{:?} should fail", args);
        assert!(
            stderr(&output).contains(expected),
            "{:?}: expected '{}' in:\n{}",
            args,
            expected,
            stderr(&output)
        );
    }

    // Nothing was installed or configured
    assert!(!home.path().join(".hermes").exists());
    assert!(!home.path().join(".config/opencode").exists());
    assert!(!home.path().join(".local").exists());
}
