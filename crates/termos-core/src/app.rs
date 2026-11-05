// SPDX-License-Identifier: GPL-3.0-or-later
//! The in-process app API: windows drawn by Rust code instead of a PTY.
//!
//! Apps draw with ratatui widgets into a ratatui `Buffer`; the desktop blits
//! that buffer into the compositor grid. Built-in apps (tutorial,
//! notification center, later the file manager etc.), agent windows and
//! devkit panels all implement [`App`] (ARCHITECTURE.md §9).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ratatui_core::buffer::{Buffer, CellDiffOption};
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::style::{Color as TuiColor, Modifier};
use termos_render::cell::{Attrs, Style};
use termos_render::color::Color;
use termos_render::graphics::Picture;
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

use crate::input::{KeyEvent, MouseEvent};

/// What an app wants the desktop to do after handling an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEffect {
    /// Nothing visible changed.
    None,
    /// Redraw the app.
    Redraw,
    /// Close the app's window.
    Close,
    /// Show a status-line message.
    Notify(String),
    /// Run a named desktop command (as if chosen in the palette).
    Command(String),
    /// Put text on the clipboard (every client's terminal, through OSC
    /// 52) and in the clipboard history.
    Copy(String),
    /// Close the app's window, then paste text into the pane that gets
    /// the focus (as if typed with bracketed paste).
    PasteAndClose(String),
    /// Text for the agent working in a worktree (`dir`): typed into its
    /// agent pane as its next message, or, with none running, given to a
    /// new one there as its first (D-114).
    TellAgent {
        worktree: String,
        dir: std::path::PathBuf,
        text: String,
    },
}

/// An image an app shows. The desktop draws it for each client the best
/// way that client's terminal allows (ARCHITECTURE.md §5.8): pixels, or
/// block characters where it cannot.
#[derive(Debug, Clone)]
pub struct AppImage {
    /// Identifies the picture; the same key means the same pixels (so
    /// terminals receive it once). Get one from [`new_image_key`].
    pub key: u64,
    pub picture: Arc<Picture>,
    /// Where it goes, in cells relative to the app's area. The picture is
    /// fitted inside, keeping its aspect ratio, and centred.
    pub rect: Rect,
    /// Fit larger than the picture's own pixel size.
    pub upscale: bool,
    /// Rows of `rect` scrolled above the app's area: `rect` then starts
    /// this many rows higher, and only its lower part shows (a picture in
    /// a document scrolled partly out of view).
    pub scroll_top: u16,
    /// Shown only in pixels (D-090): the app draws a text version beneath
    /// (a braille chart), which terminals without pixel graphics keep and
    /// the desktop clears where the picture shows.
    pub pixels_only: bool,
}

/// A fresh key for an [`AppImage`].
pub fn new_image_key() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Lets an app ask for a redraw from another thread or task, when work it
/// started (decoding an image, say) has finished.
#[derive(Clone)]
pub struct AppWaker(Arc<dyn Fn() + Send + Sync>);

impl AppWaker {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Asks the desktop to draw the app again.
    pub fn wake(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for AppWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppWaker")
    }
}

/// An in-process window.
pub trait App: Send {
    /// Window title.
    fn title(&self) -> String;
    /// The icon before the title (D-088).
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Apps
    }
    /// A key press while the app has focus.
    fn handle_key(&mut self, key: KeyEvent) -> AppEffect;
    /// A mouse event with coordinates relative to the app's area.
    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        let _ = event;
        AppEffect::None
    }
    /// Text pasted while the app has focus (bracketed paste).
    fn handle_paste(&mut self, text: &str) -> AppEffect {
        let _ = text;
        AppEffect::None
    }
    /// Draws the app into `buf`, which covers `area` (origin 0,0).
    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme);
    /// Images to show over the app's cells (called after `render`, for
    /// the same area). The cells under an image should be left blank.
    fn images(&self) -> Vec<AppImage> {
        Vec::new()
    }
    /// Waiting on something (a reading, a reply, a file): the desktop
    /// shows a spinner in the window's title (D-090).
    fn busy(&self) -> bool {
        false
    }
    /// Takes in background work that finished, before the desktop asks
    /// for the title and [`App::busy`] and draws the app.
    fn catch_up(&mut self) {}
    /// Gives the app a way to ask for a redraw later (called once, when
    /// the window opens).
    fn set_waker(&mut self, waker: AppWaker) {
        let _ = waker;
    }
    /// When the app next wants [`App::on_timer`] (a monitor's next
    /// sample). The desktop asks only while the app is on screen for some
    /// client, so hidden apps cost nothing (D-028).
    fn deadline(&self) -> Option<Instant> {
        None
    }
    /// The deadline passed (or the app came back on screen after it did).
    fn on_timer(&mut self, now: Instant) -> AppEffect {
        let _ = now;
        AppEffect::None
    }
    /// The palette command that opens this window again as it is now (the
    /// folder, file or view it shows), so it is saved with the session
    /// and comes back with it (D-104). `None` (the default): the window
    /// is not restored.
    fn reopen(&self) -> Option<String> {
        None
    }
}

/// Windows whose code lives in crates the shell does not depend on
/// (agents, ARCHITECTURE.md §4): the `termos` binary gives the desktop a
/// launcher, and the desktop asks it to open them.
pub trait Launcher: Send + Sync {
    /// Opens a window of `kind`; `arg` is the command's argument (a
    /// profile, a session id, or empty).
    fn open(
        &self,
        kind: LaunchKind,
        arg: &str,
        ctx: &LaunchContext<'_>,
    ) -> Result<Box<dyn App>, String>;

    /// The agent CLIs' accounts, for the menus (D-097): each CLI's
    /// `default` first.
    fn accounts(&self, _ctx: &LaunchContext<'_>) -> Vec<AccountInfo> {
        Vec::new()
    }

    /// Makes `name` the account `cli` uses from now on; what to tell the
    /// user.
    fn use_account(
        &self,
        cli: &str,
        _name: &str,
        _ctx: &LaunchContext<'_>,
    ) -> Result<String, String> {
        Err(format!("{cli} has no accounts here"))
    }

    /// The worktree `name` (empty: the one `ctx.cwd` is in) as a workspace
    /// is laid out (D-114).
    fn worktree(&self, name: &str, _ctx: &LaunchContext<'_>) -> Result<WorktreeLayout, String> {
        let _ = name;
        Err("worktrees are not part of this build".into())
    }

    /// What a new pane in `dir` gets when `dir` is in a worktree: its
    /// name, branch and ports (D-114). Empty elsewhere.
    fn worktree_env(
        &self,
        _dir: &std::path::Path,
        _ctx: &LaunchContext<'_>,
    ) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// A worktree opened as a workspace (D-114).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeLayout {
    pub name: String,
    pub path: std::path::PathBuf,
    /// The agent in its first pane: an agent CLI (`claude`, `codex`,
    /// `gemini`, run as the account in use) or `termos` (TermOS's agent);
    /// `None`: no agent pane.
    pub agent: Option<String>,
    /// Commands each started in a shell pane of their own.
    pub servers: Vec<String>,
}

/// An account of an agent CLI (Claude Code, Codex), for the menus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    /// `claude`, `codex`.
    pub cli: String,
    /// "Claude Code".
    pub cli_title: String,
    pub name: String,
    /// Its label, else its name.
    pub title: String,
    /// The one in use.
    pub active: bool,
}

impl std::fmt::Debug for dyn Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Launcher")
    }
}

/// What a [`Launcher`] opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchKind {
    /// A new agent session (`agent.open [profile]`).
    Agent,
    /// A saved agent conversation, or a list to pick one
    /// (`agent.resume [id]`).
    AgentResume,
    /// The audit log viewer (`audit.open [session]`).
    Audit,
    /// The command center (`center.open`, D-093).
    Center,
    /// The usage app: every agent CLI account (`usage.open`, D-097).
    Usage,
    /// The coding harness, planning the argument as its goal
    /// (`harness.open [goal]`, D-100).
    Harness,
    /// The package manager: mise, the dev rootfs, the host, Podman
    /// (`packages.open`, D-110).
    Packages,
    /// A worktree's review window (`worktree.review [name]`, D-114).
    WorktreeReview,
    /// The harness running a best-of-N: "SPEC PROMPT" (`harness.best`,
    /// D-114).
    HarnessBest,
}

/// What the desktop knows when it opens a launched window.
#[derive(Debug, Clone, Copy)]
pub struct LaunchContext<'a> {
    pub config: &'a crate::config::Config,
    pub paths: &'a crate::paths::Paths,
    /// The focused pane's folder (an agent's workspace).
    pub cwd: Option<&'a std::path::Path>,
    /// The server's socket, for windows that use its control API (the
    /// command center).
    pub socket: Option<&'a std::path::Path>,
    /// The session the window opens in.
    pub session: &'a str,
}

/// Renders `app` into `grid` at `at`.
pub fn render_app(app: &mut dyn App, theme: &Theme, grid: &mut Grid, at: Rect) {
    if at.is_empty() {
        return;
    }
    let area = TuiRect::new(0, 0, at.w, at.h);
    let mut buf = Buffer::empty(area);
    app.render(area, &mut buf, theme);
    blit_buffer(&buf, grid, at);
}

/// Copies a ratatui buffer into the grid at `at` (clipped to `at`).
pub fn blit_buffer(buf: &Buffer, grid: &mut Grid, at: Rect) {
    let area = buf.area;
    let mut skip_next = false;
    for (i, cell) in buf.content().iter().enumerate() {
        let Ok(i) = u16::try_from(i) else { break };
        let (cx, cy) = (i % area.width.max(1), i / area.width.max(1));
        if cx == 0 {
            skip_next = false;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if cell.diff_option == CellDiffOption::Skip || cx >= at.w || cy >= at.h {
            continue;
        }
        let style = convert_style(cell.fg, cell.bg, cell.modifier);
        let symbol = cell.symbol();
        let x = at.x + cx;
        let y = at.y + cy;
        if symbol.is_empty() {
            grid.put_str(x, y, " ", style, at.right());
        } else {
            grid.put_str(x, y, symbol, style, at.right());
            if UnicodeWidthStr::width(symbol) >= 2 {
                skip_next = true;
            }
        }
    }
}

fn convert_color(c: TuiColor) -> Color {
    match c {
        TuiColor::Reset => Color::Default,
        TuiColor::Black => Color::Indexed(0),
        TuiColor::Red => Color::Indexed(1),
        TuiColor::Green => Color::Indexed(2),
        TuiColor::Yellow => Color::Indexed(3),
        TuiColor::Blue => Color::Indexed(4),
        TuiColor::Magenta => Color::Indexed(5),
        TuiColor::Cyan => Color::Indexed(6),
        TuiColor::Gray => Color::Indexed(7),
        TuiColor::DarkGray => Color::Indexed(8),
        TuiColor::LightRed => Color::Indexed(9),
        TuiColor::LightGreen => Color::Indexed(10),
        TuiColor::LightYellow => Color::Indexed(11),
        TuiColor::LightBlue => Color::Indexed(12),
        TuiColor::LightMagenta => Color::Indexed(13),
        TuiColor::LightCyan => Color::Indexed(14),
        TuiColor::White => Color::Indexed(15),
        TuiColor::Rgb(r, g, b) => Color::rgb(r, g, b),
        TuiColor::Indexed(i) => Color::Indexed(i),
    }
}

fn convert_style(fg: TuiColor, bg: TuiColor, m: Modifier) -> Style {
    let mut attrs = Attrs::empty();
    for (from, to) in [
        (Modifier::BOLD, Attrs::BOLD),
        (Modifier::DIM, Attrs::DIM),
        (Modifier::ITALIC, Attrs::ITALIC),
        (Modifier::UNDERLINED, Attrs::UNDERLINE),
        (Modifier::SLOW_BLINK, Attrs::BLINK),
        (Modifier::RAPID_BLINK, Attrs::BLINK),
        (Modifier::REVERSED, Attrs::REVERSE),
        (Modifier::HIDDEN, Attrs::HIDDEN),
        (Modifier::CROSSED_OUT, Attrs::STRIKE),
    ] {
        if m.contains(from) {
            attrs |= to;
        }
    }
    let mut s = Style::new().fg(convert_color(fg)).bg(convert_color(bg));
    s.attrs = attrs;
    s
}

/// Converts a TermOS style to a ratatui style, for apps drawing with theme
/// colours.
pub fn tui_style(s: Style) -> ratatui_core::style::Style {
    let conv = |c: Color| match c {
        Color::Default => TuiColor::Reset,
        Color::Indexed(i) => TuiColor::Indexed(i),
        Color::Rgb(rgb) => TuiColor::Rgb(rgb.r, rgb.g, rgb.b),
    };
    let mut m = Modifier::empty();
    for (from, to) in [
        (Attrs::BOLD, Modifier::BOLD),
        (Attrs::DIM, Modifier::DIM),
        (Attrs::ITALIC, Modifier::ITALIC),
        (Attrs::UNDERLINE, Modifier::UNDERLINED),
        (Attrs::REVERSE, Modifier::REVERSED),
    ] {
        if s.attrs.contains(from) {
            m |= to;
        }
    }
    ratatui_core::style::Style::default()
        .fg(conv(s.fg))
        .bg(conv(s.bg))
        .add_modifier(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_core::text::Line;
    use ratatui_core::widgets::Widget;

    struct Hello;

    impl App for Hello {
        fn title(&self) -> String {
            "hello".into()
        }
        fn handle_key(&mut self, _key: KeyEvent) -> AppEffect {
            AppEffect::Close
        }
        fn render(&mut self, area: TuiRect, buf: &mut Buffer, _theme: &Theme) {
            Line::from("hi 界!")
                .style(ratatui_core::style::Style::default().fg(TuiColor::Red))
                .render(area, buf);
        }
    }

    #[test]
    fn app_output_lands_in_the_grid() {
        let mut grid = Grid::new(12, 3);
        render_app(
            &mut Hello,
            &Theme::default(),
            &mut grid,
            Rect::new(2, 1, 8, 1),
        );
        assert_eq!(grid.to_text(), "\n  hi 界!\n\n");
        assert_eq!(grid.get(2, 1).map(|c| c.style.fg), Some(Color::Indexed(1)));
    }

    #[test]
    fn style_conversion_round_trips_colours() {
        let s = Style::new()
            .fg(Color::rgb(1, 2, 3))
            .bg(Color::Indexed(4))
            .bold();
        let t = tui_style(s);
        let back = convert_style(
            t.fg.unwrap_or(TuiColor::Reset),
            t.bg.unwrap_or(TuiColor::Reset),
            t.add_modifier,
        );
        assert_eq!(back, s);
    }
}
