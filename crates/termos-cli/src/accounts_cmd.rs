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
    /// Sign in to an account through the CLI's own sign-in, in its folder:
    /// Claude Code shows a link and takes the code it gives back; Codex
    /// opens a browser, or shows a device code where it cannot.
    SignIn {
        cli: String,
        name: String,
        /// Claude Code: an Anthropic Console account instead of a Claude
        /// subscription.
        #[arg(long)]
        console: bool,
        /// Claude Code: single sign-on.
        #[arg(long, conflicts_with = "console")]
        sso: bool,
        /// Codex: a device code instead of a browser on this machine.
        #[arg(long)]
        device: bool,
    },
    /// Use an API key for this account instead of a sign-in (asked for, or
    /// read from standard input). Claude Code's is kept in TermOS's secret
    /// store and given to it when it starts; Codex keeps its own.
    Key {
        cli: String,
        name: String,
        /// Stop using the key (Claude Code's is deleted from the store;
        /// Codex is signed out).
        #[arg(long)]
        remove: bool,
    },
    /// Who each account is signed in as, asked of the CLIs themselves
    /// (`claude auth status`, `codex login status`).
    Status {
        cli: Option<String>,
        name: Option<String>,
    },
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
            bail!(
                "accounts are for `claude` (Claude Code), `codex` (Codex CLI) and `gemini` (Gemini CLI), not {cli:?}"
            )
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
            // TermOS's skills, in its folder too (D-098).
            for line in crate::skills_cmd::ensure(paths, &config) {
                println!("termos: {line}");
            }
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
        AccountCmd::SignIn {
            cli,
            name,
            console,
            sso,
            device,
        } => {
            let a = all
                .get(tool(cli)?, name)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let how = How {
                console: *console,
                sso: *sso,
                device: *device,
            };
            return sign_in_how(paths, &a, how);
        }
        AccountCmd::Status { cli, name } => {
            let tools = match cli {
                Some(c) => vec![tool(c)?],
                None => Tool::ALL.to_vec(),
            };
            for t in tools {
                let program = program_of(paths, t);
                println!("{}", t.title());
                for a in all.list(t) {
                    if name.as_ref().is_some_and(|n| *n != a.name) {
                        continue;
                    }
                    let line = match &program {
                        Some(p) => status_line(&auth_status(p, &a)),
                        None => "not installed".to_owned(),
                    };
                    println!("  {:<12} {line}", a.name);
                }
            }
        }
        AccountCmd::Key { cli, name, remove } => {
            let t = tool(cli)?;
            let a = all.get(t, name).map_err(|e| anyhow::anyhow!("{e}"))?;
            let config = Config::load(config_path).unwrap_or_default();
            return if *remove {
                forget_key(paths, &config, &mut all, &a)
            } else {
                set_key(paths, &config, &mut all, &a)
            };
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
/// Which of a CLI's sign-ins to use.
#[derive(Debug, Clone, Copy, Default)]
struct How {
    console: bool,
    sso: bool,
    device: bool,
}

fn sign_in(paths: &Paths, a: &Account) -> anyhow::Result<ExitCode> {
    sign_in_how(paths, a, How::default())
}

/// The CLI as the account, with credentials inherited from the environment
/// dropped (the sign-in is the account's own).
/// `program` as the account, with the environment the shim would give it
/// (so its status says what a window would use).
fn as_account(program: &std::path::Path, a: &Account) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    if a.isolated() {
        for v in accounts::credential_variables(a.tool) {
            cmd.env_remove(v);
        }
    }
    cmd.envs(a.env());
    cmd
}

fn installed_program(paths: &Paths, t: Tool) -> anyhow::Result<std::path::PathBuf> {
    program_of(paths, t).ok_or_else(|| {
        anyhow::anyhow!(
            "{} is not installed (`termos agent install {}`)",
            t.title(),
            t.name()
        )
    })
}

/// Runs the CLI's own sign-in in the account's folder (D-111): TermOS
/// never sees the login; the CLI keeps it.
fn sign_in_how(paths: &Paths, a: &Account, how: How) -> anyhow::Result<ExitCode> {
    if a.meta.api_key.is_some() {
        bail!(
            "{} uses an API key, which comes before a sign-in: `termos account key {} {} --remove` first",
            describe(a),
            a.tool.name(),
            a.name
        );
    }
    let program = installed_program(paths, a.tool)?;
    let mut cmd = as_account(&program, a);
    if !a.isolated() {
        // `default` keeps the environment, whose sign-in comes first.
        let set: Vec<&str> = accounts::credential_variables(a.tool)
            .iter()
            .copied()
            .filter(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()))
            .collect();
        if !set.is_empty() {
            eprintln!(
                "termos: your environment sets {}; {} uses that before this sign-in for `default` (TermOS's own accounts leave it out).",
                set.join(", "),
                a.tool.title()
            );
        }
    }
    match a.tool {
        Tool::Claude => {
            cmd.args(["auth", "login"]);
            if how.console {
                cmd.arg("--console");
            }
            if how.sso {
                cmd.arg("--sso");
            }
            cmd.env("DISABLE_AUTOUPDATER", "1");
            println!(
                "termos: Claude Code's sign-in for {}. Open the link it shows in any browser, sign in, and paste the code you get back here.",
                describe(a)
            );
        }
        Tool::Codex => {
            cmd.arg("login");
            let device = how.device || !accounts::browser_here(&env);
            if device {
                cmd.arg("--device-auth");
                println!(
                    "termos: Codex's sign-in for {} with a device code: open the link it shows on any device and enter the code. (A ChatGPT workspace may have to allow device codes first.)",
                    describe(a)
                );
            } else {
                println!(
                    "termos: Codex's sign-in for {}: it opens your browser (`--device` for a code instead).",
                    describe(a)
                );
            }
        }
        Tool::Gemini => {
            // Its own sign-in starts with the CLI: the Google account
            // chosen in advance, so it goes straight there.
            set_gemini_auth(a, "oauth-personal")?;
            if !accounts::browser_here(&env) {
                cmd.env("NO_BROWSER", "true");
            }
            println!(
                "termos: Gemini CLI's sign-in for {}: it signs you in with your Google account (where no browser opens, it shows a link and asks for the code you get). Type /quit once it is done.",
                describe(a)
            );
        }
    }
    let status = cmd.status()?;
    if !status.success() {
        bail!("the sign-in did not finish ({status})");
    }
    println!("termos: {}", status_line(&auth_status(&program, a)));
    Ok(ExitCode::SUCCESS)
}

/// What the CLI says about `a`'s sign-in. An account with a key in TermOS's
/// store is not asked (that would mean reading the key).
fn auth_status(program: &std::path::Path, a: &Account) -> accounts::AuthStatus {
    if a.meta.api_key.is_some() {
        return accounts::AuthStatus {
            signed_in: true,
            method: "API key (TermOS's secret store)".into(),
            who: None,
        };
    }
    let mut cmd = as_account(program, a);
    match a.tool {
        Tool::Claude => cmd.args(["auth", "status"]),
        Tool::Codex => cmd.args(["login", "status"]),
        // No status command: its folder says.
        Tool::Gemini => {
            return accounts::gemini_status(a.home(&env, home().as_deref()).as_deref());
        }
    };
    let Ok(out) = crate::coding_agents::output_within(cmd, std::time::Duration::from_secs(30))
    else {
        return accounts::AuthStatus {
            method: "no answer".into(),
            ..Default::default()
        };
    };
    let ok = out.status.success();
    match a.tool {
        Tool::Claude => accounts::claude_status(&String::from_utf8_lossy(&out.stdout), ok),
        Tool::Gemini => accounts::AuthStatus::default(),
        // It reports on standard error.
        Tool::Codex => accounts::codex_status(
            &format!(
                "{}{}",
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            ),
            ok,
        ),
    }
}

fn status_line(s: &accounts::AuthStatus) -> String {
    if s.method == "no answer" {
        return "did not answer".into();
    }
    s.line()
}

/// Chooses Gemini CLI's sign-in method in the account's settings.
fn set_gemini_auth(a: &Account, kind: &str) -> anyhow::Result<()> {
    let dir = a
        .home(&env, home().as_deref())
        .ok_or_else(|| anyhow::anyhow!("cannot find your home folder"))?;
    let file = dir.join("settings.json");
    let before = std::fs::read_to_string(&file).unwrap_or_default();
    let after = termos_devkit::cli_agents::gemini_auth(&before, kind)?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&file, after)?;
    Ok(())
}

/// The key, typed without echo in a terminal or read from standard input.
fn read_key(a: &Account) -> anyhow::Result<secrecy::SecretString> {
    use secrecy::ExposeSecret as _;
    let key = if std::io::stdin().is_terminal() {
        crate::agents::read_hidden(&format!("API key for {}", describe(a)))?
    } else {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        secrecy::SecretString::from(s.trim().to_owned())
    };
    if key.expose_secret().is_empty() {
        bail!("the key is empty");
    }
    Ok(key)
}

fn set_key(
    paths: &Paths,
    config: &Config,
    all: &mut accounts::Accounts,
    a: &Account,
) -> anyhow::Result<ExitCode> {
    let key = read_key(a)?;
    match accounts::key_variable(a.tool) {
        Some(var) => {
            let name = accounts::key_secret(a.tool, &a.name);
            let store = crate::agents::open_store(config, paths, true)?;
            store.set(&name, key)?;
            let mut meta = a.meta.clone();
            meta.api_key = Some(name.clone());
            all.set_meta(a.tool, &a.name, meta)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "termos: {} now uses this API key ({name} in {}), given to {} as {var} when it starts.",
                describe(a),
                store.backend(),
                a.tool.title()
            );
            if a.tool == Tool::Claude {
                println!(
                    "Claude Code asks once whether to use it: choose Yes (its suggestion is No)."
                );
            }
            if a.tool == Tool::Gemini {
                set_gemini_auth(a, "gemini-api-key")?;
            }
        }
        None => {
            // Codex keeps the key itself, in the account's folder.
            use secrecy::ExposeSecret as _;
            use std::io::Write as _;
            let program = installed_program(paths, a.tool)?;
            let mut cmd = as_account(&program, a);
            cmd.args(["login", "--with-api-key"])
                .stdin(std::process::Stdio::piped());
            let mut child = cmd.spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(key.expose_secret().as_bytes())?;
                stdin.write_all(b"\n")?;
            }
            let status = child.wait()?;
            if !status.success() {
                bail!("`codex login --with-api-key` failed ({status})");
            }
            println!("termos: {}", status_line(&auth_status(&program, a)));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn forget_key(
    paths: &Paths,
    config: &Config,
    all: &mut accounts::Accounts,
    a: &Account,
) -> anyhow::Result<ExitCode> {
    match accounts::key_variable(a.tool) {
        Some(_) => {
            let Some(name) = a.meta.api_key.clone() else {
                println!("termos: {} has no API key", describe(a));
                return Ok(ExitCode::SUCCESS);
            };
            let store = crate::agents::open_store(config, paths, true)?;
            store.remove(&name)?;
            let mut meta = a.meta.clone();
            meta.api_key = None;
            all.set_meta(a.tool, &a.name, meta)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "termos: {} no longer uses an API key; `termos account sign-in {} {}` signs in.",
                describe(a),
                a.tool.name(),
                a.name
            );
        }
        None => {
            let program = installed_program(paths, a.tool)?;
            let status = as_account(&program, a).arg("logout").status()?;
            if !status.success() {
                bail!("`codex logout` failed ({status})");
            }
            println!("termos: {} is signed out", describe(a));
        }
    }
    Ok(ExitCode::SUCCESS)
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
        assert_eq!(tool("gemini").expect("ok"), Tool::Gemini);
        assert!(tool("aider").is_err());
    }
}
