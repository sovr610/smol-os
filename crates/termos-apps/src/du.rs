// SPDX-License-Identifier: GPL-3.0-or-later
//! Disk usage (M13c, D-122): each entry of a folder sized with all its
//! contents, biggest first, as ncdu does. Run on a thread; links are not
//! followed and unreadable parts are skipped.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// One entry of the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub dir: bool,
}

/// The size of `path` and everything under it; `seen` counts files as
/// they are found, and `stop` ends the walk early.
fn size_of(path: &Path, seen: &AtomicUsize, stop: &AtomicBool) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    seen.fetch_add(1, Ordering::Relaxed);
    if !meta.is_dir() {
        return meta.len();
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_owned()];
    while let Some(dir) = stack.pop() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in read.flatten() {
            let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            seen.fetch_add(1, Ordering::Relaxed);
            if m.is_dir() {
                stack.push(e.path());
            } else {
                total = total.saturating_add(m.len());
            }
        }
    }
    total
}

/// Every entry of `dir`, sized, biggest first.
pub fn usage(dir: &Path, seen: &AtomicUsize, stop: &AtomicBool) -> Result<Vec<Usage>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("cannot open {}: {e}", dir.display()))?;
    let mut out: Vec<Usage> = read
        .flatten()
        .map(|e| {
            let path = e.path();
            let dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
            Usage {
                name: e.file_name().to_string_lossy().into_owned(),
                bytes: size_of(&path, seen, stop),
                path,
                dir,
            }
        })
        .collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// A bar `width` cells long, filled to `bytes / most`.
pub fn bar(bytes: u64, most: u64, width: usize) -> String {
    let filled = if most == 0 {
        0
    } else {
        ((bytes as f64 / most as f64) * width as f64).round() as usize
    };
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_sized_with_their_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("big/deep")).expect("mkdir");
        std::fs::write(dir.path().join("big/deep/a"), vec![0u8; 3000]).expect("write");
        std::fs::write(dir.path().join("big/b"), vec![0u8; 1000]).expect("write");
        std::fs::write(dir.path().join("small"), vec![0u8; 10]).expect("write");
        let seen = AtomicUsize::new(0);
        let u = usage(dir.path(), &seen, &AtomicBool::new(false)).expect("sized");
        let got: Vec<(&str, u64, bool)> = u
            .iter()
            .map(|e| (e.name.as_str(), e.bytes, e.dir))
            .collect();
        assert_eq!(got, vec![("big", 4000, true), ("small", 10, false)]);
        assert!(seen.load(Ordering::Relaxed) >= 5);
        assert_eq!(bar(50, 100, 4), "██░░");
        assert_eq!(bar(0, 0, 3), "░░░");
    }
}
