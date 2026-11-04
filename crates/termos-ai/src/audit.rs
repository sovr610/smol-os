// SPDX-License-Identifier: GPL-3.0-or-later
//! The tamper-evident audit log (ARCHITECTURE.md §11, D-062).
//!
//! One append-only JSONL file per agent session, in
//! `$XDG_STATE_HOME/termos/audit/<session>.jsonl` (mode 0600). Every line
//! records an event (the session starting, a tool call with its redacted
//! arguments, the decision, a result summary) with its time, and the
//! SHA-256 of the line before it (`prev`; 64 zeros for the first). Editing,
//! removing or reordering any line breaks the chain from there on, which
//! [`verify`] finds. The session's conversation file also keeps the hash
//! of the last line, so cutting the end off shows too.

use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// `prev` of the first line.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    /// RFC 3339, UTC.
    pub time: String,
    /// SHA-256 (hex) of the previous line.
    pub prev: String,
    pub session: String,
    pub event: Event,
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Event {
    /// The session started (or resumed).
    Session {
        profile: String,
        provider: String,
        model: String,
        workspace: String,
        sandbox: String,
    },
    /// A tool call: what was asked, what was decided, what came of it.
    Tool {
        id: String,
        tool: String,
        /// Arguments, redacted.
        args: Value,
        /// `allowed` (by the profile or an earlier approval), `approved`
        /// (by the user just now), `denied` (by the profile), `refused`
        /// (by the user), `cancelled`.
        decision: String,
        /// Why that decision.
        reason: String,
        /// A redacted summary of the result, if it ran.
        result: Option<String>,
        ok: Option<bool>,
    },
    /// Anything else worth keeping (the session ended, a key was missing).
    Note { text: String },
    /// The harness handed a task to a backend, or heard back (D-100).
    Handoff {
        /// The task's id in its plan (`t2`) and its kind (`coding`).
        task: String,
        task_kind: String,
        /// `claude`, `codex`, or an API backend's name; its model (empty:
        /// the backend's default) and account.
        backend: String,
        model: String,
        account: String,
        /// `started`, `done`, `failed`, `fell-back`, `skipped`, `stopped`.
        outcome: String,
        /// Why, or a short redacted summary of the result.
        detail: String,
    },
}

/// Appends to a session's log.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    file: File,
    session: String,
    seq: u64,
    prev: String,
}

/// The hex SHA-256 of a line (without its newline).
pub fn line_hash(line: &str) -> String {
    let digest = Sha256::digest(line.as_bytes());
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The audit folder in TermOS's state folder.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("audit")
}

impl AuditLog {
    /// Opens (or creates) the log of `session` in `dir`, continuing its
    /// chain. A log that does not verify is not continued: it is renamed
    /// aside with a `.broken` suffix and a new chain starts, which the new
    /// log's first note says.
    pub fn open(dir: &Path, session: &str) -> std::io::Result<Self> {
        crate::secrets::create_private_dir(dir)?;
        let path = dir.join(format!("{session}.jsonl"));
        let mut note = None;
        let (seq, prev) = match verify(&path) {
            Ok(r) => (r.entries, r.head.unwrap_or_else(|| GENESIS.to_owned())),
            Err(VerifyError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                (0, GENESIS.to_owned())
            }
            Err(e) => {
                let aside = path.with_extension("jsonl.broken");
                std::fs::rename(&path, &aside)?;
                note = Some(format!(
                    "the previous log did not verify ({e}); it was kept as {}",
                    aside.display()
                ));
                (0, GENESIS.to_owned())
            }
        };
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&path)?;
        let mut log = Self {
            path,
            file,
            session: session.to_owned(),
            seq,
            prev,
        };
        if let Some(text) = note {
            log.append(Event::Note { text })?;
        }
        Ok(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The hash of the last line written (or [`GENESIS`]).
    pub fn head(&self) -> &str {
        &self.prev
    }

    /// Appends an event; it is on disk when this returns.
    pub fn append(&mut self, event: Event) -> std::io::Result<()> {
        let entry = Entry {
            seq: self.seq,
            time: jiff::Timestamp::now().to_string(),
            prev: self.prev.clone(),
            session: self.session.clone(),
            event,
        };
        let line = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
        self.file.write_all(format!("{line}\n").as_bytes())?;
        self.file.sync_data()?;
        self.prev = line_hash(&line);
        self.seq += 1;
        Ok(())
    }
}

/// A log that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub entries: u64,
    /// The hash of the last line (`None`: empty log).
    pub head: Option<String>,
}

/// Why a log does not verify.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: not an audit entry: {message}")]
    Parse { line: u64, message: String },
    #[error("line {line}: the chain is broken: it does not follow the line before it")]
    Chain { line: u64 },
    #[error("line {line}: entry number {found}, expected {expected}")]
    Sequence {
        line: u64,
        found: u64,
        expected: u64,
    },
    #[error(
        "the log ends at line {entries} with a hash its session does not expect: lines were removed from the end"
    )]
    Truncated { entries: u64 },
}

/// Checks a log's chain from its first line.
pub fn verify(path: &Path) -> Result<Verified, VerifyError> {
    let file = File::open(path)?;
    let mut prev = GENESIS.to_owned();
    let mut n = 0u64;
    for line in BufReader::new(file).lines() {
        let line = line?;
        let entry: Entry = serde_json::from_str(&line).map_err(|e| VerifyError::Parse {
            line: n + 1,
            message: e.to_string(),
        })?;
        if entry.prev != prev {
            return Err(VerifyError::Chain { line: n + 1 });
        }
        if entry.seq != n {
            return Err(VerifyError::Sequence {
                line: n + 1,
                found: entry.seq,
                expected: n,
            });
        }
        prev = line_hash(&line);
        n += 1;
    }
    Ok(Verified {
        entries: n,
        head: (n > 0).then_some(prev),
    })
}

/// Checks a log against the head its session recorded.
pub fn verify_with_head(path: &Path, expected_head: Option<&str>) -> Result<Verified, VerifyError> {
    let v = verify(path)?;
    if let Some(expected) = expected_head
        && v.head.as_deref().unwrap_or(GENESIS) != expected
    {
        return Err(VerifyError::Truncated { entries: v.entries });
    }
    Ok(v)
}

/// Reads every entry (for the viewer). Lines that do not parse are left
/// out; [`verify`] reports them.
pub fn read(path: &Path) -> std::io::Result<Vec<Entry>> {
    let file = File::open(path)?;
    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect())
}

/// The logs in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .map(|p| {
            let t = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (t, p)
        })
        .collect();
    logs.sort_by_key(|l| std::cmp::Reverse(l.0));
    logs.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(n: u32) -> Event {
        Event::Tool {
            id: format!("t{n}"),
            tool: "run_command".into(),
            args: serde_json::json!({"command": format!("echo {n}")}),
            decision: "approved".into(),
            reason: "the user approved".into(),
            result: Some("exit 0".into()),
            ok: Some(true),
        }
    }

    #[test]
    fn a_chain_that_verifies_and_continues() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = AuditLog::open(dir.path(), "s1").expect("open");
        log.append(Event::Note {
            text: "start".into(),
        })
        .expect("append");
        log.append(tool(1)).expect("append");
        let head = log.head().to_owned();
        drop(log);
        let v = verify_with_head(&dir.path().join("s1.jsonl"), Some(&head)).expect("verifies");
        assert_eq!(v.entries, 2);
        // Reopening continues the same chain.
        let mut log = AuditLog::open(dir.path(), "s1").expect("reopen");
        log.append(tool(2)).expect("append");
        let v = verify(log.path()).expect("verifies");
        assert_eq!(v.entries, 3);
        assert_eq!(read(log.path()).expect("read").len(), 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(log.path())
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn tampering_is_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = AuditLog::open(dir.path(), "s").expect("open");
        for n in 0..4 {
            log.append(tool(n)).expect("append");
        }
        let head = log.head().to_owned();
        let path = log.path().to_owned();
        drop(log);
        let text = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = text.lines().collect();

        // An edited argument.
        let edited = text.replacen("echo 2", "echo 9", 1);
        std::fs::write(&path, &edited).expect("write");
        assert!(matches!(verify(&path), Err(VerifyError::Chain { line: 4 })));

        // A removed line.
        let removed = [lines[0], lines[2], lines[3]].join("\n") + "\n";
        std::fs::write(&path, removed).expect("write");
        assert!(matches!(verify(&path), Err(VerifyError::Chain { line: 2 })));

        // The end cut off: only the recorded head shows it.
        let cut = [lines[0], lines[1]].join("\n") + "\n";
        std::fs::write(&path, cut).expect("write");
        assert!(verify(&path).is_ok());
        assert!(matches!(
            verify_with_head(&path, Some(&head)),
            Err(VerifyError::Truncated { entries: 2 })
        ));

        // A broken log is set aside, not continued.
        std::fs::write(&path, edited).expect("write");
        let log = AuditLog::open(dir.path(), "s").expect("open");
        assert!(dir.path().join("s.jsonl.broken").exists());
        let entries = read(log.path()).expect("read");
        assert!(
            matches!(&entries[0].event, Event::Note { text } if text.contains("did not verify"))
        );
        assert!(verify(log.path()).is_ok());
    }
}
