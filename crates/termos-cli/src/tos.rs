// SPDX-License-Identifier: GPL-3.0-or-later
//! `tos`: TermOS's command in every pane (D-084, D-085).
//!
//! The `termos` binary answers to the name `tos` too. TermOS keeps a `tos`
//! link to itself in its data folder and puts that folder first on every
//! pane's `PATH`, so the same command works in PowerShell, cmd, bash, zsh
//! and fish on Linux, macOS and Windows. Its verbs act on the pane they run
//! in (`$TERMOS_PANE`) through the control socket (`$TERMOS`); anything
//! else is a `termos` subcommand (`tos lang install go`).

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use termos_core::paths::Paths;

use crate::Target;

/// The name the binary answers to as the pane command.
pub(crate) const NAME: &str = "tos";

#[derive(Debug, Parser)]
#[command(
    name = "tos",
    version,
    about = "Use TermOS from the shell of a TermOS pane",
    after_help = "Every `termos` command works too: tos lang install go, tos task run, \
                  tos create rust-cli app, tos edit main.rs, tos git, tos ls."
)]
struct Tos {
    #[command(subcommand)]
    cmd: Option<TosCmd>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Side {
    /// Along the longer side.
    Auto,
    Right,
    Down,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Subcommand)]
enum TosCmd {
    /// Open the TermOS menu.
    Menu,
    /// Split this pane; with a command after `--`, run it in the new pane.
    Split {
        #[arg(value_enum, default_value = "auto")]
        side: Side,
        /// Print the new pane's id (for `tos read`).
        #[arg(short = 'p', long)]
        print_id: bool,
        #[arg(last = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Run a command in a new pane beside this one (`--float`: a window).
    Run {
        #[arg(long)]
        float: bool,
        /// Print the new pane's id (for `tos read`).
        #[arg(short = 'p', long)]
        print_id: bool,
        #[arg(required = true, trailing_var_arg = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Float this pane (or tile it again); with a command after `--`, open
    /// a floating window running it.
    Float {
        /// Print the new pane's id (for `tos read`).
        #[arg(short = 'p', long)]
        print_id: bool,
        #[arg(last = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// The panes of this session: id, workspace, title, folder (`*`: this
    /// one).
    Panes,
    /// Print the last lines of a pane's text: its scrollback and screen
    /// (keys and passwords it shows are masked). With --command, its last
    /// command, that command's output and its exit status.
    Read {
        /// The pane's id (`tos panes`); default: this one.
        pane: Option<u64>,
        /// How many lines, from the end.
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: usize,
        /// Only the last command: its line, output and exit status (the
        /// pane's shell marks commands, D-116).
        #[arg(short, long)]
        command: bool,
    },
    /// Zoom this pane, or unzoom.
    Zoom,
    /// Close this pane.
    Close,
    /// Name this pane; with no name, it shows its program's title again.
    Title { name: Vec<String> },
    /// Open a file by kind (pictures, markdown, folders, text in the
    /// editor), or a web address in your browser.
    Open { target: String },
    /// Browse a folder (default: this one) in the file manager.
    Files { dir: Option<PathBuf> },
    /// The system monitor.
    Monitor,
    /// The package manager: mise, the dev rootfs, the system's packages,
    /// Podman.
    Packages,
    /// The command center: AI agents and the system, live and over time.
    Center,
    /// Usage and accounts: every Claude Code and Codex account.
    Usage,
    /// The coding harness, in this folder; with a goal, it plans it.
    Harness { goal: Vec<String> },
    /// Settings.
    Settings,
    /// The clipboard history.
    Clipboard,
    /// The network window.
    Network,
    /// The updates window.
    Updates,
    /// The notification center.
    Notifications,
    /// Every key binding.
    Keys,
    /// The command palette.
    Palette,
    /// The tutorial.
    Tutorial,
    /// About TermOS: the version, where its files are, the licence.
    About,
    /// An AI agent window: TermOS's own, or Claude Code (`claude`), Codex
    /// (`codex`) and the others of `termos agent presets`.
    Ai { name: Option<String> },
    /// Copy text (the arguments, or what is piped in) to the clipboard.
    Copy { text: Vec<String> },
    /// Print the last thing copied.
    Paste,
    /// Show a message in the status line and the notification center.
    Notify {
        #[arg(long, value_enum, default_value = "info")]
        level: Level,
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Pick a theme: dark, light, high-contrast or auto.
    Theme { name: String },
    /// Pick a layout: tiled, even-horizontal, even-vertical, main-vertical
    /// or main-horizontal.
    Layout { name: String },
    /// Go to a workspace by number, or `new`, `next`, `prev`.
    Workspace { which: String },
    /// Show this pane, its session and server.
    Info,
    #[command(external_subcommand)]
    Termos(Vec<String>),
}

/// Whether this process was started as `tos` (by its link's name).
pub(crate) fn invoked_as_tos() -> bool {
    std::env::args_os()
        .next()
        .and_then(|a| {
            Path::new(&a)
                .file_stem()
                .map(|s| s.eq_ignore_ascii_case(NAME))
        })
        .unwrap_or(false)
}

/// A pane's last command as `tos read --command` prints it.
fn last_command_text(last: Option<&Value>) -> String {
    let Some(b) = last.filter(|v| !v.is_null()) else {
        return "no command found: the pane's shell does not mark its commands \
                (bash, zsh, fish and PowerShell do in TermOS)\n"
            .into();
    };
    let mut out = format!(
        "$ {}\n",
        b.get("command").and_then(Value::as_str).unwrap_or_default()
    );
    for l in b
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        out.push_str(l);
        out.push('\n');
    }
    match (
        b.get("state").and_then(Value::as_str),
        b.get("exit").and_then(Value::as_i64),
    ) {
        (Some("running"), _) => out.push_str("[still running]\n"),
        (_, Some(code)) => out.push_str(&format!("[exit status {code}]\n")),
        _ => out.push_str("[exit status unknown]\n"),
    }
    out
}

/// The pane this runs in.
pub(crate) fn pane_id() -> Option<u64> {
    std::env::var("TERMOS_PANE")
        .ok()?
        .trim_start_matches('%')
        .parse()
        .ok()
}

/// The server and session of the pane this runs in.
fn target() -> anyhow::Result<Target> {
    let Some((socket, session)) = crate::inside_termos() else {
        bail!("tos works in a TermOS pane; start TermOS with `termos` first");
    };
    let name = termos_core::paths::server_name(&socket)
        .unwrap_or_else(|| crate::DEFAULT_SERVER.to_owned());
    Ok(Target {
        socket,
        name,
        session: Some(session),
    })
}

/// Runs a desktop command in the TermOS session this runs in, on its
/// pane.
pub(crate) fn run_here(command: &str) -> anyhow::Result<()> {
    on_pane(&target()?, command)
}

/// Runs a desktop command on the pane this runs in.
fn on_pane(t: &Target, command: &str) -> anyhow::Result<()> {
    let mut params = json!({ "command": command, "session": t.session });
    if let Some(p) = pane_id() {
        params["pane"] = Value::from(p);
    }
    crate::call(t, "pane.command", Some(params))?;
    Ok(())
}

fn absolute(p: &Path) -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot read this folder")?;
    let p = cwd.join(p);
    if !p.exists() {
        bail!("{} does not exist", p.display());
    }
    Ok(p.canonicalize().unwrap_or(p))
}

fn is_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// Opens a web address with the system's opener.
fn open_url(url: &str) -> anyhow::Result<()> {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        // `start` treats the first quoted argument as a title.
        c.args(["/C", "start", "", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let status = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .context("no web browser to open it with here")?;
    if !status.success() {
        bail!("the browser did not open {url}");
    }
    Ok(())
}

/// Opens `argv` in a new pane: `where` is right, down, auto or float.
fn open_program(t: &Target, place: &str, argv: &[String], print_id: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().ok();
    let mut params = json!({
        "session": t.session,
        "where": place,
        "argv": argv,
    });
    if let Some(p) = pane_id() {
        params["pane"] = Value::from(p);
    }
    if let Some(c) = cwd {
        params["cwd"] = Value::from(c.display().to_string());
    }
    let opened = crate::call(t, "pane.open", Some(params))?;
    if print_id && let Some(id) = opened.get("pane").and_then(Value::as_u64) {
        println!("{id}");
    }
    Ok(())
}

/// The `tos` command line (its own `main`).
pub(crate) fn main() -> ExitCode {
    let tos = Tos::parse();
    match run(tos) {
        Ok(code) => code,
        Err(e) if crate::broken_pipe(&e) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tos: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(tos: Tos) -> anyhow::Result<ExitCode> {
    let Some(cmd) = tos.cmd else {
        use clap::CommandFactory;
        Tos::command().print_help()?;
        return Ok(ExitCode::SUCCESS);
    };
    if let TosCmd::Termos(args) = cmd {
        return crate::run_cli(std::iter::once("termos".to_owned()).chain(args));
    }
    let t = target()?;
    let named = |name: &str| on_pane(&t, name);
    match cmd {
        // Handled above.
        TosCmd::Termos(_) => {}
        TosCmd::Menu => named("menu")?,
        TosCmd::Split {
            side,
            print_id,
            command,
        } => {
            let place = match side {
                Side::Auto => "auto",
                Side::Right => "right",
                Side::Down => "down",
            };
            if command.is_empty() {
                named(&format!("pane.split-{place}"))?;
            } else {
                open_program(&t, place, &command, print_id)?;
            }
        }
        TosCmd::Run {
            float,
            print_id,
            command,
        } => {
            open_program(&t, if float { "float" } else { "auto" }, &command, print_id)?;
        }
        TosCmd::Float { print_id, command } => {
            if command.is_empty() {
                named("pane.float")?;
            } else {
                open_program(&t, "float", &command, print_id)?;
            }
        }
        TosCmd::Panes => {
            let panes = crate::call(&t, "pane.list", Some(json!({ "session": t.session })))?;
            let me = pane_id();
            for p in panes.as_array().into_iter().flatten() {
                let id = p.get("id").and_then(Value::as_u64).unwrap_or_default();
                let text = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                let workspace = p.get("workspace").and_then(Value::as_u64).unwrap_or(0) + 1;
                let mark = if Some(id) == me { "*" } else { " " };
                let title = Some(text("name"))
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| text("title"));
                println!(
                    "{mark}{id:<4} {workspace:<3} {:<9} {title:<30} {}",
                    text("kind"),
                    text("cwd")
                );
            }
        }
        TosCmd::Read {
            pane,
            lines,
            command,
        } => {
            let own = pane_id();
            let Some(pane) = pane.or(own) else {
                bail!("which pane? `tos panes` lists them");
            };
            let got = crate::call(
                &t,
                "pane.read",
                Some(json!({
                    "session": t.session,
                    "pane": pane,
                    "lines": lines.clamp(1, 10_000),
                    "command": command,
                    // Its own newest command is this one.
                    "finished": Some(pane) == own,
                })),
            )?;
            if command {
                let redactor = termos_ai::redact::Redactor::new();
                print!(
                    "{}",
                    redactor.text(&last_command_text(got.get("last_command")))
                );
                return Ok(ExitCode::SUCCESS);
            }
            let text: Vec<&str> = got
                .get("lines")
                .and_then(Value::as_array)
                .map(|l| l.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            // As for agents over MCP: keys a pane shows are masked.
            let masked = termos_ai::redact::Redactor::new().text(&text.join("\n"));
            if !masked.is_empty() {
                println!("{masked}");
            }
        }
        TosCmd::Zoom => named("pane.zoom")?,
        TosCmd::Close => named("pane.kill")?,
        TosCmd::Title { name } => {
            let mut params = json!({ "session": t.session, "name": name.join(" ") });
            if let Some(p) = pane_id() {
                params["pane"] = Value::from(p);
            }
            crate::call(&t, "pane.rename", Some(params))?;
        }
        TosCmd::Open { target: what } => {
            if is_url(&what) {
                open_url(&what)?;
            } else {
                let p = absolute(Path::new(&what))?;
                named(&format!("file.open {}", p.display()))?;
            }
        }
        TosCmd::Files { dir } => {
            let p = absolute(dir.as_deref().unwrap_or(Path::new(".")))?;
            named(&format!("files.open {}", p.display()))?;
        }
        TosCmd::Monitor => named("monitor.open")?,
        TosCmd::Packages => named("packages.open")?,
        TosCmd::Center => named("center.open")?,
        TosCmd::Usage => named("usage.open")?,
        TosCmd::Harness { goal } => {
            let goal = goal.join(" ");
            named(&if goal.trim().is_empty() {
                "harness.open".to_owned()
            } else {
                format!("harness.open {goal}")
            })?;
        }
        TosCmd::Settings => named("settings.open")?,
        TosCmd::Clipboard => named("clipboard.open")?,
        TosCmd::Network => named("network.open")?,
        TosCmd::Updates => named("update.open")?,
        TosCmd::Notifications => named("notifications")?,
        TosCmd::Keys => named("keys")?,
        TosCmd::Palette => named("palette")?,
        TosCmd::Tutorial => named("tutorial")?,
        TosCmd::About => named("about.open")?,
        TosCmd::Ai { name } => match name {
            None => named("agent.open")?,
            Some(n) => named(&format!("agent.external {n}"))?,
        },
        TosCmd::Copy { text } => {
            let text = if text.is_empty() {
                let mut stdin = std::io::stdin();
                if stdin.is_terminal() {
                    bail!("give the text, or pipe it in: `echo hi | tos copy`");
                }
                let mut s = String::new();
                stdin
                    .by_ref()
                    .take(16 << 20)
                    .read_to_string(&mut s)
                    .context("cannot read what was piped in")?;
                s
            } else {
                text.join(" ")
            };
            crate::call(
                &t,
                "clipboard.copy",
                Some(json!({ "session": t.session, "text": text })),
            )?;
        }
        TosCmd::Paste => {
            let v = crate::call(&t, "clipboard.get", Some(json!({ "session": t.session })))?;
            if let Some(text) = v.get("text").and_then(Value::as_str) {
                use std::io::Write;
                let mut out = std::io::stdout();
                out.write_all(text.as_bytes())?;
                out.flush()?;
            }
        }
        TosCmd::Notify { level, text } => {
            let level = match level {
                Level::Info => "info",
                Level::Warn => "warn",
                Level::Error => "error",
            };
            crate::call(
                &t,
                "notify.send",
                Some(json!({ "session": t.session, "text": text.join(" "), "level": level })),
            )?;
        }
        TosCmd::Theme { name } => named(&format!("theme.select {name}"))?,
        TosCmd::Layout { name } => named(&format!("layout.select {name}"))?,
        TosCmd::Workspace { which } => match which.as_str() {
            "new" => named("workspace.new")?,
            "next" => named("workspace.next")?,
            "prev" | "previous" => named("workspace.prev")?,
            n if n.parse::<usize>().is_ok() => named(&format!("workspace.select {n}"))?,
            other => bail!("no workspace `{other}`: give a number, new, next or prev"),
        },
        TosCmd::Info => {
            let panes = crate::call(&t, "pane.list", Some(json!({ "session": t.session })))?;
            let me = pane_id();
            println!(
                "session   {}",
                t.session.as_deref().unwrap_or("?").trim_start_matches('$')
            );
            println!("server    {} ({})", t.name, t.socket.display());
            if let Some(id) = me {
                println!("pane      %{id}");
                if let Some(p) = panes
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|p| p.get("id").and_then(Value::as_u64) == Some(id))
                {
                    for key in ["name", "title", "cwd", "workspace"] {
                        let Some(v) = p.get(key).filter(|v| !v.is_null()) else {
                            continue;
                        };
                        let v = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                        if !v.is_empty() {
                            println!("{key:<9} {v}");
                        }
                    }
                }
            }
            println!("version   {}", env!("CARGO_PKG_VERSION"));
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ----- the link ---------------------------------------------------------

/// The folder holding the `tos` link, put first on panes' `PATH`.
pub(crate) fn bin_dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join("bin")
}

/// `name`, as a program's file is named here.
pub(crate) fn file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// Makes `<data>/bin/tos` this executable (a symbolic link on Unix; a hard
/// link, else a copy, on Windows, remade when the executable changes).
/// Returns the folder, or `None` when it cannot be made (panes then have
/// no `tos`, which is reported in the log).
pub(crate) fn ensure_link(paths: &Paths) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = bin_dir(paths);
    match make_link(&exe, &dir, NAME) {
        Ok(()) => Some(dir),
        Err(e) => {
            tracing::warn!("cannot put `tos` in {}: {e}", dir.display());
            None
        }
    }
}

/// Makes `<dir>/<name>` this executable (TermOS answers to the name it
/// is started by: `tos`, and the agent CLIs' shims, D-097).
pub(crate) fn make_link(exe: &Path, dir: &Path, name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let link = dir.join(file_name(name));
    #[cfg(unix)]
    {
        let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_owned());
        if std::fs::read_link(&link).is_ok_and(|t| t == exe) {
            return Ok(());
        }
        let tmp = dir.join(format!(".{name}.{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(&exe, &tmp)?;
        std::fs::rename(&tmp, &link)
    }
    #[cfg(not(unix))]
    {
        // What the link was made from, to remake it after an update.
        let stamp_path = dir.join(format!("{name}.source"));
        let meta = std::fs::metadata(exe)?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let stamp = format!("{}\n{}\n{}\n", exe.display(), meta.len(), modified);
        let current = std::fs::read_to_string(&stamp_path).is_ok_and(|s| s == stamp)
            && std::fs::metadata(&link).is_ok_and(|m| m.len() == meta.len());
        if current {
            return Ok(());
        }
        let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        if std::fs::hard_link(exe, &tmp).is_err() {
            std::fs::copy(exe, &tmp)?;
        }
        // A running program cannot be replaced; move it aside first.
        if link.exists() {
            let old = dir.join(format!("{name}.{}.old", std::process::id()));
            if std::fs::rename(&link, &old).is_ok() {
                let _ = std::fs::remove_file(&old);
            }
        }
        std::fs::rename(&tmp, &link)?;
        std::fs::write(&stamp_path, stamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_points_at_the_executable_and_is_remade() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join(if cfg!(windows) {
            "termos.exe"
        } else {
            "termos"
        });
        std::fs::write(&exe, b"one").expect("write");
        let bin = dir.path().join("data/bin");
        make_link(&exe, &bin, NAME).expect("made");
        let link = bin.join(file_name(NAME));
        assert_eq!(std::fs::read(&link).expect("read"), b"one");
        // Unchanged: left alone. Changed: remade.
        make_link(&exe, &bin, NAME).expect("again");
        std::fs::remove_file(&exe).expect("rm");
        std::fs::write(&exe, b"two!").expect("write");
        make_link(&exe, &bin, NAME).expect("remade");
        assert_eq!(std::fs::read(&link).expect("read"), b"two!");
    }

    #[test]
    fn web_addresses_and_parsing() {
        assert!(is_url("https://example.org") && is_url("HTTP://x") && !is_url("notes.md"));
        let t = Tos::try_parse_from(["tos", "split", "right", "--", "htop", "-d", "5"])
            .expect("parses");
        assert!(matches!(
            t.cmd,
            Some(TosCmd::Split { side: Side::Right, ref command, .. }) if command == &["htop", "-d", "5"]
        ));
        let t = Tos::try_parse_from(["tos", "run", "lazygit", "--version"]).expect("parses");
        assert!(
            matches!(t.cmd, Some(TosCmd::Run { float: false, ref command, .. }) if command.len() == 2)
        );
        // Anything else is a termos subcommand.
        let t = Tos::try_parse_from(["tos", "lang", "install", "go"]).expect("parses");
        assert!(matches!(t.cmd, Some(TosCmd::Termos(ref a)) if a == &["lang", "install", "go"]));
    }
}
