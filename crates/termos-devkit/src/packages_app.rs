// SPDX-License-Identifier: GPL-3.0-or-later
//! The package manager app (`packages.open`, M5b, D-110): a tab for each
//! source ([`crate::packages`]): mise, the dev rootfs's apt, the host's
//! package manager and Podman. Each lists what is installed and searches
//! for more; installing, removing and upgrading open a window running
//! `termos pkg …`, where apt's questions or sudo's password are answered,
//! and the list follows when the source's package database changes.
//!
//! Listing and searching run on their own threads; nothing runs while the
//! window is idle (D-028).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, AppWaker, tui_style};
use termos_core::input::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

use crate::packages::{Backend, Kind, Package, Source, SourceInfo, changed_at};

/// A list being fetched, or fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Load {
    Idle,
    Loading,
    Ready(Vec<Package>),
    Failed(String),
}

#[derive(Debug)]
struct Tab {
    info: SourceInfo,
    installed: Load,
    /// The last search: its term and what it found.
    search: Option<(String, Load)>,
    /// The search's results are shown (else what is installed).
    results: bool,
    /// The watched files' times when the list was made.
    seen: Vec<Option<SystemTime>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Install,
    Remove,
    Upgrade,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Browse,
    /// Typing a search.
    Search(String),
    /// Asking before running `termos pkg`.
    Confirm(Verb, Option<Package>),
}

#[derive(Debug, Clone)]
enum Job {
    Installed,
    Search(String),
}

/// Work finished on another thread, for [`App::catch_up`].
struct Done {
    source: Source,
    job: Job,
    result: Result<Vec<Package>, String>,
    seen: Vec<Option<SystemTime>>,
}

pub struct PackagesApp {
    backend: Arc<dyn Backend>,
    tabs: Vec<Tab>,
    current: usize,
    selected: usize,
    top: usize,
    rows: u16,
    mode: Mode,
    inbox: Arc<Mutex<Vec<Done>>>,
    /// Set by the desktop after the first listing may have started, so
    /// workers read it when they finish.
    waker: Arc<Mutex<Option<AppWaker>>>,
    pending: usize,
    /// Run jobs on the caller's thread (tests).
    inline: bool,
    message: Option<String>,
    /// Where each tab's title was drawn: (x, width).
    tab_spans: Vec<(u16, u16)>,
}

impl std::fmt::Debug for PackagesApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackagesApp")
            .field("current", &self.current)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

fn fit(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]) as &str);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

fn width(text: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX)
}

fn watched(backend: &dyn Backend, source: Source) -> Vec<Option<SystemTime>> {
    backend
        .watch(source)
        .iter()
        .map(|p: &PathBuf| changed_at(p))
        .collect()
}

impl PackagesApp {
    /// The app over `backend`, on the first source that can be used (or
    /// `first`).
    pub fn open(backend: Arc<dyn Backend>, first: Option<Source>) -> Self {
        Self::with(backend, first, false)
    }

    fn with(backend: Arc<dyn Backend>, first: Option<Source>, inline: bool) -> Self {
        let tabs: Vec<Tab> = backend
            .sources()
            .into_iter()
            .map(|info| Tab {
                info,
                installed: Load::Idle,
                search: None,
                results: false,
                seen: Vec::new(),
            })
            .collect();
        let current = first
            .and_then(|s| tabs.iter().position(|t| t.info.source == s))
            .or_else(|| tabs.iter().position(|t| t.info.unavailable.is_none()))
            .unwrap_or(0);
        let mut app = PackagesApp {
            backend,
            tabs,
            current,
            selected: 0,
            top: 0,
            rows: 0,
            mode: Mode::Browse,
            inbox: Arc::new(Mutex::new(Vec::new())),
            waker: Arc::new(Mutex::new(None)),
            pending: 0,
            inline,
            message: None,
            tab_spans: Vec::new(),
        };
        app.show(current);
        app
    }

    /// Shows tab `i`, listing it the first time.
    fn show(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.current = i;
        self.selected = 0;
        self.top = 0;
        self.message = None;
        let tab = &self.tabs[i];
        if tab.info.unavailable.is_none() && tab.installed == Load::Idle {
            self.start(i, Job::Installed);
        }
    }

    fn start(&mut self, i: usize, job: Job) {
        let Some(tab) = self.tabs.get_mut(i) else {
            return;
        };
        let source = tab.info.source;
        match &job {
            Job::Installed => tab.installed = Load::Loading,
            Job::Search(term) => {
                tab.search = Some((term.clone(), Load::Loading));
                tab.results = true;
            }
        }
        let backend = Arc::clone(&self.backend);
        let work = move || {
            let seen = watched(backend.as_ref(), source);
            let result = match &job {
                Job::Installed => backend.installed(source),
                Job::Search(term) => backend.search(source, term),
            };
            Done {
                source,
                job,
                result,
                seen,
            }
        };
        self.pending += 1;
        if self.inline {
            let done = work();
            self.apply(done);
            return;
        }
        let inbox = Arc::clone(&self.inbox);
        let waker = Arc::clone(&self.waker);
        std::thread::spawn(move || {
            let done = work();
            if let Ok(mut q) = inbox.lock() {
                q.push(done);
            }
            if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                w.wake();
            }
        });
    }

    fn apply(&mut self, done: Done) {
        self.pending = self.pending.saturating_sub(1);
        let Some(tab) = self.tabs.iter_mut().find(|t| t.info.source == done.source) else {
            return;
        };
        let load = match done.result {
            Ok(list) => Load::Ready(list),
            Err(e) => Load::Failed(e),
        };
        match done.job {
            Job::Installed => {
                tab.installed = load;
                tab.seen = done.seen;
            }
            Job::Search(term) => {
                // A newer search replaced this one.
                if tab.search.as_ref().is_some_and(|(t, _)| *t == term) {
                    tab.search = Some((term, load));
                }
            }
        }
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.current]
    }

    /// The packages shown.
    fn shown(&self) -> &[Package] {
        let tab = self.tab();
        let load = if tab.results {
            tab.search.as_ref().map(|(_, l)| l)
        } else {
            Some(&tab.installed)
        };
        match load {
            Some(Load::Ready(list)) => list,
            _ => &[],
        }
    }

    fn installed_names(&self) -> Vec<&str> {
        match &self.tab().installed {
            Load::Ready(list) => list.iter().map(|p| p.name.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    fn is_installed(&self, p: &Package) -> bool {
        p.kind == Kind::Container || self.installed_names().contains(&p.name.as_str())
    }

    fn select(&mut self, i: usize) -> AppEffect {
        let i = i.min(self.shown().len().saturating_sub(1));
        if i == self.selected {
            return AppEffect::None;
        }
        self.selected = i;
        AppEffect::Redraw
    }

    fn switch(&mut self, forward: bool) -> AppEffect {
        let n = self.tabs.len();
        if n == 0 {
            return AppEffect::None;
        }
        let i = if forward {
            (self.current + 1) % n
        } else {
            (self.current + n - 1) % n
        };
        self.show(i);
        AppEffect::Redraw
    }

    /// The desktop command that runs `verb` in a window.
    fn command(&self, verb: Verb, package: Option<&Package>) -> String {
        let source = self.tab().info.source.id();
        match (verb, package) {
            (Verb::Install, Some(p)) => format!("pkg.run install {source} {}", p.name),
            (Verb::Remove, Some(p)) if p.kind == Kind::Container => {
                format!("pkg.run remove {source} {} --container", p.name)
            }
            (Verb::Remove, Some(p)) => format!("pkg.run remove {source} {}", p.name),
            _ => format!("pkg.run upgrade {source}"),
        }
    }

    fn reference(p: &Package) -> String {
        // An image is pulled and removed by its full reference.
        if p.kind == Kind::Image && !p.version.is_empty() && !p.version.starts_with("sha256:") {
            format!("{}:{}", p.name, p.version)
        } else {
            p.name.clone()
        }
    }

    fn ask(&mut self, verb: Verb) -> AppEffect {
        let package = self.shown().get(self.selected).cloned().map(|mut p| {
            p.name = Self::reference(&p);
            p
        });
        match verb {
            Verb::Install | Verb::Remove if package.is_none() => return AppEffect::None,
            Verb::Install if package.as_ref().is_some_and(|p| self.is_installed(p)) => {
                self.message = Some("Already installed: d removes it.".to_owned());
                return AppEffect::Redraw;
            }
            Verb::Remove if package.as_ref().is_some_and(|p| !self.is_installed(p)) => {
                self.message = Some("Not installed: enter installs it.".to_owned());
                return AppEffect::Redraw;
            }
            Verb::Upgrade if self.tab().info.source == Source::Podman => {
                self.message = Some(
                    "Images update when pulled again: search for one and press enter.".to_owned(),
                );
                return AppEffect::Redraw;
            }
            _ => {}
        }
        self.mode = Mode::Confirm(verb, package);
        AppEffect::Redraw
    }

    fn browse_key(&mut self, key: KeyEvent) -> AppEffect {
        if key.mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER) {
            return AppEffect::None;
        }
        let page = usize::from(self.rows.max(2) - 1);
        if let Some(u) = &self.tab().info.unavailable
            && let Some((k, _, command)) = &u.action
            && key.code == KeyCode::Char(*k)
        {
            return AppEffect::Command(command.clone());
        }
        match key.code {
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.switch(true),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => self.switch(false),
            KeyCode::Char(c @ '1'..='9') => {
                let i = usize::from(c as u8 - b'1');
                if i < self.tabs.len() {
                    self.show(i);
                    AppEffect::Redraw
                } else {
                    AppEffect::None
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::PageDown => self.select(self.selected + page),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(page)),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            _ if self.tab().info.unavailable.is_some() => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
                _ => AppEffect::None,
            },
            KeyCode::Char('/' | 's') => {
                self.mode = Mode::Search(String::new());
                AppEffect::Redraw
            }
            KeyCode::Enter | KeyCode::Char('i') => {
                if self.tab().results {
                    self.ask(Verb::Install)
                } else {
                    AppEffect::None
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => self.ask(Verb::Remove),
            KeyCode::Char('u') => self.ask(Verb::Upgrade),
            KeyCode::Char('r') => {
                let i = self.current;
                self.start(i, Job::Installed);
                if let Some((term, _)) = self.tab().search.clone()
                    && self.tab().results
                {
                    self.start(i, Job::Search(term));
                }
                AppEffect::Redraw
            }
            KeyCode::Esc if self.tab().results => {
                let i = self.current;
                self.tabs[i].results = false;
                self.selected = 0;
                self.top = 0;
                AppEffect::Redraw
            }
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            _ => AppEffect::None,
        }
    }

    /// Lists the current tab again when its package database changed (an
    /// install in its window finished, or one outside TermOS).
    fn follow(&mut self) {
        let tab = self.tab();
        if !matches!(tab.installed, Load::Ready(_) | Load::Failed(_)) {
            return;
        }
        let now = watched(self.backend.as_ref(), tab.info.source);
        if !now.is_empty() && now != tab.seen {
            let i = self.current;
            self.start(i, Job::Installed);
        }
    }

    fn footer(&self) -> (String, Style, bool) {
        let tab = self.tab();
        match &self.mode {
            Mode::Search(term) => (
                format!("search {}: {term}▏", tab.info.title),
                Style::new(),
                false,
            ),
            Mode::Confirm(verb, p) => {
                let what = p.as_ref().map(|p| p.name.as_str()).unwrap_or("");
                let place = match tab.info.source {
                    Source::Mise => "with mise",
                    Source::Devroot => "in the dev rootfs",
                    Source::Host => "on this system",
                    Source::Podman => "with Podman",
                };
                let text = match verb {
                    Verb::Install => format!("Install {what} {place}? y/n"),
                    Verb::Remove => format!("Remove {what} {place}? y/n"),
                    Verb::Upgrade => format!("Upgrade everything {place}? y/n"),
                };
                (text, Style::new().attrs(Attrs::BOLD), true)
            }
            Mode::Browse => {
                if let Some(m) = &self.message {
                    return (m.clone(), Style::new(), true);
                }
                let text = match &tab.info.unavailable {
                    Some(u) => match &u.action {
                        Some((k, label, _)) => format!("{k} {label} · tab next source · q close"),
                        None => "tab next source · q close".to_owned(),
                    },
                    None if tab.results => {
                        "enter install · d remove · / search · esc back · tab source · q close"
                            .to_owned()
                    }
                    None => "/ search · d remove · u upgrade all · r reload · tab source · q close"
                        .to_owned(),
                };
                (text, Style::new(), false)
            }
        }
    }
}

impl App for PackagesApp {
    fn title(&self) -> String {
        "Packages".to_owned()
    }

    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Archive
    }

    fn reopen(&self) -> Option<String> {
        Some("packages.open".to_owned())
    }

    fn busy(&self) -> bool {
        self.pending > 0
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut slot) = self.waker.lock() {
            *slot = Some(waker.clone());
        }
        // Work that finished before there was a waker.
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
        self.selected = self.selected.min(self.shown().len().saturating_sub(1));
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        match self.mode.clone() {
            Mode::Browse => {
                let had = self.message.take().is_some();
                match self.browse_key(key) {
                    AppEffect::None if had => AppEffect::Redraw,
                    e => e,
                }
            }
            Mode::Search(mut term) => {
                match key.code {
                    KeyCode::Esc => self.mode = Mode::Browse,
                    KeyCode::Enter => {
                        self.mode = Mode::Browse;
                        let term = term.trim().to_owned();
                        if !term.is_empty() {
                            let i = self.current;
                            self.selected = 0;
                            self.top = 0;
                            self.start(i, Job::Search(term));
                        }
                    }
                    KeyCode::Backspace => {
                        term.pop();
                        self.mode = Mode::Search(term);
                    }
                    KeyCode::Char(c) if !key.mods.intersects(Mods::CTRL | Mods::ALT) => {
                        term.push(c);
                        self.mode = Mode::Search(term);
                    }
                    _ => return AppEffect::None,
                }
                AppEffect::Redraw
            }
            Mode::Confirm(verb, package) => {
                self.mode = Mode::Browse;
                if !matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter) {
                    return AppEffect::Redraw;
                }
                let what = package.as_ref().map(|p| p.name.clone()).unwrap_or_default();
                self.message = Some(match verb {
                    Verb::Install => format!("Installing {what} in its window…"),
                    Verb::Remove => format!("Removing {what} in its window…"),
                    Verb::Upgrade => "Upgrading in its window…".to_owned(),
                });
                AppEffect::Command(self.command(verb, package.as_ref()))
            }
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        match event.kind {
            MouseKind::ScrollDown => self.select(self.selected + 3),
            MouseKind::ScrollUp => self.select(self.selected.saturating_sub(3)),
            MouseKind::Down(MouseButton::Left) if event.y == 0 => {
                let hit = self
                    .tab_spans
                    .iter()
                    .position(|&(x, w)| event.x >= x && event.x < x + w);
                match hit {
                    Some(i) if i != self.current => {
                        self.show(i);
                        AppEffect::Redraw
                    }
                    _ => AppEffect::None,
                }
            }
            MouseKind::Down(MouseButton::Left) if event.y >= 2 => {
                let i = self.top + usize::from(event.y - 2);
                if i >= self.shown().len() || self.mode != Mode::Browse {
                    AppEffect::None
                } else if i == self.selected {
                    self.handle_key(KeyEvent::plain(KeyCode::Enter))
                } else {
                    self.select(i)
                }
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.follow();
        if area.height < 5 || area.width < 20 {
            return;
        }
        let c = &theme.colors;
        // The tabs.
        self.tab_spans.clear();
        let mut x = area.x + 1;
        for (i, tab) in self.tabs.iter().enumerate() {
            let label = format!(" {} ", tab.info.title);
            let w = width(&label);
            if x + w > area.right() {
                break;
            }
            let style = if i == self.current {
                Style::new().fg(c.fg).bg(c.selection_bg).attrs(Attrs::BOLD)
            } else if tab.info.unavailable.is_some() {
                Style::new().fg(c.muted)
            } else {
                Style::new().fg(c.fg)
            };
            buf.set_string(x, area.y, &label, tui_style(style));
            self.tab_spans.push((x - area.x, w));
            x += w + 1;
        }
        // What is shown.
        let w = usize::from(area.width - 2);
        let line = |buf: &mut Buffer, y: u16, text: &str, style: Style| {
            buf.set_stringn(area.x + 1, y, fit(text, w), w, tui_style(style));
        };
        let list_y = area.y + 2;
        let list_h = area.height - 3;
        self.rows = list_h;
        let tab = self.tab();
        if let Some(u) = &tab.info.unavailable {
            line(
                buf,
                area.y + 1,
                "Not available here",
                Style::new().fg(c.muted),
            );
            line(buf, list_y, &u.reason, Style::new().fg(c.fg));
        } else {
            let (heading, load) = if tab.results {
                let (term, load) = tab
                    .search
                    .as_ref()
                    .map_or(("", &Load::Idle), |(t, l)| (t.as_str(), l));
                let heading = match load {
                    Load::Ready(l) => format!("Search \"{term}\": {} found", l.len()),
                    _ => format!("Search \"{term}\""),
                };
                (heading, load)
            } else {
                let heading = match &tab.installed {
                    Load::Ready(l) => format!("Installed: {}", l.len()),
                    _ => "Installed".to_owned(),
                };
                (heading, &tab.installed)
            };
            line(buf, area.y + 1, &heading, Style::new().fg(c.muted));
            match load {
                Load::Idle | Load::Loading => {
                    line(buf, list_y, "Listing…", Style::new().fg(c.muted));
                }
                Load::Failed(e) => line(
                    buf,
                    list_y,
                    &format!("Cannot list: {e}"),
                    Style::new().fg(c.warn),
                ),
                Load::Ready(list) if list.is_empty() => {
                    let text = if tab.results {
                        "Nothing found."
                    } else {
                        "Nothing installed yet: / searches for packages."
                    };
                    line(buf, list_y, text, Style::new().fg(c.muted));
                }
                Load::Ready(_) => {}
            }
        }
        // The list: name, version, summary.
        let shown: Vec<Package> = self.shown().to_vec();
        let installed: Vec<String> = self
            .installed_names()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let results = self.tab().results;
        let rows = usize::from(list_h);
        if self.selected < self.top {
            self.top = self.selected;
        } else if rows > 0 && self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        }
        let max_name = shown
            .iter()
            .map(|p| UnicodeWidthStr::width(p.name.as_str()))
            .max()
            .unwrap_or(0);
        let max_ver = shown
            .iter()
            .map(|p| UnicodeWidthStr::width(p.version.as_str()))
            .max()
            .unwrap_or(0);
        let name_w = max_name.min(w * 2 / 5).max(4);
        let ver_w = max_ver.min(w / 4);
        for (i, p) in shown.iter().enumerate().skip(self.top).take(rows) {
            let y = list_y + u16::try_from(i - self.top).unwrap_or(0);
            let selected = i == self.selected;
            let bg = |s: Style| if selected { c.selected(s) } else { s };
            if selected {
                buf.set_style(
                    TuiRect::new(area.x, y, area.width, 1),
                    tui_style(Style::new().bg(c.selection_bg)),
                );
            }
            let mark = if results && (p.kind == Kind::Container || installed.contains(&p.name)) {
                "●"
            } else {
                " "
            };
            buf.set_string(area.x, y, mark, tui_style(bg(Style::new().fg(c.accent))));
            let mut x = area.x + 1;
            buf.set_stringn(
                x,
                y,
                fit(&p.name, name_w),
                name_w,
                tui_style(bg(Style::new().fg(c.fg))),
            );
            x += u16::try_from(name_w + 2).unwrap_or(0);
            if ver_w > 0 && x < area.right() {
                buf.set_stringn(
                    x,
                    y,
                    fit(&p.version, ver_w),
                    ver_w,
                    tui_style(bg(Style::new().fg(c.muted))),
                );
                x += u16::try_from(ver_w + 2).unwrap_or(0);
            }
            if x < area.right() {
                let rest = usize::from(area.right() - x);
                buf.set_stringn(
                    x,
                    y,
                    fit(&p.summary, rest),
                    rest,
                    tui_style(bg(Style::new().fg(c.muted))),
                );
            }
        }
        // Keys, a question, or what just happened.
        let (text, style, strong) = self.footer();
        let style = if strong {
            style.fg(c.warn)
        } else {
            style.fg(c.muted)
        };
        line(buf, area.bottom() - 1, &text, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::Unavailable;

    struct Fixture {
        watch: Option<PathBuf>,
        lists: Mutex<u32>,
    }

    fn p(name: &str, version: &str, summary: &str) -> Package {
        Package {
            name: name.to_owned(),
            version: version.to_owned(),
            summary: summary.to_owned(),
            kind: Kind::Package,
        }
    }

    impl Backend for Fixture {
        fn sources(&self) -> Vec<SourceInfo> {
            let info = |source, title: &str, unavailable| SourceInfo {
                source,
                title: title.to_owned(),
                unavailable,
            };
            vec![
                info(Source::Mise, "mise", None),
                info(Source::Devroot, "Dev rootfs (apt)", None),
                info(Source::Host, "Host (dnf)", None),
                info(
                    Source::Podman,
                    "Podman",
                    Some(Unavailable {
                        reason: "Podman is not installed (https://podman.io).".to_owned(),
                        action: None,
                    }),
                ),
            ]
        }

        fn installed(&self, source: Source) -> Result<Vec<Package>, String> {
            if let Ok(mut n) = self.lists.lock() {
                *n += 1;
            }
            match source {
                Source::Mise => Ok(vec![
                    p("go", "1.25.3", ""),
                    p("node", "22.11.0", "active"),
                    p("python", "3.13.9", "active"),
                ]),
                Source::Devroot => Ok(vec![
                    p("bash", "5.2.37-2", "GNU Bourne Again SHell"),
                    p("hello", "2.10-5", "example package based on GNU hello"),
                    p(
                        "zlib1g",
                        "1:1.3.dfsg+really1.3.1-1+b1",
                        "compression library - runtime",
                    ),
                ]),
                Source::Host => Err("rpm: cannot open the package database".to_owned()),
                Source::Podman => Ok(Vec::new()),
            }
        }

        fn search(&self, source: Source, term: &str) -> Result<Vec<Package>, String> {
            match (source, term) {
                (Source::Devroot, "hello") => Ok(vec![
                    p("hello", "", "example package based on GNU hello"),
                    p(
                        "hello-traditional",
                        "",
                        "the classic greeting, and a good example",
                    ),
                ]),
                (Source::Mise, "jq") => Ok(vec![p("jq", "", "aqua:jqlang/jq ubi:jqlang/jq")]),
                _ => Ok(Vec::new()),
            }
        }

        fn watch(&self, _source: Source) -> Vec<PathBuf> {
            self.watch.iter().cloned().collect()
        }
    }

    fn app() -> PackagesApp {
        PackagesApp::with(
            Arc::new(Fixture {
                watch: None,
                lists: Mutex::new(0),
            }),
            None,
            true,
        )
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::plain(KeyCode::Char(c))
    }

    fn render(app: &mut PackagesApp, w: u16, h: u16) -> String {
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

    fn search(app: &mut PackagesApp, term: &str) {
        let _ = app.handle_key(key('/'));
        for c in term.chars() {
            let _ = app.handle_key(key(c));
        }
        let _ = app.handle_key(KeyEvent::plain(KeyCode::Enter));
    }

    #[test]
    fn mise_tools_first() {
        let mut a = app();
        insta::assert_snapshot!("packages-mise", render(&mut a, 72, 9));
    }

    #[test]
    fn the_dev_rootfs_lists_and_installs_with_apt() {
        let mut a = app();
        let _ = a.handle_key(KeyEvent::plain(KeyCode::Tab));
        insta::assert_snapshot!("packages-devroot", render(&mut a, 72, 9));
        search(&mut a, "hello");
        insta::assert_snapshot!("packages-devroot-search", render(&mut a, 72, 9));
        // Installed already: it says so.
        assert_eq!(
            a.handle_key(KeyEvent::plain(KeyCode::Enter)),
            AppEffect::Redraw
        );
        assert!(render(&mut a, 72, 9).contains("Already installed"));
        let _ = a.handle_key(key('j'));
        assert_eq!(
            a.handle_key(KeyEvent::plain(KeyCode::Enter)),
            AppEffect::Redraw
        );
        insta::assert_snapshot!("packages-devroot-confirm", render(&mut a, 72, 9));
        assert_eq!(
            a.handle_key(key('y')),
            AppEffect::Command("pkg.run install devroot hello-traditional".to_owned())
        );
        // Back to what is installed; remove one.
        let _ = a.handle_key(KeyEvent::plain(KeyCode::Esc));
        let _ = a.handle_key(key('G'));
        let _ = a.handle_key(key('d'));
        assert_eq!(
            a.handle_key(key('y')),
            AppEffect::Command("pkg.run remove devroot zlib1g".to_owned())
        );
        let _ = a.handle_key(key('u'));
        assert_eq!(
            a.handle_key(key('n')),
            AppEffect::Redraw,
            "no: nothing runs"
        );
        let _ = a.handle_key(key('u'));
        assert_eq!(
            a.handle_key(key('y')),
            AppEffect::Command("pkg.run upgrade devroot".to_owned())
        );
    }

    #[test]
    fn a_mise_tool_is_found_and_installed() {
        let mut a = app();
        search(&mut a, "jq");
        let _ = a.handle_key(key('i'));
        assert_eq!(
            a.handle_key(key('y')),
            AppEffect::Command("pkg.run install mise jq".to_owned())
        );
    }

    #[test]
    fn failures_and_missing_sources_say_why() {
        let mut a = app();
        let _ = a.handle_key(key('3'));
        insta::assert_snapshot!("packages-host-failed", render(&mut a, 72, 6));
        let _ = a.handle_key(key('4'));
        insta::assert_snapshot!("packages-podman-missing", render(&mut a, 72, 6));
        assert_eq!(a.handle_key(key('/')), AppEffect::None, "nothing to search");
        assert_eq!(a.handle_key(key('q')), AppEffect::Close);
    }

    #[test]
    fn tabs_are_clickable() {
        let mut a = app();
        let _ = render(&mut a, 72, 9);
        let (x, _) = a.tab_spans[1];
        let click = MouseEvent {
            kind: MouseKind::Down(MouseButton::Left),
            x: x + 1,
            y: 0,
            mods: Mods::empty(),
        };
        assert_eq!(a.handle_mouse(click), AppEffect::Redraw);
        assert_eq!(a.tab().info.source, Source::Devroot);
    }

    #[test]
    fn the_list_follows_its_package_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = dir.path().join("status");
        std::fs::write(&status, "a").expect("write");
        let fixture = Arc::new(Fixture {
            watch: Some(status.clone()),
            lists: Mutex::new(0),
        });
        let mut a = PackagesApp::with(Arc::clone(&fixture) as Arc<dyn Backend>, None, true);
        let lists = || fixture.lists.lock().map(|n| *n).unwrap_or(0);
        assert_eq!(lists(), 1);
        let _ = render(&mut a, 72, 9);
        assert_eq!(lists(), 1, "nothing changed");
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&status)
            .and_then(|f| f.set_modified(later))
            .expect("touch");
        let _ = render(&mut a, 72, 9);
        assert_eq!(lists(), 2, "listed again");
        let _ = render(&mut a, 72, 9);
        assert_eq!(lists(), 2);
    }
}
