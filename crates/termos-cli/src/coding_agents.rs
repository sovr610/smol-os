// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos agent install|update|remove|status`: Claude Code and Codex CLI,
//! the vendors' own programs, checked against their published SHA-256
//! (M10d, D-094; `termos_devkit::cli_agents`).

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::bail;
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
pub(crate) fn install(paths: &Paths, spec: &str) -> anyhow::Result<ExitCode> {
    let (tool, version) = parse(spec)?;
    install_tool(paths, tool, version)?;
    let program = tool.name();
    println!();
    println!(
        "{} is ready. Start it with `{program}` in any TermOS window, or from the menu (AI → {}); it asks you to sign in the first time.",
        tool.title(),
        tool.title()
    );
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn install_tool(
    paths: &Paths,
    tool: Tool,
    version: Option<&str>,
) -> anyhow::Result<ca::Installed> {
    let platform = Platform::here();
    let release = ca::release(&Curl, tool, platform, version)?;
    if let Some(have) = ca::installed(&paths.data_dir, tool)
        && have.version == release.version
    {
        say(&format!("{} {} is installed", tool.title(), have.version));
        return Ok(have);
    }
    let got = ca::install(&Curl, &paths.data_dir, &release, &mut |m| say(m))?;
    say(&format!(
        "installed {} {} in {}",
        tool.title(),
        got.version,
        got.path.display()
    ));
    Ok(got)
}

pub(crate) fn update(paths: &Paths, name: Option<&str>) -> anyhow::Result<ExitCode> {
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
        match install_tool(paths, tool, None) {
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
    program: &str,
) -> Option<(PathBuf, Vec<(String, String)>)> {
    let tool = Tool::from_name(program)?;
    let env = |_: &ca::Installed| match tool {
        // TermOS updates it (`termos agent update`), not Claude Code itself.
        Tool::Claude => vec![("DISABLE_AUTOUPDATER".to_owned(), "1".to_owned())],
        Tool::Codex => Vec::new(),
    };
    if let Some(i) = ca::installed(&paths.data_dir, tool) {
        let e = env(&i);
        return Some((i.path, e));
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
    match install_tool(paths, tool, None) {
        Ok(i) => {
            let e = env(&i);
            Some((i.path, e))
        }
        Err(e) => {
            eprintln!("termos: {e}");
            None
        }
    }
}

/// The pane environment for TermOS-installed agents: Claude Code's own
/// updater off, as TermOS updates it.
pub(crate) fn pane_env(data_dir: &Path) -> Vec<(String, String)> {
    if ca::installed(data_dir, Tool::Claude).is_some() {
        vec![("DISABLE_AUTOUPDATER".to_owned(), "1".to_owned())]
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
