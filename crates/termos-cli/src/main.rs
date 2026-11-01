// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos`: the TermOS command-line entry point.
//!
//! Like tmux, TermOS is a server and clients (DECISIONS.md D-029 to D-031).
//! The server is a daemon that owns the sessions and their programs; it is
//! started on demand by the first client and stops when its last session
//! ends. `termos` with no subcommand attaches to the session `main`,
//! creating it (and the server) if needed. Inside a TermOS pane, commands
//! such as `termos msg` and `termos detach` talk to the server and session
//! the pane belongs to (`$TERMOS`).

mod client;
mod images;
mod probe;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use termos_core::config::{Config, DEFAULT_CONFIG};
use termos_core::paths::{Paths, server_name};
use termos_core::transport::{ConnectError, connect_frontend, control_call};
use termos_core::watch::ConfigWatcher;
use termos_core::wire::{ConnKind, Hello};
use termos_render::caps::Caps;
use termos_shell::keys::KeyTables;
use termos_shell::notify::Level;
use termos_shell::server::{Server, ServerOptions};
use termos_shell::snapshot::describe_time;
use tokio::sync::mpsc;

/// Long version string, e.g. `0.0.0 (x86_64-unknown-linux-gnu)`.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("TERMOS_BUILD_TARGET"),
    ")"
);

/// The server name when neither `-L` nor `-S` is given.
const DEFAULT_SERVER: &str = "default";

/// TermOS: a terminal-first desktop environment.
///
/// Run without a subcommand to attach to the session "main" in this
/// terminal, starting it if needed. The prefix key is Ctrl+B (like tmux);
/// Ctrl+B d detaches, Ctrl+Space opens the command palette.
#[derive(Debug, Parser)]
#[command(name = "termos", version, long_version = LONG_VERSION)]
struct Cli {
    /// Use this config file instead of ~/.config/termos/config.toml.
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,
    /// Server name: servers with different names are independent.
    #[arg(short = 'L', value_name = "NAME", global = true)]
    socket_name: Option<String>,
    /// Server socket path (overrides -L).
    #[arg(short = 'S', value_name = "PATH", global = true)]
    socket_path: Option<PathBuf>,
    /// Attach even inside another TermOS pane (nesting).
    #[arg(long, global = true)]
    force: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Create a session and attach to it.
    #[command(alias = "new-session")]
    New {
        /// Session name (default: the first free number).
        #[arg(short = 's', value_name = "NAME")]
        name: Option<String>,
        /// Create it without attaching.
        #[arg(short = 'd')]
        detached: bool,
    },
    /// Attach to a session (default: the most recently used one).
    #[command(alias = "a", alias = "attach-session")]
    Attach {
        /// Session name or id ($N).
        #[arg(short = 't', value_name = "SESSION")]
        target: Option<String>,
    },
    /// Detach the clients of a session (default: this one, inside TermOS).
    #[command(alias = "detach-client")]
    Detach {
        #[arg(short = 's', value_name = "SESSION")]
        session: Option<String>,
    },
    /// List sessions, and saved sessions that can be restored.
    #[command(alias = "list-sessions")]
    Ls,
    /// End a session and every program in it (default: this one).
    KillSession {
        #[arg(short = 't', value_name = "SESSION")]
        target: Option<String>,
    },
    /// Stop the server. Its sessions are saved and can be restored.
    KillServer,
    /// Call a control-API method, e.g. `termos msg pane.split-right` or
    /// `termos msg notify.send '{"text": "hi"}'`. Prints the result as JSON.
    Msg {
        method: String,
        /// Parameters as a JSON object.
        params: Option<String>,
    },
    /// Restore saved sessions (all, or those named).
    Restore { names: Vec<String> },
    /// Run the server in the foreground (clients start it as a daemon).
    Server,
    /// Inspect the configuration.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Diagnostics.
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        what: DebugCmd,
    },
}

#[derive(Debug, Subcommand)]
enum DebugCmd {
    /// Ask this terminal what it can do and print what TermOS detects.
    Caps {
        /// Also print the terminal's raw replies (for test fixtures).
        #[arg(long)]
        raw: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCmd {
    /// Validate a config file (default: the active one) and report problems
    /// with their line and column.
    Check {
        /// File to check.
        path: Option<PathBuf>,
    },
    /// Print a commented config file with every default.
    Default,
    /// Print where the config file is read from.
    Path,
}

/// Which server a command talks to.
#[derive(Debug, Clone)]
struct Target {
    socket: PathBuf,
    /// The server's name (its snapshot directory).
    name: String,
    /// The session of the pane we run in, when it belongs to this server.
    session: Option<String>,
}

/// `$TERMOS` inside a pane: `<socket>,<server pid>,$<session id>`.
fn inside_termos() -> Option<(PathBuf, String)> {
    let value = std::env::var("TERMOS").ok()?;
    let mut parts = value.rsplitn(3, ',');
    let session = parts.next()?;
    let _pid = parts.next()?;
    let socket = parts.next()?;
    session
        .starts_with('$')
        .then(|| (PathBuf::from(socket), session.to_owned()))
}

fn resolve_target(cli: &Cli, paths: &Paths) -> anyhow::Result<Target> {
    let inside = inside_termos();
    let name_of = |p: &Path| server_name(p).unwrap_or_else(|| DEFAULT_SERVER.to_owned());
    let (socket, name) = if let Some(path) = &cli.socket_path {
        if cfg!(windows) && !path.to_string_lossy().starts_with(r"\\.\pipe\") {
            bail!(r"on Windows -S takes a pipe name, like \\.\pipe\termos-work");
        }
        (path.clone(), name_of(path))
    } else if let Some(name) = &cli.socket_name {
        if name.is_empty() || name.contains(['/', '\\']) {
            bail!("-L takes a name, not a path (use -S for a path)");
        }
        (paths.socket_path(name), name.clone())
    } else if let Some((socket, _)) = &inside {
        (socket.clone(), name_of(socket))
    } else {
        (paths.socket_path(DEFAULT_SERVER), DEFAULT_SERVER.to_owned())
    };
    let session = inside.filter(|(s, _)| *s == socket).map(|(_, id)| id);
    Ok(Target {
        socket,
        name,
        session,
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let paths = Paths::detect();
    let config_path = cli.config.clone().unwrap_or_else(|| paths.config_file());
    let result = match &cli.command {
        Some(Cmd::Config { action }) => config_command(action, &config_path),
        Some(Cmd::Debug {
            what: DebugCmd::Caps { raw },
        }) => debug_caps(*raw),
        _ => resolve_target(&cli, &paths).and_then(|t| run(&cli, &paths, &config_path, &t)),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("termos: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli, paths: &Paths, config_path: &Path, t: &Target) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir()
        .ok()
        .map(|d| d.display().to_string());
    match &cli.command {
        None => {
            let mut hello = Hello::new(ConnKind::Frontend);
            hello.session = Some("main".into());
            hello.create = true;
            hello.cwd = cwd;
            attach(cli, paths, config_path, t, hello, true)
        }
        Some(Cmd::New { name, detached }) => {
            if !detached {
                attach_checks(cli)?;
            }
            ensure_server(paths, t, config_path)?;
            let created = call(t, "session.new", Some(json!({"name": name, "cwd": cwd})))?;
            if *detached {
                return Ok(ExitCode::SUCCESS);
            }
            let mut hello = Hello::new(ConnKind::Frontend);
            hello.session = created["name"].as_str().map(str::to_owned);
            attach(cli, paths, config_path, t, hello, false)
        }
        Some(Cmd::Attach { target }) => {
            let mut hello = Hello::new(ConnKind::Frontend);
            hello.session.clone_from(target);
            attach(cli, paths, config_path, t, hello, false)
        }
        Some(Cmd::Detach { session }) => {
            let session = session.clone().or_else(|| t.session.clone()).context(
                "not inside TermOS: say which session to detach with -s NAME (`termos ls` lists them)",
            )?;
            call(t, "client.detach", Some(json!({"session": session})))?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Cmd::Ls) => list(t),
        Some(Cmd::KillSession { target }) => {
            let session = target.clone().or_else(|| t.session.clone()).context(
                "not inside TermOS: say which session to kill with -t NAME (`termos ls` lists them)",
            )?;
            call(t, "session.kill", Some(json!({"session": session})))?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Cmd::KillServer) => {
            call(t, "server.kill", None)?;
            wait_gone(&t.socket);
            Ok(ExitCode::SUCCESS)
        }
        Some(Cmd::Msg { method, params }) => {
            let mut params: Option<Value> = params
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .context("the parameters must be JSON, e.g. '{\"text\": \"hi\"}'")?;
            // Inside a pane, methods act on that pane's session by default.
            if let Some(session) = &t.session {
                match &mut params {
                    None => params = Some(json!({"session": session})),
                    Some(Value::Object(m)) if !m.contains_key("session") => {
                        m.insert("session".into(), Value::from(session.as_str()));
                    }
                    _ => {}
                }
            }
            let result = call(t, method, params)?;
            if !result.is_null() {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Cmd::Restore { names }) => {
            let started = ensure_server(paths, t, config_path)?;
            let params = (!names.is_empty()).then(|| json!({"names": names}));
            match call(t, "session.restore", params) {
                Ok(r) => {
                    let restored: Vec<&str> = r["restored"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    println!("restored: {}", restored.join(", "));
                    for f in r["failed"].as_array().into_iter().flatten() {
                        eprintln!("termos: could not restore {}", f.as_str().unwrap_or("?"));
                    }
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    if started {
                        // Nothing to restore: do not leave an empty server.
                        let _ = call(t, "server.kill", None);
                    }
                    Err(e)
                }
            }
        }
        Some(Cmd::Server) => run_server(paths, config_path, t),
        Some(Cmd::Config { .. } | Cmd::Debug { .. }) => Ok(ExitCode::SUCCESS),
    }
}

/// `termos debug caps`: what the attach-time probe finds in this terminal.
fn debug_caps(raw: bool) -> anyhow::Result<ExitCode> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("stdin and stdout must be a terminal");
    }
    let env = |k: &str| std::env::var(k).ok();
    let mut caps = Caps::from_env(env);
    let mut stdout = std::io::stdout();
    crossterm::terminal::enable_raw_mode().context("cannot set up the terminal")?;
    // A generous timeout: this is for finding out, not for speed.
    let probed = probe::run(&mut stdout, Duration::from_secs(2));
    let _ = crossterm::terminal::disable_raw_mode();
    let (replies, bytes) = probed.context("probing the terminal failed")?;
    caps.apply_replies(&replies);
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        caps.derive_cell_px(cols, rows);
    }
    if !probe::enabled(env) {
        println!("note: the probe is off here (TERMOS_PROBE=0, or Windows); environment only");
    } else if !replies.complete() {
        println!("note: no DA1 reply within 2 s; the replies below may be incomplete");
    }
    println!("image protocol: {}", caps.image_protocol());
    println!("{}", serde_json::to_string_pretty(&caps)?);
    if raw {
        println!("raw replies: {:?}", String::from_utf8_lossy(&bytes));
    }
    Ok(ExitCode::SUCCESS)
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")
}

fn explain(t: &Target, e: ConnectError) -> anyhow::Error {
    match e {
        ConnectError::NoServer(_) => {
            anyhow::anyhow!("no server running on {}", t.socket.display())
        }
        other => anyhow::anyhow!("{other}"),
    }
}

/// One control call to the server.
fn call(t: &Target, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
    runtime()?
        .block_on(control_call(&t.socket, method, params))
        .map_err(|e| explain(t, e))
}

/// Starts the server daemon unless one is running. Returns whether it
/// started one.
fn ensure_server(paths: &Paths, t: &Target, config_path: &Path) -> anyhow::Result<bool> {
    if termos_core::local::probe(&t.socket) {
        return Ok(false);
    }
    let exe = std::env::current_exe().context("cannot find the termos executable")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("server")
        .arg("-S")
        .arg(&t.socket)
        .arg("--config")
        .arg(config_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = spawn_daemon(&mut cmd).context("cannot start the server")?;
    // Startup takes milliseconds; this loop only runs until then.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if termos_core::local::probe(&t.socket) {
            // Reap it if it exits before we do.
            std::thread::spawn(move || child.wait());
            return Ok(true);
        }
        if let Some(status) = child.try_wait()? {
            // Another client may have started a server at the same moment.
            if termos_core::local::probe(&t.socket) {
                return Ok(false);
            }
            bail!(
                "the server did not start ({status}); see the server log in {}",
                paths.log_dir().display()
            );
        }
        if Instant::now() > deadline {
            bail!("the server did not start within 10 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Starts the server so it outlives this terminal. On Unix the server
/// calls `setsid` itself.
#[cfg(not(windows))]
fn spawn_daemon(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    cmd.spawn()
}

/// Starts the server so it outlives this terminal: with a hidden console
/// of its own (no window pops up, and its plugins inherit the hidden
/// console), in its own process group (Ctrl+C here never reaches it), and
/// outside our job object if allowed (SSH and some IDE terminals kill
/// their job's processes when the session ends).
#[cfg(windows)]
fn spawn_daemon(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    keep_std_handles_to_ourselves();
    match cmd
        .creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
    {
        // The job does not allow breaking away: start inside it.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            cmd.creation_flags(flags).spawn()
        }
        other => other,
    }
}

/// Stops the daemon from inheriting our standard handles. A Windows child
/// inherits every inheritable handle, whatever its own stdio is; a server
/// holding the write end of a pipe our caller reads (`$out = termos
/// restore`) would keep that caller waiting for end of file for as long as
/// the server runs. We start no other children, so nothing else needs them
/// to be inheritable.
#[cfg(windows)]
#[allow(unsafe_code)]
fn keep_std_handles_to_ourselves() {
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle has no preconditions; it returns null or
        // INVALID_HANDLE_VALUE when there is no such handle (checked below).
        let handle = unsafe { GetStdHandle(which) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: `handle` is one of our own standard handles, valid for
        // the life of the process; this only clears its inherit flag. A
        // failure leaves the flag as it was, which is no worse than before.
        unsafe {
            SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

/// Waits briefly for a stopping server to remove its socket.
fn wait_gone(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while termos_core::local::exists(socket) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Refuses to attach where a client cannot work.
fn attach_checks(cli: &Cli) -> anyhow::Result<()> {
    if let Ok(value) = std::env::var("TERMOS")
        && !cli.force
    {
        let pid = value.split(',').nth(1).unwrap_or(&value).to_owned();
        bail!(
            "already running inside TermOS (server pid {pid}); nesting is usually a mistake, use --force to do it anyway"
        );
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("stdin and stdout must be a terminal (run `termos --help` for other commands)");
    }
    // The client writes escape sequences directly; this also turns on VT
    // processing for the console.
    #[cfg(windows)]
    if !crossterm::ansi_support::supports_ansi() {
        bail!(
            "this console does not understand terminal escape sequences; use Windows Terminal (Windows 10 1809 or later)"
        );
    }
    Ok(())
}

/// Connects this terminal to a session and runs the client until it is
/// detached or the session ends.
fn attach(
    cli: &Cli,
    paths: &Paths,
    config_path: &Path,
    t: &Target,
    hello: Hello,
    start_server: bool,
) -> anyhow::Result<ExitCode> {
    attach_checks(cli)?;
    if start_server {
        ensure_server(paths, t, config_path)?;
    }
    if let Err(e) = termos_core::log::init_file_logging(&paths.log_dir(), "termos") {
        eprintln!("termos: logging disabled: {e}");
    }
    let runtime = runtime()?;
    let (_, end) = runtime
        .block_on(connect_frontend(&t.socket, &hello))
        .map_err(|e| explain(t, e))?;
    let env = |k: &str| std::env::var(k).ok();
    let mut caps = Caps::from_env(env);
    let probe = probe::enabled(env).then(|| probe::timeout(caps.remote));

    let mut stdout = std::io::stdout();
    let (guard, replies) =
        client::TerminalGuard::enter(&mut stdout, probe).context("cannot set up the terminal")?;
    caps.apply_replies(&replies);
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        caps.derive_cell_px(cols, rows);
    }
    tracing::info!(
        "terminal: {:?}, images: {}, {:?}",
        caps.terminal,
        caps.image_protocol(),
        caps
    );
    let typeahead = probe::typeahead(&replies);
    let kitty = guard.kitty_keyboard();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        client::restore_terminal(kitty);
        default_hook(info);
    }));
    let ended = {
        let mut out = stdout.lock();
        runtime.block_on(client::run(end, caps, typeahead, &mut out))
    };
    drop(guard);
    let mut stdout = std::io::stdout();
    match ended.context("terminal I/O failed")? {
        client::Ended::Detached(session) => {
            let _ = writeln!(stdout, "[detached (from session {session})]");
        }
        client::Ended::Exited(reason) => {
            let _ = writeln!(stdout, "[exited: {reason}]");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `termos ls`.
fn list(t: &Target) -> anyhow::Result<ExitCode> {
    let rt = runtime()?;
    let sessions = match rt.block_on(control_call(&t.socket, "session.list", None)) {
        Ok(v) => v,
        Err(ConnectError::NoServer(_)) => {
            println!("no server running on {}", t.socket.display());
            return Ok(ExitCode::FAILURE);
        }
        Err(e) => return Err(explain(t, e)),
    };
    for s in sessions.as_array().into_iter().flatten() {
        let n = |k: &str| s[k].as_u64().unwrap_or(0);
        let attached = match n("clients") {
            0 => String::new(),
            1 => " (attached)".into(),
            c => format!(" ({c} clients)"),
        };
        println!(
            "{}: {} workspaces, {} panes (created {}){attached}",
            s["name"].as_str().unwrap_or("?"),
            n("workspaces"),
            n("panes"),
            describe_time(n("created")),
        );
    }
    let saved = rt
        .block_on(control_call(&t.socket, "session.saved", None))
        .map_err(|e| explain(t, e))?;
    for s in saved.as_array().into_iter().flatten() {
        println!(
            "{}: saved at {}, not running (`termos restore`)",
            s["name"].as_str().unwrap_or("?"),
            describe_time(s["saved_at"].as_u64().unwrap_or(0)),
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `termos server`: the daemon (or a foreground server for debugging).
fn run_server(paths: &Paths, config_path: &Path, t: &Target) -> anyhow::Result<ExitCode> {
    // No controlling terminal: closing the terminal that started us must
    // not stop the sessions. Fails only if we already lead a session. (On
    // Windows the client starts us without a console window instead.)
    #[cfg(unix)]
    let _ = rustix::process::setsid();
    if let Err(e) = termos_core::log::init_file_logging(&paths.log_dir(), "server") {
        eprintln!("termos: logging disabled: {e}");
    }
    let (config, config_error) = match Config::load(config_path) {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e.to_string())),
    };
    let runtime = runtime()?;
    let _enter = runtime.enter();
    let mut opts = ServerOptions::new(&t.name, paths.clone(), config_path.to_owned());
    opts.socket_path.clone_from(&t.socket);
    opts.handle_signals = true;
    let mut server = Server::new(opts, config);
    if let Some(e) = config_error {
        server.notice(Level::Error, format!("using default settings: {e}"));
    }
    // Our stderr goes nowhere when a client started us: say it in the log
    // the client points at, too.
    let listener = server
        .bind()
        .context("cannot listen on the session socket")
        .inspect_err(|e| tracing::error!("{e:#}"))?;
    let (reload_tx, reload_rx) = mpsc::unbounded_channel();
    let watcher = match ConfigWatcher::spawn(config_path, reload_tx) {
        Ok(w) => {
            server.set_reload(reload_rx);
            Some(w)
        }
        Err(e) => {
            server.notice(
                Level::Warn,
                format!("config changes will not be picked up automatically: {e}"),
            );
            None
        }
    };
    tracing::info!(
        "server {} listening on {} (pid {})",
        t.name,
        t.socket.display(),
        std::process::id()
    );
    let reason = runtime.block_on(server.run(Some(listener)));
    tracing::info!("server stopped: {reason}");
    drop(watcher);
    Ok(ExitCode::SUCCESS)
}

fn config_command(action: &ConfigCmd, active: &Path) -> anyhow::Result<ExitCode> {
    match action {
        ConfigCmd::Default => {
            print!("{DEFAULT_CONFIG}");
            Ok(ExitCode::SUCCESS)
        }
        ConfigCmd::Path => {
            println!("{}", active.display());
            Ok(ExitCode::SUCCESS)
        }
        ConfigCmd::Check { path } => {
            let path = path.as_deref().unwrap_or(active);
            if !path.exists() {
                println!("{}: not found; the defaults apply", path.display());
                return Ok(ExitCode::SUCCESS);
            }
            match Config::load(path) {
                Err(e) => {
                    println!("{e}");
                    Ok(ExitCode::FAILURE)
                }
                Ok(config) => {
                    let (_, errors) = KeyTables::from_config(&config);
                    if errors.is_empty() {
                        println!("{}: ok", path.display());
                        Ok(ExitCode::SUCCESS)
                    } else {
                        for e in errors {
                            println!("{}: {e}", path.display());
                        }
                        Ok(ExitCode::FAILURE)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_parses() {
        <Cli as clap::CommandFactory>::command().debug_assert();
        let cli = Cli::parse_from(["termos", "-L", "work", "new", "-s", "web", "-d"]);
        assert_eq!(cli.socket_name.as_deref(), Some("work"));
        assert!(matches!(
            cli.command,
            Some(Cmd::New { ref name, detached: true }) if name.as_deref() == Some("web")
        ));
        let cli = Cli::parse_from(["termos", "a", "-t", "main"]);
        assert!(
            matches!(cli.command, Some(Cmd::Attach { ref target }) if target.as_deref() == Some("main"))
        );
        let cli = Cli::parse_from(["termos", "msg", "notify.send", "{\"text\":\"hi\"}"]);
        assert!(matches!(cli.command, Some(Cmd::Msg { ref params, .. }) if params.is_some()));
    }

    #[test]
    fn targets() {
        let paths = Paths {
            config_dir: "/c".into(),
            state_dir: "/s".into(),
            runtime_dir: "/r".into(),
        };
        let t =
            resolve_target(&Cli::parse_from(["termos", "-L", "x", "ls"]), &paths).expect("target");
        assert_eq!(t.socket, paths.socket_path("x"));
        assert_eq!(t.name, "x");
        let explicit = if cfg!(windows) {
            r"\\.\pipe\termos-0123456789abcdef-a"
        } else {
            "/tmp/a.sock"
        };
        let t = resolve_target(&Cli::parse_from(["termos", "-S", explicit, "ls"]), &paths)
            .expect("target");
        assert_eq!((t.socket.to_str(), t.name.as_str()), (Some(explicit), "a"));
        for bad in ["a/b", r"a\b"] {
            assert!(resolve_target(&Cli::parse_from(["termos", "-L", bad, "ls"]), &paths).is_err());
        }
        if cfg!(windows) {
            // -S must name a pipe there.
            let cli = Cli::parse_from(["termos", "-S", "/tmp/a.sock", "ls"]);
            assert!(resolve_target(&cli, &paths).is_err());
        }
    }
}
