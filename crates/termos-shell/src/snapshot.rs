// SPDX-License-Identifier: GPL-3.0-or-later
//! Session snapshots: what survives a crash or a reboot (DECISIONS.md
//! D-033).
//!
//! A snapshot is JSON at `<state>/sessions/<server>/<session>.json` (mode
//! 0600, written atomically): the layout of every workspace, and per pane
//! its working directory, the foreground program and, if enabled, the text
//! of its scrollback. Restoring starts a shell in each saved directory and
//! re-runs programs on the `[session] restore-programs` allowlist; others
//! are only named.
//!
//! Command lines are stored only for allowlisted programs: anything else
//! (which may carry a password on its command line) is saved by name.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use termos_core::paths::ensure_private_dir;
use termos_wm::workspace::Session;

/// The snapshot format version.
pub const SNAPSHOT_VERSION: u32 = 1;

/// A saved session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: u32,
    pub name: String,
    /// Unix seconds.
    pub saved_at: u64,
    /// Workspaces, layout trees, floats, focus and names.
    pub layout: Session,
    pub panes: Vec<PaneSnapshot>,
}

/// What a pane was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedKind {
    #[default]
    Terminal,
    /// A built-in app (tutorial, notification center): not restored.
    App,
}

/// A saved pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshot {
    /// The pane's id in the saved layout.
    pub id: u64,
    #[serde(default)]
    pub kind: SavedKind,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// The foreground program's name, when it was not the shell.
    #[serde(default)]
    pub program: Option<String>,
    /// Its command line, saved only for allowlisted programs.
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub title: String,
    /// Scrollback and screen text, oldest first (`save-scrollback`).
    #[serde(default)]
    pub scrollback: Option<Vec<String>>,
}

/// The program name of a command line (`/usr/bin/vim` → `vim`).
pub fn program_name(argv0: &str) -> &str {
    let base = argv0.rsplit('/').next().unwrap_or(argv0);
    // Login shells show up as `-bash`.
    base.strip_prefix('-').unwrap_or(base)
}

/// Whether `argv` runs a program on the allowlist.
pub fn allowed(argv: &[String], allow: &[String]) -> bool {
    argv.first()
        .is_some_and(|a| allow.iter().any(|p| p == program_name(a)))
}

/// What restoring a pane does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relaunch {
    /// Just the shell.
    Shell,
    /// The shell, then this command typed into it.
    Program(Vec<String>),
    /// The shell; this program was running but is not on the allowlist.
    Skipped(String),
}

/// Decides how to restore `pane` under the allowlist in force now (it may
/// have shrunk since the snapshot was written).
pub fn relaunch(pane: &PaneSnapshot, allow: &[String]) -> Relaunch {
    match (&pane.argv, &pane.program) {
        (Some(argv), _) if allowed(argv, allow) => Relaunch::Program(argv.clone()),
        (Some(argv), _) if !argv.is_empty() => Relaunch::Skipped(program_name(&argv[0]).to_owned()),
        (_, Some(name)) => Relaunch::Skipped(name.clone()),
        _ => Relaunch::Shell,
    }
}

/// The directory to restore a pane in: the saved one if it still exists.
pub fn restore_cwd(pane: &PaneSnapshot) -> Option<PathBuf> {
    pane.cwd.clone().filter(|c| c.is_dir())
}

/// Quotes a command line for a POSIX shell.
pub fn shell_quote(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
            if plain {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The file name for a session: unreserved characters as they are, the
/// rest percent-encoded (so any session name is a safe file name).
pub fn file_name(session: &str) -> String {
    let mut out = String::new();
    for b in session.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.push_str(".json");
    out
}

/// Writes `snap` into `dir` atomically (temporary file, then rename), mode
/// 0600, in a 0700 directory.
pub fn save(dir: &Path, snap: &SessionSnapshot) -> io::Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt as _;
    ensure_private_dir(dir)?;
    let path = dir.join(file_name(&snap.name));
    let tmp = dir.join(format!(".{}.tmp", file_name(&snap.name)));
    let json = serde_json::to_vec_pretty(snap).map_err(io::Error::other)?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&json)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Deletes a session's snapshot (a missing file is fine).
pub fn remove(dir: &Path, session: &str) -> io::Result<()> {
    match std::fs::remove_file(dir.join(file_name(session))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Loads every snapshot in `dir`, sorted by name. Files that cannot be read
/// are skipped and described in the second list.
pub fn load_all(dir: &Path) -> (Vec<SessionSnapshot>, Vec<String>) {
    let mut snaps = Vec::new();
    let mut errors = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (snaps, errors),
        Err(e) => {
            errors.push(format!("cannot read {}: {e}", dir.display()));
            return (snaps, errors);
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_snapshot = path.extension().is_some_and(|e| e == "json")
            && !entry.file_name().to_string_lossy().starts_with('.');
        if !is_snapshot {
            continue;
        }
        let parsed = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<SessionSnapshot>(&b).map_err(|e| e.to_string()))
            .and_then(|s| {
                if s.version == SNAPSHOT_VERSION {
                    Ok(s)
                } else {
                    Err(format!("unsupported snapshot version {}", s.version))
                }
            });
        match parsed {
            Ok(s) => snaps.push(s),
            Err(e) => errors.push(format!("skipped {}: {e}", path.display())),
        }
    }
    snaps.sort_by(|a, b| a.name.cmp(&b.name));
    (snaps, errors)
}

/// Unix seconds now.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A save time for people: `14:02` today, else `Mar 4 14:02`.
pub fn describe_time(secs: u64) -> String {
    let Ok(ts) = jiff::Timestamp::from_second(i64::try_from(secs).unwrap_or(0)) else {
        return "an unknown time".into();
    };
    let tz = jiff::tz::TimeZone::system();
    let then = ts.to_zoned(tz.clone());
    let today = jiff::Timestamp::now().to_zoned(tz);
    if then.date() == today.date() {
        then.strftime("%H:%M").to_string()
    } else {
        then.strftime("%b %-d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_core::ids::PaneId;
    use termos_wm::workspace::{SplitHow, Workspace};

    fn pane(id: u64) -> PaneSnapshot {
        PaneSnapshot {
            id,
            kind: SavedKind::Terminal,
            cwd: None,
            program: None,
            argv: None,
            title: String::new(),
            scrollback: None,
        }
    }

    fn sample() -> SessionSnapshot {
        let area = termos_render::grid::Rect::new(0, 0, 80, 24);
        let mut ws = Workspace::new("code", PaneId(1));
        ws.add_split(PaneId(2), SplitHow::Right, area);
        ws.add_float(PaneId(3), area);
        SessionSnapshot {
            version: SNAPSHOT_VERSION,
            name: "web dev/1".into(),
            saved_at: 1_700_000_000,
            layout: Session::new("web dev/1", ws),
            panes: vec![
                PaneSnapshot {
                    cwd: Some("/tmp".into()),
                    ..pane(1)
                },
                PaneSnapshot {
                    program: Some("vim".into()),
                    argv: Some(vec!["vim".into(), "a b.txt".into()]),
                    ..pane(2)
                },
                PaneSnapshot {
                    scrollback: Some(vec!["$ make".into(), "ok".into()]),
                    ..pane(3)
                },
            ],
        }
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("main"), "main.json");
        assert_eq!(file_name("web dev/1"), "web%20dev%2F1.json");
        assert_eq!(file_name(".."), "%2E%2E.json");
    }

    #[test]
    fn save_and_load_round_trip_privately() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let snap = sample();
        let path = save(&sessions, &snap).expect("save");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let dmode = std::fs::metadata(&sessions)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(dmode & 0o777, 0o700);
        // A damaged file and a future version are skipped, not fatal.
        std::fs::write(sessions.join("bad.json"), "{").expect("write");
        let mut future = sample();
        future.name = "future".into();
        future.version = 99;
        save(&sessions, &future).expect("save");
        let (loaded, errors) = load_all(&sessions);
        assert_eq!(loaded, vec![snap.clone()]);
        assert_eq!(errors.len(), 2, "{errors:?}");
        remove(&sessions, &snap.name).expect("remove");
        remove(&sessions, &snap.name).expect("removing twice is fine");
        assert!(load_all(&sessions).0.is_empty());
        assert!(load_all(&dir.path().join("missing")).0.is_empty());
    }

    #[test]
    fn relaunch_follows_the_allowlist() {
        let allow = vec!["vim".to_owned(), "less".to_owned()];
        let vim = PaneSnapshot {
            program: Some("vim".into()),
            argv: Some(vec!["/usr/bin/vim".into(), "x".into()]),
            ..pane(1)
        };
        assert_eq!(
            relaunch(&vim, &allow),
            Relaunch::Program(vec!["/usr/bin/vim".into(), "x".into()])
        );
        // Removed from the allowlist since: only named.
        assert_eq!(
            relaunch(&vim, &["less".to_owned()]),
            Relaunch::Skipped("vim".into())
        );
        let cargo = PaneSnapshot {
            program: Some("cargo".into()),
            ..pane(2)
        };
        assert_eq!(relaunch(&cargo, &allow), Relaunch::Skipped("cargo".into()));
        assert_eq!(relaunch(&pane(3), &allow), Relaunch::Shell);
    }

    #[test]
    fn helpers() {
        assert_eq!(program_name("/usr/bin/nvim"), "nvim");
        assert_eq!(program_name("-bash"), "bash");
        assert_eq!(
            shell_quote(&["less".into(), "it's here.txt".into(), "-R".into()]),
            r"less 'it'\''s here.txt' -R"
        );
        assert_eq!(shell_quote(&[String::new()]), "''");
        let gone = PaneSnapshot {
            cwd: Some("/definitely/not/here".into()),
            ..pane(1)
        };
        assert_eq!(restore_cwd(&gone), None);
        let here = PaneSnapshot {
            cwd: Some("/".into()),
            ..pane(1)
        };
        assert_eq!(restore_cwd(&here), Some(PathBuf::from("/")));
        assert!(!describe_time(now_secs()).contains(' '), "today: time only");
        assert!(describe_time(1_000_000_000).contains(' '));
    }
}
