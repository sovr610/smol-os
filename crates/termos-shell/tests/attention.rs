// SPDX-License-Identifier: GPL-3.0-or-later
//! Agents that need you, and command blocks (D-115, D-116), on a real
//! desktop: real shells (bash, zsh and fish where installed) with TermOS's
//! integration, programs' notifications, agent hooks' events, the jump
//! to the waiting agent and agents saved for a restore.

mod common;

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::{Duration, Instant};

use termos_core::config::Config;
use termos_core::ids::{ClientId, PaneId};
use termos_core::input::KeyEvent;
use termos_core::paths::Paths;
use termos_core::proto::{ClientMsg, ServerMsg};
use termos_render::caps::Caps;
use termos_render::grid::Grid;
use termos_shell::attention::{AgentEvent, EventKind};
use termos_shell::{Desktop, DesktopOptions};
#[cfg(unix)]
use termos_term::blocks::BlockState;
use termos_term::pty::PtyEvent;
use tokio::sync::mpsc;

const C: ClientId = ClientId(1);
const COLS: u16 = 100;
const ROWS: u16 = 30;

// Some parts are used by the Unix shells' tests only.
#[cfg_attr(windows, allow(dead_code))]
struct Harness {
    desktop: Desktop,
    opts: DesktopOptions,
    pty_rx: mpsc::Receiver<PtyEvent>,
    grid: Grid,
    dir: tempfile::TempDir,
    outbox: Vec<ServerMsg>,
}

impl Harness {
    /// A desktop whose panes run `shell` (default: the tests' shell)
    /// with `HOME` in a folder of its own, set up by `home`.
    fn new(shell: Option<&str>, extra: &str, home: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            state_dir: dir.path().join("state"),
            runtime_dir: dir.path().join("run"),
            data_dir: dir.path().join("data"),
        };
        let home_dir = dir.path().join("home");
        std::fs::create_dir_all(&home_dir).expect("mkdir");
        home(&home_dir);
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
        pane_env.push(("HOME".into(), home_dir.display().to_string()));
        // The shells' own settings come only from the test's home.
        pane_env.push((
            "XDG_CONFIG_HOME".into(),
            home_dir.join(".config").display().to_string(),
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
        };
        let mut desktop =
            Desktop::new(opts.clone(), config, tx, COLS, ROWS).expect("desktop starts");
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
            opts,
            pty_rx,
            grid: Grid::new(COLS, ROWS),
            dir,
            outbox: Vec::new(),
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

    #[cfg(unix)]
    fn key(&mut self, spec: &str) {
        self.desktop
            .handle_client(C, ClientMsg::Key(spec.parse().expect("key")));
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
        }
    }

    fn pane(&self) -> PaneId {
        self.desktop.session().active().panes()[0]
    }

    fn event(&mut self, pane: PaneId, kind: EventKind, message: &str) {
        self.desktop
            .agent_event(AgentEvent {
                pane: pane.0,
                cli: "claude".into(),
                kind,
                message: message.into(),
                conversation: Some("conv-1".into()),
                account: Some("work".into()),
                cwd: None,
            })
            .expect("a pane");
    }
}

/// `name`'s path when installed. CI names the shells that must be
/// (`TERMOS_REQUIRE_SHELLS=bash,zsh,fish`): there a missing one fails.
#[cfg(unix)]
fn installed(name: &str) -> Option<PathBuf> {
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

/// Runs two commands in `shell` and reads them back as blocks.
#[cfg(unix)]
async fn marks_commands(shell: &Path, home: impl FnOnce(&Path), prompt: &str) {
    let mut h = Harness::new(Some(&shell.display().to_string()), "", home);
    // Root's prompts end in `#`.
    let ends: Vec<char> = vec![prompt.chars().next().unwrap_or('$'), '#'];
    h.wait_for("the first prompt", move |s, _| {
        s.lines().any(|l| {
            l.trim_end()
                .chars()
                .last()
                .is_some_and(|c| ends.contains(&c))
        })
    })
    .await;
    h.type_text("echo one; echo two\n");
    // Typed while a command runs, keys are echoed into its output.
    h.wait_for("the echo to finish", |_, d| {
        d.last_command(None, true)
            .ok()
            .flatten()
            .is_some_and(|b| b.command == "echo one; echo two")
    })
    .await;
    h.type_text("false\n");
    let name = shell.display().to_string();
    h.wait_for("`false` to finish and a new prompt", |_, d| {
        d.last_command(None, true)
            .ok()
            .flatten()
            .is_some_and(|b| b.command == "false")
            && d.blocks(None)
                .ok()
                .and_then(|b| b.last().map(|b| b.state == BlockState::Prompt))
                .unwrap_or(false)
    })
    .await;
    let blocks = h.desktop.blocks(None).expect("a terminal");
    let echo = blocks
        .iter()
        .find(|b| b.command == "echo one; echo two")
        .unwrap_or_else(|| panic!("{name}: the echo block in {blocks:#?}"));
    assert_eq!(
        echo.output,
        ["one", "two"],
        "{name}: {blocks:#?}\n{}",
        h.screen()
    );
    assert_eq!(echo.exit, Some(0), "{name}");
    assert_eq!(echo.state, BlockState::Finished, "{name}");
    let last = blocks
        .iter()
        .rev()
        .find(|b| b.command == "false")
        .expect("false");
    assert_eq!(last.exit, Some(1), "{name}");
    assert!(last.output.is_empty(), "{name}: {last:?}");
    // The failed command is marked on screen, and the newest prompt is
    // a block of its own.
    let screen = h.screen();
    assert!(screen.contains("\u{2717} 1"), "{name}:\n{screen}");
    assert_eq!(
        blocks.last().map(|b| b.state),
        Some(BlockState::Prompt),
        "{name}: {blocks:#?}\n{screen}"
    );
    // Copying the last command's output (`false` printed nothing, so it
    // says so) and jumping to the previous prompt.
    h.desktop
        .run("pane.copy-output".parse().expect("a command"));
    h.key("Escape");
    drop(h.dir);
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn bash_marks_its_commands_after_the_users_bashrc() {
    let Some(bash) = installed("bash") else {
        eprintln!("skipped: bash is not installed");
        return;
    };
    // PS0 (the output's start) needs bash 4.4.
    let version = std::process::Command::new(&bash)
        .args(["-c", "echo $((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1]))"])
        .output()
        .expect("bash runs");
    let v: u32 = String::from_utf8_lossy(&version.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    if v < 404 {
        eprintln!("skipped: bash {v} is older than 4.4");
        return;
    }
    // The user's own prompt command reads `$?`: it must still see it.
    marks_commands(
        &bash,
        |home| {
            std::fs::write(
                home.join(".bashrc"),
                "PROMPT_COMMAND='last=$?'\nPS1='[$last]\\$ '\n",
            )
            .expect("write");
        },
        "$",
    )
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn zsh_marks_its_commands_after_the_users_zshrc() {
    let Some(zsh) = installed("zsh") else {
        eprintln!("skipped: zsh is not installed");
        return;
    };
    marks_commands(
        &zsh,
        |home| {
            std::fs::write(home.join(".zshrc"), "PROMPT='%# '\n").expect("write");
        },
        "%",
    )
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn fish_marks_its_commands() {
    let Some(fish) = installed("fish") else {
        eprintln!("skipped: fish is not installed");
        return;
    };
    marks_commands(
        &fish,
        |home| {
            let conf = home.join(".config/fish");
            std::fs::create_dir_all(&conf).expect("mkdir");
            std::fs::write(
                conf.join("config.fish"),
                "set -g fish_greeting\nfunction fish_prompt; echo -n '> '; end\n",
            )
            .expect("write");
        },
        ">",
    )
    .await;
}

/// PowerShell marks its commands too, with native programs' exit codes.
#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
async fn powershell_marks_its_commands() {
    let shell = ["pwsh.exe", "powershell.exe"].into_iter().find_map(|name| {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    });
    let Some(shell) = shell else {
        eprintln!("skipped: no PowerShell");
        return;
    };
    let mut h = Harness::new(Some(&shell.display().to_string()), "", |_| {});
    h.wait_for("a prompt", |s, _| s.contains("PS ")).await;
    h.type_text("echo one; echo two\n");
    h.wait_for("the echo to finish", |_, d| {
        d.last_command(None, true)
            .ok()
            .flatten()
            .is_some_and(|b| b.command == "echo one; echo two")
    })
    .await;
    h.type_text("cmd /c exit 3\n");
    h.wait_for("the exit code", |_, d| {
        d.last_command(None, true)
            .ok()
            .flatten()
            .is_some_and(|b| b.command == "cmd /c exit 3" && b.exit == Some(3))
    })
    .await;
    let blocks = h.desktop.blocks(None).expect("a terminal");
    let echo = blocks
        .iter()
        .find(|b| b.command == "echo one; echo two")
        .expect("the echo");
    assert_eq!(echo.output, ["one", "two"], "{blocks:#?}");
    assert_eq!(echo.exit, Some(0));
}

#[tokio::test(flavor = "current_thread")]
async fn needs_you_is_loud_and_done_is_quiet() {
    let mut h = Harness::new(None, "", |_| {});
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    let first = h.pane();
    // A second workspace; the first pane's agent needs you.
    h.desktop.run("workspace.new".parse().expect("a command"));
    h.wait_for("the second workspace", |s, _| s.contains("1:"))
        .await;
    h.event(first, EventKind::NeedsYou, "wants to use Bash");
    let screen = h.screen();
    let status = screen.lines().last().unwrap_or_default().to_owned();
    assert!(
        status.contains("0:") && status.contains('\u{25cf}'),
        "{status}"
    );
    assert!(
        screen.contains("claude in 0:") && screen.contains("needs you: wants to use Bash"),
        "the toast:\n{screen}"
    );
    assert!(
        h.outbox.iter().any(|m| matches!(
            m,
            ServerMsg::Notify { title, body } if title == "claude needs you" && body.contains("wants to use Bash")
        )),
        "a system notification: {:?}",
        h.outbox
    );
    // Asked again: no second toast or notification.
    let sent = h.outbox.len();
    h.event(first, EventKind::NeedsYou, "wants to use Edit");
    h.screen();
    assert!(
        !h.outbox[sent..]
            .iter()
            .any(|m| matches!(m, ServerMsg::Notify { .. }))
    );
    // One key goes there; the badge shows on the pane until it is answered.
    h.desktop.run("agent.waiting".parse().expect("a command"));
    assert_eq!(h.desktop.session().active_index(), 0);
    let screen = h.screen();
    assert!(
        screen
            .lines()
            .next()
            .unwrap_or_default()
            .contains("needs you"),
        "{screen}"
    );
    h.type_text("y");
    let screen = h.screen();
    assert!(
        !screen
            .lines()
            .next()
            .unwrap_or_default()
            .contains("needs you"),
        "{screen}"
    );
    assert!(h.desktop.waiting().is_empty());

    // A finished turn elsewhere: a tick on its tab, no toast, no
    // system notification; seeing the workspace clears it.
    h.desktop
        .run("workspace.select 1".parse().expect("a command"));
    h.screen();
    let sent = h.outbox.len();
    h.event(first, EventKind::Done, "All tests pass.");
    let screen = h.screen();
    let status = screen.lines().last().unwrap_or_default().to_owned();
    assert!(status.contains('\u{2713}'), "{status}");
    assert!(!screen.contains("finished"), "no toast:\n{screen}");
    assert!(
        !h.outbox[sent..]
            .iter()
            .any(|m| matches!(m, ServerMsg::Notify { .. }))
    );
    h.desktop
        .run("workspace.select 0".parse().expect("a command"));
    let screen = h.screen();
    assert!(!screen.contains('\u{2713}'), "{screen}");
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn a_programs_notification_marks_its_pane() {
    let mut h = Harness::new(None, "[notifications]\nsystem = false\n", |_| {});
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    h.type_text("printf '\\033]9;Build done\\007'\n");
    h.wait_for("the notification", |s, _| {
        s.contains("needs you: Build done")
    })
    .await;
    assert_eq!(h.desktop.waiting().len(), 1);
    assert!(
        !h.outbox
            .iter()
            .any(|m| matches!(m, ServerMsg::Notify { .. }))
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn marks_printed_by_hand_make_blocks_and_agents_are_saved() {
    let mut h = Harness::new(None, "", |_| {});
    h.wait_for("a prompt", |s, _| !s.trim().is_empty()).await;
    // A shell without integration (`sh`): a script prints the marks.
    h.type_text(
        "printf '\\033]133;A\\007$ \\033]133;B\\007make\\n\\033]133;C\\007built\\n\\033]133;D;2\\007\\033]133;A\\007'\n",
    );
    h.wait_for("the block", |_, d| {
        d.last_command(None, true)
            .ok()
            .flatten()
            .is_some_and(|b| b.command == "make" && b.exit == Some(2))
    })
    .await;
    let b = h
        .desktop
        .last_command(None, true)
        .expect("a terminal")
        .expect("a block");
    let all = h.desktop.blocks(None).expect("a terminal");
    assert_eq!(b.output, ["built"], "{all:#?}\n{}", h.screen());
    h.desktop
        .run("pane.copy-output".parse().expect("a command"));
    let screen = h.screen();
    assert!(
        screen.contains("copied 1 line of `make`'s output"),
        "{screen}"
    );

    // An agent its hooks reported: saved for the restore, which types it
    // into the shell again.
    let pane = h.pane();
    h.event(pane, EventKind::Started, "");
    let snap = h.desktop.snapshot();
    let saved = snap
        .panes
        .iter()
        .find(|p| p.id == pane.0)
        .and_then(|p| p.agent.clone())
        .expect("the agent is saved");
    assert_eq!(saved.cli, "claude");
    assert_eq!(saved.account.as_deref(), Some("work"));
    assert_eq!(saved.conversation.as_deref(), Some("conv-1"));
    assert!(!saved.window);
    // Ended: nothing to restore.
    h.event(pane, EventKind::Ended, "");
    let snap2 = h.desktop.snapshot();
    assert!(snap2.panes.iter().all(|p| p.agent.is_none()));

    let config = Config::load(&h.desktop_config()).expect("config");
    let (tx, mut rx) = mpsc::channel(1024);
    let opts = h.opts.clone();
    let (mut d, skipped) = Desktop::from_snapshot(opts, config, tx, &snap).expect("restores");
    assert!(skipped.is_empty(), "{skipped:?}");
    d.handle_client(
        C,
        ClientMsg::Hello {
            version: termos_core::proto::PROTOCOL_VERSION,
            caps: Caps::default(),
            cols: COLS,
            rows: ROWS,
        },
    );
    let mut grid = Grid::new(COLS, ROWS);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        d.render(C, &mut grid);
        let text = grid.to_text();
        if text.contains("tos agent external --account work --resume --conversation conv-1 claude")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent typed in again:\n{text}"
        );
        if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            d.on_pty(ev);
        }
    }
}

#[cfg(unix)]
impl Harness {
    fn desktop_config(&self) -> PathBuf {
        self.dir.path().join("config").join("config.toml")
    }
}
