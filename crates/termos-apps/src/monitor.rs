// SPDX-License-Identifier: GPL-3.0-or-later
//! The system monitor: CPU, memory, GPUs, temperatures, disks, network
//! and the busiest processes, from `termos-hw`.
//!
//! Readings are taken on the monitor's own thread, once a second, and
//! only while the window is on screen: the desktop gives an app timers
//! only when some client can see it (D-028), so a minimized or
//! background monitor costs nothing.
//!
//! `Tab` switches to the process manager (M13c, D-122): every process,
//! filtered, sorted or as a tree, with its user, time, state, ports
//! (Linux) and command line; it ends, kills, pauses and renices them,
//! asking first.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::style::Style as TuiStyle;
use ratatui_core::widgets::Widget;
use termos_core::app::{App, AppEffect, AppImage, AppWaker, new_image_key, tui_style};
use termos_core::input::{KeyCode, KeyEvent};
use termos_core::widgets::{LineChart, Meter, Series, Sparkline};
use termos_hw::{Action, Inventory, ProcessInfo, ProcessSample, Sample, Source};
use termos_render::cell::{Attrs, Style};
use termos_render::color::{Color, Rgb};
use termos_render::graphics::Picture;
use termos_render::graphics::chart::{ChartSeries, area_chart};
use termos_render::grid::Rect;
use termos_render::theme::Theme;

/// How often the monitor reads the machine while it is on screen.
pub const INTERVAL: Duration = Duration::from_secs(1);
/// Seconds of history the charts keep.
const HISTORY: usize = 120;

/// Makes the source on the monitor's thread (the real one is not `Send`).
pub type SourceFactory = Box<dyn FnOnce() -> Result<Box<dyn Source>, String> + Send>;

enum Msg {
    Inventory(Inventory),
    Sample(Box<Sample>),
    /// Every process, for the process view.
    Table(Vec<ProcessInfo>),
    Failed(String),
}

/// What a reading asks the thread for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ask {
    /// Every process too (the process view is shown).
    table: bool,
}

/// Which view the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Overview,
    Processes,
}

/// What the process view waits for an answer about.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Act(u32, String, Action),
    Renice(u32, String, i32),
}

/// The process view's state.
#[derive(Debug, Default)]
struct ProcView {
    table: Vec<ProcessInfo>,
    view: crate::procs::View,
    selected: usize,
    top: usize,
    /// Typing the filter.
    filtering: bool,
    pending: Option<Pending>,
    message: Option<(String, bool)>,
    /// Ask before acting (`[processes] confirm`).
    confirm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcSort {
    Cpu,
    Memory,
}

pub struct SystemMonitor {
    inventory: Option<Inventory>,
    last: Option<Sample>,
    error: Option<String>,
    cpu: VecDeque<f64>,
    mem: VecDeque<f64>,
    rx_hist: VecDeque<f64>,
    tx_hist: VecDeque<f64>,
    /// Asks the thread for a reading; dropping it stops the thread.
    ask: Option<Sender<Ask>>,
    tab: Tab,
    procs: ProcView,
    results: Receiver<Msg>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    next: Option<Instant>,
    sort: ProcSort,
    /// Readings so far (the CPU chart's picture follows it).
    samples: u64,
    /// The CPU chart as a picture, for terminals that show pixels (D-090).
    chart: Option<Chart>,
    /// What [`App::images`] reports after the last render.
    images: Vec<AppImage>,
}

/// The CPU chart's picture, made again only when something in it changes.
#[derive(Debug, Clone)]
struct Chart {
    samples: u64,
    size: (u16, u16),
    color: Rgb,
    key: u64,
    picture: Arc<Picture>,
}

impl std::fmt::Debug for SystemMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemMonitor").finish_non_exhaustive()
    }
}

fn wake(waker: &Mutex<Option<AppWaker>>) {
    if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
        w.wake();
    }
}

fn push(h: &mut VecDeque<f64>, v: f64) {
    if h.len() == HISTORY {
        h.pop_front();
    }
    h.push_back(v);
}

impl SystemMonitor {
    /// The monitor of this machine.
    pub fn open() -> Self {
        Self::with_source(Box::new(|| {
            termos_hw::Sampler::new()
                .map(|s| Box::new(s) as Box<dyn Source>)
                .map_err(|e| format!("cannot read this machine: {e}"))
        }))
    }

    /// A monitor of any source (tests use fixtures).
    pub fn with_source(factory: SourceFactory) -> Self {
        let waker: Arc<Mutex<Option<AppWaker>>> = Arc::new(Mutex::new(None));
        let (ask_tx, ask_rx) = channel::<Ask>();
        let (tx, rx) = channel();
        let w = Arc::clone(&waker);
        let spawned = std::thread::Builder::new()
            .name("system-monitor".into())
            .spawn(move || {
                let mut source = match factory() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Msg::Failed(e));
                        wake(&w);
                        return;
                    }
                };
                if tx.send(Msg::Inventory(source.inventory())).is_err() {
                    return;
                }
                wake(&w);
                // One reading per request; none while nobody asks.
                while let Ok(ask) = ask_rx.recv() {
                    if tx.send(Msg::Sample(Box::new(source.sample()))).is_err() {
                        return;
                    }
                    if ask.table && tx.send(Msg::Table(source.processes(true))).is_err() {
                        return;
                    }
                    wake(&w);
                }
            });
        let (ask, error) = match spawned {
            Ok(_) => (Some(ask_tx), None),
            Err(e) => (None, Some(format!("cannot start: {e}"))),
        };
        Self {
            inventory: None,
            last: None,
            error,
            cpu: VecDeque::new(),
            mem: VecDeque::new(),
            rx_hist: VecDeque::new(),
            tx_hist: VecDeque::new(),
            ask,
            tab: Tab::Overview,
            procs: ProcView {
                confirm: true,
                ..ProcView::default()
            },
            results: rx,
            waker,
            // The first reading as soon as the window is on screen.
            next: Some(Instant::now()),
            sort: ProcSort::Cpu,
            samples: 0,
            chart: None,
            images: Vec::new(),
        }
    }

    fn poll(&mut self) {
        loop {
            match self.results.try_recv() {
                Ok(Msg::Inventory(i)) => self.inventory = Some(i),
                Ok(Msg::Sample(s)) => {
                    push(&mut self.cpu, f64::from(s.cpu.total));
                    let m = &s.memory;
                    push(&mut self.mem, ratio(m.used, m.total) * 100.0);
                    push(&mut self.rx_hist, s.net.rx_per_sec as f64);
                    push(&mut self.tx_hist, s.net.tx_per_sec as f64);
                    self.last = Some(*s);
                    self.samples += 1;
                }
                Ok(Msg::Table(t)) => self.procs.table = t,
                Ok(Msg::Failed(e)) => {
                    self.error = Some(e);
                    self.ask = None;
                    self.next = None;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.ask = None;
                    self.next = None;
                    break;
                }
            }
        }
    }
}

impl SystemMonitor {
    /// The monitor of this machine, open at its process view; with
    /// `root`, only that process and its descendants (a pane's).
    /// `confirm` asks before acting on one (`[processes] confirm`).
    pub fn processes(root: Option<u32>, confirm: bool) -> Self {
        let mut m = Self::open();
        m.tab = Tab::Processes;
        m.procs.view.root = root;
        m.procs.view.tree = root.is_some();
        m.procs.confirm = confirm;
        m
    }

    /// Whether to ask before acting on a process.
    pub fn set_confirm(&mut self, confirm: bool) {
        self.procs.confirm = confirm;
    }

    /// The view shown.
    pub fn tab(&self) -> Tab {
        self.tab
    }

    fn proc_rows(&self) -> Vec<crate::procs::Row> {
        crate::procs::rows(&self.procs.table, &self.procs.view)
    }

    /// The selected process.
    fn proc_selected(&self) -> Option<ProcessInfo> {
        self.proc_rows()
            .into_iter()
            .nth(self.procs.selected)
            .map(|r| r.info)
    }

    /// Asks first (or acts at once without `confirm`).
    fn proc_ask(&mut self, pending: Pending) -> AppEffect {
        if self.procs.confirm {
            self.procs.pending = Some(pending);
        } else {
            self.proc_do(pending);
        }
        AppEffect::Redraw
    }

    fn proc_do(&mut self, pending: Pending) {
        self.procs.message = Some(match pending {
            Pending::Act(pid, name, action) => match termos_hw::procs::act(pid, action) {
                Ok(()) => (format!("{}: {name} ({pid})", past(action)), false),
                Err(e) => (e, true),
            },
            Pending::Renice(pid, name, delta) => match termos_hw::procs::renice(pid, delta) {
                Ok(n) => (format!("{name} ({pid}) now has niceness {n}"), false),
                Err(e) => (e, true),
            },
        });
        // Read again soon, so the change shows.
        self.next = Some(Instant::now());
    }

    fn proc_key(&mut self, key: KeyEvent) -> AppEffect {
        let p = &mut self.procs;
        if let Some(pending) = p.pending.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.proc_do(pending);
            }
            return AppEffect::Redraw;
        }
        if p.filtering {
            match key.code {
                KeyCode::Enter => p.filtering = false,
                KeyCode::Esc => {
                    p.filtering = false;
                    p.view.filter.clear();
                }
                KeyCode::Backspace => {
                    p.view.filter.pop();
                }
                KeyCode::Char(c)
                    if !key.mods.intersects(
                        termos_core::input::Mods::CTRL | termos_core::input::Mods::ALT,
                    ) =>
                {
                    p.view.filter.push(c);
                }
                _ => return AppEffect::None,
            }
            p.selected = 0;
            return AppEffect::Redraw;
        }
        let n = self.proc_rows().len();
        let page = 10;
        let p = &mut self.procs;
        p.message = None;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                p.selected = (p.selected + 1).min(n.saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::PageDown => p.selected = (p.selected + page).min(n.saturating_sub(1)),
            KeyCode::PageUp => p.selected = p.selected.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => p.selected = 0,
            KeyCode::Char('G') | KeyCode::End => p.selected = n.saturating_sub(1),
            KeyCode::Char('/') => p.filtering = true,
            KeyCode::Char('s') => {
                p.view.sort = p.view.sort.next();
                p.message = Some((format!("sorted by {}", p.view.sort.label()), false));
            }
            KeyCode::Char('t') => p.view.tree = !p.view.tree,
            KeyCode::Char('a') => {
                // The whole machine again, after a pane's processes.
                p.view.root = None;
                p.selected = 0;
            }
            KeyCode::Esc if !p.view.filter.is_empty() => {
                p.view.filter.clear();
                p.selected = 0;
            }
            KeyCode::Char('e')
            | KeyCode::Char('K')
            | KeyCode::Char('z')
            | KeyCode::Char('+')
            | KeyCode::Char('-') => {
                let Some(sel) = self.proc_selected() else {
                    return AppEffect::None;
                };
                let name = sel.name.clone();
                let pending = match key.code {
                    KeyCode::Char('e') => Pending::Act(sel.pid, name, Action::End),
                    KeyCode::Char('K') => Pending::Act(sel.pid, name, Action::Kill),
                    KeyCode::Char('z') => {
                        let stopped = sel.state.to_lowercase().starts_with("stop");
                        let a = if stopped {
                            Action::Resume
                        } else {
                            Action::Pause
                        };
                        Pending::Act(sel.pid, name, a)
                    }
                    KeyCode::Char('+') => Pending::Renice(sel.pid, name, -1),
                    _ => Pending::Renice(sel.pid, name, 1),
                };
                return self.proc_ask(pending);
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    /// The process view, the whole window.
    fn draw_table(&mut self, pen: &mut Pen<'_>, area: TuiRect) {
        let c = pen.theme.colors.clone();
        if area.height < 4 {
            return;
        }
        let rows = self.proc_rows();
        let p = &mut self.procs;
        p.selected = p.selected.min(rows.len().saturating_sub(1));
        let scope = if p.view.root.is_some() {
            " · a pane's processes (a: all)"
        } else {
            ""
        };
        let head = format!(
            "Processes · {} · by {} (s){}{}",
            rows.len(),
            p.view.sort.label(),
            if p.view.tree { " · tree (t)" } else { "" },
            scope
        );
        pen.heading(area, &head);
        let ports = termos_hw::procs::PORTS_AVAILABLE;
        let fixed = 7 + 1 + 9 + 1 + 6 + 1 + 7 + 1 + 7 + 1 + if ports { 12 } else { 0 };
        let cmd_w = usize::from(area.width).saturating_sub(fixed);
        let header = if ports {
            format!(
                "{:>7} {:<9} {:>6} {:>7} {:>7} {:<11} {}",
                "PID", "USER", "CPU%", "MEM", "TIME", "PORTS", "COMMAND"
            )
        } else {
            format!(
                "{:>7} {:<9} {:>6} {:>7} {:>7} {}",
                "PID", "USER", "CPU%", "MEM", "TIME", "COMMAND"
            )
        };
        pen.text(
            area.x,
            area.y + 1,
            &header,
            area.width,
            Style::new().fg(c.muted),
        );
        let list_h = usize::from(area.height.saturating_sub(3));
        if p.selected < p.top {
            p.top = p.selected;
        } else if list_h > 0 && p.selected >= p.top + list_h {
            p.top = p.selected + 1 - list_h;
        }
        for (i, r) in rows.iter().enumerate().skip(p.top).take(list_h) {
            let y = area.y + 2 + u16::try_from(i - p.top).unwrap_or(0);
            let info = &r.info;
            let user: String = info
                .user
                .clone()
                .unwrap_or_else(|| "-".into())
                .chars()
                .take(9)
                .collect();
            let indent = "  ".repeat(r.depth.min(12));
            let command: String = format!("{indent}{}", info.command)
                .chars()
                .take(cmd_w)
                .collect();
            let port_text: String = info
                .ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(",")
                .chars()
                .take(11)
                .collect();
            let line = if ports {
                format!(
                    "{:>7} {:<9} {:>6.1} {:>7} {:>7} {:<11} {}",
                    info.pid,
                    user,
                    info.cpu,
                    bytes(info.memory),
                    crate::procs::run_time(info.run_time),
                    port_text,
                    command
                )
            } else {
                format!(
                    "{:>7} {:<9} {:>6.1} {:>7} {:>7} {}",
                    info.pid,
                    user,
                    info.cpu,
                    bytes(info.memory),
                    crate::procs::run_time(info.run_time),
                    command
                )
            };
            let mut st = Style::new().fg(c.fg);
            if info.state.to_lowercase().starts_with("stop") {
                st = st.fg(c.warn);
            }
            if i == p.selected {
                st = c.selected(st);
                pen.buf.set_style(
                    TuiRect::new(area.x, y, area.width, 1),
                    tui_style(Style::new().bg(c.selection_bg)),
                );
            }
            pen.text(area.x, y, &line, area.width, st);
        }
        if rows.is_empty() {
            let text = if p.table.is_empty() {
                "reading…"
            } else {
                "(no match)"
            };
            pen.text(
                area.x,
                area.y + 2,
                text,
                area.width,
                Style::new().fg(c.muted),
            );
        }
        let y = area.bottom() - 1;
        let (line, st) = if let Some(pending) = &p.pending {
            let q = match pending {
                Pending::Act(pid, name, a) => format!("{} {name} ({pid})? y/n", capital(a.verb())),
                Pending::Renice(pid, name, d) => format!(
                    "{} the priority of {name} ({pid})? y/n",
                    if *d < 0 { "Raise" } else { "Lower" }
                ),
            };
            (q, Style::new().fg(c.warn).attrs(Attrs::BOLD))
        } else if p.filtering {
            (format!("/{}▏", p.view.filter), Style::new().fg(c.fg))
        } else if let Some((m, err)) = &p.message {
            (
                m.clone(),
                Style::new().fg(if *err { c.error } else { c.muted }),
            )
        } else {
            let note = if ports { "" } else { " · ports: Linux only" };
            (
                format!(
                    "/ filter · s sort · t tree · e end · K kill · z pause · +/- priority · Tab overview{note}"
                ),
                Style::new().fg(c.muted),
            )
        };
        pen.text(area.x, y, &line, area.width, st);
    }
}

/// `ended`, `killed`, …
fn past(a: Action) -> &'static str {
    match a {
        Action::End => "asked to end",
        Action::Kill => "killed",
        Action::Pause => "paused",
        Action::Resume => "resumed",
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn ratio(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64).clamp(0.0, 1.0)
    }
}

/// `1.5G`, `512M`, …
fn bytes(n: u64) -> String {
    crate::files::human_size(n)
}

fn duration(d: Duration) -> String {
    let s = d.as_secs();
    let (days, hours, mins) = (s / 86_400, (s / 3600) % 24, (s / 60) % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

/// Fill colour by level: the accent, then warn above 75%, error above 90%.
fn level_color(theme: &Theme, r: f64) -> Color {
    let c = &theme.colors;
    if r > 0.9 {
        c.error
    } else if r > 0.75 {
        c.warn
    } else {
        c.accent
    }
}

/// A meter's colour at `t` along it (truecolor, D-090): the accent, then
/// through the warning colour to the error colour toward the end, at the
/// same levels where a flat meter changes colour.
fn heat(theme: &Theme, t: f64) -> Color {
    let c = &theme.colors;
    let (a, w, e) = (theme.rgb(c.accent), theme.rgb(c.warn), theme.rgb(c.error));
    // In range: fractions of 1.
    let rgb = if t < 0.6 {
        a
    } else if t < 0.8 {
        a.blend(w, ((t - 0.6) / 0.2) as f32)
    } else {
        w.blend(e, ((t - 0.8) / 0.15).min(1.0) as f32)
    };
    Color::Rgb(rgb)
}

fn level_style(theme: &Theme, r: f64) -> TuiStyle {
    tui_style(Style::new().fg(level_color(theme, r)))
}

struct Pen<'a> {
    buf: &'a mut Buffer,
    theme: &'a Theme,
}

impl Pen<'_> {
    fn text(&mut self, x: u16, y: u16, s: &str, max: u16, style: Style) {
        self.buf
            .set_stringn(x, y, s, usize::from(max), tui_style(style));
    }

    fn heading(&mut self, area: TuiRect, s: &str) {
        let st = Style::new().fg(self.theme.colors.fg).attrs(Attrs::BOLD);
        self.text(area.x, area.y, s, area.width, st);
    }

    fn meter(&mut self, area: TuiRect, label: &str, label_w: u16, r: f64, value: &str) {
        let c = &self.theme.colors;
        let theme = self.theme;
        let shade = |t: f64| tui_style(Style::new().fg(heat(theme, t)));
        Meter {
            ratio: r,
            label,
            value,
            label_width: label_w,
            fill: level_style(self.theme, r),
            empty: tui_style(Style::new().fg(c.border)),
            text: tui_style(Style::new().fg(c.fg)),
            fill_at: theme
                .gradients
                .then_some(&shade as &dyn Fn(f64) -> TuiStyle),
        }
        .render(area, self.buf);
    }
}

impl SystemMonitor {
    /// The CPU chart's picture for a plot of `size` cells, in `color`:
    /// the one made last if nothing changed.
    fn chart_picture(&mut self, size: (u16, u16), color: Rgb, guide: Rgb) -> Option<&Chart> {
        let fresh = self
            .chart
            .as_ref()
            .is_some_and(|c| c.samples == self.samples && c.size == size && c.color == color);
        if !fresh {
            let data: Vec<f64> = self.cpu.iter().copied().collect();
            // The chart's own resolution; the image layer scales it to the
            // terminal's cells.
            let cell = termos_render::graphics::DEFAULT_CELL;
            let picture = area_chart(
                &[ChartSeries {
                    data: &data,
                    color,
                    fill: true,
                }],
                100.0,
                guide,
                u32::from(size.0) * cell.width,
                u32::from(size.1) * cell.height,
            )
            .ok()?;
            self.chart = Some(Chart {
                samples: self.samples,
                size,
                color,
                key: new_image_key(),
                picture: Arc::new(picture),
            });
        }
        self.chart.as_ref()
    }

    /// CPU: a chart of the total and a meter per core. Returns rows used,
    /// and where the chart's plot is.
    fn draw_cpu(&self, pen: &mut Pen<'_>, area: TuiRect, s: &Sample) -> (u16, TuiRect) {
        let c = &pen.theme.colors;
        let freq = s
            .cpu
            .freq_mhz
            .map(|f| format!(" · {:.1} GHz", f as f64 / 1000.0))
            .unwrap_or_default();
        pen.heading(area, &format!("CPU {:>3.0}%{freq}", s.cpu.total));
        let chart_h = (area.height / 3)
            .clamp(3, 6)
            .min(area.height.saturating_sub(1));
        let data: Vec<f64> = self.cpu.iter().copied().collect();
        let series = [Series {
            data: &data,
            style: level_style(pen.theme, f64::from(s.cpu.total) / 100.0),
        }];
        let chart = TuiRect::new(area.x, area.y + 1, area.width, chart_h);
        LineChart {
            series: &series,
            max: Some(100.0),
            labels: Some(("100", "0")),
            label_style: tui_style(Style::new().fg(c.muted)),
        }
        .render(chart, pen.buf);
        // Where LineChart puts the plot: right of its labels.
        let label_w = 4.min(chart.width / 2);
        let plot = TuiRect::new(
            chart.x + label_w,
            chart.y,
            chart.width - label_w,
            chart.height,
        );
        let mut y = area.y + 1 + chart_h;
        // Cores in columns of meters, 18+ cells each.
        let n = s.cpu.cores.len();
        let cols = usize::from((area.width / 18).max(1)).min(n.max(1));
        let rows = n.div_ceil(cols);
        let col_w = area.width / u16::try_from(cols).unwrap_or(1);
        let label_w = u16::try_from(n.to_string().len()).unwrap_or(2) + 1;
        let avail = area.bottom().saturating_sub(y);
        for (i, core) in s.cpu.cores.iter().enumerate() {
            let (col, row) = (i / rows.max(1), i % rows.max(1));
            let row = u16::try_from(row).unwrap_or(u16::MAX);
            if row >= avail {
                continue;
            }
            let x = area.x + col_w * u16::try_from(col).unwrap_or(0);
            pen.meter(
                TuiRect::new(x, y + row, col_w.saturating_sub(1), 1),
                &i.to_string(),
                label_w,
                f64::from(*core) / 100.0,
                &format!("{core:>3.0}%"),
            );
        }
        y += u16::try_from(rows).unwrap_or(0).min(avail);
        (y - area.y, plot)
    }

    fn draw_memory(&self, pen: &mut Pen<'_>, area: TuiRect, s: &Sample) -> u16 {
        let m = &s.memory;
        pen.heading(area, "Memory");
        let mut y = area.y + 1;
        let w = area.width;
        pen.meter(
            TuiRect::new(area.x, y, w, 1),
            "RAM",
            5,
            ratio(m.used, m.total),
            &format!("{}/{}", bytes(m.used), bytes(m.total)),
        );
        y += 1;
        if m.swap_total > 0 && y < area.bottom() {
            pen.meter(
                TuiRect::new(area.x, y, w, 1),
                "Swap",
                5,
                ratio(m.swap_used, m.swap_total),
                &format!("{}/{}", bytes(m.swap_used), bytes(m.swap_total)),
            );
            y += 1;
        }
        y - area.y
    }

    fn draw_gpus(&self, pen: &mut Pen<'_>, area: TuiRect, s: &Sample) -> u16 {
        if s.gpus.is_empty() {
            return 0;
        }
        let c = pen.theme.colors.clone();
        let mut y = area.y;
        for g in &s.gpus {
            if y >= area.bottom() {
                break;
            }
            let mut extra = Vec::new();
            if let Some(t) = g.celsius {
                extra.push(format!("{t:.0}°C"));
            }
            if let Some(w) = g.watts {
                extra.push(format!("{w:.0} W"));
            }
            let head = if extra.is_empty() {
                format!("GPU {}", g.name)
            } else {
                format!("GPU {} · {}", g.name, extra.join(" · "))
            };
            pen.heading(TuiRect::new(area.x, y, area.width, 1), &head);
            y += 1;
            if let Some(u) = g.usage
                && y < area.bottom()
            {
                pen.meter(
                    TuiRect::new(area.x, y, area.width, 1),
                    "Load",
                    5,
                    f64::from(u) / 100.0,
                    &format!("{u:>3.0}%"),
                );
                y += 1;
            }
            if let (Some(used), Some(total)) = (g.memory_used, g.memory_total)
                && y < area.bottom()
            {
                pen.meter(
                    TuiRect::new(area.x, y, area.width, 1),
                    "VRAM",
                    5,
                    ratio(used, total),
                    &format!("{}/{}", bytes(used), bytes(total)),
                );
                y += 1;
            }
            if g.usage.is_none() && g.memory_total.is_none() && y < area.bottom() {
                pen.text(
                    area.x,
                    y,
                    "no load readings for this GPU",
                    area.width,
                    Style::new().fg(c.muted),
                );
                y += 1;
            }
        }
        y - area.y
    }

    fn draw_net(&self, pen: &mut Pen<'_>, area: TuiRect, s: &Sample) -> u16 {
        if area.height < 3 {
            return 0;
        }
        let c = pen.theme.colors.clone();
        pen.heading(area, "Network");
        let label_w = 13u16;
        // One scale for both, so their sizes compare.
        let top = self
            .rx_hist
            .iter()
            .chain(&self.tx_hist)
            .copied()
            .fold(1.0, f64::max);
        for (i, (name, hist, rate, color)) in [
            ("↓", &self.rx_hist, s.net.rx_per_sec, c.accent),
            ("↑", &self.tx_hist, s.net.tx_per_sec, c.warn),
        ]
        .into_iter()
        .enumerate()
        {
            let y = area.y + 1 + u16::try_from(i).unwrap_or(0);
            pen.text(
                area.x,
                y,
                &format!("{name} {:>8}/s", bytes(rate)),
                label_w,
                Style::new().fg(c.fg),
            );
            let data: Vec<f64> = hist.iter().copied().collect();
            Sparkline {
                data: &data,
                max: Some(top),
                style: tui_style(Style::new().fg(color)),
            }
            .render(
                TuiRect::new(area.x + label_w, y, area.width.saturating_sub(label_w), 1),
                pen.buf,
            );
        }
        3
    }

    fn draw_temps(&self, pen: &mut Pen<'_>, area: TuiRect, s: &Sample) -> u16 {
        if s.temps.is_empty() || area.height < 2 {
            return 0;
        }
        let c = pen.theme.colors.clone();
        pen.heading(area, "Temperatures");
        let col_w = 26u16.min(area.width);
        let cols = (area.width / col_w).max(1);
        let max_rows = area.height - 1;
        let mut used = 0u16;
        for (i, t) in s.temps.iter().enumerate() {
            let i = u16::try_from(i).unwrap_or(u16::MAX);
            let (col, row) = (i / max_rows.max(1), i % max_rows.max(1));
            if col >= cols {
                break;
            }
            used = used.max(row + 1);
            let hot = t.critical.is_some_and(|crit| t.celsius >= crit - 10.0) || t.celsius >= 90.0;
            let st = Style::new().fg(if hot { c.error } else { c.fg });
            let value = format!("{:>5.0}°C", t.celsius);
            let x = area.x + col * col_w;
            let label_w = col_w.saturating_sub(9);
            pen.text(
                x,
                area.y + 1 + row,
                &t.label,
                label_w,
                Style::new().fg(c.muted),
            );
            pen.text(x + label_w, area.y + 1 + row, &value, 8, st);
        }
        1 + used
    }

    fn draw_disks(&self, pen: &mut Pen<'_>, area: TuiRect, s: &Sample) -> u16 {
        if s.disks.is_empty() || area.height < 2 {
            return 0;
        }
        pen.heading(area, "Disks");
        let label_w = s
            .disks
            .iter()
            .map(|d| u16::try_from(d.mount.chars().count()).unwrap_or(20))
            .max()
            .unwrap_or(4)
            .clamp(4, 16)
            + 1;
        let mut y = area.y + 1;
        for d in &s.disks {
            if y >= area.bottom() {
                break;
            }
            let used = d.total.saturating_sub(d.available);
            pen.meter(
                TuiRect::new(area.x, y, area.width, 1),
                &d.mount,
                label_w,
                ratio(used, d.total),
                &format!("{}/{}", bytes(used), bytes(d.total)),
            );
            y += 1;
        }
        y - area.y
    }

    fn draw_processes(&self, pen: &mut Pen<'_>, area: TuiRect, s: &Sample) {
        if area.height < 2 {
            return;
        }
        let c = pen.theme.colors.clone();
        let mut procs: Vec<&ProcessSample> = s.processes.iter().collect();
        if self.sort == ProcSort::Memory {
            procs.sort_by(|a, b| b.memory.cmp(&a.memory).then(a.pid.cmp(&b.pid)));
        }
        let title = match self.sort {
            ProcSort::Cpu => "Processes · by CPU (s: by memory)",
            ProcSort::Memory => "Processes · by memory (s: by CPU)",
        };
        pen.heading(area, title);
        // PID, NAME, CPU% and MEM with a space between each.
        let name_w = usize::from(area.width.saturating_sub(7 + 1 + 1 + 7 + 1 + 8));
        let header = format!(
            "{:>7} {:<name_w$} {:>7} {:>8}",
            "PID", "NAME", "CPU%", "MEM"
        );
        pen.text(
            area.x,
            area.y + 1,
            &header,
            area.width,
            Style::new().fg(c.muted),
        );
        for (i, p) in procs
            .iter()
            .take(usize::from(area.height.saturating_sub(2)))
            .enumerate()
        {
            let y = area.y + 2 + u16::try_from(i).unwrap_or(0);
            let name: String = p.name.chars().take(name_w).collect();
            let line = format!(
                "{:>7} {:<name_w$} {:>7.1} {:>8}",
                p.pid,
                name,
                p.cpu,
                bytes(p.memory)
            );
            pen.text(area.x, y, &line, area.width, Style::new().fg(c.fg));
        }
    }
}

impl App for SystemMonitor {
    fn reopen(&self) -> Option<String> {
        Some(match self.tab {
            Tab::Overview => "monitor.open".to_owned(),
            Tab::Processes => "processes.open".to_owned(),
        })
    }

    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Monitor
    }

    fn images(&self) -> Vec<AppImage> {
        self.images.clone()
    }

    fn busy(&self) -> bool {
        self.last.is_none() && self.error.is_none()
    }

    fn catch_up(&mut self) {
        self.poll();
    }

    fn title(&self) -> String {
        match self.tab {
            Tab::Overview => "System monitor".into(),
            Tab::Processes => "Processes".into(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        if key.code == KeyCode::Tab && !self.procs.filtering && self.procs.pending.is_none() {
            self.tab = match self.tab {
                Tab::Overview => Tab::Processes,
                Tab::Processes => Tab::Overview,
            };
            // The process list as soon as it shows.
            self.next = self.ask.as_ref().map(|_| Instant::now());
            return AppEffect::Redraw;
        }
        if self.tab == Tab::Processes {
            if key.code == KeyCode::Char('q')
                && !self.procs.filtering
                && self.procs.pending.is_none()
            {
                return AppEffect::Close;
            }
            return self.proc_key(key);
        }
        if !key.mods.is_empty() {
            return AppEffect::None;
        }
        match key.code {
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    ProcSort::Cpu => ProcSort::Memory,
                    ProcSort::Memory => ProcSort::Cpu,
                };
                AppEffect::Redraw
            }
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.poll();
        if area.is_empty() {
            return;
        }
        let mut pen = Pen { buf, theme };
        let c = theme.colors.clone();
        if let Some(e) = &self.error {
            pen.text(area.x, area.y, e, area.width, Style::new().fg(c.error));
            return;
        }
        if self.tab == Tab::Processes {
            self.images.clear();
            self.draw_table(&mut pen, area);
            return;
        }
        let Some(s) = self.last.clone() else {
            pen.text(
                area.x,
                area.y,
                "reading…",
                area.width,
                Style::new().fg(c.muted),
            );
            return;
        };
        // The machine on top.
        if let Some(inv) = &self.inventory {
            let load = s
                .load
                .map(|l| format!(" · load {:.2} {:.2} {:.2}", l[0], l[1], l[2]))
                .unwrap_or_default();
            let line = format!(
                "{} · {} · {} · up {}{load}",
                inv.host.as_deref().unwrap_or("this machine"),
                inv.os,
                inv.cpu.model,
                duration(s.uptime)
            );
            pen.text(area.x, area.y, &line, area.width, Style::new().fg(c.muted));
        }
        let body = TuiRect::new(
            area.x,
            area.y + 2,
            area.width,
            area.height.saturating_sub(2),
        );
        let (left, right) = if body.width >= 100 {
            let lw = body.width / 2 - 1;
            (
                TuiRect::new(body.x, body.y, lw, body.height),
                Some(TuiRect::new(
                    body.x + lw + 2,
                    body.y,
                    body.width - lw - 2,
                    body.height,
                )),
            )
        } else {
            (body, None)
        };
        // Left: CPU, memory, GPUs, network. Right (or below): temperatures,
        // disks, processes.
        let mut y = left.y;
        let rest = |y: u16, a: TuiRect| TuiRect::new(a.x, y, a.width, a.bottom().saturating_sub(y));
        let cpu_area = TuiRect::new(
            left.x,
            y,
            left.width,
            (left.height / 2).max(5).min(left.height),
        );
        let (used, plot) = self.draw_cpu(&mut pen, cpu_area, &s);
        y += used + 1;
        self.images.clear();
        if !plot.is_empty() {
            let color = theme.rgb(level_color(theme, f64::from(s.cpu.total) / 100.0));
            let guide = theme.rgb(c.border);
            let made = self
                .chart_picture((plot.width, plot.height), color, guide)
                .map(|c| (c.key, Arc::clone(&c.picture)));
            if let Some((key, picture)) = made {
                self.images.push(AppImage {
                    key,
                    picture,
                    rect: Rect::new(plot.x, plot.y, plot.width, plot.height),
                    upscale: true,
                    scroll_top: 0,
                    pixels_only: true,
                });
            }
        }
        y += self.draw_memory(&mut pen, rest(y, left), &s) + 1;
        let g = self.draw_gpus(&mut pen, rest(y, left), &s);
        if g > 0 {
            y += g + 1;
        }
        let n = self.draw_net(&mut pen, rest(y, left), &s);
        if n > 0 {
            y += n + 1;
        }
        let other = right.unwrap_or_else(|| rest(y, left));
        let mut y = other.y;
        let t = self.draw_temps(&mut pen, rest(y, other), &s);
        if t > 0 {
            y += t + 1;
        }
        let d = self.draw_disks(&mut pen, rest(y, other), &s);
        if d > 0 {
            y += d + 1;
        }
        self.draw_processes(&mut pen, rest(y, other), &s);
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker.clone());
        }
        waker.wake();
    }

    fn deadline(&self) -> Option<Instant> {
        self.next
    }

    fn on_timer(&mut self, now: Instant) -> AppEffect {
        let Some(ask) = &self.ask else {
            self.next = None;
            return AppEffect::None;
        };
        let table = self.tab == Tab::Processes;
        if ask.send(Ask { table }).is_err() {
            self.ask = None;
            self.next = None;
            return AppEffect::None;
        }
        self.next = Some(now + INTERVAL);
        AppEffect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_hw::{
        CpuInfo, CpuSample, DiskSample, GpuSample, GpuVendor, MemorySample, NetSample, Temp,
    };

    /// A machine that always answers the same, with a CPU load that ramps.
    struct Fixture {
        n: u32,
    }

    impl Source for Fixture {
        fn inventory(&mut self) -> Inventory {
            Inventory {
                os: "TestOS 1.0".into(),
                kernel: None,
                arch: "x86_64".into(),
                host: Some("bench".into()),
                cpu: CpuInfo {
                    model: "Fixture CPU".into(),
                    cores: Some(4),
                    threads: 4,
                },
                memory_total: 16 << 30,
                swap_total: 0,
                gpus: Vec::new(),
            }
        }

        fn sample(&mut self) -> Sample {
            self.n += 1;
            let load = (self.n * 10).min(100) as f32;
            Sample {
                cpu: CpuSample {
                    total: load,
                    cores: vec![load, 5.0, 95.0, 50.0],
                    freq_mhz: Some(2800),
                },
                memory: MemorySample {
                    total: 16 << 30,
                    used: 6 << 30,
                    available: 10 << 30,
                    swap_total: 0,
                    swap_used: 0,
                },
                load: Some([0.5, 0.4, 0.3]),
                uptime: Duration::from_secs(3 * 3600 + 120),
                temps: vec![Temp {
                    label: "cpu package".into(),
                    celsius: 54.0,
                    critical: Some(100.0),
                }],
                gpus: vec![GpuSample {
                    name: "Fixture GPU".into(),
                    vendor: GpuVendor::Amd,
                    usage: Some(37.0),
                    memory_used: Some(1 << 30),
                    memory_total: Some(8 << 30),
                    celsius: Some(61.0),
                    watts: Some(45.0),
                }],
                disks: vec![DiskSample {
                    mount: "/".into(),
                    fs: "ext4".into(),
                    total: 500 << 30,
                    available: 100 << 30,
                    removable: false,
                }],
                net: NetSample {
                    rx_per_sec: u64::from(self.n) * 1000,
                    tx_per_sec: 500,
                },
                processes: vec![
                    ProcessSample {
                        pid: 42,
                        name: "compiler".into(),
                        cpu: 180.5,
                        memory: 2 << 30,
                    },
                    ProcessSample {
                        pid: 7,
                        name: "editor".into(),
                        cpu: 3.0,
                        memory: 300 << 20,
                    },
                ],
            }
        }
    }

    fn monitor() -> SystemMonitor {
        SystemMonitor::with_source(Box::new(|| {
            Ok(Box::new(Fixture { n: 0 }) as Box<dyn Source>)
        }))
    }

    /// Takes `n` readings, as the desktop's timer would.
    fn read(m: &mut SystemMonitor, n: usize) {
        for _ in 0..n {
            let before = m.cpu.len();
            let now = Instant::now();
            assert_eq!(m.on_timer(now), AppEffect::None);
            assert_eq!(m.deadline(), Some(now + INTERVAL));
            let until = Instant::now() + Duration::from_secs(10);
            while m.cpu.len() == before {
                assert!(Instant::now() < until, "the sampler answers");
                std::thread::sleep(Duration::from_millis(2));
                m.poll();
            }
        }
        while m.inventory.is_none() {
            std::thread::sleep(Duration::from_millis(2));
            m.poll();
        }
    }

    #[test]
    fn the_window_wide_and_narrow() {
        let mut m = monitor();
        read(&mut m, 12);
        insta::assert_snapshot!("monitor-wide", crate::testing::render_text(&mut m, 110, 24));
        insta::assert_snapshot!(
            "monitor-narrow",
            crate::testing::render_text(&mut m, 60, 40)
        );
        let _ = m.handle_key(KeyEvent::plain(KeyCode::Char('s')));
        let text = crate::testing::render_text(&mut m, 110, 24);
        assert!(text.contains("by memory"), "{text}");
    }

    #[test]
    fn the_process_view_filters_and_asks_before_acting() {
        let mut m = monitor();
        read(&mut m, 1);
        m.procs.table = vec![
            crate::procs::tests_support::info(42, None, "compiler"),
            crate::procs::tests_support::info(43, Some(42), "linker"),
        ];
        assert_eq!(
            m.handle_key(KeyEvent::plain(KeyCode::Tab)),
            AppEffect::Redraw
        );
        assert_eq!(m.tab(), Tab::Processes);
        assert_eq!(m.reopen().as_deref(), Some("processes.open"));
        let text = crate::testing::render_text(&mut m, 100, 10);
        assert!(text.contains("Processes · 2"), "{text}");
        assert!(text.contains("compiler"), "{text}");
        for k in ['/', 'l', 'i', 'n'] {
            m.handle_key(KeyEvent::ch(k));
        }
        m.handle_key(KeyEvent::plain(KeyCode::Enter));
        let text = crate::testing::render_text(&mut m, 100, 10);
        assert!(
            text.contains("Processes · 1") && !text.contains("compiler"),
            "{text}"
        );
        // Ending asks first; `n` does nothing.
        m.handle_key(KeyEvent::ch('e'));
        let text = crate::testing::render_text(&mut m, 100, 10);
        assert!(text.contains("End linker (43)? y/n"), "{text}");
        m.handle_key(KeyEvent::ch('n'));
        assert!(m.procs.pending.is_none());
        assert!(m.procs.message.is_none());
        assert_eq!(
            m.handle_key(KeyEvent::plain(KeyCode::Tab)),
            AppEffect::Redraw
        );
        assert_eq!(m.tab(), Tab::Overview);
    }

    #[test]
    fn no_readings_until_asked_and_errors_are_shown() {
        let mut m = monitor();
        std::thread::sleep(Duration::from_millis(50));
        m.poll();
        assert!(m.last.is_none(), "nothing is read before the first timer");
        let mut broken = SystemMonitor::with_source(Box::new(|| Err("no machine here".to_owned())));
        let until = Instant::now() + Duration::from_secs(10);
        while broken.error.is_none() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
            broken.poll();
        }
        assert_eq!(broken.deadline(), None, "no timers once it failed");
        let text = crate::testing::render_text(&mut broken, 40, 3);
        assert!(text.contains("no machine here"), "{text}");
    }
}
