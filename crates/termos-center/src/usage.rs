// SPDX-License-Identifier: GPL-3.0-or-later
//! The usage app (M11a, D-096, D-097): every account of Claude Code and
//! Codex CLI on one window, with its plan's limits, its tokens over five
//! hours, today and seven days, and buttons to switch to it, open it,
//! sign in and set budgets; TermOS's own agents by provider below.
//!
//! The figures come from a [`UsageSource`] on the window's own thread,
//! asked every 30 seconds while the window is on screen. Every action is
//! a desktop command (`account.use`, `agent.external claude@work`, …), the
//! same the menu and the palette run.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::widgets::Widget;
use termos_core::app::{App, AppEffect, AppWaker};
use termos_core::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind};
use termos_core::widgets::Sparkline;
use termos_render::icons::Icon;
use termos_render::theme::Theme;

use crate::app::{Ui, dollars, tokens, width};
use crate::usage_data::{AccountRow, LiveUsage, Usage};

/// How often the window reads the logs again while on screen.
const REFRESH: Duration = Duration::from_secs(30);

/// Where the figures come from (the real logs; fixtures in tests).
pub trait UsageSource {
    /// A fresh look at `now` (Unix seconds); `today_start`: local
    /// midnight.
    fn snapshot(&mut self, now: i64, today_start: i64) -> Usage;
}

impl UsageSource for LiveUsage {
    fn snapshot(&mut self, now: i64, today_start: i64) -> Usage {
        LiveUsage::snapshot(self, now, today_start)
    }
}

/// Makes the source, on the window's own thread.
pub type MakeSource = Box<dyn FnOnce() -> Box<dyn UsageSource> + Send>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Button {
    Select(usize),
    /// A desktop command.
    Run(String),
    Refresh,
}

/// The usage window.
pub struct UsageApp {
    usage: Usage,
    loaded: bool,
    selected: usize,
    jobs: Option<Sender<()>>,
    done: Receiver<Usage>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    next: Option<Instant>,
    buttons: Vec<(TuiRect, Button)>,
    message: Option<String>,
    tz: jiff::tz::TimeZone,
    /// The clock, as Unix seconds (tests: fixed).
    clock: fn() -> i64,
}

impl std::fmt::Debug for UsageApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageApp")
            .field("accounts", &self.usage.accounts.len())
            .finish_non_exhaustive()
    }
}

/// Local midnight of `now` in `tz`.
fn day_start(now: i64, tz: &jiff::tz::TimeZone) -> i64 {
    jiff::Timestamp::from_second(now)
        .ok()
        .and_then(|t| t.to_zoned(tz.clone()).start_of_day().ok())
        .map_or(now - now.rem_euclid(86_400), |z| z.timestamp().as_second())
}

impl UsageApp {
    /// Opens the window; the source `make` makes runs on a thread of its
    /// own.
    pub fn open(make: MakeSource) -> Self {
        Self::open_with(
            make,
            jiff::tz::TimeZone::system(),
            termos_core::journal::now,
        )
    }

    /// With a time zone and a clock (tests: UTC, a fixed time).
    pub fn open_with(make: MakeSource, tz: jiff::tz::TimeZone, clock: fn() -> i64) -> Self {
        let (job_tx, job_rx) = channel::<()>();
        let (done_tx, done_rx) = channel::<Usage>();
        let waker: Arc<Mutex<Option<AppWaker>>> = Arc::new(Mutex::new(None));
        let w = Arc::clone(&waker);
        let zone = tz.clone();
        let spawned = std::thread::Builder::new()
            .name("usage".into())
            .spawn(move || {
                let mut source = make();
                while job_rx.recv().is_ok() {
                    let now = clock();
                    if done_tx
                        .send(source.snapshot(now, day_start(now, &zone)))
                        .is_err()
                    {
                        return;
                    }
                    if let Some(w) = w.lock().ok().and_then(|w| w.clone()) {
                        w.wake();
                    }
                }
            });
        let (jobs, message) = match spawned {
            Ok(_) => (Some(job_tx), None),
            Err(e) => (None, Some(format!("cannot start: {e}"))),
        };
        Self {
            usage: Usage::default(),
            loaded: false,
            selected: 0,
            jobs,
            done: done_rx,
            waker,
            next: Some(Instant::now()),
            buttons: Vec::new(),
            message,
            tz,
            clock,
        }
    }

    fn refresh(&mut self, now: Instant) {
        if let Some(tx) = &self.jobs
            && tx.send(()).is_err()
        {
            self.jobs = None;
        }
        self.next = Some(now + REFRESH);
    }

    fn take_done(&mut self) -> bool {
        let mut changed = false;
        while let Ok(u) = self.done.try_recv() {
            self.usage = u;
            self.loaded = true;
            if self.selected >= self.usage.accounts.len() {
                self.selected = self.usage.accounts.len().saturating_sub(1);
            }
            changed = true;
        }
        changed
    }

    fn account(&self) -> Option<&AccountRow> {
        self.usage.accounts.get(self.selected)
    }

    fn press(&mut self, b: Button) -> AppEffect {
        match b {
            Button::Select(i) => {
                self.selected = i;
                AppEffect::Redraw
            }
            Button::Refresh => {
                self.refresh(Instant::now());
                AppEffect::Redraw
            }
            Button::Run(cmd) => {
                if let Some(rest) = cmd.strip_prefix("account.use ") {
                    // Show it at once; the next look confirms it.
                    if let Some((cli, name)) = rest.split_once(' ') {
                        for a in &mut self.usage.accounts {
                            if a.cli == cli {
                                a.active = a.name == name;
                            }
                        }
                    }
                    self.next = Some(Instant::now() + Duration::from_secs(2));
                }
                AppEffect::Command(cmd)
            }
        }
    }

    /// The command for `what` on the selected account.
    fn act(&mut self, what: &str) -> AppEffect {
        let Some(a) = self.account() else {
            return AppEffect::None;
        };
        let (cli, name) = (a.cli, a.name.clone());
        let cmd = match what {
            "use" => format!("account.use {cli} {name}"),
            "open" => format!("agent.external {cli}@{name}"),
            "sign-in" => format!("account.sign-in {cli} {name}"),
            "budget" => format!("account.budget {cli} {name}"),
            "add" => format!("account.add {cli}"),
            _ => return AppEffect::None,
        };
        self.press(Button::Run(cmd))
    }

    fn clock_text(&self, t: i64, now: i64) -> String {
        let Ok(ts) = jiff::Timestamp::from_second(t) else {
            return String::new();
        };
        let z = ts.to_zoned(self.tz.clone());
        if (t - now).abs() < 20 * 3_600 {
            z.strftime("%H:%M").to_string()
        } else {
            z.strftime("%b %-d %H:%M").to_string()
        }
    }

    fn draw(&mut self, ui: &mut Ui<'_, Button>) {
        let area = ui.area;
        let now = (self.clock)();
        let mut y = area.y;
        let x = area.x + 1;
        ui.heading(x, y, "Usage and accounts");
        let stamp = if self.loaded {
            format!("↻ {}", self.clock_text(self.usage.taken, now))
        } else {
            "reading…".into()
        };
        let sx = area.right().saturating_sub(width(&stamp) + 10);
        let after = ui.text(sx, y, &stamp, ui.muted());
        ui.button(after + 1, y, "Refresh", Button::Refresh);
        y += 1;
        ui.hline(y);
        y += 1;
        let accounts = self.usage.accounts.clone();
        let mut last_cli = "";
        for (i, a) in accounts.iter().enumerate() {
            if y + 2 >= area.bottom() {
                break;
            }
            if a.cli != last_cli {
                if !last_cli.is_empty() {
                    y += 1;
                }
                last_cli = a.cli;
                ui.heading(x, y, a.cli_title);
                let bx = area
                    .right()
                    .saturating_sub(if a.cli == "claude" { 33 } else { 15 });
                let after = ui.button(
                    bx,
                    y,
                    "Add account",
                    Button::Run(format!("account.add {}", a.cli)),
                );
                if a.cli == "claude" && self.usage.claude_limits_off {
                    ui.button(
                        after,
                        y,
                        "Show limits",
                        Button::Run("account.statusline claude on".into()),
                    );
                }
                y += 1;
            }
            y = self.draw_account(ui, i, a, y, now);
        }
        if !self.usage.termos.is_empty() && y + 2 < area.bottom() {
            y += 1;
            ui.heading(x, y, "TermOS agents");
            y += 1;
            for p in &self.usage.termos {
                if y + 1 >= area.bottom() {
                    break;
                }
                let line = format!(
                    "{:<14} today {} · 7 days {} · {}",
                    p.provider,
                    tokens(p.today.tokens),
                    tokens(p.week.tokens),
                    dollars(p.cost_week)
                );
                ui.text(x + 2, y, &line, ui.fg(ui.theme.colors.fg));
                y += 1;
            }
        }
        if self.loaded && self.usage.accounts.is_empty() {
            ui.text(
                x,
                y,
                "No Claude Code or Codex yet: termos agent install claude",
                ui.muted(),
            );
        }
        // The footer.
        let fy = area.bottom().saturating_sub(1);
        ui.hline(fy.saturating_sub(1));
        let help = match &self.message {
            Some(m) => m.clone(),
            None => {
                "↑↓ select · u use · o open · s sign in · b budget · a add · r refresh · q close"
                    .into()
            }
        };
        ui.text(x, fy, &help, ui.muted());
    }

    /// One account: its line, its limits or budgets, its tokens.
    fn draw_account(
        &self,
        ui: &mut Ui<'_, Button>,
        i: usize,
        a: &AccountRow,
        mut y: u16,
        now: i64,
    ) -> u16 {
        let area = ui.area;
        let x = area.x + 1;
        let c = ui.theme.colors.clone();
        let pointer = if i == self.selected { "▸" } else { " " };
        let mark = if a.active { "●" } else { " " };
        let style = if i == self.selected {
            ui.fg(c.accent)
        } else {
            ui.fg(c.fg)
        };
        let mut title = a.title.clone();
        if title != a.name {
            title = format!("{title} ({})", a.name);
        }
        let plan = a
            .limits
            .as_ref()
            .and_then(|l| l.plan.clone())
            .map(|p| format!("  plan {p}"))
            .unwrap_or_default();
        let head = format!("{pointer} {mark} {title}{plan}");
        let end = ui.text(x, y, &head, style);
        ui.buttons
            .push((TuiRect::new(x, y, end - x, 1), Button::Select(i)));
        let who_x = (x + 34).max(end + 2);
        ui.text(who_x, y, &a.who, ui.muted());
        // Buttons at the right.
        let labels: Vec<(&str, String)> = [
            (!a.active).then(|| ("Use", format!("account.use {} {}", a.cli, a.name))),
            Some(("Open", format!("agent.external {}@{}", a.cli, a.name))),
            Some(("Sign in", format!("account.sign-in {} {}", a.cli, a.name))),
            (a.cli == "claude").then(|| ("Budget", format!("account.budget {} {}", a.cli, a.name))),
        ]
        .into_iter()
        .flatten()
        .collect();
        let total: u16 = labels.iter().map(|(l, _)| width(l) + 3).sum();
        let mut bx = area.right().saturating_sub(total + 1);
        for (label, cmd) in labels {
            bx = ui.button(bx, y, label, Button::Run(cmd));
        }
        y += 1;
        // The plan's own limits, else the budgets.
        let meter_w = area.width.saturating_sub(6).min(60);
        let mut gauges = 0;
        if let Some(l) = &a.limits {
            for w in &l.windows {
                let fresh = w.resets_at.is_none_or(|r| r > now);
                if !fresh || y + 1 >= area.bottom() {
                    continue;
                }
                let label = match w.minutes {
                    300 => "5 hours",
                    10_080 => "week",
                    m if m < 1_440 => "short",
                    _ => "long",
                };
                let mut value = format!("{:.0}%", w.used_percent);
                if let Some(r) = w.resets_at {
                    value.push_str(&format!(" · resets {}", self.clock_text(r, now)));
                }
                ui.meter(
                    TuiRect::new(x + 4, y, meter_w, 1),
                    label,
                    f64::from(w.used_percent / 100.0).clamp(0.0, 1.0),
                    &value,
                );
                gauges += 1;
                y += 1;
            }
        }
        if gauges == 0 {
            if let Some(w) = a.window
                && y + 1 < area.bottom()
            {
                let resets = self.clock_text(w.resets, now);
                match a.budget_5h {
                    Some(b) => ui.meter(
                        TuiRect::new(x + 4, y, meter_w, 1),
                        "5 hours",
                        (w.tokens as f64 / b as f64).clamp(0.0, 1.0),
                        &format!("{} of {} · resets {resets}", tokens(w.tokens), tokens(b)),
                    ),
                    None => {
                        ui.text(
                            x + 4,
                            y,
                            &format!("5 hours  {} tokens · resets {resets}", tokens(w.tokens)),
                            ui.fg(c.fg),
                        );
                    }
                }
                y += 1;
            }
            if let Some(b) = a.budget_week
                && y + 1 < area.bottom()
            {
                ui.meter(
                    TuiRect::new(x + 4, y, meter_w, 1),
                    "week",
                    (a.week.tokens as f64 / b as f64).clamp(0.0, 1.0),
                    &format!("{} of {}", tokens(a.week.tokens), tokens(b)),
                );
                y += 1;
            }
        }
        if y + 1 < area.bottom() {
            let mut line = format!(
                "today {} · 7 days {}",
                tokens(a.today.tokens),
                tokens(a.week.tokens)
            );
            if a.week.cache_read > 0 {
                line.push_str(&format!(" · cache reads {}", tokens(a.week.cache_read)));
            }
            if let Some((m, n)) = a.models.first()
                && a.week.tokens > 0
            {
                line.push_str(&format!(" · {m} {}%", n * 100 / a.week.tokens.max(1)));
            }
            let end = ui.text(x + 4, y, &line, ui.muted());
            // Seven days, as a sparkline.
            let spark_x = end + 2;
            if a.daily.iter().any(|d| *d > 0) && spark_x + 8 < area.right() {
                let data: Vec<f64> = a.daily.iter().map(|d| *d as f64).collect();
                Sparkline {
                    data: &data,
                    max: None,
                    style: termos_core::app::tui_style(ui.fg(c.accent)),
                }
                .render(TuiRect::new(spark_x, y, 7, 1), ui.buf);
            }
            y += 1;
        }
        y
    }
}

impl App for UsageApp {
    fn title(&self) -> String {
        "Usage and accounts".into()
    }

    fn icon(&self) -> Icon {
        Icon::Agent
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
        self.refresh(now);
        AppEffect::None
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        self.message = None;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                AppEffect::Redraw
            }
            KeyCode::Down => {
                if self.selected + 1 < self.usage.accounts.len() {
                    self.selected += 1;
                }
                AppEffect::Redraw
            }
            KeyCode::Char('u') | KeyCode::Enter => self.act("use"),
            KeyCode::Char('o') => self.act("open"),
            KeyCode::Char('s') => self.act("sign-in"),
            KeyCode::Char('b') => self.act("budget"),
            KeyCode::Char('a') => self.act("add"),
            KeyCode::Char('r') => self.press(Button::Refresh),
            _ => AppEffect::None,
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        if let MouseKind::Down(MouseButton::Left) = event.kind {
            let hit = self
                .buttons
                .iter()
                .rev()
                .find(|(r, _)| {
                    event.x >= r.x && event.x < r.right() && event.y >= r.y && event.y < r.bottom()
                })
                .map(|(_, b)| b.clone());
            if let Some(b) = hit {
                self.message = None;
                return self.press(b);
            }
        }
        AppEffect::None
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.take_done();
        if area.height < 5 || area.width < 30 {
            return;
        }
        let mut buttons = Vec::new();
        {
            let mut ui = Ui {
                buf,
                theme,
                buttons: &mut buttons,
                area,
            };
            self.draw(&mut ui);
        }
        self.buttons = buttons;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_data::{ProviderRow, Use, Window};
    use termos_core::input::Mods;
    use termos_devkit::accounts::{LimitWindow, Limits};
    use unicode_width::UnicodeWidthStr;

    const NOW: i64 = 1_793_707_200; // 2026-11-03T12:00:00Z

    fn fixed() -> i64 {
        NOW
    }

    fn row(cli: &'static str, name: &str, title: &str, active: bool) -> AccountRow {
        AccountRow {
            cli,
            cli_title: if cli == "claude" {
                "Claude Code"
            } else {
                "Codex CLI"
            },
            name: name.into(),
            title: title.into(),
            active,
            who: String::new(),
            window: None,
            today: Use::default(),
            week: Use::default(),
            models: Vec::new(),
            daily: vec![0; 7],
            limits: None,
            budget_5h: None,
            budget_week: None,
        }
    }

    fn fixture() -> Usage {
        let mut work = row("claude", "work", "Work (Max)", true);
        work.who = "signed in as fixture@example.com".into();
        work.limits = Some(Limits {
            time: NOW - 60,
            plan: None,
            windows: vec![
                LimitWindow {
                    minutes: 300,
                    used_percent: 42.0,
                    resets_at: Some(NOW + 2 * 3_600),
                },
                LimitWindow {
                    minutes: 10_080,
                    used_percent: 10.0,
                    resets_at: Some(NOW + 3 * 86_400),
                },
            ],
        });
        work.today = Use {
            tokens: 1_200_000,
            cache_read: 0,
            turns: 30,
        };
        work.week = Use {
            tokens: 8_400_000,
            cache_read: 120_000_000,
            turns: 400,
        };
        work.models = vec![("fixture-model".into(), 6_720_000)];
        work.daily = vec![1, 2, 3, 0, 5, 8, 4];
        let mut default = row("claude", "default", "default", false);
        default.window = Some(Window {
            start: NOW - 3_600,
            resets: NOW + 4 * 3_600,
            tokens: 220_000,
        });
        default.budget_5h = Some(2_000_000);
        default.week = Use {
            tokens: 2_100_000,
            cache_read: 0,
            turns: 50,
        };
        let mut codex = row("codex", "default", "default", true);
        codex.who = "signed in".into();
        codex.limits = Some(Limits {
            time: NOW - 60,
            plan: Some("pro".into()),
            windows: vec![LimitWindow {
                minutes: 300,
                used_percent: 40.0,
                resets_at: Some(NOW + 3_600),
            }],
        });
        Usage {
            taken: NOW,
            accounts: vec![work, default, codex],
            termos: vec![ProviderRow {
                provider: "anthropic".into(),
                today: Use {
                    tokens: 20_000,
                    cache_read: 0,
                    turns: 2,
                },
                week: Use {
                    tokens: 400_000,
                    cache_read: 0,
                    turns: 20,
                },
                cost_week: Some(3.2),
            }],
            claude_limits_off: false,
        }
    }

    struct Fixture(Usage);
    impl UsageSource for Fixture {
        fn snapshot(&mut self, _now: i64, _today: i64) -> Usage {
            self.0.clone()
        }
    }

    fn app(u: Usage) -> UsageApp {
        let mut app = UsageApp::open_with(
            Box::new(move || Box::new(Fixture(u)) as Box<dyn UsageSource>),
            jiff::tz::TimeZone::UTC,
            fixed,
        );
        app.on_timer(Instant::now());
        let start = Instant::now();
        while !app.loaded {
            assert!(start.elapsed() < Duration::from_secs(5), "no answer");
            std::thread::sleep(Duration::from_millis(5));
            app.catch_up();
        }
        app
    }

    fn render(app: &mut UsageApp) -> String {
        let area = TuiRect::new(0, 0, 100, 24);
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

    fn click(app: &mut UsageApp, screen: &str, line_has: &str, label: &str) -> AppEffect {
        let (x, y) = screen
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains(line_has))
            .find_map(|(y, l)| {
                l.find(label).map(|i| {
                    (
                        u16::try_from(UnicodeWidthStr::width(&l[..i])).expect("narrow"),
                        u16::try_from(y).expect("short"),
                    )
                })
            })
            .unwrap_or_else(|| panic!("{label:?} not on a line with {line_has:?}:\n{screen}"));
        app.handle_mouse(MouseEvent {
            kind: MouseKind::Down(MouseButton::Left),
            x: x + 1,
            y,
            mods: Mods::empty(),
        })
    }

    #[test]
    fn every_account_with_its_limits_and_budgets() {
        let mut a = app(fixture());
        let screen = render(&mut a);
        insta::assert_snapshot!("usage-window", screen);
        assert!(screen.contains("42% · resets 14:00"), "{screen}");
        assert!(screen.contains("220.0k of 2.0M · resets 16:00"), "{screen}");
        assert!(screen.contains("plan pro"), "{screen}");
        assert!(screen.contains("anthropic"), "{screen}");
    }

    #[test]
    fn buttons_and_keys_run_desktop_commands() {
        let mut a = app(fixture());
        let screen = render(&mut a);
        assert_eq!(
            click(&mut a, &screen, "▸ ● Work", "[Open]"),
            AppEffect::Command("agent.external claude@work".into())
        );
        assert_eq!(
            click(&mut a, &screen, "  default", "[Use]"),
            AppEffect::Command("account.use claude default".into())
        );
        // Switched at once on screen.
        assert!(a.usage.accounts[1].active && !a.usage.accounts[0].active);
        let screen = render(&mut a);
        assert!(!screen.lines().any(|l| l.contains("● Work")), "{screen}");
        let key = |c: KeyCode| KeyEvent {
            code: c,
            mods: Mods::empty(),
        };
        assert_eq!(a.handle_key(key(KeyCode::Down)), AppEffect::Redraw);
        assert_eq!(
            a.handle_key(key(KeyCode::Char('s'))),
            AppEffect::Command("account.sign-in claude default".into())
        );
        assert_eq!(
            a.handle_key(key(KeyCode::Char('b'))),
            AppEffect::Command("account.budget claude default".into())
        );
        a.handle_key(key(KeyCode::Down));
        assert_eq!(
            a.handle_key(key(KeyCode::Char('a'))),
            AppEffect::Command("account.add codex".into())
        );
        assert_eq!(a.handle_key(key(KeyCode::Char('q'))), AppEffect::Close);
        // Limits not recorded: the button that turns them on.
        let mut off = fixture();
        off.claude_limits_off = true;
        let mut b = app(off);
        let screen = render(&mut b);
        assert_eq!(
            click(&mut b, &screen, "Claude Code", "[Show limits]"),
            AppEffect::Command("account.statusline claude on".into())
        );
        // Stale limits (past their reset) are not shown as current.
        let mut stale = fixture();
        if let Some(l) = &mut stale.accounts[0].limits {
            for w in &mut l.windows {
                w.resets_at = Some(NOW - 1);
            }
        }
        let screen = render(&mut app(stale));
        assert!(!screen.contains("42%"), "{screen}");
    }
}
