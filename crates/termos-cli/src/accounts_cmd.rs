// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos account`: several accounts of Claude Code and Codex CLI, each
//! a profile in its own folder (M11a, D-096, D-097).

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::bail;
use clap::Subcommand;
use termos_core::config::Config;
use termos_core::paths::Paths;
use termos_devkit::accounts::{self, Account, Meta};
use termos_devkit::cli_agents::Tool;

use crate::coding_agents::{accounts as load, describe, env, home, program_of, register_tool};

#[derive(Debug, Subcommand)]
pub(crate) enum AccountCmd {
    /// Every account of Claude Code and Codex CLI (or of one); `●` marks
    /// the one in use.
    #[command(alias = "ls")]
    List { cli: Option<String> },
    /// A new account: its own folder, with your settings, instructions and
    /// skills shared into it; then sign in.
    Add {
        cli: String,
        /// Asked for when not given.
        name: Option<String>,
        /// Shown instead of the name, e.g. "Work (Max)".
        #[arg(long)]
        label: Option<String>,
        /// Do not start the sign-in now.
        #[arg(long)]
        no_sign_in: bool,
    },
    /// Use this account: new windows, and `claude` or `codex` in every
    /// TermOS window, follow it.
    Use { cli: String, name: String },
    /// Sign in to an account (the CLI's own sign-in, in its folder).
    SignIn { cli: String, name: String },
    /// Show a label instead of the name.
    Label {
        cli: String,
        name: String,
        label: String,
    },
    /// Token budgets for the usage app's gauges (2M, 500k; 0 clears).
    /// Nothing is enforced.
    Budget {
        cli: String,
        name: String,
        /// For Claude Code's 5-hour window.
        #[arg(long)]
        five_hour: Option<String>,
        /// For 7 days.
        #[arg(long)]
        week: Option<String>,
    },
    /// Claude Code's plan limits in the usage app: TermOS's status line in
    /// every Claude Code account (yours, if any, still shows), or `off`.
    Statusline {
        cli: String,
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Remove an account: its folder, with its login and conversations.
    #[command(alias = "rm")]
    Remove {
        cli: String,
        name: String,
        /// Do not ask.
        #[arg(long)]
        yes: bool,
    },
}

/// A line typed in answer to `question` (in a terminal only).
fn ask(question: &str) -> anyhow::Result<String> {
    if !std::io::stdin().is_terminal() {
        bail!("needs an answer: give it on the command line");
    }
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

fn tool(cli: &str) -> anyhow::Result<Tool> {
    match Tool::from_name(cli) {
        Some(t) => Ok(t),
        None => {
            bail!("accounts are for `claude` (Claude Code) and `codex` (Codex CLI), not {cli:?}")
        }
    }
}

/// "2M", "500k", "1.5M", "2000000"; 0 is none.
fn tokens(text: &str) -> anyhow::Result<Option<u64>> {
    let t = text.trim().to_ascii_lowercase().replace(['_', ','], "");
    let (num, scale) = match t.chars().last() {
        Some('k') => (&t[..t.len() - 1], 1_000.0),
        Some('m') => (&t[..t.len() - 1], 1_000_000.0),
        Some('b' | 'g') => (&t[..t.len() - 1], 1_000_000_000.0),
        _ => (t.as_str(), 1.0),
    };
    let Ok(n) = num.parse::<f64>() else {
        bail!("{text:?} is not a number of tokens (2M, 500k, 2000000)");
    };
    if !n.is_finite() || n < 0.0 {
        bail!("{text:?} is not a number of tokens");
    }
    let n = (n * scale).round() as u64;
    Ok((n > 0).then_some(n))
}

fn signed_in(a: &Account) -> String {
    a.signed_in(&env, home().as_deref())
}

pub(crate) fn account_command(
    action: &AccountCmd,
    paths: &Paths,
    config_path: &std::path::Path,
) -> anyhow::Result<ExitCode> {
    let mut all = load(paths)?;
    match action {
        AccountCmd::List { cli } => {
            let tools = match cli {
                Some(c) => vec![tool(c)?],
                None => Tool::ALL.to_vec(),
            };
            for t in tools {
                let active = all.active(t).name;
                println!("{}", t.title());
                for a in all.list(t) {
                    let mark = if a.name == active { "●" } else { " " };
                    let label = a.meta.label.clone().unwrap_or_default();
                    let folder = a
                        .home(&env, home().as_deref())
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    println!(
                        "  {mark} {:<12} {:<16} {:<16} {folder}",
                        a.name,
                        label,
                        signed_in(&a)
                    );
                }
            }
        }
        AccountCmd::Add {
            cli,
            name,
            label,
            no_sign_in,
        } => {
            let t = tool(cli)?;
            let name = match name {
                Some(n) => n.clone(),
                None => ask(&format!(
                    "A name for the new {} account (letters, digits, - and _): ",
                    t.title()
                ))?,
            };
            let name = &name;
            let from = accounts::default_home(t, &env, home().as_deref());
            let a = all
                .add(t, name, label.as_deref(), from.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "termos: added {} in {}",
                describe(&a),
                a.dir
                    .as_deref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default()
            );
            let config = Config::load(config_path).unwrap_or_default();
            match program_of(paths, t) {
                Some(program) => {
                    if config.agents.termos_mcp.register {
                        match register_tool(&program, &a) {
                            Ok(how) => println!("termos: {how}"),
                            Err(e) => eprintln!("termos: {e:#}"),
                        }
                    }
                    if !no_sign_in && std::io::stdin().is_terminal() {
                        print!("Sign in to it now? [Y/n] ");
                        let _ = std::io::stdout().flush();
                        let mut answer = String::new();
                        let _ = std::io::stdin().read_line(&mut answer);
                        if !answer.trim().to_ascii_lowercase().starts_with('n') {
                            return sign_in(paths, &a);
                        }
                    }
                    println!(
                        "Sign in with `termos account sign-in {} {}`; use it with `termos account use {} {}`.",
                        t.name(),
                        a.name,
                        t.name(),
                        a.name
                    );
                }
                None => println!(
                    "{} is not installed yet: `termos agent install {}`, then `termos account sign-in {} {}`.",
                    t.title(),
                    t.name(),
                    t.name(),
                    a.name
                ),
            }
        }
        AccountCmd::Use { cli, name } => {
            let a = all
                .use_account(tool(cli)?, name)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "termos: {} is in use; new windows follow it (running ones keep theirs until switched)",
                describe(&a)
            );
        }
        AccountCmd::SignIn { cli, name } => {
            let a = all
                .get(tool(cli)?, name)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return sign_in(paths, &a);
        }
        AccountCmd::Label { cli, name, label } => {
            let t = tool(cli)?;
            let mut meta = all.get(t, name).map_err(|e| anyhow::anyhow!("{e}"))?.meta;
            meta.label = Some(label.clone()).filter(|l| !l.is_empty());
            all.set_meta(t, name, meta)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        AccountCmd::Budget {
            cli,
            name,
            five_hour,
            week,
        } => {
            let t = tool(cli)?;
            let mut meta: Meta = all.get(t, name).map_err(|e| anyhow::anyhow!("{e}"))?.meta;
            let (mut five_hour, mut week) = (five_hour.clone(), week.clone());
            if five_hour.is_none() && week.is_none() {
                // In a window: ask (Enter keeps a value).
                let shown = |b: Option<u64>| b.map_or("none".to_owned(), |n| n.to_string());
                let a = ask(&format!(
                    "Tokens per 5 hours (now {}; 2M, 500k, 0 for none): ",
                    shown(meta.budget_5h)
                ))?;
                five_hour = Some(a).filter(|a| !a.is_empty());
                let a = ask(&format!(
                    "Tokens per 7 days (now {}): ",
                    shown(meta.budget_week)
                ))?;
                week = Some(a).filter(|a| !a.is_empty());
            }
            if let Some(v) = &five_hour {
                meta.budget_5h = tokens(v)?;
            }
            if let Some(v) = &week {
                meta.budget_week = tokens(v)?;
            }
            all.set_meta(t, name, meta)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        AccountCmd::Statusline { cli, state } => {
            if tool(cli)? != Tool::Claude {
                bail!("Codex records its limits itself; only Claude Code needs the status line");
            }
            return crate::coding_agents::set_statusline(paths, state == "on");
        }
        AccountCmd::Remove { cli, name, yes } => {
            let t = tool(cli)?;
            let a = all.get(t, name).map_err(|e| anyhow::anyhow!("{e}"))?;
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!("removing deletes its login and conversations; add --yes to confirm");
                }
                print!(
                    "Remove {} with its login and conversations? Type its name to confirm: ",
                    describe(&a)
                );
                let _ = std::io::stdout().flush();
                let mut answer = String::new();
                let _ = std::io::stdin().read_line(&mut answer);
                if answer.trim() != a.name {
                    println!("termos: kept");
                    return Ok(ExitCode::FAILURE);
                }
            }
            all.remove(t, name).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("termos: removed {}", describe(&a));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The CLI's own sign-in, in `a`'s folder: Claude Code asks on its first
/// start; Codex has `codex login`.
fn sign_in(paths: &Paths, a: &Account) -> anyhow::Result<ExitCode> {
    let Some(program) = program_of(paths, a.tool) else {
        bail!(
            "{} is not installed (`termos agent install {}`)",
            a.tool.title(),
            a.tool.name()
        );
    };
    let mut cmd = std::process::Command::new(program);
    cmd.envs(a.env());
    match a.tool {
        Tool::Claude => {
            println!(
                "termos: starting Claude Code for {}; it asks you to sign in (or type /login)",
                describe(a)
            );
            cmd.env("DISABLE_AUTOUPDATER", "1");
        }
        Tool::Codex => {
            cmd.arg("login");
        }
    }
    let status = cmd.status()?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_amounts_parse() {
        assert_eq!(tokens("2M").expect("ok"), Some(2_000_000));
        assert_eq!(tokens("1.5m").expect("ok"), Some(1_500_000));
        assert_eq!(tokens("500k").expect("ok"), Some(500_000));
        assert_eq!(tokens("2_000_000").expect("ok"), Some(2_000_000));
        assert_eq!(tokens("0").expect("ok"), None);
        assert!(tokens("lots").is_err());
        assert!(tokens("-5").is_err());
        assert!(tool("gemini").is_err());
    }
}
