// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end tests of the desktop with real `/bin/sh` panes, driven
//! headlessly (no terminal involved).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use termos_core::config::Config;
use termos_core::ids::PaneId;
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
    format!("shell = \"/bin/sh\"\n{extra}\n")
}

impl Harness {
    fn new(extra: &str) -> Self {
        Self::with_tutorial(extra, false)
    }

    fn with_tutorial(extra: &str, tutorial: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            state_dir: dir.path().join("state"),
        };
        std::fs::create_dir_all(&paths.config_dir).expect("mkdir");
        let tutorial_cfg =
            format!("[tutorial]\nshow-on-first-run = {tutorial}\n[status]\nclock = false\n");
        let text = base_config(&format!("{extra}\n{tutorial_cfg}"));
        std::fs::write(paths.config_file(), &text).expect("write config");
        let config = Config::load(&paths.config_file()).expect("valid test config");
        let (tx, pty_rx) = mpsc::channel(1024);
        let opts = DesktopOptions {
            session_name: "test".into(),
            config_path: paths.config_file(),
            paths,
            env_shell: None,
        };
        let mut desktop = Desktop::new(opts, config, tx, COLS, ROWS).expect("desktop starts");
        desktop.handle_client(ClientMsg::Hello {
            version: termos_core::proto::PROTOCOL_VERSION,
            caps: Caps::default(),
            cols: COLS,
            rows: ROWS,
        });
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
        self.desktop.handle_client(ClientMsg::Key(k));
        self.outbox.extend(self.desktop.take_outbox());
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
            self.desktop.handle_client(ClientMsg::Key(k));
        }
    }

    fn mouse(&mut self, kind: MouseKind, x: u16, y: u16) {
        self.desktop.handle_client(ClientMsg::Mouse(MouseEvent {
            kind,
            x,
            y,
            mods: Mods::empty(),
        }));
    }

    fn screen(&mut self) -> String {
        self.desktop.render(&mut self.grid);
        self.outbox.extend(self.desktop.take_outbox());
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
    h.type_text("echo he''llo\n");
    let screen = h.wait_text("hello\n").await;
    let status = screen.lines().last().unwrap_or("");
    assert!(status.starts_with("[test]  0:"), "{status:?}");
    assert_eq!(h.panes().len(), 1);
}

#[tokio::test]
async fn split_right_type_and_see_separator() {
    let mut h = Harness::new("");
    h.keys("C-b %");
    assert_eq!(h.panes().len(), 2);
    let focused = h.desktop.focused();
    assert_eq!(focused, Some(h.panes()[1]));
    h.type_text("echo ri''ght\n");
    // The output line: "right" alone in the right-hand pane (the status
    // line may also contain the word, e.g. in a workspace name).
    let is_output = |l: &str| l.split('│').nth(1).is_some_and(|r| r.trim() == "right");
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
    assert!(h.desktop.prompt_open());
    h.key("C-u");
    h.type_text("web\n");
    assert!(!h.desktop.prompt_open());
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
    h.type_text("echo cop''ytext\n");
    h.wait_for("output line", |s, _| {
        s.lines().any(|l| l.trim() == "copytext")
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
    assert!(h.desktop.palette_open());
    let screen = h.screen();
    assert!(screen.contains("Command palette"), "{screen}");
    h.type_text("zoom");
    h.key("Enter");
    assert!(!h.desktop.palette_open());
    assert!(h.desktop.session().active().zoomed().is_some());
    h.keys("C-b :");
    assert!(h.desktop.palette_open());
    h.key("Escape");
    assert!(!h.desktop.palette_open());
}

#[tokio::test]
async fn which_key_appears_after_a_pause() {
    let mut h = Harness::new("");
    h.key("C-b");
    assert!(!h.desktop.whichkey_visible());
    let deadline = h.desktop.next_deadline().expect("which-key timer armed");
    h.desktop.on_timer(deadline);
    assert!(h.desktop.whichkey_visible());
    let screen = h.screen();
    assert!(screen.contains("C-b …"), "{screen}");
    assert!(screen.contains("PREFIX"), "{screen}");
    h.key("z");
    assert!(!h.desktop.whichkey_visible());
    // prefix ? shows everything until a key is pressed.
    h.keys("C-b ?");
    assert!(h.screen().contains("Key bindings"));
    h.key("q");
    assert!(!h.desktop.whichkey_visible());
}

#[tokio::test]
async fn kill_pane_needs_confirmation_and_exit_closes() {
    let mut h = Harness::new("");
    h.keys("C-b %");
    h.keys("C-b x");
    assert!(h.desktop.prompt_open());
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
    assert!(!h.desktop.prompt_open());
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
    h.type_text("for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35; do echo line$i; done\n");
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
