// SPDX-License-Identifier: GPL-3.0-or-later
//! Locating the repository and listing its files.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;

/// The workspace root (the parent of the `xtask` crate).
pub(crate) fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Tracked files plus untracked files that are not ignored, relative to
/// `root`, sorted. Using git keeps `target/`, Buildroot output and other
/// ignored trees out of every check without a hand-maintained skip list.
pub(crate) fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run `git ls-files`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git ls-files` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    let listing = String::from_utf8(out.stdout).map_err(|_| "non-UTF-8 path in repository")?;
    let mut files: Vec<PathBuf> = listing
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        // `--cached` still lists files deleted from the working tree.
        .filter(|p| root.join(p).is_file())
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}
