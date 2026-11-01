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

    fn with_options(extra: &str, tutorial: bool, animations: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            state_dir: dir.path().join("state"),
            runtime_dir: dir.path().join("run"),
        };
        std::fs::create_dir_all(&paths.config_dir).expect("mkdir");
        // Animations off: screens are read at arbitrary moments.
        let tutorial_cfg = format!(
            "[tutorial]\nshow-on-first-run = {tutorial}\n[status]\nclock = false\n[ui]\nanimations = {animations}\n"
        );
        let text = base_config(&format!("{extra}\n{tutorial_cfg}"));
        std::fs::write(paths.config_file(), &text).expect("write config");
        let config = Config::load(&paths.config_file()).expect("valid test config");
        let (tx, pty_rx) = mpsc::channel(1024);
        let opts = DesktopOptions {
            session_name: "test".into(),
            config_path: paths.config_file(),
            paths,
            env_shell: None,
            pane_ids: termos_core::ids::PaneIds::new(),
            pane_env: common::pane_env(),
            cwd: None,
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
            pty_rx,
            grid: Grid::new(COLS, ROWS),
            dir,
            outbox: Vec::new(),
        }
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
    assert!(status.starts_with("[test]  0:"), "{status:?}");
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
        base_config("[theme]\nbase = \"dark\"\n[status]\nclock = false\n[tutorial]\nshow-on-first-run = false\n[ui]\nanimations = false\n"),
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
    // Paste it back.
    h.keys("C-b ]");
    h.type_text("\n");
    h.wait_for("pasted command output", |s, _| {
        s.matches("copytext").count() >= 3
    })
    .await;
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
        session_name: "again".into(),
        config_path: h.config_path(),
        paths: Paths {
            config_dir: h.dir.path().join("config"),
            state_dir: h.dir.path().join("state"),
            runtime_dir: h.dir.path().join("run"),
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
