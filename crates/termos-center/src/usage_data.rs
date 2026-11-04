// SPDX-License-Identifier: GPL-3.0-or-later
//! What the usage app shows (M11a, D-096, D-097): every account of every
//! agent CLI, and TermOS's own agents by provider.
//!
//! Tokens come from each account's own session logs; "tokens" here are
//! input, output and cache writes (cache reads, a fraction of the price,
//! are shown on their own). Plan limits are the CLIs' own figures where
//! they report them (Codex's logs, Claude Code's status line); budgets are
//! the user's, per account, and only drawn.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use termos_core::journal::{Event, Journal, Record};
use termos_core::paths::Paths;
use termos_devkit::accounts::{self, Accounts, Limits};
use termos_devkit::cli_agents::Tool;

use crate::theirs::{Dirs, Reader, Source};

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
/// Claude Code's usage window.
pub const WINDOW: i64 = 5 * HOUR;

/// Tokens over a stretch of time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Use {
    /// Input, output and cache writes.
    pub tokens: u64,
    pub cache_read: u64,
    pub turns: u64,
}

impl Use {
    fn add(&mut self, input: u64, output: u64, cache_read: u64, cache_write: u64) {
        self.tokens += input + output + cache_write;
        self.cache_read += cache_read;
        self.turns += 1;
    }
}

/// Claude Code's current 5-hour window: it starts at the first message
/// after the last one ended (TermOS counts from that hour).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: i64,
    pub resets: i64,
    pub tokens: u64,
}

/// One account of one CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountRow {
    /// `claude` or `codex`.
    pub cli: &'static str,
    pub cli_title: &'static str,
    pub name: String,
    /// Its label, else its name.
    pub title: String,
    pub active: bool,
    /// "signed in as …", or empty when it cannot be told.
    pub who: String,
    /// Claude Code's current 5-hour window, if one is open.
    pub window: Option<Window>,
    pub today: Use,
    pub week: Use,
    /// Tokens by model over 7 days, the most first.
    pub models: Vec<(String, u64)>,
    /// Tokens per day, the last 7 days, today last.
    pub daily: Vec<u64>,
    /// The plan's own limits, as the CLI last reported them.
    pub limits: Option<Limits>,
    pub budget_5h: Option<u64>,
    pub budget_week: Option<u64>,
}

/// TermOS's own agents, by provider.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRow {
    pub provider: String,
    pub today: Use,
    pub week: Use,
    /// US dollars over 7 days, where prices are known.
    pub cost_week: Option<f64>,
}

/// Everything the window shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    /// Unix seconds.
    pub taken: i64,
    pub accounts: Vec<AccountRow>,
    pub termos: Vec<ProviderRow>,
    /// Claude Code's own limits are not being recorded (TermOS's status
    /// line is not in its settings).
    pub claude_limits_off: bool,
}

/// An account, as the usage app needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSource {
    pub tool: Tool,
    pub name: String,
    pub title: String,
    pub active: bool,
    pub who: String,
    pub budget_5h: Option<u64>,
    pub budget_week: Option<u64>,
}

/// The start of `t`'s hour.
fn hour_of(t: i64) -> i64 {
    t.div_euclid(HOUR) * HOUR
}

/// The 5-hour window open at `now` in `times` (oldest first).
pub fn current_window(records: &[(i64, u64)], now: i64) -> Option<Window> {
    let mut window: Option<Window> = None;
    for &(t, tokens) in records {
        match &mut window {
            Some(w) if t < w.resets => w.tokens += tokens,
            _ => {
                let start = hour_of(t);
                window = Some(Window {
                    start,
                    resets: start + WINDOW,
                    tokens,
                });
            }
        }
    }
    window.filter(|w| now < w.resets)
}

/// Sums each account's records (`records`: the reader's, by source
/// index into `sources`) and TermOS's own (`journal`).
pub fn usage(
    sources: &[AccountSource],
    records: &[(usize, Record)],
    limits: &[Option<Limits>],
    journal: &[Record],
    now: i64,
    today_start: i64,
) -> Usage {
    let week_start = now - 7 * DAY;
    let day_of = |t: i64| -> Option<usize> {
        // 0 is 6 days before today, 6 is today.
        if t >= today_start {
            return Some(6);
        }
        let back = (today_start - t + DAY - 1) / DAY;
        usize::try_from(6 - back).ok()
    };
    let mut rows: Vec<AccountRow> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| AccountRow {
            cli: s.tool.name(),
            cli_title: s.tool.title(),
            name: s.name.clone(),
            title: s.title.clone(),
            active: s.active,
            who: s.who.clone(),
            window: None,
            today: Use::default(),
            week: Use::default(),
            models: Vec::new(),
            daily: vec![0; 7],
            limits: limits.get(i).cloned().flatten(),
            budget_5h: s.budget_5h,
            budget_week: s.budget_week,
        })
        .collect();
    let mut models: Vec<std::collections::BTreeMap<String, u64>> =
        vec![Default::default(); rows.len()];
    let mut timeline: Vec<Vec<(i64, u64)>> = vec![Vec::new(); rows.len()];
    for (i, r) in records {
        let (
            Some(row),
            Event::Usage {
                model,
                input,
                output,
                cache_read,
                cache_write,
                ..
            },
        ) = (rows.get_mut(*i), &r.event)
        else {
            continue;
        };
        let tokens = input + output + cache_write;
        timeline[*i].push((r.time, tokens));
        if r.time >= week_start {
            row.week.add(*input, *output, *cache_read, *cache_write);
            *models[*i].entry(model.clone()).or_default() += tokens;
        }
        if r.time >= today_start {
            row.today.add(*input, *output, *cache_read, *cache_write);
        }
        if let Some(d) = day_of(r.time) {
            row.daily[d] += tokens;
        }
    }
    for (i, row) in rows.iter_mut().enumerate() {
        if row.cli == "claude" {
            row.window = current_window(&timeline[i], now);
        }
        let mut m: Vec<(String, u64)> = std::mem::take(&mut models[i]).into_iter().collect();
        m.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        row.models = m;
    }
    let mut by: std::collections::BTreeMap<String, ProviderRow> = Default::default();
    for r in journal {
        if let Event::Usage {
            provider,
            input,
            output,
            cache_read,
            cache_write,
            cost,
            ..
        } = &r.event
            && r.time >= week_start
        {
            let p = by.entry(provider.clone()).or_insert_with(|| ProviderRow {
                provider: provider.clone(),
                today: Use::default(),
                week: Use::default(),
                cost_week: None,
            });
            p.week.add(*input, *output, *cache_read, *cache_write);
            if r.time >= today_start {
                p.today.add(*input, *output, *cache_read, *cache_write);
            }
            if let Some(c) = cost {
                p.cost_week = Some(p.cost_week.unwrap_or(0.0) + c);
            }
        }
    }
    Usage {
        taken: now,
        accounts: rows,
        termos: by.into_values().collect(),
        claude_limits_off: false,
    }
}

/// Reads the environment (a variable's value).
pub type Env = Box<dyn Fn(&str) -> Option<String> + Send>;

/// The real sources: the accounts, their logs, TermOS's journal.
pub struct LiveUsage {
    paths: Paths,
    journal: Arc<Journal>,
    reader: Reader,
    get: Env,
    home: Option<PathBuf>,
}

impl std::fmt::Debug for LiveUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveUsage").finish_non_exhaustive()
    }
}

impl LiveUsage {
    pub fn new(paths: Paths, journal: Arc<Journal>) -> Self {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);
        Self::with_env(paths, journal, Box::new(|k| std::env::var(k).ok()), home)
    }

    /// With a given environment (tests).
    pub fn with_env(paths: Paths, journal: Arc<Journal>, get: Env, home: Option<PathBuf>) -> Self {
        Self {
            paths,
            journal,
            reader: Reader::default(),
            get,
            home,
        }
    }

    /// A fresh look (the logs read from where they stopped).
    pub fn snapshot(&mut self, now: i64, today_start: i64) -> Usage {
        let all = Accounts::load(&self.paths.data_dir).ok();
        let home = self.home.as_deref();
        let get = &*self.get;
        let (sources, infos) = account_sources(all.as_ref(), get, home);
        self.reader.set_sources(sources);
        let records = self.reader.read_by_source(now - 8 * DAY);
        let limits: Vec<Option<Limits>> = infos
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let logged = self.reader.limits(i).cloned();
                let reported = accounts::load_limits(&self.paths.state_dir, s.tool, &s.name);
                match (logged, reported) {
                    (Some(a), Some(b)) => Some(if a.time >= b.time { a } else { b }),
                    (a, b) => a.or(b),
                }
            })
            .collect();
        let journal = self.journal.read(now - 8 * DAY);
        let mut u = usage(&infos, &records, &limits, &journal, now, today_start);
        u.claude_limits_off = !claude_statusline_on(&infos, &all, get, home);
        u
    }
}

/// Every account's logs (`Source`s for the reader), with what the usage
/// app shows of each; `all`: the account list (`None`: unreadable, so
/// each CLI's own folder only).
pub fn account_sources(
    all: Option<&Accounts>,
    get: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> (Vec<Source>, Vec<AccountSource>) {
    let mut sources = Vec::new();
    let mut infos = Vec::new();
    for tool in Tool::ALL {
        let list = all.map_or_else(
            || {
                vec![accounts::Account {
                    tool,
                    name: accounts::DEFAULT.into(),
                    meta: accounts::Meta::default(),
                    dir: None,
                }]
            },
            |a| a.list(tool),
        );
        let active = all.map(|a| a.active(tool).name);
        for a in list {
            let Some(dir) = a.home(get, home) else {
                continue;
            };
            let source = if a.is_default() {
                // Where the CLI looks by itself (Claude Code also under
                // ~/.config/claude).
                let d = Dirs::find(get, home);
                Source {
                    cli: tool.name(),
                    account: a.name.clone(),
                    dirs: if tool == Tool::Claude {
                        d.claude
                    } else {
                        d.codex
                    },
                }
            } else {
                Source::of(tool.name(), &a.name, &dir)
            };
            sources.push(source);
            infos.push(AccountSource {
                tool,
                title: a.title(),
                active: active.as_deref().is_none_or(|n| n == a.name),
                who: a.signed_in(get, home),
                budget_5h: a.meta.budget_5h,
                budget_week: a.meta.budget_week,
                name: a.name,
            });
        }
    }
    (sources, infos)
}

/// Whether TermOS's status line is in any Claude Code account's settings.
fn claude_statusline_on(
    infos: &[AccountSource],
    all: &Option<Accounts>,
    get: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> bool {
    let Some(all) = all else {
        return false;
    };
    infos.iter().filter(|s| s.tool == Tool::Claude).any(|s| {
        all.get(Tool::Claude, &s.name)
            .ok()
            .and_then(|a| a.home(get, home))
            .and_then(|h| std::fs::read_to_string(h.join("settings.json")).ok())
            .is_some_and(|t| t.contains(accounts::STATUSLINE_ARGS))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_707_200; // 2026-11-03T12:00:00Z
    const TODAY: i64 = NOW - 12 * HOUR;

    fn rec(t: i64, model: &str, input: u64, output: u64, cache_read: u64) -> Record {
        Record {
            time: t,
            event: Event::Usage {
                session: "s".into(),
                provider: "claude-code".into(),
                model: model.into(),
                input,
                output,
                cache_read,
                cache_write: 0,
                cost: None,
            },
        }
    }

    fn source(tool: Tool, name: &str) -> AccountSource {
        AccountSource {
            tool,
            name: name.into(),
            title: name.into(),
            active: name == "work",
            who: String::new(),
            budget_5h: Some(1_000),
            budget_week: None,
        }
    }

    #[test]
    fn windows_open_on_the_hour_of_the_first_message() {
        // 09:20 starts 09:00–14:00; 13:59 is in it; 15:10 starts 15:00.
        let t = |h: i64, m: i64| TODAY + h * HOUR + m * 60;
        let w = current_window(&[(t(9, 20), 10), (t(13, 59), 5)], t(12, 0)).expect("open");
        assert_eq!((w.start, w.resets, w.tokens), (t(9, 0), t(14, 0), 15));
        assert_eq!(current_window(&[(t(9, 20), 10)], t(14, 1)), None, "closed");
        let w = current_window(&[(t(9, 20), 10), (t(15, 10), 3)], t(16, 0)).expect("open");
        assert_eq!((w.start, w.tokens), (t(15, 0), 3));
        assert_eq!(current_window(&[], NOW), None);
    }

    #[test]
    fn accounts_and_providers_add_up() {
        let sources = vec![
            source(Tool::Claude, "work"),
            source(Tool::Claude, "default"),
            source(Tool::Codex, "default"),
        ];
        let records = vec![
            (0, rec(NOW - 3 * DAY, "fixture-a", 100, 50, 1000)),
            (0, rec(NOW - HOUR, "fixture-a", 10, 5, 0)),
            (0, rec(NOW - 30 * 60, "fixture-b", 1, 1, 0)),
            (1, rec(NOW - 9 * DAY, "fixture-a", 999, 999, 0)),
            (2, rec(NOW - 2 * HOUR, "fixture-c", 7, 3, 2)),
        ];
        let limits = vec![
            None,
            None,
            Some(Limits {
                time: NOW,
                plan: Some("pro".into()),
                windows: vec![],
            }),
        ];
        let journal = vec![
            Record {
                time: NOW - HOUR,
                event: Event::Usage {
                    session: "t".into(),
                    provider: "anthropic".into(),
                    model: "fixture-model".into(),
                    input: 20,
                    output: 10,
                    cache_read: 0,
                    cache_write: 0,
                    cost: Some(0.5),
                },
            },
            Record {
                time: NOW,
                event: Event::Action { what: "x".into() },
            },
        ];
        let u = usage(&sources, &records, &limits, &journal, NOW, TODAY);
        let work = &u.accounts[0];
        assert!(work.active);
        assert_eq!(
            work.week,
            Use {
                tokens: 167,
                cache_read: 1000,
                turns: 3
            }
        );
        assert_eq!(
            work.today,
            Use {
                tokens: 17,
                cache_read: 0,
                turns: 2
            }
        );
        assert_eq!(
            work.models,
            [("fixture-a".to_owned(), 165), ("fixture-b".to_owned(), 2)]
        );
        assert_eq!(work.daily[6], 17);
        assert_eq!(work.daily[3], 150, "3 days ago");
        let w = work.window.expect("a window is open");
        assert_eq!(w.tokens, 17);
        // Too old for the week; no window.
        assert_eq!(u.accounts[1].week, Use::default());
        assert_eq!(u.accounts[1].window, None);
        let codex = &u.accounts[2];
        assert_eq!(codex.window, None, "only Claude Code has the window");
        assert_eq!(
            codex.limits.as_ref().and_then(|l| l.plan.as_deref()),
            Some("pro")
        );
        assert_eq!(u.termos.len(), 1);
        assert_eq!(u.termos[0].provider, "anthropic");
        assert_eq!(u.termos[0].week.tokens, 30);
        assert_eq!(u.termos[0].cost_week, Some(0.5));
    }

    #[test]
    fn the_live_source_reads_every_accounts_logs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            state_dir: dir.path().join("state"),
            runtime_dir: dir.path().join("run"),
            data_dir: dir.path().join("data"),
        };
        let mut all = Accounts::load(&paths.data_dir).expect("load");
        let work = all
            .add(Tool::Claude, "work", Some("Work (Max)"), None)
            .expect("add");
        all.use_account(Tool::Claude, "work").expect("use");
        let line = |id: &str| {
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"requestId\":\"r\",\"message\":{{\"id\":\"{id}\",\"model\":\"fixture-model\",\"usage\":{{\"input_tokens\":40,\"output_tokens\":2}}}}}}\n",
                jiff::Timestamp::from_second(NOW - 60).expect("t")
            )
        };
        for (folder, id) in [
            (work.dir.clone().expect("dir"), "w"),
            (home.join(".claude"), "d"),
        ] {
            let p = folder.join("projects/-x");
            std::fs::create_dir_all(&p).expect("mkdir");
            std::fs::write(p.join("s.jsonl"), line(id)).expect("write");
        }
        accounts::save_limits(
            &paths.state_dir,
            Tool::Claude,
            "work",
            &Limits {
                time: NOW,
                plan: None,
                windows: vec![accounts::LimitWindow {
                    minutes: 300,
                    used_percent: 42.0,
                    resets_at: Some(NOW + HOUR),
                }],
            },
        )
        .expect("saved");
        let journal = Arc::new(Journal::new(&paths.state_dir, 7));
        let mut live = LiveUsage::with_env(paths, journal, Box::new(|_| None), Some(home));
        let u = live.snapshot(NOW, TODAY);
        let names: Vec<(&str, &str, bool)> = u
            .accounts
            .iter()
            .map(|a| (a.cli, a.name.as_str(), a.active))
            .collect();
        assert_eq!(
            names,
            [
                ("claude", "default", false),
                ("claude", "work", true),
                ("codex", "default", true)
            ]
        );
        assert_eq!(u.accounts[0].today.tokens, 42);
        assert_eq!(u.accounts[1].today.tokens, 42);
        assert_eq!(u.accounts[1].title, "Work (Max)");
        assert_eq!(
            u.accounts[1]
                .limits
                .as_ref()
                .map(|l| l.windows[0].used_percent),
            Some(42.0),
            "Claude Code's own figure, from its status line"
        );
        assert!(u.claude_limits_off);
    }
}
