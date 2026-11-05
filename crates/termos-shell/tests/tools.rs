// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace tools (M13b, D-120) on a real desktop with real shells:
//! layout files, the drop-down terminal, typing into several panes, saved
//! commands and recordings.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use termos_core::config::Config;
use termos_core::ids::{ClientId, PaneId};
use termos_core::input::{KeyCode, KeyEvent};
use termos_core::paths::Paths;
use termos_core::proto::ClientMsg;
use termos_render::caps::Caps;
use termos_render::grid::{Grid, Rect};
use termos_shell::{Desktop, DesktopOptions};
use termos_term::pty::PtyEvent;
use tokio::sync::mpsc;

const C: ClientId = ClientId(1);
const COLS: u16 = 120;
const ROWS: u16 = 40;

struct Harness {
    desktop: Desktop,
    pty_rx: mpsc::Receiver<PtyEvent>,
    grid: Grid,
    paths: Paths,
    dir: tempfile::TempDir,
}

impl Harness {
    fn new(extra: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            state_dir: dir.path().join("state"),
            runtime_dir: dir.path().join("run"),
            data_dir: dir.path().join("data"),
        };
        std::fs::create_dir_all(&paths.config_dir).expect("mkdir");
        let text = format!(
            "{}{extra}\n[tutorial]\nshow-on-first-run = false\n[status]\nclock = false\n[ui]\nanimations = false\nstart = \"terminal\"\nsplash = false\n",
            common::shell_config()
        );
        std::fs::write(paths.config_file(), &text).expect("write config");
        let config = Config::load(&paths.config_file()).expect("valid test config");
        let (tx, pty_rx) = mpsc::channel(1024);
        let opts = DesktopOptions {
            clipboard: termos_core::clipboard::ClipboardHistory::shared(50),
            termos_exe: None,
            session_name: "test".into(),
            config_path: paths.config_file(),
            paths: paths.clone(),
            env_shell: None,
            pane_ids: termos_core::ids::PaneIds::new(),
            pane_env: common::pane_env(),
            cwd: Some(dir.path().to_owned()),
            launcher: None,
            server_socket: None,
            power: false,
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
            paths,
            dir,
        }
    }

    /// Runs a command as the test's client (prompts and searches are a
    /// client's).
    fn run(&mut self, command: &str) {
        self.desktop
            .run_as(Some(C), command.parse().expect("a command"));
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let k = match c {
                '\n' => KeyEvent::plain(KeyCode::Enter),
                c => KeyEvent::ch(c),
            };
            self.desktop.handle_client(C, ClientMsg::Key(k));
        }
    }

    fn screen(&mut self) -> String {
        self.desktop.render(C, &mut self.grid);
        let _ = self.desktop.take_outbox();
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
        }
    }

    fn area(&self) -> Rect {
        self.desktop.layout_area()
    }

    fn tiled(&self) -> Vec<(PaneId, Rect)> {
        self.desktop.session().active().placement(self.area()).tiled
    }
}

/// Lines of `screen` that are exactly `text`, in any pane (panes are
/// separated by `│`).
fn count_output(screen: &str, text: &str) -> usize {
    screen
        .lines()
        .flat_map(|l| l.split('│'))
        .filter(|cell| {
            cell.trim().strip_suffix(text).is_some_and(|before| {
                let before = before.trim_end();
                before.is_empty() || before.ends_with(['$', '#', '>'])
            })
        })
        .count()
}

fn toml_str(s: &str) -> String {
    format!("{s:?}")
}

#[tokio::test(flavor = "current_thread")]
async fn a_layout_opens_saves_and_opens_again_the_same() {
    let mut h = Harness::new("");
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    let project = h.dir.path().join("proj");
    std::fs::create_dir_all(project.join("sub")).expect("mkdir");
    let layouts = h.paths.layouts_dir();
    std::fs::create_dir_all(&layouts).expect("mkdir");
    std::fs::write(
        layouts.join("dev.toml"),
        format!(
            "workspace = \"dev\"\n[[pane]]\ncommand = {}\n[[pane]]\nsplit = \"right\"\nsize = \"30%\"\ncwd = \"sub\"\ncommand = {}\n[[pane]]\nsplit = \"down\"\nof = 2\ncommand = {}\nfocus = true\n",
            toml_str(&common::echo_split("pane", "-one")),
            toml_str(&common::echo_split("pane", "-two")),
            toml_str(&common::echo_split("pane", "-three")),
        ),
    )
    .expect("write");
    h.run(&format!("layout.open dev {}", project.display()));
    h.wait_for("every pane's command", |s, _| {
        ["pane-one", "pane-two", "pane-three"]
            .iter()
            .all(|t| count_output(s, t) == 1)
    })
    .await;
    assert_eq!(h.desktop.session().active().name, "dev");
    let tiled = h.tiled();
    assert_eq!(tiled.len(), 3, "{tiled:?}");
    let (_, r1) = tiled[0];
    let (p2, r2) = tiled[1];
    let (p3, r3) = tiled[2];
    // Two on the right, one above the other, in about 30% of the width.
    assert!(r2.x > r1.x && r3.x == r2.x && r3.y > r2.y, "{tiled:?}");
    let share = f64::from(r2.w) / f64::from(r1.w + r2.w);
    assert!((0.25..0.35).contains(&share), "{share}");
    assert_eq!(h.desktop.focused(), Some(p3));
    // Only Unix shells report their folder here.
    #[cfg(not(unix))]
    let _ = p2;
    #[cfg(unix)]
    {
        let cwd_of = |d: &Desktop, id: PaneId| {
            d.pane_infos()
                .into_iter()
                .find(|p| p.id == id.0)
                .and_then(|p| p.cwd)
                .map(std::path::PathBuf::from)
        };
        let sub = std::fs::canonicalize(project.join("sub")).expect("exists");
        assert_eq!(
            cwd_of(&h.desktop, p2).map(|c| std::fs::canonicalize(c).expect("exists")),
            Some(sub)
        );
    }

    // Saved, then opened again: the same tree.
    h.run("layout.save copy");
    let saved = std::fs::read_to_string(layouts.join("copy.toml")).expect("saved");
    let l = termos_core::layouts::Layout::parse("copy", &saved, Path::new("copy.toml"))
        .expect("a valid layout");
    let panes = &l.workspaces[0].pane;
    assert_eq!(panes.len(), 3, "{saved}");
    assert_eq!(panes[1].split, Some(termos_core::layouts::Split::Right));
    assert_eq!(panes[2].split, Some(termos_core::layouts::Split::Down));
    assert!(panes[2].focus, "{saved}");
    #[cfg(unix)]
    assert_eq!(panes[1].cwd.as_deref(), Some("sub"), "{saved}");
    let before: Vec<Rect> = tiled.iter().map(|(_, r)| *r).collect();
    h.run("layout.open copy");
    h.screen();
    assert_eq!(h.desktop.session().workspaces.len(), 3);
    let after: Vec<Rect> = h.tiled().iter().map(|(_, r)| *r).collect();
    assert_eq!(after, before, "{saved}");
}

#[tokio::test(flavor = "current_thread")]
async fn bad_layouts_say_why() {
    let mut h = Harness::new("");
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    let layouts = h.paths.layouts_dir();
    std::fs::create_dir_all(&layouts).expect("mkdir");
    std::fs::write(layouts.join("bad.toml"), "[[pane]]\nsplit = \"up\"\n").expect("write");
    h.run("layout.open bad");
    let toast = h.desktop.toast().unwrap_or_default().to_owned();
    assert!(toast.contains("bad.toml:2:"), "{toast}");
    h.run("layout.open none");
    let toast = h.desktop.toast().unwrap_or_default().to_owned();
    assert!(toast.contains("no layout named none"), "{toast}");
    assert_eq!(h.desktop.session().workspaces.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn the_drop_down_terminal_comes_and_goes_and_keeps_running() {
    let mut h = Harness::new("[quick]\nheight = \"50%\"\n");
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    let first = h.desktop.focused().expect("a pane");
    h.run("quick.toggle");
    let quick = h.desktop.focused().expect("the drop-down");
    assert_ne!(quick, first);
    let ws = h.desktop.session().active();
    let rect = ws.float_rect(quick).expect("a float");
    assert_eq!((rect.x, rect.y, rect.w), (0, 0, COLS));
    assert_eq!(rect.h, h.area().h / 2);
    h.type_text(&format!("{}\n", common::echo_split("quick", "-ok")));
    h.wait_for("its output", |s, _| count_output(s, "quick-ok") == 1)
        .await;
    // Hidden: the focus goes back; shown again: the same shell.
    h.run("quick.toggle");
    assert_eq!(h.desktop.focused(), Some(first));
    let screen = h.screen();
    assert_eq!(count_output(&screen, "quick-ok"), 0, "{screen}");
    assert!(
        !screen.lines().last().unwrap_or_default().contains("quick"),
        "not a minimized window: {screen}"
    );
    h.run("quick.toggle");
    assert_eq!(h.desktop.focused(), Some(quick));
    // On another workspace it comes along.
    h.run("quick.toggle");
    h.run("workspace.new");
    h.screen();
    h.run("quick.toggle");
    assert_eq!(h.desktop.focused(), Some(quick));
    assert_eq!(h.desktop.session().active_index(), 1);
    assert!(h.desktop.session().workspaces[0].contains(first));
    assert!(!h.desktop.session().workspaces[0].contains(quick));
}

#[tokio::test(flavor = "current_thread")]
async fn typing_reaches_every_synced_pane_until_it_stops() {
    let mut h = Harness::new("");
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    h.run("pane.split-right");
    h.run("pane.split-down");
    h.wait_for("three prompts", |_, d| {
        d.session().active().panes().len() == 3
    })
    .await;
    h.run("workspace.sync");
    let screen = h.screen();
    assert!(screen.lines().last().unwrap_or_default().contains("SYNC"));
    h.type_text(&format!("{}\n", common::echo_split("synced", "-ok")));
    h.wait_for("the output in all three", |s, _| {
        count_output(s, "synced-ok") == 3
    })
    .await;
    h.run("workspace.sync");
    let screen = h.screen();
    assert!(!screen.lines().last().unwrap_or_default().contains("SYNC"));
    h.type_text(&format!("{}\n", common::echo_split("only", "-one")));
    let screen = h
        .wait_for("the output in one", |s, _| count_output(s, "only-one") == 1)
        .await;
    // Give the others time: they never print it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count_output(&screen, "only-one"), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_snippet_asks_for_its_parameters_then_types_and_runs() {
    let mut h = Harness::new(
        "[[snippets]]\nname = \"greet\"\ncommand = \"echo {greeting:hello}-{who}\"\nrun = true\n",
    );
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    h.run("snippet.run greet");
    let screen = h.screen();
    let status = screen.lines().last().unwrap_or_default().to_owned();
    assert!(status.contains("(greet) greeting: hello"), "{status}");
    h.desktop
        .handle_client(C, ClientMsg::Key(KeyEvent::plain(KeyCode::Enter)));
    let screen = h.screen();
    let status = screen.lines().last().unwrap_or_default().to_owned();
    assert!(status.contains("(greet) who:"), "{status}");
    h.type_text("world\n");
    h.wait_for("the command's output", |s, _| {
        count_output(s, "hello-world") == 1
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_recording_is_asciinema_and_holds_the_output() {
    let mut h = Harness::new("");
    let rec = h.dir.path().join("recs");
    // The config names the folder.
    let cfg = std::fs::read_to_string(h.paths.config_file()).expect("config");
    std::fs::write(
        h.paths.config_file(),
        format!(
            "{cfg}\n[record]\ndir = {}\n",
            toml_str(&rec.display().to_string())
        ),
    )
    .expect("write");
    h.desktop.reload_config();
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    h.run("pane.record");
    let screen = h.screen();
    assert!(screen.contains("\u{25cf} REC"), "{screen}");
    h.type_text(&format!("{}\n", common::echo_split("rec", "-ok")));
    h.wait_for("the output", |s, _| count_output(s, "rec-ok") == 1)
        .await;
    h.run("pane.record");
    let toast = h.desktop.toast().unwrap_or_default().to_owned();
    assert!(toast.contains("saved the recording"), "{toast}");
    let file = std::fs::read_dir(&rec)
        .expect("the folder")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "cast"))
        .expect("a .cast file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let f = std::fs::File::open(&file).expect("open");
    let (header, events) =
        termos_core::cast::read(std::io::BufReader::new(f)).expect("valid asciinema v2");
    assert_eq!(header.version, 2);
    assert_eq!(header.width, COLS);
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            termos_core::cast::Event::Output(_, t) => Some(t.as_str()),
            termos_core::cast::Event::Resize(..) => None,
        })
        .collect();
    assert!(text.contains("rec-ok"), "{text:?}");
}
