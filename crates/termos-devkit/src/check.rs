// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos lang check <lang>`: shows that a language works on this machine
//! from start to finish (the M5a acceptance; the language CI runs it,
//! D-056).
//!
//! 1. **install**: as `termos lang install` does.
//! 2. **create**: the language's template, in a scratch folder.
//! 3. **build**, **run** and **test**: the project's detected tasks,
//!    through `mise exec` as in a TermOS pane; the run must print what the
//!    template prints.
//! 4. **editor**: TermOS's Neovim, headless, opens the template's main file
//!    and waits until every language server for it has completed the LSP
//!    `initialize` handshake.
//! 5. **debugger**: the debug adapter is started as nvim-dap starts it and
//!    must answer the DAP `initialize` request.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::editor::{self, Resolved};
use crate::lang::{self, Runner};
use crate::langs::Language;
use crate::tasks;
use crate::templates::{self, Template};

/// How a step went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Passed(String),
    Failed(String),
    /// Not run here, and why.
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub name: &'static str,
    pub outcome: Outcome,
}

impl Step {
    fn new(name: &'static str, outcome: Outcome) -> Self {
        Self { name, outcome }
    }

    pub fn failed(&self) -> bool {
        matches!(self.outcome, Outcome::Failed(_))
    }
}

/// What the check needs.
pub struct Check<'a> {
    /// The mise binary editors start servers with.
    pub mise_path: &'a Path,
    pub mise: &'a mut dyn Runner,
    /// The environment (for the editor's config folder).
    pub get: &'a dyn Fn(&str) -> Option<String>,
    /// `os-arch`.
    pub platform: &'a str,
    /// Where the project is created (new or empty).
    pub dir: &'a Path,
    /// How long a language server or debugger may take to start.
    pub timeout: Duration,
    /// Progress, as it happens.
    pub say: &'a mut dyn FnMut(&str),
}

/// The template a check creates for `lang`.
pub fn template_for(lang: &Language) -> Option<&'static Template> {
    templates::TEMPLATES.iter().find(|t| t.lang == lang.name)
}

/// Runs every step for `lang`. Steps after a failed install or create
/// are not run.
pub fn run(lang: &'static Language, version: Option<&str>, c: &mut Check<'_>) -> Vec<Step> {
    let mut steps = Vec::new();
    let push = |steps: &mut Vec<Step>, say: &mut dyn FnMut(&str), step: Step| {
        let (mark, text) = match &step.outcome {
            Outcome::Passed(t) => ("✓", t),
            Outcome::Failed(t) => ("✗", t),
            Outcome::Skipped(t) => ("-", t),
        };
        say(&format!("termos check: {mark} {}: {text}", step.name));
        steps.push(step);
    };

    (c.say)(&format!("termos check: installing {}", lang.title));
    let step = install_step(lang, version, c.platform, c.mise);
    let stop = step.failed();
    push(&mut steps, c.say, step);
    if stop {
        return steps;
    }

    let Some(template) = template_for(lang) else {
        push(
            &mut steps,
            c.say,
            Step::new("create", Outcome::Failed("no template".into())),
        );
        return steps;
    };
    let created = templates::create(template, c.dir, Some(&mut *c.mise), c.say);
    match created {
        Ok(_) => push(
            &mut steps,
            c.say,
            Step::new(
                "create",
                Outcome::Passed(format!("{} in {}", template.name, c.dir.display())),
            ),
        ),
        Err(e) => {
            push(
                &mut steps,
                c.say,
                Step::new("create", Outcome::Failed(e.to_string())),
            );
            return steps;
        }
    }

    for step in task_steps(template, c.dir, c.mise) {
        push(&mut steps, c.say, step);
    }

    let have = match lang::installed(c.mise) {
        Ok(h) => h,
        Err(e) => {
            push(
                &mut steps,
                c.say,
                Step::new("editor", Outcome::Failed(e.to_string())),
            );
            return steps;
        }
    };
    let (servers, debuggers) = editor::resolve(c.mise_path, &have);
    let step = editor_step(lang, template, &servers, c);
    push(&mut steps, c.say, step);
    let step = debugger_step(lang, &debuggers, c.dir, c.timeout);
    push(&mut steps, c.say, step);
    steps
}

fn install_step(
    lang: &Language,
    version: Option<&str>,
    platform: &str,
    mise: &mut dyn Runner,
) -> Step {
    match lang::install(lang, version, platform, mise) {
        Ok(r) if r.failed.is_empty() => {
            let mut text = r.done.join(", ");
            for (spec, why) in &r.skipped {
                text.push_str(&format!("; left out {spec}: {why}"));
            }
            Step::new("install", Outcome::Passed(text))
        }
        Ok(r) => {
            let failed: Vec<String> = r
                .failed
                .iter()
                .map(|(spec, what)| format!("{what} ({spec})"))
                .collect();
            Step::new(
                "install",
                Outcome::Failed(format!("did not install: {}", failed.join(", "))),
            )
        }
        Err(e) => Step::new("install", Outcome::Failed(e.to_string())),
    }
}

/// `mise exec -- <shell> <command>`.
fn exec_args(command: &str) -> Vec<String> {
    let (program, args) = tasks::shell(command);
    let mut v = vec!["exec".to_owned(), "--".to_owned(), program];
    v.extend(args);
    v
}

/// Build, run and test, with the project's own tasks.
fn task_steps(template: &Template, dir: &Path, mise: &mut dyn Runner) -> Vec<Step> {
    let project = match tasks::find(dir) {
        Ok(Some(p)) if p.root == dir => p,
        Ok(_) => {
            return vec![Step::new(
                "build",
                Outcome::Skipped("no build, run or test for this kind of project".into()),
            )];
        }
        Err(e) => return vec![Step::new("build", Outcome::Failed(e))],
    };
    let task: BTreeMap<&str, &String> =
        project.tasks.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let mut steps = Vec::new();
    if let Some(cmd) = task.get("build") {
        let outcome = match mise.run(&exec_args(cmd), Some(dir)) {
            Ok(true) => Outcome::Passed((*cmd).clone()),
            Ok(false) => Outcome::Failed(format!("`{cmd}` failed")),
            Err(e) => Outcome::Failed(e.to_string()),
        };
        let failed = matches!(outcome, Outcome::Failed(_));
        steps.push(Step::new("build", outcome));
        if failed {
            return steps;
        }
    }
    match task.get("run") {
        Some(cmd) => {
            let expected = template
                .prints
                .map(|p| p.replace("{name}", &templates::project_name(dir)));
            let outcome = match mise.capture(&exec_args(cmd), dir) {
                Ok((true, out)) => match &expected {
                    Some(e) if !out.contains(e.as_str()) => {
                        Outcome::Failed(format!("`{cmd}` printed {:?}, not {e:?}", out.trim()))
                    }
                    Some(e) => Outcome::Passed(format!("`{cmd}` printed {e:?}")),
                    None => Outcome::Passed(format!("`{cmd}`")),
                },
                Ok((false, out)) => {
                    Outcome::Failed(format!("`{cmd}` failed; it printed {:?}", out.trim()))
                }
                Err(e) => Outcome::Failed(e.to_string()),
            };
            steps.push(Step::new("run", outcome));
        }
        None => steps.push(Step::new(
            "run",
            Outcome::Skipped("the project has no run task".into()),
        )),
    }
    if let Some(cmd) = task.get("test") {
        let outcome = match mise.run(&exec_args(cmd), Some(dir)) {
            Ok(true) => Outcome::Passed((*cmd).clone()),
            Ok(false) => Outcome::Failed(format!("`{cmd}` failed")),
            Err(e) => Outcome::Failed(e.to_string()),
        };
        steps.push(Step::new("test", outcome));
    }
    steps
}

/// The language's servers that are installed here.
fn servers_of<'a>(lang: &Language, servers: &'a [Resolved]) -> Vec<&'a Resolved> {
    servers
        .iter()
        .filter(|r| lang.servers.iter().any(|s| s.name == r.name))
        .collect()
}

fn editor_step(
    lang: &Language,
    template: &Template,
    servers: &[Resolved],
    c: &mut Check<'_>,
) -> Step {
    let mine = servers_of(lang, servers);
    if mine.is_empty() {
        let mut limits = lang.limits_on(c.platform).peekable();
        return if limits.peek().is_some() {
            let why: Vec<&str> = limits.map(|l| l.why).collect();
            Step::new("editor", Outcome::Skipped(why.join("; ")))
        } else {
            Step::new(
                "editor",
                Outcome::Failed("no language server is installed".into()),
            )
        };
    }
    let launch = match editor::prepare_nvim(c.mise_path, c.mise, c.get, &[], c.say) {
        Ok(l) => l,
        Err(e) => return Step::new("editor", Outcome::Failed(e.to_string())),
    };
    let names: Vec<&str> = mine.iter().map(|r| r.name.as_str()).collect();
    let script_path = c
        .dir
        .parent()
        .unwrap_or(c.dir)
        .join(format!("termos-check-{}.lua", lang.name));
    if let Err(e) = std::fs::write(&script_path, lsp_script(&names, c.timeout)) {
        return Step::new("editor", Outcome::Failed(e.to_string()));
    }
    let main = c.dir.join(template.main);
    let mut cmd = Command::new(&launch.program);
    cmd.args(&launch.args)
        .arg("--headless")
        .arg("--cmd")
        .arg(ATTACH_HOOK)
        .arg("-S")
        .arg(&script_path)
        .arg(&main)
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .current_dir(c.dir)
        .stdin(Stdio::null());
    // Neovim also installs its plugins on the first start.
    let limit = c.timeout + Duration::from_secs(120);
    let outcome = match cmd.spawn() {
        Ok(child) => match wait_for(child, limit) {
            Ok(true) => Outcome::Passed(format!("{} initialized in Neovim", names.join(", "))),
            Ok(false) => Outcome::Failed(format!(
                "{} did not complete the LSP handshake (Neovim's output is above)",
                names.join(", ")
            )),
            Err(e) => Outcome::Failed(e.to_string()),
        },
        Err(e) => Outcome::Failed(format!("cannot start Neovim: {e}")),
    };
    let _ = std::fs::remove_file(&script_path);
    Step::new("editor", outcome)
}

/// Waits for `child` until `limit`, then kills it. Whether it succeeded.
fn wait_for(mut child: Child, limit: Duration) -> io::Result<bool> {
    let end = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out after {}s", limit.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Run before Neovim's config (`--cmd`): notes each server that attaches.
const ATTACH_HOOK: &str = "lua _G.termos_check_attached = {}; \
vim.api.nvim_create_autocmd('LspAttach', { callback = function(a) \
local c = vim.lsp.get_client_by_id(a.data.client_id); \
if c then _G.termos_check_attached[c.name] = true end end })";

/// The Lua run after the file is open (`-S`): waits until every server in
/// `names` that serves the buffer's filetype has initialized, prints what
/// each server said about itself, and exits 0; or prints Neovim's messages
/// and the end of the LSP log and exits 1.
pub fn lsp_script(names: &[&str], timeout: Duration) -> String {
    let list: Vec<String> = names.iter().map(|n| editor::lua_str(n)).collect();
    format!(
        r#"-- Written by `termos lang check`.
local names = {{ {names} }}
local timeout = {ms}
local function out(s) io.stdout:write(s .. "\n") end
local ft = vim.bo.filetype
local want = {{}}
for _, name in ipairs(names) do
  local cfg = vim.lsp.config[name]
  if cfg and vim.tbl_contains(cfg.filetypes or {{}}, ft) then
    want[#want + 1] = name
  end
end
if #want == 0 then
  out("termos check: no language server is set up for filetype '" .. ft .. "'")
  vim.cmd("cquit 1")
end
local function ready(name)
  if _G.termos_check_attached and _G.termos_check_attached[name] then return true end
  for _, c in ipairs(vim.lsp.get_clients({{ name = name }})) do
    if c.initialized then return true end
  end
  return false
end
local ok = vim.wait(timeout, function()
  for _, name in ipairs(want) do
    if not ready(name) then return false end
  end
  return true
end, 200)
for _, name in ipairs(want) do
  local clients = vim.lsp.get_clients({{ name = name }})
  local c = clients[1]
  local state = ready(name) and "initialized" or (c and "starting" or "not started")
  local info = ""
  if c and c.server_info then
    info = " (" .. (c.server_info.name or "?") .. " " .. (c.server_info.version or "") .. ")"
  end
  out("termos check: " .. name .. ": " .. state .. info)
end
if ok then vim.cmd("qall!") end
out(vim.api.nvim_exec2("messages", {{ output = true }}).output)
local f = io.open(vim.lsp.log.get_filename(), "r")
if f then
  local lines = {{}}
  for l in f:lines() do lines[#lines + 1] = l end
  f:close()
  out("termos check: the end of the LSP log:")
  for i = math.max(1, #lines - 40), #lines do out(lines[i]) end
end
vim.cmd("cquit 1")
"#,
        names = list.join(", "),
        ms = timeout.as_millis(),
    )
}

fn debugger_step(lang: &Language, debuggers: &[Resolved], dir: &Path, timeout: Duration) -> Step {
    let Some(d) = lang.debugger else {
        return Step::new(
            "debugger",
            Outcome::Skipped(format!("TermOS sets up no debugger for {}", lang.title)),
        );
    };
    let Some(r) = debuggers.iter().find(|r| r.name == d.name) else {
        return Step::new(
            "debugger",
            Outcome::Failed(format!("{} is not installed", d.name)),
        );
    };
    match dap_check(r, dir, timeout) {
        Ok(caps) => Step::new(
            "debugger",
            Outcome::Passed(format!("{} initialized ({caps} capabilities)", r.name)),
        ),
        Err(e) => Step::new("debugger", Outcome::Failed(format!("{}: {e}", r.name))),
    }
}

/// Starts the adapter as nvim-dap would and completes the DAP `initialize`
/// handshake. The number of capabilities it reported.
pub fn dap_check(r: &Resolved, dir: &Path, timeout: Duration) -> Result<usize, String> {
    let (program, rest) = r.cmd.split_first().ok_or("no command")?;
    let mut cmd = Command::new(program);
    cmd.args(rest).current_dir(dir).stderr(Stdio::inherit());
    match &r.port_args {
        Some(port_args) => {
            let port = free_port().map_err(|e| format!("no free port: {e}"))?;
            cmd.args(
                port_args
                    .iter()
                    .map(|a| a.replace("${port}", &port.to_string())),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null());
            let mut child = cmd.spawn().map_err(|e| format!("cannot start it: {e}"))?;
            let result = dap_over_tcp(&mut child, port, timeout, &r.name);
            let _ = child.kill();
            let _ = child.wait();
            result
        }
        None => {
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
            let mut child = cmd.spawn().map_err(|e| format!("cannot start it: {e}"))?;
            let result = dap_over_stdio(&mut child, timeout, &r.name);
            let _ = child.kill();
            let _ = child.wait();
            result
        }
    }
}

fn free_port() -> io::Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

fn dap_over_tcp(
    child: &mut Child,
    port: u16,
    timeout: Duration,
    id: &str,
) -> Result<usize, String> {
    let end = Instant::now() + timeout;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = loop {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            Ok(s) => break s,
            Err(_) if Instant::now() < end => {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!("it exited ({status}) before listening"));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("it did not listen on port {port}: {e}")),
        }
    };
    let left = end
        .saturating_duration_since(Instant::now())
        .max(Duration::from_secs(1));
    stream
        .set_read_timeout(Some(left))
        .map_err(|e| e.to_string())?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    dap_initialize(&mut reader, &mut writer, id)
}

fn dap_over_stdio(child: &mut Child, timeout: Duration, id: &str) -> Result<usize, String> {
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    write_message(&mut stdin, &initialize_request(id)).map_err(|e| e.to_string())?;
    // A thread reads, so a silent adapter cannot block us past the timeout.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let _ = tx.send(read_initialize_response(&mut reader));
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err(format!("no answer to initialize in {}s", timeout.as_secs())),
    }
}

/// The DAP `initialize` request.
pub fn initialize_request(adapter_id: &str) -> serde_json::Value {
    serde_json::json!({
        "seq": 1,
        "type": "request",
        "command": "initialize",
        "arguments": {
            "clientID": "termos",
            "clientName": "TermOS",
            "adapterID": adapter_id,
            "pathFormat": "path",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "locale": "en",
        }
    })
}

/// Sends `initialize` on `writer` and reads the answer from `reader`.
pub fn dap_initialize(
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
    adapter_id: &str,
) -> Result<usize, String> {
    write_message(writer, &initialize_request(adapter_id)).map_err(|e| e.to_string())?;
    read_initialize_response(reader)
}

fn write_message(w: &mut dyn Write, v: &serde_json::Value) -> io::Result<()> {
    let body = v.to_string();
    write!(w, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    w.flush()
}

/// The largest DAP message read.
const MAX_MESSAGE: usize = 16 << 20;

/// One DAP message (`Content-Length` framing).
pub fn read_message(r: &mut dyn BufRead) -> Result<serde_json::Value, String> {
    let mut len = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "no answer in time".to_owned(),
            _ => e.to_string(),
        })?;
        if n == 0 {
            return Err("the adapter closed the connection".into());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let len = len.ok_or("a message without Content-Length")?;
    if len > MAX_MESSAGE {
        return Err(format!("a {len}-byte message"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("not JSON: {e}"))
}

fn read_initialize_response(r: &mut dyn BufRead) -> Result<usize, String> {
    // Adapters may send events (output, telemetry) first.
    for _ in 0..100 {
        let m = read_message(r)?;
        if m["type"] == "response" && m["command"] == "initialize" {
            if m["success"] == true {
                return Ok(m["body"].as_object().map_or(0, |o| o.len()));
            }
            return Err(format!(
                "initialize failed: {}",
                m["message"].as_str().unwrap_or("no reason given")
            ));
        }
    }
    Err("no answer to initialize in 100 messages".into())
}

/// A scratch folder for a check: `<temp>/termos-check-<pid>/hello`.
pub fn scratch_dir() -> PathBuf {
    std::env::temp_dir()
        .join(format!("termos-check-{}", std::process::id()))
        .join("hello")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::tests::Fake;
    use crate::langs::find;

    fn frame(v: serde_json::Value) -> Vec<u8> {
        let body = v.to_string();
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn dap_initialize_skips_events_and_reads_the_answer() {
        let mut input = frame(serde_json::json!({"seq": 1, "type": "event", "event": "output"}));
        input.extend(frame(serde_json::json!({
            "seq": 2, "type": "response", "request_seq": 1, "command": "initialize",
            "success": true,
            "body": {"supportsConfigurationDoneRequest": true, "supportsStepBack": false}
        })));
        let mut reader = io::Cursor::new(input);
        let mut sent = Vec::new();
        assert_eq!(dap_initialize(&mut reader, &mut sent, "delve"), Ok(2));
        let sent = String::from_utf8(sent).expect("utf-8");
        let (head, body) = sent.split_once("\r\n\r\n").expect("framed");
        assert_eq!(head, format!("Content-Length: {}", body.len()));
        let v: serde_json::Value = serde_json::from_str(body).expect("json");
        assert_eq!(v["command"], "initialize");
        assert_eq!(v["arguments"]["adapterID"], "delve");

        // A refusal, a closed stream, and a frame without a length.
        let mut r = io::Cursor::new(frame(serde_json::json!({
            "type": "response", "command": "initialize", "success": false, "message": "nope"
        })));
        assert!(dap_initialize(&mut r, &mut Vec::new(), "x").is_err_and(|e| e.contains("nope")));
        let mut r = io::Cursor::new(Vec::new());
        assert!(read_message(&mut r).is_err_and(|e| e.contains("closed")));
        let mut r = io::Cursor::new(b"X-Other: 1\r\n\r\n{}".to_vec());
        assert!(read_message(&mut r).is_err_and(|e| e.contains("Content-Length")));
        let mut r = io::Cursor::new(b"Content-Length: 999999999\r\n\r\n".to_vec());
        assert!(read_message(&mut r).is_err());
    }

    #[test]
    fn dap_over_a_socket() {
        // A pretend adapter on a socket, as codelldb, delve and js-debug
        // are.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().expect("accept");
            let mut w = s.try_clone().expect("clone");
            let mut r = BufReader::new(s);
            let req = read_message(&mut r).expect("request");
            assert_eq!(req["command"], "initialize");
            w.write_all(&frame(serde_json::json!({
                "type": "response", "command": "initialize", "success": true,
                "body": {"supportsConditionalBreakpoints": true}
            })))
            .expect("answer");
        });
        let s = TcpStream::connect(addr).expect("connect");
        let mut w = s.try_clone().expect("clone");
        let mut r = BufReader::new(s);
        assert_eq!(dap_initialize(&mut r, &mut w, "codelldb"), Ok(1));
        server.join().expect("server");
    }

    #[test]
    fn the_lsp_script_waits_for_the_named_servers() {
        let s = lsp_script(&["basedpyright", "ruff"], Duration::from_secs(90));
        assert!(
            s.contains(r#"local names = { "basedpyright", "ruff" }"#),
            "{s}"
        );
        assert!(s.contains("local timeout = 90000"));
        assert!(s.contains("cquit 1") && s.contains("qall!"));
    }

    #[test]
    fn install_create_build_run_and_test_through_mise() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("hello");
        let mut m = Fake {
            ls: r#"{"go": [{"version": "1.25.3", "install_path": "/m/go", "active": true}]}"#
                .to_owned(),
            printed: "Hello from hello!\n".to_owned(),
            ..Fake::default()
        };
        let go = find("go").expect("go");
        let step = install_step(go, None, "linux-x86_64", &mut m);
        assert!(matches!(step.outcome, Outcome::Passed(_)), "{step:?}");
        let t = template_for(go).expect("template");
        templates::create(t, &project, Some(&mut m), &mut |_| {}).expect("created");
        let steps = task_steps(t, &project, &mut m);
        let names: Vec<&str> = steps.iter().map(|s| s.name).collect();
        assert_eq!(names, ["build", "run", "test"]);
        assert!(steps.iter().all(|s| !s.failed()), "{steps:?}");
        let shell = if cfg!(windows) {
            "cmd /C"
        } else {
            "/bin/sh -c"
        };
        assert!(
            m.calls
                .iter()
                .any(|c| c.starts_with(&format!("exec -- {shell} go build ./..."))),
            "{:?}",
            m.calls
        );
        // A program that prints something else fails the run.
        m.printed = "Goodbye\n".to_owned();
        let steps = task_steps(t, &project, &mut m);
        assert!(steps[1].failed(), "{steps:?}");
        // A failed build stops there.
        m.fail = vec!["go build"];
        let steps = task_steps(t, &project, &mut m);
        assert_eq!(steps.len(), 1);
        assert!(steps[0].failed());
    }

    #[test]
    fn editor_and_debugger_steps_without_tools() {
        // C on aarch64 Linux: no clangd there, which is a skip, not a
        // failure.
        let c_lang = find("c").expect("c");
        let dir = tempfile::tempdir().expect("tempdir");
        let mut m = Fake::default();
        let get = |_: &str| None;
        let mut said = |_: &str| {};
        let mut check = Check {
            mise_path: Path::new("mise"),
            mise: &mut m,
            get: &get,
            platform: "linux-aarch64",
            dir: dir.path(),
            timeout: Duration::from_secs(1),
            say: &mut said,
        };
        let t = template_for(c_lang).expect("template");
        let step = editor_step(c_lang, t, &[], &mut check);
        assert!(matches!(step.outcome, Outcome::Skipped(ref w) if w.contains("clangd")));
        check.platform = "linux-x86_64";
        assert!(editor_step(c_lang, t, &[], &mut check).failed());
        // No debugger set up for Java; one not installed fails.
        let java = find("java").expect("java");
        let step = debugger_step(java, &[], dir.path(), Duration::from_secs(1));
        assert!(matches!(step.outcome, Outcome::Skipped(_)));
        assert!(debugger_step(c_lang, &[], dir.path(), Duration::from_secs(1)).failed());
    }
}
