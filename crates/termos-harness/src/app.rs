// SPDX-License-Identifier: GPL-3.0-or-later
//! The harness window (`harness.open`, D-099, D-100).
//!
//! The goal line at the top; below, the plan's tasks on the left (state,
//! kind, title, who does or did it, time) and the selected task on the
//! right (where it goes and why, its prompt, what it did, its answer).
//! A plan waits for **Run**; until then its tasks can be edited, given
//! another kind or backend, moved or dropped. While it runs, a task or the
//! whole run can be stopped.

use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, AppWaker, tui_style};
use termos_core::config::harness::TaskKind;
use termos_core::input::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::icons::Icon;
use termos_render::theme::Theme;
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::runner::{Harness, Phase, RunView, TaskState};

/// A one-line text field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Field {
    pub text: String,
    /// The cursor, in characters.
    pub cursor: usize,
}

impl Field {
    fn with(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            cursor: text.chars().count(),
        }
    }

    fn byte(&self, at: usize) -> usize {
        self.text
            .char_indices()
            .nth(at)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn insert(&mut self, s: &str) {
        let at = self.byte(self.cursor);
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    /// A key for the field; `false` when it is not a field key.
    fn key(&mut self, k: KeyEvent) -> bool {
        let len = self.text.chars().count();
        match (k.code, k.mods) {
            (KeyCode::Char('u'), Mods::CTRL) => {
                let at = self.byte(self.cursor);
                self.text.replace_range(..at, "");
                self.cursor = 0;
            }
            (KeyCode::Char('a'), Mods::CTRL) | (KeyCode::Home, _) => self.cursor = 0,
            (KeyCode::Char('e'), Mods::CTRL) | (KeyCode::End, _) => self.cursor = len,
            (KeyCode::Char(c), m) if !m.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER) => {
                self.insert(c.encode_utf8(&mut [0; 4]));
            }
            (KeyCode::Backspace, _) if self.cursor > 0 => {
                let (a, b) = (self.byte(self.cursor - 1), self.byte(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor -= 1;
            }
            (KeyCode::Delete, _) if self.cursor < len => {
                let (a, b) = (self.byte(self.cursor), self.byte(self.cursor + 1));
                self.text.replace_range(a..b, "");
            }
            (KeyCode::Left, _) => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Right, _) => self.cursor = (self.cursor + 1).min(len),
            (KeyCode::Backspace | KeyCode::Delete, _) => {}
            _ => return false,
        }
        true
    }
}

/// Where keys go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Goal,
    Tasks,
    /// Editing the selected task's prompt.
    Prompt,
}

/// Something clickable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    Plan,
    Run,
    Edit,
    Kind,
    Backend,
    Drop,
    Up,
    Down,
    Stop,
    StopAll,
    New,
    Copy,
    /// The plan's worktree in the review window (D-113).
    Review,
    Save,
    Cancel,
    Task(usize),
}

/// The harness window.
pub struct HarnessApp {
    h: Harness,
    view: RunView,
    goal: Field,
    prompt: Field,
    focus: Focus,
    selected: usize,
    /// Lines the detail pane is scrolled down.
    scroll: u16,
    buttons: Vec<(TuiRect, Button)>,
    message: Option<(String, bool)>,
    started: Instant,
}

impl std::fmt::Debug for HarnessApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HarnessApp")
    }
}

/// `s` cut to `max` columns (with `…`).
fn fit(s: &str, max: u16) -> String {
    let max = usize::from(max);
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Word-wraps `text` to `width` columns (long words are cut).
pub fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split(' ') {
            let w = word.width();
            if !line.is_empty() && line.width() + 1 + w > width {
                out.push(std::mem::take(&mut line));
            }
            if w > width {
                let mut chunk = String::new();
                for ch in word.chars() {
                    if chunk.width() + ch.width().unwrap_or(0) > width {
                        out.push(std::mem::take(&mut chunk));
                    }
                    chunk.push(ch);
                }
                line = chunk;
                continue;
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    out
}

fn duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s / 60) % 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

impl HarnessApp {
    /// The window over `h`; `goal`, when given, is planned at once.
    pub fn open(h: Harness, goal: &str) -> Self {
        let mut app = Self {
            view: h
                .view
                .lock()
                .map(|v| v.clone())
                .unwrap_or_else(|_| RunView::new()),
            h,
            goal: Field::with(goal.trim()),
            prompt: Field::default(),
            focus: Focus::Goal,
            selected: 0,
            scroll: 0,
            buttons: Vec::new(),
            message: None,
            started: Instant::now(),
        };
        if !goal.trim().is_empty() {
            app.h.plan(goal);
            app.focus = Focus::Tasks;
        }
        app
    }

    fn reviewing(&self) -> bool {
        self.view.phase == Phase::Review
    }

    fn running(&self) -> bool {
        matches!(self.view.phase, Phase::Running | Phase::Planning(_))
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some((text.into(), error));
    }

    fn edited(&mut self, r: Result<(), String>) {
        match r {
            Ok(()) => {
                self.message = None;
                self.view = self
                    .h
                    .view
                    .lock()
                    .map(|v| v.clone())
                    .unwrap_or_else(|_| RunView::new());
                self.selected = self.selected.min(self.view.tasks.len().saturating_sub(1));
            }
            Err(e) => self.say(e, true),
        }
    }

    fn act(&mut self, b: Button) -> AppEffect {
        let i = self.selected;
        match b {
            Button::Plan => {
                let goal = self.goal.text.trim().to_owned();
                if goal.is_empty() {
                    self.focus = Focus::Goal;
                    self.say("Type what should be done, then press Enter", false);
                } else if self.running() {
                    self.say("Wait for this one, or stop it (S)", true);
                } else {
                    self.h.plan(&goal);
                    self.focus = Focus::Tasks;
                    self.selected = 0;
                    self.message = None;
                }
            }
            Button::Run if self.reviewing() => {
                self.h.run();
                self.message = None;
            }
            Button::Edit if self.reviewing() => {
                if let Some(t) = self.view.tasks.get(i) {
                    self.prompt = Field::with(&t.task.prompt.replace('\n', " ⏎ "));
                    self.focus = Focus::Prompt;
                }
            }
            Button::Save if self.focus == Focus::Prompt => {
                let text = self.prompt.text.replace(" ⏎ ", "\n");
                self.focus = Focus::Tasks;
                let r = self.h.edit(|p| {
                    if let Some(t) = p.tasks.get_mut(i) {
                        t.prompt = text;
                    }
                });
                self.edited(r);
            }
            Button::Cancel => self.focus = Focus::Tasks,
            Button::Kind if self.reviewing() => {
                let r = self.h.edit(|p| {
                    if let Some(t) = p.tasks.get_mut(i) {
                        let at = TaskKind::ALL.iter().position(|k| *k == t.kind);
                        t.kind = TaskKind::ALL[at.map_or(0, |a| (a + 1) % TaskKind::ALL.len())];
                    }
                });
                self.edited(r);
            }
            Button::Backend if self.reviewing() => {
                let names: Vec<String> = crate::route::backends(self.h.config())
                    .into_iter()
                    .map(|b| b.name)
                    .collect();
                let r = self.h.edit(|p| {
                    if let Some(t) = p.tasks.get_mut(i) {
                        // Automatic, then each backend in turn.
                        t.backend = match &t.backend {
                            None => names.first().cloned(),
                            Some(b) => names
                                .iter()
                                .position(|n| n == b)
                                .and_then(|at| names.get(at + 1).cloned()),
                        };
                    }
                });
                self.edited(r);
            }
            Button::Drop if self.reviewing() => {
                let r = self.h.edit(|p| {
                    if i < p.tasks.len() {
                        let gone = p.tasks.remove(i).id;
                        for t in &mut p.tasks {
                            t.depends_on.retain(|d| *d != gone);
                        }
                    }
                });
                self.edited(r);
            }
            Button::Up | Button::Down if self.reviewing() => {
                let j = if b == Button::Up {
                    i.checked_sub(1)
                } else {
                    (i + 1 < self.view.tasks.len()).then_some(i + 1)
                };
                if let Some(j) = j {
                    let r = self.h.edit(|p| p.tasks.swap(i, j));
                    if r.is_ok() {
                        self.selected = j;
                    }
                    self.edited(r);
                }
            }
            Button::Stop if self.view.phase == Phase::Running => {
                if let Some(t) = self.view.tasks.get(i) {
                    self.h.stop(Some(&t.task.id));
                }
            }
            Button::StopAll if self.running() => self.h.stop(None),
            Button::New if !self.running() => {
                self.h.clear();
                self.goal = Field::default();
                self.focus = Focus::Goal;
                self.selected = 0;
                self.message = None;
            }
            Button::Copy => {
                if let Some(r) = self.view.tasks.get(i).and_then(|t| t.result.clone()) {
                    return AppEffect::Copy(r);
                }
            }
            Button::Review => {
                if let Some(w) = &self.view.worktree {
                    return AppEffect::Command(format!("worktree.review {}", w.name));
                }
            }
            Button::Task(n) => {
                self.selected = n;
                self.scroll = 0;
                if self.focus == Focus::Goal {
                    self.focus = Focus::Tasks;
                }
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    /// Draws the window from `view` (the window's latest copy) as at
    /// `now`.
    fn draw(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme, now: Instant) {
        self.buttons.clear();
        let c = &theme.colors;
        let st = |fg| tui_style(Style::new().fg(fg));
        let blank = tui_style(Style::new());
        for y in area.top()..area.bottom() {
            buf.set_string(area.x, y, " ".repeat(usize::from(area.width)), blank);
        }
        if area.width < 20 || area.height < 6 {
            buf.set_string(
                area.x,
                area.y,
                fit("Harness: too small", area.width),
                st(c.muted),
            );
            return;
        }
        let right = area.right();
        // The goal line.
        let label = "Goal › ";
        let mut x = area.x + 1;
        buf.set_string(
            x,
            area.y,
            label,
            tui_style(Style::new().fg(c.accent).attrs(Attrs::BOLD)),
        );
        x += label.width() as u16;
        let plan_label = "[Plan ⏎]";
        let room = right.saturating_sub(x + plan_label.width() as u16 + 2);
        let field = if self.focus == Focus::Prompt {
            &self.prompt
        } else {
            &self.goal
        };
        let editing = matches!(self.focus, Focus::Goal | Focus::Prompt);
        self.draw_field(buf, x, area.y, room, field, editing, theme);
        if self.focus == Focus::Prompt {
            let bx = right.saturating_sub(plan_label.width() as u16 + 10);
            self.button(buf, bx, area.y, "[Save ⏎]", Button::Save, theme);
            self.button(buf, bx + 9, area.y, "[Esc]", Button::Cancel, theme);
        } else {
            let bx = right.saturating_sub(plan_label.width() as u16 + 1);
            self.button(buf, bx, area.y, plan_label, Button::Plan, theme);
        }
        // What is going on.
        let spin =
            SPINNER[(now.saturating_duration_since(self.started).as_millis() / 250 % 4) as usize];
        let (status, colour) = match &self.view.phase {
            Phase::Idle => (
                "Say what should be done; a model plans it, you approve the plan.".to_owned(),
                c.muted,
            ),
            Phase::Planning(by) if by.is_empty() => (format!("{spin} Planning…"), c.accent),
            Phase::Planning(by) => (format!("{spin} Planning with {by}…"), c.accent),
            Phase::Review => (
                format!(
                    "Plan: {} · {} tasks — change them if you like, then Run (r)",
                    if self.view.summary.is_empty() {
                        &self.view.goal
                    } else {
                        &self.view.summary
                    },
                    self.view.tasks.len()
                ),
                c.fg,
            ),
            Phase::Running => {
                let done = self
                    .view
                    .tasks
                    .iter()
                    .filter(|t| t.state.is_final())
                    .count();
                (
                    format!(
                        "{spin} Running: {done} of {} finished",
                        self.view.tasks.len()
                    ),
                    c.accent,
                )
            }
            Phase::Finished { failed: 0 } => (
                format!(
                    "✓ Done: all {} tasks finished{}",
                    self.view.tasks.len(),
                    self.result_on()
                ),
                c.accent,
            ),
            Phase::Finished { failed } => (
                format!(
                    "Finished: {failed} of {} tasks did not finish{}",
                    self.view.tasks.len(),
                    self.result_on()
                ),
                c.warn,
            ),
            Phase::Failed(why) => (why.clone(), c.error),
        };
        let t = self.view.tokens();
        let cost = if t.total() > 0 || self.view.spent > 0.0 {
            format!("{} tokens · API ${:.2}", tokens(t.total()), self.view.spent)
        } else {
            String::new()
        };
        let cost_w = cost.width() as u16;
        buf.set_string(
            area.x + 1,
            area.y + 1,
            fit(&status, area.width.saturating_sub(cost_w + 3)),
            st(colour),
        );
        if !cost.is_empty() {
            buf.set_string(
                right.saturating_sub(cost_w + 1),
                area.y + 1,
                &cost,
                st(c.muted),
            );
        }
        buf.set_string(
            area.x,
            area.y + 2,
            "─".repeat(usize::from(area.width)),
            st(c.border),
        );
        // The tasks and the selected one.
        let body = TuiRect::new(
            area.x,
            area.y + 3,
            area.width,
            area.height.saturating_sub(4),
        );
        let wide = area.width >= 80;
        let list_w = if wide {
            (area.width * 2 / 5).max(36)
        } else {
            area.width
        };
        let list = TuiRect::new(
            body.x,
            body.y,
            list_w,
            if wide {
                body.height
            } else {
                body.height.min(self.view.tasks.len() as u16 + 1)
            },
        );
        self.draw_tasks(buf, list, theme, now);
        let detail = if wide {
            for y in body.top()..body.bottom() {
                buf.set_string(body.x + list_w, y, "│", st(c.border));
            }
            TuiRect::new(
                body.x + list_w + 2,
                body.y,
                area.width.saturating_sub(list_w + 3),
                body.height,
            )
        } else {
            TuiRect::new(
                body.x + 1,
                list.bottom() + 1,
                area.width.saturating_sub(2),
                body.height.saturating_sub(list.height + 1),
            )
        };
        self.draw_detail(buf, detail, theme, now);
        self.draw_footer(
            buf,
            TuiRect::new(area.x, area.bottom() - 1, area.width, 1),
            theme,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_field(
        &self,
        buf: &mut Buffer,
        x: u16,
        y: u16,
        room: u16,
        f: &Field,
        editing: bool,
        theme: &Theme,
    ) {
        let c = &theme.colors;
        if f.text.is_empty() && !editing {
            let (shown, colour) = if self.view.goal.is_empty() {
                ("(press Tab to type a goal)", c.muted)
            } else {
                (self.view.goal.as_str(), c.fg)
            };
            buf.set_string(x, y, fit(shown, room), tui_style(Style::new().fg(colour)));
            return;
        }
        // Scroll so the cursor shows.
        let chars: Vec<char> = f.text.chars().collect();
        let room_chars = usize::from(room.max(1)) - 1;
        let start = f.cursor.saturating_sub(room_chars);
        let shown: String = chars[start..].iter().take(room_chars + 1).collect();
        buf.set_string(x, y, fit(&shown, room), tui_style(Style::new().fg(c.fg)));
        if editing {
            let before: String = chars[start..f.cursor].iter().collect();
            let cx = x + before.width() as u16;
            if cx < x + room {
                let under = chars.get(f.cursor).copied().unwrap_or(' ');
                buf.set_string(
                    cx,
                    y,
                    under.to_string(),
                    tui_style(Style::new().fg(c.fg).attrs(Attrs::REVERSE)),
                );
            }
        }
    }

    fn button(
        &mut self,
        buf: &mut Buffer,
        x: u16,
        y: u16,
        label: &str,
        b: Button,
        theme: &Theme,
    ) -> u16 {
        let w = label.width() as u16;
        buf.set_string(x, y, label, tui_style(theme.overlay_key));
        self.buttons.push((TuiRect::new(x, y, w, 1), b));
        x + w + 1
    }

    fn draw_tasks(&mut self, buf: &mut Buffer, r: TuiRect, theme: &Theme, now: Instant) {
        let c = &theme.colors;
        if self.view.tasks.is_empty() {
            if r.height > 0 {
                let hint = match self.view.phase {
                    Phase::Planning(_) => "The plan appears here.",
                    _ => "No plan yet.",
                };
                buf.set_string(
                    r.x + 1,
                    r.y,
                    fit(hint, r.width.saturating_sub(2)),
                    tui_style(Style::new().fg(c.muted)),
                );
            }
            return;
        }
        let spin =
            SPINNER[(now.saturating_duration_since(self.started).as_millis() / 250 % 4) as usize];
        // Keep the selection in view.
        let height = usize::from(r.height);
        let first = self.selected.saturating_sub(height.saturating_sub(1));
        for (row, (i, t)) in self
            .view
            .tasks
            .iter()
            .enumerate()
            .skip(first)
            .take(height)
            .enumerate()
        {
            let y = r.y + row as u16;
            let (glyph, colour) = match &t.state {
                TaskState::Waiting => ("○", c.muted),
                TaskState::Running => (spin, c.accent),
                TaskState::Done => ("✓", c.accent),
                TaskState::Failed(_) => ("✗", c.error),
                TaskState::Skipped(_) => ("–", c.muted),
                TaskState::Stopped => ("■", c.warn),
            };
            let who =
                t.by.clone()
                    .or_else(|| t.route.first().map(|ch| ch.describe()))
                    .unwrap_or_else(|| "no backend".into());
            let time = t.elapsed(now).map(duration).unwrap_or_default();
            let line = format!(
                "{} {:<3} {:<11} {}",
                glyph,
                t.task.id,
                t.task.kind.name(),
                t.task.title
            );
            let sel = i == self.selected;
            let base = if sel {
                theme.selection
            } else {
                Style::new().fg(c.fg)
            };
            buf.set_string(r.x, y, " ".repeat(usize::from(r.width)), tui_style(base));
            let left_w = r.width.saturating_sub(1);
            buf.set_string(r.x + 1, y, fit(&line, left_w), tui_style(base));
            buf.set_string(
                r.x + 1,
                y,
                glyph,
                tui_style(if sel { base } else { Style::new().fg(colour) }),
            );
            let tail = format!("{}  {}", fit(&who, 26), time);
            let tw = tail.width() as u16;
            let used = line.width() as u16 + 3;
            if used + tw < r.width {
                buf.set_string(
                    r.right().saturating_sub(tw + 1),
                    y,
                    &tail,
                    tui_style(if sel { base } else { Style::new().fg(c.muted) }),
                );
            }
            self.buttons
                .push((TuiRect::new(r.x, y, r.width, 1), Button::Task(i)));
        }
    }

    fn draw_detail(&mut self, buf: &mut Buffer, r: TuiRect, theme: &Theme, now: Instant) {
        let c = &theme.colors;
        if r.width < 10 || r.height == 0 {
            return;
        }
        let mut lines: Vec<(String, Style)> = Vec::new();
        let head = |s: &str| (s.to_owned(), Style::new().fg(c.accent).attrs(Attrs::BOLD));
        let text = Style::new().fg(c.fg);
        let muted = Style::new().fg(c.muted);
        let w = r.width;
        let push_wrapped = |lines: &mut Vec<(String, Style)>, s: &str, style: Style| {
            for l in wrap(s, w) {
                lines.push((l, style));
            }
        };
        match self.view.tasks.get(self.selected) {
            None => {
                for n in &self.view.notes {
                    push_wrapped(&mut lines, &format!("note: {n}"), Style::new().fg(c.warn));
                }
                if let Some(raw) = &self.view.raw_plan {
                    lines.push(head("The planner answered"));
                    push_wrapped(&mut lines, raw, muted);
                } else if self.view.phase == Phase::Idle {
                    for s in [
                        "Type a goal and press Enter. A planning model splits it into tasks of these kinds,",
                        "and each goes to the backend that suits it: Claude Code or Codex under your plans first,",
                        "API models by your settings or when a plan's limit is reached.",
                        "",
                        "Tasks that change files run one at a time; the others run alongside.",
                        "`termos harness status` shows where each kind goes.",
                    ] {
                        push_wrapped(&mut lines, s, muted);
                    }
                    lines.push((String::new(), muted));
                    for k in TaskKind::ALL {
                        push_wrapped(
                            &mut lines,
                            &format!("{} — {}", k.name(), k.describe()),
                            muted,
                        );
                    }
                }
            }
            Some(t) => {
                lines.push(head(&format!(
                    "{} · {} · {}",
                    t.task.id,
                    t.task.kind.name(),
                    t.task.title
                )));
                let mut facts = Vec::new();
                if !t.task.depends_on.is_empty() {
                    facts.push(format!("after {}", t.task.depends_on.join(", ")));
                }
                if t.task.kind.writes() {
                    facts.push("changes files".into());
                } else {
                    facts.push("read-only".into());
                }
                if let Some(b) = &t.task.backend {
                    facts.push(format!("you chose {b}"));
                }
                if let Some(e) = t.elapsed(now) {
                    facts.push(duration(e));
                }
                if t.tokens.total() > 0 {
                    facts.push(format!("{} tokens", tokens(t.tokens.total())));
                }
                if let Some(cost) = t.cost {
                    facts.push(format!("${cost:.4}"));
                } else if let Some(e) = t.estimate {
                    facts.push(format!("≈${e:.4} under your plan"));
                }
                push_wrapped(&mut lines, &facts.join(" · "), muted);
                match &t.state {
                    TaskState::Failed(why) => push_wrapped(
                        &mut lines,
                        &format!("Failed: {why}"),
                        Style::new().fg(c.error),
                    ),
                    TaskState::Skipped(why) => push_wrapped(
                        &mut lines,
                        &format!("Not run: {why}"),
                        Style::new().fg(c.warn),
                    ),
                    _ => {}
                }
                lines.push((String::new(), text));
                lines.push(head(match (&t.state, &t.by) {
                    (TaskState::Running, _) => "Running on",
                    (TaskState::Done, _) => "Done by",
                    (_, Some(_)) => "Tried",
                    (_, None) => "Goes to",
                }));
                let who = t.by.clone().unwrap_or_else(|| t.route.explain());
                push_wrapped(&mut lines, &who, text);
                for tried in &t.tried {
                    push_wrapped(&mut lines, &format!("✗ {tried}"), Style::new().fg(c.warn));
                }
                lines.push((String::new(), text));
                lines.push(head("Prompt"));
                push_wrapped(&mut lines, &t.task.prompt, text);
                if !t.log.is_empty() {
                    lines.push((String::new(), text));
                    lines.push(head("What it did"));
                    for l in t
                        .log
                        .iter()
                        .rev()
                        .take(40)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                    {
                        push_wrapped(&mut lines, l, muted);
                    }
                }
                if let Some(res) = &t.result {
                    lines.push((String::new(), text));
                    lines.push(head("Answer"));
                    push_wrapped(&mut lines, res, text);
                }
            }
        }
        let max_scroll = lines.len().saturating_sub(usize::from(r.height)) as u16;
        self.scroll = self.scroll.min(max_scroll);
        for (row, (l, style)) in lines
            .iter()
            .skip(usize::from(self.scroll))
            .take(usize::from(r.height))
            .enumerate()
        {
            buf.set_string(r.x, r.y + row as u16, fit(l, r.width), tui_style(*style));
        }
    }

    fn draw_footer(&mut self, buf: &mut Buffer, r: TuiRect, theme: &Theme) {
        let y = r.y;
        let mut x = r.x + 1;
        if let Some((m, error)) = self.message.clone() {
            let colour = if error {
                theme.colors.error
            } else {
                theme.colors.fg
            };
            buf.set_string(
                x,
                y,
                fit(&m, r.width.saturating_sub(2)),
                tui_style(Style::new().fg(colour)),
            );
            return;
        }
        let buttons: &[(&str, Button)] = match (&self.view.phase, self.focus) {
            (_, Focus::Prompt) => &[("Save ⏎", Button::Save), ("Cancel Esc", Button::Cancel)],
            (Phase::Review, _) => &[
                ("Run r", Button::Run),
                ("Edit e", Button::Edit),
                ("Kind t", Button::Kind),
                ("Backend b", Button::Backend),
                ("Drop d", Button::Drop),
                ("↑ K", Button::Up),
                ("↓ J", Button::Down),
                ("New n", Button::New),
            ],
            (Phase::Running | Phase::Planning(_), _) => &[
                ("Stop task s", Button::Stop),
                ("Stop all S", Button::StopAll),
            ],
            (Phase::Finished { .. }, _) if self.view.worktree.is_some() => &[
                ("Review the result R", Button::Review),
                ("New n", Button::New),
                ("Copy answer c", Button::Copy),
            ],
            (Phase::Finished { .. }, _) => {
                &[("New n", Button::New), ("Copy answer c", Button::Copy)]
            }
            (Phase::Idle | Phase::Failed(_), _) => &[("Plan ⏎", Button::Plan)],
        };
        for (label, b) in buttons {
            let text = format!("[{label}]");
            if x + text.width() as u16 >= r.right() {
                break;
            }
            x = self.button(buf, x, y, &text, *b, theme);
        }
        let hint = match self.focus {
            Focus::Goal => "Tab: tasks · q: close",
            _ => "Tab: goal · q: close",
        };
        let hw = hint.width() as u16;
        if x + hw + 1 < r.right() {
            buf.set_string(r.right() - hw - 1, y, hint, tui_style(theme.overlay_muted));
        }
    }
}

impl HarnessApp {
    /// Where the result is, for the status line: " · on termos/plan-…".
    fn result_on(&self) -> String {
        self.view
            .worktree
            .as_ref()
            .map(|w| format!(" · on {} (R reviews it)", w.branch))
            .unwrap_or_default()
    }
}

impl App for HarnessApp {
    fn title(&self) -> String {
        if self.view.goal.is_empty() {
            "Harness".into()
        } else {
            format!("Harness · {}", fit(&self.view.goal, 40))
        }
    }

    fn icon(&self) -> Icon {
        Icon::Agent
    }

    fn busy(&self) -> bool {
        self.running()
    }

    fn catch_up(&mut self) {
        if let Ok(v) = self.h.view.lock()
            && v.generation != self.view.generation
        {
            self.view = v.clone();
        }
        self.selected = self.selected.min(self.view.tasks.len().saturating_sub(1));
    }

    fn set_waker(&mut self, waker: AppWaker) {
        self.h.set_waker(waker);
    }

    fn deadline(&self) -> Option<Instant> {
        // The spinner and the clocks move only while something runs.
        self.running()
            .then(|| Instant::now() + Duration::from_millis(250))
    }

    fn on_timer(&mut self, _now: Instant) -> AppEffect {
        AppEffect::Redraw
    }

    fn handle_paste(&mut self, text: &str) -> AppEffect {
        let one_line = text.replace(['\r', '\n'], " ");
        match self.focus {
            Focus::Goal => self.goal.insert(&one_line),
            Focus::Prompt => self.prompt.insert(&one_line),
            Focus::Tasks => {
                self.focus = Focus::Goal;
                self.goal.insert(&one_line);
            }
        }
        AppEffect::Redraw
    }

    fn handle_key(&mut self, k: KeyEvent) -> AppEffect {
        self.message = None;
        match self.focus {
            Focus::Goal => match k.code {
                KeyCode::Enter => return self.act(Button::Plan),
                KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => {
                    self.focus = Focus::Tasks;
                    return AppEffect::Redraw;
                }
                _ => {
                    return if self.goal.key(k) {
                        AppEffect::Redraw
                    } else {
                        AppEffect::None
                    };
                }
            },
            Focus::Prompt => match k.code {
                KeyCode::Enter => return self.act(Button::Save),
                KeyCode::Esc => return self.act(Button::Cancel),
                _ => {
                    return if self.prompt.key(k) {
                        AppEffect::Redraw
                    } else {
                        AppEffect::None
                    };
                }
            },
            Focus::Tasks => {}
        }
        let n = self.view.tasks.len();
        match k.code {
            KeyCode::Char('q') if k.mods.is_empty() => return AppEffect::Close,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('/') => self.focus = Focus::Goal,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < n {
                    self.selected += 1;
                    self.scroll = 0;
                }
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Char('r') => return self.act(Button::Run),
            KeyCode::Char('e') => return self.act(Button::Edit),
            KeyCode::Char('t') => return self.act(Button::Kind),
            KeyCode::Char('b') => return self.act(Button::Backend),
            KeyCode::Char('d') => return self.act(Button::Drop),
            KeyCode::Char('K') => return self.act(Button::Up),
            KeyCode::Char('J') => return self.act(Button::Down),
            KeyCode::Char('s') => return self.act(Button::Stop),
            KeyCode::Char('S') => return self.act(Button::StopAll),
            KeyCode::Char('n') => return self.act(Button::New),
            KeyCode::Char('c') => return self.act(Button::Copy),
            KeyCode::Char('R') => return self.act(Button::Review),
            KeyCode::Enter if matches!(self.view.phase, Phase::Idle | Phase::Failed(_)) => {
                return self.act(Button::Plan);
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    fn handle_mouse(&mut self, ev: MouseEvent) -> AppEffect {
        match ev.kind {
            MouseKind::Down(MouseButton::Left) => {
                let hit = self
                    .buttons
                    .iter()
                    .rev()
                    .find(|(r, _)| {
                        ev.x >= r.x && ev.x < r.right() && ev.y >= r.y && ev.y < r.bottom()
                    })
                    .map(|(_, b)| *b);
                match hit {
                    Some(b) => self.act(b),
                    None if ev.y == 0 => {
                        self.focus = Focus::Goal;
                        AppEffect::Redraw
                    }
                    None => AppEffect::None,
                }
            }
            MouseKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(3);
                AppEffect::Redraw
            }
            MouseKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(3);
                AppEffect::Redraw
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.draw(area, buf, theme, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_field_edits_text() {
        let mut f = Field::default();
        for c in "héllo".chars() {
            assert!(f.key(KeyEvent::ch(c)));
        }
        f.key(KeyEvent::plain(KeyCode::Left));
        f.key(KeyEvent::plain(KeyCode::Backspace));
        assert_eq!(f.text, "hélo");
        f.key(KeyEvent::plain(KeyCode::Home));
        f.key(KeyEvent::plain(KeyCode::Delete));
        assert_eq!(f.text, "élo");
        f.key(KeyEvent::plain(KeyCode::End));
        f.insert(" world");
        f.key(KeyEvent::ctrl('u'));
        assert_eq!((f.text.as_str(), f.cursor), ("", 0));
        assert!(!f.key(KeyEvent::plain(KeyCode::Enter)));
    }

    use std::sync::Arc;

    use crate::backends::{Program, Programs};
    use crate::events::Tokens;
    use crate::plan::Plan;
    use crate::runner::Context;
    use termos_core::config::Config;
    use termos_core::config::harness::BackendKind;

    /// Claude Code is there; Codex is not.
    struct OnlyClaude;

    impl Programs for OnlyClaude {
        fn cli(&self, kind: BackendKind, _: Option<&str>) -> Result<Program, String> {
            match kind {
                BackendKind::Claude => Ok(Program {
                    path: "/opt/claude".into(),
                    env: Vec::new(),
                    unset: Vec::new(),
                    secrets: Default::default(),
                    account: None,
                }),
                _ => Err("not installed".into()),
            }
        }
    }

    fn app(dir: &std::path::Path) -> HarnessApp {
        let config = Config::parse(
            "[harness.backends.claude]\nmodels = { frontier = \"fixture-big\", fast = \"fixture-small\" }\n",
            std::path::Path::new("c.toml"),
        )
        .expect("config");
        let h = Harness::start(Context {
            config,
            programs: Arc::new(OnlyClaude),
            api: None,
            state_dir: dir.join("state"),
            data_dir: None,
            journal: None,
            project: dir.to_owned(),
        })
        .expect("start");
        let plan: Plan = serde_json::from_value(serde_json::json!({
            "goal": "add retries to the client", "summary": "Retry failed requests",
            "tasks": [
                {"id": "t1", "kind": "search", "title": "Find the HTTP calls", "prompt": "Find every place the client sends a request."},
                {"id": "t2", "kind": "coding", "title": "Add retries", "prompt": "Retry failed requests three times with backoff.", "depends_on": ["t1"]},
                {"id": "t3", "kind": "review", "title": "Review the change", "prompt": "Review the retry change for bugs.", "depends_on": ["t2"]}
            ]}))
        .expect("plan");
        h.load(plan).expect("load");
        let mut a = HarnessApp::open(h, "");
        a.catch_up();
        a.focus = Focus::Tasks;
        a
    }

    fn render(a: &mut HarnessApp) -> String {
        let area = TuiRect::new(0, 0, 100, 24);
        let mut buf = Buffer::empty(area);
        let at = a.started;
        a.draw(area, &mut buf, &Theme::default(), at);
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

    #[test]
    fn a_plan_waits_for_approval_and_can_be_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut a = app(dir.path());
        let screen = render(&mut a);
        insta::assert_snapshot!("harness-review", screen);
        assert!(
            screen.contains("Plan: Retry failed requests · 3 tasks"),
            "{screen}"
        );
        assert!(
            screen.contains("search → fast: Claude Code · fixture-small"),
            "{screen}"
        );
        // Change the second task's kind, move it, drop the review.
        a.handle_key(KeyEvent::ch('j'));
        a.handle_key(KeyEvent::ch('t'));
        a.catch_up();
        assert_eq!(a.view.tasks[1].task.kind, TaskKind::Review);
        a.handle_key(KeyEvent::ch('K'));
        a.catch_up();
        assert!(
            a.message
                .as_ref()
                .is_some_and(|(m, e)| *e && m.contains("not an earlier task")),
            "{:?}",
            a.message
        );
        a.handle_key(KeyEvent::ch('j'));
        a.handle_key(KeyEvent::ch('d'));
        a.catch_up();
        assert_eq!(a.view.tasks.len(), 2);
        // Edit a prompt.
        a.handle_key(KeyEvent::ch('k'));
        a.handle_key(KeyEvent::ch('e'));
        assert_eq!(a.focus, Focus::Prompt);
        a.handle_key(KeyEvent::ctrl('u'));
        a.handle_paste("Find the request code");
        a.handle_key(KeyEvent::plain(KeyCode::Enter));
        a.catch_up();
        assert_eq!(a.view.tasks[0].task.prompt, "Find the request code");
        // Pick a backend for it by hand, then back to automatic.
        a.handle_key(KeyEvent::ch('b'));
        a.catch_up();
        assert_eq!(a.view.tasks[0].task.backend.as_deref(), Some("claude"));
        for _ in 0..2 {
            a.handle_key(KeyEvent::ch('b'));
        }
        a.catch_up();
        assert_eq!(a.view.tasks[0].task.backend, None);
    }

    #[test]
    fn a_running_plan_shows_who_does_what() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut a = app(dir.path());
        a.view.phase = Phase::Running;
        let t = &mut a.view.tasks;
        t[0].state = TaskState::Done;
        t[0].by = Some("Claude Code · fixture-small".into());
        t[0].result = Some("The client sends requests in src/net.rs.".into());
        t[0].tokens = Tokens {
            input: 1200,
            output: 80,
            cache_read: 0,
            cache_write: 0,
        };
        t[0].log.push_back("· Grep send(".into());
        t[1].state = TaskState::Running;
        t[1].by = Some("Codex CLI (its default model)".into());
        t[1].tried.push("Claude Code: usage limit reached".into());
        a.selected = 1;
        let screen = render(&mut a);
        insta::assert_snapshot!("harness-running", screen);
        assert!(
            screen.contains("✗ Claude Code: usage limit reached"),
            "{screen}"
        );
        assert!(screen.contains("[Stop task s]"), "{screen}");
        // Clicking a task selects it.
        let y = screen
            .lines()
            .position(|l| l.contains("t1  search"))
            .expect("row");
        a.handle_mouse(MouseEvent {
            kind: MouseKind::Down(MouseButton::Left),
            x: 3,
            y: u16::try_from(y).expect("short"),
            mods: Mods::empty(),
        });
        assert_eq!(a.selected, 0);
        assert!(render(&mut a).contains("The client sends requests in src/net.rs."));
    }

    #[test]
    fn text_wraps_at_words() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\n\nb", 10), ["a", "", "b"]);
    }
}
