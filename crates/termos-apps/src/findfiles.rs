// SPDX-License-Identifier: GPL-3.0-or-later
//! Searching under a folder (M13c, D-122): names (`?` in Files) and text
//! inside files (`Ctrl+F`), on a thread. `.gitignore` is honoured, and
//! hidden files are searched only when they are shown.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The most results one search gives.
pub const MAX_RESULTS: usize = 1000;

/// A match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    /// The line, for a text match.
    pub line: Option<u32>,
    /// What to show: the path below the folder, and the matching text.
    pub shown: String,
}

fn walker(dir: &Path, hidden: bool) -> ignore::Walk {
    let mut w = ignore::WalkBuilder::new(dir);
    w.hidden(!hidden).require_git(false);
    w.build()
}

fn relative(path: &Path, dir: &Path) -> String {
    path.strip_prefix(dir).unwrap_or(path).display().to_string()
}

/// Files and folders under `dir` whose name contains `query` (ignoring
/// case).
pub fn names(dir: &Path, query: &str, hidden: bool, stop: &AtomicBool) -> Vec<Found> {
    let q = query.to_lowercase();
    let mut out = Vec::new();
    for e in walker(dir, hidden).filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) || out.len() >= MAX_RESULTS {
            break;
        }
        if e.depth() == 0 {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.contains(&q) {
            out.push(Found {
                shown: relative(e.path(), dir),
                path: e.into_path(),
                line: None,
            });
        }
    }
    out
}

/// Lines of text files under `dir` that contain `query` (plain text; case
/// ignored unless it has a capital).
pub fn text(
    dir: &Path,
    query: &str,
    hidden: bool,
    stop: &AtomicBool,
) -> Result<Vec<Found>, String> {
    use grep::regex::RegexMatcherBuilder;
    use grep::searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
    let matcher = RegexMatcherBuilder::new()
        .case_smart(true)
        .build(&regex::escape(query))
        .map_err(|e| e.to_string())?;
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .build();
    let mut out = Vec::new();
    for e in walker(dir, hidden).filter_map(Result::ok) {
        if stop.load(Ordering::Relaxed) || out.len() >= MAX_RESULTS {
            break;
        }
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = relative(e.path(), dir);
        let path = e.path().to_owned();
        let _ = searcher.search_path(
            &matcher,
            e.path(),
            UTF8(|line, found| {
                if out.len() >= MAX_RESULTS {
                    return Ok(false);
                }
                let snippet: String = found.trim().chars().take(200).collect();
                out.push(Found {
                    path: path.clone(),
                    line: u32::try_from(line).ok(),
                    shown: format!("{rel}:{line}: {snippet}"),
                });
                Ok(true)
            }),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_text_are_found_gitignore_honoured() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join("target")).expect("mkdir");
        std::fs::write(dir.path().join(".gitignore"), "target/\n").expect("write");
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"Hello\");\n}\n",
        )
        .expect("write");
        std::fs::write(dir.path().join("target/main.rs"), "Hello").expect("write");
        let stop = AtomicBool::new(false);
        let n = names(dir.path(), "MAIN", false, &stop);
        assert_eq!(n.len(), 1, "{n:?}");
        assert_eq!(
            n[0].shown,
            Path::new("src").join("main.rs").display().to_string()
        );
        let t = text(dir.path(), "hello", false, &stop).expect("searched");
        assert_eq!(t.len(), 1, "{t:?}");
        assert_eq!(t[0].line, Some(2));
        assert!(
            t[0].shown.ends_with(":2: println!(\"Hello\");"),
            "{}",
            t[0].shown
        );
        // A capital: case counts.
        assert!(
            text(dir.path(), "HELLO", false, &stop)
                .expect("searched")
                .is_empty()
        );
    }
}
