// SPDX-License-Identifier: GPL-3.0-or-later
//! Links, hints, the search across panes and finished commands (M13a,
//! D-118) on a real desktop with real shells.

mod common;

use std::time::{Duration, Instant};

use termos_core::config::Config;
use termos_core::ids::ClientId;
use termos_core::input::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use termos_core::paths::Paths;
use termos_core::proto::{ClientMsg, ServerMsg};
use termos_render::caps::Caps;
use termos_render::grid::Grid;
use termos_shell::{Desktop, DesktopOptions};
use termos_term::pty::PtyEvent;
use tokio::sync::mpsc;

const C: ClientId = ClientId(1);
const COLS: u16 = 100;
const ROWS: u16 = 30;

struct Harness {
    desktop: Desktop,
    pty_rx: mpsc::Receiver<PtyEvent>,
    grid: Grid,
    _dir: tempfile::TempDir,
    outbox: Vec<ServerMsg>,
}

impl Harness {
    fn new(shell: Option<&str>, extra: &str, caps: Caps) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            state_dir: dir.path().join("state"),
            runtime_dir: dir.path().join("run"),
            data_dir: dir.path().join("data"),
        };
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("mkdir");
        std::fs::create_dir_all(&paths.config_dir).expect("mkdir");
        let shell_line = match shell {
            Some(s) => format!("shell = {s:?}\n"),
            None => common::shell_config(),
        };
        let text = format!(
            "{shell_line}{extra}\n[tutorial]\nshow-on-first-run = false\n[status]\nclock = false\n[ui]\nanimations = false\nstart = \"terminal\"\nsplash = false\n"
        );
        std::fs::write(paths.config_file(), &text).expect("write config");
        let config = Config::load(&paths.config_file()).expect("valid test config");
        let (tx, pty_rx) = mpsc::channel(1024);
        let mut pane_env = common::pane_env();
        pane_env.push(("HOME".into(), home.display().to_string()));
        pane_env.push((
            "XDG_CONFIG_HOME".into(),
            home.join(".config").display().to_string(),
        ));
        let opts = DesktopOptions {
            clipboard: termos_core::clipboard::ClipboardHistory::shared(50),
            termos_exe: None,
            session_name: "test".into(),
            config_path: paths.config_file(),
            paths,
            env_shell: None,
            pane_ids: termos_core::ids::PaneIds::new(),
            pane_env,
            cwd: None,
            launcher: None,
            server_socket: None,
            power: false,
        };
        let mut desktop = Desktop::new(opts, config, tx, COLS, ROWS).expect("desktop starts");
        desktop.handle_client(
            C,
            ClientMsg::Hello {
                version: termos_core::proto::PROTOCOL_VERSION,
                caps,
                cols: COLS,
                rows: ROWS,
            },
        );
        Self {
            desktop,
            pty_rx,
            grid: Grid::new(COLS, ROWS),
            _dir: dir,
            outbox: Vec::new(),
        }
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let k = match c {
                '\n' => KeyEvent::plain(KeyCode::Enter),
                c => KeyEvent::ch(c),
            };
            self.key(k);
        }
    }

    fn key(&mut self, k: KeyEvent) {
        self.desktop.handle_client(C, ClientMsg::Key(k));
    }

    /// Runs a command as the test's client (prompts and searches are a
    /// client's).
    fn run(&mut self, command: &str) {
        self.desktop
            .run_as(Some(C), command.parse().expect("a command"));
    }

    fn screen(&mut self) -> String {
        self.desktop.render(C, &mut self.grid);
        self.outbox
            .extend(self.desktop.take_outbox().into_iter().map(|(_, m)| m));
        self.grid.to_text()
    }

    async fn wait_for(&mut self, what: &str, pred: impl Fn(&str, &Desktop) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
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
            self.desktop.on_timer(Instant::now());
        }
    }

    fn click(&mut self, x: u16, y: u16, mods: Mods) {
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            self.desktop
                .handle_client(C, ClientMsg::Mouse(MouseEvent { kind, x, y, mods }));
        }
    }

    fn opened(&self) -> Vec<String> {
        self.outbox
            .iter()
            .filter_map(|m| match m {
                ServerMsg::Open { url } => Some(url.clone()),
                _ => None,
            })
            .collect()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ctrl_click_and_hints_open_and_copy_links() {
    let mut h = Harness::new(None, "", Caps::default());
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    h.type_text(&format!(
        "{}\n",
        common::echo_split("https://example.com/a", "bc")
    ));
    h.wait_for("the address", |s, _| {
        s.lines()
            .any(|l| l.trim_end().ends_with("https://example.com/abc"))
    })
    .await;
    // The output's address, not the command line's.
    let screen = h.screen();
    let y = screen
        .lines()
        .position(|l| l.trim_end() == "https://example.com/abc")
        .expect("the output line");
    let y = u16::try_from(y).expect("on screen");
    // A plain click opens nothing.
    h.click(3, y, Mods::empty());
    h.screen();
    assert!(h.opened().is_empty());
    h.click(3, y, Mods::CTRL);
    h.screen();
    assert_eq!(h.opened(), vec!["https://example.com/abc"]);

    // Hints: the bottom match gets `a`; Shift copies.
    h.run("pane.hints url");
    let screen = h.screen();
    assert!(screen.contains("hints"), "{screen}");
    h.key(KeyEvent::ch('A'));
    h.screen();
    assert!(
        h.outbox.iter().any(|m| matches!(
            m,
            ServerMsg::Clipboard(t) if t == "https://example.com/abc"
        )),
        "{:?}",
        h.outbox
    );
    // And a plain label opens.
    h.run("pane.hints");
    h.key(KeyEvent::ch('a'));
    h.screen();
    assert_eq!(h.opened().len(), 2, "{:?}", h.outbox);
}

#[tokio::test(flavor = "current_thread")]
async fn on_the_console_and_over_ssh_addresses_are_copied() {
    for caps in [
        Caps {
            term: "termos-kms".into(),
            ..Caps::default()
        },
        Caps {
            remote: true,
            ..Caps::default()
        },
    ] {
        let mut h = Harness::new(None, "", caps);
        h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
        h.type_text(&format!("{}\n", common::echo_split("https://x.org/", "y")));
        h.wait_for("the address", |s, _| {
            s.lines().any(|l| l.trim_end() == "https://x.org/y")
        })
        .await;
        // The output is a line of its own.
        let screen = h.screen();
        let y = screen
            .lines()
            .position(|l| l.trim_end() == "https://x.org/y")
            .expect("the output line");
        h.click(2, u16::try_from(y).expect("on screen"), Mods::CTRL);
        let screen = h.screen();
        assert!(h.opened().is_empty());
        assert!(
            h.outbox
                .iter()
                .any(|m| matches!(m, ServerMsg::Clipboard(t) if t == "https://x.org/y")),
            "{:?}",
            h.outbox
        );
        assert!(screen.contains("copied"), "{screen}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_search_lands_on_the_match_in_copy_mode() {
    let mut h = Harness::new(None, "", Caps::default());
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    h.run("pane.split-right");
    h.run("pane.split-down");
    let panes = h.desktop.session().active().panes();
    assert_eq!(panes.len(), 3);
    for (i, id) in panes.iter().enumerate() {
        h.desktop
            .send_text(
                Some(*id),
                &format!("{}\n", common::echo_split("word", &format!("-{i}x"))),
            )
            .expect("a terminal");
    }
    h.wait_for("every pane's output", |s, _| {
        (0..3).all(|i| s.contains(&format!("word-{i}x")))
    })
    .await;
    h.run("search.all");
    h.type_text("word-1x");
    let screen = h
        .wait_for("the results", |s, _| s.contains("1 match"))
        .await;
    assert!(screen.contains("Search panes"), "{screen}");
    h.key(KeyEvent::plain(KeyCode::Enter));
    h.screen();
    assert!(h.desktop.in_copy_mode());
    assert_eq!(h.desktop.focused(), Some(panes[1]));
    // `y` copies the selected match.
    h.type_text("y");
    h.screen();
    assert!(
        h.outbox
            .iter()
            .any(|m| matches!(m, ServerMsg::Clipboard(t) if t == "word-1x")),
        "{:?}",
        h.outbox
    );
}

/// `name`'s path when installed, as the attention tests find it.
#[cfg(unix)]
fn installed(name: &str) -> Option<std::path::PathBuf> {
    let found = std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file());
    let required = std::env::var("TERMOS_REQUIRE_SHELLS")
        .is_ok_and(|v| v.split(',').any(|s| s.trim() == name));
    assert!(
        found.is_some() || !required,
        "{name} must be installed here"
    );
    found
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn long_commands_say_so_where_no_one_looks() {
    let Some(bash) = installed("bash") else {
        return;
    };
    let shell = bash.display().to_string();
    let mut h = Harness::new(
        Some(&shell),
        "[notifications]\nlong-command = 1\n",
        Caps::default(),
    );
    h.wait_for("a prompt", |s, _| {
        s.lines().any(|l| l.trim_end().ends_with(['$', '#']))
    })
    .await;
    let ends = |text: &str, d: &Desktop| d.toast().is_some_and(|t| t.contains(text));
    // A long command, then another workspace: a tick and a toast.
    h.type_text("sleep 2\n");
    h.run("workspace.new");
    let screen = h
        .wait_for("the finished command", |_, d| {
            ends("`sleep 2` in 0:", d) && ends("finished after", d)
        })
        .await;
    let status = screen.lines().last().unwrap_or_default().to_owned();
    assert!(status.contains('\u{2713}'), "{status}");
    assert!(
        !h.outbox
            .iter()
            .any(|m| matches!(m, ServerMsg::Notify { .. })),
        "a success is quiet"
    );
    // Seen: the tick goes.
    h.run("workspace.select 0");
    let screen = h.screen();
    assert!(!screen.contains('\u{2713}'), "{screen}");
    // A failure: a cross and a system notification.
    h.type_text("sleep 2; false\n");
    h.run("workspace.select 1");
    let screen = h
        .wait_for("the failed command", |_, d| ends("failed (1) after", d))
        .await;
    let status = screen.lines().last().unwrap_or_default().to_owned();
    assert!(status.contains('\u{2717}'), "{status}");
    assert!(
        h.outbox.iter().any(|m| matches!(
            m,
            ServerMsg::Notify { title, .. } if title.contains("failed")
        )),
        "{:?}",
        h.outbox
    );
    // A short command while looking is not announced; asked for, it is.
    h.run("workspace.select 0");
    h.screen();
    h.run("pane.notify-when-done");
    h.type_text("true\n");
    h.wait_for("the command asked about", |_, d| {
        ends("`true` in 0:", d) && ends("finished after 0s", d)
    })
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn without_integration_quiet_means_done() {
    // `sh` has no shell integration.
    let mut h = Harness::new(Some("/bin/sh"), "", Caps::default());
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    h.run("pane.notify-when-done");
    h.type_text("echo started\n");
    let started = Instant::now();
    h.wait_for("the quiet pane", |_, d| {
        d.toast()
            .is_some_and(|t| t.contains("has been quiet for 5s"))
    })
    .await;
    assert!(started.elapsed() >= Duration::from_secs(4));
}

#[test]
fn hint_and_search_keys_are_bound() {
    let keys = termos_shell::keys::default_prefix_bindings();
    let find = |k: &str| {
        keys.iter()
            .find(|b| b.key == k.parse::<KeyEvent>().expect("a key"))
            .map(|b| b.command.to_string())
    };
    assert_eq!(find("u").as_deref(), Some("pane.hints"));
    assert_eq!(find("/").as_deref(), Some("search.all"));
}
