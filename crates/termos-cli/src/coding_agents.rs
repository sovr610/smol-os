// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos agent install|update|remove|status`: Claude Code and Codex CLI,
//! the vendors' own programs, checked against their published SHA-256
//! (M10d, D-094; `termos_devkit::cli_agents`).

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use termos_core::config::Config;
use termos_core::paths::Paths;
use termos_devkit::cli_agents::{self as ca, Curl, Platform, Tool};

/// `claude` or `claude@2.1.90`.
fn parse(spec: &str) -> anyhow::Result<(Tool, Option<&str>)> {
    let (name, version) = match spec.split_once('@') {
        Some((n, v)) => (n, Some(v)),
        None => (spec, None),
    };
    match Tool::from_name(name) {
        Some(t) => Ok((t, version)),
        None => {
            bail!("TermOS installs `claude` (Claude Code) and `codex` (Codex CLI), not {name:?}")
        }
    }
}

fn say(line: &str) {
    println!("termos: {line}");
}

/// `termos agent install NAME[@VERSION]`.
pub(crate) fn install(paths: &Paths, config: &Config, spec: &str) -> anyhow::Result<ExitCode> {
    let (tool, version) = parse(spec)?;
    install_tool(paths, config, tool, version)?;
    let program = tool.name();
    println!();
    println!(
        "{} is ready. Start it with `{program}` in any TermOS window, or from the menu (AI agents → {}); it asks you to sign in the first time.",
        tool.title(),
        tool.title()
    );
    Ok(ExitCode::SUCCESS)
}

/// Installs `tool` (unless that version is there) and, the first time,
/// registers TermOS's MCP server with it (`[agents.termos-mcp] register`).
pub(crate) fn install_tool(
    paths: &Paths,
    config: &Config,
    tool: Tool,
    version: Option<&str>,
) -> anyhow::Result<ca::Installed> {
    let platform = Platform::here();
    let release = ca::release(&Curl, tool, platform, version)?;
    let had = ca::installed(&paths.data_dir, tool);
    if let Some(have) = &had
        && have.version == release.version
    {
        say(&format!("{} {} is installed", tool.title(), have.version));
        return Ok(have.clone());
    }
    let got = ca::install(&Curl, &paths.data_dir, &release, &mut |m| say(m))?;
    say(&format!(
        "installed {} {} in {}",
        tool.title(),
        got.version,
        got.path.display()
    ));
    if had.is_none() && config.agents.termos_mcp.register {
        match register_tool(tool, &got.path) {
            Ok(how) => say(&how),
            // The install stands; the server can be added later.
            Err(e) => eprintln!(
                "termos: could not register TermOS's MCP server with {}: {e:#} (try `termos mcp register {}`)",
                tool.title(),
                tool.name()
            ),
        }
    }
    Ok(got)
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// The program to register with: this one, by its real path.
fn termos_exe() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot find the termos program")?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

/// Registers `termos mcp serve` with `tool` (run as `program`).
fn register_tool(tool: Tool, program: &Path) -> anyhow::Result<String> {
    let termos = termos_exe()?;
    match ca::mcp_register(tool, program, &termos) {
        Some(lines) => {
            for (i, argv) in lines.iter().enumerate() {
                let out = std::process::Command::new(&argv[0])
                    .args(&argv[1..])
                    .stdin(std::process::Stdio::null())
                    .output()
                    .with_context(|| format!("cannot run {}", argv[0]))?;
                // The first line removes an older entry, which may not
                // exist.
                if i + 1 == lines.len() && !out.status.success() {
                    bail!(
                        "`{} mcp add` failed: {}",
                        tool.name(),
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
            }
            Ok(format!(
                "registered TermOS's MCP server with {} (`{} mcp list` shows it)",
                tool.title(),
                tool.name()
            ))
        }
        None => {
            let dir =
                ca::codex_home(&env, home().as_deref()).context("cannot find your home folder")?;
            let file = dir.join("config.toml");
            let before = match std::fs::read_to_string(&file) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e).with_context(|| format!("cannot read {}", file.display())),
            };
            let after = ca::codex_config(&before, &termos)?;
            std::fs::create_dir_all(&dir)?;
            write_replacing(&file, &after)?;
            Ok(format!(
                "registered TermOS's MCP server with {} (in {})",
                tool.title(),
                file.display()
            ))
        }
    }
}

/// Writes `file` through a temporary file beside it, keeping its
/// permissions.
fn write_replacing(file: &Path, text: &str) -> anyhow::Result<()> {
    let tmp = file.with_extension(format!("toml.termos-{}", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(file) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, file).with_context(|| format!("cannot replace {}", file.display()))?;
    Ok(())
}

/// `tool`'s program: TermOS's install, else the one on `PATH`.
fn program_of(paths: &Paths, tool: Tool) -> Option<PathBuf> {
    ca::installed(&paths.data_dir, tool)
        .map(|i| i.path)
        .or_else(|| termos_devkit::mise::which(tool.name(), std::env::var_os("PATH").as_ref()))
}

/// The tools named, or both.
fn tools_named(name: Option<&str>) -> anyhow::Result<Vec<Tool>> {
    Ok(match name {
        Some(n) => vec![parse(n)?.0],
        None => Tool::ALL.to_vec(),
    })
}

/// `termos mcp register [NAME]`.
pub(crate) fn register(paths: &Paths, name: Option<&str>) -> anyhow::Result<ExitCode> {
    let mut failed = false;
    for tool in tools_named(name)? {
        let Some(program) = program_of(paths, tool) else {
            if name.is_some() {
                eprintln!(
                    "termos: {} is not installed (`termos agent install {}`)",
                    tool.title(),
                    tool.name()
                );
                failed = true;
            }
            continue;
        };
        match register_tool(tool, &program) {
            Ok(how) => say(&how),
            Err(e) => {
                eprintln!("termos: {}: {e:#}", tool.title());
                failed = true;
            }
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `termos mcp unregister [NAME]`.
pub(crate) fn unregister(paths: &Paths, name: Option<&str>) -> anyhow::Result<ExitCode> {
    for tool in tools_named(name)? {
        match tool {
            Tool::Claude => {
                let Some(program) = program_of(paths, tool) else {
                    continue;
                };
                let ok = std::process::Command::new(&program)
                    .args(["mcp", "remove", "--scope", "user", "termos"])
                    .stdin(std::process::Stdio::null())
                    .output()
                    .is_ok_and(|o| o.status.success());
                if ok {
                    say("removed TermOS's MCP server from Claude Code");
                }
            }
            Tool::Codex => {
                let Some(dir) = ca::codex_home(&env, home().as_deref()) else {
                    continue;
                };
                let file = dir.join("config.toml");
                let Ok(before) = std::fs::read_to_string(&file) else {
                    continue;
                };
                if ca::registered(tool, &before) {
                    write_replacing(&file, &ca::codex_config_without(&before)?)?;
                    say(&format!(
                        "removed TermOS's MCP server from Codex CLI ({})",
                        file.display()
                    ));
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Whether TermOS's MCP server is in `tool`'s settings.
fn is_registered(tool: Tool) -> Option<bool> {
    let file = match tool {
        Tool::Claude => ca::claude_settings(&env, home().as_deref())?,
        Tool::Codex => ca::codex_home(&env, home().as_deref())?.join("config.toml"),
    };
    let text = std::fs::read_to_string(file).ok()?;
    Some(ca::registered(tool, &text))
}

/// `termos mcp serve`: TermOS's MCP server on stdin and stdout, for the
/// agent CLI that started it.
pub(crate) fn serve(paths: &Paths, config_path: &Path) -> anyhow::Result<ExitCode> {
    use termos_ai::mcp_server::{Context, Server, Socket};
    let _ = termos_core::log::init_file_logging(&paths.log_dir(), "mcp");
    // A config that does not load is not ignored: its rules may deny
    // tools. The agent shows this message.
    let config = Config::load(config_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let inside = crate::inside_termos();
    let socket = inside.as_ref().map_or_else(
        || paths.socket_path(crate::DEFAULT_SERVER),
        |(s, _)| s.clone(),
    );
    let context = Context {
        pane: crate::tos::pane_id(),
        session: inside.map(|(_, s)| s),
        cwd: std::env::current_dir()?,
    };
    let journal = std::sync::Arc::new(termos_core::journal::Journal::new(
        &paths.state_dir,
        config.center.keep_days,
    ));
    let mut server = Server::new(
        Box::new(Socket::new(socket)?),
        context,
        config.agents.termos_mcp,
        Some(&paths.state_dir),
        Some(journal),
    );
    tracing::info!("MCP connection {} started", server.id());
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    server.serve(&mut input, &mut output)?;
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn update(
    paths: &Paths,
    config: &Config,
    name: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let tools: Vec<Tool> = match name {
        Some(n) => vec![parse(n)?.0],
        None => Tool::ALL
            .into_iter()
            .filter(|t| ca::installed(&paths.data_dir, *t).is_some())
            .collect(),
    };
    if tools.is_empty() {
        println!(
            "TermOS has not installed Claude Code or Codex CLI (`termos agent install claude`)"
        );
        return Ok(ExitCode::SUCCESS);
    }
    let mut failed = false;
    for tool in tools {
        let before = ca::installed(&paths.data_dir, tool).map(|i| i.version);
        match install_tool(paths, config, tool, None) {
            Ok(now) if before.as_deref() == Some(now.version.as_str()) => {}
            Ok(now) => say(&format!(
                "{}: {} → {}",
                tool.title(),
                before.as_deref().unwrap_or("not installed"),
                now.version
            )),
            Err(e) => {
                eprintln!("termos: {}: {e}", tool.title());
                failed = true;
            }
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

pub(crate) fn remove(paths: &Paths, name: &str) -> anyhow::Result<ExitCode> {
    let (tool, _) = parse(name)?;
    if ca::remove(&paths.data_dir, tool)? {
        say(&format!(
            "removed {} (your settings and sign-in stay in your home folder)",
            tool.title()
        ));
    } else {
        say(&format!("TermOS has not installed {}", tool.title()));
    }
    Ok(ExitCode::SUCCESS)
}

/// What runs as `name` outside TermOS's own folder.
fn elsewhere(paths: &Paths, tool: Tool) -> Option<PathBuf> {
    let ours = ca::bin_dir(&paths.data_dir);
    let path = std::env::var_os("PATH")?;
    let rest = std::env::join_paths(std::env::split_paths(&path).filter(|d| *d != ours)).ok()?;
    termos_devkit::mise::which(tool.name(), Some(&rest))
}

pub(crate) fn status(paths: &Paths) -> anyhow::Result<ExitCode> {
    let platform = Platform::here();
    for tool in Tool::ALL {
        let line = match ca::installed(&paths.data_dir, tool) {
            Some(i) => format!("{}  ({})", i.version, i.path.display()),
            None if tool_supported(tool, platform) => "not installed by TermOS".to_owned(),
            None => "not offered for this computer".to_owned(),
        };
        println!("{:<12} {line}", tool.title());
        if let Some(other) = elsewhere(paths, tool) {
            println!("{:<12} also on PATH: {}", "", other.display());
        }
        match is_registered(tool) {
            Some(true) => println!("{:<12} TermOS's MCP server: registered", ""),
            Some(false) => println!(
                "{:<12} TermOS's MCP server: not registered (`termos mcp register {}`)",
                "",
                tool.name()
            ),
            None => {}
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn tool_supported(tool: Tool, p: Platform) -> bool {
    match tool {
        Tool::Claude => p.claude().is_some(),
        Tool::Codex => p.codex().is_some(),
    }
}

/// The program to run for an external agent preset whose program is
/// `program`: TermOS's own install, else the one on `PATH`. When neither
/// exists and TermOS can install it, it asks (in the agent's window).
pub(crate) fn resolve_external(
    paths: &Paths,
    config: &Config,
    program: &str,
) -> Option<(PathBuf, Vec<(String, String)>)> {
    let tool = Tool::from_name(program)?;
    if let Some(i) = ca::installed(&paths.data_dir, tool) {
        return Some((i.path, extra_env(tool)));
    }
    if termos_devkit::mise::which(program, std::env::var_os("PATH").as_ref()).is_some() {
        return None;
    }
    if !std::io::stdin().is_terminal() || !tool_supported(tool, Platform::here()) {
        return None;
    }
    print!(
        "{} is not installed. Install it now (the official program from {}, checked against its SHA-256)? [Y/n] ",
        tool.title(),
        match tool {
            Tool::Claude => "Anthropic",
            Tool::Codex => "OpenAI's GitHub releases",
        }
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).ok()?;
    if answer.trim().to_ascii_lowercase().starts_with('n') {
        return None;
    }
    match install_tool(paths, config, tool, None) {
        Ok(i) => Some((i.path, extra_env(tool))),
        Err(e) => {
            eprintln!("termos: {e}");
            None
        }
    }
}

/// The environment TermOS's copy of `tool` runs with.
fn extra_env(tool: Tool) -> Vec<(String, String)> {
    match tool {
        // TermOS updates it (`termos agent update`), not Claude Code itself.
        Tool::Claude => vec![("DISABLE_AUTOUPDATER".to_owned(), "1".to_owned())],
        Tool::Codex => Vec::new(),
    }
}

/// The pane environment for TermOS-installed agents: Claude Code's own
/// updater off, as TermOS updates it.
pub(crate) fn pane_env(data_dir: &Path) -> Vec<(String, String)> {
    if ca::installed(data_dir, Tool::Claude).is_some() {
        extra_env(Tool::Claude)
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse() {
        assert_eq!(parse("claude").expect("ok"), (Tool::Claude, None));
        assert_eq!(
            parse("codex@0.46.0").expect("ok"),
            (Tool::Codex, Some("0.46.0"))
        );
        assert!(parse("gemini").is_err());
    }
}
