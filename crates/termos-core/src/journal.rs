// SPDX-License-Identifier: GPL-3.0-or-later
//! The command center's history (D-092, D-093): what happened, kept on disk
//! for `[center] keep-days` (7 by default) and read back for its charts.
//!
//! One JSON line per record, one file per UTC day
//! (`<state>/center/2026-11-03.jsonl`), so expiring is deleting old files.
//! Records hold numbers and names (providers, models, tools, decisions),
//! never what was typed, a tool's arguments or a secret. Files are private
//! to the user. Appends come from several threads (the system sampler,
//! agent sessions); each record is one write of one line.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Unix seconds.
    pub time: i64,
    #[serde(flatten)]
    pub event: Event,
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Event {
    /// The machine, sampled every minute (`[center] sample`).
    System {
        /// Percent of all CPUs.
        cpu: f32,
        memory_used: u64,
        memory_total: u64,
        rx_per_sec: u64,
        tx_per_sec: u64,
    },
    /// Tokens one agent turn used.
    Usage {
        session: String,
        provider: String,
        model: String,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        /// US dollars, when the model's prices are known.
        cost: Option<f64>,
    },
    /// A tool call and its decision (`allowed`, `approved`, `denied`,
    /// `refused`, `cancelled`).
    Tool {
        session: String,
        tool: String,
        decision: String,
    },
    /// An agent session started or ended.
    Session {
        session: String,
        profile: String,
        provider: String,
        model: String,
        /// `started` or `ended`.
        state: String,
    },
    /// Something done from the command center.
    Action { what: String },
    /// The harness handed a task to a backend (D-100): names and the
    /// outcome only.
    Handoff {
        plan: String,
        task: String,
        /// The task's kind (`coding`).
        task_kind: String,
        backend: String,
        model: String,
        outcome: String,
    },
}

/// The history files.
#[derive(Debug)]
pub struct Journal {
    dir: PathBuf,
    keep_days: u32,
    /// The day expiry last ran (days since the epoch), so it runs once a
    /// day at most.
    expired_on: Mutex<i64>,
}

/// Days since the Unix epoch of `time` (UTC).
fn day(time: i64) -> i64 {
    time.div_euclid(86_400)
}

/// `YYYY-MM-DD` of a day since the epoch.
fn day_name(day: i64) -> String {
    jiff::Timestamp::from_second(day * 86_400).map_or_else(
        |_| format!("day-{day}"),
        |t| {
            t.to_zoned(jiff::tz::TimeZone::UTC)
                .date()
                .strftime("%Y-%m-%d")
                .to_string()
        },
    )
}

/// The day a file is for, from its name.
fn file_day(name: &str) -> Option<i64> {
    let date: jiff::civil::Date = name.strip_suffix(".jsonl")?.parse().ok()?;
    let t = date.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp();
    Some(day(t.as_second()))
}

/// Now, in Unix seconds.
pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

impl Journal {
    /// The history under `state_dir` (`<state>/center`), keeping
    /// `keep_days` days.
    pub fn new(state_dir: &Path, keep_days: u32) -> Self {
        Self {
            dir: state_dir.join("center"),
            keep_days: keep_days.clamp(1, 365),
            expired_on: Mutex::new(i64::MIN),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Adds a record (and, once a day, removes days past keeping).
    pub fn append(&self, record: &Record) -> io::Result<()> {
        let mut line = serde_json::to_string(record).map_err(io::Error::other)?;
        line.push('\n');
        crate::paths::ensure_private_dir(&self.dir)?;
        let path = self
            .dir
            .join(format!("{}.jsonl", day_name(day(record.time))));
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        opts.open(&path)?.write_all(line.as_bytes())?;
        let today = day(record.time);
        let due = self
            .expired_on
            .lock()
            .map(|mut last| {
                let due = *last < today;
                *last = today.max(*last);
                due
            })
            .unwrap_or(false);
        if due {
            self.expire(record.time)?;
        }
        Ok(())
    }

    /// Adds `event` now, logging (not returning) a failure: history is
    /// worth keeping, never worth stopping for.
    pub fn record(&self, event: Event) {
        if let Err(e) = self.append(&Record { time: now(), event }) {
            tracing::warn!("cannot write the command center's history: {e}");
        }
    }

    /// Removes the days older than `keep_days` before `now`. Returns how
    /// many files went.
    pub fn expire(&self, now: i64) -> io::Result<usize> {
        let oldest = day(now) - i64::from(self.keep_days) + 1;
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Ok(0);
        };
        let mut removed = 0;
        for e in entries.flatten() {
            let name = e.file_name();
            if let Some(d) = file_day(&name.to_string_lossy())
                && d < oldest
            {
                std::fs::remove_file(e.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Every record from `since` (Unix seconds) on, oldest first. Lines
    /// that do not parse (a write cut short) are skipped.
    pub fn read(&self, since: i64) -> Vec<Record> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut days: Vec<(i64, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let d = file_day(&e.file_name().to_string_lossy())?;
                (d >= day(since)).then(|| (d, e.path()))
            })
            .collect();
        days.sort();
        let mut out = Vec::new();
        for (_, path) in days {
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            for line in BufReader::new(file).lines() {
                let Ok(line) = line else { break };
                if let Ok(r) = serde_json::from_str::<Record>(&line)
                    && r.time >= since
                {
                    out.push(r);
                }
            }
        }
        out.sort_by_key(|r| r.time);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;
    /// 2026-11-03T12:00:00Z.
    const NOON: i64 = 1_793_707_200;

    fn usage(session: &str, cost: f64) -> Event {
        Event::Usage {
            session: session.into(),
            provider: "fixture".into(),
            model: "fixture-model".into(),
            input: 100,
            output: 20,
            cache_read: 0,
            cache_write: 0,
            cost: Some(cost),
        }
    }

    #[test]
    fn days_are_named_and_read_back() {
        assert_eq!(day_name(day(NOON)), "2026-11-03");
        assert_eq!(file_day("2026-11-03.jsonl"), Some(day(NOON)));
        assert_eq!(file_day("notes.txt"), None);
    }

    #[test]
    fn survives_a_restart_reads_ranges_and_skips_torn_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = Journal::new(dir.path(), 7);
        j.append(&Record {
            time: NOON - DAY,
            event: usage("a", 0.5),
        })
        .expect("write");
        j.append(&Record {
            time: NOON,
            event: Event::Tool {
                session: "a".into(),
                tool: "run_command".into(),
                decision: "approved".into(),
            },
        })
        .expect("write");
        // A line cut short by a crash.
        let today = j.dir().join("2026-11-03.jsonl");
        let mut f = OpenOptions::new().append(true).open(&today).expect("open");
        f.write_all(b"{\"time\": 17937").expect("write");
        drop(f);
        // Another process (or the next start) reads what was written.
        let again = Journal::new(dir.path(), 7);
        let all = again.read(0);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].event, usage("a", 0.5));
        assert_eq!(again.read(NOON).len(), 1, "only from noon on");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&today)
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "private");
        }
    }

    #[test]
    fn old_days_expire() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = Journal::new(dir.path(), 7);
        for back in 0..10 {
            j.append(&Record {
                time: NOON - back * DAY,
                event: Event::Action {
                    what: format!("day {back}"),
                },
            })
            .expect("write");
        }
        // Writing for a new day expires the days past keeping: today and
        // the six before stay.
        j.append(&Record {
            time: NOON + DAY,
            event: Event::Action {
                what: "tomorrow".into(),
            },
        })
        .expect("write");
        let days: std::collections::BTreeSet<i64> = j.read(0).iter().map(|r| day(r.time)).collect();
        assert_eq!(days.len(), 7, "{days:?}");
        assert_eq!(days.first(), Some(&(day(NOON) - 5)));
        assert_eq!(j.expire(NOON + 30 * DAY).expect("expire"), 7);
        assert!(j.read(0).is_empty());
    }
}
