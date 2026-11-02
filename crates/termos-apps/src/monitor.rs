// SPDX-License-Identifier: GPL-3.0-or-later
//! The system monitor: CPU, memory, GPUs, temperatures, disks, network
//! and the busiest processes, from `termos-hw`.
//!
//! Readings are taken on the monitor's own thread, once a second, and
//! only while the window is on screen: the desktop gives an app timers
//! only when some client can see it (D-028), so a minimized or
//! background monitor costs nothing.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::style::Style as TuiStyle;
use ratatui_core::widgets::Widget;
use termos_core::app::{App, AppEffect, AppWaker, tui_style};
use termos_core::input::{KeyCode, KeyEvent};
use termos_core::widgets::{LineChart, Meter, Series, Sparkline};
use termos_hw::{Inventory, ProcessSample, Sample, Source};
use termos_render::cell::{Attrs, Style};
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
    Failed(String),
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
    ask: Option<Sender<()>>,
    results: Receiver<Msg>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    next: Option<Instant>,
    sort: ProcSort,
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
        let (ask_tx, ask_rx) = channel::<()>();
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
                while ask_rx.recv().is_ok() {
                    if tx.send(Msg::Sample(Box::new(source.sample()))).is_err() {
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
            results: rx,
            waker,
            // The first reading as soon as the window is on screen.
            next: Some(Instant::now()),
            sort: ProcSort::Cpu,
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
                }
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
fn level_style(theme: &Theme, r: f64) -> TuiStyle {
    let c = &theme.colors;
    let fg = if r > 0.9 {
        c.error
    } else if r > 0.75 {
        c.warn
    } else {
        c.accent
    };
    tui_style(Style::new().fg(fg))
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
        Meter {
            ratio: r,
            label,
            value,
            label_width: label_w,
            fill: level_style(self.theme, r),
            empty: tui_style(Style::new().fg(c.border)),
            text: tui_style(Style::new().fg(c.fg)),
        }
        .render(area, self.buf);
    }
}

impl SystemMonitor {
    /// CPU: a chart of the total and a meter per core. Returns rows used.
    fn draw_cpu(&self, pen: &mut Pen<'_>, area: TuiRect, s: &Sample) -> u16 {
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
        LineChart {
            series: &series,
            max: Some(100.0),
            labels: Some(("100", "0")),
            label_style: tui_style(Style::new().fg(c.muted)),
        }
        .render(
            TuiRect::new(area.x, area.y + 1, area.width, chart_h),
            pen.buf,
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
        y - area.y
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
    fn title(&self) -> String {
        "System monitor".into()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
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
        y += self.draw_cpu(&mut pen, cpu_area, &s) + 1;
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
        if ask.send(()).is_err() {
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
