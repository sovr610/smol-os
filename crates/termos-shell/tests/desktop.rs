// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end tests of the desktop with real shell panes (`/bin/sh`, or
//! `cmd.exe` on Windows), driven headlessly (no terminal involved).

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use termos_core::config::Config;
use termos_core::ids::{ClientId, PaneId};

/// The one client these tests attach.
const C: ClientId = ClientId(1);
use termos_core::input::{KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use termos_core::paths::Paths;
use termos_core::proto::{ClientMsg, ServerMsg};
use termos_render::caps::Caps;
use termos_render::grid::Grid;
use termos_shell::{Desktop, DesktopOptions};
use termos_term::pty::PtyEvent;
use tokio::sync::mpsc;

const COLS: u16 = 100;
const ROWS: u16 = 30;

struct Harness {
    desktop: Desktop,
    clipboard: termos_core::clipboard::SharedClipboard,
    pty_rx: mpsc::Receiver<PtyEvent>,
    grid: Grid,
    dir: tempfile::TempDir,
    outbox: Vec<ServerMsg>,
}

fn base_config(extra: &str) -> String {
    format!("{}{extra}\n", common::shell_config())
}

impl Harness {
    fn new(extra: &str) -> Self {
        Self::with_tutorial(extra, false)
    }

    fn with_tutorial(extra: &str, tutorial: bool) -> Self {
        Self::with_options(extra, tutorial, false)
    }

    /// A harness whose panes run `exe` for `termos` subcommands.
    #[cfg(unix)]
    fn with_termos(exe: PathBuf) -> Self {
        Self::build("", false, false, Some(exe))
    }

    fn with_options(extra: &str, tutorial: bool, animations: bool) -> Self {
        Self::build(extra, tutorial, animations, None)
    }

    fn build(extra: &str, tutorial: bool, animations: bool, termos_exe: Option<PathBuf>) -> Self {
        Self::build_ui(extra, tutorial, animations, termos_exe, "splash = false")
    }

    /// A harness with more `[ui]` settings (`ui` is a few `key = value`
    /// lines); a file `wall.png` beside the config is made first when
    /// `wallpaper` is given (green, 64×32).
    fn with_ui(ui: &str, animations: bool, wallpaper: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        if wallpaper {
            let config = dir.path().join("config");
            std::fs::create_dir_all(&config).expect("mkdir");
            let pic = termos_render::graphics::Picture::from_rgba(
                64,
                32,
                [0, 200, 0, 255].repeat(64 * 32),
            )
            .expect("valid");
            std::fs::write(config.join("wall.png"), pic.to_png()).expect("write");
        }
        Self::build_in(dir, "", false, animations, None, ui)
    }

    fn build_ui(
        extra: &str,
        tutorial: bool,
        animations: bool,
        termos_exe: Option<PathBuf>,
        ui: &str,
    ) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        Self::build_in(dir, extra, tutorial, animations, termos_exe, ui)
    }

    fn build_in(
        dir: tempfile::TempDir,
        extra: &str,
        tutorial: bool,
        animations: bool,
        termos_exe: Option<PathBuf>,
        ui: &str,
    ) -> Self {
        let paths = Paths {
            config_dir: dir.path().join("config"),
            state_dir: dir.path().join("state"),
            runtime_dir: dir.path().join("run"),
            data_dir: dir.path().join("data"),
        };
        std::fs::create_dir_all(&paths.config_dir).expect("mkdir");
        // Animations off: screens are read at arbitrary moments.
        // Most tests start as one tiled terminal; desktop-first is
        // tested on its own.
        let start = if ui.contains("start =") {
            ""
        } else {
            "start = \"terminal\"\n"
        };
        let tutorial_cfg = format!(
            "[tutorial]\nshow-on-first-run = {tutorial}\n[status]\nclock = false\n[ui]\nanimations = {animations}\n{start}{ui}\n"
        );
        let text = base_config(&format!("{extra}\n{tutorial_cfg}"));
        std::fs::write(paths.config_file(), &text).expect("write config");
        let config = Config::load(&paths.config_file()).expect("valid test config");
        let (tx, pty_rx) = mpsc::channel(1024);
        let clipboard = termos_core::clipboard::ClipboardHistory::shared(50);
        let opts = DesktopOptions {
            clipboard: std::sync::Arc::clone(&clipboard),
            termos_exe,
            session_name: "test".into(),
            config_path: paths.config_file(),
            paths,
            env_shell: None,
            pane_ids: termos_core::ids::PaneIds::new(),
            pane_env: common::pane_env(),
            cwd: None,
            launcher: None,
            server_socket: None,
        };
        let mut desktop = Desktop::new(opts, config, tx, COLS, ROWS).expect("desktop starts");
        desktop.handle_client(
            C,
            ClientMsg::Hello {
                version: termos_core::proto::PROTOCOL_VERSION,
                caps: Caps::default(),
                cols: COLS,
                rows: ROWS,
            },
        );
        Self {
            desktop,
            clipboard,
            pty_rx,
            grid: Grid::new(COLS, ROWS),
            dir,
            outbox: Vec::new(),
        }
    }

    /// The clipboard history, newest first: (text, source).
    fn history(&self) -> Vec<(String, String)> {
        let h = self.clipboard.lock().expect("not poisoned");
        h.entries()
            .map(|e| (e.text.clone(), e.source.to_string()))
            .collect()
    }

    fn config_path(&self) -> PathBuf {
        self.dir.path().join("config").join("config.toml")
    }

    fn key(&mut self, spec: &str) {
        let k: KeyEvent = spec.parse().expect("key spec");
        self.desktop.handle_client(C, ClientMsg::Key(k));
        self.outbox
            .extend(self.desktop.take_outbox().into_iter().map(|(_, m)| m));
    }

    fn keys(&mut self, specs: &str) {
        for s in specs.split_whitespace() {
            self.key(s);
        }
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let k = match c {
                '\n' => KeyEvent::plain(termos_core::input::KeyCode::Enter),
                c => KeyEvent::ch(c),
            };
            self.desktop.handle_client(C, ClientMsg::Key(k));
        }
    }

    fn mouse(&mut self, kind: MouseKind, x: u16, y: u16) {
        self.desktop.handle_client(
            C,
            ClientMsg::Mouse(MouseEvent {
                kind,
                x,
                y,
                mods: Mods::empty(),
            }),
        );
    }

    fn screen(&mut self) -> String {
        self.desktop.render(C, &mut self.grid);
        self.outbox
            .extend(self.desktop.take_outbox().into_iter().map(|(_, m)| m));
        self.grid.to_text()
    }

    /// Processes PTY output until `pred(screen)` holds (or panics after a
    /// timeout, printing the screen).
    async fn wait_for(&mut self, what: &str, pred: impl Fn(&str, &Desktop) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let screen = self.screen();
            if pred(&screen, &self.desktop) {
                return screen;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {what}; screen:\n{screen}"
            );
            if let Ok(Some(ev)) =
                tokio::time::timeout(left.min(Duration::from_millis(200)), self.pty_rx.recv()).await
            {
                self.desktop.on_pty(ev);
            }
        }
    }

    /// Like [`Harness::wait_for`], looking at the rendered grid.
    async fn wait_grid(&mut self, what: &str, pred: impl Fn(&Grid) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            self.screen();
            if pred(&self.grid) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}");
            if let Ok(Some(ev)) =
                tokio::time::timeout(left.min(Duration::from_millis(200)), self.pty_rx.recv()).await
            {
                self.desktop.on_pty(ev);
            }
        }
    }

    async fn wait_text(&mut self, needle: &str) -> String {
        let n = needle.to_owned();
        self.wait_for(needle, move |s, _| s.contains(&n)).await
    }

    fn panes(&self) -> Vec<PaneId> {
        self.desktop.session().active().panes()
    }
}

#[tokio::test]
async fn starts_with_a_shell_and_a_status_line() {
    let mut h = Harness::new("");
    h.type_text(&format!("{}\n", common::echo_split("he", "llo")));
    let screen = h.wait_text("hello\n").await;
    let status = screen.lines().last().unwrap_or("");
    // The start button, the session, the tabs and a `+` for a new one.
    assert!(status.starts_with(" ◆ Menu  [test]  0:"), "{status:?}");
    assert!(status.contains("*  +"), "{status:?}");
    assert_eq!(h.panes().len(), 1);
}

#[tokio::test]
async fn themes_follow_each_terminal_and_can_be_picked() {
    use termos_render::color::{Color, Rgb};
    use termos_render::theme::ThemeColors;

    let mut h = Harness::new("");
    // A second terminal, with a light background.
    const L: ClientId = ClientId(2);
    h.desktop.handle_client(
        L,
        ClientMsg::Hello {
            version: termos_core::proto::PROTOCOL_VERSION,
            caps: Caps {
                background: Some(Rgb::new(250, 250, 250)),
                ..Caps::default()
            },
            cols: COLS,
            rows: ROWS,
        },
    );
    let status_bg = |d: &mut Desktop, client: ClientId| -> Color {
        let mut g = Grid::new(COLS, ROWS);
        d.render(client, &mut g);
        g.get(COLS - 1, ROWS - 1).expect("status cell").style.bg
    };
    assert_eq!(status_bg(&mut h.desktop, C), ThemeColors::dark().status_bg);
    assert_eq!(status_bg(&mut h.desktop, L), ThemeColors::light().status_bg);

    // The first client picks high contrast in the palette; the other
    // keeps its own theme.
    h.key("C-Space");
    h.type_text("theme high");
    h.key("Enter");
    assert_eq!(
        status_bg(&mut h.desktop, C),
        ThemeColors::high_contrast().status_bg
    );
    assert_eq!(status_bg(&mut h.desktop, L), ThemeColors::light().status_bg);
    assert!(h.screen().contains("theme: high-contrast"));

    // A config with a fixed base applies to the client without a pick;
    // the pick survives the reload.
    std::fs::write(
        h.config_path(),
        base_config("[theme]\nbase = \"dark\"\n[status]\nclock = false\n[tutorial]\nshow-on-first-run = false\n[ui]\nanimations = false\nstart = \"terminal\"\n"),
    )
    .expect("write config");
    h.desktop.reload_config();
    assert_eq!(status_bg(&mut h.desktop, L), ThemeColors::dark().status_bg);
    assert_eq!(
        status_bg(&mut h.desktop, C),
        ThemeColors::high_contrast().status_bg
    );
}

/// A program that turns on the Kitty keyboard protocol gets Esc and
/// Ctrl+letter as `CSI u`; when it turns the protocol off, legacy again.
#[cfg(unix)]
#[tokio::test]
async fn programs_get_kitty_keys_when_they_ask() {
    let mut h = Harness::new("");
    h.type_text("printf '\\033[>1u'; cat -v\n");
    h.wait_for("cat running", |_, d| {
        d.focused_modes().is_some_and(|m| m.kitty_flags == 1)
    })
    .await;
    h.key("Escape");
    h.key("C-a");
    h.type_text("x\n");
    h.wait_text("^[[27u^[[97;5ux").await;
    h.key("C-c");
}

/// A float opening animates for a local client and not for one over SSH:
/// about eight changed frames for one, none for the other, then no timer.
#[tokio::test]
async fn floats_animate_only_where_they_should() {
    use termos_shell::anim::{DURATION, Kind};
    use termos_shell::commands::Command;

    let mut h = Harness::with_options("", false, true);
    const SSH: ClientId = ClientId(2);
    h.desktop.handle_client(
        SSH,
        ClientMsg::Hello {
            version: termos_core::proto::PROTOCOL_VERSION,
            caps: Caps {
                remote: true,
                ..Caps::default()
            },
            cols: COLS,
            rows: ROWS,
        },
    );
    // Settle both screens before the float opens.
    let mut local = Grid::new(COLS, ROWS);
    let mut remote = Grid::new(COLS, ROWS);
    let t0 = Instant::now();
    h.desktop.render_at(C, &mut local, t0);
    h.desktop.render_at(SSH, &mut remote, t0);

    // The tutorial is an app window: no program output changes it.
    h.desktop.run(Command::Tutorial);
    let id = h.desktop.focused().expect("the tutorial has focus");
    let start = h
        .desktop
        .animations()
        .started(Kind::FloatOpen(id))
        .expect("animating");

    // A frame mid-animation, at a fixed moment.
    let mut mid = Grid::new(COLS, ROWS);
    h.desktop.render_at(C, &mut mid, start + DURATION / 2);
    insta::assert_snapshot!("float-opening-mid", mid.to_text());

    let (mut local_changes, mut remote_changes) = (0, 0);
    let mut frames = 0;
    while let Some(d) = h.desktop.next_deadline() {
        frames += 1;
        assert!(frames < 30, "the animation never ends");
        h.desktop.on_timer(d);
        let (before_l, before_r) = (local.clone(), remote.clone());
        h.desktop.render_at(C, &mut local, d);
        h.desktop.render_at(SSH, &mut remote, d);
        local_changes += usize::from(local != before_l);
        remote_changes += usize::from(remote != before_r);
    }
    // The first remote render after opening shows the float; after that
    // it never changes. The local one grows over ~8 frames.
    let mut final_remote = Grid::new(COLS, ROWS);
    h.desktop
        .render_at(SSH, &mut final_remote, start + DURATION * 2);
    assert_eq!(remote, final_remote, "the SSH client saw the final frame");
    assert!(
        remote_changes <= 1,
        "SSH client got {remote_changes} changed frames"
    );
    assert!(
        (5..=10).contains(&local_changes),
        "{local_changes} changed frames"
    );
    assert_eq!(local.to_text(), remote.to_text(), "both end the same");
    assert_eq!(h.desktop.next_deadline(), None, "no timer once it ends");
}

/// Without pixel graphics an image is drawn in block characters, and a
/// float opened over it covers it like any other cells.
#[tokio::test]
async fn images_fall_back_to_blocks_that_floats_cover() {
    use termos_render::color::{Color, Rgb};
    use termos_render::graphics::Picture;
    use termos_shell::commands::Command;

    let mut h = Harness::new("");
    let path = h.dir.path().join("halves.png");
    let pic = Picture::from_rgba(32, 16, {
        let mut px = Vec::new();
        for _y in 0..16 {
            for x in 0..32 {
                px.extend_from_slice(if x < 16 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        px
    })
    .expect("valid");
    std::fs::write(&path, pic.to_png()).expect("write");
    h.desktop
        .run(Command::ImageOpen(path.display().to_string()));
    let image_cells = |g: &Grid| {
        let mut n = 0;
        for y in 0..g.rows() {
            for x in 0..g.cols() {
                let bg = g.get(x, y).map(|c| c.style.bg);
                if bg == Some(Color::Rgb(Rgb::new(255, 0, 0)))
                    || bg == Some(Color::Rgb(Rgb::new(0, 0, 255)))
                {
                    n += 1;
                }
            }
        }
        n
    };
    // Decoding finishes on another thread and wakes the desktop.
    h.wait_grid("the image in blocks", |g| image_cells(g) > 100)
        .await;
    let before = image_cells(&h.grid);
    assert!(
        h.desktop.pixel_images(C).is_empty(),
        "this client has no pixel graphics"
    );

    h.desktop.run(Command::Tutorial);
    h.screen();
    let after = image_cells(&h.grid);
    assert!(after < before && after > 0, "{before} → {after}");
}

/// A markdown document with a local picture: text wrapped in its window,
/// the picture in blocks below it, scrolling with the text.
#[tokio::test]
async fn markdown_with_a_picture() {
    use termos_render::color::{Color, Rgb};
    use termos_render::graphics::Picture;
    use termos_shell::commands::Command;

    let mut h = Harness::new("");
    let dir = h.dir.path().join("docs");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let pic = Picture::from_rgba(200, 100, [0, 200, 0, 255].repeat(200 * 100)).expect("valid");
    std::fs::write(dir.join("green.png"), pic.to_png()).expect("write");
    std::fs::write(
        dir.join("README.md"),
        "# Hello docs\n\nSome *text* before the picture.\n\n![green](green.png)\n\nAfter it.\n",
    )
    .expect("write");
    h.desktop.run(Command::MarkdownOpen(
        dir.join("README.md").display().to_string(),
    ));
    let green = |g: &Grid| {
        let mut n = 0;
        for y in 0..g.rows() {
            for x in 0..g.cols() {
                if g.get(x, y).map(|c| c.style.bg) == Some(Color::Rgb(Rgb::new(0, 200, 0))) {
                    n += 1;
                }
            }
        }
        n
    };
    h.wait_grid("the picture in blocks", |g| green(g) > 4).await;
    let screen = h.screen();
    assert!(screen.contains("Markdown · README.md"), "{screen}");
    assert!(screen.contains("Hello docs"), "{screen}");
    assert!(screen.contains("Some text before the picture."), "{screen}");
    assert!(screen.contains("After it."), "{screen}");
}

/// The file manager lists a folder (folders first), previews what is
/// selected, asks before trashing, and opens files in the right app.
#[tokio::test]
async fn file_manager_lists_previews_and_opens() {
    use termos_shell::commands::Command;

    let mut h = Harness::new("");
    let d = h.dir.path().join("work");
    std::fs::create_dir_all(d.join("sub")).expect("mkdir");
    std::fs::write(d.join("b.txt"), "plain text inside\n").expect("write");
    std::fs::write(d.join("notes.md"), "# Big notes\n\nhello\n").expect("write");
    h.desktop.run(Command::FilesOpen(d.display().to_string()));
    let screen = h.wait_text("notes.md").await;
    assert!(screen.contains("Files · work"), "{screen}");
    let row = |s: &str, name: &str| s.lines().position(|l| l.contains(name));
    assert!(
        row(&screen, "sub/") < row(&screen, "b.txt"),
        "folders first: {screen}"
    );
    h.key("j");
    h.wait_text("plain text inside").await;
    h.key("j");
    h.wait_text("Big notes").await;
    // Trashing asks first; n keeps the file.
    h.key("d");
    h.wait_text("Move notes.md to the trash? y/n").await;
    h.key("n");
    assert!(d.join("notes.md").exists());
    // Enter opens markdown in its viewer.
    h.type_text("\n");
    h.wait_text("Markdown · notes.md").await;
}

/// The system monitor reads the machine only while it is on screen.
#[tokio::test]
async fn the_monitor_samples_only_while_visible() {
    use termos_shell::commands::Command;

    let mut h = Harness::new("");
    assert_eq!(h.desktop.next_deadline(), None, "idle: no timers");
    h.desktop.run(Command::MonitorOpen);
    let first = h.desktop.next_deadline().expect("a first reading is due");
    h.desktop.on_timer(first);
    h.wait_text("Memory").await;
    let next = h.desktop.next_deadline().expect("the next reading");
    assert!(next > first, "one second later");
    // Minimized: nobody sees it, so no timer.
    h.desktop.run(Command::Minimize);
    assert_eq!(h.desktop.next_deadline(), None);
    h.desktop.run(Command::RestoreMinimized);
    assert_eq!(h.desktop.next_deadline(), Some(next));
    // Closed: gone for good.
    h.desktop.run(Command::KillPane);
    assert_eq!(h.desktop.next_deadline(), None);
}

/// prefix = lists what was copied; Enter pastes it into the pane.
#[tokio::test]
async fn the_clipboard_history_pastes_back() {
    let mut h = Harness::new("");
    // Quoted so the output differs from the command, in every shell.
    let command = common::echo_split("from-hist", "ory");
    h.clipboard
        .lock()
        .expect("not poisoned")
        .push(&command, termos_core::clipboard::ClipSource::Selection);
    h.keys("C-b =");
    h.wait_text("Clipboard").await;
    h.wait_text(&command).await;
    h.type_text("\n");
    // The window closed and the text went to the shell: run it.
    h.type_text("\n");
    h.wait_for("the pasted command's output", |s, _| {
        s.lines()
            .any(|l| l.trim_end().ends_with("from-history") && !l.contains("echo"))
    })
    .await;
}

/// Settings write the config file and the desktop picks the change up.
#[tokio::test]
async fn settings_change_the_config() {
    use termos_shell::commands::Command;

    let mut h = Harness::new("");
    h.desktop.run(Command::SettingsOpen);
    h.wait_text("Window borders").await;
    // The first setting is the theme: auto → dark.
    h.key("Right");
    let text = std::fs::read_to_string(h.config_path()).expect("config");
    assert!(
        text.contains("[theme]") && text.contains("base = \"dark\""),
        "{text}"
    );
    // The original settings are still there.
    assert!(text.contains("[tutorial]"), "{text}");
    h.wait_text("config reloaded").await;
}

/// Task panels and the Git UI run `termos` subcommands in panes: here a
/// stand-in that prints its arguments and waits.
#[cfg(unix)]
#[tokio::test]
async fn task_panels_and_git_run_termos() {
    use std::os::unix::fs::PermissionsExt;
    use termos_shell::commands::Command;

    let stub_dir = tempfile::tempdir().expect("tempdir");
    let stub = stub_dir.path().join("termos");
    std::fs::write(&stub, "#!/bin/sh\necho \"stub-termos $*\"\nsleep 30\n").expect("write");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let mut h = Harness::with_termos(stub);
    h.desktop.run(Command::Task("build".into()));
    h.wait_text("stub-termos task build --wait").await;
    assert_eq!(h.panes().len(), 2, "the panel is a pane of its own");
    // Running it again replaces the panel instead of adding one.
    h.desktop.run(Command::Task("build".into()));
    h.wait_text("stub-termos task build --wait").await;
    assert_eq!(h.panes().len(), 2);
    h.desktop.run(Command::Task("test".into()));
    h.wait_text("stub-termos task test --wait").await;
    assert_eq!(h.panes().len(), 3);
    h.keys("C-b g");
    h.wait_text("stub-termos git").await;
    assert_eq!(h.panes().len(), 4);
}

#[tokio::test]
async fn split_right_type_and_see_separator() {
    let mut h = Harness::new("");
    h.keys("C-b %");
    assert_eq!(h.panes().len(), 2);
    let focused = h.desktop.focused();
    assert_eq!(focused, Some(h.panes()[1]));
    h.type_text(&format!("{}\n", common::echo_split("ri", "ght")));
    // The output line in the right-hand pane ends with "right" (it may
    // follow a prompt when the keys were typed before the shell printed
    // it); the echoed command line contains "echo".
    let is_output = |l: &str| {
        l.split('│')
            .nth(1)
            .is_some_and(|r| r.trim_end().ends_with("right") && !r.contains("echo"))
    };
    let screen = h
        .wait_for("output in the right pane", move |s, _| {
            s.lines().any(is_output)
        })
        .await;
    assert!(screen.contains('│'), "{screen}");
    let line = screen.lines().find(|l| is_output(l)).unwrap_or("");
    assert!(
        line.find("right").unwrap_or(0) > usize::from(COLS / 2),
        "{line:?}"
    );
    h.keys("C-b \"");
    assert_eq!(h.panes().len(), 3);
    let screen = h.screen();
    assert!(screen.contains('├'), "junction expected:\n{screen}");
}

#[tokio::test]
async fn zoom_float_and_layouts() {
    let mut h = Harness::new("");
    h.keys("C-b % C-b z");
    assert!(h.desktop.session().active().zoomed().is_some());
    h.keys("C-b z C-b f");
    let focused = h.desktop.focused().expect("focus");
    assert!(h.desktop.session().active().is_floating(focused));
    let screen = h.screen();
    assert!(screen.contains('╭') && screen.contains('×'), "{screen}");
    h.keys("C-b f");
    assert!(!h.desktop.session().active().is_floating(focused));
    h.keys("C-b % C-b M-5");
    assert_eq!(h.panes().len(), 3);
    h.keys("C-b Space");
    assert!(h.desktop.toast().is_some_and(|t| t.starts_with("layout:")));
}

#[tokio::test]
async fn workspaces_new_switch_rename() {
    let mut h = Harness::new("");
    h.keys("C-b c");
    assert_eq!(h.desktop.session().workspaces.len(), 2);
    assert_eq!(h.desktop.session().active_index(), 1);
    h.keys("C-b 0");
    assert_eq!(h.desktop.session().active_index(), 0);
    h.keys("C-b ,");
    assert!(h.desktop.prompt_open(C));
    h.key("C-u");
    h.type_text("web\n");
    assert!(!h.desktop.prompt_open(C));
    let screen = h.screen();
    let status = screen.lines().last().unwrap_or("");
    assert!(status.contains("0:web*"), "{status:?}");
    assert!(status.contains("1:"), "{status:?}");
    h.keys("C-b l");
    assert_eq!(h.desktop.session().active_index(), 1);
}

#[tokio::test]
async fn copy_mode_search_select_yank() {
    let mut h = Harness::new("");
    h.type_text(&format!("{}\n", common::echo_split("cop", "ytext")));
    h.wait_for("output line", |s, _| {
        s.lines()
            .any(|l| l.trim_end().ends_with("copytext") && !l.contains("echo"))
    })
    .await;
    h.keys("C-b [");
    assert!(h.desktop.in_copy_mode());
    h.key("?");
    h.type_text("copytext\n");
    h.keys("v e y");
    assert!(!h.desktop.in_copy_mode());
    assert_eq!(h.desktop.paste_buffer(), Some("copytext"));
    assert!(
        h.outbox
            .iter()
            .any(|m| *m == ServerMsg::Clipboard("copytext".into()))
    );
    assert_eq!(
        h.history(),
        vec![("copytext".to_owned(), "selection".to_owned())]
    );
    // Paste it back.
    h.keys("C-b ]");
    h.type_text("\n");
    h.wait_for("pasted command output", |s, _| {
        s.matches("copytext").count() >= 3
    })
    .await;
}

/// Programs that copy (OSC 52) land in the clipboard history too.
#[cfg(unix)]
#[tokio::test]
async fn programs_copies_are_remembered() {
    let mut h = Harness::new("");
    h.type_text("printf '\\033]52;c;aGk=\\007'; echo do''ne\n");
    h.wait_text("done").await;
    let hist = h.history();
    assert_eq!(hist.len(), 1, "{hist:?}");
    assert_eq!(hist[0].0, "hi");
    assert_eq!(h.desktop.paste_buffer(), Some("hi"));
}

#[tokio::test]
async fn palette_runs_commands() {
    let mut h = Harness::new("");
    h.keys("C-b %");
    h.key("C-Space");
    assert!(h.desktop.palette_open(C));
    let screen = h.screen();
    assert!(screen.contains("Command palette"), "{screen}");
    h.type_text("zoom");
    h.key("Enter");
    assert!(!h.desktop.palette_open(C));
    assert!(h.desktop.session().active().zoomed().is_some());
    h.keys("C-b :");
    assert!(h.desktop.palette_open(C));
    h.key("Escape");
    assert!(!h.desktop.palette_open(C));
}

#[tokio::test]
async fn which_key_appears_after_a_pause() {
    let mut h = Harness::new("");
    h.key("C-b");
    assert!(!h.desktop.whichkey_visible(C));
    let deadline = h.desktop.next_deadline().expect("which-key timer armed");
    h.desktop.on_timer(deadline);
    assert!(h.desktop.whichkey_visible(C));
    let screen = h.screen();
    assert!(screen.contains("C-b …"), "{screen}");
    assert!(screen.contains("PREFIX"), "{screen}");
    h.key("z");
    assert!(!h.desktop.whichkey_visible(C));
    // prefix ? shows everything until a key is pressed.
    h.keys("C-b ?");
    assert!(h.screen().contains("Key bindings"));
    h.key("q");
    assert!(!h.desktop.whichkey_visible(C));
}

#[tokio::test]
async fn kill_pane_needs_confirmation_and_exit_closes() {
    let mut h = Harness::new("");
    h.keys("C-b %");
    h.keys("C-b x");
    assert!(h.desktop.prompt_open(C));
    h.key("n");
    assert_eq!(h.panes().len(), 2);
    h.keys("C-b x y");
    assert_eq!(h.panes().len(), 1);
    h.keys("C-b %");
    h.type_text("exit\n");
    h.wait_for("pane to close", |_, d| {
        d.session().active().panes().len() == 1
    })
    .await;
    h.type_text("exit\n");
    h.wait_for("session to end", |_, d| d.quit_reason().is_some())
        .await;
}

#[tokio::test]
async fn send_prefix_and_unbound_keys() {
    let mut h = Harness::new("");
    // C-b C-b sends C-b to the shell; C-b <unbound> does nothing.
    h.keys("C-b C-b C-b Q");
    assert_eq!(h.panes().len(), 1);
    assert!(!h.desktop.prompt_open(C));
}

#[tokio::test]
async fn config_hot_reload_and_errors() {
    let mut h = Harness::new("");
    let path = h.config_path();
    std::fs::write(&path, base_config("prefix = \"C-a\"")).expect("write");
    h.desktop.reload_config();
    assert_eq!(h.desktop.keys().prefix_key, KeyEvent::ctrl('a'));
    h.keys("C-a %");
    assert_eq!(h.panes().len(), 2);
    std::fs::write(&path, "prefix = \"C-\"\n").expect("write");
    h.desktop.reload_config();
    assert_eq!(
        h.desktop.keys().prefix_key,
        KeyEvent::ctrl('a'),
        "bad config keeps the old one"
    );
    let toast = h.desktop.toast().unwrap_or_default().to_owned();
    assert!(toast.contains("config.toml:1:"), "{toast}");
}

/// The row of `text` on `screen`, and its column.
fn find(screen: &str, text: &str) -> Option<(u16, u16)> {
    screen.lines().enumerate().find_map(|(y, line)| {
        line.find(text).map(|byte| {
            let x = unicode_width::UnicodeWidthStr::width(&line[..byte]);
            (
                u16::try_from(x).expect("narrow"),
                u16::try_from(y).expect("short"),
            )
        })
    })
}

#[tokio::test]
async fn the_menu_and_context_menus_work_with_the_mouse() {
    let mut h = Harness::new("");
    h.type_text(&format!("{}\n", common::echo_split("rea", "dy")));
    h.wait_text("ready\n").await;
    let status = ROWS - 1;
    // The start button opens the TermOS menu above the status line.
    h.mouse(MouseKind::Down(MouseButton::Left), 2, status);
    let screen = h.screen();
    assert!(
        screen.contains("TermOS") && screen.contains("New terminal"),
        "{screen}"
    );
    // Hovering "Apps" opens it beside; a press runs one of its items.
    let (x, y) = find(&screen, "Apps").expect("Apps listed");
    h.mouse(MouseKind::Moved, x, y);
    let screen = h.screen();
    let (fx, fy) = find(&screen, "System monitor").expect("the Apps submenu");
    assert!(fx > x, "beside the menu:\n{screen}");
    h.mouse(MouseKind::Down(MouseButton::Left), fx, fy);
    assert_eq!(h.panes().len(), 2, "the monitor opened");
    let screen = h.screen();
    assert!(
        !screen.contains("New terminal"),
        "the menu closed:\n{screen}"
    );
    // Close it again from its title bar's context menu.
    let title_y = find(&screen, "System monitor")
        .map(|(_, y)| y)
        .expect("its title");
    let title_x = find(&screen, "System monitor").expect("title").0;
    h.mouse(MouseKind::Down(MouseButton::Right), title_x, title_y);
    let screen = h.screen();
    assert!(screen.contains("Maximize or restore"), "{screen}");
    let (cx, cy) = find(&screen, "Close").expect("Close");
    h.mouse(MouseKind::Down(MouseButton::Left), cx, cy);
    assert_eq!(h.panes().len(), 1);

    // A right-click in the shell: what can be done with it.
    h.mouse(MouseKind::Down(MouseButton::Right), 10, 5);
    let screen = h.screen();
    assert!(
        screen.contains("Split right") && screen.contains("Paste"),
        "{screen}"
    );
    let (sx, sy) = find(&screen, "Split right").expect("Split right");
    h.mouse(MouseKind::Down(MouseButton::Left), sx, sy);
    assert_eq!(h.panes().len(), 2, "split");
    // Near the bottom, a menu opens upward and leaves the status line be.
    h.mouse(MouseKind::Down(MouseButton::Right), 10, ROWS - 3);
    let screen = h.screen();
    assert!(screen.contains("Split down"), "{screen}");
    assert!(
        screen.lines().last().is_some_and(|l| l.contains("[test]")),
        "{screen}"
    );
    h.key("Escape");
    // Esc closes a menu without running anything; so does a press elsewhere.
    h.mouse(MouseKind::Down(MouseButton::Right), 10, 5);
    h.key("Escape");
    assert!(!h.screen().contains("Split right"));
    h.mouse(MouseKind::Down(MouseButton::Right), 10, 5);
    h.mouse(MouseKind::Down(MouseButton::Left), 90, 20);
    assert!(!h.screen().contains("Split right"));
    assert_eq!(h.panes().len(), 2);

    // The keyboard: prefix Enter opens the menu; arrows and Enter run.
    h.keys("C-b Enter");
    assert!(h.screen().contains("New floating terminal"));
    h.keys("Down Down Enter");
    assert_eq!(h.panes().len(), 3, "a floating terminal");

    // The `+` tab makes a workspace; a right-click on a tab offers Rename.
    let screen = h.screen();
    let plus = screen
        .lines()
        .last()
        .and_then(|l| l.find("  +"))
        .expect("+");
    let plus = u16::try_from(unicode_width::UnicodeWidthStr::width(
        &screen.lines().last().unwrap_or("")[..plus],
    ))
    .expect("narrow")
        + 3;
    h.mouse(MouseKind::Down(MouseButton::Left), plus, status);
    assert_eq!(h.desktop.session().workspaces.len(), 2);
    let screen = h.screen();
    let (tx, _) = find(&screen, " 0:").expect("tab 0");
    h.mouse(MouseKind::Down(MouseButton::Right), tx + 1, status);
    let screen = h.screen();
    assert!(screen.contains("Rename"), "{screen}");
    assert_eq!(
        h.desktop.session().active_index(),
        0,
        "the tab was selected"
    );
}

#[tokio::test]
async fn hover_redraws_only_when_the_target_changes_and_double_clicks() {
    let mut h = Harness::new("");
    h.type_text(&format!("{}\n", common::echo_split("rea", "dy")));
    h.wait_text("ready\n").await;
    h.screen();
    let status = ROWS - 1;
    // Over the shell, the pointer costs nothing.
    h.desktop.take_dirty();
    h.mouse(MouseKind::Moved, 10, 5);
    h.mouse(MouseKind::Moved, 11, 5);
    assert!(
        !h.desktop.take_dirty(),
        "moving over a shell does not redraw"
    );
    // Onto the start button: one redraw; along it: none.
    h.mouse(MouseKind::Moved, 1, status);
    assert!(h.desktop.take_dirty());
    h.mouse(MouseKind::Moved, 3, status);
    assert!(!h.desktop.take_dirty());

    // A double-click on a floating window's title maximizes it, and
    // another gives it its size back.
    h.keys("C-b F");
    let area_w = COLS;
    let before = h
        .desktop
        .session()
        .active()
        .float_rect(h.desktop.focused().expect("the float"))
        .expect("a float");
    let tx = before.x + 2;
    let ty = before.y;
    h.mouse(MouseKind::Down(MouseButton::Left), tx, ty);
    h.mouse(MouseKind::Up(MouseButton::Left), tx, ty);
    h.mouse(MouseKind::Down(MouseButton::Left), tx, ty);
    h.mouse(MouseKind::Up(MouseButton::Left), tx, ty);
    let pane = h.desktop.focused().expect("still focused");
    let max = h
        .desktop
        .session()
        .active()
        .float_rect(pane)
        .expect("float");
    assert_eq!(max.w, area_w, "maximized: {max:?}");
    h.mouse(MouseKind::Down(MouseButton::Left), max.x + 2, max.y);
    h.mouse(MouseKind::Up(MouseButton::Left), max.x + 2, max.y);
    h.mouse(MouseKind::Down(MouseButton::Left), max.x + 2, max.y);
    let back = h
        .desktop
        .session()
        .active()
        .float_rect(pane)
        .expect("float");
    assert_eq!(back, before, "restored");

    // The palette: a press on a command runs it.
    h.key("C-Space");
    h.type_text("workspace");
    let screen = h.screen();
    let (x, y) = find(&screen, "New workspace").expect("listed");
    h.mouse(MouseKind::Down(MouseButton::Left), x, y);
    assert_eq!(h.desktop.session().workspaces.len(), 2);
    assert!(!h.screen().contains("Command palette"));

    // The key list (prefix ?): a click on a command runs it.
    let before = h.panes().len();
    h.keys("C-b ?");
    let screen = h.screen();
    let (x, y) = find(&screen, "Split: new pane below").expect("listed");
    h.mouse(MouseKind::Down(MouseButton::Left), x, y);
    assert_eq!(h.panes().len(), before + 1);
    assert!(!h.screen().contains("Key bindings"));
}

#[tokio::test]
async fn mouse_focus_resize_and_float_drag() {
    let mut h = Harness::new("");
    h.keys("C-b %");
    let left = h.panes()[0];
    // Click in the left pane focuses it.
    h.mouse(MouseKind::Down(MouseButton::Left), 2, 2);
    h.mouse(MouseKind::Up(MouseButton::Left), 2, 2);
    assert_eq!(h.desktop.focused(), Some(left));
    // Drag the separator from column 50 to column 30.
    h.screen();
    let area = h.desktop.layout_area();
    let before = h.desktop.session().active().placement(area).tiled[0].1.w;
    h.mouse(MouseKind::Down(MouseButton::Left), before, 5);
    h.mouse(MouseKind::Drag(MouseButton::Left), 30, 5);
    h.mouse(MouseKind::Up(MouseButton::Left), 30, 5);
    let after = h.desktop.session().active().placement(area).tiled[0].1.w;
    assert_eq!(after, 30, "separator moved");
    // Float the focused pane and drag its title bar.
    h.keys("C-b f");
    let rect = h
        .desktop
        .session()
        .active()
        .float_rect(left)
        .expect("floating");
    h.mouse(MouseKind::Down(MouseButton::Left), rect.x + 3, rect.y);
    h.mouse(MouseKind::Drag(MouseButton::Left), rect.x + 8, rect.y + 2);
    h.mouse(MouseKind::Up(MouseButton::Left), rect.x + 8, rect.y + 2);
    let moved = h
        .desktop
        .session()
        .active()
        .float_rect(left)
        .expect("floating");
    assert_eq!((moved.x, moved.y), (rect.x + 5, rect.y + 2));
    // Drop on the left edge: snaps to the left half.
    h.mouse(MouseKind::Down(MouseButton::Left), moved.x + 3, moved.y);
    h.mouse(MouseKind::Drag(MouseButton::Left), 0, 10);
    h.mouse(MouseKind::Up(MouseButton::Left), 0, 10);
    let snapped = h
        .desktop
        .session()
        .active()
        .float_rect(left)
        .expect("floating");
    assert_eq!((snapped.x, snapped.w), (0, COLS / 2));
}

#[tokio::test]
async fn mouse_wheel_scrolls_into_copy_mode_and_drag_copies() {
    let mut h = Harness::new("");
    h.type_text(&format!("{}\n", common::print_lines(35)));
    h.wait_text("line35").await;
    h.mouse(MouseKind::ScrollUp, 5, 5);
    assert!(h.desktop.in_copy_mode());
    h.mouse(MouseKind::ScrollDown, 5, 5);
    assert!(
        !h.desktop.in_copy_mode(),
        "wheel back to the bottom leaves copy mode"
    );
    // Drag-select "line35" on its row.
    let screen = h.screen();
    let (row, col) = screen
        .lines()
        .enumerate()
        .find_map(|(i, l)| (l.trim() == "line35").then(|| (i, l.find("line35").unwrap_or(0))))
        .expect("line35 row");
    let (row, col) = (
        u16::try_from(row).expect("row"),
        u16::try_from(col).expect("col"),
    );
    h.mouse(MouseKind::Down(MouseButton::Left), col, row);
    h.mouse(MouseKind::Drag(MouseButton::Left), col + 3, row);
    h.mouse(MouseKind::Drag(MouseButton::Left), col + 5, row);
    h.mouse(MouseKind::Up(MouseButton::Left), col + 5, row);
    assert_eq!(h.desktop.paste_buffer(), Some("line35"));
}

#[tokio::test]
async fn tutorial_on_first_run_and_notification_center() {
    let mut h = Harness::with_tutorial("", true);
    let focused = h.desktop.focused().expect("tutorial focused");
    assert!(h.desktop.session().active().is_floating(focused));
    let screen = h.screen();
    assert!(screen.contains("Welcome to TermOS"), "{screen}");
    assert!(h.dir.path().join("state").join("tutorial-shown").exists());
    h.key("q");
    assert_eq!(h.panes().len(), 1);
    h.keys("C-b N");
    let screen = h.screen();
    assert!(screen.contains("Notifications"), "{screen}");
    h.key("Escape");
    assert_eq!(h.panes().len(), 1);
}

fn hello(cols: u16, rows: u16) -> ClientMsg {
    ClientMsg::Hello {
        version: termos_core::proto::PROTOCOL_VERSION,
        caps: Caps::default(),
        cols,
        rows,
    }
}

#[tokio::test]
async fn two_clients_share_the_session_but_not_overlays() {
    let mut h = Harness::new("");
    let d2 = ClientId(2);
    h.desktop.handle_client(d2, hello(COLS, ROWS));
    assert_eq!(h.desktop.client_ids(), vec![C, d2]);
    // Client 2 opens the palette; client 1 does not see it.
    h.desktop
        .handle_client(d2, ClientMsg::Key(KeyEvent::ctrl(' ')));
    assert!(h.desktop.palette_open(d2));
    assert!(!h.desktop.palette_open(C));
    // Client 1 splits; both see the new layout.
    h.keys("C-b %");
    assert_eq!(h.panes().len(), 2);
    let mut g2 = Grid::new(COLS, ROWS);
    h.desktop.render(d2, &mut g2);
    assert!(g2.to_text().contains('│'));
    assert!(g2.to_text().contains("Command palette"));
    assert!(!h.screen().contains("Command palette"));
    // A prefix pressed on client 2 does not arm client 1.
    h.desktop.handle_client(
        d2,
        ClientMsg::Key(KeyEvent::plain(termos_core::input::KeyCode::Esc)),
    );
    h.desktop
        .handle_client(d2, ClientMsg::Key(KeyEvent::ctrl('b')));
    h.key("z");
    assert!(
        h.desktop.session().active().zoomed().is_none(),
        "z went to the shell, not the WM"
    );
    // Detach is addressed to the client that asked.
    h.desktop
        .handle_client(d2, ClientMsg::Key(KeyEvent::ch('d')));
    let out = h.desktop.take_outbox();
    assert!(out.iter().any(|(to, m)| *to == Some(d2)
        && *m
            == ServerMsg::Detached {
                session: "test".into()
            }));
    h.desktop.detach_client(d2);
    assert_eq!(h.desktop.client_ids(), vec![C]);
}

#[tokio::test]
async fn window_size_policies() {
    for (policy, expect) in [
        ("latest", (90, 20)),
        ("smallest", (90, 20)),
        ("largest", (COLS, ROWS)),
    ] {
        let mut h = Harness::new(&format!("[session]\nwindow-size = \"{policy}\""));
        h.desktop.handle_client(ClientId(2), hello(90, 20));
        assert_eq!((h.desktop.cols(), h.desktop.rows()), expect, "{policy}");
        // Under `latest`, typing on client 1 makes it the size again.
        if policy == "latest" {
            h.key("a");
            assert_eq!((h.desktop.cols(), h.desktop.rows()), (COLS, ROWS));
        }
        h.desktop.detach_client(ClientId(2));
        assert_eq!(
            (h.desktop.cols(), h.desktop.rows()),
            (COLS, ROWS),
            "{policy}"
        );
    }
}

#[tokio::test]
async fn snapshots_keep_layout_and_scrollback() {
    let mut h = Harness::new("[session]\nsave-scrollback = true\n");
    h.type_text(&format!("{}\r", common::echo_split("hist", "ory-line")));
    h.wait_for("the output", |s, _| {
        s.lines()
            .any(|l| l.trim_end().ends_with("history-line") && !l.contains("echo"))
    })
    .await;
    h.desktop.run(termos_shell::commands::Command::Split(
        termos_wm::workspace::SplitHow::Right,
    ));
    let snap = h.desktop.snapshot();
    assert_eq!(snap.name, "test");
    assert_eq!(snap.panes.len(), 2);
    let first = snap
        .panes
        .iter()
        .find(|p| {
            p.scrollback.as_ref().is_some_and(|s| {
                s.iter()
                    .any(|l| l.ends_with("history-line") && !l.contains("echo"))
            })
        })
        .unwrap_or_else(|| panic!("the first pane's text is saved: {:?}", snap.panes));
    assert!(first.argv.is_none(), "the shell itself is not a program");

    // Rebuilt from the snapshot: same layout, the old text above a new
    // shell, new pane ids.
    let (tx, _rx) = mpsc::channel(1024);
    let opts = DesktopOptions {
        clipboard: termos_core::clipboard::ClipboardHistory::shared(50),
        termos_exe: None,
        launcher: None,
        server_socket: None,
        session_name: "again".into(),
        config_path: h.config_path(),
        paths: Paths {
            config_dir: h.dir.path().join("config"),
            state_dir: h.dir.path().join("state"),
            runtime_dir: h.dir.path().join("run"),
            data_dir: h.dir.path().join("data"),
        },
        env_shell: None,
        pane_ids: termos_core::ids::PaneIds::new(),
        pane_env: common::pane_env(),
        cwd: None,
    };
    let config = Config::load(&h.config_path()).expect("config");
    let (mut restored, skipped) =
        Desktop::from_snapshot(opts, config, tx, &snap).expect("restores");
    assert!(skipped.is_empty());
    assert_eq!(restored.name(), "again");
    assert_eq!(restored.session().all_panes().len(), 2);
    restored.handle_client(
        C,
        ClientMsg::Hello {
            version: termos_core::proto::PROTOCOL_VERSION,
            caps: Caps::default(),
            cols: COLS,
            rows: ROWS,
        },
    );
    let mut grid = Grid::new(COLS, ROWS);
    restored.render(C, &mut grid);
    assert!(
        grid.to_text().contains("history-line"),
        "{}",
        grid.to_text()
    );
    assert!(grid.to_text().contains('│'), "the split is back");
}

/// Where `needle` is on `screen`: (column, row).
fn locate(screen: &str, needle: &str) -> (u16, u16) {
    for (y, line) in screen.lines().enumerate() {
        if let Some(i) = line.find(needle) {
            let x = unicode_width::UnicodeWidthStr::width(&line[..i]);
            return (
                u16::try_from(x).expect("narrow"),
                u16::try_from(y).expect("short"),
            );
        }
    }
    panic!("{needle:?} is not on the screen:\n{screen}");
}

/// Where a workspace has no tiled panes the wallpaper shows (D-089): its
/// shortcuts light up under the pointer and open things, and a right-click
/// on it opens the TermOS menu.
#[tokio::test]
async fn the_wallpaper_shows_where_nothing_is_tiled() {
    use termos_shell::commands::Command;

    let mut h = Harness::new("");
    let hint = "right-click anywhere for the menu";
    assert!(!h.screen().contains(hint));
    // The only pane floats: nothing is tiled.
    h.desktop.run(Command::ToggleFloat);
    let screen = h.screen();
    assert!(screen.contains(hint), "{screen}");
    assert!(screen.contains('█'), "the logo:\n{screen}");

    let (x, y) = locate(&screen, "Files");
    let plain = h.grid.get(x, y).map(|c| c.style);
    h.mouse(MouseKind::Moved, x, y);
    h.screen();
    assert_ne!(
        h.grid.get(x, y).map(|c| c.style),
        plain,
        "lit under the pointer"
    );
    h.mouse(MouseKind::Down(MouseButton::Left), x, y);
    h.mouse(MouseKind::Up(MouseButton::Left), x, y);
    assert_eq!(h.panes().len(), 2, "Files opened");

    let screen = h.screen();
    let (x, y) = locate(&screen, "right-click");
    h.mouse(MouseKind::Down(MouseButton::Right), x, y);
    let screen = h.screen();
    assert!(screen.contains("New floating terminal"), "{screen}");

    // Tiled again: no wallpaper.
    h.key("Escape");
    h.desktop
        .run(Command::Split(termos_wm::workspace::SplitHow::Auto));
    assert!(!h.screen().contains(hint));
}

/// `[ui] wallpaper` naming a picture (relative to the config file): it is
/// decoded on its own thread and drawn behind the floats; `none` draws
/// nothing at all.
#[tokio::test]
async fn a_picture_wallpaper_and_none() {
    use termos_render::color::{Color, Rgb};
    use termos_shell::commands::Command;

    let mut h = Harness::with_ui("wallpaper = \"wall.png\"\nsplash = false", false, true);
    h.desktop.run(Command::ToggleFloat);
    let green = |g: &Grid| {
        let mut n = 0;
        for y in 0..g.rows() {
            for x in 0..g.cols() {
                if g.get(x, y).map(|c| c.style.bg) == Some(Color::Rgb(Rgb::new(0, 200, 0))) {
                    n += 1;
                }
            }
        }
        n
    };
    h.wait_grid("the wallpaper in blocks", |g| green(g) > 100)
        .await;
    let screen = h.screen();
    assert!(
        screen.contains("Files"),
        "the shortcuts beside it:\n{screen}"
    );

    let mut h = Harness::with_ui("wallpaper = \"none\"\nsplash = false", false, false);
    h.desktop.run(Command::ToggleFloat);
    let screen = h.screen();
    assert!(
        !screen.contains("Files") && !screen.contains('·'),
        "{screen}"
    );
}

/// A client that animates sees the logo for a moment when it attaches; any
/// key ends it at once, and the key still reaches the pane.
#[tokio::test]
async fn the_splash_shows_on_attach_until_a_key() {
    use termos_shell::anim::{Kind, SPLASH};

    let mut h = Harness::with_ui("splash = true", true, false);
    let start = h
        .desktop
        .animations()
        .started(Kind::Splash(C))
        .expect("the splash runs");
    let mut g = Grid::new(COLS, ROWS);
    h.desktop.render_at(C, &mut g, start + SPLASH / 2);
    let text = g.to_text();
    assert!(text.contains("a terminal-first desktop"), "{text}");
    // It ends by itself, and the timer with it.
    let mut frames = 0;
    while let Some(d) = h.desktop.next_deadline() {
        frames += 1;
        assert!(frames < 80, "the splash never ends");
        h.desktop.on_timer(d);
    }
    assert!(h.desktop.animations().started(Kind::Splash(C)).is_none());

    // Another client: a key ends its splash at once.
    const D: ClientId = ClientId(2);
    h.desktop.handle_client(
        D,
        ClientMsg::Hello {
            version: termos_core::proto::PROTOCOL_VERSION,
            caps: Caps::default(),
            cols: COLS,
            rows: ROWS,
        },
    );
    assert!(h.desktop.animations().started(Kind::Splash(D)).is_some());
    h.desktop
        .handle_client(D, ClientMsg::Key(KeyEvent::ch('x')));
    assert!(h.desktop.animations().started(Kind::Splash(D)).is_none());
    h.wait_text("x").await;
}

/// About TermOS opens from the Help menu's command and shows the version.
#[tokio::test]
async fn about_opens_with_the_version() {
    use termos_shell::commands::Command;

    let mut h = Harness::new("");
    h.desktop.run(Command::AboutOpen);
    let screen = h.screen();
    assert!(screen.contains("About TermOS"), "{screen}");
    assert!(screen.contains(env!("CARGO_PKG_VERSION")), "{screen}");
    assert_eq!(
        "about.open".parse::<Command>().map(|c| c.to_string()),
        Ok("about.open".to_owned())
    );
}

/// The monitor's CPU chart (D-090): in pixels for a terminal that shows
/// them, with the braille beneath cleared; in braille for one that does
/// not. Its title spins until the first reading.
#[tokio::test]
async fn the_cpu_chart_in_pixels_or_braille() {
    use termos_render::caps::{ImageProtocol, PxSize};
    use termos_shell::commands::Command;

    let mut h = Harness::new("");
    const K: ClientId = ClientId(2);
    h.desktop.handle_client(
        K,
        ClientMsg::Hello {
            version: termos_core::proto::PROTOCOL_VERSION,
            caps: Caps {
                graphics_override: Some(ImageProtocol::Kitty),
                cell_px: Some(PxSize {
                    width: 10,
                    height: 20,
                }),
                ..Caps::default()
            },
            cols: COLS,
            rows: ROWS,
        },
    );
    h.desktop.run(Command::MonitorOpen);
    let screen = h.screen();
    assert!(screen.contains("⠋ System monitor"), "busy:\n{screen}");
    let first = h.desktop.next_deadline().expect("a first reading is due");
    h.desktop.on_timer(first);
    h.wait_text("Memory").await;
    // The reading may land between the title and the drawing; its wakeup
    // brings the next frame, as here.
    let screen = h.screen();
    assert!(
        screen.contains("◔ System monitor"),
        "the icon is back:\n{screen}"
    );

    // No pixels here: the braille chart, and no picture.
    let braille = |g: &Grid, r: termos_render::grid::Rect| {
        (r.y..r.bottom()).any(|y| {
            (r.x..r.right()).any(|x| {
                g.get(x, y)
                    .and_then(|c| c.symbol.chars().next())
                    .is_some_and(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
            })
        })
    };
    assert!(h.desktop.pixel_images(C).is_empty());
    let mut kitty = Grid::new(COLS, ROWS);
    h.desktop.render(K, &mut kitty);
    let shown = h.desktop.pixel_images(K);
    assert_eq!(shown.len(), 1, "the chart, in pixels");
    let piece = shown[0].pieces[0];
    assert!(
        braille(&h.grid, piece),
        "braille where pixels are not shown"
    );
    assert!(!braille(&kitty, piece), "cleared beneath the picture");
}

/// A busy window's spinner keeps a timer only while some client animates
/// (D-090); otherwise only the app's own timers run.
#[tokio::test]
async fn spinners_turn_only_where_something_animates() {
    use termos_shell::commands::Command;

    for animations in [true, false] {
        let mut h = Harness::with_options("", false, animations);
        h.desktop.run(Command::MonitorOpen);
        // Past the opening animation; the first reading is asked for and
        // not yet taken in, so the monitor is still busy.
        let later = Instant::now() + Duration::from_secs(1);
        h.desktop.on_timer(later);
        let next = h
            .desktop
            .next_deadline()
            .expect("the next reading at least");
        let soon = Instant::now() + 2 * termos_render::icons::SPINNER_FRAME;
        if animations {
            assert!(next <= soon, "the spinner turns");
        } else {
            assert!(next > soon, "no spinner timer");
        }
    }
}

/// Each terminal gets the chrome it can show (D-088): a gradient title bar
/// in truecolor, the same bar flat in 256 colours; and the icons of
/// `[ui] icons`.
#[tokio::test]
async fn chrome_follows_each_terminal_and_the_icon_set() {
    use termos_render::color::Color;
    use termos_render::color::ColorDepth;
    use termos_shell::commands::Command;

    // The harness's client has 256 colours; this one has truecolor.
    let mut h = Harness::with_ui("icons = \"ascii\"", false, false);
    const TRUE: ClientId = ClientId(2);
    h.desktop.handle_client(
        TRUE,
        ClientMsg::Hello {
            version: termos_core::proto::PROTOCOL_VERSION,
            caps: Caps {
                color: ColorDepth::Truecolor,
                ..Caps::default()
            },
            cols: COLS,
            rows: ROWS,
        },
    );
    h.desktop.run(Command::Tutorial);
    let screen = h.screen();
    // The ASCII set: `?` for the tutorial and `*` for the menu.
    assert!(screen.contains("? Tutorial 1/6"), "{screen}");
    assert!(
        screen.lines().last().unwrap_or("").starts_with(" * Menu"),
        "{screen}"
    );
    let (_, y) = locate(&screen, "Tutorial 1/6");
    let bar_bgs = |g: &Grid| {
        (0..g.cols())
            .filter_map(|x| g.get(x, y).map(|c| c.style.bg))
            .filter(|bg| *bg != Color::Default)
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    let mut truecolor = Grid::new(COLS, ROWS);
    h.desktop.render(TRUE, &mut truecolor);
    let shades = bar_bgs(&truecolor);
    assert!(shades > 10, "a gradient: {shades} colours");
    assert_eq!(bar_bgs(&h.grid), 1, "flat in 256 colours");
}

/// Keys typed one at a time into PowerShell (PSReadLine reads key events
/// from the pseudo-console) arrive exactly once each, in order: slowly and
/// in a burst. An owner saw letters repeat and go missing on Windows.
#[cfg(windows)]
#[tokio::test]
async fn keys_reach_powershell_exactly_once() {
    let mut h = Harness::new("");
    // cmd's prompt is `$ ` here; screen lines lose trailing spaces.
    h.wait_for("the cmd prompt", |s, _| {
        s.lines().any(|l| l.trim_end() == "$")
    })
    .await;
    h.type_text("powershell -NoLogo -NoProfile\n");
    h.wait_for("the PowerShell prompt", |s, _| {
        s.lines()
            .any(|l| l.starts_with("PS ") && l.trim_end().ends_with('>'))
    })
    .await;
    for (round, gap) in [(1, Duration::from_millis(40)), (2, Duration::ZERO)] {
        let word = format!("kq{round}term");
        let line = format!("echo {word}");
        for c in line.chars() {
            h.desktop.handle_client(C, ClientMsg::Key(KeyEvent::ch(c)));
            // Let output in between keys, like a person typing.
            let until = Instant::now() + gap;
            while let Ok(Some(ev)) = tokio::time::timeout(
                until.saturating_duration_since(Instant::now()),
                h.pty_rx.recv(),
            )
            .await
            {
                h.desktop.on_pty(ev);
            }
        }
        let typed = format!("> {line}");
        h.wait_for(&format!("{typed:?} typed exactly"), |s, _| {
            s.lines().any(|l| l.trim_end().ends_with(&typed))
        })
        .await;
        h.type_text("\n");
        h.wait_for(&format!("{word} printed"), |s, _| {
            s.lines().any(|l| l.trim_end() == word)
        })
        .await;
    }
}

/// With `[ui] start = "desktop"` (the default), a session starts on the
/// desktop: the wallpaper, and the terminal in a floating window that has
/// focus and takes typing. New workspaces start the same way, and the
/// wallpaper's Terminal shortcut opens another window (D-091).
#[tokio::test]
async fn sessions_start_on_the_desktop() {
    use termos_shell::commands::Command;

    assert_eq!(
        Config::default().ui.start,
        termos_core::config::StartLook::Desktop,
        "the default"
    );
    let mut h = Harness::with_ui("start = \"desktop\"", false, false);
    let first = h.desktop.focused().expect("the terminal has focus");
    assert!(h.desktop.session().active().is_floating(first));
    let screen = h.screen();
    assert!(
        screen.contains("right-click anywhere for the menu"),
        "{screen}"
    );
    h.type_text(&format!("{}\n", common::echo_split("on-the", "-desktop")));
    // The output (the typed command has quotes in the word).
    h.wait_text("on-the-desktop").await;

    // The Terminal shortcut: another window, the desktop stays.
    let screen = h.screen();
    let (x, y) = locate(&screen, "Terminal");
    h.mouse(MouseKind::Down(MouseButton::Left), x, y);
    h.mouse(MouseKind::Up(MouseButton::Left), x, y);
    let ws = h.desktop.session().active();
    assert_eq!(ws.panes().len(), 2);
    assert!(ws.panes().iter().all(|p| ws.is_floating(*p)));

    h.desktop.run(Command::WorkspaceNew);
    let ws = h.desktop.session().active();
    assert_eq!(ws.panes().len(), 1);
    assert!(
        ws.is_floating(ws.panes()[0]),
        "a new workspace is a desktop too"
    );
}
