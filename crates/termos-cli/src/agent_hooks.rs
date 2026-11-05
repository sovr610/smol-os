// SPDX-License-Identifier: GPL-3.0-or-later
//! The coding CLIs' hooks (D-115, D-116).
//!
//! Claude Code, Codex and Gemini CLI run a command at points of their
//! work, passing JSON that says which. TermOS registers `termos agent
//! event --cli NAME` in each account's settings for: a permission prompt
//! or question (needs you), the end of a turn (done), a prompt submitted
//! (working again), and a conversation starting or ending (its id, for
//! restoring the pane). The command tells the TermOS server which pane
//! (`$TERMOS_PANE`) and returns at once; outside TermOS it does nothing.
//! It prints nothing and always succeeds, so it can never block or
//! change the agent's work.

use std::io::{IsTerminal as _, Read as _};
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use serde_json::{Value, json};
use termos_core::paths::Paths;
use termos_devkit::accounts::Account;
use termos_devkit::cli_agents::{self as ca, Tool};

use crate::coding_agents::{accounts, describe, env, home, say};

/// The most of a hook's JSON read.
const MAX_INPUT: u64 = 1024 * 1024;

/// What a hook reported, for the server's `agent.event`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reported {
    /// `needs-you`, `done`, `working`, `started` or `ended`.
    pub kind: &'static str,
    pub message: String,
    pub conversation: Option<String>,
    pub cwd: Option<String>,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The first line of a reply, as a summary.
fn first_line(s: Option<String>) -> String {
    s.and_then(|t| {
        t.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_owned)
    })
    .unwrap_or_default()
}

/// Reads a hook's JSON as a TermOS event (`None`: one TermOS ignores).
pub(crate) fn translate(cli: &str, v: &Value) -> Option<Reported> {
    let conversation = text(v, "session_id").or_else(|| text(v, "thread-id"));
    let cwd = text(v, "cwd");
    let tool = || {
        text(v, "tool_name")
            .map(|t| format!("wants to use {t}"))
            .unwrap_or_else(|| "asks for permission".into())
    };
    let (kind, message) = match text(v, "hook_event_name").as_deref() {
        Some("PermissionRequest") => ("needs-you", tool()),
        Some("Notification") => {
            let message = text(v, "message").unwrap_or_default();
            match text(v, "notification_type").as_deref() {
                // Claude Code: these wait for an answer.
                Some(
                    "permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog"
                    | "agent_needs_input",
                )
                // Gemini CLI's only kind.
                | Some("ToolPermission")
                | None => ("needs-you", message),
                Some("agent_completed") => ("done", message),
                // A reminder after the turn ended (`Stop` said so), a
                // sign-in, quota notices.
                Some(_) => return None,
            }
        }
        Some("Stop" | "AfterAgent") => (
            "done",
            first_line(text(v, "last_assistant_message").or_else(|| text(v, "prompt_response"))),
        ),
        Some("StopFailure") => ("needs-you", "the turn stopped with an error".to_owned()),
        Some("UserPromptSubmit" | "BeforeAgent") => ("working", String::new()),
        Some("SessionStart") => ("started", String::new()),
        Some("SessionEnd") => ("ended", String::new()),
        Some(_) => return None,
        // Codex's `notify` program: its JSON names a type instead.
        None => match text(v, "type").as_deref() {
            Some("agent-turn-complete") if cli == "codex" => {
                ("done", first_line(text(v, "last-assistant-message")))
            }
            _ => return None,
        },
    };
    Some(Reported {
        kind,
        message: message.chars().take(500).collect(),
        conversation,
        cwd,
    })
}

/// `termos agent event --cli NAME [JSON]`: what a hook runs.
pub(crate) fn event(cli: &str, json_arg: Option<&str>) -> ExitCode {
    if let Err(e) = report(cli, json_arg) {
        // Never in the agent's way: the log only.
        tracing::debug!("agent event from {cli}: {e:#}");
    }
    ExitCode::SUCCESS
}

fn report(cli: &str, json_arg: Option<&str>) -> anyhow::Result<()> {
    let inside = crate::inside_termos().zip(crate::tos::pane_id());
    // `TERMOS_HOOK_LOG`: also write what the hook reported to that file
    // (to see a CLI's hooks reach TermOS, as CI does).
    let log = std::env::var_os("TERMOS_HOOK_LOG").map(std::path::PathBuf::from);
    if inside.is_none() && log.is_none() {
        // Not in a TermOS pane.
        return Ok(());
    }
    let input = match json_arg {
        Some(j) => j.to_owned(),
        None => {
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                return Ok(());
            }
            let mut s = String::new();
            stdin.take(MAX_INPUT).read_to_string(&mut s)?;
            s
        }
    };
    let v: Value = serde_json::from_str(&input).context("the hook's JSON")?;
    let Some(r) = translate(cli, &v) else {
        return Ok(());
    };
    if let Some(log) = log {
        use std::io::Write as _;
        // Names and ids only, never what the agent said.
        let line = json!({ "cli": cli, "kind": r.kind, "conversation": r.conversation });
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .with_context(|| format!("cannot write {}", log.display()))?;
        writeln!(f, "{line}")?;
    }
    let Some(((socket, session), pane)) = inside else {
        return Ok(());
    };
    let account = Tool::from_name(cli)
        .and_then(|t| std::env::var(crate::shim::pin_variable(t)).ok())
        .filter(|a| !a.is_empty());
    let mut params = json!({
        "pane": pane,
        "cli": cli,
        "kind": r.kind,
        "message": r.message,
        "session": session,
    });
    if let Some(c) = r.conversation {
        params["conversation"] = Value::from(c);
    }
    if let Some(a) = account {
        params["account"] = Value::from(a);
    }
    if let Some(d) = r.cwd {
        params["cwd"] = Value::from(d);
    }
    let t = crate::Target {
        socket,
        name: String::new(),
        session: None,
    };
    crate::call(&t, "agent.event", Some(params))?;
    Ok(())
}

/// The hooks file of `account`. An account's settings may be a link to
/// the main ones (D-097): the file it points to, so the link stays.
fn file_of(account: &Account) -> Option<std::path::PathBuf> {
    let file = account
        .home(&env, home().as_deref())
        .map(|d| d.join(ca::hooks_file(account.tool)))?;
    Some(std::fs::canonicalize(&file).unwrap_or(file))
}

fn read(file: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(file) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
    }
}

/// Writes `file` through a temporary file beside it.
fn write(file: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension(format!("json.termos-{}", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(file) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, file).with_context(|| format!("cannot replace {}", file.display()))
}

/// Adds TermOS's hooks to `account`'s settings (replacing older ones);
/// what it did. `false` in the result: they were there already.
pub(crate) fn register(account: &Account) -> anyhow::Result<(bool, String)> {
    let termos = crate::coding_agents::termos_exe()?;
    let file = file_of(account).context("cannot find your home folder")?;
    let before = read(&file)?;
    if ca::hooks_registered(account.tool, &before, &termos) {
        return Ok((false, format!("{} has TermOS's hooks", describe(account))));
    }
    let after = ca::with_hooks(account.tool, &before, &termos)?;
    write(&file, &after)?;
    let mut how = format!(
        "added TermOS's hooks to {} (in {})",
        describe(account),
        file.display()
    );
    if account.tool == Tool::Codex {
        how.push_str("; Codex asks you to trust them when it next starts");
    }
    Ok((true, how))
}

/// Takes TermOS's hooks out of `account`'s settings.
pub(crate) fn unregister(account: &Account) -> anyhow::Result<bool> {
    let Some(file) = file_of(account) else {
        return Ok(false);
    };
    let before = read(&file)?;
    if before.is_empty() {
        return Ok(false);
    }
    let after = ca::without_hooks(account.tool, &before)?;
    if after == before {
        return Ok(false);
    }
    write(&file, &after)?;
    Ok(true)
}

/// Registers the hooks with `account` unless they are there, quietly:
/// for agent windows started before this TermOS had hooks (D-116).
pub(crate) fn ensure(config: &termos_core::config::Config, account: &Account) {
    if !config.agents.hooks.register {
        return;
    }
    match register(account) {
        Ok((true, how)) => say(&how),
        Ok((false, _)) => {}
        Err(e) => eprintln!(
            "termos: could not add TermOS's hooks to {}: {e:#}",
            describe(account)
        ),
    }
}

/// `termos agent hooks [--remove] [NAME]`.
pub(crate) fn command(paths: &Paths, remove: bool, name: Option<&str>) -> anyhow::Result<ExitCode> {
    let tools = match name {
        Some(n) => vec![
            Tool::from_name(n)
                .with_context(|| format!("no coding CLI named {n} (claude, codex or gemini)"))?,
        ],
        None => Tool::ALL.to_vec(),
    };
    let mut failed = false;
    for tool in tools {
        for account in accounts(paths)?.list(tool) {
            if remove {
                match unregister(&account) {
                    Ok(true) => say(&format!(
                        "removed TermOS's hooks from {}",
                        describe(&account)
                    )),
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("termos: {}: {e:#}", describe(&account));
                        failed = true;
                    }
                }
                continue;
            }
            match register(&account) {
                Ok((_, how)) => say(&how),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(cli: &str, j: &str) -> Option<(&'static str, String, Option<String>)> {
        let v: Value = serde_json::from_str(j).expect("json");
        translate(cli, &v).map(|r| (r.kind, r.message, r.conversation))
    }

    #[test]
    fn claude_code_hooks() {
        assert_eq!(
            kind(
                "claude",
                r#"{"hook_event_name":"PermissionRequest","session_id":"s1","tool_name":"Bash","tool_input":{"command":"rm -rf x"}}"#
            ),
            Some(("needs-you", "wants to use Bash".into(), Some("s1".into())))
        );
        assert_eq!(
            kind(
                "claude",
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}"#
            ),
            Some((
                "needs-you",
                "Claude needs your permission to use Bash".into(),
                None
            ))
        );
        assert_eq!(
            kind(
                "claude",
                r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"waiting"}"#
            ),
            None
        );
        assert_eq!(
            kind(
                "claude",
                r#"{"hook_event_name":"Stop","session_id":"s1","last_assistant_message":"\n Done: tests pass.\nMore"}"#
            ),
            Some(("done", "Done: tests pass.".into(), Some("s1".into())))
        );
        assert_eq!(
            kind(
                "claude",
                r#"{"hook_event_name":"SessionStart","session_id":"s2","source":"resume"}"#
            ),
            Some(("started", String::new(), Some("s2".into())))
        );
        assert_eq!(
            kind(
                "claude",
                r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#
            )
            .map(|k| k.0),
            Some("working")
        );
        assert_eq!(kind("claude", r#"{"hook_event_name":"PreToolUse"}"#), None);
    }

    #[test]
    fn codex_and_gemini() {
        assert_eq!(
            kind(
                "codex",
                r#"{"type":"agent-turn-complete","thread-id":"t1","last-assistant-message":"All set"}"#
            ),
            Some(("done", "All set".into(), Some("t1".into())))
        );
        assert_eq!(kind("claude", r#"{"type":"agent-turn-complete"}"#), None);
        assert_eq!(
            kind(
                "gemini",
                r#"{"hook_event_name":"Notification","notification_type":"ToolPermission","message":"Allow shell?"}"#
            )
            .map(|k| k.0),
            Some("needs-you")
        );
        assert_eq!(
            kind(
                "gemini",
                r#"{"hook_event_name":"AfterAgent","prompt_response":"Fixed it"}"#
            ),
            Some(("done", "Fixed it".into(), None))
        );
    }
}
