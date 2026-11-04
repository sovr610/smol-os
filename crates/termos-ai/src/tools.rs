// SPDX-License-Identifier: GPL-3.0-or-later
//! The tools an agent can call (ARCHITECTURE.md §11).
//!
//! A call is first *prepared*: its arguments checked, the paths resolved
//! (symbolic links included), an edit computed so its diff can be shown,
//! and the [`Action`]s it amounts to listed for the permission engine.
//! Only an allowed or approved call *runs*. File tools run in-process
//! within the policy's paths; `run_command` runs in the sandbox on a PTY,
//! whose output the agent window shows as it comes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use termos_core::config::agents::SandboxMode;

use crate::mcp;
use crate::policy::{Action, Decision, Policy, SessionApprovals};
use crate::provider::ToolSpec;
use crate::redact::Redactor;
use crate::sandbox::{self, Capabilities};

/// Lines `read_file` returns unless asked for fewer.
const READ_LINES: usize = 2000;
/// Longest line `read_file` returns whole.
const LINE_CHARS: usize = 2000;
/// Largest file the file tools read.
const MAX_FILE: u64 = 8 * 1024 * 1024;
/// Most entries `list_dir` returns.
const LIST_ENTRIES: usize = 500;
/// Most matches `search_code` returns.
const SEARCH_MATCHES: usize = 200;
/// Most text a command's result keeps (its end).
const COMMAND_OUTPUT: usize = 30 * 1024;
/// Most bytes `fetch_url` reads, and returns as text.
const FETCH_BYTES: usize = 2 * 1024 * 1024;
const FETCH_TEXT: usize = 60 * 1024;

/// Where commands run.
#[derive(Debug, Clone)]
pub struct SandboxSetup {
    pub caps: Capabilities,
    /// The `termos` binary (the Landlock helper).
    pub helper: Option<PathBuf>,
    /// Hidden inside the sandbox.
    pub hidden: Vec<PathBuf>,
}

/// Everything a session's tools need.
pub struct Toolbox {
    pub policy: Policy,
    pub sandbox: SandboxSetup,
    pub redactor: Redactor,
    pub mcp: mcp::Hub,
    /// No one there to approve: what the session may do by itself.
    pub headless: Headless,
}

/// What a session with no one there to approve may do (D-098, D-100).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Headless {
    /// Someone is there: the profile's rules, asking when they say so.
    #[default]
    Off,
    /// Only read, list and search files (`termos agent ask`).
    ReadOnly,
    /// Also change files in the workspace, and run commands that match
    /// these patterns (and are not dangerous): a harness task.
    Edits { commands: Vec<String> },
}

impl Headless {
    pub fn is_on(&self) -> bool {
        *self != Self::Off
    }
}

/// The tools a read-only session is offered.
pub const READ_ONLY_TOOLS: &[&str] = &["read_file", "list_dir", "search_code"];
/// The tools an unattended session that may edit is offered.
pub const EDIT_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "search_code",
    "write_file",
    "edit_file",
    "run_command",
];

impl std::fmt::Debug for Toolbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Toolbox")
            .field("policy", &self.policy.profile)
            .finish_non_exhaustive()
    }
}

/// A prepared call.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: String,
    pub tool: String,
    pub args: Value,
    pub actions: Vec<Action>,
    pub preview: Preview,
    kind: Kind,
}

/// What the confirmation shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Preview {
    None,
    /// A unified diff of a file change.
    Diff {
        path: String,
        diff: String,
        created: bool,
    },
    Command {
        line: String,
        network: bool,
    },
    Fetch {
        url: String,
    },
    Mcp {
        server: String,
        tool: String,
        args: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Read {
        path: PathBuf,
        offset: usize,
        limit: usize,
    },
    List {
        path: PathBuf,
    },
    Search {
        pattern: String,
        path: PathBuf,
        glob: Option<String>,
        literal: bool,
    },
    /// Write `content`; `expect` is the hash of what the file held when
    /// the change was prepared (None: it did not exist).
    Write {
        path: PathBuf,
        content: String,
        expect: Option<String>,
    },
    Command {
        line: String,
        timeout: Duration,
        network: bool,
    },
    Fetch {
        url: String,
    },
    Mcp {
        server: String,
        tool: String,
    },
}

/// A tool's result for the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub text: String,
    pub is_error: bool,
}

impl Output {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// The built-in tools' descriptions and input schemas.
pub fn builtin_specs() -> Vec<ToolSpec> {
    let spec = |name: &str, description: &str, schema: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        schema,
    };
    vec![
        spec(
            "read_file",
            "Read a text file. Lines come numbered (`N\\t…`). Paths are relative to the workspace.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "description": "First line to read (1-based)"},
                "limit": {"type": "integer", "description": "Most lines to read (default 2000)"}
            }, "required": ["path"]}),
        ),
        spec(
            "list_dir",
            "List a folder's entries (folders end in /), with file sizes.",
            json!({"type": "object", "properties": {"path": {"type": "string", "description": "Default: the workspace"}}}),
        ),
        spec(
            "search_code",
            "Search file contents like ripgrep (respects .gitignore). Returns `path:line: text` matches.",
            json!({"type": "object", "properties": {
                "pattern": {"type": "string", "description": "A regular expression (or text with literal=true); smart case"},
                "path": {"type": "string", "description": "Folder or file to search (default: the workspace)"},
                "glob": {"type": "string", "description": "Only files matching this glob, e.g. *.rs"},
                "literal": {"type": "boolean"}
            }, "required": ["pattern"]}),
        ),
        spec(
            "write_file",
            "Create or replace a whole file. Prefer edit_file for changes to an existing file.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            }, "required": ["path", "content"]}),
        ),
        spec(
            "edit_file",
            "Replace exact text in a file. old_text must appear exactly once (or set replace_all). Include enough context to make it unique.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "old_text": {"type": "string"},
                "new_text": {"type": "string"},
                "replace_all": {"type": "boolean"}
            }, "required": ["path", "old_text", "new_text"]}),
        ),
        spec(
            "run_command",
            "Run a shell command in the workspace, in a sandbox: only the workspace is writable and there is no network unless you set network=true (which the user must approve). Returns the exit code and output.",
            json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "timeout_secs": {"type": "integer", "description": "Default 120, at most 1800"},
                "network": {"type": "boolean", "description": "The command needs the network (downloads, git fetch)"}
            }, "required": ["command"]}),
        ),
        spec(
            "fetch_url",
            "Fetch a web page or file over HTTP(S) and return its text (HTML tags removed). Redirects are reported, not followed.",
            json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}),
        ),
    ]
}

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{name}` must be a string"))
}

fn opt_str<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn opt_usize(args: &Value, name: &str) -> Option<usize> {
    args.get(name)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Toolbox {
    /// Tools offered to the model: the built-in ones and the MCP servers'.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = builtin_specs();
        match &self.headless {
            Headless::Off => {}
            Headless::ReadOnly => {
                specs.retain(|s| READ_ONLY_TOOLS.contains(&s.name.as_str()));
                return specs;
            }
            Headless::Edits { .. } => {
                specs.retain(|s| EDIT_TOOLS.contains(&s.name.as_str()));
                return specs;
            }
        }
        specs.extend(self.mcp.specs());
        specs
    }

    /// How a path is shown: relative inside the workspace.
    pub fn show(&self, path: &Path) -> String {
        path.strip_prefix(&self.policy.workspace)
            .map(|p| {
                if p.as_os_str().is_empty() {
                    ".".into()
                } else {
                    p.display().to_string()
                }
            })
            .unwrap_or_else(|_| path.display().to_string())
    }

    /// Checks a call's arguments and works out what it would do. An error
    /// goes back to the model without asking anyone.
    pub fn prepare(&self, id: &str, tool: &str, args: &Value) -> Result<Call, String> {
        let p = &self.policy;
        let (kind, actions, preview) = match tool {
            "read_file" => {
                let path = p.resolve(str_arg(args, "path")?);
                let offset = opt_usize(args, "offset").unwrap_or(1).max(1);
                let limit = opt_usize(args, "limit")
                    .unwrap_or(READ_LINES)
                    .clamp(1, READ_LINES);
                (
                    Kind::Read {
                        path: path.clone(),
                        offset,
                        limit,
                    },
                    vec![Action::Read(path)],
                    Preview::None,
                )
            }
            "list_dir" => {
                let path = p.resolve(opt_str(args, "path").unwrap_or("."));
                (
                    Kind::List { path: path.clone() },
                    vec![Action::Read(path)],
                    Preview::None,
                )
            }
            "search_code" => {
                let pattern = str_arg(args, "pattern")?.to_owned();
                if pattern.is_empty() {
                    return Err("`pattern` is empty".into());
                }
                let path = p.resolve(opt_str(args, "path").unwrap_or("."));
                let kind = Kind::Search {
                    pattern,
                    path: path.clone(),
                    glob: opt_str(args, "glob").map(str::to_owned),
                    literal: args
                        .get("literal")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                };
                (kind, vec![Action::Read(path)], Preview::None)
            }
            "write_file" => {
                let path = p.resolve(str_arg(args, "path")?);
                let content = str_arg(args, "content")?.to_owned();
                let old = read_existing(&path)?;
                let preview = diff_preview(&self.show(&path), old.as_deref(), &content);
                let expect = old.as_deref().map(|o| hash(o.as_bytes()));
                (
                    Kind::Write {
                        path: path.clone(),
                        content,
                        expect,
                    },
                    vec![Action::Write(path)],
                    preview,
                )
            }
            "edit_file" => {
                let path = p.resolve(str_arg(args, "path")?);
                let old_text = str_arg(args, "old_text")?;
                let new_text = str_arg(args, "new_text")?;
                let all = args
                    .get("replace_all")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let Some(old) = read_existing(&path)? else {
                    return Err(format!(
                        "{} does not exist (use write_file to create it)",
                        self.show(&path)
                    ));
                };
                if old_text.is_empty() {
                    return Err("`old_text` is empty".into());
                }
                let count = old.matches(old_text).count();
                let content = match (count, all) {
                    (0, _) => {
                        return Err(format!("`old_text` was not found in {}", self.show(&path)));
                    }
                    (1, _) | (_, true) => old.replace(old_text, new_text),
                    (n, false) => {
                        return Err(format!(
                            "`old_text` appears {n} times in {}: add context to make it unique, or set replace_all",
                            self.show(&path)
                        ));
                    }
                };
                let preview = diff_preview(&self.show(&path), Some(&old), &content);
                let expect = Some(hash(old.as_bytes()));
                (
                    Kind::Write {
                        path: path.clone(),
                        content,
                        expect,
                    },
                    vec![Action::Write(path)],
                    preview,
                )
            }
            "run_command" => {
                let line = str_arg(args, "command")?.trim().to_owned();
                if line.is_empty() {
                    return Err("`command` is empty".into());
                }
                let timeout = opt_usize(args, "timeout_secs")
                    .unwrap_or(120)
                    .clamp(1, 1800) as u64;
                let network = args
                    .get("network")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let preview = Preview::Command {
                    line: line.clone(),
                    network,
                };
                let action = Action::Command {
                    line: line.clone(),
                    network,
                };
                (
                    Kind::Command {
                        line,
                        timeout: Duration::from_secs(timeout),
                        network,
                    },
                    vec![action],
                    preview,
                )
            }
            "fetch_url" => {
                let url = str_arg(args, "url")?.trim().to_owned();
                let host =
                    url_host(&url).ok_or("`url` must be an http:// or https:// URL with a host")?;
                (
                    Kind::Fetch { url: url.clone() },
                    vec![Action::Fetch { host }],
                    Preview::Fetch { url },
                )
            }
            name if name.starts_with(mcp::PREFIX) => {
                let Some((server, t)) = self.mcp.lookup(name) else {
                    return Err(format!("no tool named {name}"));
                };
                let preview = Preview::Mcp {
                    server: server.to_owned(),
                    tool: t.to_owned(),
                    args: self.redactor.value(args).to_string(),
                };
                let action = Action::Mcp {
                    server: server.to_owned(),
                    tool: t.to_owned(),
                };
                (
                    Kind::Mcp {
                        server: server.to_owned(),
                        tool: t.to_owned(),
                    },
                    vec![action],
                    preview,
                )
            }
            other => return Err(format!("no tool named {other}")),
        };
        Ok(Call {
            id: id.to_owned(),
            tool: tool.to_owned(),
            args: args.clone(),
            actions,
            preview,
            kind,
        })
    }

    /// The permission engine's answer for every action of a call: a
    /// refusal wins, then a question (remembered only if every part may
    /// be), then permission.
    pub fn decide(&self, call: &Call, approvals: &SessionApprovals) -> Decision {
        let mut asks: Vec<(String, bool)> = Vec::new();
        let mut allowed = Vec::new();
        for a in &call.actions {
            let decision = match self.unattended(a, approvals) {
                Some(d) => d,
                None => self.policy.decide(a, approvals),
            };
            match decision {
                Decision::Deny(why) => return Decision::Deny(why),
                Decision::Ask { why, remember } => asks.push((why, remember)),
                Decision::Allow(why) => allowed.push(why),
            }
        }
        if asks.is_empty() {
            return Decision::Allow(allowed.join("; "));
        }
        Decision::Ask {
            why: asks
                .iter()
                .map(|(w, _)| w.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            remember: asks.iter().all(|(_, r)| *r),
        }
    }

    /// What a session with no one there decides about `a`, in place of
    /// the profile's questions: `None` when someone is there.
    fn unattended(&self, a: &Action, approvals: &SessionApprovals) -> Option<Decision> {
        let commands = match &self.headless {
            Headless::Off => return None,
            Headless::ReadOnly => None,
            Headless::Edits { commands } => Some(commands),
        };
        let nobody = |why: String| Decision::Deny(format!("{why}, and no one is there to ask"));
        let profile = self.policy.decide(a, approvals);
        if matches!(profile, Decision::Deny(_)) {
            return Some(profile);
        }
        Some(match (a, commands) {
            (Action::Read(_), _) => match profile {
                Decision::Ask { why, .. } => nobody(why),
                d => d,
            },
            (Action::Write(path), Some(_)) if self.policy.in_workspace(path) => {
                Decision::Allow("an unattended task may change files in its project".into())
            }
            (Action::Command { line, network }, Some(patterns)) => {
                let shape = crate::policy::classify(line);
                if *network {
                    nobody("it asks for the network".into())
                } else if let Some(why) = shape.dangerous.first() {
                    nobody(why.clone())
                } else if let Decision::Allow(why) = profile {
                    Decision::Allow(why)
                } else if !shape.substitution
                    && !shape.segments.is_empty()
                    && shape
                        .segments
                        .iter()
                        .all(|seg| patterns.iter().any(|p| crate::policy::glob_match(p, seg)))
                {
                    Decision::Allow("an allowed command for unattended tasks".into())
                } else {
                    nobody("it is not one of the commands allowed for unattended tasks".into())
                }
            }
            (_, Some(_)) => nobody("an unattended task may only work in its project".into()),
            (_, None) => {
                Decision::Deny("this session only reads files (a headless ask, D-098)".into())
            }
        })
    }

    /// Runs a prepared, permitted call. `on_output` sees a command's
    /// output as it arrives.
    pub async fn run(&self, call: &Call, on_output: &mut (dyn FnMut(&[u8]) + Send)) -> Output {
        match &call.kind {
            Kind::Read {
                path,
                offset,
                limit,
            } => read_file(&self.show(path), path, *offset, *limit),
            Kind::List { path } => list_dir(path),
            Kind::Search {
                pattern,
                path,
                glob,
                literal,
            } => {
                let (pattern, path, glob, literal, root) = (
                    pattern.clone(),
                    path.clone(),
                    glob.clone(),
                    *literal,
                    self.policy.workspace.clone(),
                );
                tokio::task::spawn_blocking(move || {
                    search(&pattern, &path, glob.as_deref(), literal, &root)
                })
                .await
                .unwrap_or_else(|e| Output::err(format!("the search failed: {e}")))
            }
            Kind::Write {
                path,
                content,
                expect,
            } => {
                let now = read_existing(path)
                    .ok()
                    .flatten()
                    .map(|o| hash(o.as_bytes()));
                if now != *expect {
                    return Output::err(format!(
                        "{} changed since this edit was prepared; read it again",
                        self.show(path)
                    ));
                }
                write_file(&self.show(path), path, content)
            }
            Kind::Command {
                line,
                timeout,
                network,
            } => self.run_command(line, *timeout, *network, on_output).await,
            Kind::Fetch { url } => fetch(url).await,
            Kind::Mcp { server, tool } => {
                let (text, is_error) = self.mcp.call(server, tool, &call.args).await;
                Output {
                    text: truncate_end(&text, FETCH_TEXT),
                    is_error,
                }
            }
        }
    }

    async fn run_command(
        &self,
        line: &str,
        timeout: Duration,
        network: bool,
        on_output: &mut (dyn FnMut(&[u8]) + Send),
    ) -> Output {
        let (argv, env) = shell_command(line);
        let spec = sandbox::Spec {
            workspace: self.policy.workspace.clone(),
            writable: self.policy.write_paths.clone(),
            hidden: self.sandbox.hidden.clone(),
            network,
            cwd: self.policy.workspace.clone(),
            keep_env: false,
        };
        let mode = self.policy.sandbox;
        let argv = match sandbox::wrap(
            &spec,
            &self.sandbox.caps,
            mode,
            self.sandbox.helper.as_deref(),
            &argv,
        ) {
            Ok(a) => a,
            Err(e) => return Output::err(e.to_string()),
        };
        run_on_pty(
            &argv,
            env,
            &self.policy.workspace,
            timeout,
            on_output,
            &self.redactor,
        )
        .await
    }
}

/// The environment variable that carries a command line to `cmd.exe`.
const WINDOWS_COMMAND_VAR: &str = "TERMOS_COMMAND";

/// How to run `line` in the platform's shell: the program and its
/// arguments, and extra environment variables.
///
/// On Windows, an argument can only reach a program through the
/// C runtime's quoting (`\"`), which `cmd.exe` does not read: a line with
/// quotes would arrive mangled. So `cmd.exe` gets `%TERMOS_COMMAND%`
/// instead, which it expands to a second `cmd.exe /D /S /C "<line>"` (see
/// [`windows_inner_command`]); that one reads the line exactly as typed,
/// `%VARIABLES%`, `&&`, pipes and quotes included.
pub fn shell_command(line: &str) -> (Vec<String>, Vec<(String, String)>) {
    if cfg!(windows) {
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
        (
            vec![
                comspec.clone(),
                "/D".into(),
                "/S".into(),
                "/C".into(),
                format!("%{WINDOWS_COMMAND_VAR}%"),
            ],
            vec![(
                WINDOWS_COMMAND_VAR.into(),
                windows_inner_command(&comspec, line),
            )],
        )
    } else {
        (vec!["/bin/sh".into(), "-c".into(), line.into()], Vec::new())
    }
}

/// What the outer `cmd.exe` runs: `"<comspec>" /D /S /C "<line>"`. The
/// outer one reads the line inside the quotes around it, so the parts
/// inside the line's own quotes are outside quotes for it: there, the
/// characters it would act on (`& | < > ^ ( )`) are escaped with `^`, and
/// it passes the line on unchanged. With `/S`, the inner one drops only
/// the quotes around the line.
fn windows_inner_command(comspec: &str, line: &str) -> String {
    let mut out = format!("\"{comspec}\" /D /S /C \"");
    // The quote just written is open.
    let mut quoted = true;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            '&' | '|' | '<' | '>' | '^' | '(' | ')' if !quoted => out.push('^'),
            _ => {}
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Runs `argv` on a PTY, feeding its output to `on_output`; the result
/// is its exit code and the end of its output as plain text.
pub async fn run_on_pty(
    argv: &[String],
    extra_env: Vec<(String, String)>,
    cwd: &Path,
    timeout: Duration,
    on_output: &mut (dyn FnMut(&[u8]) + Send),
    redactor: &Redactor,
) -> Output {
    use termos_core::ids::PaneId;
    use termos_term::{AlacrittyVt, PtyEvent, PtyPane, SpawnSpec, VtBackend, VtEvent};
    let (tx, mut rx) = tokio::sync::mpsc::channel::<PtyEvent>(64);
    let spec = SpawnSpec {
        program: argv[0].clone(),
        args: argv[1..].to_vec(),
        cwd: Some(cwd.to_owned()),
        env: [
            ("TERM".into(), "xterm-256color".into()),
            ("TERMOS_SANDBOX".into(), "1".into()),
        ]
        .into_iter()
        .chain(extra_env)
        .collect(),
        env_remove: sandbox::secret_env_names(std::env::vars()),
        cols: 120,
        rows: 30,
    };
    let mut pty = match PtyPane::spawn(PaneId(u64::MAX), &spec, tx) {
        Ok(p) => p,
        Err(e) => return Output::err(format!("cannot start the command: {e}")),
    };
    // Programs (and Windows' ConPTY itself) ask the terminal questions
    // (device attributes, the cursor position) and may wait for the
    // answers: an emulator answers them, as it does for panes.
    let mut answers = AlacrittyVt::new(spec.cols, spec.rows, 0);
    let mut raw: Vec<u8> = Vec::new();
    let mut dropped = false;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let code = loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Some(PtyEvent::Output(_, bytes)) => {
                    answers.advance(&bytes);
                    for ev in answers.take_events() {
                        if let VtEvent::Reply(reply) = ev {
                            pty.write(reply);
                        }
                    }
                    on_output(&bytes);
                    raw.extend_from_slice(&bytes);
                    // Keep the end: four times what the model gets, as raw
                    // bytes with escapes in them.
                    if raw.len() > COMMAND_OUTPUT * 8 {
                        let cut = raw.len() - COMMAND_OUTPUT * 4;
                        raw.drain(..cut);
                        dropped = true;
                    }
                }
                Some(PtyEvent::Exited(_, code)) => break Some(code),
                Some(PtyEvent::Wake(_)) => {}
                None => break None,
            },
            () = &mut deadline => {
                pty.kill();
                let text = plain_text(&raw);
                return Output::err(redactor.text(&format!(
                    "timed out after {}s and was stopped. Output so far:\n{}",
                    timeout.as_secs(),
                    truncate_end(&text, COMMAND_OUTPUT)
                )));
            }
        }
    };
    let text = plain_text(&raw);
    let mut text = truncate_end(&text, COMMAND_OUTPUT);
    if dropped && !text.starts_with('[') {
        text = format!("[earlier output left out]\n{text}");
    }
    let status = match code.flatten() {
        Some(0) => "exit code 0".to_owned(),
        Some(c) => format!("exit code {c}"),
        None => "exit code unknown".to_owned(),
    };
    let failed = !matches!(code.flatten(), Some(0));
    let body = if text.trim().is_empty() {
        "(no output)".to_owned()
    } else {
        text
    };
    Output {
        text: redactor.text(&format!("{status}\n{body}")),
        is_error: failed,
    }
}

/// Terminal output as plain text: escape sequences removed, carriage
/// returns overwrite the line (progress bars), backspaces erase.
pub fn plain_text(bytes: &[u8]) -> String {
    #[derive(PartialEq)]
    enum St {
        Text,
        Esc,
        Csi,
        /// OSC, DCS, APC, PM, SOS: until BEL or ST.
        Str,
        StrEsc,
    }
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut st = St::Text;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match st {
            St::Text => match c {
                '\x1b' => st = St::Esc,
                '\n' => lines.push(std::mem::take(&mut line)),
                '\r' => {
                    if chars.peek() != Some(&'\n') {
                        line.clear();
                    }
                }
                '\x08' => {
                    line.pop();
                }
                '\t' => line.push('\t'),
                c if c.is_control() => {}
                c => line.push(c),
            },
            St::Esc => {
                st = match c {
                    '[' => St::Csi,
                    ']' | 'P' | '_' | '^' | 'X' => St::Str,
                    _ => St::Text,
                }
            }
            St::Csi => {
                if ('\x40'..='\x7e').contains(&c) {
                    st = St::Text;
                }
            }
            St::Str => match c {
                '\x07' => st = St::Text,
                '\x1b' => st = St::StrEsc,
                _ => {}
            },
            St::StrEsc => st = if c == '\\' { St::Text } else { St::Str },
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let mut out = lines
        .iter()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// The end of `text`, at most `max` bytes, cut at a line.
fn truncate_end(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let start = text[start..].find('\n').map_or(start, |n| start + n + 1);
    let left_out = text[..start].lines().count();
    format!("[{left_out} earlier lines left out]\n{}", &text[start..])
}

fn read_existing(path: &Path) -> Result<Option<String>, String> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(m) if m.is_dir() => Err(format!("{} is a folder", path.display())),
        Ok(m) if m.len() > MAX_FILE => Err(format!(
            "{} is larger than {} MiB",
            path.display(),
            MAX_FILE >> 20
        )),
        Ok(_) => {
            let bytes =
                std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            if bytes.iter().take(8192).any(|&b| b == 0) {
                return Err(format!("{} is a binary file", path.display()));
            }
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| format!("{} is not UTF-8 text", path.display()))
        }
    }
}

fn read_file(shown: &str, path: &Path, offset: usize, limit: usize) -> Output {
    let text = match read_existing(path) {
        Ok(Some(t)) => t,
        Ok(None) => return Output::err(format!("{shown} does not exist")),
        Err(e) => return Output::err(e),
    };
    let total = text.lines().count();
    if total == 0 {
        return Output::ok(format!("{shown} is empty"));
    }
    let mut out = String::new();
    for (i, l) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let l = if l.chars().count() > LINE_CHARS {
            let cut: String = l.chars().take(LINE_CHARS).collect();
            format!("{cut}… [line cut at {LINE_CHARS} characters]")
        } else {
            l.to_owned()
        };
        out.push_str(&format!("{}\t{l}\n", i + 1));
    }
    let last = (offset - 1 + limit).min(total);
    if offset > total {
        return Output::err(format!("{shown} has only {total} lines"));
    }
    if last < total || offset > 1 {
        out.push_str(&format!("[lines {offset}-{last} of {total}]\n"));
    }
    Output::ok(out)
}

fn write_file(shown: &str, path: &Path, content: &str) -> Output {
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        return Output::err(format!("cannot create {}: {e}", dir.display()));
    }
    match std::fs::write(path, content) {
        Ok(()) => Output::ok(format!("wrote {shown} ({} lines)", content.lines().count())),
        Err(e) => Output::err(format!("cannot write {shown}: {e}")),
    }
}

fn list_dir(path: &Path) -> Output {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => return Output::err(format!("cannot list {}: {e}", path.display())),
    };
    let mut items: Vec<(bool, String, u64)> = entries
        .filter_map(Result::ok)
        .map(|e| {
            let meta = e.metadata().ok();
            let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
            (
                is_dir,
                e.file_name().to_string_lossy().into_owned(),
                meta.map_or(0, |m| m.len()),
            )
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = items.len();
    let mut out: Vec<String> = items
        .iter()
        .take(LIST_ENTRIES)
        .map(|(dir, name, size)| {
            if *dir {
                format!("{name}/")
            } else {
                format!("{name}  ({size} bytes)")
            }
        })
        .collect();
    if total > LIST_ENTRIES {
        out.push(format!("[{} more entries]", total - LIST_ENTRIES));
    }
    if out.is_empty() {
        out.push("(empty)".into());
    }
    Output::ok(out.join("\n"))
}

fn search(pattern: &str, path: &Path, glob: Option<&str>, literal: bool, root: &Path) -> Output {
    use grep::regex::RegexMatcherBuilder;
    use grep::searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
    let pattern = if literal {
        regex::escape(pattern)
    } else {
        pattern.to_owned()
    };
    let matcher = match RegexMatcherBuilder::new().case_smart(true).build(&pattern) {
        Ok(m) => m,
        Err(e) => return Output::err(format!("bad pattern: {e}")),
    };
    let mut walk = ignore::WalkBuilder::new(path);
    walk.require_git(false);
    if let Some(g) = glob {
        let mut ov = ignore::overrides::OverrideBuilder::new(path);
        if let Err(e) = ov.add(g) {
            return Output::err(format!("bad glob: {e}"));
        }
        match ov.build() {
            Ok(o) => {
                walk.overrides(o);
            }
            Err(e) => return Output::err(format!("bad glob: {e}")),
        }
    }
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .build();
    let mut out = Vec::new();
    let mut more = false;
    for entry in walk.build().filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let shown = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        let _ = searcher.search_path(
            &matcher,
            entry.path(),
            UTF8(|line, text| {
                if out.len() >= SEARCH_MATCHES {
                    more = true;
                    return Ok(false);
                }
                let text: String = text.trim_end().chars().take(300).collect();
                out.push(format!("{shown}:{line}: {text}"));
                Ok(true)
            }),
        );
        if more {
            break;
        }
    }
    if out.is_empty() {
        return Output::ok("no matches");
    }
    if more {
        out.push(format!(
            "[stopped at {SEARCH_MATCHES} matches: narrow the search]"
        ));
    }
    Output::ok(out.join("\n"))
}

/// The host of an http(s) URL.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        host_port.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

async fn fetch(url: &str) -> Output {
    use futures_util::StreamExt as _;
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    if url.starts_with("https://") && !crate::provider::http::tls_available() {
        return Output::err("this build of TermOS has no TLS");
    }
    // The shared client installs the TLS provider; this one does not
    // follow redirects, so each new host goes through the permissions.
    if let Err(e) = crate::provider::http::client() {
        return Output::err(e.message);
    }
    let client = match CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("termos/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(0)
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())
    }) {
        Ok(c) => c,
        Err(e) => return Output::err(e.clone()),
    };
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => return Output::err(crate::provider::http::network(&e).message),
    };
    let status = resp.status();
    if status.is_redirection() {
        let to = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?");
        return Output::ok(format!(
            "HTTP {}: redirected to {to} (fetch that URL to follow)",
            status.as_u16()
        ));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    let mut cut = false;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(c) => {
                body.extend_from_slice(&c);
                if body.len() > FETCH_BYTES {
                    body.truncate(FETCH_BYTES);
                    cut = true;
                    break;
                }
            }
            Err(e) => return Output::err(crate::provider::http::network(&e).message),
        }
    }
    let texty = content_type.is_empty()
        || content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript");
    if !texty || body.iter().take(4096).any(|&b| b == 0) {
        return Output::ok(format!(
            "HTTP {}: {} bytes of {content_type}, not text",
            status.as_u16(),
            body.len()
        ));
    }
    let raw = String::from_utf8_lossy(&body);
    let text = if content_type.contains("html") {
        html_text(&raw)
    } else {
        raw.into_owned()
    };
    let mut text = if text.len() > FETCH_TEXT {
        let mut end = FETCH_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n[cut at {} KiB]", &text[..end], FETCH_TEXT / 1024)
    } else {
        text
    };
    if cut {
        text.push_str("\n[download stopped at 2 MiB]");
    }
    let out = format!("HTTP {}\n{text}", status.as_u16());
    if status.is_success() {
        Output::ok(out)
    } else {
        Output::err(out)
    }
}

/// HTML's visible text: tags, scripts and styles removed, common
/// entities decoded, blank lines collapsed.
pub fn html_text(html: &str) -> String {
    let mut out = String::new();
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i..];
            let skip_to = ["script", "style", "noscript", "template"]
                .iter()
                .find(|t| rest.starts_with(&format!("<{t}")))
                .and_then(|t| rest.find(&format!("</{t}")).map(|e| i + e));
            let from = skip_to.unwrap_or(i);
            match html[from..].find('>') {
                Some(end) => {
                    let tag = &lower[i..from + end];
                    if [
                        "<p", "<br", "<div", "<li", "<h1", "<h2", "<h3", "<h4", "<tr", "</p",
                        "</div", "</li", "<pre", "</pre",
                    ]
                    .iter()
                    .any(|t| tag.starts_with(t))
                    {
                        out.push('\n');
                    }
                    i = from + end + 1;
                }
                None => break,
            }
        } else {
            let next = html[i..].find('<').map_or(html.len(), |n| i + n);
            out.push_str(&html[i..next]);
            i = next;
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let mut lines: Vec<String> = Vec::new();
    for l in decoded.lines() {
        let l = l.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() && lines.last().is_some_and(String::is_empty) {
            continue;
        }
        lines.push(l);
    }
    lines.join("\n").trim().to_owned()
}

/// A unified diff for the confirmation.
fn diff_preview(shown: &str, old: Option<&str>, new: &str) -> Preview {
    let diff = similar::TextDiff::from_lines(old.unwrap_or(""), new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{shown}"), &format!("b/{shown}"))
        .to_string();
    Preview::Diff {
        path: shown.to_owned(),
        diff,
        created: old.is_none(),
    }
}

/// What the agent window shows about a sandbox for this toolbox.
pub fn sandbox_line(setup: &SandboxSetup, mode: SandboxMode) -> String {
    setup.caps.describe(mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Confinement;
    use termos_core::config::agents::ProfileConfig;

    fn toolbox(ws: &Path) -> Toolbox {
        Toolbox {
            policy: Policy::new(
                "t",
                &ProfileConfig::default(),
                ws,
                None,
                Vec::new(),
                Confinement::Full,
            ),
            sandbox: SandboxSetup {
                caps: Capabilities::none(),
                helper: None,
                hidden: Vec::new(),
            },
            redactor: Redactor::new(),
            mcp: mcp::Hub::default(),
            headless: Headless::Off,
        }
    }

    #[test]
    fn a_read_only_toolbox_only_reads_and_never_asks() {
        let (dir, mut tb) = ws();
        tb.headless = Headless::ReadOnly;
        let names: Vec<String> = tb.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, READ_ONLY_TOOLS);
        let approvals = SessionApprovals::default();
        let decide = |tool: &str, args: Value| {
            let call = tb.prepare("id", tool, &args).expect("prepared");
            tb.decide(&call, &approvals)
        };
        assert!(matches!(
            decide("read_file", json!({"path": "src/lib.rs"})),
            Decision::Allow(_)
        ));
        // Writing and commands, even when offered by name.
        for (tool, args) in [
            ("write_file", json!({"path": "x", "content": "y"})),
            ("run_command", json!({"command": "ls"})),
            ("fetch_url", json!({"url": "https://example.com/"})),
        ] {
            assert!(matches!(decide(tool, args), Decision::Deny(_)), "{tool}");
        }
        // Reading outside the workspace would ask: refused instead.
        let outside = dir.path().parent().expect("parent").join("elsewhere.txt");
        match decide("read_file", json!({"path": outside.display().to_string()})) {
            Decision::Deny(why) => assert!(why.contains("no one is there to ask"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unattended_task_edits_its_project_and_runs_allowed_commands() {
        let (dir, mut tb) = ws();
        tb.headless = Headless::Edits {
            commands: vec!["cargo test*".into(), "make".into()],
        };
        let names: Vec<String> = tb.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, EDIT_TOOLS);
        let approvals = SessionApprovals::default();
        let decide = |tool: &str, args: Value| {
            let call = tb.prepare("id", tool, &args).expect("prepared");
            tb.decide(&call, &approvals)
        };
        let allowed = |d: Decision| matches!(d, Decision::Allow(_));
        assert!(allowed(decide(
            "write_file",
            json!({"path": "src/new.rs", "content": "x"})
        )));
        assert!(allowed(decide(
            "edit_file",
            json!({"path": "src/lib.rs", "old_text": "a - b", "new_text": "a + b"})
        )));
        assert!(allowed(decide(
            "run_command",
            json!({"command": "cargo test --all"})
        )));
        assert!(allowed(decide(
            "run_command",
            json!({"command": "make && cargo test"})
        )));
        let outside = dir.path().parent().expect("parent").join("elsewhere.txt");
        for (tool, args) in [
            (
                "write_file",
                json!({"path": outside.display().to_string(), "content": "x"}),
            ),
            (
                "run_command",
                json!({"command": "curl https://example.com"}),
            ),
            ("run_command", json!({"command": "cargo test; rm -rf ~"})),
            (
                "run_command",
                json!({"command": "cargo test --all", "network": true}),
            ),
            ("run_command", json!({"command": "make $(cat x)"})),
            ("fetch_url", json!({"url": "https://example.com/"})),
        ] {
            assert!(
                matches!(decide(tool, args.clone()), Decision::Deny(_)),
                "{tool} {args}"
            );
        }
    }

    fn ws() -> (tempfile::TempDir, Toolbox) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("src");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
        )
        .expect("file");
        let tb = toolbox(dir.path());
        (dir, tb)
    }

    async fn run(tb: &Toolbox, tool: &str, args: Value) -> Output {
        let call = tb.prepare("id", tool, &args).expect("prepared");
        tb.run(&call, &mut |_| {}).await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_list_search() {
        let (_dir, tb) = ws();
        let out = run(&tb, "read_file", json!({"path": "src/lib.rs"})).await;
        assert_eq!(
            out.text,
            "1\tfn add(a: i32, b: i32) -> i32 {\n2\t    a - b\n3\t}\n"
        );
        let out = run(
            &tb,
            "read_file",
            json!({"path": "src/lib.rs", "offset": 2, "limit": 1}),
        )
        .await;
        assert_eq!(out.text, "2\t    a - b\n[lines 2-2 of 3]\n");
        let out = run(&tb, "list_dir", json!({})).await;
        assert_eq!(out.text, "src/");
        let out = run(&tb, "search_code", json!({"pattern": "a - b"})).await;
        assert_eq!(
            out.text,
            format!("src{}lib.rs:2:     a - b", std::path::MAIN_SEPARATOR)
        );
        let out = run(&tb, "search_code", json!({"pattern": "(", "literal": true})).await;
        assert!(out.text.contains("lib.rs:1:"), "{}", out.text);
        assert!(tb.prepare("x", "read_file", &json!({})).is_err());
        assert!(tb.prepare("x", "nope", &json!({})).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn edits_show_a_diff_and_refuse_stale_files() {
        let (dir, tb) = ws();
        let call = tb
            .prepare(
                "e",
                "edit_file",
                &json!({"path": "src/lib.rs", "old_text": "a - b", "new_text": "a + b"}),
            )
            .expect("prepared");
        let Preview::Diff { diff, created, .. } = &call.preview else {
            unreachable!("a diff")
        };
        assert!(!created);
        assert!(diff.contains("-    a - b\n+    a + b\n"), "{diff}");
        assert_eq!(
            tb.decide(&call, &SessionApprovals::default()),
            Decision::Ask {
                why: "the profile asks before write".into(),
                remember: true,
            }
        );
        let out = tb.run(&call, &mut |_| {}).await;
        assert!(!out.is_error, "{}", out.text);
        assert!(
            std::fs::read_to_string(dir.path().join("src/lib.rs"))
                .expect("read")
                .contains("a + b")
        );
        // The same prepared edit again: the file changed under it.
        assert!(tb.run(&call, &mut |_| {}).await.is_error);
        // Ambiguous and missing text are refused before asking.
        std::fs::write(dir.path().join("twice.txt"), "x\nx\n").expect("write");
        assert!(
            tb.prepare(
                "e",
                "edit_file",
                &json!({"path": "twice.txt", "old_text": "x", "new_text": "y"})
            )
            .is_err()
        );
        assert!(
            tb.prepare(
                "e",
                "edit_file",
                &json!({"path": "twice.txt", "old_text": "z", "new_text": "y"})
            )
            .is_err()
        );
        let out = run(
            &tb,
            "write_file",
            json!({"path": "new/dir/f.txt", "content": "hi\n"}),
        )
        .await;
        assert_eq!(
            out.text,
            format!(
                "wrote new{0}dir{0}f.txt (1 lines)",
                std::path::MAIN_SEPARATOR
            )
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn commands_run_on_a_pty() {
        let (_dir, tb) = ws();
        let mut seen = Vec::new();
        let call = tb.prepare("c", "run_command", &json!({"command": "printf 'a\\033[31mb\\033[0m\\n'; test -t 1 && echo tty; exit 3"})).expect("prepared");
        let out = tb.run(&call, &mut |b| seen.extend_from_slice(b)).await;
        assert!(out.is_error);
        assert_eq!(out.text, "exit code 3\nab\ntty");
        assert!(String::from_utf8_lossy(&seen).contains("\x1b[31m"));
        let call = tb
            .prepare(
                "c",
                "run_command",
                &json!({"command": "sleep 5", "timeout_secs": 1}),
            )
            .expect("prepared");
        let out = tb.run(&call, &mut |_| {}).await;
        assert!(
            out.is_error && out.text.starts_with("timed out after 1s"),
            "{}",
            out.text
        );
    }

    #[test]
    fn windows_command_lines_survive_two_cmd_passes() {
        let c = "C:\\Windows\\system32\\cmd.exe";
        assert_eq!(
            windows_inner_command(c, r#"findstr /C:"a + b" src\lib.rs"#),
            r#""C:\Windows\system32\cmd.exe" /D /S /C "findstr /C:"a + b" src\lib.rs""#
        );
        // The outer pass reads the line inside the quotes around it: only
        // what is inside the line's own quotes is outside quotes for it,
        // and only there are its operators escaped.
        assert_eq!(
            windows_inner_command(c, r#"cd sub && echo "x & y" | more > out.txt"#),
            r#""C:\Windows\system32\cmd.exe" /D /S /C "cd sub && echo "x ^& y" | more > out.txt""#
        );
        assert_eq!(
            windows_inner_command(c, r#"echo "(a) ^ <b>""#),
            r#""C:\Windows\system32\cmd.exe" /D /S /C "echo "^(a^) ^^ ^<b^>"""#
        );
        let (argv, env) = shell_command("echo hi");
        if cfg!(windows) {
            assert_eq!(argv[1..], ["/D", "/S", "/C", "%TERMOS_COMMAND%"]);
            assert_eq!(env[0].0, "TERMOS_COMMAND");
        } else {
            assert_eq!(argv, ["/bin/sh", "-c", "echo hi"]);
            assert!(env.is_empty());
        }
    }

    /// The same lines through the real `cmd.exe`.
    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    async fn cmd_reads_command_lines_as_typed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("f.txt"), "x = a + b\r\n").expect("write");
        let cases = [
            (r#"findstr /C:"a + b" f.txt"#, "x = a + b", "exit code 0"),
            ("echo %OS%", "Windows_NT", "exit code 0"),
            ("cd sub && cd", "sub", "exit code 0"),
            ("echo one| findstr one", "one", "exit code 0"),
            (
                r#""%ComSpec%" /D /C echo quoted program"#,
                "quoted program",
                "exit code 0",
            ),
            (r#"echo "a & b" & exit /b 3"#, "\"a & b\"", "exit code 3"),
        ];
        for (line, want, status) in cases {
            let (argv, env) = shell_command(line);
            let out = run_on_pty(
                &argv,
                env,
                dir.path(),
                Duration::from_secs(30),
                &mut |_| {},
                &Redactor::new(),
            )
            .await;
            assert!(out.text.starts_with(status), "{line}: {}", out.text);
            assert!(out.text.contains(want), "{line}: {}", out.text);
        }
    }

    #[test]
    fn plain_text_and_html() {
        assert_eq!(
            plain_text(b"\x1b[1mbold\x1b[0m\r\n50%\r100%\ndone\x1b]0;title\x07!"),
            "bold\n100%\ndone!"
        );
        assert_eq!(plain_text(b"ab\x08c"), "ac");
        assert_eq!(
            html_text(
                "<html><head><style>p{}</style><script>x()</script></head><body><h1>Title</h1><p>a &amp; b</p><p>c</p></body></html>"
            ),
            "Title\na & b\n\nc"
        );
        assert_eq!(
            url_host("https://user@Docs.RS:443/x?y"),
            Some("docs.rs".into())
        );
        assert_eq!(url_host("http://[::1]:8080/"), Some("::1".into()));
        assert_eq!(url_host("ftp://x"), None);
        assert_eq!(
            truncate_end("a\nb\nc\nd", 3),
            "[3 earlier lines left out]\nd"
        );
    }
}
