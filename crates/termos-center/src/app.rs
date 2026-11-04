// SPDX-License-Identifier: GPL-3.0-or-later
//! The command center window (D-093): four tabs (Overview, AI, System,
//! History), every action a button and a key.
//!
//! The data comes from a [`Gather`] on the window's own thread, asked
//! every two seconds while the window is on screen (the desktop gives
//! hidden windows no timers, D-028), and from the live agent registry,
//! read on the server loop. Actions that change things outside the window
//! (ending a process, closing a pane) ask first.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::widgets::Widget;
use termos_ai::registry::{Act, Live, Registry, State};
use termos_core::app::{App, AppEffect, AppImage, AppWaker, new_image_key, tui_style};
use termos_core::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind};
use termos_core::widgets::{LineChart, Meter, Series, Sparkline};
use termos_render::cell::{Attrs, Style};
use termos_render::color::Color;
use termos_render::graphics::Picture;
use termos_render::graphics::chart::{ChartSeries, area_chart};
use termos_render::grid::Rect;
use termos_render::icons::Icon;
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

use crate::data::{Done, Gather, Job, Range, Snapshot, answer};

/// How often the window refreshes while on screen.
const REFRESH: Duration = Duration::from_secs(2);
/// How often it reads the history again.
const HISTORY_REFRESH: Duration = Duration::from_secs(30);

/// The live agent sessions (the registry, or fixtures in tests).
pub trait Agents: Send + Sync {
    fn list(&self) -> Vec<Live>;
    fn act(&self, key: u64, act: Act) -> bool;
}

impl Agents for Registry {
    fn list(&self) -> Vec<Live> {
        Registry::list(self)
    }
    fn act(&self, key: u64, act: Act) -> bool {
        Registry::act(self, key, act)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Ai,
    System,
    History,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Overview, Tab::Ai, Tab::System, Tab::History];

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Ai => "AI",
            Tab::System => "System",
            Tab::History => "History",
        }
    }
}

/// What a click on a region does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Button {
    Tab(Tab),
    Range(Range),
    Refresh,
    Agent(u64, Act),
    /// Select a process row.
    Process(usize),
    End,
    Go(u64),
    Close(u64, String),
    Yes,
    No,
    /// Run a desktop command (open a window).
    Command(String),
}

/// The CPU history's picture, made again only when it changes.
#[derive(Debug, Clone)]
struct Chart {
    taken: i64,
    size: (u16, u16),
    key: u64,
    picture: Arc<Picture>,
}

/// An action waiting for "yes".
#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirm {
    End { pid: u32, name: String },
    Close { id: u64, name: String },
}

/// The command center.
pub struct CommandCenter {
    tab: Tab,
    range: Range,
    snap: Snapshot,
    /// The first snapshot has arrived.
    loaded: bool,
    agents: Arc<dyn Agents>,
    jobs: Option<Sender<Job>>,
    done: Receiver<Done>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    next: Option<Instant>,
    history_read: Option<Instant>,
    message: Option<(String, bool)>,
    confirm: Option<Confirm>,
    /// The selected process (System) and the first one shown.
    process: usize,
    process_top: usize,
    buttons: Vec<(TuiRect, Button)>,
    images: Vec<AppImage>,
    chart: Option<Chart>,
    /// Times are shown in it (the system's; tests use UTC).
    tz: jiff::tz::TimeZone,
}

impl std::fmt::Debug for CommandCenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandCenter")
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

/// Makes the data source, on the window's own thread.
pub type MakeGather = Box<dyn FnOnce() -> Box<dyn Gather> + Send>;

impl CommandCenter {
    /// Opens the window; the source `make` makes runs on a thread of its
    /// own.
    pub fn open(make: MakeGather, agents: Arc<dyn Agents>) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<Done>();
        let waker: Arc<Mutex<Option<AppWaker>>> = Arc::new(Mutex::new(None));
        let w = Arc::clone(&waker);
        let spawned = std::thread::Builder::new()
            .name("command-center".into())
            .spawn(move || {
                let mut gather = make();
                // Ends when the window (the sender) goes.
                while let Ok(job) = job_rx.recv() {
                    if done_tx.send(answer(gather.as_mut(), job)).is_err() {
                        return;
                    }
                    if let Some(w) = w.lock().ok().and_then(|w| w.clone()) {
                        w.wake();
                    }
                }
            });
        let (jobs, message) = match spawned {
            Ok(_) => (Some(job_tx), None),
            Err(e) => (None, Some((format!("cannot start: {e}"), true))),
        };
        let range = Range::Day;
        Self {
            tab: Tab::Overview,
            range,
            snap: Snapshot::empty(range),
            loaded: false,
            agents,
            jobs,
            done: done_rx,
            waker,
            // The first look as soon as the window is on screen.
            next: Some(Instant::now()),
            history_read: None,
            message,
            confirm: None,
            process: 0,
            process_top: 0,
            buttons: Vec::new(),
            images: Vec::new(),
            chart: None,
            tz: jiff::tz::TimeZone::system(),
        }
    }

    /// Shows times in `tz` (tests: UTC, so snapshots do not depend on
    /// the machine's zone).
    #[must_use]
    pub fn with_time_zone(mut self, tz: jiff::tz::TimeZone) -> Self {
        self.tz = tz;
        self
    }

    fn send(&mut self, job: Job) {
        if let Some(tx) = &self.jobs
            && tx.send(job).is_err()
        {
            self.jobs = None;
        }
    }

    fn refresh(&mut self, now: Instant, history: bool) {
        let journal = history
            || self
                .history_read
                .is_none_or(|t| now.saturating_duration_since(t) >= HISTORY_REFRESH);
        if journal {
            self.history_read = Some(now);
        }
        self.send(Job::Refresh {
            range: self.range,
            journal,
        });
        self.next = Some(now + REFRESH);
    }

    fn take_done(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.done.try_recv() {
                Ok(Done::Snapshot(s)) => {
                    self.snap = *s;
                    self.loaded = true;
                    let n = self.processes().len();
                    if self.process >= n {
                        self.process = n.saturating_sub(1);
                    }
                    changed = true;
                }
                Ok(Done::Message(m, error)) => {
                    self.message = Some((m, error));
                    changed = true;
                    // Show what the action changed.
                    self.refresh(Instant::now(), false);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    fn processes(&self) -> &[termos_hw::ProcessSample] {
        self.snap.sample.as_ref().map_or(&[], |s| &s.processes)
    }

    fn press(&mut self, button: Button) -> AppEffect {
        match button {
            Button::Tab(t) => self.tab = t,
            Button::Range(r) => {
                self.range = r;
                self.refresh(Instant::now(), true);
            }
            Button::Refresh => self.refresh(Instant::now(), true),
            Button::Agent(key, act) => {
                let done = self.agents.act(key, act);
                self.message = Some(match (act, done) {
                    (Act::Approve, true) => ("approved".into(), false),
                    (Act::Deny, true) => ("denied".into(), false),
                    (Act::Stop, true) => ("stopped".into(), false),
                    (_, false) => ("that agent is no longer waiting for this".into(), true),
                });
            }
            Button::Process(i) => self.process = i,
            Button::End => {
                if let Some(p) = self.processes().get(self.process) {
                    self.confirm = Some(Confirm::End {
                        pid: p.pid,
                        name: p.name.clone(),
                    });
                }
            }
            Button::Go(id) => self.send(Job::GoToPane(id)),
            Button::Close(id, name) => self.confirm = Some(Confirm::Close { id, name }),
            Button::Yes => match self.confirm.take() {
                Some(Confirm::End { pid, name }) => self.send(Job::EndProcess { pid, name }),
                Some(Confirm::Close { id, .. }) => self.send(Job::ClosePane(id)),
                None => {}
            },
            Button::No => self.confirm = None,
            Button::Command(c) => return AppEffect::Command(c),
        }
        AppEffect::Redraw
    }

    /// The first agent waiting for an answer.
    fn asking(&self) -> Option<Live> {
        self.agents
            .list()
            .into_iter()
            .find(|l| l.state == State::Asking)
    }
}

// ---------------------------------------------------------------------
// Drawing

/// Draws into the buffer and remembers where the buttons are (`B`: what
/// a button does).
pub(crate) struct Ui<'a, B> {
    pub(crate) buf: &'a mut Buffer,
    pub(crate) theme: &'a Theme,
    pub(crate) buttons: &'a mut Vec<(TuiRect, B)>,
    pub(crate) area: TuiRect,
}

pub(crate) fn width(s: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(s)).unwrap_or(u16::MAX)
}

/// `s` cut to `max` columns (with `…`).
pub(crate) fn fit(s: &str, max: u16) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = width(ch.encode_utf8(&mut [0; 4]));
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n}{}", UNITS[0])
    } else if v < 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.0}{}", UNITS[u])
    }
}

/// A series from the first bucket that has a value (before it, TermOS was
/// not running or not sampling: not zero), gaps as zero.
fn series(values: &[Option<f32>]) -> Vec<f64> {
    let first = values
        .iter()
        .position(Option::is_some)
        .unwrap_or(values.len());
    values[first..]
        .iter()
        .map(|v| f64::from(v.unwrap_or(0.0)))
        .collect()
}

/// `data` stretched to `n` points, each repeated, so a chart that draws
/// one point per column fills its width with the whole range. `full`:
/// the range's whole length (a series trimmed at the start keeps its
/// place at the right).
fn stretch(data: &[f64], full: usize, n: usize) -> Vec<f64> {
    if data.is_empty() || full == 0 || n == 0 {
        return Vec::new();
    }
    let keep = n * data.len() / full;
    (0..keep)
        .map(|i| data[(i * data.len() / keep.max(1)).min(data.len() - 1)])
        .collect()
}

pub(crate) fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

pub(crate) fn dollars(c: Option<f64>) -> String {
    c.map_or_else(|| "–".to_owned(), |c| format!("${c:.2}"))
}

impl<B> Ui<'_, B> {
    pub(crate) fn text(&mut self, x: u16, y: u16, s: &str, style: Style) -> u16 {
        if y >= self.area.bottom() || x >= self.area.right() {
            return x;
        }
        let room = self.area.right() - x;
        let shown = fit(s, room);
        self.buf.set_string(x, y, &shown, tui_style(style));
        x + width(&shown)
    }

    pub(crate) fn fg(&self, c: Color) -> Style {
        Style::new().fg(c)
    }

    pub(crate) fn muted(&self) -> Style {
        self.fg(self.theme.colors.muted)
    }

    pub(crate) fn heading(&mut self, x: u16, y: u16, s: &str) -> u16 {
        let style = self.fg(self.theme.colors.accent).attrs(Attrs::BOLD);
        self.text(x, y, s, style)
    }

    /// A clickable label: `[ label ]` in the panel's key style.
    pub(crate) fn button(&mut self, x: u16, y: u16, label: &str, button: B) -> u16 {
        let text = format!("[{label}]");
        let w = width(&text);
        if y >= self.area.bottom() || x + w > self.area.right() {
            return x;
        }
        let style = self.theme.overlay_key;
        self.buf.set_string(x, y, &text, tui_style(style));
        self.buttons.push((TuiRect::new(x, y, w, 1), button));
        x + w + 1
    }

    pub(crate) fn meter(&mut self, rect: TuiRect, label: &str, ratio: f64, value: &str) {
        let c = &self.theme.colors;
        let fill = if ratio > 0.9 {
            c.error
        } else if ratio > 0.75 {
            c.warn
        } else {
            c.accent
        };
        Meter {
            ratio,
            label,
            value,
            label_width: 8,
            fill: tui_style(Style::new().fg(fill)),
            empty: tui_style(Style::new().fg(c.border)),
            text: tui_style(Style::new().fg(c.fg)),
            fill_at: None,
        }
        .render(rect, self.buf);
    }

    pub(crate) fn hline(&mut self, y: u16) {
        if y < self.area.bottom() {
            let line = "─".repeat(usize::from(self.area.width));
            let style = self.fg(self.theme.colors.border);
            self.buf.set_string(self.area.x, y, &line, tui_style(style));
        }
    }
}

impl CommandCenter {
    fn draw_tabs(&mut self, ui: &mut Ui<'_, Button>) {
        let mut x = ui.area.x + 1;
        let y = ui.area.y;
        for t in Tab::ALL {
            let label = format!(" {} ", t.label());
            let style = if t == self.tab {
                ui.theme.status_tab_active
            } else {
                ui.theme.status_tab
            };
            let w = width(&label);
            ui.buf.set_string(x, y, &label, tui_style(style));
            ui.buttons.push((TuiRect::new(x, y, w, 1), Button::Tab(t)));
            x += w + 1;
        }
        let when = if self.loaded {
            jiff::Timestamp::from_second(self.snap.taken).map_or_else(
                |_| String::new(),
                |t| t.to_zoned(self.tz.clone()).strftime("%H:%M:%S").to_string(),
            )
        } else {
            "reading…".to_owned()
        };
        let right = format!("↻ {when}");
        let rx = ui.area.right().saturating_sub(width(&right) + 1);
        if rx > x {
            let style = ui.muted();
            ui.text(rx, y, &right, style);
            ui.buttons
                .push((TuiRect::new(rx, y, width(&right), 1), Button::Refresh));
        }
        ui.hline(y + 1);
    }

    fn draw_footer(&mut self, ui: &mut Ui<'_, Button>) {
        let y = ui.area.bottom().saturating_sub(1);
        let x = ui.area.x + 1;
        if let Some(c) = &self.confirm {
            let q = match c {
                Confirm::End { pid, name } => format!("End process {pid} ({name})?"),
                Confirm::Close { id, name } => format!("Close pane %{id} ({name})?"),
            };
            let style = ui.fg(ui.theme.colors.warn).attrs(Attrs::BOLD);
            let x = ui.text(x, y, &q, style) + 1;
            let x = ui.button(x, y, "Yes  y", Button::Yes);
            ui.button(x, y, "No  n", Button::No);
            return;
        }
        if let Some((m, error)) = &self.message {
            let c = if *error {
                ui.theme.colors.error
            } else {
                ui.theme.colors.fg
            };
            let style = ui.fg(c);
            ui.text(x, y, m, style);
            return;
        }
        let hints = match self.tab {
            Tab::Overview | Tab::Ai => {
                "1-4 tabs · a approve · d deny · s stop · r refresh · q close"
            }
            Tab::System => "1-4 tabs · ↑↓ select · e end process · r refresh · q close",
            Tab::History => "1-4 tabs · [ ] range · r refresh · q close",
        };
        let style = ui.muted();
        ui.text(x, y, hints, style);
    }

    fn draw_overview(&mut self, ui: &mut Ui<'_, Button>, body: TuiRect) {
        let half = body.width / 2;
        let left = TuiRect::new(body.x + 1, body.y, half.saturating_sub(2), body.height);
        let right = TuiRect::new(
            body.x + half + 1,
            body.y,
            body.width - half - 2,
            body.height,
        );
        // AI
        let mut y = left.y;
        ui.heading(left.x, y, "AI");
        y += 1;
        let live = self.agents.list();
        let asking: Vec<&Live> = live.iter().filter(|l| l.state == State::Asking).collect();
        let fgs = ui.fg(ui.theme.colors.fg);
        ui.text(
            left.x,
            y,
            &format!(
                "Agents     {} running{}",
                live.len(),
                if asking.is_empty() {
                    String::new()
                } else {
                    format!(" · {} asking", asking.len())
                }
            ),
            fgs,
        );
        y += 1;
        for l in asking.iter().take(3) {
            let Some(p) = &l.pending else { continue };
            let warn = ui.fg(ui.theme.colors.warn);
            let x = ui.text(
                left.x + 2,
                y,
                &format!("⚠ {}: {}", l.profile, fit(&p.summary, left.width / 2)),
                warn,
            ) + 1;
            let x = ui.button(x, y, "Approve", Button::Agent(l.key, Act::Approve));
            ui.button(x, y, "Deny", Button::Agent(l.key, Act::Deny));
            y += 1;
        }
        let h = &self.snap.history;
        ui.text(
            left.x,
            y,
            &format!(
                "{:<11}{} tokens · {}",
                self.range.label(),
                tokens(h.total_tokens()),
                dollars(h.total_cost())
            ),
            fgs,
        );
        y += 1;
        let calls: u64 = h.decisions.values().sum();
        ui.text(
            left.x,
            y,
            &format!("Tool calls {calls} ({} refused)", h.refused()),
            fgs,
        );
        y += 1;
        let audit = &self.snap.audit;
        let (text, c) = if audit.broken.is_empty() {
            (
                format!("Audit      {} logs, all verified", audit.logs),
                ui.theme.colors.fg,
            )
        } else {
            (
                format!("Audit      {} broken of {}", audit.broken.len(), audit.logs),
                ui.theme.colors.error,
            )
        };
        let style = ui.fg(c);
        ui.text(left.x, y, &text, style);
        y += 1;
        let m = &self.snap.models;
        ui.text(
            left.x,
            y,
            &format!(
                "Models     {} installed · router {}",
                m.installed.len(),
                if m.router.is_some() { "running" } else { "off" }
            ),
            fgs,
        );
        // System
        let mut y = right.y;
        ui.heading(right.x, y, "System");
        y += 1;
        if let Some(s) = self.snap.sample.clone() {
            ui.meter(
                TuiRect::new(right.x, y, right.width, 1),
                "CPU",
                f64::from(s.cpu.total) / 100.0,
                &format!("{:>3.0}%", s.cpu.total),
            );
            y += 1;
            let m = s.memory;
            let r = if m.total > 0 {
                m.used as f64 / m.total as f64
            } else {
                0.0
            };
            ui.meter(
                TuiRect::new(right.x, y, right.width, 1),
                "Memory",
                r,
                &format!("{}/{}", human_bytes(m.used), human_bytes(m.total)),
            );
            y += 1;
            ui.text(
                right.x,
                y,
                &format!(
                    "Network ↓ {}/s ↑ {}/s",
                    human_bytes(s.net.rx_per_sec),
                    human_bytes(s.net.tx_per_sec)
                ),
                fgs,
            );
            y += 1;
            let busiest: Vec<String> = s
                .processes
                .iter()
                .take(3)
                .map(|p| format!("{} {:.0}%", p.name, p.cpu))
                .collect();
            ui.text(right.x, y, &format!("Busiest {}", busiest.join(" · ")), fgs);
            y += 1;
        }
        let clients: u64 = self.snap.sessions.iter().map(|s| s.clients).sum();
        let text = match &self.snap.control_error {
            Some(e) => format!("Sessions ({e})"),
            None => format!(
                "Sessions {} · panes {} · terminals attached {clients}",
                self.snap.sessions.len(),
                self.snap.panes.len()
            ),
        };
        ui.text(right.x, y, &text, fgs);
        // The history at the bottom, filling what is left.
        let chart_y = left.y + 9;
        if chart_y + 4 < body.bottom() {
            let x = body.x + 1;
            ui.heading(x, chart_y, &format!("Last {}", self.range.label()));
            let w = body.width.saturating_sub(14);
            let n = h.cpu.len();
            let rows = body.bottom().saturating_sub(chart_y + 1);
            let cpu_rows = (rows * 2 / 3).max(2);
            let muted = ui.muted();
            ui.text(x, chart_y + 1, "CPU %", muted);
            let cpu = series(&h.cpu);
            let cpu = stretch(&cpu, n, usize::from(w) * 2);
            let s = [Series {
                data: &cpu,
                style: tui_style(Style::new().fg(ui.theme.colors.accent)),
            }];
            LineChart {
                series: &s,
                max: Some(100.0),
                labels: None,
                label_style: tui_style(muted),
            }
            .render(TuiRect::new(x + 12, chart_y + 1, w, cpu_rows), ui.buf);
            let ty = chart_y + 1 + cpu_rows;
            if ty < body.bottom() {
                ui.text(x, ty, "Tokens", muted);
                let toks: Vec<f64> = h.tokens.iter().map(|t| *t as f64).collect();
                let toks = stretch(&toks, n, usize::from(w));
                Sparkline {
                    data: &toks,
                    max: None,
                    style: tui_style(Style::new().fg(ui.theme.colors.warn)),
                }
                .render(
                    TuiRect::new(x + 12, ty, w, body.bottom().saturating_sub(ty)),
                    ui.buf,
                );
            }
        }
    }

    fn draw_ai(&mut self, ui: &mut Ui<'_, Button>, body: TuiRect) {
        let x = body.x + 1;
        let mut y = body.y;
        let fgs = ui.fg(ui.theme.colors.fg);
        let muted = ui.muted();
        ui.heading(x, y, "Running agents");
        y += 1;
        let live = self.agents.list();
        if live.is_empty() {
            let nx = ui.text(x + 2, y, "No agent is running.", muted) + 1;
            ui.button(nx, y, "New agent", Button::Command("agent.open".into()));
            y += 1;
        }
        for l in &live {
            let what = format!(
                "{:<10} {:<24} {:<10} {:>7} {:>7}",
                fit(&l.profile, 10),
                fit(&format!("{}/{}", l.provider, l.model), 24),
                l.state.label(),
                tokens(l.usage.input + l.usage.output + l.usage.cache_read + l.usage.cache_write),
                dollars(l.cost),
            );
            let style = if l.state == State::Asking {
                ui.fg(ui.theme.colors.warn)
            } else {
                fgs
            };
            let mut bx = ui.text(x + 2, y, &what, style) + 1;
            if l.state == State::Asking {
                bx = ui.button(bx, y, "Approve", Button::Agent(l.key, Act::Approve));
                bx = ui.button(bx, y, "Deny", Button::Agent(l.key, Act::Deny));
            }
            if matches!(l.state, State::Busy | State::Asking | State::Starting) {
                bx = ui.button(bx, y, "Stop", Button::Agent(l.key, Act::Stop));
            }
            if !l.session.is_empty() {
                ui.button(
                    bx,
                    y,
                    "Audit",
                    Button::Command(format!("audit.open {}", l.session)),
                );
            }
            y += 1;
            if let Some(p) = &l.pending {
                ui.text(x + 4, y, &format!("asks: {} — {}", p.summary, p.why), muted);
                y += 1;
            }
        }
        y += 1;
        let h = self.snap.history.clone();
        ui.heading(x, y, &format!("Usage · last {}", self.range.label()));
        y += 1;
        if h.models.is_empty() {
            ui.text(x + 2, y, "No tokens used.", muted);
            y += 1;
        }
        for m in h.models.iter().take(5) {
            ui.text(
                x + 2,
                y,
                &format!(
                    "{:<36} {:>4} turns  in {:>7}  out {:>7}  {:>8}",
                    fit(&format!("{}/{}", m.provider, m.model), 36),
                    m.turns,
                    tokens(m.input),
                    tokens(m.output),
                    dollars(m.cost)
                ),
                fgs,
            );
            y += 1;
        }
        let decisions: Vec<String> = h
            .decisions
            .iter()
            .map(|(d, n)| format!("{d} {n}"))
            .collect();
        let tools: Vec<String> = h
            .tools
            .iter()
            .take(4)
            .map(|(t, n)| format!("{t} {n}"))
            .collect();
        ui.text(
            x,
            y,
            &format!(
                "Tool calls  {}",
                if decisions.is_empty() {
                    "none".to_owned()
                } else {
                    decisions.join(" · ")
                }
            ),
            fgs,
        );
        y += 1;
        if !tools.is_empty() {
            ui.text(x, y, &format!("Most used   {}", tools.join(" · ")), muted);
            y += 1;
        }
        let audit = self.snap.audit.clone();
        let (text, c) = if audit.broken.is_empty() {
            (
                format!("Audit       {} logs, every chain verified", audit.logs),
                ui.theme.colors.fg,
            )
        } else {
            (
                format!("Audit       broken chain: {}", audit.broken.join(", ")),
                ui.theme.colors.error,
            )
        };
        let style = ui.fg(c);
        let ax = ui.text(x, y, &text, style) + 1;
        ui.button(ax, y, "Open", Button::Command("audit.open".into()));
        y += 1;
        let m = self.snap.models.clone();
        let installed: Vec<String> = m
            .installed
            .iter()
            .take(3)
            .map(|(n, s)| format!("{n} {}", human_bytes(*s)))
            .collect();
        ui.text(
            x,
            y,
            &format!(
                "Models      router {} · {}",
                m.router.as_deref().unwrap_or("not running"),
                if installed.is_empty() {
                    "none installed".to_owned()
                } else {
                    installed.join(" · ")
                }
            ),
            fgs,
        );
        y += 1;
        let mcp: Vec<String> = self
            .snap
            .mcp
            .iter()
            .map(|(n, how)| format!("{n} ({how})"))
            .collect();
        ui.text(
            x,
            y,
            &format!(
                "MCP servers {}",
                if mcp.is_empty() {
                    "none configured".to_owned()
                } else {
                    mcp.join(" · ")
                }
            ),
            fgs,
        );
        y += 1;
        ui.text(x, y, "CLI agents", fgs);
        let mut bx = x + 12;
        for a in self.snap.external.clone() {
            let label = if a.running > 0 {
                format!("{} ({} open)", a.name, a.running)
            } else {
                a.name.clone()
            };
            bx = ui.button(
                bx,
                y,
                &label,
                Button::Command(format!("agent.external {}", a.name)),
            );
        }
    }

    fn draw_system(&mut self, ui: &mut Ui<'_, Button>, body: TuiRect) {
        let half = (body.width * 11 / 20).max(30).min(body.width);
        let left = TuiRect::new(body.x + 1, body.y, half.saturating_sub(2), body.height);
        let right = TuiRect::new(
            body.x + half + 1,
            body.y,
            body.width.saturating_sub(half + 2),
            body.height,
        );
        let fgs = ui.fg(ui.theme.colors.fg);
        let muted = ui.muted();
        // Processes.
        let mut y = left.y;
        let hx = ui.heading(left.x, y, "Processes (by CPU)") + 1;
        ui.button(hx, y, "End  e", Button::End);
        y += 1;
        // PID, CPU and memory keep their width; the name takes the rest.
        let name_w = usize::from(left.width.saturating_sub(7 + 2 + 7 + 8)).clamp(4, 30);
        ui.text(
            left.x,
            y,
            &format!(
                "{:>7}  {:<name_w$} {:>6} {:>7}",
                "PID", "NAME", "CPU", "MEM"
            ),
            muted,
        );
        y += 1;
        let rows = usize::from(left.bottom().saturating_sub(y));
        if self.process < self.process_top {
            self.process_top = self.process;
        } else if rows > 0 && self.process >= self.process_top + rows {
            self.process_top = self.process + 1 - rows;
        }
        let procs = self.processes().to_vec();
        for (i, p) in procs.iter().enumerate().skip(self.process_top).take(rows) {
            let line = format!(
                "{:>7}  {:<name_w$} {:>5.0}% {:>7}",
                p.pid,
                fit(&p.name, u16::try_from(name_w).unwrap_or(4)),
                p.cpu,
                human_bytes(p.memory)
            );
            let rect = TuiRect::new(left.x, y, left.width, 1);
            let style = if i == self.process {
                ui.theme.overlay_selected
            } else {
                fgs
            };
            ui.buf.set_style(rect, tui_style(style));
            ui.text(left.x, y, &line, style);
            ui.buttons.push((rect, Button::Process(i)));
            y += 1;
        }
        if procs.is_empty() {
            ui.text(left.x, y, "reading…", muted);
        }
        // Sessions and panes.
        let mut y = right.y;
        ui.heading(right.x, y, "Sessions and panes");
        y += 1;
        if let Some(e) = &self.snap.control_error {
            let err = ui.fg(ui.theme.colors.error);
            ui.text(right.x, y, e, err);
            y += 1;
        }
        let sessions = self.snap.sessions.clone();
        let panes = self.snap.panes.clone();
        for s in &sessions {
            ui.text(
                right.x,
                y,
                &format!(
                    "{} · {} workspaces · {} attached",
                    s.name, s.workspaces, s.clients
                ),
                fgs,
            );
            y += 1;
            for p in panes.iter().filter(|p| p.session == s.name).take(6) {
                if y + 8 >= right.bottom() {
                    break;
                }
                let mark = if p.focused { "●" } else { " " };
                // The buttons at the right; the name in what is left.
                let bx = right.right().saturating_sub(13).max(right.x + 10);
                let room = bx.saturating_sub(right.x + 10);
                let line = format!("{mark} %{:<4} {}", p.id, fit(&p.name, room));
                ui.text(right.x + 1, y, &line, fgs);
                let bx = ui.button(bx, y, "Go", Button::Go(p.id));
                ui.button(bx, y, "Close", Button::Close(p.id, p.name.clone()));
                y += 1;
            }
        }
        y += 1;
        // The machine.
        if let Some(s) = self.snap.sample.clone() {
            ui.heading(right.x, y, "Machine");
            y += 1;
            for d in s.disks.iter().take(2) {
                let used = d.total.saturating_sub(d.available);
                let r = if d.total > 0 {
                    used as f64 / d.total as f64
                } else {
                    0.0
                };
                ui.meter(
                    TuiRect::new(right.x, y, right.width, 1),
                    &fit(&d.mount, 8),
                    r,
                    &format!("{}/{}", human_bytes(used), human_bytes(d.total)),
                );
                y += 1;
            }
            let nx = ui.text(
                right.x,
                y,
                &format!(
                    "Network ↓ {}/s ↑ {}/s",
                    human_bytes(s.net.rx_per_sec),
                    human_bytes(s.net.tx_per_sec)
                ),
                fgs,
            ) + 1;
            ui.button(nx, y, "Network", Button::Command("network.open".into()));
            y += 1;
        }
        let vx = ui.text(right.x, y, &format!("TermOS {}", self.snap.version), fgs) + 1;
        let vx = ui.button(vx, y, "Updates", Button::Command("update.open".into()));
        ui.button(vx, y, "Monitor", Button::Command("monitor.open".into()));
        y += 2;
        // The log.
        let lx = ui.heading(right.x, y, "Log") + 1;
        if let Some(f) = self.snap.log_file.clone() {
            ui.button(
                lx,
                y,
                "Open",
                Button::Command(format!("file.edit {}", f.display())),
            );
        }
        y += 1;
        let room = usize::from(right.bottom().saturating_sub(y));
        let logs = self.snap.logs.clone();
        for line in logs.iter().rev().take(room).rev() {
            // Logs start with a long timestamp: the time is enough.
            let short = line.split_once('T').map_or(line.as_str(), |(_, rest)| {
                rest.get(..8).map_or(rest, |_| rest)
            });
            ui.text(right.x, y, short, muted);
            y += 1;
        }
    }

    fn draw_history(&mut self, ui: &mut Ui<'_, Button>, body: TuiRect) {
        let x = body.x + 1;
        let mut y = body.y;
        let mut bx = ui.heading(x, y, "Range") + 1;
        for r in Range::ALL {
            let label = if r == self.range {
                format!("● {}", r.label())
            } else {
                r.label().to_owned()
            };
            bx = ui.button(bx, y, &label, Button::Range(r));
        }
        y += 1;
        let h = self.snap.history.clone();
        let chart_h = (body.height / 4).clamp(3, 6);
        let w = body.width.saturating_sub(2);
        let muted = ui.muted();
        let fgs = ui.fg(ui.theme.colors.fg);
        // CPU and memory over time.
        ui.text(x, y, "CPU and memory (%)", fgs);
        y += 1;
        let n = h.cpu.len();
        let dots = usize::from(w.saturating_sub(4)) * 2;
        let cpu = stretch(&series(&h.cpu), n, dots);
        let mem = stretch(&series(&h.memory), n, dots);
        let lines = [
            Series {
                data: &mem,
                style: tui_style(Style::new().fg(ui.theme.colors.muted)),
            },
            Series {
                data: &cpu,
                style: tui_style(Style::new().fg(ui.theme.colors.accent)),
            },
        ];
        let chart = TuiRect::new(x, y, w, chart_h);
        LineChart {
            series: &lines,
            max: Some(100.0),
            labels: Some(("100", "0")),
            label_style: tui_style(muted),
        }
        .render(chart, ui.buf);
        self.chart_image(chart, &series(&h.cpu), ui.theme);
        y += chart_h + 1;
        // Tokens over time.
        ui.text(
            x,
            y,
            &format!(
                "Tokens: {} · cost {} · {} sessions",
                tokens(h.total_tokens()),
                dollars(h.total_cost()),
                h.sessions
            ),
            fgs,
        );
        y += 1;
        let toks: Vec<f64> = h.tokens.iter().map(|t| *t as f64).collect();
        let toks = stretch(&toks, toks.len(), usize::from(w));
        Sparkline {
            data: &toks,
            max: None,
            style: tui_style(Style::new().fg(ui.theme.colors.warn)),
        }
        .render(TuiRect::new(x, y, w, 2), ui.buf);
        y += 3;
        for m in h.models.iter().take(3) {
            ui.text(
                x,
                y,
                &format!(
                    "{:<36} {:>4} turns {:>8}",
                    fit(&format!("{}/{}", m.provider, m.model), 36),
                    m.turns,
                    dollars(m.cost)
                ),
                fgs,
            );
            y += 1;
        }
        let decisions: Vec<String> = h
            .decisions
            .iter()
            .map(|(d, n)| format!("{d} {n}"))
            .collect();
        if !decisions.is_empty() {
            ui.text(x, y, &format!("Tool calls: {}", decisions.join(" · ")), fgs);
            y += 1;
        }
        y += 1;
        ui.text(x, y, "Done from here", fgs);
        y += 1;
        if h.actions.is_empty() {
            ui.text(x + 2, y, "nothing yet", muted);
        }
        for (t, what) in &h.actions {
            if y >= body.bottom() {
                break;
            }
            let when = jiff::Timestamp::from_second(*t).map_or_else(
                |_| String::new(),
                |t| t.to_zoned(self.tz.clone()).strftime("%a %H:%M").to_string(),
            );
            ui.text(x + 2, y, &format!("{when}  {what}"), muted);
            y += 1;
        }
    }

    /// The CPU history as a picture, for terminals that show pixels
    /// (D-090): the braille chart stays beneath for the others.
    fn chart_image(&mut self, chart: TuiRect, cpu: &[f64], theme: &Theme) {
        let label_w = 4.min(chart.width / 2);
        let plot = TuiRect::new(
            chart.x + label_w,
            chart.y,
            chart.width.saturating_sub(label_w),
            chart.height,
        );
        if plot.is_empty() || cpu.iter().all(|v| *v == 0.0) {
            return;
        }
        let size = (plot.width, plot.height);
        let fresh = self
            .chart
            .as_ref()
            .is_some_and(|c| c.taken == self.snap.taken && c.size == size);
        if !fresh {
            let cell = termos_render::graphics::DEFAULT_CELL;
            let Ok(p) = area_chart(
                &[ChartSeries {
                    data: cpu,
                    color: theme.rgb(theme.colors.accent),
                    fill: true,
                }],
                100.0,
                theme.rgb(theme.colors.border),
                u32::from(size.0) * cell.width,
                u32::from(size.1) * cell.height,
            ) else {
                return;
            };
            self.chart = Some(Chart {
                taken: self.snap.taken,
                size,
                key: new_image_key(),
                picture: Arc::new(p),
            });
        }
        if let Some(c) = &self.chart {
            self.images.push(AppImage {
                key: c.key,
                picture: Arc::clone(&c.picture),
                rect: Rect::new(plot.x, plot.y, plot.width, plot.height),
                upscale: true,
                scroll_top: 0,
                pixels_only: true,
            });
        }
    }
}

impl App for CommandCenter {
    fn reopen(&self) -> Option<String> {
        Some("center.open".to_owned())
    }

    fn title(&self) -> String {
        "Command center".into()
    }

    fn icon(&self) -> Icon {
        Icon::Center
    }

    fn busy(&self) -> bool {
        !self.loaded
    }

    fn catch_up(&mut self) {
        self.take_done();
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker);
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.next
    }

    fn on_timer(&mut self, now: Instant) -> AppEffect {
        self.refresh(now, false);
        AppEffect::None
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        if self.confirm.is_some() {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.press(Button::Yes),
                KeyCode::Char('n') | KeyCode::Esc => self.press(Button::No),
                _ => AppEffect::None,
            };
        }
        self.message = None;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            KeyCode::Char(c @ '1'..='4') => {
                let i = usize::from(c as u8 - b'1');
                self.press(Button::Tab(Tab::ALL[i]))
            }
            KeyCode::Tab | KeyCode::Right => {
                let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.press(Button::Tab(Tab::ALL[(i + 1) % Tab::ALL.len()]))
            }
            KeyCode::BackTab | KeyCode::Left => {
                let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.press(Button::Tab(
                    Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()],
                ))
            }
            KeyCode::Char('r') => self.press(Button::Refresh),
            KeyCode::Char('[') => {
                let i = Range::ALL
                    .iter()
                    .position(|r| *r == self.range)
                    .unwrap_or(0);
                self.press(Button::Range(Range::ALL[i.saturating_sub(1)]))
            }
            KeyCode::Char(']') => {
                let i = Range::ALL
                    .iter()
                    .position(|r| *r == self.range)
                    .unwrap_or(0);
                self.press(Button::Range(Range::ALL[(i + 1).min(Range::ALL.len() - 1)]))
            }
            KeyCode::Char(c @ ('a' | 'd')) => match self.asking() {
                Some(l) => {
                    let act = if c == 'a' { Act::Approve } else { Act::Deny };
                    self.press(Button::Agent(l.key, act))
                }
                None => {
                    self.message = Some(("no agent is waiting for an answer".into(), false));
                    AppEffect::Redraw
                }
            },
            KeyCode::Char('s') => {
                let busy = self
                    .agents
                    .list()
                    .into_iter()
                    .find(|l| matches!(l.state, State::Busy | State::Asking));
                match busy {
                    Some(l) => self.press(Button::Agent(l.key, Act::Stop)),
                    None => AppEffect::None,
                }
            }
            KeyCode::Up if self.tab == Tab::System => {
                self.process = self.process.saturating_sub(1);
                AppEffect::Redraw
            }
            KeyCode::Down if self.tab == Tab::System => {
                if self.process + 1 < self.processes().len() {
                    self.process += 1;
                }
                AppEffect::Redraw
            }
            KeyCode::Char('e') if self.tab == Tab::System => self.press(Button::End),
            _ => AppEffect::None,
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        match event.kind {
            MouseKind::Down(MouseButton::Left) => {
                let hit = self
                    .buttons
                    .iter()
                    .rev()
                    .find(|(r, _)| {
                        event.x >= r.x
                            && event.x < r.right()
                            && event.y >= r.y
                            && event.y < r.bottom()
                    })
                    .map(|(_, b)| b.clone());
                match hit {
                    Some(b) => {
                        self.message = None;
                        self.press(b)
                    }
                    None => AppEffect::None,
                }
            }
            MouseKind::ScrollDown if self.tab == Tab::System => {
                if self.process + 1 < self.processes().len() {
                    self.process += 1;
                }
                AppEffect::Redraw
            }
            MouseKind::ScrollUp if self.tab == Tab::System => {
                self.process = self.process.saturating_sub(1);
                AppEffect::Redraw
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.take_done();
        self.images.clear();
        let mut buttons = Vec::new();
        {
            let mut ui = Ui {
                buf,
                theme,
                buttons: &mut buttons,
                area,
            };
            if area.height < 4 || area.width < 20 {
                return;
            }
            self.draw_tabs(&mut ui);
            let body = TuiRect::new(area.x, area.y + 2, area.width, area.height - 3);
            match self.tab {
                Tab::Overview => self.draw_overview(&mut ui, body),
                Tab::Ai => self.draw_ai(&mut ui, body),
                Tab::System => self.draw_system(&mut ui, body),
                Tab::History => self.draw_history(&mut ui, body),
            }
            self.draw_footer(&mut ui);
        }
        self.buttons = buttons;
    }

    fn images(&self) -> Vec<AppImage> {
        self.images.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{AuditInfo, ExternalAgent, ModelsInfo, PaneRow, SessionRow, history};
    use termos_ai::agent::Pending;
    use termos_ai::provider::Usage;
    use termos_core::input::Mods;
    use termos_core::journal::{Event, Record};
    use termos_hw::{CpuSample, DiskSample, MemorySample, NetSample, ProcessSample, Sample};

    const NOW: i64 = 1_793_707_200;

    type Calls = Arc<Mutex<Vec<String>>>;

    struct FixtureGather(Calls);

    fn snapshot(range: Range) -> Snapshot {
        let mut records = Vec::new();
        for i in 0..60 {
            records.push(Record {
                time: NOW - 3_600 + i * 60,
                event: Event::System {
                    cpu: 20.0 + (i % 10) as f32 * 5.0,
                    memory_used: 6 << 30,
                    memory_total: 16 << 30,
                    rx_per_sec: 0,
                    tx_per_sec: 0,
                },
            });
        }
        records.push(Record {
            time: NOW - 1_200,
            event: Event::Usage {
                session: "s1".into(),
                provider: "fixture".into(),
                model: "fixture-coder".into(),
                input: 12_000,
                output: 800,
                cache_read: 0,
                cache_write: 0,
                cost: Some(0.42),
            },
        });
        for d in ["allowed", "approved", "refused"] {
            records.push(Record {
                time: NOW - 1_100,
                event: Event::Tool {
                    session: "s1".into(),
                    tool: "run_command".into(),
                    decision: d.into(),
                },
            });
        }
        records.push(Record {
            time: NOW - 600,
            event: Event::Action {
                what: "ended process 4242 (runaway)".into(),
            },
        });
        let mut s = Snapshot::empty(range);
        s.taken = NOW;
        s.version = "0.0.0".into();
        s.history = history(&records, range, NOW);
        s.sample = Some(Sample {
            cpu: CpuSample {
                total: 37.0,
                ..CpuSample::default()
            },
            memory: MemorySample {
                used: 6 << 30,
                total: 16 << 30,
                ..MemorySample::default()
            },
            net: NetSample {
                rx_per_sec: 2048,
                tx_per_sec: 512,
            },
            disks: vec![DiskSample {
                mount: "/".into(),
                fs: "ext4".into(),
                total: 500 << 30,
                available: 200 << 30,
                removable: false,
            }],
            processes: vec![
                ProcessSample {
                    pid: 4242,
                    name: "runaway".into(),
                    cpu: 97.0,
                    memory: 300 << 20,
                },
                ProcessSample {
                    pid: 777,
                    name: "editor".into(),
                    cpu: 3.0,
                    memory: 80 << 20,
                },
            ],
            ..Sample::default()
        });
        s.sessions = vec![SessionRow {
            name: "main".into(),
            workspaces: 2,
            panes: 2,
            clients: 1,
        }];
        s.panes = vec![
            PaneRow {
                id: 1,
                session: "main".into(),
                name: "bash".into(),
                kind: "term".into(),
                focused: true,
            },
            PaneRow {
                id: 5,
                session: "main".into(),
                name: "claude".into(),
                kind: "term".into(),
                focused: false,
            },
        ];
        s.audit = AuditInfo {
            logs: 3,
            broken: Vec::new(),
        };
        s.models = ModelsInfo {
            router: None,
            installed: vec![("org/fixture-gguf".into(), 4 << 30)],
        };
        s.mcp = vec![("files".into(), "mcp-files".into())];
        s.external = vec![ExternalAgent {
            name: "claude".into(),
            description: "Claude Code".into(),
            running: 1,
        }];
        s.logs = vec![
            "2026-11-03T11:58:01.000Z  INFO termos: session main started".into(),
            "2026-11-03T11:59:30.000Z  WARN termos: a plugin was slow".into(),
        ];
        s
    }

    impl Gather for FixtureGather {
        fn snapshot(&mut self, range: Range, journal: bool) -> Snapshot {
            self.0
                .lock()
                .expect("calls")
                .push(format!("refresh {} {journal}", range.label()));
            snapshot(range)
        }
        fn end_process(&mut self, pid: u32, name: &str) -> Result<String, String> {
            self.0
                .lock()
                .expect("calls")
                .push(format!("end {pid} {name}"));
            Ok(format!("ended process {pid} ({name})"))
        }
        fn close_pane(&mut self, id: u64) -> Result<String, String> {
            self.0.lock().expect("calls").push(format!("close {id}"));
            Err("no pane %9".into())
        }
        fn go_to_pane(&mut self, id: u64) -> Result<String, String> {
            self.0.lock().expect("calls").push(format!("go {id}"));
            Ok(format!("pane %{id} is in front"))
        }
    }

    struct FixtureAgents(Calls);

    impl Agents for FixtureAgents {
        fn list(&self) -> Vec<Live> {
            let live = |key, state, pending| Live {
                key,
                session: format!("s{key}"),
                profile: "default".into(),
                provider: "fixture".into(),
                model: "fixture-coder".into(),
                state,
                status: String::new(),
                usage: Usage {
                    input: 12_000,
                    output: 800,
                    cache_read: 0,
                    cache_write: 0,
                },
                cost: Some(0.42),
                pending,
                started: NOW - 1_200,
            };
            vec![
                live(1, State::Busy, None),
                live(
                    2,
                    State::Asking,
                    Some(Pending {
                        call_id: "c1".into(),
                        tool: "run_command".into(),
                        summary: "cargo test".into(),
                        why: "commands ask in this profile".into(),
                        remember: true,
                        preview: termos_ai::tools::Preview::None,
                    }),
                ),
            ]
        }
        fn act(&self, key: u64, act: Act) -> bool {
            self.0.lock().expect("calls").push(format!("{act:?} {key}"));
            true
        }
    }

    fn center() -> (CommandCenter, Calls) {
        let calls: Calls = Arc::default();
        let c = Arc::clone(&calls);
        let app = CommandCenter::open(
            Box::new(move || Box::new(FixtureGather(c)) as Box<dyn Gather>),
            Arc::new(FixtureAgents(Arc::clone(&calls))),
        )
        .with_time_zone(jiff::tz::TimeZone::UTC);
        (app, calls)
    }

    /// Lets the window's thread answer.
    fn settle(app: &mut CommandCenter, until: impl Fn(&CommandCenter) -> bool) {
        let start = Instant::now();
        while !until(app) {
            assert!(start.elapsed() < Duration::from_secs(5), "no answer");
            std::thread::sleep(Duration::from_millis(5));
            app.catch_up();
        }
    }

    fn render(app: &mut CommandCenter) -> String {
        let area = TuiRect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf, &Theme::default());
        let mut out = String::new();
        for y in 0..area.height {
            let line: String = (0..area.width)
                .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    fn click(app: &mut CommandCenter, screen: &str, label: &str) -> AppEffect {
        let (x, y) = screen
            .lines()
            .enumerate()
            .find_map(|(y, l)| {
                l.find(label).map(|i| {
                    (
                        u16::try_from(UnicodeWidthStr::width(&l[..i])).expect("narrow"),
                        u16::try_from(y).expect("short"),
                    )
                })
            })
            .unwrap_or_else(|| panic!("{label:?} not on screen:\n{screen}"));
        app.handle_mouse(MouseEvent {
            kind: MouseKind::Down(MouseButton::Left),
            x: x + 1,
            y,
            mods: Mods::empty(),
        })
    }

    fn loaded(app: &mut CommandCenter) {
        app.on_timer(Instant::now());
        settle(app, |a| a.loaded);
    }

    #[test]
    fn series_start_with_data_and_stretch_to_the_width() {
        assert_eq!(
            series(&[None, None, Some(1.0), None, Some(3.0)]),
            [1.0, 0.0, 3.0]
        );
        assert_eq!(stretch(&[1.0, 2.0], 2, 4), [1.0, 1.0, 2.0, 2.0]);
        // The newest half of the range keeps the right half of the width.
        assert_eq!(stretch(&[5.0], 2, 4), [5.0, 5.0]);
        assert!(stretch(&[], 2, 4).is_empty());
    }

    #[test]
    fn every_tab_from_fixtures() {
        let (mut app, calls) = center();
        assert!(app.busy(), "reading at first");
        loaded(&mut app);
        assert!(!app.busy());
        assert_eq!(calls.lock().expect("calls")[0], "refresh 24 h true");
        for (n, name) in ["1", "2", "3", "4"]
            .iter()
            .zip(["overview", "ai", "system", "history"])
        {
            app.handle_key(KeyEvent::ch(n.chars().next().expect("digit")));
            insta::assert_snapshot!(name, render(&mut app));
        }
        // The CPU history as a picture, over the braille.
        assert!(app.images().iter().all(|i| i.pixels_only));
        assert_eq!(app.images().len(), 1);
    }

    #[test]
    fn agents_are_approved_denied_and_stopped() {
        let (mut app, calls) = center();
        loaded(&mut app);
        let screen = render(&mut app);
        click(&mut app, &screen, "[Approve]");
        app.handle_key(KeyEvent::ch('d'));
        app.handle_key(KeyEvent::ch('2'));
        let screen = render(&mut app);
        click(&mut app, &screen, "[Stop]");
        assert_eq!(app.handle_key(KeyEvent::ch('q')), AppEffect::Close);
        let calls = calls.lock().expect("calls").clone();
        assert!(calls.contains(&"Approve 2".to_owned()), "{calls:?}");
        assert!(calls.contains(&"Deny 2".to_owned()), "{calls:?}");
        assert!(calls.contains(&"Stop 1".to_owned()), "{calls:?}");
        // Opening related windows runs desktop commands.
        let mut app = center().0;
        loaded(&mut app);
        app.handle_key(KeyEvent::ch('2'));
        let screen = render(&mut app);
        assert_eq!(
            click(&mut app, &screen, "[Audit]"),
            AppEffect::Command("audit.open s1".into())
        );
        assert_eq!(
            click(&mut app, &screen, "[claude (1 open)]"),
            AppEffect::Command("agent.external claude".into())
        );
    }

    #[test]
    fn processes_end_and_panes_close_after_asking() {
        let (mut app, calls) = center();
        loaded(&mut app);
        app.handle_key(KeyEvent::ch('3'));
        let screen = render(&mut app);
        // Select the second process, then End: it asks first.
        click(&mut app, &screen, "editor");
        app.handle_key(KeyEvent::plain(KeyCode::Up));
        app.handle_key(KeyEvent::ch('e'));
        let asking = render(&mut app);
        assert!(asking.contains("End process 4242 (runaway)?"), "{asking}");
        app.handle_key(KeyEvent::ch('y'));
        settle(&mut app, |a| a.message.is_some());
        let done = render(&mut app);
        assert!(done.contains("ended process 4242 (runaway)"), "{done}");
        // Close asks too, and a failure says so.
        click(&mut app, &done, "[Close]");
        let asking = render(&mut app);
        assert!(asking.contains("Close pane %1 (bash)?"), "{asking}");
        click(&mut app, &asking, "[Yes  y]");
        settle(&mut app, |a| a.message.as_ref().is_some_and(|m| m.1));
        // No, then nothing happens.
        let screen = render(&mut app);
        click(&mut app, &screen, "[Close]");
        app.handle_key(KeyEvent::ch('n'));
        let screen = render(&mut app);
        click(&mut app, &screen, "[Go]");
        settle(&mut app, |a| {
            a.message.as_ref().is_some_and(|m| m.0.contains("in front"))
        });
        let calls = calls.lock().expect("calls").clone();
        let actions: Vec<&String> = calls.iter().filter(|c| !c.starts_with("refresh")).collect();
        assert_eq!(actions, ["end 4242 runaway", "close 1", "go 1"]);
    }

    #[test]
    fn the_range_reads_the_history_again() {
        let (mut app, calls) = center();
        loaded(&mut app);
        app.handle_key(KeyEvent::ch('4'));
        let screen = render(&mut app);
        click(&mut app, &screen, "[7 days]");
        settle(&mut app, |a| a.snap.history.range == Range::Week);
        app.handle_key(KeyEvent::ch('['));
        settle(&mut app, |a| a.snap.history.range == Range::Day);
        assert!(
            calls
                .lock()
                .expect("calls")
                .contains(&"refresh 7 days true".to_owned())
        );
        // The timer refreshes without reading the history again.
        app.on_timer(Instant::now());
        let start = Instant::now();
        while !calls
            .lock()
            .expect("calls")
            .contains(&"refresh 24 h false".to_owned())
        {
            assert!(start.elapsed() < Duration::from_secs(5), "no refresh");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.deadline().is_some_and(|d| d > Instant::now()));
    }
}
