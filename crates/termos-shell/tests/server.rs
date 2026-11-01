// SPDX-License-Identifier: GPL-3.0-or-later
//! The session server over a real Unix socket: attach, detach and reattach,
//! several clients, the handshake checks, the control API, session
//! switching, incremental frames and a silent idle server.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use termos_core::config::Config;
use termos_core::input::{KeyCode, KeyEvent};
use termos_core::paths::{Paths, current_uid};
use termos_core::proto::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use termos_core::transport::{ClientEnd, ConnectError, connect_frontend, control_call};
use termos_core::wire::{ConnKind, Hello, codes};
use termos_render::caps::Caps;
use termos_render::diff::{apply, cell_count};
use termos_render::grid::Grid;
use termos_shell::server::{Server, ServerOptions};
use tokio::task::JoinHandle;

const BASE_CONFIG: &str =
    "shell = \"/bin/sh\"\n[tutorial]\nshow-on-first-run = false\n[status]\nclock = false\n";

struct TestServer {
    socket: PathBuf,
    handle: JoinHandle<String>,
    _dir: Option<tempfile::TempDir>,
}

fn paths_in(dir: &Path) -> Paths {
    Paths {
        config_dir: dir.join("config"),
        state_dir: dir.join("state"),
        runtime_dir: dir.join("run"),
    }
}

/// Starts a server whose state lives in `dir` (so a later server can find
/// its snapshots).
fn start_in(dir: &Path, extra_config: &str, tweak: impl FnOnce(&mut ServerOptions)) -> TestServer {
    let paths = paths_in(dir);
    let config_path = paths.config_file();
    let config =
        Config::parse(&format!("{BASE_CONFIG}{extra_config}"), &config_path).expect("config");
    let mut opts = ServerOptions::new("test", paths, config_path);
    opts.env_shell = None;
    tweak(&mut opts);
    let server = Server::new(opts, config);
    let socket = server.options().socket_path.clone();
    let listener = server.bind().expect("bind");
    let handle = tokio::spawn(server.run(Some(listener)));
    TestServer {
        socket,
        handle,
        _dir: None,
    }
}

async fn start_with(extra_config: &str, tweak: impl FnOnce(&mut ServerOptions)) -> TestServer {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut srv = start_in(dir.path(), extra_config, tweak);
    srv._dir = Some(dir);
    srv
}

async fn start() -> TestServer {
    start_with("", |_| {}).await
}

fn frontend(session: Option<&str>, create: bool) -> Hello {
    let mut h = Hello::new(ConnKind::Frontend);
    h.session = session.map(str::to_owned);
    h.create = create;
    h
}

/// An attached client with a mirror of its screen.
struct Screen {
    client: ClientEnd,
    grid: Grid,
    session: String,
}

impl Screen {
    async fn attach(socket: &Path, hello: Hello, cols: u16, rows: u16) -> Self {
        let (reply, client) = connect_frontend(socket, &hello).await.expect("attach");
        client
            .tx
            .send(ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                caps: Caps::default(),
                cols,
                rows,
            })
            .expect("server alive");
        Self {
            client,
            grid: Grid::new(cols, rows),
            session: reply.session.unwrap_or_default(),
        }
    }

    fn text(&self) -> String {
        self.grid.to_text()
    }

    fn key(&self, k: KeyEvent) {
        self.client
            .tx
            .send(ClientMsg::Key(k))
            .expect("server alive");
    }

    fn type_text(&self, text: &str) {
        for c in text.chars() {
            self.key(match c {
                '\r' => KeyEvent::plain(KeyCode::Enter),
                c => KeyEvent::ch(c),
            });
        }
    }

    /// Applies frames until `pred(screen)` holds; returns the cells sent.
    async fn until(&mut self, what: &str, pred: impl Fn(&str) -> bool) -> usize {
        let mut cells = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if pred(&self.text()) {
                return cells;
            }
            let msg = tokio::time::timeout_at(deadline, self.client.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}:\n{}", self.text()));
            match msg {
                Some(ServerMsg::Frame(f)) => {
                    if f.clear {
                        self.grid.resize(f.cols, f.rows);
                    }
                    cells += cell_count(&f.spans);
                    apply(&mut self.grid, &f.spans);
                }
                Some(_) => {}
                None => panic!("server hung up while waiting for {what}:\n{}", self.text()),
            }
        }
    }

    /// Applies frames until the server ends the connection (`Detached`,
    /// `Exit`, or hanging up) and returns that.
    async fn next_event(&mut self, what: &str) -> Option<ServerMsg> {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(15), self.client.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}:\n{}", self.text()));
            match msg {
                Some(ServerMsg::Frame(f)) => {
                    if f.clear {
                        self.grid.resize(f.cols, f.rows);
                    }
                    apply(&mut self.grid, &f.spans);
                }
                Some(ServerMsg::Title(_) | ServerMsg::Bell | ServerMsg::Clipboard(_)) => {}
                other => return other,
            }
        }
    }

    /// Waits until a shell prompt is on screen.
    async fn ready(&mut self) {
        self.until("a shell prompt", |s| {
            s.lines().any(|l| l.trim_end().ends_with(['$', '#']))
        })
        .await;
    }
}

/// Runs `cmd` in the shell and waits for its output line.
async fn run_shell(screen: &mut Screen, cmd: &str, expect: &str) {
    screen.type_text(&format!("{cmd}\r"));
    screen
        .until(expect, |s| {
            s.lines()
                .any(|l| l.trim_end().ends_with(expect) && !l.contains("echo"))
        })
        .await;
}

fn rpc_code(err: ConnectError) -> i64 {
    match err {
        ConnectError::Rpc(e) => e.code,
        other => panic!("expected an RPC error, got {other}"),
    }
}

async fn call(socket: &Path, method: &str, params: Value) -> Value {
    control_call(socket, method, Some(params))
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e}"))
}

#[tokio::test]
async fn frames_are_incremental_and_idle_is_silent() {
    let srv = start().await;
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 80, 24).await;
    assert_eq!(s.session, "main");
    s.ready().await;
    run_shell(&mut s, "echo rea''dy", "ready").await;
    // Let the prompt settle, then make sure nothing else arrives: no
    // timers, no redraws while idle.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(m) = s.client.rx.try_recv() {
        if let ServerMsg::Frame(f) = m {
            apply(&mut s.grid, &f.spans);
        }
    }
    let idle = tokio::time::timeout(Duration::from_millis(500), s.client.rx.recv()).await;
    assert!(idle.is_err(), "idle server sent {idle:?}");

    // One keystroke echoed by the shell changes exactly one cell.
    s.key(KeyEvent::ch('Q'));
    let cells = s.until("echoed Q", |t| t.contains('Q')).await;
    assert_eq!(
        cells, 1,
        "a single echoed character should be a one-cell frame"
    );
}

#[tokio::test]
async fn detach_and_reattach_keep_the_screen_and_the_shell() {
    let srv = start().await;
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 80, 24).await;
    s.ready().await;
    run_shell(&mut s, "echo mark''er-1", "marker-1").await;
    // Panes learn where the server is.
    run_shell(&mut s, "echo T=${TERMOS##*,}", "T=$1").await;
    // C-b d detaches.
    s.key(KeyEvent::ctrl('b'));
    s.key(KeyEvent::ch('d'));
    match s.next_event("detach").await {
        Some(ServerMsg::Detached { session }) => assert_eq!(session, "main"),
        other => panic!("expected Detached, got {other:?}"),
    }
    let list = call(&srv.socket, "session.list", json!({})).await;
    assert_eq!(list[0]["name"], "main");
    assert_eq!(list[0]["clients"], 0);

    // Reattach (no session named, no create): the same shell and screen.
    let mut s = Screen::attach(&srv.socket, frontend(None, false), 80, 24).await;
    assert_eq!(s.session, "main");
    s.until("the earlier output", |t| t.contains("marker-1"))
        .await;
    run_shell(&mut s, "echo mark''er-2", "marker-2").await;
    assert!(s.text().contains("marker-1"));
}

#[tokio::test]
async fn two_clients_share_a_session() {
    let srv = start_with("[session]\nwindow-size = \"largest\"\n", |_| {}).await;
    let mut a = Screen::attach(&srv.socket, frontend(Some("main"), true), 80, 24).await;
    a.ready().await;
    let mut b = Screen::attach(&srv.socket, frontend(Some("main"), false), 60, 20).await;
    // The session stays at the largest size; the small client sees it
    // cropped to its terminal (so without the status line at row 23).
    b.ready().await;
    assert!(!b.text().contains("[main]"));
    assert_eq!((b.grid.cols(), b.grid.rows()), (60, 20));
    // Keys from the second client reach the pane; both see the output.
    run_shell(&mut b, "echo sha''red", "shared").await;
    a.until("output typed by the other client", |t| t.contains("shared"))
        .await;
    let clients = call(&srv.socket, "client.list", json!({})).await;
    assert_eq!(clients.as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn handshake_rejects_other_users_bad_versions_and_missing_sessions() {
    let srv = start_with("", |o| o.allowed_uid = current_uid().wrapping_add(1)).await;
    let err = connect_frontend(&srv.socket, &frontend(None, true))
        .await
        .expect_err("a foreign uid must be refused");
    assert_eq!(rpc_code(err), codes::FORBIDDEN);
    let err = control_call(&srv.socket, "server.info", None)
        .await
        .expect_err("control connections too");
    assert_eq!(rpc_code(err), codes::FORBIDDEN);

    let srv = start().await;
    let mut old = frontend(None, true);
    old.version = PROTOCOL_VERSION + 1;
    let err = connect_frontend(&srv.socket, &old)
        .await
        .expect_err("version mismatch");
    assert_eq!(rpc_code(err), codes::VERSION_MISMATCH);
    let err = connect_frontend(&srv.socket, &frontend(Some("nope"), false))
        .await
        .expect_err("no such session");
    assert_eq!(rpc_code(err), codes::NOT_FOUND);
}

#[tokio::test]
async fn control_api_drives_sessions_and_panes() {
    let srv = start().await;
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 100, 30).await;
    s.ready().await;

    let info = call(&srv.socket, "server.info", json!({})).await;
    assert_eq!(info["pid"], std::process::id());
    assert_eq!(info["sessions"], 1);

    // Named commands work through command.run and directly as methods.
    call(
        &srv.socket,
        "command.run",
        json!({"command": "pane.split-right"}),
    )
    .await;
    call(&srv.socket, "pane.split-down", json!({"session": "main"})).await;
    let panes = call(&srv.socket, "pane.list", json!({"session": "main"})).await;
    let panes = panes.as_array().expect("array");
    assert_eq!(panes.len(), 3);
    assert!(panes.iter().all(|p| p["session"] == "main"));
    let focused: Vec<&Value> = panes.iter().filter(|p| p["focused"] == true).collect();
    assert_eq!(focused.len(), 1);
    let target = focused[0]["id"].as_u64().expect("pane id");

    // Text typed into a pane by id.
    call(
        &srv.socket,
        "pane.send-text",
        json!({"pane": target, "text": "echo via''-api\r"}),
    )
    .await;
    s.until("text sent through the API", |t| {
        t.lines()
            .any(|l| l.contains("via-api") && !l.contains("echo"))
    })
    .await;
    call(
        &srv.socket,
        "pane.send-keys",
        json!({"pane": target, "keys": ["e", "c", "h", "o", "Space", "k", "e", "y", "s", "Enter"]}),
    )
    .await;
    s.until("keys sent through the API", |t| {
        t.lines()
            .any(|l| l.trim_end().ends_with("keys") && !l.contains("echo"))
    })
    .await;

    call(
        &srv.socket,
        "notify.send",
        json!({"text": "hello from a script"}),
    )
    .await;
    s.until("the toast", |t| t.contains("hello from a script"))
        .await;

    call(&srv.socket, "pane.kill", json!({"pane": target})).await;
    let panes = call(&srv.socket, "pane.list", json!({})).await;
    assert_eq!(panes.as_array().map(Vec::len), Some(2));

    // Sessions: create, rename, list, kill.
    let new = call(&srv.socket, "session.new", json!({"name": "work"})).await;
    assert_eq!(new["name"], "work");
    call(
        &srv.socket,
        "session.rename",
        json!({"session": "work", "name": "play"}),
    )
    .await;
    let list = call(&srv.socket, "session.list", json!({})).await;
    let names: Vec<&str> = list
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|v| v["name"].as_str())
        .collect();
    assert_eq!(names, ["main", "play"]);
    call(&srv.socket, "session.kill", json!({"session": "play"})).await;

    // Errors are JSON-RPC errors with useful codes.
    let err = control_call(&srv.socket, "no.such-method", None)
        .await
        .expect_err("unknown method");
    assert_eq!(rpc_code(err), codes::METHOD_NOT_FOUND);
    let err = control_call(&srv.socket, "session.new", Some(json!({"name": "main"})))
        .await
        .expect_err("duplicate name");
    assert_eq!(rpc_code(err), codes::INVALID_PARAMS);
    let err = control_call(&srv.socket, "pane.kill", Some(json!({"pane": 999_999})))
        .await
        .expect_err("no such pane");
    assert_eq!(rpc_code(err), codes::NOT_FOUND);

    // Give the killed session a moment to go, then check it went.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let list = call(&srv.socket, "session.list", json!({})).await;
    assert_eq!(list.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn clients_switch_sessions_and_move_when_one_ends() {
    let srv = start().await;
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 80, 24).await;
    s.ready().await;
    call(&srv.socket, "session.new", json!({"name": "work"})).await;
    // C-b ) goes to the next session.
    s.key(KeyEvent::ctrl('b'));
    s.key(KeyEvent::ch(')'));
    s.until("the work session", |t| t.contains("[work]")).await;
    s.ready().await;
    run_shell(&mut s, "echo in-wo''rk", "in-work").await;
    // Back with C-b (.
    s.key(KeyEvent::ctrl('b'));
    s.key(KeyEvent::ch('('));
    s.until("the main session", |t| t.contains("[main]")).await;
    let list = call(&srv.socket, "session.list", json!({})).await;
    assert_eq!(list[0]["clients"], 1);
    assert_eq!(list[1]["clients"], 0);
    // Ending the session a client is in moves it to another one.
    call(&srv.socket, "session.kill", json!({"session": "main"})).await;
    s.until("the remaining session", |t| {
        t.contains("[work]") && t.contains("in-work")
    })
    .await;
}

#[tokio::test]
async fn the_server_stops_with_its_last_session_and_removes_the_socket() {
    let srv = start().await;
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 80, 24).await;
    s.ready().await;
    s.type_text("exit\r");
    match s.next_event("exit").await {
        Some(ServerMsg::Exit { .. }) => {}
        other => panic!("expected Exit, got {other:?}"),
    }
    let reason = tokio::time::timeout(Duration::from_secs(10), srv.handle)
        .await
        .expect("server stops")
        .expect("server task");
    assert_eq!(reason, "no sessions are left");
    assert!(!srv.socket.exists(), "socket file left behind");
}

#[tokio::test]
async fn server_kill_and_resize() {
    let srv = start().await;
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 80, 24).await;
    s.ready().await;
    s.client
        .tx
        .send(ClientMsg::Resize { cols: 60, rows: 20 })
        .expect("alive");
    loop {
        match s.client.rx.recv().await {
            Some(ServerMsg::Frame(f)) if f.clear && f.cols == 60 && f.rows == 20 => break,
            Some(_) => {}
            None => panic!("no full frame after resize"),
        }
    }
    // A second server on the same socket is refused while this one runs.
    let dup = {
        let dir = srv.socket.parent().expect("socket dir").to_owned();
        let paths = Paths {
            config_dir: dir.join("c"),
            state_dir: dir.join("s"),
            runtime_dir: dir.clone(),
        };
        let mut o = ServerOptions::new("test", paths, dir.join("c.toml"));
        o.socket_path = srv.socket.clone();
        Server::new(o, Config::default()).bind()
    };
    assert!(dup.is_err(), "a second server bound a live socket");

    call(&srv.socket, "server.kill", json!({})).await;
    match s.next_event("exit").await {
        Some(ServerMsg::Exit { reason }) => assert!(reason.contains("killed"), "{reason}"),
        other => panic!("expected Exit, got {other:?}"),
    }
    let reason = tokio::time::timeout(Duration::from_secs(10), srv.handle)
        .await
        .expect("server stops")
        .expect("server task");
    assert_eq!(reason, "killed by request");
    assert!(!srv.socket.exists());
}

/// Asks the server to stop and waits until it has.
async fn kill(srv: TestServer) {
    call(&srv.socket, "server.kill", json!({})).await;
    tokio::time::timeout(Duration::from_secs(10), srv.handle)
        .await
        .expect("server stops")
        .expect("server task");
}

/// Polls `pane.list` until `pred` holds for it.
async fn until_panes(socket: &Path, what: &str, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let panes = call(socket, "pane.list", json!({})).await;
        let panes = panes.as_array().cloned().unwrap_or_default();
        if pred(&panes) {
            return panes;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}: {panes:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn sessions_survive_a_server_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = dir.path().join("work dir");
    std::fs::create_dir(&work).expect("mkdir");
    std::fs::write(work.join("notes.txt"), "remembered line\n").expect("write");
    let snapshots = paths_in(dir.path()).sessions_dir("test");
    let config = |policy: &str| {
        format!(
            "[session]\nrestore = \"{policy}\"\nrestore-programs = [\"tail\"]\nsnapshot-delay-ms = 50\n"
        )
    };

    // 1. A session with two panes: `tail` (allowlisted) in `work dir`, and
    //    `sleep` (not allowlisted).
    let srv = start_in(dir.path(), &config("always"), |_| {});
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 100, 30).await;
    s.ready().await;
    s.type_text(&format!("cd '{}' && tail -f notes.txt\r", work.display()));
    s.until("tail", |t| t.contains("remembered line")).await;
    call(
        &srv.socket,
        "command.run",
        json!({"command": "pane.split-right"}),
    )
    .await;
    s.ready().await;
    s.type_text("sleep 1000\r");
    until_panes(&srv.socket, "sleep in front", |p| {
        p.iter().any(|p| p["name"] == "sleep")
    })
    .await;
    kill(srv).await;
    let saved = std::fs::read_to_string(snapshots.join("main.json")).expect("snapshot written");
    assert!(
        saved.contains("\"notes.txt\""),
        "tail's command line is kept"
    );
    assert!(saved.contains("\"sleep\""), "sleep is named");
    assert!(
        !saved.contains("\"1000\""),
        "but its command line is not kept"
    );

    // 2. restore = always: the next server brings it back by itself.
    let srv = start_in(dir.path(), &config("always"), |_| {});
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 100, 30).await;
    assert_eq!(s.session, "main");
    s.until("tail re-run in the saved directory", |t| {
        t.contains("remembered line")
    })
    .await;
    s.until("the skipped program named", |t| {
        t.contains("not restarted: sleep")
    })
    .await;
    let panes = until_panes(&srv.socket, "two panes", |p| p.len() == 2).await;
    assert!(
        panes
            .iter()
            .any(|p| p["cwd"].as_str().is_some_and(|c| c.ends_with("work dir"))),
        "{panes:?}"
    );
    kill(srv).await;

    // 3. restore = ask, answered "y": the asking session becomes the saved
    //    one.
    let srv = start_in(dir.path(), &config("ask"), |_| {});
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 100, 30).await;
    s.until("the question", |t| {
        t.contains("Restore session main saved at")
    })
    .await;
    s.key(KeyEvent::ch('y'));
    s.until("the restored session", |t| t.contains("remembered line"))
        .await;
    until_panes(&srv.socket, "two panes", |p| p.len() == 2).await;
    let list = call(&srv.socket, "session.list", json!({})).await;
    assert_eq!(list.as_array().map(Vec::len), Some(1), "replaced in place");
    kill(srv).await;

    // 4. restore = ask, answered "n": kept for later, restored on demand
    //    next to the running session.
    let srv = start_in(dir.path(), &config("ask"), |_| {});
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 100, 30).await;
    s.until("the question", |t| t.contains("Restore session main"))
        .await;
    s.key(KeyEvent::ch('n'));
    s.until("the answer acknowledged", |t| {
        t.contains("saved sessions kept")
    })
    .await;
    let info = call(&srv.socket, "server.info", json!({})).await;
    assert_eq!(info["restorable"], json!(["main"]));
    let restored = call(&srv.socket, "session.restore", json!({})).await;
    assert_eq!(restored["restored"], json!(["main-2"]));
    let err = control_call(&srv.socket, "session.restore", Some(json!({})))
        .await
        .expect_err("nothing left to restore");
    assert_eq!(rpc_code(err), codes::NOT_FOUND);

    // 5. Ending sessions on purpose deletes their snapshots.
    call(&srv.socket, "session.kill", json!({"session": "main-2"})).await;
    call(&srv.socket, "session.kill", json!({"session": "main"})).await;
    let reason = tokio::time::timeout(Duration::from_secs(10), srv.handle)
        .await
        .expect("server stops")
        .expect("server task");
    assert_eq!(reason, "no sessions are left");
    let left: Vec<_> = std::fs::read_dir(&snapshots)
        .expect("dir")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "snapshots left behind: {left:?}");
}

fn write_plugin(dir: &Path, name: &str, exec: &[&str], permissions: &str) {
    let d = paths_in(dir).plugins_dir().join(name);
    std::fs::create_dir_all(&d).expect("mkdir");
    let exec: Vec<String> = exec.iter().map(|a| format!("{a:?}")).collect();
    std::fs::write(
        d.join("plugin.toml"),
        format!(
            "name = \"{name}\"\nversion = \"0.1.0\"\nexec = [{}]\n[permissions]\n{permissions}\n",
            exec.join(", ")
        ),
    )
    .expect("write manifest");
}

/// Polls `plugin.list` until `pred` holds for the named plugin.
async fn until_plugin(socket: &Path, name: &str, what: &str, pred: impl Fn(&Value) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let list = call(socket, "plugin.list", json!({})).await;
        let entry = list
            .as_array()
            .and_then(|l| l.iter().find(|p| p["name"] == name))
            .cloned()
            .unwrap_or(Value::Null);
        if pred(&entry) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}: {list}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn plugins_add_windows_widgets_and_commands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello_plugin.py");
    let script = script.to_str().expect("utf-8 path");
    let all = "windows = true\nstatus = true\ncommands = true\nnotify = true";
    write_plugin(dir.path(), "hello", &["python3", script], all);
    write_plugin(dir.path(), "limited", &["python3", script], "notify = true");
    // Stands in for a plugin process so the test can drive the Rust SDK
    // with its token.
    write_plugin(
        dir.path(),
        "rusty",
        &[
            "/bin/sh",
            "-c",
            "printf %s \"$TERMOS_PLUGIN_TOKEN\" > token.tmp && mv token.tmp token && exec sleep 600",
        ],
        all,
    );
    let enabled = "[plugins]\nenabled = [\"hello\", \"limited\", \"rusty\", \"missing\"]\n";
    let config_file = paths_in(dir.path()).config_file();
    std::fs::create_dir_all(config_file.parent().expect("dir")).expect("mkdir");
    std::fs::write(&config_file, format!("{BASE_CONFIG}{enabled}")).expect("config");
    let srv = start_in(dir.path(), enabled, |_| {});
    let mut s = Screen::attach(&srv.socket, frontend(None, true), 120, 40).await;

    // The Python plugin's window and status widget appear.
    s.until("the plugin window", |t| t.contains("Hello from Python"))
        .await;
    s.until("the status widget", |t| t.contains("py:hello"))
        .await;
    until_plugin(&srv.socket, "hello", "hello's command", |p| {
        p["commands"] == json!(["hello.ping"])
    })
    .await;
    // Without the windows permission the plugin is refused, and told why.
    until_plugin(&srv.socket, "limited", "limited connected", |p| {
        p["connected"] == true && p["windows"] == json!([])
    })
    .await;

    // Commands run from the control API and from the palette.
    call(&srv.socket, "hello.ping", json!({})).await;
    s.until("the first pong", |t| t.contains("hello: pong #1 in main"))
        .await;
    s.key(KeyEvent::ctrl(' '));
    s.type_text("ping the plugin");
    s.until("the plugin command in the palette", |t| {
        t.contains("Ping the plugin (hello)")
    })
    .await;
    s.key(KeyEvent::plain(KeyCode::Enter));
    s.until("the second pong", |t| t.contains("hello: pong #2 in main"))
        .await;

    // Keys in the (focused) plugin window go to the plugin.
    s.key(KeyEvent::ch('x'));
    s.until("the plugin's redraw", |t| t.contains("you pressed x"))
        .await;

    // The Rust SDK against the real server, with the token the server gave
    // the stand-in process.
    let token_file = paths_in(dir.path()).plugins_dir().join("rusty/token");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let token = loop {
        if let Ok(t) = std::fs::read_to_string(&token_file) {
            break t;
        }
        assert!(tokio::time::Instant::now() < deadline, "no token file");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let socket = srv.socket.clone();
    let sdk = tokio::task::spawn_blocking(move || {
        let mut p = termos_plugin::Plugin::connect(&socket, &token, "rusty")?;
        let w = p.open_window("rust", &[termos_plugin::Line::from("Hello from Rust")])?;
        p.set_status(vec![termos_plugin::Span::new("rs:ok").fg("green")])?;
        p.register_command("go", "Go somewhere")?;
        loop {
            match p.next_event()? {
                Some(termos_plugin::Event::Command { name, session }) => {
                    p.close_window(w)?;
                    return Ok::<_, termos_plugin::Error>((name, session));
                }
                Some(_) => {}
                None => return Err(termos_plugin::Error::Closed),
            }
        }
    });
    s.until("the Rust window and widget", |t| {
        t.contains("Hello from Rust") && t.contains("rs:ok")
    })
    .await;
    // A second hello with the same token is refused. (The SDK blocks, and
    // this test's runtime thread also runs the server.)
    let (socket, again) = (srv.socket.clone(), token_file.clone());
    let err = tokio::task::spawn_blocking(move || {
        let token = std::fs::read_to_string(again).expect("token");
        termos_plugin::Plugin::connect(&socket, &token, "again")
    })
    .await
    .expect("thread")
    .expect_err("one connection per plugin");
    assert!(err.to_string().contains("already connected"), "{err}");
    call(&srv.socket, "rusty.go", json!({})).await;
    let (name, session) = sdk.await.expect("sdk thread").expect("sdk calls");
    assert_eq!((name.as_str(), session.as_deref()), ("go", Some("main")));
    s.until("the Rust window closed", |t| !t.contains("Hello from Rust"))
        .await;

    // Closing a window tells its plugin.
    let window = call(&srv.socket, "plugin.list", json!({})).await;
    let window = window
        .as_array()
        .and_then(|l| l.iter().find(|p| p["name"] == "hello"))
        .and_then(|p| p["windows"][0].as_u64())
        .expect("hello has a window");
    call(&srv.socket, "pane.kill", json!({"pane": window})).await;
    s.until("the closed notice", |t| {
        t.contains(&format!("hello: window {window} closed"))
    })
    .await;

    // The startup problem with the missing plugin is in the notification
    // center.
    s.key(KeyEvent::ctrl('b'));
    s.key(KeyEvent::ch('N'));
    s.until("the missing plugin reported", |t| {
        t.contains("plugin missing not started")
    })
    .await;
    s.key(KeyEvent::ch('q'));

    // Disabling plugins (config reload) stops them and removes their UI.
    std::fs::write(&config_file, BASE_CONFIG).expect("config");
    call(&srv.socket, "config.reload", json!({})).await;
    s.until("the widgets gone", |t| !t.contains("py:hello"))
        .await;
    let list = call(&srv.socket, "plugin.list", json!({})).await;
    assert_eq!(list, json!([]));
}
