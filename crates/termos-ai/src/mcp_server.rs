// SPDX-License-Identifier: GPL-3.0-or-later
//! TermOS's own MCP server (`termos mcp serve`, D-094): Claude Code, Codex
//! CLI and any other MCP client see and use TermOS's windows.
//!
//! It speaks MCP over stdio (JSON-RPC, one message per line) and reaches
//! TermOS through its control socket, as the pane the agent runs in
//! (`$TERMOS`, `$TERMOS_PANE`). The agent's CLI asks the user before each
//! call; on top of that, `[agents.termos-mcp]` denies tools and commands,
//! and every call is written to a tamper-evident audit log (arguments
//! redacted) and to the command center's history (tool names and
//! decisions only).

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use termos_core::config::agents::TermosMcpConfig;
use termos_core::journal::{self, Journal};

use crate::audit::{self, AuditLog, Event};
use crate::redact::Redactor;

/// MCP versions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The most a tool's text argument may hold.
const MAX_TEXT: usize = 64 * 1024;
/// The most lines `read_pane` returns.
const MAX_LINES: u64 = 2000;

/// A pane's last command (from `pane.read`) as text for the agent.
fn last_command_text(last: Option<&Value>) -> String {
    let Some(b) = last.filter(|v| !v.is_null()) else {
        return "(no command found: the pane's shell does not mark its commands)".into();
    };
    let mut out = format!(
        "$ {}\n",
        b.get("command").and_then(Value::as_str).unwrap_or_default()
    );
    for l in b
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&match (
        b.get("state").and_then(Value::as_str),
        b.get("exit").and_then(Value::as_i64),
    ) {
        (Some("running"), _) => "(still running)".to_owned(),
        (_, Some(code)) => format!("(exit status {code})"),
        _ => "(exit status unknown)".to_owned(),
    });
    out
}

/// The TermOS server, through its control API.
pub trait Control {
    fn call(&self, method: &str, params: Value) -> Result<Value, String>;
}

/// The control socket of a running TermOS.
pub struct Socket {
    path: PathBuf,
    rt: tokio::runtime::Runtime,
}

impl Socket {
    pub fn new(path: PathBuf) -> io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self { path, rt })
    }
}

impl Control for Socket {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let call = termos_core::transport::control_call(&self.path, method, Some(params));
        match self
            .rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), call).await })
        {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(termos_core::transport::ConnectError::Rpc(e))) => Err(e.message),
            Ok(Err(e)) => Err(format!(
                "cannot reach TermOS ({e}); is it running? Start it with `termos`"
            )),
            Err(_) => Err("TermOS did not answer within 10 seconds".into()),
        }
    }
}

/// Where the agent runs.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Its pane (`$TERMOS_PANE`).
    pub pane: Option<u64>,
    /// Its session (`$<id>` from `$TERMOS`).
    pub session: Option<String>,
    /// Its folder: relative paths are read from here.
    pub cwd: PathBuf,
}

/// One MCP connection.
pub struct Server {
    control: Box<dyn Control>,
    context: Context,
    rules: TermosMcpConfig,
    audit: Option<AuditLog>,
    journal: Option<Arc<Journal>>,
    redactor: Redactor,
    id: String,
    /// The client's name (`claude-code`, …), once it said hello.
    client: Option<String>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("id", &self.id)
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

type RpcResult = Result<Value, (i64, String)>;

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const PARSE_ERROR: i64 = -32700;

impl Server {
    /// A server for one connection; `state_dir` holds its audit log.
    pub fn new(
        control: Box<dyn Control>,
        context: Context,
        rules: TermosMcpConfig,
        state_dir: Option<&Path>,
        journal: Option<Arc<Journal>>,
    ) -> Self {
        let id = format!("mcp-{}", crate::history::new_id());
        let audit = state_dir.and_then(|d| match AuditLog::open(&audit::dir(d), &id) {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!("cannot open the audit log: {e}");
                None
            }
        });
        Self {
            control,
            context,
            rules,
            audit,
            journal,
            redactor: Redactor::new(),
            id,
            client: None,
        }
    }

    /// This connection's audit log and history id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Serves until the client closes `input`.
    pub fn serve(&mut self, input: &mut dyn BufRead, output: &mut dyn Write) -> io::Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                break;
            }
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            if let Some(reply) = self.handle_line(text) {
                output.write_all(reply.as_bytes())?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        self.end();
        Ok(())
    }

    /// One line from the client; the reply, if it needs one.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error(Value::Null, PARSE_ERROR, &e.to_string()).to_string()),
        };
        match msg {
            Value::Array(batch) => {
                let replies: Vec<Value> =
                    batch.into_iter().filter_map(|m| self.handle(m)).collect();
                (!replies.is_empty()).then(|| Value::Array(replies).to_string())
            }
            m => self.handle(m).map(|r| r.to_string()),
        }
    }

    fn handle(&mut self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
            // A reply to a request (this server sends none), or junk.
            let is_reply = msg.get("result").is_some() || msg.get("error").is_some();
            return match id {
                Some(id) if !is_reply => Some(error(id, INVALID_REQUEST, "no method")),
                _ => None,
            };
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (`notifications/initialized`, cancellations) need
        // no reply.
        let id = id?;
        Some(match self.request(&method, &params, &id) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error(id, code, &message),
        })
    }

    fn request(&mut self, method: &str, params: &Value, id: &Value) -> RpcResult {
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked
                    .filter(|v| PROTOCOL_VERSIONS.contains(v))
                    .unwrap_or(PROTOCOL_VERSIONS[0]);
                let client = params
                    .pointer("/clientInfo/name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                self.begin(client);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "termos", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.tool_list() })),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or((INVALID_PARAMS, "`name` is required".to_owned()))?;
                if !TOOLS.iter().any(|t| t.name == name) {
                    return Err((INVALID_PARAMS, format!("no tool `{name}`")));
                }
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Ok(self.call_tool(name, &args, id))
            }
            other => Err((METHOD_NOT_FOUND, format!("no method `{other}`"))),
        }
    }

    fn begin(&mut self, client: &str) {
        if self.client.is_some() {
            return;
        }
        let client: String = client
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c))
            .take(40)
            .collect();
        let client = if client.is_empty() {
            "unknown".to_owned()
        } else {
            client
        };
        self.audit(Event::Session {
            profile: "termos-mcp".into(),
            provider: client.clone(),
            model: String::new(),
            workspace: self.context.cwd.display().to_string(),
            sandbox: "the agent's own".into(),
        });
        if let Some(j) = &self.journal {
            j.record(journal::Event::Session {
                session: self.id.clone(),
                profile: "termos-mcp".into(),
                provider: client.clone(),
                model: String::new(),
                state: "started".into(),
            });
        }
        self.client = Some(client);
    }

    fn end(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.audit(Event::Note {
            text: "the connection ended".into(),
        });
        if let Some(j) = &self.journal {
            j.record(journal::Event::Session {
                session: self.id.clone(),
                profile: "termos-mcp".into(),
                provider: client,
                model: String::new(),
                state: "ended".into(),
            });
        }
    }

    fn audit(&mut self, event: Event) {
        if let (Some(j), Event::Tool { tool, decision, .. }) = (&self.journal, &event) {
            j.record(journal::Event::Tool {
                session: self.id.clone(),
                tool: tool.clone(),
                decision: decision.clone(),
            });
        }
        if let Some(a) = &mut self.audit
            && let Err(e) = a.append(event)
        {
            tracing::warn!("cannot write the audit log: {e}");
            self.audit = None;
        }
    }

    /// The tools agents may use (the denied ones are left out).
    fn tool_list(&self) -> Vec<Value> {
        TOOLS
            .iter()
            .filter(|t| !self.rules.deny.iter().any(|d| d == t.name))
            .map(|t| {
                json!({
                    "name": t.name,
                    "title": t.title,
                    "description": t.description,
                    "inputSchema": (t.schema)(),
                    "annotations": {
                        "title": t.title,
                        "readOnlyHint": t.read_only,
                        "destructiveHint": t.destructive,
                        "idempotentHint": t.read_only,
                        "openWorldHint": false,
                    },
                })
            })
            .collect()
    }

    fn call_tool(&mut self, name: &str, args: &Value, id: &Value) -> Value {
        if self.client.is_none() {
            self.begin("unknown");
        }
        let call_id = match id {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let audited_args = self.redactor.value(args);
        if self.rules.deny.iter().any(|d| d == name) {
            let why = format!("TermOS's settings do not allow `{name}` ([agents.termos-mcp] deny)");
            self.audit(Event::Tool {
                id: call_id,
                tool: name.to_owned(),
                args: audited_args,
                decision: "denied".into(),
                reason: "[agents.termos-mcp] deny".into(),
                result: None,
                ok: None,
            });
            return tool_result(&why, true);
        }
        if name == "run_command" {
            let command = args.get("command").and_then(Value::as_str).unwrap_or("");
            if !self.rules.command_allowed(command) {
                let why = format!(
                    "TermOS's settings do not allow the command `{}` ([agents.termos-mcp] deny-commands)",
                    command.split_whitespace().next().unwrap_or("")
                );
                self.audit(Event::Tool {
                    id: call_id,
                    tool: name.to_owned(),
                    args: audited_args,
                    decision: "denied".into(),
                    reason: "[agents.termos-mcp] deny-commands".into(),
                    result: None,
                    ok: None,
                });
                return tool_result(&why, true);
            }
        }
        let outcome = self.run(name, args);
        let (text, failed) = match &outcome {
            Ok(t) => (t.clone(), false),
            Err(e) => (e.clone(), true),
        };
        let summary: String = self
            .redactor
            .text(text.lines().next().unwrap_or(""))
            .chars()
            .take(120)
            .collect();
        self.audit(Event::Tool {
            id: call_id,
            tool: name.to_owned(),
            args: audited_args,
            decision: "allowed".into(),
            reason: "the agent's CLI asks before each call".into(),
            result: Some(summary),
            ok: Some(!failed),
        });
        tool_result(&text, failed)
    }

    fn with_session(&self, mut params: Value) -> Value {
        if let (Some(s), Value::Object(m)) = (&self.context.session, &mut params) {
            m.insert("session".into(), Value::from(s.clone()));
        }
        params
    }

    /// Runs a TermOS command as the agent's pane.
    fn command(&self, command: &str) -> Result<Value, String> {
        match self.context.pane {
            Some(pane) => self
                .control
                .call("pane.command", json!({ "command": command, "pane": pane })),
            None => self.control.call(
                "command.run",
                self.with_session(json!({ "command": command })),
            ),
        }
    }

    fn run(&self, name: &str, args: &Value) -> Result<String, String> {
        let text_arg = |key: &str| -> Result<String, String> {
            let t = args
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("`{key}` is required"))?;
            if t.len() > MAX_TEXT {
                return Err(format!("`{key}` is longer than {MAX_TEXT} bytes"));
            }
            Ok(t.to_owned())
        };
        let pane_arg = || -> Result<u64, String> {
            args.get("pane")
                .and_then(Value::as_u64)
                .ok_or_else(|| "`pane` (a pane id from list_panes) is required".to_owned())
        };
        match name {
            "list_panes" => {
                let mut panes = self.control.call("pane.list", json!({}))?;
                if let (Some(own), Some(list)) = (self.context.pane, panes.as_array_mut()) {
                    for p in list {
                        if p.get("id").and_then(Value::as_u64) == Some(own)
                            && let Value::Object(m) = p
                        {
                            m.insert("this".into(), Value::Bool(true));
                        }
                    }
                }
                serde_json::to_string_pretty(&panes).map_err(|e| e.to_string())
            }
            "read_pane" => {
                let pane = pane_arg()?;
                let lines = args
                    .get("lines")
                    .and_then(Value::as_u64)
                    .unwrap_or(100)
                    .clamp(1, MAX_LINES);
                let command = args
                    .get("last_command")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let got = self.control.call(
                    "pane.read",
                    json!({
                        "pane": pane,
                        "lines": lines,
                        "command": command,
                        // An agent reading its own pane: its newest
                        // command is still running.
                        "finished": Some(pane) == self.context.pane,
                    }),
                )?;
                if command {
                    return Ok(self
                        .redactor
                        .text(&last_command_text(got.get("last_command"))));
                }
                let text: Vec<&str> = got
                    .get("lines")
                    .and_then(Value::as_array)
                    .map(|l| l.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if text.is_empty() {
                    Ok(format!("(pane {pane} shows no text)"))
                } else {
                    // Keys a pane happens to show do not leave the machine.
                    Ok(self.redactor.text(&text.join("\n")))
                }
            }
            "send_to_pane" => {
                let pane = pane_arg()?;
                if Some(pane) == self.context.pane {
                    return Err("that is your own pane".into());
                }
                let mut text = text_arg("text")?;
                if args.get("enter").and_then(Value::as_bool).unwrap_or(false) {
                    text.push('\r');
                }
                self.control
                    .call("pane.send-text", json!({ "pane": pane, "text": text }))?;
                Ok(format!(
                    "typed {} characters into pane {pane}",
                    text.chars().count()
                ))
            }
            "open_pane" => {
                let argv: Vec<String> = args
                    .get("command")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                if argv.is_empty() {
                    return Err("`command` (the program and its arguments) is required".into());
                }
                let place = args.get("place").and_then(Value::as_str).unwrap_or("auto");
                let cwd = args
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map_or_else(|| self.context.cwd.clone(), |c| self.context.cwd.join(c));
                let mut params = json!({ "argv": argv, "where": place, "cwd": cwd });
                if let (Some(own), Value::Object(m)) = (self.context.pane, &mut params) {
                    m.insert("pane".into(), Value::from(own));
                }
                let got = self.control.call("pane.open", self.with_session(params))?;
                let id = got.get("pane").and_then(Value::as_u64).unwrap_or_default();
                Ok(format!("opened pane {id} running {}", argv.join(" ")))
            }
            "open_file" => {
                let path = self.context.cwd.join(text_arg("path")?);
                if !path.exists() {
                    return Err(format!("{} does not exist", path.display()));
                }
                self.command(&format!("file.open {}", path.display()))?;
                Ok(format!("opened {}", path.display()))
            }
            "run_command" => {
                let command = text_arg("command")?;
                self.command(&command)?;
                Ok(format!("ran `{command}`"))
            }
            "notify" => {
                let text: String = text_arg("text")?.chars().take(500).collect();
                let level = args.get("level").and_then(Value::as_str).unwrap_or("info");
                let who = self.client_title();
                self.control.call(
                    "notify.send",
                    self.with_session(json!({ "text": format!("{who}: {text}"), "level": level })),
                )?;
                Ok("posted".into())
            }
            "copy_to_clipboard" => {
                let text = text_arg("text")?;
                self.control
                    .call("clipboard.copy", self.with_session(json!({ "text": text })))?;
                Ok(format!("copied {} characters", text.chars().count()))
            }
            other => Err(format!("no tool `{other}`")),
        }
    }

    fn client_title(&self) -> String {
        let c = self.client.as_deref().unwrap_or("an agent");
        if c.contains("claude") {
            "Claude Code".into()
        } else if c.contains("codex") {
            "Codex".into()
        } else {
            c.to_owned()
        }
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

const INSTRUCTIONS: &str = "You run inside TermOS, a terminal desktop. These tools see and use \
its windows (panes): list them, read a pane's text, type into one, open programs and files, \
run TermOS commands, post notifications and copy to its clipboard. Your own pane is marked \
`\"this\": true` in list_panes.";

struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    read_only: bool,
    destructive: bool,
    schema: fn() -> Value,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "list_panes",
        title: "List TermOS windows",
        description: "Every TermOS pane (window): id, session, workspace, title, program kind, \
                      folder, size, whether it floats or has focus. Yours has \"this\": true.",
        read_only: true,
        destructive: false,
        schema: || json!({ "type": "object", "properties": {} }),
    },
    Tool {
        name: "read_pane",
        title: "Read a TermOS window",
        description: "The last lines of a terminal pane's text (its scrollback and screen), \
                      e.g. a build's or server's output. With last_command, only the last \
                      command run in its shell: the command line, its output and exit status. \
                      Keys and passwords it shows are masked.",
        read_only: true,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "pane": { "type": "integer", "description": "The pane's id (list_panes)" },
                    "lines": { "type": "integer", "minimum": 1, "maximum": MAX_LINES, "description": "How many lines, from the end (default 100)" },
                    "last_command": { "type": "boolean", "description": "Only the last command: its line, output and exit status (shells in TermOS mark their commands)" },
                },
                "required": ["pane"],
            })
        },
    },
    Tool {
        name: "send_to_pane",
        title: "Type into a TermOS window",
        description: "Types text into another terminal pane, as if pasted; with enter, presses \
                      Enter after it (runs a command in a shell).",
        read_only: false,
        destructive: true,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "pane": { "type": "integer", "description": "The pane's id (list_panes)" },
                    "text": { "type": "string" },
                    "enter": { "type": "boolean", "description": "Press Enter after the text" },
                },
                "required": ["pane", "text"],
            })
        },
    },
    Tool {
        name: "open_pane",
        title: "Open a TermOS window",
        description: "Runs a program in a new pane beside yours (a dev server, a watcher, a \
                      REPL), so the user sees it and you can read it with read_pane.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "The program and its arguments" },
                    "cwd": { "type": "string", "description": "Its folder (default: yours)" },
                    "place": { "type": "string", "enum": ["auto", "right", "down", "float"], "description": "Where (default auto)" },
                },
                "required": ["command"],
            })
        },
    },
    Tool {
        name: "open_file",
        title: "Show a file in TermOS",
        description: "Opens a file in TermOS's viewer for its type: images, Markdown, text.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            })
        },
    },
    Tool {
        name: "run_command",
        title: "Run a TermOS command",
        description: "Runs a TermOS command as from its palette, e.g. `layout.select tiled`, \
                      `monitor.open`, `pane.split-right`, `markdown.open README.md`, \
                      `task.test`. Some (quitting, closing) are not allowed.",
        read_only: false,
        destructive: true,
        schema: || {
            json!({
                "type": "object",
                "properties": { "command": { "type": "string", "description": "The command and its argument" } },
                "required": ["command"],
            })
        },
    },
    Tool {
        name: "notify",
        title: "Notify the user",
        description: "Posts a short notification on the TermOS desktop (e.g. a long task is done).",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "maxLength": 500 },
                    "level": { "type": "string", "enum": ["info", "warn", "error"] },
                },
                "required": ["text"],
            })
        },
    },
    Tool {
        name: "copy_to_clipboard",
        title: "Copy to the clipboard",
        description: "Puts text on TermOS's clipboard (and the terminal's), for the user to paste.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            })
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// TermOS, recorded: each call and a canned answer.
    #[derive(Clone, Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl Control for Fake {
        fn call(&self, method: &str, params: Value) -> Result<Value, String> {
            self.calls
                .lock()
                .expect("calls")
                .push((method.to_owned(), params.clone()));
            match method {
                "pane.list" => Ok(json!([
                    { "id": 1, "title": "zsh", "kind": "terminal" },
                    { "id": 2, "title": "claude", "kind": "terminal" },
                ])),
                "pane.read" => Ok(
                    json!({ "lines": ["$ cargo build", "export OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwx", "ok"] }),
                ),
                "pane.open" => Ok(json!({ "pane": 7 })),
                "pane.send-text" if params["pane"] == 99 => Err("no pane %99".into()),
                _ => Ok(json!({})),
            }
        }
    }

    fn server(dir: &Path, rules: TermosMcpConfig) -> (Server, Fake, Arc<Journal>) {
        let fake = Fake::default();
        let journal = Arc::new(Journal::new(dir, 7));
        let s = Server::new(
            Box::new(fake.clone()),
            Context {
                pane: Some(2),
                session: Some("$1".into()),
                cwd: dir.to_owned(),
            },
            rules,
            Some(dir),
            Some(journal.clone()),
        );
        (s, fake, journal)
    }

    fn send(s: &mut Server, msg: Value) -> Value {
        let reply = s.handle_line(&msg.to_string()).expect("a reply");
        serde_json::from_str(&reply).expect("JSON")
    }

    fn call(s: &mut Server, id: u64, tool: &str, args: Value) -> (String, bool) {
        let r = send(
            s,
            json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": tool, "arguments": args } }),
        );
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .expect("text")
            .to_owned();
        (text, r["result"]["isError"].as_bool().expect("isError"))
    }

    #[test]
    fn handshake_lists_tools_and_negotiates_the_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut s, _, _) = server(dir.path(), TermosMcpConfig::default());
        let r = send(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "9.9.9" } } }),
        );
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "termos");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        // A version this server does not know: its newest.
        let (mut other, _, _) = server(dir.path(), TermosMcpConfig::default());
        let r = send(
            &mut other,
            json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } }),
        );
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        // Notifications get no reply; unknown methods an error.
        assert!(
            s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        let r = send(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" }),
        );
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        let r = send(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }),
        );
        assert_eq!(r["result"], json!({}));
        let r: Value =
            serde_json::from_str(&s.handle_line("{not json").expect("reply")).expect("json");
        assert_eq!(r["error"]["code"], PARSE_ERROR);
        let r = send(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        );
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(
            names,
            termos_core::config::agents::TERMOS_MCP_TOOLS,
            "the config's list of tools is this server's"
        );
        assert_eq!(r["result"]["tools"][0]["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn tools_reach_termos_as_the_agents_pane() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut s, fake, journal) = server(dir.path(), TermosMcpConfig::default());
        send(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "clientInfo": { "name": "claude-code" } } }),
        );
        let (text, err) = call(&mut s, 1, "list_panes", json!({}));
        assert!(!err);
        let panes: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(panes[1]["this"], true, "{text}");
        assert!(panes[0].get("this").is_none());

        // Pane text is masked before it leaves.
        let (text, err) = call(&mut s, 2, "read_pane", json!({ "pane": 1, "lines": 5000 }));
        assert!(!err);
        assert!(
            text.contains("cargo build") && !text.contains("sk-proj-abc"),
            "{text}"
        );

        let (text, err) = call(
            &mut s,
            3,
            "send_to_pane",
            json!({ "pane": 1, "text": "make test", "enter": true }),
        );
        assert!(!err, "{text}");
        let (text, err) = call(&mut s, 4, "send_to_pane", json!({ "pane": 2, "text": "x" }));
        assert!(err && text.contains("your own pane"), "{text}");
        let (text, err) = call(
            &mut s,
            5,
            "send_to_pane",
            json!({ "pane": 99, "text": "x" }),
        );
        assert!(err && text.contains("no pane"), "{text}");
        let (_, err) = call(
            &mut s,
            6,
            "open_pane",
            json!({ "command": ["npm", "run", "dev"], "place": "right" }),
        );
        assert!(!err);
        let (text, err) = call(&mut s, 7, "open_pane", json!({}));
        assert!(err && text.contains("`command`"), "{text}");
        let (_, err) = call(
            &mut s,
            8,
            "run_command",
            json!({ "command": "layout.select tiled" }),
        );
        assert!(!err);
        let (text, err) = call(
            &mut s,
            9,
            "run_command",
            json!({ "command": "session.quit" }),
        );
        assert!(err && text.contains("deny-commands"), "{text}");
        let (_, err) = call(&mut s, 10, "notify", json!({ "text": "tests pass" }));
        assert!(!err);
        let (text, err) = call(&mut s, 11, "open_file", json!({ "path": "missing.md" }));
        assert!(err && text.contains("does not exist"), "{text}");

        let calls = fake.calls.lock().expect("calls").clone();
        let find = |m: &str| calls.iter().find(|(n, _)| n == m).map(|(_, p)| p.clone());
        assert_eq!(find("pane.read").expect("read")["lines"], MAX_LINES);
        assert_eq!(find("pane.send-text").expect("sent")["text"], "make test\r");
        let open = find("pane.open").expect("open");
        assert_eq!(
            (open["pane"].clone(), open["where"].clone()),
            (json!(2), json!("right"))
        );
        assert_eq!(find("pane.command").expect("command")["pane"], 2);
        assert_eq!(
            find("notify.send").expect("notify")["text"],
            "Claude Code: tests pass"
        );
        assert!(
            !calls.iter().any(|(_, p)| p["command"] == "session.quit"),
            "denied commands never reach TermOS"
        );

        // The audit log verifies and holds the calls; the history holds
        // names and decisions only.
        let id = s.id().to_owned();
        s.end();
        let log = audit::dir(dir.path()).join(format!("{id}.jsonl"));
        let checked = audit::verify(&log).expect("verifies");
        assert!(checked.entries >= 12, "{}", checked.entries);
        let text = std::fs::read_to_string(&log).expect("log");
        assert!(text.contains("\"decision\":\"denied\"") && text.contains("make test"));
        let history = journal.read(0);
        assert!(history.iter().any(|r| matches!(&r.event,
            journal::Event::Session { state, provider, .. } if state == "ended" && provider == "claude-code")));
        let tools = history
            .iter()
            .filter(|r| matches!(r.event, journal::Event::Tool { .. }))
            .count();
        assert_eq!(tools, 11);
        let raw = std::fs::read_to_string(
            std::fs::read_dir(journal.dir())
                .expect("dir")
                .flatten()
                .next()
                .expect("a day")
                .path(),
        )
        .expect("read");
        assert!(
            !raw.contains("make test") && !raw.contains("tests pass"),
            "{raw}"
        );
    }

    #[test]
    fn denied_tools_are_hidden_and_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rules = TermosMcpConfig {
            deny: vec!["read_pane".into(), "send_to_pane".into()],
            ..TermosMcpConfig::default()
        };
        let (mut s, fake, _) = server(dir.path(), rules);
        let r = send(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        );
        let tools = r["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), TOOLS.len() - 2);
        let (text, err) = call(&mut s, 2, "read_pane", json!({ "pane": 1 }));
        assert!(err && text.contains("do not allow `read_pane`"), "{text}");
        assert!(fake.calls.lock().expect("calls").is_empty());
        let r = send(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "rm_rf" } }),
        );
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn serves_lines_until_the_client_closes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut s, _, _) = server(dir.path(), TermosMcpConfig::default());
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","clientInfo":{"name":"codex-mcp-client"}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n\n",
            r#"[{"jsonrpc":"2.0","id":2,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/cancelled"}]"#,
            "\n",
        );
        let mut out = Vec::new();
        s.serve(&mut input.as_bytes(), &mut out).expect("serves");
        let out = String::from_utf8(out).expect("utf-8");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(lines[0].contains("\"protocolVersion\":\"2025-03-26\""));
        assert_eq!(lines[1], r#"[{"id":2,"jsonrpc":"2.0","result":{}}]"#);
    }
}
