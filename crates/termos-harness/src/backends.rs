// SPDX-License-Identifier: GPL-3.0-or-later
//! Running one task on one backend (D-099, D-100).
//!
//! - **Claude Code:** `claude -p --output-format stream-json --verbose`,
//!   the prompt on standard input. A task that changes files gets
//!   `--permission-mode acceptEdits` and `--allowedTools` with the allowed
//!   commands; a read-only one has editing and shell tools taken away.
//!   Headless, anything else it would ask for is refused (and reported).
//! - **Codex CLI:** `codex exec --json`, the prompt on standard input, in
//!   its own sandbox: `workspace-write` (no network) for tasks that change
//!   files, `read-only` otherwise.
//! - **API:** TermOS's own agent ([`termos_ai::agent::ask`]), unattended:
//!   edits in the project and the allowed commands, or read-only.
//!
//! The CLIs run as the backend's account (their folder variable, through
//! [`Programs`]), in the project folder, and are stopped (with what they
//! started) when the task is stopped or runs out of time.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use termos_ai::agent::{SecretSlot, View};
use termos_ai::tools::Headless;
use termos_core::config::agents::AgentsConfig;
use termos_core::config::harness::BackendKind;
use termos_core::journal::Journal;
use termos_core::paths::Paths;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::watch;

use crate::events::{self, Event, Tokens};
use crate::route::Choice;

/// How to start a CLI as an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub path: PathBuf,
    pub env: Vec<(String, String)>,
    /// The account it runs as, for showing (`work`; `None`: the default).
    pub account: Option<String>,
}

/// Where the CLIs are: the `termos` binary finds them through its shims'
/// logic (TermOS's install or `PATH`, with the account's folder); tests
/// give stand-ins.
pub trait Programs: Send + Sync {
    /// `kind` (Claude Code or Codex) as `account` (`None`: the one in
    /// use), or why it cannot run.
    fn cli(&self, kind: BackendKind, account: Option<&str>) -> Result<Program, String>;
}

/// What TermOS's own agent needs for API tasks.
#[derive(Clone)]
pub struct ApiContext {
    pub agents: AgentsConfig,
    pub secrets: Arc<SecretSlot>,
    pub paths: Paths,
    /// The `termos` binary (sandbox helper, model router).
    pub helper: Option<PathBuf>,
    pub config_file: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub journal: Option<Arc<Journal>>,
}

impl std::fmt::Debug for ApiContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiContext").finish_non_exhaustive()
    }
}

/// One try of a task on one backend.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub choice: Choice,
    pub prompt: String,
    /// The task may change files.
    pub writes: bool,
    pub project: PathBuf,
    /// Commands it may run (patterns), for tasks that change files.
    pub commands: Vec<String>,
    pub timeout: Duration,
}

/// What came of an attempt.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    /// The answer, or what went wrong.
    pub text: String,
    /// A plan's usage or rate limit was reached.
    pub limit: bool,
    pub stopped: bool,
    pub tokens: Tokens,
    /// US dollars charged (API backends, where prices are known).
    pub cost: Option<f64>,
    /// The CLI's own estimate (not charged under a plan).
    pub estimate: Option<f64>,
    /// Its session, to continue it (`claude --resume`, `codex resume`, an
    /// agent window).
    pub session: Option<String>,
    pub account: Option<String>,
    /// What it was not allowed to do.
    pub denied: Vec<String>,
}

/// Something to show while a task runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// A tool it used: its name and what on.
    Tool { name: String, detail: String },
    /// Text from the model (its latest message).
    Text(String),
}

/// Reads one line a CLI printed.
type LineReader = fn(&str) -> Vec<Event>;

/// The tools Claude Code may not use in a read-only task.
const CLAUDE_EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit", "Bash"];

/// A command pattern as a Claude Code permission rule: `cargo test*` →
/// `Bash(cargo test:*)`, `make` → `Bash(make)`. `None` for a pattern it
/// cannot say as narrowly (a `*` inside).
pub fn claude_rule(pattern: &str) -> Option<String> {
    let p = pattern.trim();
    if p.is_empty() {
        return None;
    }
    match p.strip_suffix('*') {
        Some(prefix) if !prefix.contains('*') => {
            let prefix = prefix.trim_end();
            (!prefix.is_empty()).then(|| format!("Bash({prefix}:*)"))
        }
        Some(_) => None,
        None if p.contains('*') => None,
        None => Some(format!("Bash({p})")),
    }
}

/// `claude`'s arguments for an attempt (the prompt goes on stdin).
pub fn claude_args(model: Option<&str>, writes: bool, commands: &[String]) -> Vec<String> {
    let mut a: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"]
        .map(String::from)
        .to_vec();
    if let Some(m) = model {
        a.extend(["--model".into(), m.to_owned()]);
    }
    if writes {
        a.extend(["--permission-mode".into(), "acceptEdits".into()]);
        let rules: Vec<String> = commands.iter().filter_map(|c| claude_rule(c)).collect();
        if !rules.is_empty() {
            a.push("--allowedTools".into());
            a.extend(rules);
        }
    } else {
        a.extend(["--permission-mode".into(), "default".into()]);
        a.push("--disallowedTools".into());
        a.extend(CLAUDE_EDIT_TOOLS.iter().map(|t| (*t).to_owned()));
    }
    a
}

/// `codex`'s arguments for an attempt (the prompt goes on stdin).
pub fn codex_args(model: Option<&str>, writes: bool, project: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "exec".into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        "--cd".into(),
        project.display().to_string(),
        "--sandbox".into(),
        if writes {
            "workspace-write"
        } else {
            "read-only"
        }
        .into(),
    ];
    if let Some(m) = model {
        a.extend(["--model".into(), m.to_owned()]);
    }
    // The prompt, from standard input.
    a.push("-".into());
    a
}

/// Runs an attempt. `stop` turning true stops it.
pub async fn run(
    a: &Attempt,
    programs: &dyn Programs,
    api: Option<&ApiContext>,
    progress: Arc<dyn Fn(Progress) + Send + Sync>,
    stop: watch::Receiver<bool>,
) -> Outcome {
    let b = &a.choice.backend;
    let model = a.choice.model.as_deref();
    match b.kind {
        BackendKind::Claude | BackendKind::Codex => {
            let program = match programs.cli(b.kind, b.account.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    return Outcome {
                        text: e,
                        ..Outcome::default()
                    };
                }
            };
            let (args, parse): (Vec<String>, LineReader) = if b.kind == BackendKind::Claude {
                (
                    claude_args(model, a.writes, &a.commands),
                    events::claude_line,
                )
            } else {
                (codex_args(model, a.writes, &a.project), events::codex_line)
            };
            let mut out = run_cli(&program, &args, a, parse, &progress, stop).await;
            out.account = program.account;
            out
        }
        BackendKind::Api => match api {
            Some(api) => run_api(a, api, progress, stop).await,
            None => Outcome {
                text: "API backends are not available here".into(),
                ..Outcome::default()
            },
        },
    }
}

/// The last lines a program wrote to stderr, for its error.
fn tail(lines: &[String]) -> String {
    let start = lines.len().saturating_sub(3);
    lines[start..].join(" / ")
}

async fn run_cli(
    program: &Program,
    args: &[String],
    a: &Attempt,
    parse: LineReader,
    progress: &Arc<dyn Fn(Progress) + Send + Sync>,
    mut stop: watch::Receiver<bool>,
) -> Outcome {
    let mut cmd = tokio::process::Command::new(&program.path);
    cmd.args(args)
        .current_dir(&a.project)
        .envs(program.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own process group (Unix) or job (Windows, started suspended
    // until it is in the job), so stopping it stops what it started.
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(crate::job::CREATE_SUSPENDED);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Outcome {
                text: format!("cannot start {}: {e}", program.path.display()),
                ..Outcome::default()
            };
        }
    };
    #[cfg(windows)]
    let job = match crate::job::Job::start(&mut child) {
        Ok(job) => job,
        Err(e) => {
            return Outcome {
                text: format!("cannot start {}: {e}", program.path.display()),
                ..Outcome::default()
            };
        }
    };
    #[cfg(not(windows))]
    let job = ();
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = a.prompt.clone();
        tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    if let Some(stderr) = child.stderr.take() {
        let errors = Arc::clone(&errors);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Ok(mut e) = errors.lock() {
                    if e.len() >= 200 {
                        e.remove(0);
                    }
                    e.push(l);
                }
            }
        });
    }
    let mut out = Outcome::default();
    let mut last_text = String::new();
    let mut result: Option<(String, bool)> = None;
    let mut failures: Vec<String> = Vec::new();
    let deadline = tokio::time::sleep(a.timeout);
    tokio::pin!(deadline);
    let mut lines = child.stdout.take().map(|s| BufReader::new(s).lines());
    let mut ended_early: Option<&str> = None;
    if let Some(lines) = lines.as_mut() {
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Ok(Some(line)) = line else { break };
                    for e in parse(&line) {
                        match e {
                            Event::Started { session, .. } => out.session = session.or(out.session.take()),
                            Event::Text(t) => {
                                progress(Progress::Text(events::one_line(&t, 200)));
                                last_text = t;
                            }
                            Event::Tool { name, detail } => progress(Progress::Tool { name, detail }),
                            Event::Usage(t) => out.tokens.add(t),
                            Event::Result { text, error, cost, tokens, denied } => {
                                out.tokens.add(tokens);
                                out.estimate = cost;
                                out.denied = denied;
                                result = Some((text, error));
                            }
                            Event::Failed(m) => failures.push(m),
                        }
                    }
                }
                _ = stop.changed() => {
                    if *stop.borrow() {
                        ended_early = Some("stopped");
                        break;
                    }
                }
                () = &mut deadline => {
                    ended_early = Some("timed out");
                    break;
                }
            }
        }
    }
    if let Some(why) = ended_early {
        stop_child(&mut child, &job).await;
        out.stopped = why == "stopped";
        out.text = if out.stopped {
            "stopped".into()
        } else {
            format!(
                "stopped after {} minutes ([harness] task-minutes)",
                a.timeout.as_secs() / 60
            )
        };
        return out;
    }
    let status = child.wait().await;
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    let exit_ok = status.as_ref().is_ok_and(std::process::ExitStatus::success);
    match result {
        // Claude Code: its last line says it all.
        Some((text, error)) => {
            out.ok = !error && exit_ok;
            out.text = if text.is_empty() && !exit_ok {
                tail(&errors)
            } else {
                text
            };
        }
        // Codex (or a CLI that stopped before its last line).
        None => {
            out.ok = exit_ok && failures.is_empty() && !last_text.is_empty();
            out.text = if out.ok {
                last_text
            } else if let Some(f) = failures.last() {
                f.clone()
            } else if !errors.is_empty() {
                tail(&errors)
            } else {
                match status {
                    Ok(s) if s.success() => "it gave no answer".into(),
                    Ok(s) => format!("it exited with {s}"),
                    Err(e) => e.to_string(),
                }
            };
        }
    }
    out.limit = !out.ok && events::is_limit(&out.text);
    out
}

/// Stops a CLI and everything it started: its process group on Unix, its
/// job on Windows.
#[cfg(windows)]
async fn stop_child(child: &mut tokio::process::Child, job: &Option<crate::job::Job>) {
    if let Some(job) = job {
        job.terminate();
    }
    let _ = child.kill().await;
}

/// Stops a CLI and everything it started: its process group on Unix, its
/// job on Windows.
#[cfg(not(windows))]
async fn stop_child(child: &mut tokio::process::Child, _job: &()) {
    #[cfg(unix)]
    if let Some(pid) = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
        if tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .is_ok()
        {
            return;
        }
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill().await;
}

async fn run_api(
    a: &Attempt,
    api: &ApiContext,
    progress: Arc<dyn Fn(Progress) + Send + Sync>,
    mut stop: watch::Receiver<bool>,
) -> Outcome {
    let b = &a.choice.backend;
    let launch = termos_ai::agent::Launch {
        agents: api.agents.clone(),
        secrets: Arc::clone(&api.secrets),
        paths: api.paths.clone(),
        profile: "default".into(),
        workspace: a.project.clone(),
        resume: None,
        helper: api.helper.clone(),
        config_file: api.config_file.clone(),
        home: api.home.clone(),
        journal: api.journal.clone(),
        registry: None,
        provider: b.provider.clone(),
        model: a.choice.model.clone(),
        headless: if a.writes {
            Headless::Edits {
                commands: a.commands.clone(),
            }
        } else {
            Headless::ReadOnly
        },
    };
    // Each tool once, when it finishes.
    let shown = Mutex::new(std::collections::HashSet::<String>::new());
    let watcher = move |v: &View| {
        use termos_ai::agent::{Item, ToolState};
        let Ok(mut shown) = shown.lock() else { return };
        for item in &v.items {
            let Item::Tool(t) = item else { continue };
            if matches!(t.state, ToolState::Asking | ToolState::Running) {
                continue;
            }
            if shown.insert(t.id.clone()) {
                progress(Progress::Tool {
                    name: t.tool.clone(),
                    detail: t.summary.clone(),
                });
            }
        }
    };
    let asked = termos_ai::agent::ask(launch, a.prompt.clone(), watcher);
    let answer = tokio::select! {
        r = asked => r,
        () = async {
            while stop.changed().await.is_ok() {
                if *stop.borrow() {
                    return;
                }
            }
            std::future::pending::<()>().await;
        } => {
            return Outcome { stopped: true, text: "stopped".into(), ..Outcome::default() };
        }
        () = tokio::time::sleep(a.timeout) => {
            return Outcome {
                text: format!("stopped after {} minutes ([harness] task-minutes)", a.timeout.as_secs() / 60),
                ..Outcome::default()
            };
        }
    };
    match answer {
        Ok(ans) => Outcome {
            ok: true,
            text: ans.text,
            tokens: Tokens {
                input: ans.usage.input,
                output: ans.usage.output,
                cache_read: ans.usage.cache_read,
                cache_write: ans.usage.cache_write,
            },
            cost: ans.cost,
            session: Some(ans.session),
            denied: ans.notes,
            ..Outcome::default()
        },
        Err(e) => Outcome {
            limit: events::is_limit(&e),
            text: e,
            ..Outcome::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_patterns_become_claude_rules() {
        assert_eq!(
            claude_rule("cargo test*").as_deref(),
            Some("Bash(cargo test:*)")
        );
        assert_eq!(
            claude_rule("npm run lint *").as_deref(),
            Some("Bash(npm run lint:*)")
        );
        assert_eq!(claude_rule("make").as_deref(), Some("Bash(make)"));
        assert_eq!(claude_rule("go * ./..."), None, "too broad to say");
        assert_eq!(claude_rule("*"), None);
        assert_eq!(claude_rule(" "), None);
    }

    #[test]
    fn the_clis_are_given_their_permissions() {
        let w = claude_args(
            Some("fixture"),
            true,
            &["cargo test*".into(), "go * x".into()],
        );
        assert_eq!(
            w,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--model",
                "fixture",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Bash(cargo test:*)"
            ]
        );
        let r = claude_args(None, false, &["cargo test*".into()]);
        assert!(r.windows(2).any(|w| w == ["--permission-mode", "default"]));
        assert!(r.contains(&"--disallowedTools".to_owned()) && r.contains(&"Bash".to_owned()));
        assert!(!r.contains(&"--model".to_owned()));
        let c = codex_args(None, true, Path::new("/p"));
        assert!(c.windows(2).any(|w| w == ["--sandbox", "workspace-write"]));
        assert_eq!(c.last().map(String::as_str), Some("-"));
        let c = codex_args(Some("m"), false, Path::new("/p"));
        assert!(c.windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(c.windows(2).any(|w| w == ["--model", "m"]));
    }
}
