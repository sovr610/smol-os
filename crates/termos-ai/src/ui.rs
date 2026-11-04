// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent window and the audit log viewer: in-process [`App`]s the
//! desktop opens through the [`AgentLauncher`] (ARCHITECTURE.md §9, §11).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::style::Style as TuiStyle;
use ratatui_core::text::{Line, Span};
use secrecy::SecretString;
use termos_core::app::{App, AppEffect, AppWaker, LaunchContext, LaunchKind, Launcher, tui_style};
use termos_core::input::{KeyCode, KeyEvent, Mods, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;
use termos_term::VtBackend;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agent::{self, Handle, Input, Item, Launch, Phase, SecretSlot, ToolState, View};
use crate::audit;
use crate::history::{self, Saved};
use crate::tools::Preview;

// ---------------------------------------------------------------------
// The launcher

/// Opens agent windows and audit logs for the desktop.
pub struct AgentLauncher {
    /// The `termos` binary (the sandbox helper).
    helper: Option<PathBuf>,
    /// The server's config file.
    config_file: Option<PathBuf>,
    home: Option<PathBuf>,
    secrets: std::sync::OnceLock<Arc<SecretSlot>>,
    /// The command center's history and live sessions (D-093).
    journal: Option<Arc<termos_core::journal::Journal>>,
    registry: Option<Arc<crate::registry::Registry>>,
}

impl AgentLauncher {
    pub fn new(
        helper: Option<PathBuf>,
        config_file: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            helper,
            config_file,
            home,
            secrets: std::sync::OnceLock::new(),
            journal: None,
            registry: None,
        }
    }

    /// Sessions record their usage and decisions in `journal` and list
    /// themselves in `registry` while they run (the command center).
    #[must_use]
    pub fn with_center(
        mut self,
        journal: Arc<termos_core::journal::Journal>,
        registry: Arc<crate::registry::Registry>,
    ) -> Self {
        self.journal = Some(journal);
        self.registry = Some(registry);
        self
    }

    fn launch(&self, ctx: &LaunchContext<'_>, profile: String, resume: Option<String>) -> Launch {
        let secrets = self
            .secrets
            .get_or_init(|| {
                Arc::new(SecretSlot::new(
                    ctx.config.secrets.store,
                    ctx.paths.data_dir.clone(),
                ))
            })
            .clone();
        Launch {
            agents: ctx.config.agents.clone(),
            secrets,
            paths: ctx.paths.clone(),
            profile,
            workspace: ctx
                .cwd
                .map(Path::to_owned)
                .or_else(|| self.home.clone())
                .unwrap_or_else(|| PathBuf::from(".")),
            resume,
            helper: self.helper.clone(),
            config_file: self.config_file.clone(),
            home: self.home.clone(),
            journal: self.journal.clone(),
            registry: self.registry.clone(),
        }
    }
}

impl Launcher for AgentLauncher {
    fn open(
        &self,
        kind: LaunchKind,
        arg: &str,
        ctx: &LaunchContext<'_>,
    ) -> Result<Box<dyn App>, String> {
        match kind {
            LaunchKind::Agent => {
                let profile = if arg.is_empty() {
                    ctx.config.agents.default_profile.clone()
                } else {
                    arg.to_owned()
                };
                let launch = self.launch(ctx, profile, None);
                Ok(Box::new(
                    AgentWindow::start(launch)
                        .map_err(|e| format!("cannot start the agent: {e}"))?,
                ))
            }
            LaunchKind::AgentResume => {
                let launch = self.launch(ctx, ctx.config.agents.default_profile.clone(), None);
                if !ctx.config.agents.history {
                    return Err("conversations are not kept ([agents] history = false)".into());
                }
                if arg.is_empty() {
                    Ok(Box::new(AgentWindow::picker(launch)))
                } else {
                    let mut launch = launch;
                    launch.resume = Some(arg.to_owned());
                    Ok(Box::new(
                        AgentWindow::start(launch)
                            .map_err(|e| format!("cannot start the agent: {e}"))?,
                    ))
                }
            }
            LaunchKind::Audit => Ok(Box::new(AuditViewer::open(
                &audit::dir(&ctx.paths.state_dir),
                arg,
            ))),
            // The termos binary opens it (termos-center); an agent-only
            // launcher has none.
            LaunchKind::Center => Err("the command center is not part of this build".into()),
            LaunchKind::Usage => Err("the usage app is not part of this build".into()),
        }
    }
}

// ---------------------------------------------------------------------
// Text helpers

fn style(theme: &Theme, f: impl FnOnce(&Theme) -> Style) -> TuiStyle {
    tui_style(f(theme))
}

fn fg(
    theme: &Theme,
    pick: impl FnOnce(&termos_render::theme::ThemeColors) -> termos_render::color::Color,
) -> TuiStyle {
    tui_style(Style::new().fg(pick(&theme.colors)))
}

/// Word-wraps `text` to `width` columns; the first line starts with
/// `first`, the rest with `rest` (both as wide as each other).
fn wrap(
    text: &str,
    width: u16,
    st: TuiStyle,
    first: Span<'static>,
    rest: &str,
) -> Vec<Line<'static>> {
    let indent = first.content.width();
    let avail = usize::from(width).saturating_sub(indent).max(8);
    let mut out = Vec::new();
    let mut first_line = true;
    let mut push = |s: String, out: &mut Vec<Line<'static>>| {
        let lead = if first_line {
            first.clone()
        } else {
            Span::styled(rest.to_owned(), first.style)
        };
        first_line = false;
        out.push(Line::from(vec![lead, Span::styled(s, st)]));
    };
    for para in text.split('\n') {
        let mut line = String::new();
        let mut w = 0;
        for word in para.split_inclusive(' ') {
            let ww = word.width();
            if w + ww > avail && !line.is_empty() {
                push(std::mem::take(&mut line).trim_end().to_owned(), &mut out);
                w = 0;
            }
            if ww > avail {
                // A word longer than the line: cut it.
                for c in word.chars() {
                    let cw = c.width().unwrap_or(0);
                    if w + cw > avail {
                        push(std::mem::take(&mut line), &mut out);
                        w = 0;
                    }
                    line.push(c);
                    w += cw;
                }
            } else {
                line.push_str(word);
                w += ww;
            }
        }
        push(line.trim_end().to_owned(), &mut out);
    }
    out
}

/// A command's terminal as lines (trailing blank rows dropped).
fn term_lines(term: &termos_term::AlacrittyVt, width: u16, indent: u16) -> Vec<Line<'static>> {
    let (cols, rows) = term.size();
    let mut grid = Grid::new(cols, rows);
    term.render_into(&mut grid, Rect::new(0, 0, cols, rows));
    let mut lines = Vec::new();
    for y in 0..rows {
        let mut spans = vec![Span::raw(" ".repeat(usize::from(indent)))];
        let mut cur = String::new();
        let mut cur_style = None;
        let mut used = indent;
        for cell in grid.row(y) {
            if cell.is_continuation() {
                continue;
            }
            let w = u16::from(cell.width.max(1));
            if used + w > width {
                break;
            }
            used += w;
            let st = tui_style(cell.style);
            if cur_style != Some(st) && !cur.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut cur),
                    cur_style.unwrap_or_default(),
                ));
            }
            cur_style = Some(st);
            cur.push_str(cell.display_symbol());
        }
        if !cur.is_empty() {
            spans.push(Span::styled(cur, cur_style.unwrap_or_default()));
        }
        lines.push((grid.row_text(y).trim().is_empty(), Line::from(spans)));
    }
    while lines.last().is_some_and(|(blank, _)| *blank) {
        lines.pop();
    }
    lines.into_iter().map(|(_, l)| l).collect()
}

fn human(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn put_line(buf: &mut Buffer, area: TuiRect, y: u16, line: &Line<'_>) {
    if y < area.height {
        buf.set_line(area.x, area.y + y, line, area.width);
    }
}

// ---------------------------------------------------------------------
// The input line

#[derive(Debug, Default, Clone)]
struct LineEdit {
    text: String,
    /// Byte offset, on a character boundary.
    cursor: usize,
}

impl LineEdit {
    fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    fn clear(&mut self) {
        self.take();
    }

    /// Handles an editing key; whether it was one.
    fn key(&mut self, k: KeyEvent) -> bool {
        let prev = |s: &str, i: usize| s[..i].chars().next_back().map_or(i, |c| i - c.len_utf8());
        let next = |s: &str, i: usize| s[i..].chars().next().map_or(i, |c| i + c.len_utf8());
        match k.code {
            KeyCode::Char(c) if !k.mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER) => {
                self.insert(c.encode_utf8(&mut [0; 4]));
            }
            KeyCode::Enter if k.mods.contains(Mods::ALT) => self.insert("\n"),
            KeyCode::Char('j') if k.mods == Mods::CTRL => self.insert("\n"),
            KeyCode::Char('u') if k.mods == Mods::CTRL => self.clear(),
            KeyCode::Char('a') if k.mods == Mods::CTRL => self.cursor = 0,
            KeyCode::Char('e') if k.mods == Mods::CTRL => self.cursor = self.text.len(),
            KeyCode::Backspace => {
                let p = prev(&self.text, self.cursor);
                self.text.replace_range(p..self.cursor, "");
                self.cursor = p;
            }
            KeyCode::Delete => {
                let n = next(&self.text, self.cursor);
                self.text.replace_range(self.cursor..n, "");
            }
            KeyCode::Left => self.cursor = prev(&self.text, self.cursor),
            KeyCode::Right => self.cursor = next(&self.text, self.cursor),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => return false,
        }
        true
    }

    /// The visible part for `width` columns, and the cursor's column.
    fn view(&self, width: u16, masked: bool) -> (String, u16) {
        let shown: String = if masked {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.replace('\n', "⏎")
        };
        let before = if masked {
            self.text[..self.cursor].chars().count()
        } else {
            self.text[..self.cursor].replace('\n', "⏎").width()
        };
        let width = usize::from(width.max(2)) - 1;
        let start = before.saturating_sub(width);
        let mut out = String::new();
        let mut col = 0;
        for c in shown.chars() {
            let w = c.width().unwrap_or(0);
            if col >= start && col + w - start <= width {
                out.push(c);
            }
            col += w;
        }
        (out, u16::try_from(before - start).unwrap_or(0))
    }
}

// ---------------------------------------------------------------------
// The agent window

/// An agent conversation in a window.
pub struct AgentWindow {
    handle: Option<Handle>,
    /// Waiting to start a saved conversation picked from a list.
    pending_launch: Option<Launch>,
    saved: Vec<Saved>,
    input: LineEdit,
    /// Lines scrolled up from the bottom of the transcript.
    scroll: u16,
    sel: usize,
    confirm_scroll: u16,
    waker: Option<AppWaker>,
    /// The transcript's height at the last draw (for paging).
    page: u16,
}

impl std::fmt::Debug for AgentWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentWindow")
    }
}

impl AgentWindow {
    /// A window running a new (or resumed) session.
    pub fn start(launch: Launch) -> std::io::Result<Self> {
        Ok(Self::with_handle(Some(agent::start(launch)?), None))
    }

    /// A window listing saved conversations; Enter resumes one.
    pub fn picker(launch: Launch) -> Self {
        let mut w = Self::with_handle(None, None);
        w.saved = history::list(&history::dir(&launch.paths.state_dir));
        w.pending_launch = Some(launch);
        w
    }

    /// A window showing an existing session (tests drive one directly).
    pub fn attach(handle: Handle) -> Self {
        Self::with_handle(Some(handle), None)
    }

    fn with_handle(handle: Option<Handle>, pending: Option<Launch>) -> Self {
        Self {
            handle,
            pending_launch: pending,
            saved: Vec::new(),
            input: LineEdit::default(),
            scroll: 0,
            sel: 0,
            confirm_scroll: 0,
            waker: None,
            page: 10,
        }
    }

    fn phase(&self) -> Option<Phase> {
        let h = self.handle.as_ref()?;
        h.view.lock().ok().map(|v| v.phase.clone())
    }

    fn send(&self, input: Input) {
        if let Some(h) = &self.handle {
            h.send(input);
        }
    }

    /// The models matching what is typed.
    fn filtered_models(
        models: &[crate::provider::ModelInfo],
        filter: &str,
    ) -> Vec<crate::provider::ModelInfo> {
        let f = filter.to_lowercase();
        models
            .iter()
            .filter(|m| {
                f.is_empty()
                    || m.id.to_lowercase().contains(&f)
                    || m.name
                        .as_deref()
                        .is_some_and(|n| n.to_lowercase().contains(&f))
            })
            .cloned()
            .collect()
    }

    fn key_picker_saved(&mut self, k: KeyEvent) -> AppEffect {
        match k.code {
            KeyCode::Esc => AppEffect::Close,
            KeyCode::Up => {
                self.sel = self.sel.saturating_sub(1);
                AppEffect::Redraw
            }
            KeyCode::Down => {
                self.sel = (self.sel + 1).min(self.saved.len().saturating_sub(1));
                AppEffect::Redraw
            }
            KeyCode::Enter => {
                let (Some(s), Some(mut launch)) =
                    (self.saved.get(self.sel), self.pending_launch.clone())
                else {
                    return AppEffect::None;
                };
                launch.resume = Some(s.id.clone());
                launch.profile.clone_from(&s.profile);
                launch.workspace.clone_from(&s.workspace);
                match agent::start(launch) {
                    Ok(h) => {
                        if let Some(w) = &self.waker {
                            h.set_waker(w.clone());
                        }
                        self.handle = Some(h);
                        self.pending_launch = None;
                        self.sel = 0;
                        AppEffect::Redraw
                    }
                    Err(e) => AppEffect::Notify(format!("cannot start the agent: {e}")),
                }
            }
            _ => AppEffect::None,
        }
    }
}

impl Drop for AgentWindow {
    fn drop(&mut self) {
        self.send(Input::Close);
    }
}

impl App for AgentWindow {
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Agent
    }

    fn busy(&self) -> bool {
        matches!(self.phase(), Some(Phase::Busy | Phase::Starting))
    }

    fn title(&self) -> String {
        let Some(h) = &self.handle else {
            return "Agent: resume a conversation".into();
        };
        match h.view.lock() {
            Ok(v) if !v.model.is_empty() => format!("Agent · {} {}", v.provider, v.model),
            _ => "Agent".into(),
        }
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Some(h) = &self.handle {
            h.set_waker(waker.clone());
        }
        self.waker = Some(waker);
    }

    fn handle_paste(&mut self, text: &str) -> AppEffect {
        self.input.insert(text);
        AppEffect::Redraw
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        match event.kind {
            MouseKind::ScrollUp => {
                self.scroll = self.scroll.saturating_add(3);
                AppEffect::Redraw
            }
            MouseKind::ScrollDown => {
                self.scroll = self.scroll.saturating_sub(3);
                AppEffect::Redraw
            }
            _ => AppEffect::None,
        }
    }

    fn handle_key(&mut self, k: KeyEvent) -> AppEffect {
        if self.handle.is_none() {
            return self.key_picker_saved(k);
        }
        let Some(phase) = self.phase() else {
            return AppEffect::None;
        };
        // Scrolling works in every phase but confirmation.
        if !matches!(phase, Phase::Confirm(_)) {
            match k.code {
                KeyCode::PageUp => {
                    self.scroll = self.scroll.saturating_add(self.page.max(2) - 1);
                    return AppEffect::Redraw;
                }
                KeyCode::PageDown => {
                    self.scroll = self.scroll.saturating_sub(self.page.max(2) - 1);
                    return AppEffect::Redraw;
                }
                _ => {}
            }
        }
        let cancel = k == KeyEvent::plain(KeyCode::Esc) || k == KeyEvent::ctrl('c');
        match phase {
            Phase::Confirm(p) => match k.code {
                KeyCode::Char('y') => {
                    self.confirm_scroll = 0;
                    self.send(Input::Approve { remember: false });
                    AppEffect::Redraw
                }
                KeyCode::Char('a') if p.remember => {
                    self.confirm_scroll = 0;
                    self.send(Input::Approve { remember: true });
                    AppEffect::Redraw
                }
                KeyCode::Char('n') => {
                    self.confirm_scroll = 0;
                    self.send(Input::Deny);
                    AppEffect::Redraw
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.confirm_scroll = self.confirm_scroll.saturating_add(1);
                    AppEffect::Redraw
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.confirm_scroll = self.confirm_scroll.saturating_sub(1);
                    AppEffect::Redraw
                }
                KeyCode::PageDown => {
                    self.confirm_scroll = self.confirm_scroll.saturating_add(10);
                    AppEffect::Redraw
                }
                KeyCode::PageUp => {
                    self.confirm_scroll = self.confirm_scroll.saturating_sub(10);
                    AppEffect::Redraw
                }
                _ if cancel => {
                    self.send(Input::Cancel);
                    AppEffect::Redraw
                }
                _ => AppEffect::None,
            },
            Phase::Busy => {
                if cancel {
                    self.send(Input::Cancel);
                    return AppEffect::Redraw;
                }
                if self.input.key(k) {
                    AppEffect::Redraw
                } else {
                    AppEffect::None
                }
            }
            Phase::Failed(_) | Phase::Starting
                if cancel || k.code == KeyCode::Char('q') && matches!(phase, Phase::Failed(_)) =>
            {
                AppEffect::Close
            }
            Phase::NeedPassphrase { .. } => match k.code {
                KeyCode::Enter if !self.input.text.is_empty() => {
                    let p = SecretString::from(self.input.take());
                    self.send(Input::Passphrase(p));
                    AppEffect::Redraw
                }
                _ if cancel => AppEffect::Close,
                _ => {
                    if self.input.key(k) {
                        AppEffect::Redraw
                    } else {
                        AppEffect::None
                    }
                }
            },
            Phase::PickProvider { names } => match k.code {
                KeyCode::Up => {
                    self.sel = self.sel.saturating_sub(1);
                    AppEffect::Redraw
                }
                KeyCode::Down => {
                    self.sel = (self.sel + 1).min(names.len().saturating_sub(1));
                    AppEffect::Redraw
                }
                KeyCode::Enter => {
                    if let Some(n) = names.get(self.sel) {
                        self.send(Input::Provider(n.clone()));
                        self.sel = 0;
                    }
                    AppEffect::Redraw
                }
                _ if cancel => AppEffect::Close,
                _ => AppEffect::None,
            },
            Phase::PickModel { models, .. } => {
                let shown = Self::filtered_models(&models, &self.input.text);
                match k.code {
                    KeyCode::Up => {
                        self.sel = self.sel.saturating_sub(1);
                        AppEffect::Redraw
                    }
                    KeyCode::Down => {
                        self.sel = (self.sel + 1).min(shown.len().saturating_sub(1));
                        AppEffect::Redraw
                    }
                    KeyCode::Enter => {
                        let pick = shown.get(self.sel).map(|m| m.id.clone()).or_else(|| {
                            let t = self.input.text.trim().to_owned();
                            (!t.is_empty()).then_some(t)
                        });
                        if let Some(m) = pick {
                            self.input.clear();
                            self.sel = 0;
                            self.send(Input::Model(m));
                        }
                        AppEffect::Redraw
                    }
                    _ if cancel => AppEffect::Close,
                    _ => {
                        if self.input.key(k) {
                            self.sel = 0;
                            AppEffect::Redraw
                        } else {
                            AppEffect::None
                        }
                    }
                }
            }
            Phase::Ready | Phase::Starting | Phase::Failed(_) => match k.code {
                KeyCode::Enter if !k.mods.contains(Mods::ALT) => {
                    if matches!(phase, Phase::Ready) && !self.input.text.trim().is_empty() {
                        let text = self.input.take();
                        self.scroll = 0;
                        self.send(Input::Send(text));
                    }
                    AppEffect::Redraw
                }
                _ if cancel && self.input.text.is_empty() => AppEffect::Close,
                _ if cancel => {
                    self.input.clear();
                    AppEffect::Redraw
                }
                _ => {
                    if self.input.key(k) {
                        AppEffect::Redraw
                    } else {
                        AppEffect::None
                    }
                }
            },
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        if area.width < 10 || area.height < 4 {
            return;
        }
        let inner = TuiRect::new(area.x + 1, area.y, area.width - 2, area.height);
        if self.handle.is_none() {
            self.render_saved(inner, buf, theme);
            return;
        }
        let Some(h) = self.handle.clone() else { return };
        let Ok(view) = h.view.lock() else { return };
        // Header.
        let mut head = vec![Span::styled(
            view.profile.clone(),
            style(theme, |t| t.overlay_muted.bold()),
        )];
        if !view.provider.is_empty() {
            head.push(Span::styled(
                format!(" · {}", view.provider),
                fg(theme, |c| c.muted),
            ));
        }
        if !view.model.is_empty() {
            head.push(Span::styled(
                format!(" {}", view.model),
                fg(theme, |c| c.accent),
            ));
        }
        let u = view.usage;
        if u.input + u.output + u.cache_read > 0 {
            let mut t = format!(" · {} in", human(u.input + u.cache_read + u.cache_write));
            if u.cache_read > 0 {
                t.push_str(&format!(" ({} cached)", human(u.cache_read)));
            }
            t.push_str(&format!(" / {} out", human(u.output)));
            head.push(Span::styled(t, fg(theme, |c| c.muted)));
        }
        if let Some(c) = view.cost {
            head.push(Span::styled(format!(" · ${c:.4}"), fg(theme, |c| c.muted)));
        }
        if !view.sandbox.is_empty() {
            let warn = !view.sandbox.starts_with("sandbox: bubblewrap");
            head.push(Span::styled(
                format!(" · {}", view.sandbox),
                if warn {
                    fg(theme, |c| c.warn)
                } else {
                    fg(theme, |c| c.muted)
                },
            ));
        }
        put_line(buf, inner, 0, &Line::from(head));
        // Bottom: status and input.
        let input_y = inner.height - 1;
        let status_y = inner.height - 2;
        let busy = matches!(view.phase, Phase::Busy | Phase::Starting);
        let hint = match &view.phase {
            Phase::Ready => "Enter sends · Alt+Enter new line · PgUp/PgDn scroll · Esc closes",
            Phase::Busy => "Esc cancels",
            Phase::Confirm(_) => "",
            Phase::NeedPassphrase { .. } => "Enter unlocks · Esc closes",
            Phase::PickModel { .. } => "type to filter, or a model id · ↑↓ · Enter picks",
            Phase::PickProvider { .. } => "↑↓ · Enter picks",
            Phase::Starting => "",
            Phase::Failed(_) => "Esc closes",
        };
        let mut status = vec![Span::styled("─ ".to_owned(), style(theme, |t| t.border))];
        if !view.status.is_empty() {
            status.push(Span::styled(
                format!("{} ", view.status),
                if busy {
                    fg(theme, |c| c.accent)
                } else {
                    fg(theme, |c| c.warn)
                },
            ));
        }
        status.push(Span::styled(
            hint.to_owned(),
            style(theme, |t| t.overlay_muted),
        ));
        put_line(buf, inner, status_y, &Line::from(status));
        let masked = matches!(view.phase, Phase::NeedPassphrase { .. });
        let prompt = if masked { "passphrase › " } else { "› " };
        let (text, _cursor) = self
            .input
            .view(inner.width.saturating_sub(prompt.width() as u16), masked);
        put_line(
            buf,
            inner,
            input_y,
            &Line::from(vec![
                Span::styled(prompt.to_owned(), fg(theme, |c| c.accent)),
                Span::styled(text, fg(theme, |c| c.fg)),
                Span::styled("▏".to_owned(), fg(theme, |c| c.accent)),
            ]),
        );
        // The middle.
        let mid = TuiRect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(3),
        );
        match &view.phase {
            Phase::PickProvider { names } => {
                let mut lines = vec![Line::from(Span::styled(
                    "Pick a provider:".to_owned(),
                    style(theme, |t| t.overlay_title),
                ))];
                for (i, n) in names.iter().enumerate() {
                    let st = if i == self.sel {
                        style(theme, |t| t.overlay_selected)
                    } else {
                        fg(theme, |c| c.fg)
                    };
                    lines.push(Line::from(Span::styled(format!("  {n}"), st)));
                }
                draw_lines(buf, mid, &lines, 0);
            }
            Phase::PickModel { models, error } => {
                let shown = Self::filtered_models(models, &self.input.text);
                let mut lines = vec![Line::from(Span::styled(
                    format!(
                        "Pick a model from {} ({} listed):",
                        view.provider,
                        models.len()
                    ),
                    style(theme, |t| t.overlay_title),
                ))];
                if let Some(e) = error {
                    lines.extend(wrap(e, mid.width, fg(theme, |c| c.warn), Span::raw(""), ""));
                }
                let start = self
                    .sel
                    .saturating_sub(usize::from(mid.height.saturating_sub(4)));
                for (i, m) in shown.iter().enumerate().skip(start) {
                    let mut text = format!("  {}", m.id);
                    let mut extra = Vec::new();
                    if let Some(n) = &m.name {
                        extra.push(n.clone());
                    }
                    if let Some(c) = m.context {
                        extra.push(format!("{} context", human(c)));
                    }
                    if let Some(p) = m.price {
                        extra.push(format!("${}/${} per M", p.input, p.output));
                    }
                    if !extra.is_empty() {
                        text.push_str(&format!("  ({})", extra.join(", ")));
                    }
                    let st = if i == self.sel {
                        style(theme, |t| t.overlay_selected)
                    } else {
                        fg(theme, |c| c.fg)
                    };
                    lines.push(Line::from(Span::styled(text, st)));
                }
                if shown.is_empty() && !self.input.text.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("  Enter uses \"{}\"", self.input.text.trim()),
                        style(theme, |t| t.overlay_muted),
                    )));
                }
                draw_lines(buf, mid, &lines, 0);
            }
            Phase::NeedPassphrase { error } => {
                let mut lines = wrap(
                    "TermOS keeps API keys in its secrets file, locked with your passphrase. Enter it to unlock the file until the server stops.",
                    mid.width,
                    fg(theme, |c| c.fg),
                    Span::raw(""),
                    "",
                );
                if let Some(e) = error {
                    lines.extend(wrap(
                        e,
                        mid.width,
                        fg(theme, |c| c.error),
                        Span::raw(""),
                        "",
                    ));
                }
                draw_lines(buf, mid, &lines, 0);
            }
            Phase::Failed(why) => {
                let mut lines = transcript(&view, theme, mid.width);
                lines.extend(wrap(
                    why,
                    mid.width,
                    fg(theme, |c| c.error),
                    Span::styled("! ".to_owned(), fg(theme, |c| c.error)),
                    "  ",
                ));
                draw_bottom(buf, mid, &lines, self.scroll);
            }
            Phase::Confirm(p) => {
                let panel_h = (mid.height * 3 / 5).max(6).min(mid.height);
                let top = TuiRect::new(mid.x, mid.y, mid.width, mid.height - panel_h);
                let lines = transcript(&view, theme, mid.width);
                draw_bottom(buf, top, &lines, 0);
                let panel = TuiRect::new(mid.x, mid.y + mid.height - panel_h, mid.width, panel_h);
                render_confirm(buf, panel, p, theme, self.confirm_scroll);
            }
            Phase::Ready | Phase::Busy | Phase::Starting => {
                let lines = transcript(&view, theme, mid.width);
                let max = u16::try_from(lines.len())
                    .unwrap_or(u16::MAX)
                    .saturating_sub(mid.height);
                self.scroll = self.scroll.min(max);
                if lines.is_empty() && matches!(view.phase, Phase::Ready) {
                    let hello = format!(
                        "Ask the agent to do something in {}. Each tool call follows the {} profile; you approve what it asks for.",
                        view.workspace.display(),
                        view.profile
                    );
                    draw_lines(
                        buf,
                        mid,
                        &wrap(
                            &hello,
                            mid.width,
                            style(theme, |t| t.overlay_muted),
                            Span::raw(""),
                            "",
                        ),
                        0,
                    );
                } else {
                    draw_bottom(buf, mid, &lines, self.scroll);
                }
            }
        }
        self.page = mid.height;
    }
}

impl AgentWindow {
    fn render_saved(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let mut lines = vec![Line::from(Span::styled(
            "Resume a conversation (↑↓, Enter; Esc closes):".to_owned(),
            style(theme, |t| t.overlay_title),
        ))];
        if self.saved.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no saved conversations".to_owned(),
                style(theme, |t| t.overlay_muted),
            )));
        }
        let start = self
            .sel
            .saturating_sub(usize::from(area.height.saturating_sub(3)));
        for (i, s) in self.saved.iter().enumerate().skip(start) {
            let when: String = s
                .updated
                .chars()
                .take(16)
                .collect::<String>()
                .replace('T', " ");
            let text = format!("  {when}  {}  · {} {}", s.title(), s.provider, s.model);
            let st = if i == self.sel {
                style(theme, |t| t.overlay_selected)
            } else {
                fg(theme, |c| c.fg)
            };
            lines.push(Line::from(Span::styled(text, st)));
        }
        draw_lines(buf, area, &lines, 0);
    }
}

/// Lines from the top of `area`, skipping `skip`.
fn draw_lines(buf: &mut Buffer, area: TuiRect, lines: &[Line<'static>], skip: usize) {
    for (y, l) in lines
        .iter()
        .skip(skip)
        .take(usize::from(area.height))
        .enumerate()
    {
        put_line(buf, area, y as u16, l);
    }
}

/// The last lines in `area`, `scroll` lines up from the end.
fn draw_bottom(buf: &mut Buffer, area: TuiRect, lines: &[Line<'static>], scroll: u16) {
    let h = usize::from(area.height);
    let end = lines.len().saturating_sub(usize::from(scroll));
    let start = end.saturating_sub(h);
    draw_lines(buf, area, &lines[start..end], 0);
}

fn render_confirm(buf: &mut Buffer, area: TuiRect, p: &agent::Pending, theme: &Theme, scroll: u16) {
    let bar = Line::from(Span::styled(
        "─".repeat(usize::from(area.width)),
        fg(theme, |c| c.warn),
    ));
    put_line(buf, area, 0, &bar);
    let mut head = wrap(
        &p.summary,
        area.width,
        style(theme, |t| Style::new().fg(t.colors.fg).bold()),
        Span::styled(
            "Allow? ".to_owned(),
            style(theme, |t| Style::new().fg(t.colors.warn).bold()),
        ),
        "       ",
    );
    head.truncate(3);
    head.push(Line::from(Span::styled(
        format!("It asks because {}.", p.why),
        style(theme, |t| t.overlay_muted),
    )));
    let mut body: Vec<Line<'static>> = Vec::new();
    match &p.preview {
        Preview::Diff { diff, created, .. } => {
            if *created {
                body.push(Line::from(Span::styled(
                    "(a new file)".to_owned(),
                    style(theme, |t| t.overlay_muted),
                )));
            }
            for l in diff.lines() {
                let st = if l.starts_with("+++") || l.starts_with("---") {
                    style(theme, |t| t.overlay_muted)
                } else if l.starts_with('+') {
                    tui_style(Style::new().fg(termos_render::color::Color::rgb(0x57, 0xab, 0x5a)))
                } else if l.starts_with('-') {
                    fg(theme, |c| c.error)
                } else if l.starts_with("@@") {
                    fg(theme, |c| c.accent)
                } else {
                    fg(theme, |c| c.fg)
                };
                let text: String = l.chars().take(usize::from(area.width)).collect();
                body.push(Line::from(Span::styled(text, st)));
            }
        }
        Preview::Command { line, network } => {
            body.extend(wrap(
                line,
                area.width,
                fg(theme, |c| c.fg),
                Span::styled("$ ".to_owned(), fg(theme, |c| c.accent)),
                "  ",
            ));
            if *network {
                body.push(Line::from(Span::styled(
                    "with network access (the sandbox shares this machine's network)".to_owned(),
                    fg(theme, |c| c.warn),
                )));
            }
        }
        Preview::Fetch { url } => body.extend(wrap(
            url,
            area.width,
            fg(theme, |c| c.fg),
            Span::raw("GET "),
            "    ",
        )),
        Preview::Mcp { server, tool, args } => {
            body.push(Line::from(Span::styled(
                format!("MCP server {server}, tool {tool}"),
                fg(theme, |c| c.fg),
            )));
            body.extend(wrap(
                args,
                area.width,
                style(theme, |t| t.overlay_muted),
                Span::raw(""),
                "",
            ));
        }
        Preview::None => {}
    }
    let keys = if p.remember {
        "y allow · a allow for this session · n refuse · Esc stop the turn · ↑↓ scroll"
    } else {
        "y allow · n refuse · Esc stop the turn · ↑↓ scroll (this always asks)"
    };
    let foot = Line::from(Span::styled(
        keys.to_owned(),
        style(theme, |t| t.overlay_key),
    ));
    let mut y = 1u16;
    for l in &head {
        put_line(buf, area, y, l);
        y += 1;
    }
    let body_h = area.height.saturating_sub(y + 1);
    let max_scroll = u16::try_from(body.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(body_h);
    let skip = usize::from(scroll.min(max_scroll));
    for l in body.iter().skip(skip).take(usize::from(body_h)) {
        put_line(buf, area, y, l);
        y += 1;
    }
    put_line(buf, area, area.height - 1, &foot);
}

/// The conversation as lines for `width` columns.
fn transcript(view: &View, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for item in &view.items {
        match item {
            Item::User(t) => {
                out.push(Line::from(""));
                out.extend(wrap(
                    t,
                    width,
                    style(theme, |t| Style::new().fg(t.colors.fg).bold()),
                    Span::styled("› ".to_owned(), fg(theme, |c| c.accent)),
                    "  ",
                ));
            }
            Item::Assistant { text, .. } => {
                if !text.trim().is_empty() {
                    out.extend(wrap(
                        text.trim_matches('\n'),
                        width,
                        fg(theme, |c| c.fg),
                        Span::raw("  "),
                        "  ",
                    ));
                }
            }
            Item::Thinking { text, .. } => {
                if !text.trim().is_empty() {
                    let st = tui_style(Style::new().fg(theme.colors.muted).attrs(Attrs::ITALIC));
                    out.extend(wrap(
                        text.trim(),
                        width,
                        st,
                        Span::styled("  ∴ ".to_owned(), st),
                        "    ",
                    ));
                }
            }
            Item::Tool(t) => {
                let (mark, st) = match &t.state {
                    ToolState::Asking => ("?", fg(theme, |c| c.warn)),
                    ToolState::Running => ("…", fg(theme, |c| c.accent)),
                    ToolState::Done => ("✓", fg(theme, |c| c.muted)),
                    ToolState::Failed => ("✗", fg(theme, |c| c.error)),
                    ToolState::Denied(_) | ToolState::Cancelled => ("⊘", fg(theme, |c| c.error)),
                };
                let mut lines = wrap(
                    &t.summary,
                    width,
                    fg(theme, |c| c.fg),
                    Span::styled(format!("  {mark} "), st),
                    "    ",
                );
                match &t.state {
                    ToolState::Denied(why) => lines.push(Line::from(Span::styled(
                        format!("    not run: {why}"),
                        fg(theme, |c| c.error),
                    ))),
                    ToolState::Cancelled => lines.push(Line::from(Span::styled(
                        "    cancelled".to_owned(),
                        fg(theme, |c| c.error),
                    ))),
                    _ => {}
                }
                out.extend(lines);
                if let Some(term) = &t.term {
                    out.extend(term_lines(term, width, 4));
                }
                if let Some(r) = &t.result {
                    for l in r.lines().take(6) {
                        let l = l.replace('\t', "  ");
                        let text: String = l
                            .chars()
                            .take(usize::from(width.saturating_sub(4)))
                            .collect();
                        out.push(Line::from(Span::styled(
                            format!("    {text}"),
                            style(theme, |t| t.overlay_muted),
                        )));
                    }
                }
            }
            Item::Notice(t) => out.push(Line::from(Span::styled(
                format!("  — {t}"),
                style(theme, |t| t.overlay_muted),
            ))),
            Item::Error(t) => out.extend(wrap(
                t,
                width,
                fg(theme, |c| c.error),
                Span::styled("  ! ".to_owned(), fg(theme, |c| c.error)),
                "    ",
            )),
        }
    }
    out
}

// ---------------------------------------------------------------------
// The audit viewer

/// The agents' audit logs: a list of sessions, each checked, and the
/// entries of the one selected.
#[derive(Debug)]
pub struct AuditViewer {
    dir: PathBuf,
    logs: Vec<(PathBuf, String, bool)>,
    sel: usize,
    entries: Vec<audit::Entry>,
    scroll: u16,
    focus_entries: bool,
}

impl AuditViewer {
    pub fn open(dir: &Path, session: &str) -> Self {
        let mut v = Self {
            dir: dir.to_owned(),
            logs: Vec::new(),
            sel: 0,
            entries: Vec::new(),
            scroll: 0,
            focus_entries: false,
        };
        v.reload();
        if !session.is_empty()
            && let Some(i) = v
                .logs
                .iter()
                .position(|(p, _, _)| p.file_stem().is_some_and(|s| s == session))
        {
            v.sel = i;
            v.focus_entries = true;
        }
        v.load_entries();
        v
    }

    fn reload(&mut self) {
        let hist = history::dir(self.dir.parent().unwrap_or(&self.dir));
        self.logs = audit::list(&self.dir)
            .into_iter()
            .map(|p| {
                let id = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let head = Saved::load(&hist, &id).ok().and_then(|s| s.audit_head);
                let (status, ok) = match audit::verify_with_head(&p, head.as_deref()) {
                    Ok(v) => (format!("✓ {} entries", v.entries), true),
                    Err(e) => (format!("✗ {e}"), false),
                };
                (p, status, ok)
            })
            .collect();
    }

    fn load_entries(&mut self) {
        self.entries = self
            .logs
            .get(self.sel)
            .and_then(|(p, _, _)| audit::read(p).ok())
            .unwrap_or_default();
        self.scroll = 0;
    }
}

impl App for AuditViewer {
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Audit
    }

    fn title(&self) -> String {
        "Agent audit logs".into()
    }

    fn handle_key(&mut self, k: KeyEvent) -> AppEffect {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => AppEffect::Close,
            KeyCode::Tab | KeyCode::Enter => {
                self.focus_entries = !self.focus_entries;
                AppEffect::Redraw
            }
            KeyCode::Char('r') => {
                self.reload();
                self.load_entries();
                AppEffect::Redraw
            }
            KeyCode::Up | KeyCode::Char('k') if self.focus_entries => {
                self.scroll = self.scroll.saturating_sub(1);
                AppEffect::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus_entries => {
                self.scroll = self.scroll.saturating_add(1);
                AppEffect::Redraw
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.sel = self.sel.saturating_sub(1);
                self.load_entries();
                AppEffect::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1).min(self.logs.len().saturating_sub(1));
                self.load_entries();
                AppEffect::Redraw
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        if area.width < 10 || area.height < 4 {
            return;
        }
        let inner = TuiRect::new(area.x + 1, area.y, area.width - 2, area.height);
        let list_h = (inner.height / 3)
            .max(3)
            .min(inner.height.saturating_sub(2));
        let mut lines = vec![Line::from(Span::styled(
            format!(
                "{} logs in {} (r re-checks · Tab entries · q closes)",
                self.logs.len(),
                self.dir.display()
            ),
            style(theme, |t| t.overlay_title),
        ))];
        if self.logs.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no agent sessions yet".to_owned(),
                style(theme, |t| t.overlay_muted),
            )));
        }
        let start = self
            .sel
            .saturating_sub(usize::from(list_h.saturating_sub(2)));
        for (i, (p, status, ok)) in self.logs.iter().enumerate().skip(start) {
            let id = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let st = if i == self.sel && !self.focus_entries {
                style(theme, |t| t.overlay_selected)
            } else if *ok {
                fg(theme, |c| c.fg)
            } else {
                fg(theme, |c| c.error)
            };
            lines.push(Line::from(Span::styled(format!("  {id}  {status}"), st)));
        }
        draw_lines(
            buf,
            TuiRect::new(inner.x, inner.y, inner.width, list_h),
            &lines,
            0,
        );
        let rest = TuiRect::new(
            inner.x,
            inner.y + list_h,
            inner.width,
            inner.height - list_h,
        );
        put_line(
            buf,
            rest,
            0,
            &Line::from(Span::styled(
                "─".repeat(usize::from(rest.width)),
                style(theme, |t| t.border),
            )),
        );
        let mut lines: Vec<Line<'static>> = Vec::new();
        for e in &self.entries {
            let time: String = e.time.chars().skip(11).take(8).collect();
            let (text, st) = match &e.event {
                audit::Event::Session {
                    profile,
                    provider,
                    model,
                    workspace,
                    sandbox,
                } => (
                    format!("session  {profile} · {provider} {model} · {workspace} · {sandbox}"),
                    fg(theme, |c| c.accent),
                ),
                audit::Event::Tool {
                    tool,
                    args,
                    decision,
                    reason,
                    result,
                    ok,
                    ..
                } => {
                    let args = args.to_string();
                    let args: String = args.chars().take(120).collect();
                    let mut t = format!("{decision:<9} {tool} {args}  ({reason})");
                    if let Some(r) = result {
                        t.push_str(&format!(" → {}", r.replace('\t', " ")));
                    }
                    let st = match (decision.as_str(), ok) {
                        ("denied" | "refused" | "invalid" | "cancelled", _) | (_, Some(false)) => {
                            fg(theme, |c| c.warn)
                        }
                        _ => fg(theme, |c| c.fg),
                    };
                    (t, st)
                }
                audit::Event::Note { text } => (
                    format!("note     {text}"),
                    style(theme, |t| t.overlay_muted),
                ),
            };
            lines.extend(wrap(
                &text,
                rest.width,
                st,
                Span::styled(format!("{time} "), style(theme, |t| t.overlay_muted)),
                "         ",
            ));
        }
        let body = TuiRect::new(
            rest.x,
            rest.y + 1,
            rest.width,
            rest.height.saturating_sub(1),
        );
        let max = u16::try_from(lines.len())
            .unwrap_or(u16::MAX)
            .saturating_sub(body.height);
        self.scroll = self.scroll.min(max);
        draw_lines(buf, body, &lines, usize::from(self.scroll));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_and_the_input_line() {
        let st = TuiStyle::default();
        let lines = wrap("one two three four", 12, st, Span::raw("› "), "  ");
        let text: Vec<String> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(text, ["› one two", "  three four"]);
        let mut e = LineEdit::default();
        for c in "héllo".chars() {
            e.key(KeyEvent::plain(KeyCode::Char(c)));
        }
        e.key(KeyEvent::plain(KeyCode::Left));
        e.key(KeyEvent::plain(KeyCode::Backspace));
        assert_eq!(e.text, "hélo");
        assert_eq!(e.view(20, true).0, "••••");
        e.key(KeyEvent::ctrl('u'));
        assert!(e.text.is_empty());
    }
}
