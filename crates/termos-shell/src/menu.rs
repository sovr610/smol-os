// SPDX-License-Identifier: GPL-3.0-or-later
//! Menus (ARCHITECTURE.md §6, D-085): the TermOS menu behind the start
//! button, and the context menus a right-click opens.
//!
//! Mouse first: the pointer selects what it is over, an item with a
//! submenu opens it beside, a press runs an item, and a press anywhere
//! else closes the menu. The keyboard works too: arrows, Enter, Esc, and a
//! letter jumps to the next item starting with it.

use termos_core::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind};
use termos_render::grid::{Grid, Rect};
use termos_render::icons::{Icon, IconSet};
use termos_render::theme::Theme;

use crate::commands::Command;
use crate::keys::is_cancel;
use crate::ui::{self, Chrome, truncate, width};

/// One row of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Item(Item),
    /// Opens another menu beside this one.
    Submenu {
        label: String,
        icon: Option<Icon>,
        entries: Vec<Entry>,
    },
    Separator,
}

/// A row that runs a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// The key that does the same, shown at the right (`C-b %`).
    pub hint: String,
    pub command: Command,
    /// Shown before the label (D-088); from the command unless set.
    pub icon: Option<Icon>,
    /// Shown, but not runnable (Copy with nothing selected).
    pub enabled: bool,
}

impl Entry {
    pub fn item(label: impl Into<String>, command: Command) -> Self {
        Entry::Item(Item {
            label: label.into(),
            hint: String::new(),
            icon: command_icon(&command),
            command,
            enabled: true,
        })
    }

    pub fn submenu(label: impl Into<String>, entries: Vec<Entry>) -> Self {
        Entry::Submenu {
            label: label.into(),
            icon: None,
            entries,
        }
    }

    /// The same entry with this icon.
    #[must_use]
    pub fn icon(mut self, with: Icon) -> Self {
        match &mut self {
            Entry::Item(Item { icon, .. }) | Entry::Submenu { icon, .. } => *icon = Some(with),
            Entry::Separator => {}
        }
        self
    }

    fn icon_of(&self) -> Option<Icon> {
        match self {
            Entry::Item(i) => i.icon,
            Entry::Submenu { icon, .. } => *icon,
            Entry::Separator => None,
        }
    }

    /// The same entry, shown but not runnable.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        if let Entry::Item(i) = &mut self {
            i.enabled = false;
        }
        self
    }

    fn selectable(&self) -> bool {
        match self {
            Entry::Item(i) => i.enabled,
            Entry::Submenu { entries, .. } => !entries.is_empty(),
            Entry::Separator => false,
        }
    }

    fn label(&self) -> &str {
        match self {
            Entry::Item(i) => &i.label,
            Entry::Submenu { label, .. } => label,
            Entry::Separator => "",
        }
    }
}

/// The icon a menu shows for a command, when it has one.
fn command_icon(c: &Command) -> Option<Icon> {
    use termos_wm::workspace::SplitHow;
    Some(match c {
        Command::Split(SplitHow::Down) => Icon::SplitDown,
        Command::Split(SplitHow::Auto) => Icon::Terminal,
        Command::Split(_) => Icon::Split,
        Command::NewFloat => Icon::FloatingTerminal,
        Command::KillPane | Command::WorkspaceKill => Icon::Close,
        Command::Zoom | Command::MaximizeToggle => Icon::Zoom,
        Command::ToggleFloat => Icon::Float,
        Command::Minimize => Icon::Minimize,
        Command::PaneRename | Command::WorkspaceRename | Command::SessionRename => Icon::Rename,
        Command::WorkspaceNew | Command::SessionNew => Icon::New,
        Command::CopyMode => Icon::Copy,
        Command::Paste => Icon::Paste,
        Command::Palette => Icon::Palette,
        Command::Menu => Icon::Menu,
        Command::Keys => Icon::Keys,
        Command::Notifications => Icon::Notifications,
        Command::Tutorial => Icon::Tutorial,
        Command::AboutOpen => Icon::About,
        Command::Quit => Icon::Quit,
        Command::Detach => Icon::Detach,
        Command::SessionRestore => Icon::Session,
        Command::ImageOpen(_) => Icon::ImageViewer,
        Command::MarkdownOpen(_) => Icon::Markdown,
        Command::FilesOpen(_) => Icon::Files,
        Command::FileOpen(_) => Icon::File,
        Command::FileEdit(_) => Icon::Code,
        Command::MonitorOpen => Icon::Monitor,
        Command::NetworkOpen => Icon::Network,
        Command::UpdateOpen => Icon::Updates,
        Command::SettingsOpen => Icon::Settings,
        Command::Task(t) if t == "build" => Icon::Build,
        Command::Task(t) if t == "test" => Icon::Test,
        Command::Task(_) => Icon::Run,
        Command::GitOpen => Icon::Git,
        Command::ClipboardOpen => Icon::Clipboard,
        Command::AgentOpen(_) | Command::AgentResume(_) | Command::AgentExternal(_) => Icon::Agent,
        Command::AuditOpen(_) => Icon::Audit,
        _ => return None,
    })
}

/// Fills in the key hint of every item, from `hint(command)`.
pub fn with_hints(entries: &mut [Entry], hint: &dyn Fn(&Command) -> Option<String>) {
    for e in entries {
        match e {
            Entry::Item(i) if i.hint.is_empty() => {
                i.hint = hint(&i.command).unwrap_or_default();
            }
            Entry::Submenu { entries, .. } => with_hints(entries, hint),
            _ => {}
        }
    }
}

/// What a menu wants after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Nothing changed.
    None,
    /// It looks different (another item is selected, a submenu opened).
    Redraw,
    /// Run this, and close.
    Run(Command),
    /// Close without running anything.
    Close,
}

#[derive(Debug, Clone)]
struct Level {
    title: String,
    entries: Vec<Entry>,
    selected: Option<usize>,
    rect: Rect,
}

/// An open menu and its open submenus.
#[derive(Debug, Clone)]
pub struct Menu {
    levels: Vec<Level>,
    area: Rect,
    /// How icons are drawn (the client's theme).
    icons: IconSet,
}

/// The widest a menu gets (labels are cut beyond).
const MAX_WIDTH: u16 = 48;

/// The columns before a level's labels: an icon and a space, when the
/// level has any icons and the icon set draws them.
fn gutter(entries: &[Entry], icons: IconSet) -> u16 {
    if icons != IconSet::None && entries.iter().any(|e| e.icon_of().is_some()) {
        2
    } else {
        0
    }
}

fn inner_width(entries: &[Entry], icons: IconSet) -> u16 {
    let gutter = gutter(entries, icons);
    let w = entries
        .iter()
        .map(|e| match e {
            Entry::Item(i) => {
                width(&i.label)
                    + if i.hint.is_empty() {
                        0
                    } else {
                        width(&i.hint) + 3
                    }
            }
            Entry::Submenu { label, .. } => width(label) + 3,
            Entry::Separator => 0,
        })
        .max()
        .unwrap_or(0)
        + gutter;
    // A space on each side.
    (w + 2).clamp(16, MAX_WIDTH)
}

impl Menu {
    /// A menu with its top-left corner at (`x`, `y`), moved left or up to
    /// stay inside `area` (above the point when there is no room below).
    /// Icons are drawn as `icons` says (the client's theme).
    pub fn at(
        title: impl Into<String>,
        entries: Vec<Entry>,
        (x, y): (u16, u16),
        area: Rect,
        icons: IconSet,
    ) -> Self {
        let title = title.into();
        let w = (inner_width(&entries, icons) + 2)
            .max(width(&title) + 6)
            .min(area.w);
        let h = (u16::try_from(entries.len()).unwrap_or(u16::MAX) + 2).min(area.h);
        let rx = x.min(area.right().saturating_sub(w)).max(area.x);
        let ry = if y + h <= area.bottom() {
            y.max(area.y)
        } else {
            // Above: its bottom border just above the point.
            y.saturating_sub(h).max(area.y)
        };
        let mut menu = Menu {
            levels: vec![Level {
                title,
                entries,
                selected: None,
                rect: Rect::new(rx, ry, w, h),
            }],
            area,
            icons,
        };
        menu.levels[0].selected = None;
        menu
    }

    /// Every rectangle the menu covers (for what is drawn beneath).
    pub fn rects(&self) -> Vec<Rect> {
        self.levels.iter().map(|l| l.rect).collect()
    }

    /// The level and entry under (`x`, `y`): `None` outside every menu,
    /// `Some((level, None))` on a border.
    fn hit(&self, x: u16, y: u16) -> Option<(usize, Option<usize>)> {
        self.levels
            .iter()
            .enumerate()
            .rev()
            .find(|(_, l)| l.rect.contains(x, y))
            .map(|(i, l)| {
                let inner = l.rect.inset(1);
                let entry = inner
                    .contains(x, y)
                    .then(|| usize::from(y - inner.y))
                    .filter(|&e| e < l.entries.len());
                (i, entry)
            })
    }

    /// Opens the submenu at `entry` of `level` beside it (closing any
    /// deeper ones). Returns whether anything changed.
    fn open_child(&mut self, level: usize, entry: usize) -> bool {
        let Some(Entry::Submenu { label, entries, .. }) =
            self.levels[level].entries.get(entry).cloned()
        else {
            return false;
        };
        let already = self.levels.len() == level + 2 && self.levels[level + 1].title == label;
        if already {
            return false;
        }
        self.levels.truncate(level + 1);
        let parent = self.levels[level].rect;
        let w = (inner_width(&entries, self.icons) + 2).min(self.area.w);
        let h = (u16::try_from(entries.len()).unwrap_or(u16::MAX) + 2).min(self.area.h);
        let x = if parent.right() + w <= self.area.right() {
            parent.right()
        } else {
            parent.x.saturating_sub(w).max(self.area.x)
        };
        let row = parent.y + 1 + u16::try_from(entry).unwrap_or(0);
        let y = row
            .saturating_sub(1)
            .min(self.area.bottom().saturating_sub(h))
            .max(self.area.y);
        self.levels.push(Level {
            title: label,
            entries,
            selected: None,
            rect: Rect::new(x, y, w, h),
        });
        true
    }

    /// Selects `entry` of `level`, opening it if it is a submenu and
    /// closing deeper menus otherwise.
    fn select(&mut self, level: usize, entry: usize) -> bool {
        let mut changed = self.levels[level].selected != Some(entry);
        self.levels[level].selected = Some(entry);
        match self.levels[level].entries.get(entry) {
            Some(Entry::Submenu { .. }) => changed |= self.open_child(level, entry),
            _ => {
                if self.levels.len() > level + 1 {
                    self.levels.truncate(level + 1);
                    changed = true;
                }
            }
        }
        changed
    }

    /// A mouse event at screen coordinates.
    pub fn on_mouse(&mut self, m: MouseEvent) -> MenuAction {
        let hit = self.hit(m.x, m.y);
        match m.kind {
            MouseKind::Moved | MouseKind::Drag(_) => match hit {
                Some((level, Some(entry))) if self.levels[level].entries[entry].selectable() => {
                    if self.select(level, entry) {
                        MenuAction::Redraw
                    } else {
                        MenuAction::None
                    }
                }
                _ => MenuAction::None,
            },
            MouseKind::Down(MouseButton::Left | MouseButton::Right) => match hit {
                None => MenuAction::Close,
                Some((_, None)) => MenuAction::None,
                Some((level, Some(entry))) => match self.levels[level].entries[entry].clone() {
                    Entry::Item(i) if i.enabled => MenuAction::Run(i.command),
                    Entry::Submenu { .. } => {
                        self.select(level, entry);
                        if self.first_in(level + 1) {
                            MenuAction::Redraw
                        } else {
                            MenuAction::None
                        }
                    }
                    _ => MenuAction::None,
                },
            },
            MouseKind::Down(MouseButton::Middle) if hit.is_none() => MenuAction::Close,
            _ => MenuAction::None,
        }
    }

    /// Selects the first selectable entry of `level`, if it is open.
    fn first_in(&mut self, level: usize) -> bool {
        let Some(l) = self.levels.get_mut(level) else {
            return false;
        };
        let first = l.entries.iter().position(Entry::selectable);
        let changed = l.selected != first;
        l.selected = first;
        changed
    }

    /// Moves the selection of the deepest menu by `step`, skipping rows
    /// that cannot be selected.
    fn step(&mut self, step: isize) {
        let level = self.levels.len() - 1;
        let l = &self.levels[level];
        let n = l.entries.len();
        if n == 0 {
            return;
        }
        let start = l.selected.map_or(if step > 0 { n - 1 } else { 0 }, |s| s);
        let mut i = start;
        for _ in 0..n {
            i = (i as isize + step).rem_euclid(n as isize) as usize;
            if self.levels[level].entries[i].selectable() {
                self.levels[level].selected = Some(i);
                // Moving with the keyboard closes submenus of this level.
                self.levels.truncate(level + 1);
                return;
            }
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) -> MenuAction {
        let level = self.levels.len() - 1;
        if is_cancel(k) {
            if level == 0 {
                return MenuAction::Close;
            }
            self.levels.pop();
            return MenuAction::Redraw;
        }
        match k.code {
            KeyCode::Up => self.step(-1),
            KeyCode::Down | KeyCode::Tab => self.step(1),
            KeyCode::Home => {
                self.levels[level].selected = None;
                self.step(1);
            }
            KeyCode::End => {
                self.levels[level].selected = None;
                self.step(-1);
            }
            KeyCode::Left => {
                if level > 0 {
                    self.levels.pop();
                }
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                let Some(sel) = self.levels[level].selected else {
                    self.step(1);
                    return MenuAction::Redraw;
                };
                match self.levels[level].entries[sel].clone() {
                    Entry::Item(i) if i.enabled && k.code != KeyCode::Right => {
                        return MenuAction::Run(i.command);
                    }
                    Entry::Submenu { .. } => {
                        self.open_child(level, sel);
                        self.first_in(level + 1);
                    }
                    _ => return MenuAction::None,
                }
            }
            KeyCode::Char(c) if c.is_alphanumeric() => {
                let l = &self.levels[level];
                let n = l.entries.len();
                let start = l.selected.map_or(0, |s| s + 1);
                let lower = c.to_lowercase().next().unwrap_or(c);
                let found = (0..n).map(|d| (start + d) % n).find(|&i| {
                    let e = &l.entries[i];
                    e.selectable()
                        && e.label()
                            .chars()
                            .next()
                            .is_some_and(|f| f.to_lowercase().next() == Some(lower))
                });
                match found {
                    Some(i) => {
                        self.levels[level].selected = Some(i);
                        self.levels.truncate(level + 1);
                    }
                    None => return MenuAction::None,
                }
            }
            _ => return MenuAction::None,
        }
        MenuAction::Redraw
    }

    /// Draws every open level.
    pub fn render(&self, grid: &mut Grid, theme: &Theme, chrome: Chrome) {
        for l in &self.levels {
            let inner = ui::panel(grid, l.rect, self.area, theme, &l.title, chrome);
            let gutter = gutter(&l.entries, self.icons);
            for (i, e) in l.entries.iter().enumerate() {
                let Ok(dy) = u16::try_from(i) else { break };
                if dy >= inner.h {
                    break;
                }
                let y = inner.y + dy;
                let selected = l.selected == Some(i);
                let row_style = if selected {
                    theme.overlay_selected
                } else {
                    theme.overlay
                };
                let row = Rect::new(inner.x, y, inner.w, 1);
                let muted = match e {
                    Entry::Item(i) => !i.enabled,
                    Entry::Submenu { entries, .. } => entries.is_empty(),
                    Entry::Separator => false,
                };
                let lx = inner.x + 1 + gutter;
                match e {
                    Entry::Separator => {
                        let line = "─".repeat(usize::from(inner.w));
                        grid.put_str(inner.x, y, &line, theme.overlay_border, inner.right());
                    }
                    Entry::Item(item) => {
                        grid.fill(row, &termos_render::cell::Cell::blank(row_style));
                        let label_style = if !item.enabled {
                            theme.overlay_muted
                        } else {
                            row_style
                        };
                        let hint_w = if item.hint.is_empty() {
                            0
                        } else {
                            width(&item.hint) + 2
                        };
                        let room = inner.w.saturating_sub(2 + gutter + hint_w);
                        grid.put_str(
                            lx,
                            y,
                            &truncate(&item.label, room),
                            label_style,
                            inner.right(),
                        );
                        if !item.hint.is_empty() {
                            let hx = inner.right().saturating_sub(width(&item.hint) + 1);
                            let hint_style = if selected {
                                theme.overlay_selected
                            } else {
                                theme.overlay_key
                            };
                            grid.put_str(hx, y, &item.hint, hint_style, inner.right());
                        }
                    }
                    Entry::Submenu { label, entries, .. } => {
                        grid.fill(row, &termos_render::cell::Cell::blank(row_style));
                        let style = if entries.is_empty() {
                            theme.overlay_muted
                        } else {
                            row_style
                        };
                        let room = inner.w.saturating_sub(4 + gutter);
                        grid.put_str(lx, y, &truncate(label, room), style, inner.right());
                        grid.put_str(
                            inner.right().saturating_sub(2),
                            y,
                            "▸",
                            style,
                            inner.right(),
                        );
                    }
                }
                if let Some(icon) = e.icon_of().filter(|_| gutter > 0) {
                    let style = if muted {
                        theme.overlay_muted
                    } else if selected {
                        theme.overlay_selected
                    } else {
                        theme.overlay_key
                    };
                    grid.put_str(inner.x + 1, y, icon.glyph(self.icons), style, lx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_core::input::Mods;
    use termos_wm::workspace::SplitHow;

    fn mouse(kind: MouseKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            x,
            y,
            mods: Mods::empty(),
        }
    }

    fn entries() -> Vec<Entry> {
        vec![
            Entry::item("Split right", Command::Split(SplitHow::Right)),
            Entry::item("Copy", Command::Paste).disabled(),
            Entry::Separator,
            Entry::submenu(
                "Layout",
                vec![
                    Entry::item("Tiled", Command::LayoutNext),
                    Entry::item("Even", Command::Zoom),
                ],
            ),
            Entry::item("Close", Command::KillPane),
        ]
    }

    #[test]
    fn placed_inside_the_area_and_above_the_point_when_low() {
        let area = Rect::new(0, 0, 80, 23);
        let m = Menu::at("", entries(), (70, 2), area, IconSet::None);
        let r = m.rects()[0];
        assert!(r.right() <= area.right() && r.x < 70, "moved left: {r:?}");
        assert_eq!(r.y, 2);
        assert_eq!(r.h, 7);
        // From the status line: just above it.
        let m = Menu::at("TermOS", entries(), (0, 23), area, IconSet::None);
        let r = m.rects()[0];
        assert_eq!((r.x, r.bottom()), (0, 23));
    }

    #[test]
    fn hovering_selects_and_opens_submenus_and_a_press_runs() {
        let area = Rect::new(0, 0, 80, 23);
        let mut m = Menu::at("", entries(), (2, 2), area, IconSet::None);
        let r = m.rects()[0];
        // The first row.
        assert_eq!(
            m.on_mouse(mouse(MouseKind::Moved, r.x + 2, r.y + 1)),
            MenuAction::Redraw
        );
        assert_eq!(
            m.on_mouse(mouse(MouseKind::Moved, r.x + 3, r.y + 1)),
            MenuAction::None,
            "same item: no redraw"
        );
        // The disabled one is not selected.
        assert_eq!(
            m.on_mouse(mouse(MouseKind::Moved, r.x + 2, r.y + 2)),
            MenuAction::None
        );
        // Hovering "Layout" opens it to the right.
        assert_eq!(
            m.on_mouse(mouse(MouseKind::Moved, r.x + 2, r.y + 4)),
            MenuAction::Redraw
        );
        let rects = m.rects();
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[1].x, r.right());
        // Pressing in the submenu runs its item.
        assert_eq!(
            m.on_mouse(mouse(
                MouseKind::Down(MouseButton::Left),
                rects[1].x + 2,
                rects[1].y + 2
            )),
            MenuAction::Run(Command::Zoom)
        );
        // Back on the root, another item closes the submenu.
        m.on_mouse(mouse(MouseKind::Moved, r.x + 2, r.y + 5));
        assert_eq!(m.rects().len(), 1);
        // A disabled item does nothing; a press outside closes.
        assert_eq!(
            m.on_mouse(mouse(MouseKind::Down(MouseButton::Left), r.x + 2, r.y + 2)),
            MenuAction::None
        );
        assert_eq!(
            m.on_mouse(mouse(MouseKind::Down(MouseButton::Left), 70, 20)),
            MenuAction::Close
        );
    }

    #[test]
    fn submenus_open_to_the_left_at_the_right_edge() {
        let area = Rect::new(0, 0, 60, 23);
        let mut m = Menu::at("", entries(), (59, 2), area, IconSet::None);
        let r = m.rects()[0];
        m.on_mouse(mouse(MouseKind::Moved, r.x + 2, r.y + 4));
        let sub = m.rects()[1];
        assert!(sub.right() <= r.x, "{sub:?} left of {r:?}");
    }

    #[test]
    fn the_keyboard_skips_what_cannot_be_chosen() {
        let area = Rect::new(0, 0, 80, 23);
        let mut m = Menu::at("", entries(), (2, 2), area, IconSet::None);
        m.on_key(KeyEvent::plain(KeyCode::Down));
        assert_eq!(
            m.on_key(KeyEvent::plain(KeyCode::Enter)),
            MenuAction::Run(Command::Split(SplitHow::Right))
        );
        // Down skips "Copy" (disabled) and the separator.
        m.on_key(KeyEvent::plain(KeyCode::Down));
        m.on_key(KeyEvent::plain(KeyCode::Right));
        assert_eq!(m.rects().len(), 2, "Layout opened");
        assert_eq!(
            m.on_key(KeyEvent::plain(KeyCode::Enter)),
            MenuAction::Run(Command::LayoutNext)
        );
        assert_eq!(m.on_key(KeyEvent::plain(KeyCode::Esc)), MenuAction::Redraw);
        assert_eq!(m.rects().len(), 1);
        // A letter jumps; Esc on the root closes.
        m.on_key(KeyEvent::ch('c'));
        assert_eq!(
            m.on_key(KeyEvent::plain(KeyCode::Enter)),
            MenuAction::Run(Command::KillPane)
        );
        assert_eq!(m.on_key(KeyEvent::plain(KeyCode::Esc)), MenuAction::Close);
    }

    #[test]
    fn drawing() {
        let area = Rect::new(0, 0, 60, 14);
        let mut e = entries();
        with_hints(&mut e, &|c| {
            (*c == Command::KillPane).then(|| "C-b x".to_owned())
        });
        let mut m = Menu::at("bash", e, (4, 1), area, IconSet::None);
        let r = m.rects()[0];
        m.on_mouse(mouse(MouseKind::Moved, r.x + 2, r.y + 4));
        let mut g = Grid::new(60, 14);
        m.render(&mut g, &Theme::default(), Chrome::default());
        insta::assert_snapshot!(g.to_text());
    }
    #[test]
    fn icons_line_up_in_a_gutter() {
        let area = Rect::new(0, 0, 60, 14);
        let mut e = entries();
        if let Entry::Submenu { icon, .. } = &mut e[3] {
            *icon = Some(Icon::Layout);
        }
        for set in [IconSet::Unicode, IconSet::Nerd, IconSet::Ascii] {
            let mut m = Menu::at("bash", e.clone(), (4, 1), area, set);
            let r = m.rects()[0];
            m.on_mouse(mouse(MouseKind::Moved, r.x + 2, r.y + 4));
            let mut g = Grid::new(60, 14);
            let theme = Theme {
                icons: set,
                ..Theme::default()
            };
            m.render(&mut g, &theme, Chrome::default());
            insta::assert_snapshot!(format!("icons_{set}"), g.to_text());
        }
        // No icons: no gutter.
        let wide = vec![Entry::item(
            "Split right, and run a long command",
            Command::Zoom,
        )];
        let plain = Menu::at("", wide.clone(), (4, 1), area, IconSet::None);
        let iconic = Menu::at("", wide, (4, 1), area, IconSet::Unicode);
        assert_eq!(plain.rects()[0].w + 2, iconic.rects()[0].w);
    }
}
