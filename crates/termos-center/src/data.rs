// SPDX-License-Identifier: GPL-3.0-or-later
//! What the command center shows (D-093), gathered off the server loop: a
//! [`Gather`] runs on the window's own thread and answers [`Job`]s with
//! [`Done`]s. [`Live`] reads the real machine, the server's control API,
//! the journal, the audit logs, the logs and the model cache; tests give a
//! fixture instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use termos_core::config::Config;
use termos_core::journal::{Event, Journal, Record};
use termos_core::paths::Paths;
use termos_hw::{Sample, Source};

/// How far back the history looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Range {
    Hour,
    Day,
    Week,
}

impl Range {
    pub const ALL: [Range; 3] = [Range::Hour, Range::Day, Range::Week];

    pub fn secs(self) -> i64 {
        match self {
            Range::Hour => 3_600,
            Range::Day => 86_400,
            Range::Week => 7 * 86_400,
        }
    }

    /// How many points its charts have.
    pub fn buckets(self) -> usize {
        match self {
            Range::Hour => 60,
            Range::Day => 96,
            Range::Week => 168,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Range::Hour => "1 h",
            Range::Day => "24 h",
            Range::Week => "7 days",
        }
    }
}

/// Tokens and cost of one model over the range.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub turns: u64,
    /// US dollars; `None` when no turn had a price.
    pub cost: Option<f64>,
}

/// The journal over a range: series for the charts and totals.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub range: Range,
    /// Mean CPU per bucket (percent), `None` where nothing was sampled.
    pub cpu: Vec<Option<f32>>,
    /// Mean memory use per bucket (percent).
    pub memory: Vec<Option<f32>>,
    /// Tokens (in and out) per bucket.
    pub tokens: Vec<u64>,
    /// Dollars per bucket.
    pub cost: Vec<f64>,
    /// By model, most expensive (then most used) first.
    pub models: Vec<ModelUsage>,
    /// Tool calls by decision (`allowed`, `approved`, …).
    pub decisions: BTreeMap<String, u64>,
    /// The most called tools.
    pub tools: Vec<(String, u64)>,
    pub sessions: u64,
    /// What was done from the command center, newest first.
    pub actions: Vec<(i64, String)>,
}

impl History {
    pub fn empty(range: Range) -> Self {
        let n = range.buckets();
        Self {
            range,
            cpu: vec![None; n],
            memory: vec![None; n],
            tokens: vec![0; n],
            cost: vec![0.0; n],
            models: Vec::new(),
            decisions: BTreeMap::new(),
            tools: Vec::new(),
            sessions: 0,
            actions: Vec::new(),
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.tokens.iter().sum()
    }

    pub fn total_cost(&self) -> Option<f64> {
        self.models
            .iter()
            .filter_map(|m| m.cost)
            .fold(None, |acc, c| Some(acc.unwrap_or(0.0) + c))
    }

    /// Calls the user (or a profile) said no to.
    pub fn refused(&self) -> u64 {
        ["denied", "refused"]
            .iter()
            .filter_map(|d| self.decisions.get(*d))
            .sum()
    }
}

/// Sums `records` (oldest first) into the `range` ending at `now`.
pub fn history(records: &[Record], range: Range, now: i64) -> History {
    let mut h = History::empty(range);
    let start = now - range.secs();
    let n = range.buckets();
    let width = range.secs() / i64::try_from(n).unwrap_or(1);
    let bucket = |t: i64| -> Option<usize> {
        if t < start || t > now {
            return None;
        }
        usize::try_from((t - start) / width.max(1))
            .ok()
            .map(|b| b.min(n - 1))
    };
    let mut cpu_sum = vec![(0.0f64, 0u32); n];
    let mut mem_sum = vec![(0.0f64, 0u32); n];
    let mut models: BTreeMap<(String, String), ModelUsage> = BTreeMap::new();
    let mut tools: BTreeMap<String, u64> = BTreeMap::new();
    for r in records {
        let Some(b) = bucket(r.time) else { continue };
        match &r.event {
            Event::System {
                cpu,
                memory_used,
                memory_total,
                ..
            } => {
                cpu_sum[b].0 += f64::from(*cpu);
                cpu_sum[b].1 += 1;
                if *memory_total > 0 {
                    mem_sum[b].0 += *memory_used as f64 * 100.0 / *memory_total as f64;
                    mem_sum[b].1 += 1;
                }
            }
            Event::Usage {
                provider,
                model,
                input,
                output,
                cache_read,
                cache_write,
                cost,
                ..
            } => {
                let tokens = input + output + cache_read + cache_write;
                h.tokens[b] += tokens;
                h.cost[b] += cost.unwrap_or(0.0);
                let m = models
                    .entry((provider.clone(), model.clone()))
                    .or_insert_with(|| ModelUsage {
                        provider: provider.clone(),
                        model: model.clone(),
                        ..ModelUsage::default()
                    });
                m.input += input + cache_read + cache_write;
                m.output += output;
                m.turns += 1;
                if let Some(c) = cost {
                    m.cost = Some(m.cost.unwrap_or(0.0) + c);
                }
            }
            Event::Tool { tool, decision, .. } => {
                *h.decisions.entry(decision.clone()).or_default() += 1;
                *tools.entry(tool.clone()).or_default() += 1;
            }
            Event::Session { state, .. } => {
                if state == "started" {
                    h.sessions += 1;
                }
            }
            Event::Action { what } => h.actions.push((r.time, what.clone())),
        }
    }
    let mean = |(sum, n): (f64, u32)| (n > 0).then(|| (sum / f64::from(n)) as f32);
    h.cpu = cpu_sum.into_iter().map(mean).collect();
    h.memory = mem_sum.into_iter().map(mean).collect();
    h.models = models.into_values().collect();
    h.models.sort_by(|a, b| {
        b.cost
            .unwrap_or(0.0)
            .total_cmp(&a.cost.unwrap_or(0.0))
            .then(b.turns.cmp(&a.turns))
    });
    let mut tools: Vec<(String, u64)> = tools.into_iter().collect();
    tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    tools.truncate(8);
    h.tools = tools;
    h.actions.reverse();
    h.actions.truncate(20);
    h
}

/// A session of the server (`session.list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub name: String,
    pub workspaces: u64,
    pub panes: u64,
    pub clients: u64,
}

/// A pane of any session (`pane.list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    pub id: u64,
    pub session: String,
    pub name: String,
    /// `term` or `app`.
    pub kind: String,
    pub focused: bool,
}

/// The audit logs (D-062).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditInfo {
    pub logs: usize,
    /// Sessions whose chain does not verify.
    pub broken: Vec<String>,
}

/// Local models (M7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelsInfo {
    /// The router's address, when it answers.
    pub router: Option<String>,
    /// Repositories in the cache, with their size in bytes.
    pub installed: Vec<(String, u64)>,
}

/// A CLI agent preset and how many windows run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAgent {
    pub name: String,
    pub description: String,
    pub running: usize,
}

/// One look at everything but the live agents (those come from the
/// registry, read on the server loop).
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Unix seconds.
    pub taken: i64,
    pub sample: Option<Sample>,
    pub sessions: Vec<SessionRow>,
    pub panes: Vec<PaneRow>,
    /// Why sessions and panes are missing.
    pub control_error: Option<String>,
    pub history: History,
    pub audit: AuditInfo,
    /// The newest log's last lines.
    pub logs: Vec<String>,
    pub log_file: Option<PathBuf>,
    pub models: ModelsInfo,
    /// MCP servers: name and what runs it.
    pub mcp: Vec<(String, String)>,
    pub external: Vec<ExternalAgent>,
    pub version: String,
}

impl Snapshot {
    pub fn empty(range: Range) -> Self {
        Self {
            taken: 0,
            sample: None,
            sessions: Vec::new(),
            panes: Vec::new(),
            control_error: None,
            history: History::empty(range),
            audit: AuditInfo::default(),
            logs: Vec::new(),
            log_file: None,
            models: ModelsInfo::default(),
            mcp: Vec::new(),
            external: Vec::new(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// What the window asks its thread for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// A fresh snapshot; `journal`: read the history again too.
    Refresh {
        range: Range,
        journal: bool,
    },
    EndProcess {
        pid: u32,
        name: String,
    },
    ClosePane(u64),
    GoToPane(u64),
}

/// What the thread answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    Snapshot(Box<Snapshot>),
    /// An action's outcome (`true`: it failed).
    Message(String, bool),
}

/// Where the data comes from. Made on the window's thread (the machine's
/// sampler must stay on the thread that made it).
pub trait Gather {
    fn snapshot(&mut self, range: Range, journal: bool) -> Snapshot;
    fn end_process(&mut self, pid: u32, name: &str) -> Result<String, String>;
    fn close_pane(&mut self, id: u64) -> Result<String, String>;
    fn go_to_pane(&mut self, id: u64) -> Result<String, String>;
}

/// Answers `job` with `gather`.
pub fn answer(gather: &mut dyn Gather, job: Job) -> Done {
    let message = |r: Result<String, String>| match r {
        Ok(m) => Done::Message(m, false),
        Err(m) => Done::Message(m, true),
    };
    match job {
        Job::Refresh { range, journal } => {
            Done::Snapshot(Box::new(gather.snapshot(range, journal)))
        }
        Job::EndProcess { pid, name } => message(gather.end_process(pid, &name)),
        Job::ClosePane(id) => message(gather.close_pane(id)),
        Job::GoToPane(id) => message(gather.go_to_pane(id)),
    }
}

/// The real sources.
pub struct Live {
    socket: Option<PathBuf>,
    journal: Arc<Journal>,
    paths: Paths,
    config: Config,
    /// Made on the thread that samples (it is not `Send`).
    sampler: Option<termos_hw::Sampler>,
    runtime: Option<tokio::runtime::Runtime>,
    /// The history, read again only when asked (the journal is big).
    history: Option<History>,
    /// Audit logs verified, by path and size, so each is read once.
    verified: BTreeMap<PathBuf, (u64, bool)>,
    /// Claude Code's and Codex CLI's own usage (`[center] cli-usage`).
    theirs: Option<crate::theirs::Reader>,
    /// Their logs were given (tests), not found from the accounts.
    theirs_given: bool,
}

impl Live {
    pub fn new(
        socket: Option<PathBuf>,
        journal: Arc<Journal>,
        paths: Paths,
        config: Config,
    ) -> Self {
        Self {
            socket,
            journal,
            paths,
            config,
            sampler: None,
            runtime: None,
            history: None,
            verified: BTreeMap::new(),
            theirs: None,
            theirs_given: false,
        }
    }

    /// Reads the CLIs' usage from `dirs` (rather than where the
    /// environment says).
    #[must_use]
    pub fn with_cli_usage(mut self, dirs: crate::theirs::Dirs) -> Self {
        self.theirs = Some(crate::theirs::Reader::new(dirs));
        self.theirs_given = true;
        self
    }

    fn call(&mut self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let socket = self
            .socket
            .clone()
            .ok_or_else(|| "this window has no server to ask".to_owned())?;
        if self.runtime.is_none() {
            self.runtime = Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| format!("cannot ask the server: {e}"))?,
            );
        }
        let rt = self.runtime.as_ref().ok_or("no runtime")?;
        rt.block_on(termos_core::transport::control_call(
            &socket, method, params,
        ))
        .map_err(|e| e.to_string())
    }

    fn sessions_and_panes(&mut self) -> Result<(Vec<SessionRow>, Vec<PaneRow>), String> {
        let u = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let sessions = self
            .call("session.list", None)?
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|v| SessionRow {
                        name: s(v, "name"),
                        workspaces: u(v, "workspaces"),
                        panes: u(v, "panes"),
                        clients: u(v, "clients"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let panes = self
            .call("pane.list", None)?
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|v| PaneRow {
                        id: u(v, "id"),
                        session: s(v, "session"),
                        name: s(v, "name"),
                        kind: s(v, "kind"),
                        focused: v.get("focused").and_then(Value::as_bool).unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((sessions, panes))
    }

    fn audit(&mut self) -> AuditInfo {
        let dir = termos_ai::audit::dir(&self.paths.state_dir);
        let logs = termos_ai::audit::list(&dir);
        let mut broken = Vec::new();
        for path in &logs {
            let size = std::fs::metadata(path).map_or(0, |m| m.len());
            let ok = match self.verified.get(path) {
                Some((s, ok)) if *s == size => *ok,
                _ => {
                    let ok = termos_ai::audit::verify(path).is_ok();
                    self.verified.insert(path.clone(), (size, ok));
                    ok
                }
            };
            if !ok {
                broken.push(
                    path.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                );
            }
        }
        AuditInfo {
            logs: logs.len(),
            broken,
        }
    }

    fn models(&self) -> ModelsInfo {
        let router = termos_core::models_endpoint::read(&self.paths)
            .map(|e| e.url)
            .filter(|url| answers(url));
        let get = |k: &str| std::env::var(k).ok();
        let dir = termos_models::cache::default_dir(self.config.models.cache.as_deref(), &get);
        let installed = termos_models::cache::Cache::new(dir)
            .repos()
            .into_iter()
            .map(|r| {
                let size = r
                    .snapshots
                    .iter()
                    .map(termos_models::cache::Snapshot::size)
                    .sum();
                (r.repo, size)
            })
            .collect();
        ModelsInfo { router, installed }
    }
}

/// Whether something listens at `url`'s host and port.
fn answers(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split('/').next().unwrap_or(rest);
    use std::net::ToSocketAddrs as _;
    host.to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|addr| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200))
                .is_ok()
        })
}

/// The last `n` lines of the newest file in `dir`.
fn newest_log(dir: &Path, n: usize) -> (Option<PathBuf>, Vec<String>) {
    let newest = std::fs::read_dir(dir).ok().and_then(|entries| {
        entries
            .flatten()
            .filter(|e| e.path().is_file())
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
            .map(|e| e.path())
    });
    let Some(path) = newest else {
        return (None, Vec::new());
    };
    // The end of the file is enough.
    let text = std::fs::read(&path)
        .map(|b| {
            let from = b.len().saturating_sub(64 * 1024);
            String::from_utf8_lossy(&b[from..]).into_owned()
        })
        .unwrap_or_default();
    let mut lines: Vec<String> = text.lines().rev().take(n).map(str::to_owned).collect();
    lines.reverse();
    (Some(path), lines)
}

impl Gather for Live {
    fn snapshot(&mut self, range: Range, journal: bool) -> Snapshot {
        let now = termos_core::journal::now();
        let mut snap = Snapshot::empty(range);
        snap.taken = now;
        if self.sampler.is_none() {
            self.sampler = termos_hw::Sampler::new().ok();
            // The first reading only starts the CPU clock.
            if let Some(s) = &mut self.sampler {
                let _ = s.sample();
            }
        }
        snap.sample = self.sampler.as_mut().map(Source::sample);
        match self.sessions_and_panes() {
            Ok((s, p)) => {
                snap.sessions = s;
                snap.panes = p;
            }
            Err(e) => snap.control_error = Some(e),
        }
        let stale = self.history.as_ref().is_none_or(|h| h.range != range);
        if journal || stale {
            let since = now - range.secs();
            let mut records = self.journal.read(since);
            if self.config.center.cli_usage {
                let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .filter(|h| !h.is_empty())
                    .map(PathBuf::from);
                let fixed = self.theirs.is_some();
                let reader = self
                    .theirs
                    .get_or_insert_with(crate::theirs::Reader::default);
                if !fixed || reader.sources().is_empty() {
                    // Every account's logs (D-097).
                    let all = termos_devkit::accounts::Accounts::load(&self.paths.data_dir).ok();
                    let (sources, _) = crate::usage_data::account_sources(
                        all.as_ref(),
                        &|k| std::env::var(k).ok(),
                        home.as_deref(),
                    );
                    reader.set_sources(sources);
                }
                records.extend(reader.read(since));
                records.sort_by_key(|r| r.time);
            }
            self.history = Some(history(&records, range, now));
        }
        if let Some(h) = &self.history {
            snap.history = h.clone();
        }
        snap.audit = self.audit();
        let (file, lines) = newest_log(&self.paths.log_dir(), 12);
        snap.log_file = file;
        snap.logs = lines;
        snap.models = self.models();
        snap.mcp = self
            .config
            .agents
            .mcp
            .iter()
            .map(|(name, m)| {
                let how = m
                    .url
                    .clone()
                    .or_else(|| m.command.first().cloned())
                    .unwrap_or_default();
                (name.clone(), how)
            })
            .collect();
        let presets = termos_ai::external::presets(&self.config.agents.external);
        snap.external = presets
            .into_values()
            .map(|p| {
                let program = p.command.first().cloned().unwrap_or_default();
                let running = snap
                    .panes
                    .iter()
                    .filter(|pane| pane.name == program || pane.name == p.name)
                    .count();
                ExternalAgent {
                    name: p.name,
                    description: p.description,
                    running,
                }
            })
            .collect();
        snap
    }

    fn end_process(&mut self, pid: u32, name: &str) -> Result<String, String> {
        termos_hw::end_process(pid)?;
        let what = format!("ended process {pid} ({name})");
        self.journal.record(Event::Action { what: what.clone() });
        Ok(what)
    }

    fn close_pane(&mut self, id: u64) -> Result<String, String> {
        self.call("pane.kill", Some(json!({ "pane": id })))?;
        let what = format!("closed pane %{id}");
        self.journal.record(Event::Action { what: what.clone() });
        Ok(what)
    }

    fn go_to_pane(&mut self, id: u64) -> Result<String, String> {
        self.call("pane.focus", Some(json!({ "pane": id })))?;
        Ok(format!("pane %{id} is in front"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_707_200;

    fn rec(ago: i64, event: Event) -> Record {
        Record {
            time: NOW - ago,
            event,
        }
    }

    fn usage(model: &str, tokens: u64, cost: Option<f64>) -> Event {
        Event::Usage {
            session: "s1".into(),
            provider: "fixture".into(),
            model: model.into(),
            input: tokens,
            output: 10,
            cache_read: 0,
            cache_write: 0,
            cost,
        }
    }

    #[test]
    fn the_history_sums_into_buckets_and_totals() {
        let records = vec![
            // Two samples in the oldest minute, one in the newest.
            rec(
                3_590,
                Event::System {
                    cpu: 10.0,
                    memory_used: 4,
                    memory_total: 16,
                    rx_per_sec: 0,
                    tx_per_sec: 0,
                },
            ),
            rec(
                3_580,
                Event::System {
                    cpu: 30.0,
                    memory_used: 8,
                    memory_total: 16,
                    rx_per_sec: 0,
                    tx_per_sec: 0,
                },
            ),
            rec(
                10,
                Event::System {
                    cpu: 90.0,
                    memory_used: 16,
                    memory_total: 16,
                    rx_per_sec: 0,
                    tx_per_sec: 0,
                },
            ),
            rec(
                1_800,
                Event::Session {
                    session: "s1".into(),
                    profile: "default".into(),
                    provider: "fixture".into(),
                    model: "fixture-coder".into(),
                    state: "started".into(),
                },
            ),
            rec(1_790, usage("fixture-coder", 100, Some(0.25))),
            rec(1_700, usage("fixture-coder", 200, Some(0.5))),
            rec(1_600, usage("fixture-small", 50, None)),
            rec(
                1_650,
                Event::Tool {
                    session: "s1".into(),
                    tool: "run_command".into(),
                    decision: "approved".into(),
                },
            ),
            rec(
                1_640,
                Event::Tool {
                    session: "s1".into(),
                    tool: "run_command".into(),
                    decision: "refused".into(),
                },
            ),
            rec(
                60,
                Event::Action {
                    what: "ended process 42 (yes)".into(),
                },
            ),
            // Too old for the hour.
            rec(7_200, usage("fixture-coder", 9_999, Some(9.0))),
        ];
        let h = history(&records, Range::Hour, NOW);
        assert_eq!(h.cpu.len(), 60);
        assert_eq!(h.cpu[0], Some(20.0), "the mean of the first minute");
        assert_eq!(h.memory[0], Some(37.5));
        assert_eq!(h.cpu[59], Some(90.0));
        assert_eq!(h.cpu[30], None, "nothing sampled then");
        assert_eq!(h.total_tokens(), 110 + 210 + 60);
        assert_eq!(h.total_cost(), Some(0.75));
        assert_eq!(h.models[0].model, "fixture-coder");
        assert_eq!(h.models[0].turns, 2);
        assert_eq!(h.models[1].cost, None, "no price, no cost");
        assert_eq!(h.decisions.get("approved"), Some(&1));
        assert_eq!(h.refused(), 1);
        assert_eq!(h.tools, [("run_command".to_owned(), 2)]);
        assert_eq!(h.sessions, 1);
        assert_eq!(h.actions.len(), 1);
        // A week holds the old turn too.
        let w = history(&records, Range::Week, NOW);
        assert_eq!(w.total_cost(), Some(9.75));
        assert_eq!(w.cpu.len(), 168);
    }

    #[test]
    fn logs_keep_their_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("old.log"), "old\n").expect("write");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let lines: Vec<String> = (1..=30).map(|i| format!("line {i}")).collect();
        std::fs::write(dir.path().join("new.log"), lines.join("\n")).expect("write");
        let (file, tail) = newest_log(dir.path(), 3);
        assert!(file.is_some_and(|f| f.ends_with("new.log")));
        assert_eq!(tail, ["line 28", "line 29", "line 30"]);
        assert_eq!(
            newest_log(&dir.path().join("none"), 3).1,
            Vec::<String>::new()
        );
    }
}
