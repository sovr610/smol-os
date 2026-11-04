// SPDX-License-Identifier: GPL-3.0-or-later
//! Carrying a conversation from one account to another (M11a, D-097), so
//! a Claude Code or Codex window can continue under another account.
//!
//! - Claude Code keeps each conversation in
//!   `<account>/projects/<folder>/<id>.jsonl`, the folder named after the
//!   working directory (every character but ASCII letters and digits made
//!   `-`), with its sub-agents and tool results in `<id>/` beside it and
//!   file snapshots in `<account>/file-history/<id>/`. It resumes one
//!   from its file's path (`claude --resume <path>`).
//! - Codex keeps `<account>/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`,
//!   whose first line (`session_meta`) names the folder and the id; it
//!   finds one by id (`codex resume <id>`), looking through `sessions/`.
//!
//! The files are copied, never moved: the first account keeps its copy.
//! These are the CLIs' own formats, not interfaces; only the names and
//! the first line are read.

use std::io::{self, BufRead as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cli_agents::Tool;

/// A conversation an account's CLI keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub tool: Tool,
    pub id: String,
    /// Its file.
    pub path: PathBuf,
    /// Its file, from the account's folder.
    pub relative: PathBuf,
}

/// Claude Code's folder name for a working directory.
pub fn project_folder(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn modified(p: &Path) -> SystemTime {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// `.jsonl` files directly in `dir`, newest first.
fn newest_jsonl(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "jsonl"))
                .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|p| std::cmp::Reverse(modified(p)));
    files
}

/// The newest conversation `tool` keeps in `home` for `cwd`.
pub fn latest(tool: Tool, home: &Path, cwd: &Path) -> Option<Conversation> {
    match tool {
        Tool::Claude => {
            let projects = home.join("projects");
            let name = project_folder(cwd);
            // Long paths are cut short (and a hash added): a folder that
            // starts the same way.
            let dir = if projects.join(&name).is_dir() {
                projects.join(&name)
            } else {
                let start: String = name.chars().take(200).collect();
                std::fs::read_dir(&projects)
                    .ok()?
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| {
                        p.is_dir()
                            && p.file_name()
                                .is_some_and(|n| n.to_string_lossy().starts_with(&start))
                    })?
            };
            let path = newest_jsonl(&dir).into_iter().next()?;
            let id = path.file_stem()?.to_string_lossy().into_owned();
            let relative = path.strip_prefix(home).ok()?.to_owned();
            Some(Conversation {
                tool,
                id,
                path,
                relative,
            })
        }
        Tool::Codex => {
            let mut files = Vec::new();
            walk(&home.join("sessions"), 4, &mut files);
            files.sort_by_key(|p| std::cmp::Reverse(modified(p)));
            let want = cwd.canonicalize().unwrap_or_else(|_| cwd.to_owned());
            files.into_iter().take(500).find_map(|path| {
                let (id, folder) = codex_meta(&path)?;
                let folder = PathBuf::from(folder);
                let folder = folder.canonicalize().unwrap_or(folder);
                if folder != want {
                    return None;
                }
                let relative = path.strip_prefix(home).ok()?.to_owned();
                Some(Conversation {
                    tool,
                    id,
                    relative,
                    path,
                })
            })
        }
    }
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() && depth > 0 {
            walk(&p, depth - 1, out);
        } else if p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
            && p.extension().is_some_and(|x| x == "jsonl")
        {
            out.push(p);
        }
    }
}

/// A Codex session's id and folder, from its first line.
fn codex_meta(path: &Path) -> Option<(String, String)> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    io::BufReader::new(file).read_line(&mut line).ok()?;
    let v: serde_json::Value = serde_json::from_str(&line).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return None;
    }
    let p = v.get("payload")?;
    let id = p.get("id").and_then(|i| i.as_str())?.to_owned();
    let cwd = p.get("cwd").and_then(|c| c.as_str())?.to_owned();
    Some((id, cwd))
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)?.flatten() {
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// Copies `c` (from the account folder `from`) into the account folder
/// `to`; the copy, to resume.
pub fn carry(c: &Conversation, from: &Path, to: &Path) -> io::Result<Conversation> {
    let dest = to.join(&c.relative);
    copy_tree(&c.path, &dest)?;
    if c.tool == Tool::Claude {
        // Its sub-agents and tool results, and its file snapshots.
        let beside = c.path.with_extension("");
        if beside.is_dir() {
            copy_tree(&beside, &dest.with_extension(""))?;
        }
        let history = from.join("file-history").join(&c.id);
        if history.is_dir() {
            copy_tree(&history, &to.join("file-history").join(&c.id))?;
        }
    }
    Ok(Conversation {
        tool: c.tool,
        id: c.id.clone(),
        path: dest,
        relative: c.relative.clone(),
    })
}

/// The arguments that continue `c` (already in the account's folder).
pub fn resume_args(c: &Conversation) -> Vec<String> {
    match c.tool {
        Tool::Claude => vec!["--resume".into(), c.path.display().to_string()],
        Tool::Codex => vec!["resume".into(), c.id.clone()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_names_folders_after_the_working_directory() {
        assert_eq!(
            project_folder(Path::new("/home/a/my.proj")),
            "-home-a-my-proj"
        );
        assert_eq!(
            project_folder(Path::new("/Users/you/my project")),
            "-Users-you-my-project"
        );
    }

    #[test]
    fn a_claude_conversation_is_carried_with_its_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (from, to) = (dir.path().join("a"), dir.path().join("b"));
        let cwd = Path::new("/w/proj");
        let folder = from.join("projects").join(project_folder(cwd));
        std::fs::create_dir_all(folder.join("old-id")).expect("mkdir");
        std::fs::write(folder.join("old-id.jsonl"), "{\"old\":1}\n").expect("write");
        // A moment later: the newer one.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(folder.join("new-id.jsonl"), "{\"new\":1}\n").expect("write");
        std::fs::create_dir_all(folder.join("new-id/subagents")).expect("mkdir");
        std::fs::write(folder.join("new-id/subagents/a.jsonl"), "{}").expect("write");
        std::fs::create_dir_all(from.join("file-history/new-id")).expect("mkdir");
        std::fs::write(from.join("file-history/new-id/f"), "snap").expect("write");

        let c = latest(Tool::Claude, &from, cwd).expect("found");
        assert_eq!(c.id, "new-id");
        let moved = carry(&c, &from, &to).expect("carried");
        let dest = to
            .join("projects")
            .join(project_folder(cwd))
            .join("new-id.jsonl");
        assert_eq!(moved.path, dest);
        assert_eq!(
            std::fs::read_to_string(&dest).expect("read"),
            "{\"new\":1}\n"
        );
        assert!(
            to.join("projects/-w-proj/new-id/subagents/a.jsonl")
                .exists()
        );
        assert!(to.join("file-history/new-id/f").exists());
        assert!(c.path.exists(), "copied, not moved");
        assert_eq!(
            resume_args(&moved),
            ["--resume", dest.to_string_lossy().as_ref()]
        );
        assert!(latest(Tool::Claude, &from, Path::new("/elsewhere")).is_none());
    }

    #[test]
    fn a_codex_session_is_found_by_its_folder_and_resumed_by_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (from, to) = (dir.path().join("a"), dir.path().join("b"));
        let day = from.join("sessions/2026/11/04");
        std::fs::create_dir_all(&day).expect("mkdir");
        let cwd = dir.path().join("proj");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        let meta = |id: &str, cwd: &Path| {
            format!(
                "{{\"timestamp\":\"2026-11-04T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":{}}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"text\":\"SECRET\"}}}}\n",
                serde_json::Value::from(cwd.display().to_string())
            )
        };
        std::fs::write(
            day.join("rollout-2026-11-04T10-00-00-aaa.jsonl"),
            meta("aaa", &cwd),
        )
        .expect("write");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            day.join("rollout-2026-11-04T10-05-00-other.jsonl"),
            meta("other", Path::new("/somewhere/else")),
        )
        .expect("write");
        let c = latest(Tool::Codex, &from, &cwd).expect("found");
        assert_eq!(c.id, "aaa", "the newest for this folder, not the newest");
        let moved = carry(&c, &from, &to).expect("carried");
        assert!(
            to.join("sessions/2026/11/04/rollout-2026-11-04T10-00-00-aaa.jsonl")
                .exists()
        );
        assert_eq!(resume_args(&moved), ["resume", "aaa"]);
    }
}
