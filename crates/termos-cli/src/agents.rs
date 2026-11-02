// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos secret`, `termos agent`, `termos audit` and `termos sandbox`
//! (M6, ARCHITECTURE.md §11).

use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use clap::Subcommand;
use secrecy::{ExposeSecret as _, SecretString};
use termos_ai::secrets::{Backend, SecretError, SecretStore};
use termos_ai::{audit, external, history, sandbox};
use termos_core::config::Config;
use termos_core::paths::Paths;

#[derive(Debug, Subcommand)]
pub(crate) enum SecretCmd {
    /// Store an API key (asked for without echo, or read from stdin).
    Set {
        /// The name the config uses (`keyring:<name>`).
        name: String,
    },
    /// Remove a stored key.
    #[command(alias = "rm")]
    Remove { name: String },
    /// Where keys are kept, and whether each key the config names is set.
    #[command(alias = "ls")]
    List,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentCmd {
    /// List the models a provider offers (from its API).
    Models {
        /// A provider under [agents.providers].
        provider: String,
    },
    /// Saved conversations, newest first.
    History,
    /// The external agent presets (Claude Code, Codex CLI, …).
    Presets,
    /// Run an external agent preset here (what `agent.external` opens).
    External {
        name: String,
        /// Arguments for the agent.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum AuditCmd {
    /// Every agent session's audit log, checked.
    #[command(alias = "ls")]
    List,
    /// Check audit logs (a session id or a file; default: all). Exits
    /// non-zero if any was tampered with.
    Verify { target: Option<String> },
    /// Print a session's audit log.
    Show { session: String },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SandboxCmd {
    /// What sandbox this machine offers agents.
    Status,
    /// Run a command in the agent sandbox, with this folder writable.
    Run {
        /// Let it reach the network.
        #[arg(long)]
        net: bool,
        /// Also writable.
        #[arg(long = "write", value_name = "PATH")]
        write: Vec<PathBuf>,
        /// Also hidden (made empty).
        #[arg(long = "hide", value_name = "PATH")]
        hide: Vec<PathBuf>,
        /// Kept reachable although under a hidden folder (a socket).
        #[arg(long = "keep", value_name = "PATH")]
        keep: Vec<PathBuf>,
        /// Pass the environment on unchanged.
        #[arg(long)]
        keep_env: bool,
        /// The writable folder (default: the current one, where the
        /// command starts either way).
        #[arg(long, value_name = "DIR")]
        workspace: Option<PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn load_config(path: &Path) -> anyhow::Result<Config> {
    Config::load(path).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Reads a line without echoing it, from the terminal.
pub(crate) fn read_hidden(prompt: &str) -> anyhow::Result<SecretString> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    if !std::io::stdin().is_terminal() && !std::io::stderr().is_terminal() {
        bail!("{prompt}: run this in a terminal (the answer is not read from a pipe)");
    }
    eprint!("{prompt}: ");
    let _ = std::io::stderr().flush();
    enable_raw_mode().context("cannot read from the terminal")?;
    let mut text = String::new();
    let result = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            },
            Ok(Event::Paste(p)) => text.push_str(&p),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    let _ = disable_raw_mode();
    eprintln!();
    result.map(|()| SecretString::from(text))
}

/// Opens the configured store, unlocking (or creating) the secrets file
/// when `need_open` says so.
fn open_store(config: &Config, paths: &Paths, need_open: bool) -> anyhow::Result<SecretStore> {
    let store = SecretStore::open(config.secrets.store, &paths.data_dir);
    if store.backend() == Backend::File && need_open {
        if store.file_exists() {
            let pass = read_hidden(&format!("Passphrase for {}", store.file().display()))?;
            store.unlock(pass)?;
        } else {
            eprintln!(
                "TermOS keeps API keys in {}, an age file locked with a passphrase you choose.",
                store.file().display()
            );
            let a = read_hidden("New passphrase")?;
            if a.expose_secret().chars().count() < 8 {
                bail!("use a passphrase of at least 8 characters");
            }
            let b = read_hidden("The same again")?;
            if a.expose_secret() != b.expose_secret() {
                bail!("the passphrases differ");
            }
            store.create(a)?;
        }
    }
    Ok(store)
}

/// Every secret name the config refers to, with what uses it.
fn referenced(config: &Config) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, p) in &config.agents.providers {
        if let Some(k) = &p.key {
            out.push((k.name().to_owned(), format!("provider {name}")));
        }
    }
    for (name, m) in &config.agents.mcp {
        if let Some(k) = &m.key {
            out.push((k.name().to_owned(), format!("MCP server {name}")));
        }
    }
    out
}

pub(crate) fn secret_command(
    action: &SecretCmd,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    let config = load_config(config_path)?;
    match action {
        SecretCmd::Set { name } => {
            if !termos_core::config::agents::valid_secret_name(name) {
                bail!("invalid secret name `{name}`: use letters, digits, `.`, `-` and `_`");
            }
            let value = if std::io::stdin().is_terminal() {
                read_hidden(&format!("Value for {name}"))?
            } else {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                SecretString::from(s.trim_end_matches(['\r', '\n']).to_owned())
            };
            if value.expose_secret().is_empty() {
                bail!("the value is empty");
            }
            let store = open_store(&config, paths, true)?;
            store.set(name, value)?;
            println!("stored {name} in {}", store.backend());
        }
        SecretCmd::Remove { name } => {
            let store = open_store(&config, paths, true)?;
            if store.remove(name)? {
                println!("removed {name}");
            } else {
                println!("no secret named {name}");
            }
        }
        SecretCmd::List => {
            let store = open_store(&config, paths, false)?;
            match store.backend() {
                Backend::Keyring => println!(
                    "store: the OS keyring (service `{}`)",
                    termos_ai::secrets::KEYRING_SERVICE
                ),
                Backend::File => println!("store: {} (age, passphrase)", store.file().display()),
            }
            let locked = store.is_locked();
            if locked && std::io::stdin().is_terminal() {
                let pass = read_hidden("Passphrase (to check which keys are set)")?;
                store.unlock(pass)?;
            }
            for (name, used_by) in referenced(&config) {
                let state = match store.get(&name) {
                    Ok(_) => "set",
                    Err(SecretError::NotFound(_)) => "MISSING",
                    Err(SecretError::Locked) => "locked",
                    Err(e) => return Err(e.into()),
                };
                println!("  {name:<20} {state:<8} ({used_by})");
            }
            if let Ok(Some(names)) = store.names() {
                let used: Vec<String> = referenced(&config).into_iter().map(|(n, _)| n).collect();
                for n in names.iter().filter(|n| !used.contains(n)) {
                    println!("  {n:<20} set      (not used by the config)");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn agent_command(
    action: &AgentCmd,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    let config = load_config(config_path)?;
    match action {
        AgentCmd::Models { provider } => {
            let Some(pcfg) = config.agents.providers.get(provider) else {
                bail!("no provider named {provider} under [agents.providers]");
            };
            let key = match &pcfg.key {
                Some(r) => {
                    let store = open_store(&config, paths, false)?;
                    if store.is_locked() {
                        store.unlock(read_hidden(&format!(
                            "Passphrase for {}",
                            store.file().display()
                        ))?)?;
                    }
                    Some(store.get(r.name())?)
                }
                None => None,
            };
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            // A `local` provider without an address uses the model router.
            let mut pcfg = pcfg.clone();
            let mut key = key;
            if termos_ai::provider::uses_router(&pcfg) {
                let helper = std::env::current_exe().ok();
                let (url, token) = rt
                    .block_on(termos_ai::provider::local::router(
                        paths,
                        helper.as_deref(),
                        Some(config_path),
                    ))
                    .map_err(|e| anyhow::anyhow!(e.message))?;
                pcfg.base_url = Some(url);
                key = Some(token);
            }
            let p = termos_ai::provider::build(provider, &pcfg, key)
                .map_err(|e| anyhow::anyhow!(e.message))?;
            let models = rt
                .block_on(p.list_models())
                .map_err(|e| anyhow::anyhow!("{provider}: {e}"))?;
            for m in models {
                let mut extra = Vec::new();
                if let Some(n) = &m.name {
                    extra.push(n.clone());
                }
                if let Some(c) = m.context {
                    extra.push(format!("{c} tokens of context"));
                }
                if let Some(pr) = m.price {
                    extra.push(format!("${} in / ${} out per million", pr.input, pr.output));
                }
                if extra.is_empty() {
                    println!("{}", m.id);
                } else {
                    println!("{}  ({})", m.id, extra.join(", "));
                }
            }
        }
        AgentCmd::History => {
            let all = history::list(&history::dir(&paths.state_dir));
            if all.is_empty() {
                println!("no saved conversations");
            }
            for s in all {
                let when: String = s
                    .updated
                    .chars()
                    .take(16)
                    .collect::<String>()
                    .replace('T', " ");
                println!(
                    "{}  {when}  {} {}  {}",
                    s.id,
                    s.provider,
                    s.model,
                    s.title()
                );
            }
        }
        AgentCmd::Presets => {
            for p in external::presets(&config.agents.external).values() {
                let sandbox = if p.sandbox {
                    "sandboxed"
                } else {
                    "not sandboxed"
                };
                println!(
                    "{:<10} {:<14} `{}` ({sandbox})",
                    p.name,
                    p.description,
                    p.command.join(" ")
                );
            }
        }
        AgentCmd::External { name, args } => return external_command(&config, name, args),
    }
    Ok(ExitCode::SUCCESS)
}

fn external_command(config: &Config, name: &str, args: &[String]) -> anyhow::Result<ExitCode> {
    let presets = external::presets(&config.agents.external);
    let Some(preset) = presets.get(name) else {
        let names: Vec<&str> = presets.keys().map(String::as_str).collect();
        bail!(
            "no external agent named {name} (there are {})",
            names.join(", ")
        );
    };
    let here = std::env::current_dir()?;
    let helper = std::env::current_exe().ok();
    let caps = sandbox::probe_now(helper.as_deref());
    let argv = external::command_line(
        preset,
        args,
        &here,
        home().as_deref(),
        &caps,
        helper.as_deref(),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    if preset.sandbox {
        println!(
            "termos: {} ({})",
            preset.description,
            caps.describe(termos_core::config::agents::SandboxMode::Auto)
        );
    }
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    match cmd.status() {
        Ok(s) => Ok(if s.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }),
        Err(e) => {
            eprintln!("termos: cannot start {}: {e}", preset.command[0]);
            if let Some(url) = preset.install {
                eprintln!("termos: install {} first: {url}", preset.description);
            }
            eprintln!("\n[press Enter to close]");
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            Ok(ExitCode::FAILURE)
        }
    }
}

pub(crate) fn audit_command(action: &AuditCmd, paths: &Paths) -> anyhow::Result<ExitCode> {
    let dir = audit::dir(&paths.state_dir);
    let hist = history::dir(&paths.state_dir);
    let check = |p: &Path| {
        let id = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let head = history::Saved::load(&hist, &id)
            .ok()
            .and_then(|s| s.audit_head);
        (id, audit::verify_with_head(p, head.as_deref()))
    };
    let resolve = |target: &str| -> PathBuf {
        let p = PathBuf::from(target);
        if p.exists() {
            p
        } else {
            dir.join(format!("{target}.jsonl"))
        }
    };
    match action {
        AuditCmd::List | AuditCmd::Verify { target: None } => {
            let logs = audit::list(&dir);
            if logs.is_empty() {
                println!("no audit logs in {}", dir.display());
            }
            let mut bad = 0;
            for p in logs {
                match check(&p) {
                    (id, Ok(v)) => println!("{id}  ok ({} entries)", v.entries),
                    (id, Err(e)) => {
                        bad += 1;
                        println!("{id}  TAMPERED: {e}");
                    }
                }
            }
            if bad > 0 && matches!(action, AuditCmd::Verify { .. }) {
                return Ok(ExitCode::FAILURE);
            }
        }
        AuditCmd::Verify { target: Some(t) } => {
            let p = resolve(t);
            match check(&p) {
                (id, Ok(v)) => println!("{id}  ok ({} entries)", v.entries),
                (id, Err(e)) => {
                    println!("{id}  TAMPERED: {e}");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        AuditCmd::Show { session } => {
            let p = resolve(session);
            for e in audit::read(&p).with_context(|| format!("cannot read {}", p.display()))? {
                println!("{}", serde_json::to_string(&e)?);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn sandbox_command(action: &SandboxCmd, paths: &Paths) -> anyhow::Result<ExitCode> {
    let helper = std::env::current_exe().ok();
    let caps = sandbox::probe_now(helper.as_deref());
    match action {
        SandboxCmd::Status => {
            println!(
                "{}",
                caps.describe(termos_core::config::agents::SandboxMode::Auto)
            );
            Ok(ExitCode::SUCCESS)
        }
        SandboxCmd::Run {
            net,
            write,
            hide,
            keep,
            keep_env,
            workspace,
            command,
        } => {
            let here = std::env::current_dir()?;
            let workspace = workspace.clone().unwrap_or_else(|| here.clone());
            let home = home();
            let mut hidden: Vec<PathBuf> = home
                .iter()
                .flat_map(|h| {
                    termos_ai::policy::SENSITIVE_IN_HOME
                        .iter()
                        .map(move |s| h.join(s))
                })
                .collect();
            if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
                hidden.push(PathBuf::from(run));
            }
            hidden.push(paths.runtime_dir.clone());
            hidden.push(history::dir(&paths.state_dir));
            hidden.push(audit::dir(&paths.state_dir));
            hidden.push(termos_ai::secrets::file_path(&paths.data_dir));
            hidden.extend(hide.iter().cloned());
            let spec = sandbox::Spec {
                workspace,
                writable: write.clone(),
                hidden,
                network: *net,
                cwd: here,
                keep_env: *keep_env,
            };
            let mut argv = sandbox::wrap(
                &spec,
                &caps,
                termos_core::config::agents::SandboxMode::Auto,
                helper.as_deref(),
                command,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            // Paths to keep reachable inside hidden folders (the session
            // socket for sandboxed plugins): bound again after the hiding.
            if caps.bwrap.is_some() && !keep.is_empty() {
                let at = argv
                    .iter()
                    .position(|a| a == "--chdir")
                    .unwrap_or(argv.len());
                let mut binds = Vec::new();
                for k in keep {
                    binds.extend([
                        "--bind".to_owned(),
                        k.display().to_string(),
                        k.display().to_string(),
                    ]);
                }
                argv.splice(at..at, binds);
            }
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            // Become the sandbox: whoever stops us (the server stopping a
            // plugin) stops what runs inside.
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt as _;
                let err = cmd.exec();
                Err(anyhow::anyhow!("cannot run {}: {err}", argv[0]))
            }
            #[cfg(not(unix))]
            {
                let status = cmd
                    .status()
                    .with_context(|| format!("cannot run {}", argv[0]))?;
                Ok(ExitCode::from(
                    u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
                ))
            }
        }
    }
}
