// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the server runs from on Windows (D-086).
//!
//! Windows will not replace an executable while a process runs from it, and
//! the server outlives every terminal. Run from the installed `termos.exe`,
//! it would make `cargo install` and unpacking a new build fail with
//! "Access is denied" until `termos kill-server`. So the server runs from a
//! copy in TermOS's data folder, named after the executable's size and time:
//! a new build gets a new copy, and old copies are removed once nothing
//! runs from them.

use std::io;
use std::path::{Path, PathBuf};

/// A name for this build of `exe`: its size and modification time.
fn stamp(exe: &Path) -> io::Result<String> {
    let meta = std::fs::metadata(exe)?;
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(format!("{}-{modified}", meta.len()))
}

/// The copy of `exe` in `dir` to run the server from, made when missing.
/// Copies of older builds are removed when they can be (one still running
/// stays until it exits).
pub(crate) fn server_exe(dir: &Path, exe: &Path) -> io::Result<PathBuf> {
    let name = format!("termos-{}.exe", stamp(exe)?);
    let copy = dir.join(&name);
    if !copy.is_file() {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{name}.{}.part", std::process::id()));
        std::fs::copy(exe, &tmp)?;
        std::fs::rename(&tmp, &copy)?;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.starts_with("termos-") && *n != *name {
                // In use by a server still running: it goes next time.
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_copy_per_build_and_old_ones_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("termos.exe");
        std::fs::write(&exe, b"build one").expect("write");
        let bin = dir.path().join("data/bin/server");
        let first = server_exe(&bin, &exe).expect("copied");
        assert_eq!(std::fs::read(&first).expect("read"), b"build one");
        // The same build: the same copy.
        assert_eq!(server_exe(&bin, &exe).expect("again"), first);
        // A new build (another size): a new copy, and the old one goes.
        std::fs::write(&exe, b"build number two").expect("write");
        let second = server_exe(&bin, &exe).expect("copied");
        assert_ne!(second, first);
        assert_eq!(std::fs::read(&second).expect("read"), b"build number two");
        assert!(!first.exists(), "the old copy was removed");
        let left: Vec<_> = std::fs::read_dir(&bin).expect("dir").flatten().collect();
        assert_eq!(left.len(), 1, "no partial copies left behind");
    }
}
