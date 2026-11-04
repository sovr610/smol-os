// SPDX-License-Identifier: GPL-3.0-or-later
//! Claude Code's and Codex CLI's own usage, for the command center
//! (D-094): token counts, models and times from the session logs they
//! write, read as numbers only.
//!
//! - Claude Code: `~/.claude/projects/<project>/<session>.jsonl` (or under
//!   `$CLAUDE_CONFIG_DIR`, or `~/.config/claude`), one line per message;
//!   an assistant message carries `message.model` and `message.usage`. A
//!   message written in several lines (one per content block) is counted
//!   once, by its message and request ids.
//! - Codex CLI: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (or under
//!   `$CODEX_HOME`); `turn_context` lines name the model, `token_count`
//!   events carry the session's running totals, so a turn's use is the
//!   difference.
//!
//! These are the CLIs' own files, not documented interfaces: a line this
//! reader does not understand is skipped. Text fields (prompts, replies,
//! tool output) are skipped by the deserializer, never held or kept.
//! Files are read from where the last read stopped.

use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use termos_core::journal::{Event, Record};

/// The provider name their records carry.
pub const CLAUDE: &str = "claude-code";
pub const CODEX: &str = "codex";

/// The most read from one file at a time (a huge first read is spread
/// over refreshes).
const MAX_READ: u64 = 32 * 1024 * 1024;

/// Where each CLI keeps its logs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dirs {
    pub claude: Vec<PathBuf>,
    pub codex: Vec<PathBuf>,
}

impl Dirs {
    /// From the environment, as the CLIs find them.
    pub fn find(get: &dyn Fn(&str) -> Option<String>, home: Option<&Path>) -> Self {
        let set = |k: &str| get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let claude = match set("CLAUDE_CONFIG_DIR") {
            Some(d) => vec![d.join("projects")],
            None => home
                .map(|h| {
                    vec![
                        h.join(".claude").join("projects"),
                        h.join(".config").join("claude").join("projects"),
                    ]
                })
                .unwrap_or_default(),
        };
        let codex = set("CODEX_HOME")
            .or_else(|| home.map(|h| h.join(".codex")))
            .map(|d| vec![d.join("sessions")])
            .unwrap_or_default();
        Self { claude, codex }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Claude,
    Codex,
}

/// What is known of one file.
#[derive(Debug, Default)]
struct FileState {
    /// Bytes read (up to the end of the last whole line).
    offset: u64,
    /// Codex: the model of the current turn, and the running totals.
    model: Option<String>,
    totals: Option<Totals>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Totals {
    input: u64,
    cached: u64,
    output: u64,
}

/// Reads their logs, a little more each time.
#[derive(Debug, Default)]
pub struct Reader {
    dirs: Dirs,
    files: BTreeMap<PathBuf, FileState>,
    /// Claude Code messages already counted (hashes of their ids).
    seen: HashSet<u64>,
    records: Vec<Record>,
}

impl Reader {
    pub fn new(dirs: Dirs) -> Self {
        Self {
            dirs,
            ..Self::default()
        }
    }

    /// Their usage from `since` (Unix seconds) on, oldest first. Files not
    /// changed since then are not opened.
    pub fn read(&mut self, since: i64) -> Vec<Record> {
        let mut found = Vec::new();
        for d in &self.dirs.claude {
            walk(d, 3, &mut found, Kind::Claude);
        }
        for d in &self.dirs.codex {
            walk(d, 4, &mut found, Kind::Codex);
        }
        for (path, kind) in found {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_secs()).ok())
                .unwrap_or(i64::MAX);
            let tracked = self.files.contains_key(&path);
            if !tracked && modified < since {
                continue;
            }
            let state = self.files.entry(path.clone()).or_default();
            if meta.len() < state.offset {
                // Rewritten: read it again.
                *state = FileState::default();
            }
            if meta.len() == state.offset {
                continue;
            }
            let session = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Ok(text) = read_from(&path, state.offset) else {
                continue;
            };
            // Only whole lines; the rest waits for its newline.
            let Some(end) = text.iter().rposition(|b| *b == b'\n') else {
                continue;
            };
            state.offset += end as u64 + 1;
            for line in text[..end].split(|b| *b == b'\n') {
                let Ok(line) = std::str::from_utf8(line) else {
                    continue;
                };
                let record = match kind {
                    Kind::Claude => claude_line(line, &session, &mut self.seen),
                    Kind::Codex => codex_line(line, &session, state),
                };
                self.records.extend(record);
            }
        }
        self.records.sort_by_key(|r| r.time);
        // Kept as long as the longest range shows.
        let oldest = since.min(termos_core::journal::now() - 8 * 86_400);
        self.records.retain(|r| r.time >= oldest);
        self.records
            .iter()
            .filter(|r| r.time >= since)
            .cloned()
            .collect()
    }
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, Kind)>, kind: Kind) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() && depth > 0 {
            walk(&path, depth - 1, out, kind);
        } else if t.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
            out.push((path, kind));
        }
    }
}

fn read_from(path: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    f.take(MAX_READ).read_to_end(&mut buf)?;
    Ok(buf)
}

fn time(stamp: Option<&str>) -> Option<i64> {
    stamp?
        .parse::<jiff::Timestamp>()
        .ok()
        .map(jiff::Timestamp::as_second)
}

/// Only names and numbers: everything else in a line is skipped unread.
#[derive(Deserialize)]
struct ClaudeLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClaudeUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

fn claude_line(line: &str, session: &str, seen: &mut HashSet<u64>) -> Option<Record> {
    if !line.contains("\"usage\"") {
        return None;
    }
    let l: ClaudeLine = serde_json::from_str(line).ok()?;
    if l.kind.as_deref() != Some("assistant") {
        return None;
    }
    let m = l.message?;
    let u = m.usage?;
    let total = u.input_tokens
        + u.output_tokens
        + u.cache_creation_input_tokens
        + u.cache_read_input_tokens;
    let model = m.model.unwrap_or_default();
    // Claude Code's own placeholder messages carry no use.
    if total == 0 || model.starts_with('<') {
        return None;
    }
    if let Some(id) = &m.id {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        l.request_id.hash(&mut h);
        if !seen.insert(h.finish()) {
            return None;
        }
    }
    Some(Record {
        time: time(l.timestamp.as_deref())?,
        event: Event::Usage {
            session: session.to_owned(),
            provider: CLAUDE.into(),
            model,
            input: u.input_tokens,
            output: u.output_tokens,
            cache_read: u.cache_read_input_tokens,
            cache_write: u.cache_creation_input_tokens,
            cost: None,
        },
    })
}

#[derive(Deserialize)]
struct CodexLine {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    total_token_usage: Option<CodexUsage>,
    last_token_usage: Option<CodexUsage>,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(default)]
struct CodexUsage {
    /// Includes the cached part.
    input_tokens: u64,
    cached_input_tokens: u64,
    /// Includes reasoning.
    output_tokens: u64,
}

impl From<CodexUsage> for Totals {
    fn from(u: CodexUsage) -> Self {
        Self {
            input: u.input_tokens,
            cached: u.cached_input_tokens,
            output: u.output_tokens,
        }
    }
}

fn codex_line(line: &str, session: &str, state: &mut FileState) -> Option<Record> {
    if !line.contains("\"turn_context\"") && !line.contains("\"token_count\"") {
        return None;
    }
    let l: CodexLine = serde_json::from_str(line).ok()?;
    let p = l.payload?;
    if l.kind.as_deref() == Some("turn_context") {
        state.model = p.model.filter(|m| !m.is_empty());
        return None;
    }
    if p.kind.as_deref() != Some("token_count") {
        return None;
    }
    let info = p.info?;
    let used: Totals = match (info.total_token_usage, state.totals) {
        (Some(total), Some(before)) => {
            let total = Totals::from(total);
            state.totals = Some(total);
            if total.input < before.input || total.output < before.output {
                // The totals started again (a new conversation).
                info.last_token_usage.map(Totals::from).unwrap_or(total)
            } else {
                Totals {
                    input: total.input - before.input,
                    cached: total.cached.saturating_sub(before.cached),
                    output: total.output - before.output,
                }
            }
        }
        (Some(total), None) => {
            let total = Totals::from(total);
            state.totals = Some(total);
            total
        }
        (None, _) => info.last_token_usage.map(Totals::from)?,
    };
    if used == Totals::default() {
        // The same totals again.
        return None;
    }
    Some(Record {
        time: time(l.timestamp.as_deref())?,
        event: Event::Usage {
            session: session.to_owned(),
            provider: CODEX.into(),
            model: state.model.clone().unwrap_or_else(|| "codex".into()),
            input: used.input.saturating_sub(used.cached),
            output: used.output,
            cache_read: used.cached,
            cache_write: 0,
            cost: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures written from the formats as they are known (2026); they are
    // not copied from anyone's logs. The text fields hold markers that
    // must never come out.

    const CLAUDE_LOG: &str = concat!(
        r#"{"type":"user","timestamp":"2026-11-04T10:00:00.000Z","sessionId":"s1","message":{"role":"user","content":"SECRET PROMPT"}}"#,
        "\n",
        // One message in two lines (two content blocks): counted once.
        r#"{"type":"assistant","timestamp":"2026-11-04T10:00:05.000Z","requestId":"req_1","message":{"id":"msg_1","model":"fixture-claude","content":[{"type":"thinking","thinking":"SECRET THOUGHT"}],"usage":{"input_tokens":10,"output_tokens":200,"cache_creation_input_tokens":1000,"cache_read_input_tokens":5000}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-11-04T10:00:06.000Z","requestId":"req_1","message":{"id":"msg_1","model":"fixture-claude","content":[{"type":"text","text":"SECRET REPLY"}],"usage":{"input_tokens":10,"output_tokens":200,"cache_creation_input_tokens":1000,"cache_read_input_tokens":5000}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-11-04T10:01:00.000Z","message":{"id":"msg_x","model":"<synthetic>","content":[],"usage":{"input_tokens":0,"output_tokens":0}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-11-04T10:02:00.000Z","requestId":"req_2","message":{"id":"msg_2","model":"fixture-claude","usage":{"input_tokens":3,"output_tokens":50,"cache_read_input_tokens":6000}}}"#,
        "\n",
        "not json at all\n",
    );

    const CODEX_LOG: &str = concat!(
        r#"{"timestamp":"2026-11-04T11:00:00.000Z","type":"session_meta","payload":{"id":"x","instructions":"SECRET INSTRUCTIONS"}}"#,
        "\n",
        r#"{"timestamp":"2026-11-04T11:00:01.000Z","type":"turn_context","payload":{"cwd":"/w","model":"fixture-codex"}}"#,
        "\n",
        r#"{"timestamp":"2026-11-04T11:00:02.000Z","type":"response_item","payload":{"type":"message","content":[{"type":"input_text","text":"SECRET PROMPT"}]}}"#,
        "\n",
        r#"{"timestamp":"2026-11-04T11:00:09.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":100,"reasoning_output_tokens":60,"total_tokens":1100},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":100}}}}"#,
        "\n",
        // The same totals again: nothing new.
        r#"{"timestamp":"2026-11-04T11:00:10.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":100}}}}"#,
        "\n",
        r#"{"timestamp":"2026-11-04T11:05:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2500,"cached_input_tokens":1400,"output_tokens":180}}}}"#,
        "\n",
        r#"{"timestamp":"2026-11-04T11:06:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#,
        "\n",
    );

    fn usage(r: &Record) -> (&str, &str, u64, u64, u64, u64) {
        match &r.event {
            Event::Usage {
                provider,
                model,
                input,
                output,
                cache_read,
                cache_write,
                ..
            } => (
                provider.as_str(),
                model.as_str(),
                *input,
                *output,
                *cache_read,
                *cache_write,
            ),
            other => panic!("not usage: {other:?}"),
        }
    }

    fn setup() -> (tempfile::TempDir, Reader) {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let project = home.join(".claude/projects/-w-proj");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::write(project.join("s1.jsonl"), CLAUDE_LOG).expect("write");
        let day = home.join(".codex/sessions/2026/11/04");
        std::fs::create_dir_all(&day).expect("mkdir");
        std::fs::write(day.join("rollout-2026-11-04T11-00-00-x.jsonl"), CODEX_LOG).expect("write");
        let dirs = Dirs::find(&|_| None, Some(home));
        (dir, Reader::new(dirs))
    }

    #[test]
    fn reads_numbers_and_nothing_else() {
        let (_dir, mut reader) = setup();
        let got = reader.read(0);
        let all: Vec<_> = got.iter().map(usage).collect();
        assert_eq!(
            all,
            [
                (CLAUDE, "fixture-claude", 10, 200, 5000, 1000),
                (CLAUDE, "fixture-claude", 3, 50, 6000, 0),
                (CODEX, "fixture-codex", 600, 100, 400, 0),
                (CODEX, "fixture-codex", 500, 80, 1000, 0),
            ]
        );
        let text = format!("{got:?}");
        assert!(!text.contains("SECRET"), "{text}");
        // Times are the lines' own.
        assert_eq!(got[0].time, time(Some("2026-11-04T10:00:05Z")).expect("t"));
        // Read again: nothing new, nothing counted twice.
        assert_eq!(reader.read(0).len(), 4);
        // From a later time on: only what came after.
        let later = time(Some("2026-11-04T11:00:00Z")).expect("t");
        assert_eq!(reader.read(later).len(), 2);
    }

    #[test]
    fn appended_lines_are_read_once_and_half_lines_wait() {
        use std::io::Write as _;
        let (dir, mut reader) = setup();
        assert_eq!(reader.read(0).len(), 4);
        let file = dir.path().join(".claude/projects/-w-proj/s1.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("open");
        let line = r#"{"type":"assistant","timestamp":"2026-11-04T12:00:00.000Z","requestId":"req_3","message":{"id":"msg_3","model":"fixture-claude","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        let (head, tail) = line.split_at(40);
        f.write_all(head.as_bytes()).expect("write");
        f.flush().expect("flush");
        assert_eq!(reader.read(0).len(), 4, "half a line waits");
        f.write_all(tail.as_bytes()).expect("write");
        f.write_all(b"\n").expect("write");
        f.flush().expect("flush");
        let got = reader.read(0);
        assert_eq!(got.len(), 5);
        assert_eq!(usage(&got[4]).2, 1);
    }

    #[test]
    fn their_folders_follow_their_settings() {
        let env = |k: &str| match k {
            "CLAUDE_CONFIG_DIR" => Some("/c".to_owned()),
            "CODEX_HOME" => Some("/x".to_owned()),
            _ => None,
        };
        let d = Dirs::find(&env, Some(Path::new("/h")));
        assert_eq!(d.claude, [PathBuf::from("/c/projects")]);
        assert_eq!(d.codex, [PathBuf::from("/x/sessions")]);
        let d = Dirs::find(&|_| None, None);
        assert!(d.claude.is_empty() && d.codex.is_empty());
    }
}
