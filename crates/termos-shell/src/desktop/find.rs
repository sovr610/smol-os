// SPDX-License-Identifier: GPL-3.0-or-later
//! What is on screen, acted on (M13a, D-118): Ctrl+click and Ctrl-hover on
//! links and paths, opening them, hint mode, and the search across panes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use termos_core::clipboard::ClipSource;
use termos_core::ids::{ClientId, PaneId};
use termos_core::input::{KeyEvent, Mods, MouseEvent};
use termos_core::proto::ServerMsg;
use termos_term::links::{self, Match, Target};
use termos_term::pty::PtyEvent;
use termos_wm::hit::Hit;

use super::{Desktop, Pane, PaneKind, SessionRequest};
use crate::copymode::CopyMode;
use crate::hints::{HintAction, HintMode, Hints};
use crate::notify::Level;
use crate::search::{Hit as Found, PaneText, SearchAction, SearchBox};

/// What the KMS console calls its terminal (`Caps::term`).
pub(super) const CONSOLE_TERM: &str = "termos-kms";

/// Paths looked for are remembered this long.
const PATH_FRESH: Duration = Duration::from_secs(10);

/// At most this many paths are remembered.
const PATH_CACHE: usize = 512;

/// The longest text a toast quotes.
const QUOTE: usize = 60;

impl Desktop {
    /// The folder a pane's relative paths are in: what its shell reported
    /// (OSC 7), else its program's.
    pub(super) fn pane_dir(&self, id: PaneId) -> Option<PathBuf> {
        let pane = self.panes.get(&id)?;
        match &pane.kind {
            PaneKind::Term { pty, .. } => pane.cwd.clone().or_else(|| pty.foreground_cwd()),
            PaneKind::App(_) => None,
        }
    }

    /// Links, paths, hashes and addresses in a pane's view; with `at`
    /// (column, row in the pane), only what covers that cell.
    fn find_in_pane(&mut self, id: PaneId, at: Option<(u16, u16)>) -> Vec<Match> {
        let base = self.pane_dir(id);
        let Some(Pane {
            kind: PaneKind::Term { vt, .. },
            ..
        }) = self.panes.get(&id)
        else {
            return Vec::new();
        };
        let rows = vt.view_rows();
        let cache = &mut self.path_cache;
        let now = Instant::now();
        let mut exists = |p: &Path| -> bool {
            if let Some((found, at)) = cache.get(p)
                && now.saturating_duration_since(*at) < PATH_FRESH
            {
                return *found;
            }
            if cache.len() >= PATH_CACHE {
                cache.clear();
            }
            let found = p.exists();
            cache.insert(p.to_path_buf(), (found, now));
            found
        };
        links::find(&rows, base.as_deref(), at, &mut exists)
    }

    /// The pane and the cell in it under a screen cell.
    fn pane_cell(&self, x: u16, y: u16) -> Option<(PaneId, u16, u16)> {
        match self.placement().hit(x, y) {
            Hit::Pane { pane, x, y } => Some((pane, x, y)),
            _ => None,
        }
    }

    /// Ctrl+click (Ctrl+Shift over a program that reads the mouse) on a
    /// link or a path that exists opens it. Whether it did.
    pub(super) fn link_click(&mut self, m: &MouseEvent) -> bool {
        if !m.mods.contains(Mods::CTRL) {
            return false;
        }
        let Some((pane, x, y)) = self.pane_cell(m.x, m.y) else {
            return false;
        };
        if self.pane_wants_mouse(pane, m) {
            return false;
        }
        let Some(found) = self.find_in_pane(pane, Some((x, y))).into_iter().next() else {
            return false;
        };
        self.ui.link_hover = None;
        self.dirty = true;
        self.open_match(pane, &found);
        true
    }

    /// The pointer moved: with Ctrl held, the link or path under it is
    /// underlined; on the console a program's link is, without Ctrl too.
    pub(super) fn link_hover_moved(&mut self, m: &MouseEvent) {
        let next = self.hover_target(m);
        if next != self.ui.link_hover {
            self.ui.link_hover = next;
            self.dirty = true;
        }
    }

    fn hover_target(&mut self, m: &MouseEvent) -> Option<super::LinkCells> {
        let ctrl = m.mods.contains(Mods::CTRL);
        let console = self.ui.caps.term == CONSOLE_TERM;
        if !ctrl && !console {
            return None;
        }
        let (pane, x, y) = self.pane_cell(m.x, m.y)?;
        if self.pane_wants_mouse(pane, m) {
            return None;
        }
        // Still on the same link: nothing to look up.
        if let Some((p, cells)) = &self.ui.link_hover
            && *p == pane
            && cells
                .iter()
                .any(|(row, x0, x1)| *row == y && (*x0..*x1).contains(&x))
        {
            return self.ui.link_hover.clone();
        }
        let printed = matches!(
            self.panes.get(&pane),
            Some(Pane { kind: PaneKind::Term { vt, .. }, .. }) if vt.link_at(x, y).is_some()
        );
        if !ctrl && !printed {
            return None;
        }
        let found = if ctrl {
            self.find_in_pane(pane, Some((x, y)))
        } else {
            // A program's link: no file system needed.
            match self.panes.get(&pane) {
                Some(Pane {
                    kind: PaneKind::Term { vt, .. },
                    ..
                }) => links::find(&vt.view_rows(), None, Some((x, y)), &mut |_| false),
                _ => Vec::new(),
            }
        };
        found.into_iter().next().map(|f| (pane, f.cells))
    }

    /// Opens what a link or a hint marks: an address on the client's
    /// machine, a path in TermOS, other text copied.
    pub(super) fn open_match(&mut self, _pane: PaneId, m: &Match) {
        match &m.target {
            Target::Url(url) => self.open_url(url),
            Target::Path { path, line, column } => self.open_path(path, *line, *column),
            Target::Text(text) => {
                self.copy_text(text.clone(), ClipSource::Selection);
                self.notify(
                    Level::Info,
                    format!("copied {}", crate::attention::short(text, QUOTE)),
                );
            }
        }
    }

    /// A web or mail address opens on the client's machine; on the
    /// console, or over SSH, where TermOS cannot open it for you, it is
    /// copied. A `file:` address on this machine opens in TermOS.
    pub(super) fn open_url(&mut self, url: &str) {
        let scheme = termos_render::link::scheme(url).unwrap_or_default();
        if scheme == "file" {
            match links::file_url(url) {
                Some((host, path))
                    if links::is_local_host(&host, links::local_host().as_deref()) =>
                {
                    self.open_path(&path, None, None);
                }
                Some((host, _)) => {
                    self.copy_text(url.to_owned(), ClipSource::Selection);
                    self.notify(
                        Level::Info,
                        format!("the file is on {host}: copied its address"),
                    );
                }
                None => self.notify(Level::Warn, "not a file address TermOS can open"),
            }
            return;
        }
        if !matches!(scheme.as_str(), "http" | "https" | "mailto")
            || !termos_render::link::allowed(url)
        {
            self.notify(
                Level::Warn,
                "TermOS opens only http, https, mailto and file addresses",
            );
            return;
        }
        let shown = crate::attention::short(url, QUOTE);
        let why = if self.current.is_none() {
            Some("no terminal is attached")
        } else if self.ui.caps.term == CONSOLE_TERM {
            Some("there is no browser on the console")
        } else if self.ui.caps.remote {
            Some("your terminal is over SSH: open it from there")
        } else {
            None
        };
        match why {
            Some(why) => {
                self.copy_text(url.to_owned(), ClipSource::Selection);
                self.notify(Level::Info, format!("copied {shown} ({why})"));
            }
            None => {
                self.outbox.push((
                    self.current,
                    ServerMsg::Open {
                        url: url.to_owned(),
                    },
                ));
                self.notify(Level::Info, format!("opening {shown}"));
            }
        }
    }

    /// A folder in Files, a picture or a document in its viewer, other
    /// files in the editor; with a line, the editor at that line.
    fn open_path(&mut self, path: &Path, line: Option<u32>, column: Option<u32>) {
        let shown = path.display().to_string();
        match line {
            Some(l) => {
                let at = match column {
                    Some(c) => format!("{shown}:{l}:{c}"),
                    None => format!("{shown}:{l}"),
                };
                self.run(crate::commands::Command::FileEdit(at));
            }
            None => self.run(crate::commands::Command::FileOpen(shown)),
        }
    }

    /// `text` typed into `pane` as a paste.
    pub(super) fn paste_into(&mut self, pane: PaneId, text: &str) {
        let bytes = match self.panes.get(&pane) {
            Some(Pane {
                kind: PaneKind::Term { vt, .. },
                ..
            }) => termos_term::input::encode_paste(text, &vt.modes()),
            _ => return,
        };
        self.write_to_pane(pane, bytes);
    }

    // ----- hint mode ----------------------------------------------------

    /// `pane.hints KIND`: labels on what the focused pane shows.
    pub(super) fn start_hints(&mut self, kind: &str) {
        let Some(id) = self.focused() else {
            return;
        };
        if !matches!(
            self.panes.get(&id).map(|p| &p.kind),
            Some(PaneKind::Term { .. })
        ) {
            self.notify(Level::Info, "hints work in terminal panes");
            return;
        }
        // Files may have come and gone since the last look.
        self.path_cache.clear();
        let found: Vec<Match> = self
            .find_in_pane(id, None)
            .into_iter()
            .filter(|m| m.kind.in_filter(kind))
            .collect();
        let alphabet = self.config.hints.alphabet.clone();
        match Hints::new(id, found, &alphabet) {
            Some(h) => self.ui.hints = Some(h),
            None => {
                let what = if kind.is_empty() {
                    "link or path"
                } else {
                    kind
                };
                self.notify(Level::Info, format!("no {what} to label here"));
            }
        }
        self.ui.link_hover = None;
        self.dirty = true;
    }

    /// A key in hint mode.
    pub(super) fn hint_key(&mut self, k: KeyEvent) {
        let Some(h) = self.ui.hints.as_mut() else {
            return;
        };
        let pane = h.pane;
        let action = h.handle_key(k);
        self.dirty = true;
        match action {
            HintAction::None => {}
            HintAction::Exit => self.ui.hints = None,
            HintAction::Act(mode, m) => {
                self.ui.hints = None;
                match mode {
                    HintMode::Open => self.open_match(pane, &m),
                    HintMode::Copy => {
                        self.copy_text(m.text.clone(), ClipSource::Selection);
                        self.notify(
                            Level::Info,
                            format!("copied {}", crate::attention::short(&m.text, QUOTE)),
                        );
                    }
                    HintMode::Paste => self.paste_into(pane, &m.text),
                }
            }
        }
    }

    // ----- the search across panes ----------------------------------------

    /// The text of every terminal pane, for a search.
    pub fn search_texts(&self) -> Vec<PaneText> {
        let mut out = Vec::new();
        for (i, ws) in self.session.workspaces.iter().enumerate() {
            for id in ws.panes() {
                let Some(pane) = self.panes.get(&id) else {
                    continue;
                };
                let PaneKind::Term { vt, .. } = &pane.kind else {
                    continue;
                };
                out.push(PaneText {
                    session: self.session.name.clone(),
                    pane: id,
                    label: format!("{i}:{} · {}", ws.name, pane.display_name()),
                    cols: vt.size().0,
                    lines: vt.logical_lines_at(),
                });
            }
        }
        out
    }

    /// `search.all`: the box over this session's panes, or asks the
    /// server for every session's.
    pub(super) fn start_search(&mut self, all_sessions: bool) {
        if all_sessions {
            match self.current {
                Some(client) => self.requests.push(SessionRequest::SearchAll { client }),
                None => self.notify(Level::Warn, "searching every session needs a terminal"),
            }
            return;
        }
        let texts = self.search_texts();
        self.open_search(texts, false);
    }

    /// Opens the search box for the client being served over `texts`.
    pub(super) fn open_search(&mut self, texts: Vec<PaneText>, all_sessions: bool) {
        self.ui.hints = None;
        self.ui.palette = None;
        self.ui.search = Some(SearchBox::new(texts, all_sessions));
        self.dirty = true;
    }

    /// The search box of `client`, over every session's panes (the server
    /// gathered them).
    pub fn open_search_for(&mut self, client: ClientId, texts: Vec<PaneText>) {
        if !self.enter(client) {
            return;
        }
        self.open_search(texts, true);
        self.leave();
    }

    /// The pane the search thread wakes the desktop through.
    fn search_waker(&mut self) -> PaneId {
        match self.search_wake {
            Some(id) => id,
            None => {
                let id = self.alloc_id();
                self.search_wake = Some(id);
                id
            }
        }
    }

    pub(super) fn is_search_wake(&self, id: PaneId) -> bool {
        self.search_wake == Some(id)
    }

    /// A search finished: every client's box takes in its results.
    pub(super) fn search_woke(&mut self) {
        let mut changed = false;
        for ui in self.clients.values_mut() {
            if let Some(s) = ui.search.as_mut() {
                changed |= s.poll();
            }
        }
        if let Some(s) = self.ui.search.as_mut() {
            changed |= s.poll();
        }
        if changed {
            self.dirty = true;
        }
    }

    fn search_again(&mut self) {
        let id = self.search_waker();
        let wake = self.pty_tx.clone();
        if let Some(s) = self.ui.search.as_mut() {
            s.start(move || {
                // A full queue already has a wakeup in it.
                let _ = wake.try_send(PtyEvent::Wake(id));
            });
        }
    }

    /// A key while the search box is open.
    pub(super) fn search_key(&mut self, k: KeyEvent) {
        let Some(s) = self.ui.search.as_mut() else {
            return;
        };
        let action = s.handle_key(k);
        self.dirty = true;
        self.search_action(action);
    }

    /// Text pasted while the search box is open.
    pub(super) fn search_paste(&mut self, text: &str) {
        if let Some(s) = self.ui.search.as_mut() {
            let action = s.insert_str(text);
            self.search_action(action);
        }
    }

    fn search_action(&mut self, action: SearchAction) {
        match action {
            SearchAction::None => {}
            SearchAction::Close => self.ui.search = None,
            SearchAction::Search => self.search_again(),
            SearchAction::Go(hit) => {
                self.ui.search = None;
                if hit.session == self.session.name {
                    self.reveal(&hit);
                } else if let Some(client) = self.current {
                    self.requests.push(SessionRequest::Reveal { client, hit });
                }
            }
        }
    }

    /// Goes to a match: its pane gets focus and copy mode, at its line,
    /// with the match selected.
    pub fn reveal(&mut self, hit: &Found) {
        if let Err(e) = self.focus_pane(Some(hit.pane)) {
            self.notify(Level::Warn, e);
            return;
        }
        let Some(Pane {
            kind: PaneKind::Term { vt, .. },
            ..
        }) = self.panes.get_mut(&hit.pane)
        else {
            return;
        };
        let mut cm = CopyMode::new(hit.pane, vt.as_ref());
        let end = hit.col.saturating_add(hit.width.saturating_sub(1));
        cm.select_range((hit.col, hit.row), (end, hit.row), vt.as_mut());
        self.copy = Some(cm);
        self.dirty = true;
    }

    /// [`Desktop::reveal`] for `client` (moved here from another session).
    pub fn reveal_for(&mut self, client: ClientId, hit: &Found) {
        if !self.enter(client) {
            return;
        }
        self.reveal(hit);
        self.leave();
    }
}
