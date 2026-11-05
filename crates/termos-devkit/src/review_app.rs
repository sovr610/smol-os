// SPDX-License-Identifier: GPL-3.0-or-later
//! A worktree's review window (`worktree.review`, M12a, D-114).
//!
//! The files the worktree changed against its base, each file's diff,
//! and comments on lines that go to the worktree's agent as its next
//! message. From here it is committed, merged, opened as a pull request,
//! archived or discarded: each runs `termos worktree …` in a window of
//! its own, where git's editor, hooks and questions work as in any
//! terminal.
//!
//! Its git work runs on threads; while the window is on screen it looks
//! again every few seconds, so the agent's edits show as they land.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, AppWaker, tui_style};
use termos_core::input::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::color::Color;
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

use crate::worktrees::{self, Change, Changes, DiffLine, LineKind, Worktree};

/// How often the window looks for new changes while it is on screen.
const REFRESH: Duration = Duration::from_secs(3);

/// Where the git work comes from (the worktree itself, or a fixture).
pub trait Source: Send + Sync {
    fn changes(&self) -> Result<Changes, String>;
    fn diff(&self, change: &Change, merge_base: &str) -> Result<Vec<DiffLine>, String>;
}

/// The worktree on disk.
pub struct Live(pub Worktree);

impl Source for Live {
    fn changes(&self) -> Result<Changes, String> {
        worktrees::changes(&self.0).map_err(|e| e.to_string())
    }
    fn diff(&self, change: &Change, merge_base: &str) -> Result<Vec<DiffLine>, String> {
        worktrees::file_diff(&self.0, change, merge_base).map_err(|e| e.to_string())
    }
}

/// A comment on a line (or a whole file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub path: String,
    /// "line 12", "removed line 7"; empty for the file.
    pub at: String,
    /// The line it is about.
    pub quote: String,
    pub text: String,
}

enum Done {
    Changes(Result<Changes, String>),
    Diff(String, Result<Vec<DiffLine>, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Files,
    Diff,
}

/// Buttons along the top: label, action key.
const BUTTONS: [(&str, char); 7] = [
    ("Send to agent", 's'),
    ("Commit", 'C'),
    ("Merge", 'm'),
    ("Pull request", 'p'),
    ("Archive", 'a'),
    ("Discard", 'd'),
    ("Open", 'o'),
];

pub struct ReviewApp {
    wt: Worktree,
    source: Arc<dyn Source>,
    changes: Option<Result<Changes, String>>,
    selected: usize,
    file_top: usize,
    /// The selected file's diff (its path, its lines).
    diff: Option<(String, Result<Vec<DiffLine>, String>)>,
    cursor: usize,
    diff_top: usize,
    focus: Focus,
    comments: Vec<Comment>,
    /// A comment being typed.
    typing: Option<String>,
    message: Option<String>,
    inbox: Arc<Mutex<Vec<Done>>>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    pending: usize,
    inline: bool,
    next_look: Option<Instant>,
    /// Where each button was drawn: (x, width).
    button_spans: Vec<(u16, u16)>,
    /// Rows of the last drawing: the list's top, its height.
    body: (u16, u16),
    files_width: u16,
}

impl std::fmt::Debug for ReviewApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewApp")
            .field("worktree", &self.wt.name)
            .finish_non_exhaustive()
    }
}

fn width(text: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX)
}

/// `text` cut to `max` columns.
fn fit(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max {
            break;
        }
        w += cw;
        out.push(if c == '\t' { ' ' } else { c });
    }
    out
}

impl ReviewApp {
    pub fn open(wt: Worktree) -> Self {
        let source: Arc<dyn Source> = Arc::new(Live(wt.clone()));
        Self::with(wt, source, false)
    }

    /// Over `source`; `inline` does the work at once (tests).
    pub fn with(wt: Worktree, source: Arc<dyn Source>, inline: bool) -> Self {
        let mut app = Self {
            wt,
            source,
            changes: None,
            selected: 0,
            file_top: 0,
            diff: None,
            cursor: 0,
            diff_top: 0,
            focus: Focus::Files,
            comments: Vec::new(),
            typing: None,
            message: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
            waker: Arc::new(Mutex::new(None)),
            pending: 0,
            inline,
            next_look: None,
            button_spans: Vec::new(),
            body: (0, 0),
            files_width: 0,
        };
        app.look();
        app
    }

    fn run(&mut self, work: impl FnOnce() -> Done + Send + 'static) {
        self.pending += 1;
        if self.inline {
            let d = work();
            self.apply(d);
            return;
        }
        let inbox = Arc::clone(&self.inbox);
        let waker = Arc::clone(&self.waker);
        std::thread::spawn(move || {
            let d = work();
            if let Ok(mut q) = inbox.lock() {
                q.push(d);
            }
            if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                w.wake();
            }
        });
    }

    /// Lists the changes again.
    fn look(&mut self) {
        let source = Arc::clone(&self.source);
        self.next_look = Some(Instant::now() + REFRESH);
        self.run(move || Done::Changes(source.changes()));
    }

    fn files(&self) -> &[Change] {
        match &self.changes {
            Some(Ok(c)) => &c.files,
            _ => &[],
        }
    }

    fn load_diff(&mut self) {
        let Some(change) = self.files().get(self.selected).cloned() else {
            self.diff = None;
            return;
        };
        let mb = match &self.changes {
            Some(Ok(c)) => c.merge_base.clone(),
            _ => return,
        };
        let source = Arc::clone(&self.source);
        self.run(move || {
            let d = source.diff(&change, &mb);
            Done::Diff(change.path, d)
        });
    }

    fn apply(&mut self, done: Done) {
        self.pending = self.pending.saturating_sub(1);
        match done {
            Done::Changes(c) => {
                let before: Option<String> =
                    self.files().get(self.selected).map(|f| f.path.clone());
                let same = matches!((&self.changes, &c), (Some(Ok(a)), Ok(b)) if a == b);
                self.changes = Some(c);
                // Keep the same file selected when it is still there.
                if let Some(p) = before
                    && let Some(i) = self.files().iter().position(|f| f.path == p)
                {
                    self.selected = i;
                }
                self.selected = self.selected.min(self.files().len().saturating_sub(1));
                if !same || self.diff.is_none() {
                    self.load_diff();
                }
            }
            Done::Diff(path, d) => {
                let current = self.files().get(self.selected).map(|f| f.path.as_str());
                if current == Some(path.as_str()) {
                    let changed = self.diff.as_ref().is_none_or(|(p, _)| *p != path);
                    self.diff = Some((path, d));
                    if changed {
                        self.cursor = 0;
                        self.diff_top = 0;
                    }
                    let n = self.diff_lines().len();
                    self.cursor = self.cursor.min(n.saturating_sub(1));
                }
            }
        }
    }

    fn diff_lines(&self) -> &[DiffLine] {
        match &self.diff {
            Some((_, Ok(l))) => l,
            _ => &[],
        }
    }

    fn select_file(&mut self, i: usize) -> AppEffect {
        let n = self.files().len();
        if n == 0 {
            return AppEffect::None;
        }
        let i = i.min(n - 1);
        if i != self.selected || self.diff.is_none() {
            self.selected = i;
            self.diff = None;
            self.load_diff();
        }
        AppEffect::Redraw
    }

    fn move_cursor(&mut self, to: usize) -> AppEffect {
        let n = self.diff_lines().len();
        if n == 0 {
            return AppEffect::None;
        }
        self.cursor = to.min(n - 1);
        AppEffect::Redraw
    }

    /// Where the cursor's line is, for a comment.
    fn here(&self) -> Option<Comment> {
        let f = self.files().get(self.selected)?;
        if self.focus == Focus::Files {
            return Some(Comment {
                path: f.path.clone(),
                at: String::new(),
                quote: String::new(),
                text: String::new(),
            });
        }
        let l = self.diff_lines().get(self.cursor)?;
        let at = match (l.kind, l.old, l.new) {
            (LineKind::Del, Some(o), _) => format!("removed line {o}"),
            (_, _, Some(n)) => format!("line {n}"),
            _ => String::new(),
        };
        let quote = match l.kind {
            LineKind::Add | LineKind::Del | LineKind::Context => l.text.trim().to_owned(),
            _ => String::new(),
        };
        Some(Comment {
            path: f.path.clone(),
            at,
            quote,
            text: String::new(),
        })
    }

    /// The comments as one message for the agent.
    pub fn message_for_agent(&self) -> String {
        let mut m = format!(
            "Review comments on your changes in this worktree (branch {}):\n\n",
            self.wt.branch
        );
        for c in &self.comments {
            let mut place = c.path.clone();
            if !c.at.is_empty() {
                place.push_str(&format!(", {}", c.at));
            }
            if !c.quote.is_empty() {
                place.push_str(&format!(" (`{}`)", fit(&c.quote, 80)));
            }
            m.push_str(&format!("- {place}: {}\n", c.text));
        }
        m.push_str("\nPlease address them, then say what you changed.");
        m
    }

    pub fn comments(&self) -> &[Comment] {
        &self.comments
    }

    /// `termos worktree VERB NAME` in a window.
    fn run_verb(&self, verb: &str) -> AppEffect {
        AppEffect::Command(format!("worktree.run {verb} {}", self.wt.name))
    }

    fn action(&mut self, key: char) -> AppEffect {
        match key {
            's' => {
                if self.comments.is_empty() {
                    self.message = Some("No comments yet: c on a line adds one".into());
                    return AppEffect::Redraw;
                }
                let text = self.message_for_agent();
                let n = self.comments.len();
                self.comments.clear();
                self.message = Some(format!(
                    "Sent {n} comment{} to the agent",
                    if n == 1 { "" } else { "s" }
                ));
                AppEffect::TellAgent {
                    worktree: self.wt.name.clone(),
                    dir: self.wt.path.clone(),
                    text,
                }
            }
            'C' => self.run_verb("commit"),
            'm' => self.run_verb("merge"),
            'p' => self.run_verb("pr"),
            'a' => self.run_verb("archive"),
            'd' => self.run_verb("discard"),
            'o' => AppEffect::Command(format!("worktree.open {}", self.wt.name)),
            _ => AppEffect::None,
        }
    }

    fn summary(&self) -> String {
        match &self.changes {
            None => "looking…".to_owned(),
            Some(Err(e)) => e.clone(),
            Some(Ok(c)) => {
                let mut parts = vec![format!(
                    "{} commit{}",
                    c.ahead,
                    if c.ahead == 1 { "" } else { "s" }
                )];
                if c.uncommitted {
                    parts.push("changes not committed".into());
                }
                if c.behind > 0 {
                    parts.push(format!("{} behind", c.behind));
                }
                parts.push(format!("+{} -{}", c.added(), c.removed()));
                parts.join(", ")
            }
        }
    }
}

impl App for ReviewApp {
    fn title(&self) -> String {
        format!("Review: {}", self.wt.name)
    }

    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Git
    }

    fn reopen(&self) -> Option<String> {
        Some(format!("worktree.review {}", self.wt.name))
    }

    fn busy(&self) -> bool {
        self.pending > 0
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut slot) = self.waker.lock() {
            *slot = Some(waker.clone());
        }
        if self.inbox.lock().is_ok_and(|q| !q.is_empty()) {
            waker.wake();
        }
    }

    fn catch_up(&mut self) {
        let done: Vec<Done> = self
            .inbox
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for d in done {
            self.apply(d);
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.next_look
    }

    fn on_timer(&mut self, now: Instant) -> AppEffect {
        if self.next_look.is_some_and(|t| t <= now) {
            if self.pending == 0 {
                self.look();
            } else {
                self.next_look = Some(now + REFRESH);
            }
        }
        AppEffect::None
    }

    fn handle_paste(&mut self, text: &str) -> AppEffect {
        if let Some(t) = &mut self.typing {
            t.push_str(&text.replace(['\r', '\n'], " "));
            return AppEffect::Redraw;
        }
        AppEffect::None
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        if let Some(t) = &mut self.typing {
            match key.code {
                KeyCode::Esc => self.typing = None,
                KeyCode::Enter => {
                    let text = t.trim().to_owned();
                    self.typing = None;
                    if !text.is_empty()
                        && let Some(mut c) = self.here()
                    {
                        c.text = text;
                        self.comments.push(c);
                    }
                }
                KeyCode::Backspace => {
                    t.pop();
                }
                KeyCode::Char(ch) if !key.mods.contains(Mods::CTRL) => t.push(ch),
                _ => return AppEffect::None,
            }
            return AppEffect::Redraw;
        }
        let had = self.message.take().is_some();
        let page = usize::from(self.body.1.max(2) - 1);
        let effect = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => AppEffect::Close,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Files => Focus::Diff,
                    Focus::Diff => Focus::Files,
                };
                AppEffect::Redraw
            }
            KeyCode::Right | KeyCode::Enter if self.focus == Focus::Files => {
                self.focus = Focus::Diff;
                AppEffect::Redraw
            }
            KeyCode::Left if self.focus == Focus::Diff => {
                self.focus = Focus::Files;
                AppEffect::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Files => self.select_file(self.selected + 1),
                Focus::Diff => self.move_cursor(self.cursor + 1),
            },
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Files => self.select_file(self.selected.saturating_sub(1)),
                Focus::Diff => self.move_cursor(self.cursor.saturating_sub(1)),
            },
            KeyCode::PageDown | KeyCode::Char(' ') => self.move_cursor(self.cursor + page),
            KeyCode::PageUp => self.move_cursor(self.cursor.saturating_sub(page)),
            KeyCode::Char('n') => self.select_file(self.selected + 1),
            KeyCode::Char('N') => self.select_file(self.selected.saturating_sub(1)),
            KeyCode::Char('c') => {
                if self.here().is_some() {
                    self.typing = Some(String::new());
                    AppEffect::Redraw
                } else {
                    AppEffect::None
                }
            }
            KeyCode::Char('u') => {
                if self.comments.pop().is_some() {
                    AppEffect::Redraw
                } else {
                    AppEffect::None
                }
            }
            KeyCode::Char('r') => {
                self.look();
                AppEffect::Redraw
            }
            KeyCode::Char(ch) => self.action(ch),
            _ => AppEffect::None,
        };
        match effect {
            AppEffect::None if had => AppEffect::Redraw,
            e => e,
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        let (top, height) = self.body;
        match event.kind {
            MouseKind::Down(MouseButton::Left) if event.y == 1 => {
                for (i, (x, w)) in self.button_spans.iter().enumerate() {
                    if event.x >= *x && event.x < x + w {
                        return self.action(BUTTONS[i].1);
                    }
                }
                AppEffect::None
            }
            MouseKind::Down(MouseButton::Left) if event.y >= top && event.y < top + height => {
                let row = usize::from(event.y - top);
                if event.x < self.files_width {
                    self.focus = Focus::Files;
                    self.select_file(self.file_top + row)
                } else {
                    self.focus = Focus::Diff;
                    self.move_cursor(self.diff_top + row)
                }
            }
            MouseKind::ScrollDown if event.x >= self.files_width => {
                self.move_cursor(self.cursor + 3)
            }
            MouseKind::ScrollUp if event.x >= self.files_width => {
                self.move_cursor(self.cursor.saturating_sub(3))
            }
            MouseKind::ScrollDown => self.select_file(self.selected + 1),
            MouseKind::ScrollUp => self.select_file(self.selected.saturating_sub(1)),
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        if area.height < 8 || area.width < 30 {
            return;
        }
        let c = &theme.colors;
        let w = usize::from(area.width - 2);
        let put = |buf: &mut Buffer, x: u16, y: u16, text: &str, max: usize, style: Style| {
            buf.set_stringn(x, y, fit(text, max), max, tui_style(style));
        };
        // The worktree and where it stands.
        let mut head = format!("{}  {} → {}", self.wt.name, self.wt.branch, self.wt.base);
        if self.wt.ports.is_some() {
            head.push_str(&format!("  ports {}", self.wt.ports_text()));
        }
        put(
            buf,
            area.x + 1,
            area.y,
            &head,
            w,
            Style::new().fg(c.fg).attrs(Attrs::BOLD),
        );
        let summary = self.summary();
        let sx = area.x + 1 + width(&fit(&head, w)) + 3;
        if sx < area.right() {
            put(
                buf,
                sx,
                area.y,
                &summary,
                usize::from(area.right() - sx),
                Style::new().fg(c.muted),
            );
        }
        // The buttons.
        self.button_spans.clear();
        let mut x = area.x + 1;
        for (label, _) in BUTTONS {
            let label = if label == "Send to agent" && !self.comments.is_empty() {
                format!("[Send to agent ({})]", self.comments.len())
            } else {
                format!("[{label}]")
            };
            let bw = width(&label);
            if x + bw > area.right() {
                break;
            }
            put(
                buf,
                x,
                area.y + 1,
                &label,
                usize::from(bw),
                Style::new().fg(c.accent),
            );
            self.button_spans.push((x - area.x, bw));
            x += bw + 1;
        }
        if let Some(pr) = &self.wt.pr
            && x + 2 < area.right()
        {
            put(
                buf,
                x + 1,
                area.y + 1,
                pr,
                usize::from(area.right() - x - 1),
                Style::new().fg(c.muted),
            );
        }
        // Files on the left, the diff on the right.
        let top = area.y + 2;
        let height = area.height - 4;
        self.body = (top - area.y, height);
        let files_w = (area.width / 3).clamp(16, 40);
        self.files_width = files_w;
        let rows = usize::from(height);
        if self.selected < self.file_top {
            self.file_top = self.selected;
        } else if self.selected >= self.file_top + rows {
            self.file_top = self.selected + 1 - rows;
        }
        let files = self.files().to_vec();
        if files.is_empty() {
            let text = match &self.changes {
                Some(Ok(_)) => "No changes yet",
                Some(Err(_)) => "Cannot read it",
                None => "Looking…",
            };
            put(
                buf,
                area.x + 1,
                top,
                text,
                usize::from(files_w - 2),
                Style::new().fg(c.muted),
            );
        }
        for (row, (i, f)) in files
            .iter()
            .enumerate()
            .skip(self.file_top)
            .take(rows)
            .enumerate()
        {
            let y = top + u16::try_from(row).unwrap_or(0);
            let counts = match (f.added, f.removed) {
                (Some(a), Some(r)) => format!("+{a} -{r}"),
                _ => "bin".to_owned(),
            };
            let name_w = usize::from(files_w).saturating_sub(4 + counts.len());
            let name = std::path::Path::new(&f.path)
                .file_name()
                .map_or(f.path.clone(), |n| n.to_string_lossy().into_owned());
            let shown = if f.path.len() <= name_w {
                f.path.clone()
            } else {
                name
            };
            let line = format!(
                "{} {:<nw$} {counts}",
                f.status,
                fit(&shown, name_w),
                nw = name_w
            );
            let mut style = Style::new().fg(c.fg);
            if i == self.selected {
                style = if self.focus == Focus::Files {
                    c.selected(style)
                } else {
                    style.attrs(Attrs::BOLD)
                };
            }
            put(buf, area.x, y, &line, usize::from(files_w - 1), style);
        }
        for y in top..top + height {
            put(buf, area.x + files_w, y, "│", 1, Style::new().fg(c.border));
        }
        let dx = area.x + files_w + 2;
        let dw = usize::from(area.right().saturating_sub(dx));
        let lines = self.diff_lines().to_vec();
        if self.cursor < self.diff_top {
            self.diff_top = self.cursor;
        } else if self.cursor >= self.diff_top + rows {
            self.diff_top = self.cursor + 1 - rows;
        }
        if let Some((_, Err(e))) = &self.diff {
            put(buf, dx, top, e, dw, Style::new().fg(c.error));
        }
        for (row, (i, l)) in lines
            .iter()
            .enumerate()
            .skip(self.diff_top)
            .take(rows)
            .enumerate()
        {
            let y = top + u16::try_from(row).unwrap_or(0);
            let num = |n: Option<u32>| n.map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
            let (text, style) = match l.kind {
                LineKind::Header => (l.text.clone(), Style::new().fg(c.muted)),
                LineKind::Hunk => (l.text.clone(), Style::new().fg(c.accent)),
                LineKind::Note => (l.text.clone(), Style::new().fg(c.muted)),
                LineKind::Add => (
                    format!("{} {} + {}", num(l.old), num(l.new), l.text),
                    Style::new().fg(Color::Indexed(2)),
                ),
                LineKind::Del => (
                    format!("{} {} - {}", num(l.old), num(l.new), l.text),
                    Style::new().fg(c.error),
                ),
                LineKind::Context => (
                    format!("{} {}   {}", num(l.old), num(l.new), l.text),
                    Style::new().fg(c.fg),
                ),
            };
            let style = if i == self.cursor && self.focus == Focus::Diff {
                style.bg(c.selection_bg)
            } else {
                style
            };
            put(buf, dx, y, &text, dw, style);
        }
        // Comments, or the one being typed; then the keys.
        let cy = area.bottom() - 2;
        if let Some(t) = &self.typing {
            let at = self.here().map_or_else(String::new, |h| {
                if h.at.is_empty() {
                    h.path
                } else {
                    format!("{} {}", h.path, h.at)
                }
            });
            put(
                buf,
                area.x + 1,
                cy,
                &format!("Comment on {at}: {t}█"),
                w,
                Style::new().fg(c.fg),
            );
        } else if let Some(m) = &self.message {
            put(buf, area.x + 1, cy, m, w, Style::new().fg(c.accent));
        } else if !self.comments.is_empty() {
            let last = self
                .comments
                .last()
                .map_or(String::new(), |l| l.text.clone());
            put(
                buf,
                area.x + 1,
                cy,
                &format!(
                    "{} comment{} for the agent (last: {last}); s sends them, u removes the last",
                    self.comments.len(),
                    if self.comments.len() == 1 { "" } else { "s" }
                ),
                w,
                Style::new().fg(c.fg),
            );
        }
        let keys = if self.typing.is_some() {
            "Enter adds it · Esc cancels"
        } else {
            "↑↓ move · Tab files/diff · c comment · s send to agent · C commit · m merge · p pull request · d discard · r refresh · q close"
        };
        put(
            buf,
            area.x + 1,
            area.bottom() - 1,
            keys,
            w,
            Style::new().fg(c.muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktrees::State;
    use ratatui_core::layout::Rect as TuiRect;
    use std::path::PathBuf;

    struct Fixture;

    impl Source for Fixture {
        fn changes(&self) -> Result<Changes, String> {
            Ok(Changes {
                files: vec![
                    Change {
                        path: "src/login.rs".into(),
                        old_path: None,
                        status: 'M',
                        added: Some(1),
                        removed: Some(1),
                    },
                    Change {
                        path: "notes.md".into(),
                        old_path: None,
                        status: '?',
                        added: Some(2),
                        removed: Some(0),
                    },
                ],
                ahead: 1,
                behind: 0,
                uncommitted: true,
                merge_base: "base".into(),
            })
        }
        fn diff(&self, change: &Change, _mb: &str) -> Result<Vec<DiffLine>, String> {
            Ok(if change.path == "src/login.rs" {
                worktrees::parse_diff(
                    "diff --git a/src/login.rs b/src/login.rs\n--- a/src/login.rs\n+++ b/src/login.rs\n@@ -1,3 +1,3 @@\n fn login() {\n-    check(user)\n+    check(user, password)\n }\n",
                )
            } else {
                vec![
                    DiffLine {
                        kind: LineKind::Add,
                        text: "# Notes".into(),
                        old: None,
                        new: Some(1),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        text: "todo".into(),
                        old: None,
                        new: Some(2),
                    },
                ]
            })
        }
    }

    fn wt() -> Worktree {
        Worktree {
            name: "fix-login".into(),
            repo: PathBuf::from("/p"),
            path: PathBuf::from("/p-wt/fix-login"),
            branch: "termos/fix-login".into(),
            base: "main".into(),
            base_commit: "abc".into(),
            title: "Fix the login".into(),
            ports: Some((20010, 10)),
            created: 0,
            origin: String::new(),
            state: State::Active,
            head: None,
            pr: None,
            set_up: true,
        }
    }

    fn app() -> ReviewApp {
        ReviewApp::with(wt(), Arc::new(Fixture), true)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::plain(code)
    }

    fn render(app: &mut ReviewApp, w: u16, h: u16) -> String {
        let area = TuiRect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf, &Theme::default());
        let mut out = String::new();
        for y in 0..h {
            let mut line = String::new();
            let mut skip = 0;
            for x in 0..w {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let sym = buf[(x, y)].symbol();
                skip = UnicodeWidthStr::width(sym).saturating_sub(1);
                line.push_str(sym);
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    #[test]
    fn the_review_window_shows_files_and_their_diff() {
        let mut a = app();
        insta::assert_snapshot!("review-window", render(&mut a, 100, 14));
        assert_eq!(a.reopen().as_deref(), Some("worktree.review fix-login"));
    }

    #[test]
    fn comments_go_to_the_agent_and_actions_run_in_windows() {
        let mut a = app();
        // To the diff, down to the changed line, comment on it.
        assert_eq!(a.handle_key(key(KeyCode::Tab)), AppEffect::Redraw);
        for _ in 0..6 {
            a.handle_key(key(KeyCode::Down));
        }
        a.handle_key(key(KeyCode::Char('c')));
        for ch in "also check the password".chars() {
            a.handle_key(key(KeyCode::Char(ch)));
        }
        a.handle_key(key(KeyCode::Enter));
        assert_eq!(a.comments().len(), 1);
        assert_eq!(a.comments()[0].at, "line 2");
        assert_eq!(a.comments()[0].quote, "check(user, password)");
        // A comment on the other file as a whole.
        a.handle_key(key(KeyCode::Tab));
        a.handle_key(key(KeyCode::Down));
        a.handle_key(key(KeyCode::Char('c')));
        a.handle_paste("needs a title");
        a.handle_key(key(KeyCode::Enter));
        assert!(render(&mut a, 100, 14).contains("[Send to agent (2)]"));
        let msg = a.message_for_agent();
        assert!(msg.contains("branch termos/fix-login"), "{msg}");
        assert!(
            msg.contains(
                "- src/login.rs, line 2 (`check(user, password)`): also check the password"
            ),
            "{msg}"
        );
        assert!(msg.contains("- notes.md: needs a title"), "{msg}");
        match a.handle_key(key(KeyCode::Char('s'))) {
            AppEffect::TellAgent {
                worktree,
                dir,
                text,
            } => {
                assert_eq!(worktree, "fix-login");
                assert_eq!(dir, PathBuf::from("/p-wt/fix-login"));
                assert_eq!(text, msg);
            }
            other => panic!("{other:?}"),
        }
        assert!(a.comments().is_empty());
        // Nothing to send now.
        assert_eq!(a.handle_key(key(KeyCode::Char('s'))), AppEffect::Redraw);
        for (k, cmd) in [
            ('C', "worktree.run commit fix-login"),
            ('m', "worktree.run merge fix-login"),
            ('p', "worktree.run pr fix-login"),
            ('d', "worktree.run discard fix-login"),
            ('o', "worktree.open fix-login"),
        ] {
            assert_eq!(
                a.handle_key(key(KeyCode::Char(k))),
                AppEffect::Command(cmd.into())
            );
        }
        // The buttons too.
        render(&mut a, 100, 14);
        let (x, _) = a.button_spans[2];
        let click = MouseEvent {
            kind: MouseKind::Down(MouseButton::Left),
            x: x + 1,
            y: 1,
            mods: Mods::empty(),
        };
        assert_eq!(
            a.handle_mouse(click),
            AppEffect::Command("worktree.run merge fix-login".into())
        );
        assert_eq!(a.handle_key(key(KeyCode::Char('q'))), AppEffect::Close);
    }

    #[test]
    fn it_looks_again_while_shown() {
        let mut a = app();
        let due = a.deadline().expect("a next look");
        assert!(due > Instant::now());
        assert_eq!(a.on_timer(due), AppEffect::None);
        assert!(a.deadline().expect("again") > due);
    }
}
