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

//! Helpers for files and directories holding secrets (tokens). Owner-only
//! permissions are enforced on Unix; elsewhere the platform defaults apply.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Creates the directory (and its parents) and restricts it to its owner.
pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("Failed to create directory {}", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Restricts an existing file to its owner (no-op on non-Unix platforms).
pub fn restrict_to_owner(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Atomically writes a file readable by its owner only: the content is written
/// to a 0600 temporary file which then replaces the target, so the secret is
/// never exposed with looser permissions and a crash never truncates the file.
/// Refuses to write through a symlink (e.g. a dotfiles repository), which would
/// leak the secret to wherever the link points.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        anyhow::bail!(
            "{} is a symlink: refusing to write a secret through it",
            path.display()
        );
    }
    let parent = path.parent().context("Invalid file path")?;
    fs::create_dir_all(parent)?;

    let file_name = path.file_name().context("Invalid file path")?;
    let tmp_path = parent.join(format!(".{}.tmp", file_name.to_string_lossy()));
    // A leftover from an interrupted write would make create_new fail
    let _ = fs::remove_file(&tmp_path);

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&tmp_path)
        .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp_path, path).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Asserts that the file is readable by its owner only (Unix only).
#[cfg(test)]
pub fn assert_owner_only(path: &Path) {
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600,
        "{}",
        path.display()
    );
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_write_private_replaces_existing_file() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("config.json");
        fs::write(&path, "old").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, "secret").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "secret");
        assert_owner_only(&path);
    }

    #[cfg(unix)]
    #[test]
    fn test_private_dir() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("a/b");

        private_dir(&path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn test_write_private_refuses_symlink() {
        let temp_dir = tempdir().unwrap();
        let target = temp_dir.path().join("dotfiles.json");
        let link = temp_dir.path().join("config.json");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(write_private(&link, "secret").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "{}");
    }
}
