// SPDX-License-Identifier: GPL-3.0-or-later
//! Session snapshots: what survives a crash or a reboot (DECISIONS.md
//! D-033).
//!
//! A snapshot is JSON at `<state>/sessions/<server>/<session>.json` (mode
//! 0600, written atomically): the layout of every workspace, and per pane
//! its working directory, the foreground program and, if enabled, the text
//! of its scrollback, or for an app window the command that opens it again.
//! Restoring starts a shell in each saved directory and re-runs programs on
//! the `[session] restore-programs` allowlist; others are only named. App
//! windows that can say how to reopen (files, viewers, the monitor...)
//! come back in their places (D-104).
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
    /// A built-in app: restored when it says how ([`PaneSnapshot::reopen`]).
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
    /// For an app window: the palette command that opens it again as it
    /// was (D-104).
    #[serde(default)]
    pub reopen: Option<String>,
    /// An agent CLI it ran: restored as that CLI, in its account and
    /// folder, continuing its conversation (D-115).
    #[serde(default)]
    pub agent: Option<SavedAgent>,
}

/// An agent CLI a pane ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAgent {
    /// `claude`, `codex`, `gemini`.
    pub cli: String,
    #[serde(default)]
    pub account: Option<String>,
    /// The conversation its hooks reported.
    #[serde(default)]
    pub conversation: Option<String>,
    /// The pane was the agent's own window; else a shell it ran in.
    #[serde(default)]
    pub window: bool,
}

/// The command that brings `agent` back: `termos agent external` (after
/// `termos`, or after `tos` typed into a shell), continuing the folder's
/// latest conversation, or the one reported.
pub fn agent_args(agent: &SavedAgent) -> Vec<String> {
    let mut args = vec!["agent".to_owned(), "external".to_owned()];
    if let Some(a) = &agent.account {
        args.extend(["--account".to_owned(), a.clone()]);
    }
    args.push("--resume".to_owned());
    if let Some(c) = &agent.conversation {
        args.extend(["--conversation".to_owned(), c.clone()]);
    }
    args.push(agent.cli.clone());
    args
}

/// The program name of a command line (`/usr/bin/vim` → `vim`,
/// `C:\Tools\vim.exe` → `vim`).
pub fn program_name(argv0: &str) -> &str {
    let base = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    // Login shells show up as `-bash`.
    let base = base.strip_prefix('-').unwrap_or(base);
    match base.len().checked_sub(4) {
        Some(i) if base.is_char_boundary(i) && base[i..].eq_ignore_ascii_case(".exe") => &base[..i],
        _ => base,
    }
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

/// The kind of shell a restored program is typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Posix,
    PowerShell,
    Cmd,
}

impl ShellKind {
    /// The kind of the shell program `program` (a path or a name,
    /// perhaps followed by arguments).
    pub fn of(program: &str) -> Self {
        let first = program.split_whitespace().next().unwrap_or(program);
        let is = |want: &str| {
            [program, first]
                .iter()
                .any(|p| program_name(p).eq_ignore_ascii_case(want))
        };
        if is("pwsh") || is("powershell") {
            Self::PowerShell
        } else if is("cmd") {
            Self::Cmd
        } else {
            Self::Posix
        }
    }
}

/// The line that runs `argv` in a shell of kind `shell`, or `None` when an
/// argument cannot be quoted safely for it (cmd.exe has no way to quote
/// `"`, `%` or `!`).
pub fn command_line(argv: &[String], shell: ShellKind) -> Option<String> {
    if argv.is_empty() || argv.iter().any(|a| a.contains(['\r', '\n'])) {
        return None;
    }
    Some(match shell {
        ShellKind::Posix => shell_quote(argv),
        // `&` runs a quoted program; single quotes are literal, '' is one.
        ShellKind::PowerShell => {
            let quoted: Vec<String> = argv
                .iter()
                .map(|a| format!("'{}'", a.replace('\'', "''")))
                .collect();
            format!("& {}", quoted.join(" "))
        }
        ShellKind::Cmd => {
            if argv.iter().any(|a| a.contains(['"', '%', '!'])) {
                return None;
            }
            argv.iter()
                .map(|a| {
                    let plain = !a.is_empty()
                        && a.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_./\\:=,+@".contains(c));
                    if plain { a.clone() } else { format!("\"{a}\"") }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    })
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

/// Names Windows reserves for devices, even with an extension.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The file name for a session: unreserved characters as they are, the
/// rest percent-encoded (so any session name is a safe file name, also on
/// Windows, where device names such as `con` are encoded too).
pub fn file_name(session: &str) -> String {
    let mut out = String::new();
    for (i, b) in session.bytes().enumerate() {
        let device = i == 0 && RESERVED.contains(&session.to_ascii_lowercase().as_str());
        if (b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) && !device {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.push_str(".json");
    out
}

/// Writes `snap` into `dir` atomically (temporary file, then rename), mode
/// 0600 in a 0700 directory on Unix (on Windows the profile's ACLs apply).
pub fn save(dir: &Path, snap: &SessionSnapshot) -> io::Result<PathBuf> {
    ensure_private_dir(dir)?;
    let path = dir.join(file_name(&snap.name));
    let tmp = dir.join(format!(".{}.tmp", file_name(&snap.name)));
    let json = serde_json::to_vec_pretty(snap).map_err(io::Error::other)?;
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&json)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    rename(&tmp, &path)?;
    Ok(path)
}

/// Replaces `to` with `from`. On Windows a virus scanner can hold the old
/// file open for a moment, so a refused rename is retried briefly.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied && tries < 3 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => return other,
        }
    }
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
            reopen: None,
            agent: None,
        }
    }

    #[test]
    fn agents_come_back_continuing_their_conversation() {
        let agent = SavedAgent {
            cli: "claude".into(),
            account: Some("work".into()),
            conversation: Some("abc".into()),
            window: true,
        };
        assert_eq!(
            agent_args(&agent),
            [
                "agent",
                "external",
                "--account",
                "work",
                "--resume",
                "--conversation",
                "abc",
                "claude"
            ]
        );
        let plain = SavedAgent {
            account: None,
            conversation: None,
            ..agent
        };
        assert_eq!(
            agent_args(&plain),
            ["agent", "external", "--resume", "claude"]
        );
        // Snapshots from before M12b have no agent.
        let old: PaneSnapshot = serde_json::from_str(r#"{"id":1,"title":"x"}"#).expect("reads");
        assert_eq!(old.agent, None);
    }

    #[test]
    fn command_lines_for_each_shell() {
        let argv = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let vim = argv(&[r"C:\Tools\vim.exe", "my notes.md", "it's"]);
        assert_eq!(
            ShellKind::of(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            ShellKind::PowerShell
        );
        assert_eq!(ShellKind::of("powershell.exe"), ShellKind::PowerShell);
        assert_eq!(
            ShellKind::of(r"C:\Windows\system32\cmd.exe"),
            ShellKind::Cmd
        );
        assert_eq!(ShellKind::of("/bin/zsh"), ShellKind::Posix);
        assert_eq!(
            command_line(&vim, ShellKind::PowerShell).as_deref(),
            Some(r"& 'C:\Tools\vim.exe' 'my notes.md' 'it''s'")
        );
        assert_eq!(
            command_line(&vim, ShellKind::Cmd).as_deref(),
            Some(r#"C:\Tools\vim.exe "my notes.md" "it's""#)
        );
        assert_eq!(
            command_line(&vim, ShellKind::Posix).as_deref(),
            Some(r"'C:\Tools\vim.exe' 'my notes.md' 'it'\''s'")
        );
        // What cmd cannot quote is not typed at all.
        assert_eq!(command_line(&argv(&["vim", "100%"]), ShellKind::Cmd), None);
        assert_eq!(
            command_line(&argv(&["vim", "a\nb"]), ShellKind::Posix),
            None
        );
        assert_eq!(command_line(&[], ShellKind::Posix), None);
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
        assert_eq!(file_name("con"), "%63on.json");
        assert_eq!(file_name("Lpt1"), "%4Cpt1.json");
        assert_eq!(file_name("console"), "console.json");
    }

    #[test]
    fn save_and_load_round_trip_privately() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        let snap = sample();
        let path = save(&sessions, &snap).expect("save");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dmode = std::fs::metadata(&sessions)
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(dmode & 0o777, 0o700);
        }
        assert!(path.is_file());
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
        assert_eq!(program_name(r"C:\Tools\Vim.EXE"), "Vim");
        assert_eq!(program_name("pwsh.exe"), "pwsh");
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
