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
use termos_devkit::accounts::{Account, Accounts};
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

pub(crate) fn say(line: &str) {
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
    let node = if tool == Tool::Gemini {
        Some(gemini_node(paths)?)
    } else {
        None
    };
    let got = ca::install_with(
        &Curl,
        &paths.data_dir,
        &release,
        node.as_deref(),
        &mut |m| say(m),
    )?;
    crate::shim::ensure(paths);
    say(&format!(
        "installed {} {} in {}",
        tool.title(),
        got.version,
        got.path.display()
    ));
    // TermOS's skills, for it to read (D-098).
    for line in crate::skills_cmd::ensure(paths, config) {
        say(&line);
    }
    if had.is_none() && config.agents.termos_mcp.register {
        // In every account: each has its own settings (D-097).
        for account in accounts(paths)?.list(tool) {
            match register_tool(&got.path, &account) {
                Ok(how) => say(&how),
                // The install stands; the server can be added later.
                Err(e) => eprintln!(
                    "termos: could not register TermOS's MCP server with {}: {e:#} (try `termos mcp register {}`)",
                    describe(&account),
                    tool.name()
                ),
            }
        }
    }
    // The hooks that mark an agent needing you (D-116).
    for account in accounts(paths)?.list(tool) {
        crate::agent_hooks::ensure(config, &account);
    }
    Ok(got)
}

/// The Node.js that runs Gemini CLI: installed by mise (not made your
/// default), its program found with `mise where`.
fn gemini_node(paths: &Paths) -> anyhow::Result<PathBuf> {
    let mise = termos_devkit::mise::ensure(&paths.data_dir, &mut |m| say(m))?;
    say(&format!(
        "installing Node.js ({}) with mise…",
        ca::GEMINI_NODE
    ));
    let status = std::process::Command::new(&mise.path)
        .args(["install", ca::GEMINI_NODE])
        .status()
        .context("cannot run mise")?;
    if !status.success() {
        bail!("mise could not install Node.js ({status})");
    }
    let out = std::process::Command::new(&mise.path)
        .args(["where", ca::GEMINI_NODE])
        .output()
        .context("cannot run mise")?;
    let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let node = if cfg!(windows) {
        dir.join("node.exe")
    } else {
        dir.join("bin").join("node")
    };
    if !out.status.success() || !node.is_file() {
        bail!(
            "mise installed Node.js, but {} is not there",
            node.display()
        );
    }
    Ok(node)
}

/// TermOS's data folder, as this process finds it.
fn termos_data() -> PathBuf {
    Paths::detect().data_dir
}

/// The accounts of every CLI (D-097).
pub(crate) fn accounts(paths: &Paths) -> anyhow::Result<Accounts> {
    Accounts::load(&paths.data_dir).map_err(|e| anyhow::anyhow!("{e}"))
}

/// "Claude Code" for the default account, "Claude Code · Work" for
/// another.
pub(crate) fn describe(a: &Account) -> String {
    if a.is_default() {
        a.tool.title().to_owned()
    } else {
        format!("{} · {}", a.tool.title(), a.title())
    }
}

pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// The program to register with: this one, by its real path.
pub(crate) fn termos_exe() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot find the termos program")?;
    let real = exe.canonicalize().unwrap_or(exe);
    Ok(if cfg!(windows) { plain(real) } else { real })
}

/// Windows' canonical paths are verbatim (`\\?\D:\…`); the plain form
/// works the same and reads better in the CLIs' settings.
fn plain(p: PathBuf) -> PathBuf {
    let text = p.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

/// How long a CLI may take to add or remove an MCP server.
const REGISTER_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Runs `cmd` without input, for at most `limit`: a CLI that waits for
/// something (a prompt it cannot show) does not hold TermOS up.
pub(crate) fn output_within(
    mut cmd: std::process::Command,
    limit: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let start = std::time::Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            bail!("it did not finish within {} seconds", limit.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Registers `termos mcp serve` with `account`'s CLI (run as `program`).
pub(crate) fn register_tool(program: &Path, account: &Account) -> anyhow::Result<String> {
    let tool = account.tool;
    let termos = termos_exe()?;
    match ca::mcp_register(tool, program, &termos) {
        Some(lines) => {
            for (i, argv) in lines.iter().enumerate() {
                let mut cmd = std::process::Command::new(&argv[0]);
                cmd.args(&argv[1..]).envs(account.env());
                let out = output_within(cmd, REGISTER_LIMIT)
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
                describe(account),
                tool.name()
            ))
        }
        None => {
            let dir = account
                .home(&env, home().as_deref())
                .context("cannot find your home folder")?;
            let file = dir.join(settings_file(tool));
            let before = match std::fs::read_to_string(&file) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e).with_context(|| format!("cannot read {}", file.display())),
            };
            let after = if tool == Tool::Codex {
                ca::codex_config(&before, &termos)?
            } else {
                // TermOS's own install updates through TermOS.
                let managed = program.starts_with(ca::dir(&termos_data(), tool));
                ca::gemini_settings(&before, &termos, managed)?
            };
            std::fs::create_dir_all(&dir)?;
            write_replacing(&file, &after)?;
            Ok(format!(
                "registered TermOS's MCP server with {} (in {})",
                describe(account),
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
pub(crate) fn program_of(paths: &Paths, tool: Tool) -> Option<PathBuf> {
    ca::installed(&paths.data_dir, tool)
        .map(|i| i.path)
        .or_else(|| elsewhere(paths, tool))
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
        for account in accounts(paths)?.list(tool) {
            match register_tool(&program, &account) {
                Ok(how) => say(&how),
                Err(e) => {
                    eprintln!("termos: {}: {e:#}", describe(&account));
                    failed = true;
                }
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
        for account in accounts(paths)?.list(tool) {
            unregister_tool(paths, &account)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Takes TermOS's MCP server out of `account`'s settings.
pub(crate) fn unregister_tool(paths: &Paths, account: &Account) -> anyhow::Result<()> {
    let tool = account.tool;
    {
        match tool {
            Tool::Claude => {
                let Some(program) = program_of(paths, tool) else {
                    return Ok(());
                };
                let mut cmd = std::process::Command::new(&program);
                cmd.args(["mcp", "remove", "--scope", "user", "termos"])
                    .envs(account.env());
                let ok = output_within(cmd, REGISTER_LIMIT).is_ok_and(|o| o.status.success());
                if ok {
                    say(&format!(
                        "removed TermOS's MCP server from {}",
                        describe(account)
                    ));
                }
            }
            Tool::Codex | Tool::Gemini => {
                let Some(dir) = account.home(&env, home().as_deref()) else {
                    return Ok(());
                };
                let file = dir.join(settings_file(tool));
                let Ok(before) = std::fs::read_to_string(&file) else {
                    return Ok(());
                };
                if ca::registered(tool, &before) {
                    let after = if tool == Tool::Codex {
                        ca::codex_config_without(&before)?
                    } else {
                        ca::gemini_settings_without(&before)?
                    };
                    write_replacing(&file, &after)?;
                    say(&format!(
                        "removed TermOS's MCP server from {} ({})",
                        describe(account),
                        file.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Whether TermOS's MCP server is in `account`'s settings.
pub(crate) fn is_registered(account: &Account) -> Option<bool> {
    let tool = account.tool;
    let file = match tool {
        Tool::Claude => account.claude_settings(&env, home().as_deref())?,
        Tool::Codex | Tool::Gemini => account
            .home(&env, home().as_deref())?
            .join(settings_file(tool)),
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
    let removed = ca::remove(&paths.data_dir, tool)?;
    // Its shim goes too, unless another copy is on PATH.
    crate::shim::ensure(paths);
    if removed {
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
        let all = accounts(paths)?;
        let list = all.list(tool);
        for account in &list {
            let whose = if list.len() > 1 {
                format!(" ({})", account.name)
            } else {
                String::new()
            };
            match is_registered(account) {
                Some(true) => println!("{:<12} TermOS's MCP server{whose}: registered", ""),
                Some(false) => println!(
                    "{:<12} TermOS's MCP server{whose}: not registered (`termos mcp register {}`)",
                    "",
                    tool.name()
                ),
                None => {}
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn tool_supported(tool: Tool, p: Platform) -> bool {
    match tool {
        Tool::Claude => p.claude().is_some(),
        Tool::Codex => p.codex().is_some(),
        Tool::Gemini => {
            matches!(p.os, "linux" | "macos" | "windows") && matches!(p.arch, "x86_64" | "aarch64")
        }
    }
}

/// The settings file TermOS edits in a CLI's folder (Claude Code's is
/// changed through `claude mcp`).
fn settings_file(tool: Tool) -> &'static str {
    match tool {
        Tool::Codex => "config.toml",
        _ => "settings.json",
    }
}

/// Where TermOS keeps the status line commands it chains (the user's own,
/// by the settings file they were in).
fn chain_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("accounts").join("statusline.json")
}

fn load_chain(paths: &Paths) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(chain_file(paths))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_chain(
    paths: &Paths,
    chain: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let file = chain_file(paths);
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&file, serde_json::to_vec_pretty(chain)?)?;
    Ok(())
}

/// An account's Claude Code settings file, as the link (if any) resolves.
fn claude_settings_file(a: &Account) -> Option<PathBuf> {
    let file = a.home(&env, home().as_deref())?.join("settings.json");
    Some(file.canonicalize().unwrap_or(file))
}

/// `termos agent statusline`: what Claude Code runs for its status line.
/// It keeps the plan limits Claude Code passes (D-096, D-097), then runs
/// the user's own status line if they had one, else prints a short one.
pub(crate) fn statusline(paths: &Paths) -> anyhow::Result<ExitCode> {
    use std::io::Read as _;
    use termos_devkit::accounts::{DEFAULT, claude_status_limits, save_limits};
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    let all = accounts(paths)?;
    let account = match env("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        None => all.get(Tool::Claude, DEFAULT).ok(),
        Some(dir) => {
            let dir = PathBuf::from(dir);
            let dir = dir.canonicalize().unwrap_or(dir);
            all.list(Tool::Claude).into_iter().find(|a| {
                a.dir
                    .as_ref()
                    .is_some_and(|d| d.canonicalize().unwrap_or_else(|_| d.clone()) == dir)
            })
        }
    };
    if let (Some(a), Some(l)) = (
        &account,
        claude_status_limits(&v, termos_core::journal::now()),
    ) {
        let _ = save_limits(&paths.state_dir, Tool::Claude, &a.name, &l);
    }
    let chained = account
        .as_ref()
        .and_then(claude_settings_file)
        .and_then(|f| load_chain(paths).get(&f.display().to_string()).cloned());
    if let Some(cmd) = chained {
        let mut child = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", &cmd]);
            c
        } else {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", &cmd]);
            c
        }
        .stdin(std::process::Stdio::piped())
        .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes());
        }
        let status = child.wait()?;
        return Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = v.pointer("/model/display_name").and_then(|m| m.as_str()) {
        parts.push(m.to_owned());
    }
    if let Some(a) = account.as_ref().filter(|a| !a.is_default()) {
        parts.push(a.title());
    }
    if let Some(p) = v
        .pointer("/context_window/used_percentage")
        .and_then(serde_json::Value::as_f64)
    {
        parts.push(format!("context {p:.0}%"));
    }
    for (key, label) in [("five_hour", "5 h"), ("seven_day", "week")] {
        if let Some(p) = v
            .pointer(&format!("/rate_limits/{key}/used_percentage"))
            .and_then(serde_json::Value::as_f64)
        {
            parts.push(format!("{label} {p:.0}%"));
        }
    }
    println!("{}", parts.join(" · "));
    Ok(ExitCode::SUCCESS)
}

/// `termos account statusline claude on|off`: TermOS's status line in
/// every Claude Code account's settings (keeping the user's own, which
/// runs after TermOS's), or taken out again.
pub(crate) fn set_statusline(paths: &Paths, on: bool) -> anyhow::Result<ExitCode> {
    use termos_devkit::accounts::{with_statusline, without_statusline};
    let termos = termos_exe()?.display().to_string();
    let quoted = if termos.contains(char::is_whitespace) {
        if cfg!(windows) {
            format!("\"{termos}\"")
        } else {
            format!("'{termos}'")
        }
    } else {
        termos
    };
    let mut chain = load_chain(paths);
    let mut done = std::collections::BTreeSet::new();
    for a in accounts(paths)?.list(Tool::Claude) {
        let Some(file) = claude_settings_file(&a) else {
            continue;
        };
        if !done.insert(file.clone()) {
            continue;
        }
        let key = file.display().to_string();
        let mut v: serde_json::Value = match std::fs::read_to_string(&file) {
            Ok(t) => serde_json::from_str(&t)
                .with_context(|| format!("{} is not valid JSON", file.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", file.display())),
        };
        let changed = if on {
            if let Some(old) =
                with_statusline(&mut v, &quoted).map_err(|e| anyhow::anyhow!("{e}"))?
            {
                chain.insert(key.clone(), old);
            }
            true
        } else {
            without_statusline(&mut v, chain.remove(&key).as_deref())
        };
        if changed {
            if let Some(d) = file.parent() {
                std::fs::create_dir_all(d)?;
            }
            let tmp = file.with_extension(format!("json.termos-{}", std::process::id()));
            std::fs::write(&tmp, serde_json::to_string_pretty(&v)? + "\n")?;
            std::fs::rename(&tmp, &file)?;
            say(&format!(
                "{} TermOS's status line in {} ({})",
                if on { "put" } else { "took out" },
                file.display(),
                describe(&a)
            ));
        }
    }
    save_chain(paths, &chain)?;
    if on {
        say(
            "Claude Code now reports its plan limits to TermOS (subscriptions only); your own status line, if you had one, still shows",
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The arguments that continue `cwd`'s latest conversation of account
/// `from` in `launch`'s account (copied there first), D-097; none when
/// there is nothing to continue.
pub(crate) fn continue_args(
    paths: &Paths,
    launch: &crate::shim::Launch,
    from: &str,
    cwd: &Path,
) -> anyhow::Result<Vec<String>> {
    use termos_devkit::conversations;
    let Some(to) = &launch.account else {
        return Ok(Vec::new());
    };
    let home = home();
    let source = accounts(paths)?
        .get(to.tool, from)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (Some(from_home), Some(to_home)) = (
        source.home(&env, home.as_deref()),
        to.home(&env, home.as_deref()),
    ) else {
        return Ok(Vec::new());
    };
    let Some(c) = conversations::latest(to.tool, &from_home, cwd) else {
        say(&format!(
            "{} has no conversation in this folder to continue; starting a new one",
            describe(&source)
        ));
        return Ok(Vec::new());
    };
    let c = if from_home == to_home {
        c
    } else {
        if to.tool == Tool::Codex {
            // Codex wants the folder to exist before it starts.
            std::fs::create_dir_all(&to_home)?;
        }
        let carried = conversations::carry(&c, &from_home, &to_home)?;
        say(&format!(
            "continuing {}'s conversation as {}",
            describe(&source),
            describe(to)
        ));
        carried
    };
    Ok(conversations::resume_args(&c))
}

/// The arguments that continue `launch`'s account's conversation in
/// `cwd`: `conversation` when it is still there, else the folder's latest
/// (a restored pane, D-115). None: it starts a new one.
pub(crate) fn resume_args_here(
    launch: &crate::shim::Launch,
    conversation: Option<&str>,
    cwd: &Path,
) -> Vec<String> {
    use termos_devkit::conversations;
    let Some(account) = &launch.account else {
        return Vec::new();
    };
    let Some(dir) = account.home(&env, home().as_deref()) else {
        return Vec::new();
    };
    let found = conversation
        .and_then(|id| conversations::find(account.tool, &dir, cwd, id))
        .or_else(|| conversations::latest(account.tool, &dir, cwd));
    match found {
        Some(c) => {
            say(&format!(
                "continuing {}'s conversation in this folder",
                describe(account)
            ));
            conversations::resume_args(&c)
        }
        None => {
            say(&format!(
                "{} has no conversation in this folder to continue; starting a new one",
                describe(account)
            ));
            Vec::new()
        }
    }
}

/// The program and environment for an external agent preset whose
/// program is `program`: for Claude Code and Codex, the real program as
/// `account` (else the pinned one, else the one in use, D-097); when it
/// is not installed and TermOS can install it, it asks (in the agent's
/// window). `None`: not one of them, or nothing to run.
pub(crate) fn resolve_external(
    paths: &Paths,
    config: &Config,
    program: &str,
    account: Option<&str>,
) -> anyhow::Result<Option<crate::shim::Launch>> {
    let Some(tool) = Tool::from_name(program) else {
        return Ok(None);
    };
    if program_of(paths, tool).is_none() {
        if !std::io::stdin().is_terminal() || !tool_supported(tool, Platform::here()) {
            return Ok(None);
        }
        print!(
            "{} is not installed. Install it now (the official program from {}, checked against its SHA-256)? [Y/n] ",
            tool.title(),
            match tool {
                Tool::Claude => "Anthropic",
                Tool::Codex => "OpenAI's GitHub releases",
                Tool::Gemini => "Google's GitHub releases, run by Node.js from mise",
            }
        );
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim().to_ascii_lowercase().starts_with('n') {
            return Ok(None);
        }
        if let Err(e) = install_tool(paths, config, tool, None) {
            eprintln!("termos: {e:#}");
            return Ok(None);
        }
    }
    let launch = crate::shim::plan(paths, tool, account)?;
    if let Some(a) = &launch.account
        && !a.is_default()
    {
        println!("termos: {}", describe(a));
    }
    Ok(Some(launch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_cli_that_hangs_is_given_up_on() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let start = std::time::Instant::now();
        let err = output_within(cmd, std::time::Duration::from_millis(200)).expect_err("gave up");
        assert!(err.to_string().contains("did not finish"), "{err}");
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        let mut ok = std::process::Command::new("sh");
        ok.args(["-c", "echo done"]);
        let out = output_within(ok, std::time::Duration::from_secs(10)).expect("ran");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
    }

    #[test]
    fn verbatim_paths_are_made_plain() {
        assert_eq!(
            plain(PathBuf::from(r"\\?\D:\a\termos.exe")),
            PathBuf::from(r"D:\a\termos.exe")
        );
        // A network path keeps its verbatim form.
        let unc = PathBuf::from(r"\\?\UNC\host\share\termos.exe");
        assert_eq!(plain(unc.clone()), unc);
        assert_eq!(
            plain(PathBuf::from("/usr/bin/termos")),
            PathBuf::from("/usr/bin/termos")
        );
    }

    #[test]
    fn specs_parse() {
        assert_eq!(parse("claude").expect("ok"), (Tool::Claude, None));
        assert_eq!(
            parse("codex@0.46.0").expect("ok"),
            (Tool::Codex, Some("0.46.0"))
        );
        assert_eq!(parse("gemini").expect("ok"), (Tool::Gemini, None));
        assert!(parse("aider").is_err());
    }
}
