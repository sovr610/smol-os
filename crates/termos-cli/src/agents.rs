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
    /// Ask TermOS's agent once and print its answer: read-only (it reads
    /// and searches the files here, nothing else), for programs such as
    /// Claude Code or Codex handing it a question.
    Ask {
        /// The agent profile (`termos agent profiles` lists them).
        #[arg(long, default_value = "default")]
        profile: String,
        /// The provider, over the profile's.
        #[arg(long)]
        provider: Option<String>,
        /// The model, over the profile's.
        #[arg(long)]
        model: Option<String>,
        /// The folder it may read (default: the current one).
        #[arg(long, value_name = "DIR")]
        workspace: Option<PathBuf>,
        /// Print JSON: the answer, the model, its tokens and cost, the
        /// saved conversation.
        #[arg(long)]
        json: bool,
        /// Show its file reads and refusals on stderr as they happen.
        #[arg(long, short)]
        verbose: bool,
        /// The question; a lone `-` (or no question) reads standard input
        /// (`git diff | termos agent ask "Review this:" -`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        prompt: Vec<String>,
    },
    /// The agent profiles, with their provider and model.
    Profiles,
    /// List the models a provider offers (from its API).
    Models {
        /// A provider under [agents.providers].
        provider: String,
    },
    /// Saved conversations, newest first.
    History,
    /// The external agent presets (Claude Code, Codex CLI, …).
    Presets,
    /// Install Claude Code or Codex CLI: the vendor's own program, checked
    /// against its published SHA-256 (`claude`, `codex`, `claude@2.1.90`).
    Install {
        name: String,
        /// Wait for Enter when it ends (TermOS's windows).
        #[arg(long)]
        wait: bool,
    },
    /// Update what TermOS installed (or only the one named).
    Update {
        name: Option<String>,
        /// Wait for Enter when it ends (TermOS's windows).
        #[arg(long)]
        wait: bool,
    },
    /// Remove one TermOS installed (your sign-in and settings stay).
    #[command(alias = "rm")]
    Remove { name: String },
    /// Which Claude Code and Codex CLI TermOS installed.
    Status,
    /// Claude Code's status line command: keeps the plan limits it passes
    /// (for the usage app), then shows your own status line or a short one.
    Statusline,
    /// Run an external agent preset here (what `agent.external` opens).
    External {
        /// For Claude Code and Codex: the account (default: the one in
        /// use).
        #[arg(long)]
        account: Option<String>,
        /// Continue this folder's latest conversation from that account
        /// (switching accounts, D-097).
        #[arg(long)]
        resume_from: Option<String>,
        name: String,
        /// Arguments for the agent.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCmd {
    /// TermOS's MCP server on stdin and stdout (what Claude Code and Codex
    /// CLI start).
    Serve,
    /// Register it with Claude Code and Codex CLI (or the one named).
    Register { name: Option<String> },
    /// Take it out of their settings.
    Unregister { name: Option<String> },
}

pub(crate) fn mcp_command(
    action: &McpCmd,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    match action {
        McpCmd::Serve => crate::coding_agents::serve(paths, config_path),
        McpCmd::Register { name } => crate::coding_agents::register(paths, name.as_deref()),
        McpCmd::Unregister { name } => crate::coding_agents::unregister(paths, name.as_deref()),
    }
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

/// `termos agent ask`'s options.
struct Ask {
    profile: String,
    provider: Option<String>,
    model: Option<String>,
    workspace: Option<PathBuf>,
    json: bool,
    verbose: bool,
}

/// The question: the arguments, with a lone `-` (or nothing at all)
/// standing for standard input.
fn ask_text(args: &[String], stdin: &mut dyn std::io::Read) -> anyhow::Result<String> {
    let mut read_stdin = || -> anyhow::Result<String> {
        let mut text = String::new();
        stdin
            .read_to_string(&mut text)
            .context("cannot read the question from standard input")?;
        Ok(text)
    };
    let text = if args.is_empty() {
        read_stdin()?
    } else {
        let mut parts = Vec::new();
        for a in args {
            parts.push(if a == "-" { read_stdin()? } else { a.clone() });
        }
        parts.join(" ")
    };
    if text.trim().is_empty() {
        bail!("no question: give it as arguments or on standard input");
    }
    Ok(text)
}

/// `termos agent ask` (D-098): TermOS's agent, once, read-only.
fn ask_command(
    ask: &Ask,
    args: &[String],
    config: &Config,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() && std::io::stdin().is_terminal() {
        bail!("no question: `termos agent ask \"…\"`, or pipe it in");
    }
    let prompt = ask_text(args, &mut std::io::stdin())?;
    let workspace = match &ask.workspace {
        Some(w) => w.clone(),
        None => std::env::current_dir()?,
    };
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("cannot open {}", workspace.display()))?;
    // The provider's key: unlock the secrets file here when someone is at
    // the terminal to type its passphrase.
    let agents = &config.agents;
    let profile = agents
        .profiles
        .get(&ask.profile)
        .cloned()
        .unwrap_or_default();
    let provider = ask.provider.clone().or(profile.provider).or_else(|| {
        (agents.providers.len() == 1)
            .then(|| agents.providers.keys().next().cloned())
            .flatten()
    });
    let needs_key = provider
        .as_ref()
        .and_then(|p| agents.providers.get(p))
        .is_some_and(|p| p.key.is_some());
    let store = SecretStore::open(config.secrets.store, &paths.data_dir);
    if needs_key
        && store.is_locked()
        && store.file_exists()
        && (std::io::stdin().is_terminal() || std::io::stderr().is_terminal())
    {
        store.unlock(read_hidden(&format!(
            "Passphrase for {}",
            store.file().display()
        ))?)?;
    }
    let launch = termos_ai::agent::Launch {
        agents: agents.clone(),
        secrets: std::sync::Arc::new(termos_ai::agent::SecretSlot::with(store)),
        paths: paths.clone(),
        profile: ask.profile.clone(),
        workspace,
        resume: None,
        helper: std::env::current_exe().ok(),
        config_file: Some(config_path.to_owned()),
        home: home(),
        journal: Some(std::sync::Arc::new(termos_core::journal::Journal::new(
            &paths.state_dir,
            config.center.keep_days,
        ))),
        registry: None,
        provider: ask.provider.clone(),
        model: ask.model.clone(),
        headless: termos_ai::tools::Headless::ReadOnly,
    };
    let shown = std::sync::Mutex::new(std::collections::HashSet::<String>::new());
    let verbose = ask.verbose;
    let progress = move |v: &termos_ai::agent::View| {
        if !verbose {
            return;
        }
        use termos_ai::agent::{Item, ToolState};
        let Ok(mut shown) = shown.lock() else { return };
        for item in &v.items {
            let Item::Tool(t) = item else { continue };
            let line = match &t.state {
                ToolState::Done => format!("· {}", t.summary),
                ToolState::Failed => format!("· {} (failed)", t.summary),
                ToolState::Denied(why) => format!("· {} refused: {why}", t.summary),
                _ => continue,
            };
            if shown.insert(t.id.clone()) {
                eprintln!("{line}");
            }
        }
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let answer = rt
        .block_on(termos_ai::agent::ask(launch, prompt, progress))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut out = std::io::stdout().lock();
    if ask.json {
        let u = answer.usage;
        let v = serde_json::json!({
            "answer": answer.text,
            "provider": answer.provider,
            "model": answer.model,
            "tokens": {
                "input": u.input,
                "output": u.output,
                "cache_read": u.cache_read,
                "cache_write": u.cache_write,
            },
            "cost_usd": answer.cost,
            "session": answer.session,
            "notes": answer.notes,
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&v)?)?;
    } else {
        writeln!(out, "{}", answer.text)?;
        if verbose {
            for n in &answer.notes {
                eprintln!("note: {n}");
            }
            eprintln!(
                "{} {} · {} in, {} out{} · session {}",
                answer.provider,
                answer.model,
                answer.usage.input + answer.usage.cache_read + answer.usage.cache_write,
                answer.usage.output,
                answer
                    .cost
                    .map(|c| format!(" · ${c:.4}"))
                    .unwrap_or_default(),
                answer.session
            );
        }
    }
    Ok(ExitCode::SUCCESS)
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
        AgentCmd::Ask {
            profile,
            provider,
            model,
            workspace,
            json,
            verbose,
            prompt,
        } => {
            let ask = Ask {
                profile: profile.clone(),
                provider: provider.clone(),
                model: model.clone(),
                workspace: workspace.clone(),
                json: *json,
                verbose: *verbose,
            };
            return ask_command(&ask, prompt, &config, paths, config_path);
        }
        AgentCmd::Profiles => {
            let agents = &config.agents;
            let only = (agents.providers.len() == 1)
                .then(|| agents.providers.keys().next().cloned())
                .flatten();
            let mut names: Vec<&String> = agents.profiles.keys().collect();
            let default = "default".to_owned();
            if !agents.profiles.contains_key("default") {
                names.insert(0, &default);
            }
            for name in names {
                let p = agents.profiles.get(name).cloned().unwrap_or_default();
                let provider = p
                    .provider
                    .clone()
                    .or_else(|| only.clone())
                    .unwrap_or_else(|| "(asks)".into());
                let model = p.model.clone().unwrap_or_else(|| "(asks)".into());
                println!("{name:<16} {provider:<14} {model}");
            }
            if agents.providers.is_empty() {
                println!("no providers: add one under [agents.providers.<name>] (docs/agents.md)");
            }
        }
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
        AgentCmd::Install { name, wait } => {
            return pause(*wait, crate::coding_agents::install(paths, &config, name));
        }
        AgentCmd::Update { name, wait } => {
            return pause(
                *wait,
                crate::coding_agents::update(paths, &config, name.as_deref()),
            );
        }
        AgentCmd::Remove { name } => return crate::coding_agents::remove(paths, name),
        AgentCmd::Status => return crate::coding_agents::status(paths),
        AgentCmd::Statusline => return crate::coding_agents::statusline(paths),
        AgentCmd::External {
            account,
            resume_from,
            name,
            args,
        } => {
            return external_command(
                &config,
                paths,
                name,
                account.as_deref(),
                resume_from.as_deref(),
                args,
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// In a TermOS window: the outcome stays until Enter.
pub(crate) fn pause(wait: bool, result: anyhow::Result<ExitCode>) -> anyhow::Result<ExitCode> {
    if !wait {
        return result;
    }
    let code = result.unwrap_or_else(|e| {
        eprintln!("termos: {e:#}");
        ExitCode::FAILURE
    });
    println!("\n[press Enter to close]");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    Ok(code)
}

fn external_command(
    config: &Config,
    paths: &Paths,
    name: &str,
    account: Option<&str>,
    resume_from: Option<&str>,
    args: &[String],
) -> anyhow::Result<ExitCode> {
    let presets = external::presets(&config.agents.external);
    let Some(preset) = presets.get(name) else {
        let names: Vec<&str> = presets.keys().map(String::as_str).collect();
        bail!(
            "no external agent named {name} (there are {})",
            names.join(", ")
        );
    };
    // Claude Code and Codex: TermOS's own install, or offered when there
    // is none (D-094).
    let mut preset = preset.clone();
    let mut env = Vec::new();
    if let Some(program) = preset.command.first()
        && let Some(launch) =
            crate::coding_agents::resolve_external(paths, config, program, account)?
    {
        if let Some(from) = resume_from {
            let here = std::env::current_dir()?;
            preset.command.extend(crate::coding_agents::continue_args(
                paths, &launch, from, &here,
            )?);
        }
        preset.command[0] = launch.program.display().to_string();
        env = launch.env;
    }
    let preset = &preset;
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
    cmd.args(&argv[1..]).envs(env);
    match cmd.status() {
        Ok(s) => Ok(if s.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }),
        Err(e) => {
            eprintln!("termos: cannot start {}: {e}", preset.command[0]);
            if termos_devkit::cli_agents::Tool::from_name(name).is_some() {
                eprintln!("termos: install it with `termos agent install {name}`");
            } else if let Some(url) = preset.install {
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
