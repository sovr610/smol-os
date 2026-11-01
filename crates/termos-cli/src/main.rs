// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos`: the TermOS command-line entry point.
//!
//! `termos` with no subcommand starts the hosted desktop in the current
//! terminal: the desktop server and the terminal client run in one process,
//! connected by the in-process transport (DECISIONS.md D-007). Detaching and
//! reattaching arrive with the socket transport in M3.

mod client;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use termos_core::config::{Config, DEFAULT_CONFIG};
use termos_core::paths::Paths;
use termos_core::watch::ConfigWatcher;
use termos_core::wire::{ConnKind, Hello};
use termos_render::caps::Caps;
use termos_shell::keys::KeyTables;
use termos_shell::notify::Level;
use termos_shell::server::{Server, ServerOptions};
use tokio::sync::mpsc;

/// Long version string, e.g. `0.0.0 (x86_64-unknown-linux-gnu)`.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("TERMOS_BUILD_TARGET"),
    ")"
);

/// TermOS: a terminal-first desktop environment.
///
/// Run without a subcommand to start the desktop in this terminal. The
/// prefix key is Ctrl+B (like tmux); Ctrl+Space opens the command palette.
#[derive(Debug, Parser)]
#[command(name = "termos", version, long_version = LONG_VERSION)]
struct Cli {
    /// Use this config file instead of ~/.config/termos/config.toml.
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,
    /// Start even inside another TermOS pane (nesting).
    #[arg(long)]
    force: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Inspect the configuration.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let paths = Paths::detect();
    let config_path = cli.config.clone().unwrap_or_else(|| paths.config_file());
    let result = match cli.command {
        Some(Cmd::Config { action }) => config_command(action, &config_path),
        None => run_desktop(cli.force, &paths, &config_path),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("termos: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn config_command(action: ConfigCmd, active: &Path) -> anyhow::Result<ExitCode> {
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

fn run_desktop(force: bool, paths: &Paths, config_path: &Path) -> anyhow::Result<ExitCode> {
    if let Ok(pid) = std::env::var("TERMOS")
        && !force
    {
        anyhow::bail!(
            "already running inside TermOS (server pid {pid}); nesting is usually a mistake, use --force to do it anyway"
        );
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "stdin and stdout must be a terminal (run `termos --help` for other commands)"
        );
    }
    if let Err(e) = termos_core::log::init_file_logging(&paths.log_dir()) {
        eprintln!("termos: logging disabled: {e}");
    }
    let (config, config_error) = match Config::load(config_path) {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e.to_string())),
    };
    let mut caps = Caps::from_env(|k| std::env::var(k).ok());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    let _enter = runtime.enter();

    // TODO(M3): run the server as a daemon on its socket and attach to it.
    let opts = ServerOptions::new("default", paths.clone(), config_path.to_owned());
    let mut server = Server::new(opts, config);
    if let Some(e) = config_error {
        server.notice(Level::Error, format!("using default settings: {e}"));
    }
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
    let connector = server.connector();
    let server = runtime.spawn(server.run(None));
    let mut hello = Hello::new(ConnKind::Frontend);
    hello.session = Some("main".into());
    hello.create = true;
    let (_, client_end) = runtime
        .block_on(connector.attach(hello))
        .map_err(|e| anyhow::anyhow!("{}", e.message))
        .context("cannot start the session")?;

    let mut stdout = std::io::stdout();
    let guard = client::TerminalGuard::enter(&mut stdout).context("cannot set up the terminal")?;
    caps.kitty_keyboard = guard.kitty_keyboard();
    let kitty = guard.kitty_keyboard();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        client::restore_terminal(kitty);
        default_hook(info);
    }));

    let reason = {
        let mut out = stdout.lock();
        runtime.block_on(client::run(client_end, caps, &mut out))
    };
    drop(guard);
    server.abort();
    drop(watcher);
    let reason = reason.context("terminal I/O failed")?;
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "[exited: {reason}]");
    Ok(ExitCode::SUCCESS)
}
