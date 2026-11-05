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
//! - Gemini CLI: `~/.gemini/tmp/<project>/chats/session-*.jsonl` (or under
//!   `$GEMINI_CLI_HOME/.gemini`), one line per message, a message written
//!   again once its tokens are known: a `gemini` message with `tokens` and
//!   `model` is counted once, by its id (D-112).
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
pub const GEMINI: &str = "gemini-cli";

/// The most read from one file at a time (a huge first read is spread
/// over refreshes).
const MAX_READ: u64 = 32 * 1024 * 1024;

/// Where each CLI keeps its logs (the CLIs' own folders, before accounts).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dirs {
    pub claude: Vec<PathBuf>,
    pub codex: Vec<PathBuf>,
    pub gemini: Vec<PathBuf>,
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
        let gemini = set("GEMINI_CLI_HOME")
            .or_else(|| home.map(Path::to_owned))
            .map(|d| vec![d.join(".gemini").join("tmp")])
            .unwrap_or_default();
        Self {
            claude,
            codex,
            gemini,
        }
    }

    /// As the `default` account of each CLI.
    pub fn sources(self) -> Vec<Source> {
        vec![
            Source {
                cli: "claude",
                account: "default".into(),
                dirs: self.claude,
            },
            Source {
                cli: "codex",
                account: "default".into(),
                dirs: self.codex,
            },
            Source {
                cli: "gemini",
                account: "default".into(),
                dirs: self.gemini,
            },
        ]
    }
}

/// One account's logs (D-097).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// `claude` or `codex`.
    pub cli: &'static str,
    pub account: String,
    /// Claude Code's `projects/` folders, or Codex's `sessions/`.
    pub dirs: Vec<PathBuf>,
}

impl Source {
    /// An account's logs, from its folder (`CLAUDE_CONFIG_DIR`,
    /// `CODEX_HOME`, or Gemini CLI's `.gemini`).
    pub fn of(cli: &'static str, account: &str, home: &Path) -> Self {
        let dir = match cli {
            "claude" => "projects",
            "gemini" => "tmp",
            _ => "sessions",
        };
        Self {
            cli,
            account: account.to_owned(),
            dirs: vec![home.join(dir)],
        }
    }
}

pub use termos_devkit::accounts::{LimitWindow, Limits};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Claude,
    Codex,
    Gemini,
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
    sources: Vec<Source>,
    files: BTreeMap<PathBuf, FileState>,
    /// Claude Code messages already counted (hashes of their ids).
    seen: HashSet<u64>,
    /// Each record with the source it came from.
    records: Vec<(usize, Record)>,
    /// The newest limits each source reported.
    limits: BTreeMap<usize, Limits>,
}

impl Reader {
    /// The CLIs' own folders (their `default` accounts).
    pub fn new(dirs: Dirs) -> Self {
        Self::with_sources(dirs.sources())
    }

    /// Every account's logs.
    pub fn with_sources(sources: Vec<Source>) -> Self {
        Self {
            sources,
            ..Self::default()
        }
    }

    /// Reads these accounts from now on (all is read again when they
    /// changed).
    pub fn set_sources(&mut self, sources: Vec<Source>) {
        if sources != self.sources {
            *self = Self::with_sources(sources);
        }
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// The newest limits source `i` reported.
    pub fn limits(&self, i: usize) -> Option<&Limits> {
        self.limits.get(&i)
    }

    /// Their usage from `since` (Unix seconds) on, oldest first. Files not
    /// changed since then are not opened.
    pub fn read(&mut self, since: i64) -> Vec<Record> {
        self.read_by_source(since)
            .into_iter()
            .map(|(_, r)| r)
            .collect()
    }

    /// Like [`Reader::read`], each record with the index of its source.
    pub fn read_by_source(&mut self, since: i64) -> Vec<(usize, Record)> {
        let mut found = Vec::new();
        for (i, src) in self.sources.iter().enumerate() {
            let (kind, depth) = match src.cli {
                "claude" => (Kind::Claude, 3),
                // tmp/<project>/chats/<parent>/<sub-agent>.jsonl
                "gemini" => (Kind::Gemini, 4),
                _ => (Kind::Codex, 4),
            };
            for d in &src.dirs {
                walk(d, depth, &mut found, kind, i);
            }
        }
        for (path, kind, source) in found {
            // Gemini CLI's folder holds more than its chats.
            if kind == Kind::Gemini && !path.components().any(|c| c.as_os_str() == "chats") {
                continue;
            }
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
                match kind {
                    Kind::Claude => {
                        if let Some(r) = claude_line(line, &session, &mut self.seen) {
                            self.records.push((source, r));
                        }
                    }
                    Kind::Gemini => {
                        if let Some(r) = gemini_line(line, &session, &mut self.seen) {
                            self.records.push((source, r));
                        }
                    }
                    Kind::Codex => {
                        let (record, limits) = codex_line(line, &session, state);
                        if let Some(r) = record {
                            self.records.push((source, r));
                        }
                        if let Some(l) = limits
                            && self
                                .limits
                                .get(&source)
                                .is_none_or(|old| old.time <= l.time)
                        {
                            self.limits.insert(source, l);
                        }
                    }
                }
            }
        }
        self.records.sort_by_key(|(_, r)| r.time);
        // Kept as long as the longest range shows.
        let oldest = since.min(termos_core::journal::now() - 8 * 86_400);
        self.records.retain(|(_, r)| r.time >= oldest);
        self.records
            .iter()
            .filter(|(_, r)| r.time >= since)
            .cloned()
            .collect()
    }
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, Kind, usize)>, kind: Kind, source: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() && depth > 0 {
            walk(&path, depth - 1, out, kind, source);
        } else if t.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
            out.push((path, kind, source));
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
struct GeminiLine {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    tokens: Option<GeminiTokens>,
    model: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GeminiTokens {
    input: u64,
    output: u64,
    cached: u64,
    thoughts: u64,
}

/// A Gemini CLI message with its tokens: the prompt's uncached part as
/// input, thinking counted as output (as it is billed), cached reads apart.
fn gemini_line(line: &str, session: &str, seen: &mut HashSet<u64>) -> Option<Record> {
    if !line.contains("\"tokens\"") {
        return None;
    }
    let l: GeminiLine = serde_json::from_str(line).ok()?;
    if l.kind.as_deref() != Some("gemini") {
        return None;
    }
    let t = l.tokens?;
    let output = t.output + t.thoughts;
    if t.input + output == 0 {
        return None;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    session.hash(&mut h);
    l.id?.hash(&mut h);
    if !seen.insert(h.finish()) {
        return None;
    }
    Some(Record {
        time: time(l.timestamp.as_deref())?,
        event: Event::Usage {
            session: session.to_owned(),
            provider: GEMINI.into(),
            model: l.model.unwrap_or_default(),
            input: t.input.saturating_sub(t.cached),
            output,
            cache_read: t.cached,
            cache_write: 0,
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
    rate_limits: Option<CodexRateLimits>,
}

/// Codex's plan limits, as its logs record them (`primary` is the shorter
/// window, `secondary` the longer).
#[derive(Deserialize)]
struct CodexRateLimits {
    primary: Option<CodexWindow>,
    secondary: Option<CodexWindow>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct CodexWindow {
    used_percent: Option<f64>,
    window_minutes: Option<u64>,
    /// Unix seconds (an RFC 3339 string in some versions).
    resets_at: Option<serde_json::Value>,
    /// Older versions: seconds from the line's time.
    resets_in_seconds: Option<i64>,
}

impl CodexWindow {
    fn window(&self, line_time: i64) -> Option<LimitWindow> {
        let resets_at = match &self.resets_at {
            Some(serde_json::Value::Number(n)) => n.as_i64(),
            Some(serde_json::Value::String(s)) => time(Some(s)),
            _ => self.resets_in_seconds.map(|s| line_time + s),
        };
        Some(LimitWindow {
            minutes: self.window_minutes?,
            used_percent: self.used_percent? as f32,
            resets_at,
        })
    }
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

/// A Codex line's usage and the plan limits it reports.
fn codex_line(
    line: &str,
    session: &str,
    state: &mut FileState,
) -> (Option<Record>, Option<Limits>) {
    if !line.contains("\"turn_context\"") && !line.contains("\"token_count\"") {
        return (None, None);
    }
    let Ok(l) = serde_json::from_str::<CodexLine>(line) else {
        return (None, None);
    };
    let Some(p) = l.payload else {
        return (None, None);
    };
    if l.kind.as_deref() == Some("turn_context") {
        state.model = p.model.filter(|m| !m.is_empty());
        return (None, None);
    }
    if p.kind.as_deref() != Some("token_count") {
        return (None, None);
    }
    let Some(line_time) = time(l.timestamp.as_deref()) else {
        return (None, None);
    };
    let limits = p.rate_limits.and_then(|r| {
        let windows: Vec<LimitWindow> = [r.primary, r.secondary]
            .iter()
            .flatten()
            .filter_map(|w| w.window(line_time))
            .collect();
        (!windows.is_empty()).then(|| Limits {
            time: line_time,
            plan: r.plan_type.filter(|p| !p.is_empty() && p != "unknown"),
            windows,
        })
    });
    (codex_usage(p.info, session, state, line_time), limits)
}

fn codex_usage(
    info: Option<CodexInfo>,
    session: &str,
    state: &mut FileState,
    line_time: i64,
) -> Option<Record> {
    let info = info?;
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
        time: line_time,
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

    #[test]
    fn gemini_messages_count_once_when_their_tokens_arrive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join(".gemini");
        let chats = home.join("tmp/work/chats");
        std::fs::create_dir_all(&chats).expect("mkdir");
        // Not a chat: never read.
        std::fs::write(home.join("tmp/work/logs.jsonl"), "{\"tokens\":{}}\n").expect("write");
        let lines = [
            r#"{"sessionId":"s1","projectHash":"h","startTime":"2026-11-05T10:00:00.000Z"}"#,
            r#"{"id":"u1","timestamp":"2026-11-05T10:00:01.000Z","type":"user","content":"PRIVATE PROMPT"}"#,
            r#"{"id":"g1","timestamp":"2026-11-05T10:00:02.000Z","type":"gemini","content":"PRIVATE REPLY","model":"fixture-gemini"}"#,
            r#"{"id":"g1","timestamp":"2026-11-05T10:00:02.000Z","type":"gemini","content":"PRIVATE REPLY","model":"fixture-gemini","tokens":{"input":1200,"output":80,"cached":1000,"thoughts":20,"tool":0,"total":1300}}"#,
            r#"{"$set":{"lastUpdated":"2026-11-05T10:00:03.000Z"}}"#,
            r#"{"id":"g1","timestamp":"2026-11-05T10:00:02.000Z","type":"gemini","model":"fixture-gemini","tokens":{"input":1200,"output":80,"cached":1000,"thoughts":20,"tool":0,"total":1300}}"#,
        ];
        std::fs::write(
            chats.join("session-2026-11-05T10-00-s1.jsonl"),
            lines.join("\n") + "\n",
        )
        .expect("write");
        let mut r = Reader::with_sources(vec![Source::of("gemini", "default", &home)]);
        let got = r.read(0);
        assert_eq!(got.len(), 1, "{got:?}");
        let Event::Usage {
            provider,
            model,
            input,
            output,
            cache_read,
            ..
        } = &got[0].event
        else {
            panic!("usage");
        };
        assert_eq!(
            (
                provider.as_str(),
                model.as_str(),
                *input,
                *output,
                *cache_read
            ),
            (GEMINI, "fixture-gemini", 200, 100, 1000)
        );
        assert!(!format!("{got:?}").contains("PRIVATE"));
        let d = Dirs::find(&|k| (k == "GEMINI_CLI_HOME").then(|| "/g".to_owned()), None);
        assert_eq!(d.gemini, [PathBuf::from("/g/.gemini/tmp")]);
    }

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

    #[test]
    fn each_account_is_read_on_its_own_with_codex_limits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("work");
        let play = dir.path().join("play");
        for (home, model) in [(&work, "fixture-a"), (&play, "fixture-b")] {
            let p = home.join("projects/-w");
            std::fs::create_dir_all(&p).expect("mkdir");
            std::fs::write(
                p.join("s.jsonl"),
                format!(
                    "{{\"type\":\"assistant\",\"timestamp\":\"2026-11-04T10:00:00Z\",\"requestId\":\"r\",\"message\":{{\"id\":\"m-{model}\",\"model\":\"{model}\",\"usage\":{{\"input_tokens\":5,\"output_tokens\":7}}}}}}\n"
                ),
            )
            .expect("write");
        }
        let codex = dir.path().join("codex");
        let day = codex.join("sessions/2026/11/04");
        std::fs::create_dir_all(&day).expect("mkdir");
        // The shape Codex's own tests record, then a newer reading, then
        // an older version's shape (seconds until the reset).
        let lines = concat!(
            r#"{"timestamp":"2026-11-04T12:00:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":null,"limit_name":null,"primary":{"used_percent":0.0,"window_minutes":300,"resets_at":1800000000},"secondary":{"used_percent":12.5,"window_minutes":10080,"resets_at":1800100000},"credits":null,"individual_limit":null,"spend_control_reached":null,"plan_type":"pro","rate_limit_reached_type":null}}}"#,
            "\n",
            r#"{"timestamp":"2026-11-04T12:30:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":40.5,"window_minutes":300,"resets_at":1800000000},"secondary":{"used_percent":20.0,"window_minutes":10080,"resets_at":1800100000},"plan_type":"pro"}}}"#,
            "\n",
        );
        std::fs::write(day.join("rollout-a.jsonl"), lines).expect("write");
        let old = r#"{"timestamp":"2026-11-04T11:00:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":3.0,"window_minutes":300,"resets_in_seconds":60}}}}"#;
        std::fs::write(day.join("rollout-b.jsonl"), format!("{old}\n")).expect("write");

        let mut r = Reader::with_sources(vec![
            Source::of("claude", "work", &work),
            Source::of("claude", "play", &play),
            Source::of("codex", "default", &codex),
        ]);
        let got = r.read_by_source(0);
        let by: Vec<(usize, &str)> = got.iter().map(|(i, rec)| (*i, usage(rec).1)).collect();
        assert!(
            by.contains(&(0, "fixture-a")) && by.contains(&(1, "fixture-b")),
            "{by:?}"
        );
        let l = r.limits(2).expect("codex limits");
        assert_eq!(l.plan.as_deref(), Some("pro"));
        assert_eq!(l.windows.len(), 2);
        assert_eq!(
            (l.windows[0].minutes, l.windows[0].used_percent),
            (300, 40.5),
            "the newest"
        );
        assert_eq!(l.windows[1].resets_at, Some(1_800_100_000));
        assert!(r.limits(0).is_none());
        // Accounts changed: read again for the new set.
        r.set_sources(vec![Source::of("codex", "default", &codex)]);
        assert!(r.read_by_source(0).is_empty());
        let only_old =
            Reader::with_sources(vec![Source::of("codex", "x", &dir.path().join("none"))]);
        assert!(only_old.limits(0).is_none());
    }

    #[test]
    fn older_codex_limits_count_from_the_line() {
        let mut state = FileState::default();
        let line = r#"{"timestamp":"2026-11-04T11:00:00Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":3.0,"window_minutes":300,"resets_in_seconds":60}}}}"#;
        let (rec, limits) = codex_line(line, "s", &mut state);
        assert!(rec.is_none(), "no usage in it");
        let l = limits.expect("limits");
        let at = time(Some("2026-11-04T11:00:00Z")).expect("t");
        assert_eq!(l.windows[0].resets_at, Some(at + 60));
        assert_eq!(l.plan, None);
    }
}
