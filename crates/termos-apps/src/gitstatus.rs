// SPDX-License-Identifier: GPL-3.0-or-later
//! Git status in the file manager (M13c, D-122): a mark per entry of a
//! folder in a repository, from `git status --porcelain=v2 -z`, run on
//! the file manager's thread.
//!
//! The marks: `M` changed, `A` added, `D` deleted, `R` renamed, `U`
//! conflict, `?` untracked, `!` ignored. A folder shows the strongest of
//! what is inside it.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

/// How much a mark says, for folders: the strongest wins.
fn weight(c: char) -> u8 {
    match c {
        'U' => 6,
        'M' | 'R' | 'D' => 5,
        'A' => 4,
        '?' => 3,
        '!' => 1,
        _ => 0,
    }
}

fn mark_of(xy: &str) -> char {
    let (x, y) = {
        let mut c = xy.chars();
        (c.next().unwrap_or('.'), c.next().unwrap_or('.'))
    };
    if x == 'A' {
        'A'
    } else if x == 'R' || y == 'R' {
        'R'
    } else if x == 'D' || y == 'D' {
        'D'
    } else {
        'M'
    }
}

/// The marks of a folder's entries from `git status --porcelain=v2 -z`
/// output, its paths relative to the repository; `prefix` is the folder's
/// own path there (`git rev-parse --show-prefix`: `src/` or empty).
pub fn parse(out: &[u8], prefix: &str) -> HashMap<String, char> {
    let text = String::from_utf8_lossy(out);
    let mut fields = text.split('\0');
    let mut marks: HashMap<String, char> = HashMap::new();
    let mut put = |path: &str, c: char| {
        let Some(rest) = path.strip_prefix(prefix) else {
            return;
        };
        let name = rest.split('/').next().unwrap_or(rest);
        if name.is_empty() {
            return;
        }
        let e = marks.entry(name.to_owned()).or_insert(c);
        if weight(c) > weight(*e) {
            *e = c;
        }
    };
    while let Some(f) = fields.next() {
        let mut parts = f.splitn(2, ' ');
        match (parts.next(), parts.next()) {
            (Some("1"), Some(rest)) => {
                // 1 XY sub mH mI mW hH hI path
                let cols: Vec<&str> = rest.splitn(8, ' ').collect();
                if let (Some(xy), Some(path)) = (cols.first(), cols.get(7)) {
                    put(path, mark_of(xy));
                }
            }
            (Some("2"), Some(rest)) => {
                // 2 XY sub mH mI mW hH hI Xscore path, then the old path.
                let cols: Vec<&str> = rest.splitn(9, ' ').collect();
                if let Some(path) = cols.get(8) {
                    put(path, 'R');
                }
                let _ = fields.next();
            }
            (Some("u"), Some(rest)) => {
                let cols: Vec<&str> = rest.splitn(10, ' ').collect();
                if let Some(path) = cols.get(9) {
                    put(path, 'U');
                }
            }
            (Some("?"), Some(path)) => put(path, '?'),
            (Some("!"), Some(path)) => put(path, '!'),
            _ => {}
        }
    }
    marks
}

/// Whether `dir` is inside a git repository (a `.git` above it).
pub fn in_repository(dir: &Path) -> bool {
    dir.ancestors().any(|d| d.join(".git").exists())
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// The marks of `dir`'s entries; `None` outside a repository or without
/// git.
pub fn status(dir: &Path) -> Option<HashMap<String, char>> {
    if !in_repository(dir) {
        return None;
    }
    let prefix = git(dir, &["rev-parse", "--show-prefix"])?;
    let prefix = String::from_utf8_lossy(&prefix).trim().to_owned();
    let out = git(
        dir,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--ignored=matching",
            "--untracked-files=normal",
            "--",
            ".",
        ],
    )?;
    Some(parse(&out, &prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_marks_folders_and_files() {
        let out = b"1 .M N... 100644 100644 100644 a b src/lib.rs\0\
1 A. N... 000000 100644 100644 0 b new.txt\0\
2 R. N... 100644 100644 100644 a a R100 src/new.rs\0src/old.rs\0\
? notes.md\0\
! target/\0\
1 .M N... 100644 100644 100644 a b other/x.rs\0";
        let top = parse(out, "");
        assert_eq!(top.get("src"), Some(&'M'));
        assert_eq!(top.get("new.txt"), Some(&'A'));
        assert_eq!(top.get("notes.md"), Some(&'?'));
        assert_eq!(top.get("target"), Some(&'!'));
        let src = parse(out, "src/");
        assert_eq!(src.get("lib.rs"), Some(&'M'));
        assert_eq!(src.get("new.rs"), Some(&'R'));
        assert_eq!(src.get("x.rs"), None);
    }

    #[test]
    fn a_real_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !run(&["init", "-q"]) {
            // No git on this machine: nothing to test.
            return;
        }
        std::fs::write(dir.path().join("kept.txt"), "a").expect("write");
        run(&["add", "kept.txt"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "x",
        ]);
        std::fs::write(dir.path().join("kept.txt"), "b").expect("write");
        std::fs::write(dir.path().join("fresh.txt"), "c").expect("write");
        let marks = status(dir.path()).expect("a repository");
        assert_eq!(marks.get("kept.txt"), Some(&'M'));
        assert_eq!(marks.get("fresh.txt"), Some(&'?'));
    }
}
