// SPDX-License-Identifier: GPL-3.0-or-later
//! The desktop's menus and pointer polish (D-085): the TermOS menu and the
//! context menus, what the pointer is over (for hover highlights), double
//! clicks, and clicks in the palette and the key list.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use termos_core::clipboard::ClipSource;
use termos_core::ids::PaneId;
use termos_core::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind};
use termos_render::grid::Grid;
use termos_render::icons::Icon;
use termos_render::theme::ThemeBase;
use termos_wm::hit::{Button, CLOSE_OFFSET, Hit, MINIMIZE_OFFSET};
use termos_wm::layout::Preset;
use termos_wm::workspace::{Placement, Snap, SplitHow};

use super::{Desktop, Hover, PaneKind};
use crate::commands::Command;
use crate::menu::{self, Entry, Menu, MenuAction};
use crate::notify::Level;
use crate::palette::PaletteAction;
use crate::prompt::Prompt;

/// Two presses this close together, on the same cell, are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// External agents the menu offers (`termos agent presets`); others from
/// `[agents.external]` are in the palette.
const EXTERNAL_AGENTS: &[(&str, &str)] = &[
    ("claude", "Claude Code"),
    ("codex", "Codex CLI"),
    ("gemini", "Gemini CLI"),
    ("aider", "aider"),
    ("opencode", "opencode"),
];

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

impl Desktop {
    // ----- what the menus hold ------------------------------------------

    /// The TermOS menu.
    pub(super) fn start_entries(&self) -> Vec<Entry> {
        let mut agents = vec![
            Entry::item("Harness: plan a goal", Command::HarnessOpen(String::new())),
            Entry::item(
                "New worktree for a task",
                Command::WorktreeNew(String::new()),
            ),
            Entry::item(
                "Review this worktree",
                Command::WorktreeReview(String::new()),
            ),
            Entry::item("New conversation", Command::AgentOpen(String::new())),
            Entry::item("Resume a conversation", Command::AgentResume(String::new())),
            Entry::item("Audit logs", Command::AuditOpen(String::new())),
            Entry::Separator,
        ];
        agents.extend(
            EXTERNAL_AGENTS
                .iter()
                .map(|(name, title)| Entry::item(*title, Command::AgentExternal((*name).into()))),
        );
        // Accounts (D-097): switch which one new windows use.
        let accounts = self.agent_accounts();
        let mut switch: Vec<Entry> = Vec::new();
        for cli in ["claude", "codex"] {
            let mine: Vec<_> = accounts.iter().filter(|a| a.cli == cli).collect();
            if mine.len() < 2 {
                continue;
            }
            if !switch.is_empty() {
                switch.push(Entry::Separator);
            }
            for a in mine {
                let mark = if a.active { "●" } else { " " };
                switch.push(Entry::item(
                    format!("{mark} {}: {}", a.cli_title, a.title),
                    Command::AccountUse(format!("{} {}", a.cli, a.name)),
                ));
            }
        }
        agents.push(Entry::Separator);
        agents.push(Entry::item("Usage and accounts", Command::UsageOpen));
        if !switch.is_empty() {
            agents.push(Entry::submenu("Accounts", switch).icon(Icon::Agent));
        }
        agents.push(Entry::item(
            "Update Claude Code and Codex",
            Command::AgentUpdate,
        ));
        let layouts = Preset::ALL
            .into_iter()
            .map(|p| Entry::item(capitalized(p.name()), Command::LayoutSelect(p)))
            .collect();
        let themes = ThemeBase::ALL
            .into_iter()
            .map(|b| Entry::item(capitalized(&b.to_string()), Command::ThemeSelect(b)))
            .collect();
        let active = self.session.active_index();
        let mut workspaces: Vec<Entry> = self
            .session
            .workspaces
            .iter()
            .enumerate()
            .take(20)
            .map(|(i, ws)| {
                let mark = if i == active { "●" } else { " " };
                Entry::item(
                    format!("{mark} {i}: {}", ws.name),
                    Command::WorkspaceSelect(i),
                )
            })
            .collect();
        workspaces.extend([
            Entry::Separator,
            Entry::item("New workspace", Command::WorkspaceNew),
            Entry::item("Rename this one", Command::WorkspaceRename),
            Entry::item("Close this one", Command::WorkspaceKill),
        ]);
        let mut sessions: Vec<Entry> = self
            .known_sessions
            .iter()
            .filter(|n| **n != self.session.name)
            .take(20)
            .map(|n| Entry::item(format!("  {n}"), Command::SessionSwitch(n.clone())))
            .collect();
        if !sessions.is_empty() {
            sessions.insert(
                0,
                Entry::item(format!("● {}", self.session.name), Command::SessionRename),
            );
            sessions.push(Entry::Separator);
        }
        sessions.extend([
            Entry::item("New session", Command::SessionNew),
            Entry::item("Rename this session", Command::SessionRename),
            Entry::item("Restore saved sessions", Command::SessionRestore),
        ]);
        let mut v = vec![
            Entry::item("New terminal", Command::Split(SplitHow::Auto)),
            Entry::item("New floating terminal", Command::NewFloat),
            Entry::Separator,
            Entry::submenu(
                "Apps",
                vec![
                    Entry::item("Files", Command::FilesOpen(String::new())),
                    Entry::item("System monitor", Command::MonitorOpen),
                    Entry::item("Clipboard history", Command::ClipboardOpen),
                    Entry::item("Packages", Command::PackagesOpen),
                    Entry::item("Network", Command::NetworkOpen),
                    Entry::item("Updates", Command::UpdateOpen),
                    Entry::item("Settings", Command::SettingsOpen),
                ],
            )
            .icon(Icon::Apps),
            Entry::submenu("AI agents", agents).icon(Icon::Agent),
            Entry::item("Command center", Command::CenterOpen),
            Entry::submenu(
                "Project",
                vec![
                    Entry::item("Build", Command::Task("build".into())),
                    Entry::item("Run", Command::Task("run".into())),
                    Entry::item("Test", Command::Task("test".into())),
                    Entry::item("Git", Command::GitOpen),
                    Entry::item("Dev rootfs shell", Command::DevrootEnter),
                ],
            )
            .icon(Icon::Project),
            Entry::Separator,
            Entry::submenu("Layout", layouts).icon(Icon::Layout),
            Entry::submenu("Theme", themes).icon(Icon::Theme),
            Entry::submenu("Workspaces", workspaces).icon(Icon::Workspace),
            Entry::submenu("Sessions", sessions).icon(Icon::Session),
            Entry::Separator,
            Entry::submenu(
                "Help",
                vec![
                    Entry::item("Tutorial", Command::Tutorial),
                    Entry::item("All keys", Command::Keys),
                    Entry::item("Command palette", Command::Palette),
                    Entry::item("Notifications", Command::Notifications),
                    Entry::Separator,
                    Entry::item("About TermOS", Command::AboutOpen),
                ],
            )
            .icon(Icon::Help),
            Entry::item("Detach", Command::Detach),
            Entry::item("Quit this session", Command::Quit),
        ];
        menu::with_hints(&mut v, &|c| self.keys.key_for(c));
        v
    }

    /// The context menu of a pane.
    fn pane_entries(&self, pane: PaneId) -> Vec<Entry> {
        let floating = self.session.active().is_floating(pane);
        let is_term = matches!(
            self.panes.get(&pane).map(|p| &p.kind),
            Some(PaneKind::Term { .. })
        );
        let mut v = Vec::new();
        if is_term {
            let paste = Entry::item("Paste", Command::Paste);
            v.push(if self.paste_buffer.is_some() {
                paste
            } else {
                paste.disabled()
            });
            v.push(Entry::item("Select and copy text", Command::CopyMode));
            v.push(Entry::Separator);
        }
        v.push(Entry::item("Split right", Command::Split(SplitHow::Right)));
        v.push(Entry::item("Split down", Command::Split(SplitHow::Down)));
        if floating {
            v.push(Entry::item("Maximize or restore", Command::MaximizeToggle));
            v.push(Entry::item("Tile", Command::ToggleFloat));
        } else {
            v.push(Entry::item("Zoom or unzoom", Command::Zoom));
            v.push(Entry::item("Float", Command::ToggleFloat));
        }
        v.push(Entry::item("Rename", Command::PaneRename));
        // An agent CLI's window: continue it as another account (D-097).
        if let Some(agent) = self.panes.get(&pane).and_then(|p| p.agent.clone()) {
            let current = agent.account.unwrap_or_else(|| "default".into());
            let others: Vec<Entry> = self
                .agent_accounts()
                .into_iter()
                .filter(|a| a.cli == agent.preset && a.name != current)
                .map(|a| {
                    Entry::item(
                        format!("Continue as {}", a.title),
                        Command::AgentSwap(a.name),
                    )
                })
                .collect();
            if !others.is_empty() {
                v.push(Entry::Separator);
                v.extend(others);
            }
        }
        if is_term {
            v.push(Entry::item(
                "Browse this folder",
                Command::FilesOpen(String::new()),
            ));
        }
        v.push(Entry::Separator);
        v.push(Entry::item("Close", Command::KillPane));
        menu::with_hints(&mut v, &|c| self.keys.key_for(c));
        v
    }

    /// The context menu of a floating window's title bar.
    fn float_entries(&self) -> Vec<Entry> {
        let mut v = vec![
            Entry::item("Maximize or restore", Command::MaximizeToggle),
            Entry::item("Left half", Command::SnapFloat(Snap::LeftHalf)),
            Entry::item("Right half", Command::SnapFloat(Snap::RightHalf)),
            Entry::item("Center", Command::SnapFloat(Snap::Center)),
            Entry::item("Minimize", Command::Minimize),
            Entry::item("Tile", Command::ToggleFloat),
            Entry::item("Rename", Command::PaneRename),
            Entry::Separator,
            Entry::item("Close", Command::KillPane),
        ];
        menu::with_hints(&mut v, &|c| self.keys.key_for(c));
        v
    }

    /// The context menu of a workspace tab (the tab is selected first, so
    /// these act on it).
    fn tab_entries(&self) -> Vec<Entry> {
        let layouts = Preset::ALL
            .into_iter()
            .map(|p| Entry::item(capitalized(p.name()), Command::LayoutSelect(p)))
            .collect();
        let mut v = vec![
            Entry::item("Rename", Command::WorkspaceRename),
            Entry::submenu("Layout", layouts).icon(Icon::Layout),
            Entry::item("New workspace", Command::WorkspaceNew),
            Entry::Separator,
            Entry::item("Close", Command::WorkspaceKill),
        ];
        menu::with_hints(&mut v, &|c| self.keys.key_for(c));
        v
    }

    // ----- opening and driving menus ------------------------------------

    fn open_menu(&mut self, title: &str, entries: Vec<Entry>, x: u16, y: u16) {
        // Above or below the status line, never over it.
        let area = self.layout_area();
        self.ui.menu = Some(Menu::at(title, entries, (x, y), area, self.ui.theme.icons));
        self.ui.palette = None;
        self.ui.whichkey_visible = false;
        self.ui.whichkey_full = false;
        self.dirty = true;
    }

    /// The TermOS menu, by the start button.
    pub(super) fn open_start_menu(&mut self) {
        let y = self.status_y();
        // Above the status line at the bottom, below it at the top.
        let y = if y == 0 { 1 } else { y };
        let entries = self.start_entries();
        self.open_menu("TermOS", entries, 0, y);
    }

    /// Runs what a menu asked for.
    fn menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::None => {}
            MenuAction::Redraw => self.dirty = true,
            MenuAction::Close => {
                self.ui.menu = None;
                self.dirty = true;
            }
            MenuAction::Run(cmd) => {
                self.ui.menu = None;
                self.dirty = true;
                self.run(cmd);
            }
        }
    }

    /// A key while a menu is open. Returns whether the menu took it.
    pub(super) fn menu_key(&mut self, k: KeyEvent) -> bool {
        let Some(m) = &mut self.ui.menu else {
            return false;
        };
        let action = m.on_key(k);
        self.menu_action(action);
        true
    }

    /// A mouse event while a menu is open (menus take every event).
    pub(super) fn menu_mouse(&mut self, m: MouseEvent) -> bool {
        let Some(menu) = &mut self.ui.menu else {
            return false;
        };
        let action = menu.on_mouse(m);
        self.menu_action(action);
        true
    }

    /// A right-click on the status line: a tab's menu, or the TermOS menu.
    pub(super) fn status_context(&mut self, m: MouseEvent) {
        if let Some(idx) = self.ui.status_regions.tab_at(m.x, m.y) {
            self.session.select(idx);
            let name = self
                .session
                .workspaces
                .get(idx)
                .map(|w| w.name.clone())
                .unwrap_or_default();
            let entries = self.tab_entries();
            let y = if m.y == 0 { 1 } else { m.y };
            self.open_menu(&name, entries, m.x, y);
        } else {
            self.open_start_menu();
        }
    }

    /// A right-click in the layout: the context menu of what is there.
    /// Returns whether it opened one.
    pub(super) fn layout_context(&mut self, m: MouseEvent, hit: &Hit) -> bool {
        let (pane, entries) = match hit {
            Hit::Pane { pane, .. } => (*pane, self.pane_entries(*pane)),
            Hit::FloatTitle(pane) | Hit::FloatButton(pane, _) | Hit::FloatEdge(pane, _) => {
                (*pane, self.float_entries())
            }
            Hit::Nothing => {
                // The wallpaper: everything TermOS offers, at the pointer.
                let entries = self.start_entries();
                self.open_menu("TermOS", entries, m.x, m.y);
                return true;
            }
            Hit::Separator(_) => return false,
        };
        self.session.active_mut().set_focus(pane);
        let title = self
            .panes
            .get(&pane)
            .map(super::Pane::display_name)
            .unwrap_or_default();
        self.open_menu(&title, entries, m.x, m.y);
        true
    }

    // ----- double clicks --------------------------------------------------

    /// Records a left press; whether it completes a double click.
    pub(super) fn double_click(&mut self, m: &MouseEvent) -> bool {
        let now = Instant::now();
        let double = self
            .ui
            .last_click
            .is_some_and(|(t, x, y)| x == m.x && y == m.y && now.duration_since(t) <= DOUBLE_CLICK);
        // A third press starts again.
        self.ui.last_click = if double { None } else { Some((now, m.x, m.y)) };
        double
    }

    /// Maximizes the focused floating window, or gives it back the size it
    /// had; a tiled pane zooms.
    pub(super) fn toggle_maximize(&mut self) {
        let Some(pane) = self.focused() else {
            return;
        };
        let area = self.layout_area();
        if !self.session.active().is_floating(pane) {
            self.session.active_mut().toggle_zoom();
            return;
        }
        let current = self.session.active().float_rect(pane);
        match self.max_restore.remove(&pane) {
            Some(before) if current == Some(area) => {
                self.session.active_mut().set_float_rect(pane, before, area);
            }
            _ => {
                if let Some(r) = current {
                    self.max_restore.insert(pane, r);
                }
                self.session.active_mut().snap(pane, Snap::Maximize, area);
            }
        }
        self.layout_changed = true;
    }

    // ----- what the pointer is over ---------------------------------------

    /// The pointer moved (no button down). Redraws only when what it is
    /// over changes, so moving over one thing costs nothing.
    pub(super) fn pointer_moved(&mut self, m: MouseEvent) {
        self.ui.pointer = Some((m.x, m.y));
        if self.menu_mouse(m) {
            return;
        }
        let hover = if m.y == self.status_y() {
            self.ui
                .status_regions
                .target_at(m.x, m.y)
                .map_or(Hover::None, Hover::Status)
        } else {
            match self.placement().hit(m.x, m.y) {
                Hit::FloatButton(p, b) => Hover::FloatButton(p, b),
                Hit::Nothing => self
                    .wallpaper_at(m.x, m.y)
                    .map_or(Hover::None, |(i, _)| Hover::Wallpaper(i)),
                _ => Hover::None,
            }
        };
        if hover != self.ui.hover {
            self.ui.hover = hover;
            self.dirty = true;
        }
        if let Hit::Pane { pane, .. } = self.placement().hit(m.x, m.y)
            && self.pane_wants_mouse(pane, &m)
        {
            self.forward_mouse(pane, m);
        }
    }

    /// The pointer's column on the status line, for its highlights.
    pub(super) fn status_pointer(&self) -> Option<u16> {
        self.ui
            .pointer
            .filter(|&(_, y)| y == self.status_y())
            .map(|(x, _)| x)
    }

    /// Highlights the window button under the pointer.
    pub(super) fn draw_hover(&self, grid: &mut Grid, placement: &Placement) {
        let Hover::FloatButton(pane, button) = self.ui.hover else {
            return;
        };
        let Some(f) = placement.floats.iter().find(|f| f.pane == pane) else {
            return;
        };
        let (offset, glyph) = match button {
            Button::Minimize => (MINIMIZE_OFFSET, "–"),
            Button::Close => (CLOSE_OFFSET, "×"),
        };
        // Where `termos_wm::hit` finds them.
        let x = f.outer.right().saturating_sub(offset);
        let style = if button == Button::Close {
            self.ui.theme.status_prefix
        } else {
            self.ui.theme.overlay_selected
        };
        grid.put_str(x, f.outer.y, glyph, style, x + 1);
    }

    // ----- the palette and the key list -----------------------------------

    /// A mouse event while the palette is open: a press on a command runs
    /// it, the wheel moves the selection, a press outside closes it.
    /// Returns whether the palette took the event.
    pub(super) fn palette_mouse(&mut self, m: MouseEvent) -> bool {
        let area = self.layout_area();
        let Some(p) = &mut self.ui.palette else {
            return false;
        };
        match m.kind {
            MouseKind::Down(MouseButton::Left) => {
                if let Some(i) = p.row_at(area, m.x, m.y) {
                    p.select(i);
                    let action = p.handle_key(KeyEvent::plain(KeyCode::Enter));
                    self.palette_action(action);
                } else if !p.rect(area).contains(m.x, m.y) {
                    self.ui.palette = None;
                }
                self.dirty = true;
            }
            MouseKind::Down(_) if !p.rect(area).contains(m.x, m.y) => {
                self.ui.palette = None;
                self.dirty = true;
            }
            MouseKind::ScrollUp | MouseKind::ScrollDown => {
                let key = if m.kind == MouseKind::ScrollUp {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                };
                p.handle_key(KeyEvent::plain(key));
                self.dirty = true;
            }
            MouseKind::Moved => {
                if let Some(i) = p.row_at(area, m.x, m.y)
                    && p.select(i)
                {
                    self.dirty = true;
                }
            }
            _ => {}
        }
        true
    }

    /// Runs what the palette asked for.
    pub(super) fn palette_action(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::None => {}
            PaletteAction::Close => self.ui.palette = None,
            PaletteAction::Run(cmd) => {
                self.ui.palette = None;
                match cmd.parse::<Command>() {
                    Ok(c) => self.run(c),
                    Err(_) if self.plugin_commands.iter().any(|c| c.name == cmd) => {
                        self.requests
                            .push(super::SessionRequest::PluginCommand { name: cmd });
                    }
                    Err(e) => self.notify(Level::Error, e.to_string()),
                }
            }
        }
        self.dirty = true;
    }

    /// A press while the key list shows: a command runs, anything else
    /// closes the list. Returns whether the list took the event.
    pub(super) fn whichkey_mouse(&mut self, m: MouseEvent) -> bool {
        if !(self.ui.whichkey_visible || self.ui.whichkey_full) {
            return false;
        }
        let MouseKind::Down(_) = m.kind else {
            return matches!(m.kind, MouseKind::Moved);
        };
        let cmd = self
            .ui
            .whichkey_regions
            .iter()
            .find(|(r, _)| r.contains(m.x, m.y))
            .map(|(_, c)| c.clone());
        self.ui.whichkey_visible = false;
        self.ui.whichkey_full = false;
        self.ui.whichkey_deadline = None;
        self.ui.key_state = super::KeyState::Normal;
        self.dirty = true;
        if let Some(c) = cmd {
            self.run(c);
        }
        true
    }

    // ----- what `tos` asks for (D-085) --------------------------------------

    /// Focuses `pane` (going to its workspace) when given.
    pub fn focus_pane(&mut self, pane: Option<PaneId>) -> Result<(), String> {
        let Some(id) = pane else {
            return Ok(());
        };
        let idx = self
            .session
            .workspaces
            .iter()
            .position(|w| w.panes().contains(&id))
            .ok_or_else(|| format!("no pane {id} in this session"))?;
        if idx != self.session.active_index() {
            self.session.select(idx);
        }
        self.session.active_mut().set_focus(id);
        self.after_focus_change();
        Ok(())
    }

    /// Opens `argv` in a new pane beside `pane` (`right`, `down`, `auto`)
    /// or in a floating window (`float`).
    pub fn open_program(
        &mut self,
        pane: Option<PaneId>,
        place: &str,
        argv: Vec<String>,
        cwd: Option<PathBuf>,
    ) -> Result<PaneId, String> {
        let how = match place {
            "right" => Some(SplitHow::Right),
            "down" => Some(SplitHow::Down),
            "auto" => Some(SplitHow::Auto),
            "float" => None,
            other => return Err(format!("`{other}`: right, down, auto or float")),
        };
        let mut argv = argv.into_iter();
        let program = argv.next().ok_or("no command to run")?;
        self.focus_pane(pane)?;
        let area = self.layout_area();
        let cwd = cwd.or_else(|| self.focused_cwd());
        let size = match how {
            Some(_) => (area.w / 2, area.h / 2),
            None => (area.w * 7 / 10, area.h * 7 / 10),
        };
        let id = self
            .spawn_pane(cwd, size, Some((program, argv.collect())))
            .map_err(|e| format!("cannot start it: {e}"))?;
        match how {
            Some(how) => {
                if !self.session.active_mut().add_split(id, how, area) {
                    self.close_pane(id);
                    return Err("no space for a new pane".into());
                }
            }
            None => {
                self.session.active_mut().add_float(id, area);
                self.animate(crate::anim::Kind::FloatOpen(id), Instant::now());
            }
        }
        self.layout_changed = true;
        self.dirty = true;
        Ok(id)
    }

    /// Names `pane` (empty: its program's title again).
    pub fn rename_pane(&mut self, pane: PaneId, name: &str) -> Result<(), String> {
        let p = self
            .panes
            .get_mut(&pane)
            .ok_or_else(|| format!("no pane {pane}"))?;
        let name = name.trim();
        p.name = (!name.is_empty()).then(|| name.to_owned());
        self.dirty = true;
        Ok(())
    }

    /// Copies `text` as `tos copy` does: the history, and every client's
    /// terminal (OSC 52).
    pub fn copy_from_command(&mut self, text: String) {
        self.copy_text(text, ClipSource::Program("tos".into()));
    }

    /// The prompt for naming the focused pane.
    pub(super) fn prompt_pane_name(&mut self) {
        if let Some(id) = self.focused() {
            let current = self
                .panes
                .get(&id)
                .and_then(|p| p.name.clone())
                .unwrap_or_default();
            self.ui.prompt = Some(Prompt::rename_pane(id, &current));
            self.dirty = true;
        }
    }
}
