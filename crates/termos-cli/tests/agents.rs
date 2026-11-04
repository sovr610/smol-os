// SPDX-License-Identifier: GPL-3.0-or-later
//! Agents through the real binary: the sandbox's confinement, audit
//! verification, and a whole agent session on the desktop that must leave
//! its key nowhere (D-017's canary test).

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Env, Termos};
use secrecy::SecretString;
use termos_ai::secrets::{Backend, SecretStore};

const T: Duration = Duration::from_secs(30);

/// A scripted OpenAI-compatible provider: `answer` gets each request's
/// JSON body and returns the event-stream pieces to send. Every body is
/// kept.
fn mock_provider(
    answer: fn(&serde_json::Value) -> Vec<String>,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let kept = Arc::clone(&kept);
            std::thread::spawn(move || serve(stream, answer, &kept));
        }
    });
    (base, bodies)
}

fn serve(
    mut s: TcpStream,
    answer: fn(&serde_json::Value) -> Vec<String>,
    kept: &Mutex<Vec<String>>,
) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let Ok(n) = s.read(&mut chunk) else { return };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let Ok(n) = s.read(&mut chunk) else { return };
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    if head.starts_with("get") {
        let json = r#"{"data":[{"id":"fixture-model","created":1}]}"#;
        let _ = write!(
            s,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
            json.len()
        );
        return;
    }
    kept.lock().expect("lock").push(body.clone());
    let req: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let _ = write!(
        s,
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
    );
    for piece in answer(&req) {
        let _ = s.write_all(piece.as_bytes());
        let _ = s.flush();
    }
}

fn chunk(delta: serde_json::Value, finish: Option<&str>) -> String {
    let v = serde_json::json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    format!("data: {v}\n\n")
}

/// The model asks for the environment, then says it is done.
fn env_then_done(req: &serde_json::Value) -> Vec<String> {
    let results = req["messages"]
        .as_array()
        .map_or(0, |m| m.iter().filter(|x| x["role"] == "tool").count());
    if results == 0 {
        let command = if cfg!(windows) { "set" } else { "env" };
        vec![
            chunk(
                serde_json::json!({"tool_calls": [{"index": 0, "id": "c1", "type": "function",
                    "function": {"name": "run_command", "arguments": serde_json::json!({"command": command}).to_string()}}]}),
                Some("tool_calls"),
            ),
            "data: [DONE]\n\n".into(),
        ]
    } else {
        vec![
            chunk(
                serde_json::json!({"content": "All done here."}),
                Some("stop"),
            ),
            "data: [DONE]\n\n".into(),
        ]
    }
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn an_agent_session_leaves_no_key_behind() {
    const CANARY: &str = "sk-canary-e2e-0123456789abcdef";
    let (base, bodies) = mock_provider(env_then_done);
    let config = format!(
        "[agents.providers.lab]\nkind = \"openai-compatible\"\nbase-url = \"{base}/v1\"\nkey = \"keyring:lab\"\n\
         [agents.profiles.default]\nmodel = \"fixture-model\"\n[secrets]\nstore = \"file\"\n"
    );
    // The key is in the secrets file, and also in the environment the
    // server starts with, where a command could print it.
    let env = Env::with_vars(&config, vec![("LAB_API_KEY", PathBuf::from(CANARY))]);
    let paths = env.paths();
    let store = SecretStore::with_backend(
        Backend::File,
        termos_ai::secrets::file_path(&paths.data_dir),
    )
    .with_work_factor(4);
    store
        .create(SecretString::from("pass phrase".to_owned()))
        .expect("create");
    store
        .set("lab", SecretString::from(CANARY.to_owned()))
        .expect("set");

    let t = Termos::start_in(Arc::clone(&env));
    t.wait_ready();
    t.send(b"\x02a");
    t.wait_for("the passphrase prompt", T, |s| s.contains("passphrase ›"));
    t.send(b"pass phrase\r");
    t.wait_for("the agent", T, |s| s.contains("Enter sends"));
    t.send(b"show me the environment\r");
    t.wait_for("the command to ask", T, |s| s.contains("Allow?"));
    t.send(b"y");
    t.wait_for("the answer", T, |s| s.contains("All done here."));

    // The audit log verifies, through the CLI.
    let out = env.run(&["audit", "verify"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ok (2 entries)"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Stop the server so it writes its snapshots and closes its logs.
    let out = env.run(&["kill-server"]);
    assert!(out.status.success());
    drop(t);

    // The model never saw the key: the command's environment had none.
    let sent = bodies.lock().expect("lock").clone();
    assert_eq!(sent.len(), 2, "two turns");
    assert!(
        sent[1].to_uppercase().contains("PATH="),
        "the environment was listed: {}",
        sent[1]
    );
    for b in &sent {
        assert!(!b.contains(CANARY), "a request carried the key");
    }
    // Nothing TermOS wrote holds it: logs, audit, conversations,
    // snapshots, config, runtime files.
    let mut files = Vec::new();
    for dir in [
        &paths.state_dir,
        &paths.config_dir,
        &paths.runtime_dir,
        &paths.data_dir,
    ] {
        files_under(dir, &mut files);
    }
    let secrets = termos_ai::secrets::file_path(&paths.data_dir);
    for want in ["log", "audit", "agents", "sessions"] {
        assert!(
            files
                .iter()
                .any(|f| f.components().any(|c| c.as_os_str() == want)),
            "no {want} files were written: {files:?}"
        );
    }
    for f in files.iter().filter(|f| **f != secrets) {
        let bytes = std::fs::read(f).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(CANARY),
            "{} holds the key",
            f.display()
        );
    }
}

/// The model reads the question's file, then answers with what it read.
fn read_then_answer(req: &serde_json::Value) -> Vec<String> {
    let results: Vec<String> = req["messages"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter(|x| x["role"] == "tool")
                .filter_map(|x| x["content"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if results.is_empty() {
        vec![
            chunk(
                serde_json::json!({"tool_calls": [{"index": 0, "id": "c1", "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"notes.txt\"}"}}]}),
                Some("tool_calls"),
            ),
            "data: [DONE]\n\n".into(),
        ]
    } else {
        let said = if results[0].contains("the build needs zig") {
            "It says the build needs zig."
        } else {
            "I could not read it."
        };
        vec![
            chunk(serde_json::json!({"content": said}), Some("stop")),
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":40,\"completion_tokens\":7}}\n\n"
                .into(),
            "data: [DONE]\n\n".into(),
        ]
    }
}

#[test]
fn agent_ask_answers_on_stdout() {
    let (base, bodies) = mock_provider(read_then_answer);
    let config = format!(
        "[agents.providers.lab]\nkind = \"openai-compatible\"\nbase-url = \"{base}/v1\"\n\
         [agents.profiles.quick]\nmodel = \"fixture-model\"\n"
    );
    let env = Env::new(&config);
    let project = env.path().join("project");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(project.join("notes.txt"), "the build needs zig\n").expect("notes");

    let out = env.run_in(
        &project,
        &[
            "agent",
            "ask",
            "--profile",
            "quick",
            "What",
            "do",
            "the",
            "notes",
            "say?",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout.trim(), "It says the build needs zig.");
    let sent = bodies.lock().expect("lock").clone();
    assert!(sent[0].contains("What do the notes say?"), "{}", sent[0]);

    // The question from standard input, and JSON out.
    let mut cmd = env.command_in(
        &project,
        &[
            "agent",
            "ask",
            "--profile",
            "quick",
            "--json",
            "Notes:",
            "-",
        ],
    );
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"what do they say?")
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["answer"], "It says the build needs zig.");
    assert_eq!(v["model"], "fixture-model");
    assert_eq!(v["tokens"]["output"], 7);
    let session = v["session"].as_str().expect("session").to_owned();
    let sent = bodies.lock().expect("lock").clone();
    assert!(
        sent.iter().any(|b| b.contains("Notes: what do they say?")),
        "{sent:?}"
    );

    // Saved, so `agent history` lists it; and the profiles.
    let out = env.run(&["agent", "history"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&session));
    let out = env.run(&["agent", "profiles"]);
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        listed.contains("quick") && listed.contains("fixture-model"),
        "{listed}"
    );

    // A profile with no model says what to pass.
    let out = env.run_in(&project, &["agent", "ask", "hello"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--model") && err.contains("fixture-model"),
        "{err}"
    );
}

#[test]
fn audit_verify_finds_tampering() {
    let env = Env::new("");
    let dir = termos_ai::audit::dir(&env.paths().state_dir);
    let mut log = termos_ai::audit::AuditLog::open(&dir, "20261102-000000-abcdef").expect("open");
    for n in 0..3 {
        log.append(termos_ai::audit::Event::Note {
            text: format!("note {n}"),
        })
        .expect("append");
    }
    let path = log.path().to_owned();
    drop(log);
    let out = env.run(&["audit", "verify", "20261102-000000-abcdef"]);
    assert!(out.status.success());
    let text = std::fs::read_to_string(&path)
        .expect("read")
        .replacen("note 1", "note X", 1);
    std::fs::write(&path, text).expect("write");
    let out = env.run(&["audit", "verify"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("TAMPERED: line 3: the chain is broken"),
        "{stdout}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_sandbox_confines_writes_and_the_network() {
    let env = Env::new("");
    let status = env.run(&["sandbox", "status"]);
    let line = String::from_utf8_lossy(&status.stdout).trim().to_owned();
    if line.starts_with("no sandbox") {
        // CI sets this where bubblewrap is installed, so a broken sandbox
        // cannot pass as an absent one.
        assert!(
            std::env::var("TERMOS_REQUIRE_SANDBOX").is_err(),
            "the sandbox is required here but: {line}"
        );
        eprintln!("SKIPPED: {line}");
        return;
    }
    let ws = env.path().join("ws");
    std::fs::create_dir_all(&ws).expect("ws");
    let sh = |script: &str| env.run_in(&ws, &["sandbox", "run", "--", "sh", "-c", script]);

    let out = sh("echo hi > inside.txt");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("inside.txt")).expect("inside"),
        "hi\n"
    );

    for outside in [
        env.path().join("outside.txt"),
        env.path().join("data").join("outside.txt"),
    ] {
        let _ = sh(&format!(
            "mkdir -p {0} 2>/dev/null; echo x > {1}",
            outside.parent().expect("dir").display(),
            outside.display()
        ));
        assert!(
            !outside.exists(),
            "{} was written from the sandbox ({line})",
            outside.display()
        );
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let termos = env!("CARGO_BIN_EXE_termos");
    let out = env.run_in(
        &ws,
        &["sandbox", "run", "--", termos, "debug", "connect", &addr],
    );
    assert!(
        !out.status.success(),
        "reached the network without --net: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let out = env.run_in(
        &ws,
        &[
            "sandbox", "run", "--net", "--", termos, "debug", "connect", &addr,
        ],
    );
    assert!(
        out.status.success(),
        "--net did not open the network: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Windows: the AppContainer (D-108). Writes reach the workspace only,
/// the rest of the user's files cannot be read, the network is closed
/// without `--net`, and `termos sandbox reset` takes the grants back.
#[cfg(windows)]
#[test]
fn the_appcontainer_confines_writes_reads_and_the_network() {
    let env = Env::new("");
    let status = env.run(&["sandbox", "status"]);
    let line = String::from_utf8_lossy(&status.stdout).trim().to_owned();
    if line != "sandbox: AppContainer" {
        // CI sets this, so a broken sandbox cannot pass as an absent one.
        assert!(
            std::env::var("TERMOS_REQUIRE_SANDBOX").is_err(),
            "the sandbox is required here but: {line}"
        );
        eprintln!("SKIPPED: {line}");
        return;
    }
    let ws = env.path().join("ws");
    std::fs::create_dir_all(&ws).expect("ws");
    // Only the system on PATH: no toolchain folders to grant.
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let path = format!(r"{root}\System32;{root}");
    let run = |args: &[&str]| {
        let mut all = vec!["sandbox", "run"];
        all.extend_from_slice(args);
        env.command_in(&ws, &all)
            .env("PATH", &path)
            .output()
            .expect("run termos")
    };

    let out = run(&["--", "cmd", "/c", "echo hi> inside.txt"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("inside.txt"))
            .expect("inside")
            .trim(),
        "hi"
    );

    let outside = env.path().join("outside.txt");
    let _ = run(&["--", "cmd", "/c", &format!("echo x> {}", outside.display())]);
    assert!(!outside.exists(), "written from the sandbox");

    let secret = env.path().join("secret.txt");
    std::fs::write(&secret, "s3cret").expect("write");
    let out = run(&["--", "cmd", "/c", &format!("type {}", secret.display())]);
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("s3cret"),
        "read from the sandbox"
    );

    // No network without --net (AppContainers never reach loopback, so a
    // public address tells).
    let termos = env!("CARGO_BIN_EXE_termos");
    let out = run(&["--", termos, "debug", "connect", "1.1.1.1:443"]);
    assert!(
        !out.status.success(),
        "reached the network without --net: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let out = run(&["--net", "--", termos, "debug", "connect", "1.1.1.1:443"]);
    assert!(
        out.status.success(),
        "--net did not open the network: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let out = env.run(&["sandbox", "reset"]);
    assert!(out.status.success(), "{out:?}");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("took back"), "{said}");
}

#[cfg(target_os = "linux")]
#[test]
fn plugins_run_in_the_sandbox() {
    let env = Env::new("[plugins]\nenabled = [\"hello\", \"probe\"]\n");
    let status = env.run(&["sandbox", "status"]);
    let line = String::from_utf8_lossy(&status.stdout).trim().to_owned();
    if line.starts_with("no sandbox") {
        assert!(
            std::env::var("TERMOS_REQUIRE_SANDBOX").is_err(),
            "the sandbox is required here but: {line}"
        );
        eprintln!("SKIPPED: {line}");
        return;
    }
    let plugins = env.paths().plugins_dir();
    let write = |name: &str, exec: &[&str], permissions: &str| {
        let d = plugins.join(name);
        std::fs::create_dir_all(&d).expect("mkdir");
        let exec: Vec<String> = exec.iter().map(|a| format!("{a:?}")).collect();
        std::fs::write(
            d.join("plugin.toml"),
            format!(
                "name = \"{name}\"\nexec = [{}]\n[permissions]\n{permissions}\n",
                exec.join(", ")
            ),
        )
        .expect("manifest");
    };
    // A real plugin: it must still reach the session socket.
    let hello = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../termos-shell/tests/fixtures/hello_plugin.py");
    write(
        "hello",
        &["python3", &hello.display().to_string()],
        "windows = true\nstatus = true\ncommands = true\nnotify = true",
    );
    // A probe: what can it write?
    let escaped = env.path().join("escaped.txt");
    let script = format!(
        "echo mine > \"$TERMOS_PLUGIN_DATA/result\"; echo x > {}; echo y > ./escaped.txt; echo done >> \"$TERMOS_PLUGIN_DATA/result\"; exec sleep 600",
        escaped.display()
    );
    write("probe", &["/bin/sh", "-c", &script], "");

    assert!(env.run(&["new", "-d"]).status.success());
    let result = env.paths().state_dir.join("plugins/probe/result");
    let deadline = std::time::Instant::now() + T;
    while !std::fs::read_to_string(&result).is_ok_and(|r| r.contains("done")) {
        assert!(
            std::time::Instant::now() < deadline,
            "the probe plugin never wrote its result"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!escaped.exists(), "a plugin wrote outside its folder");
    assert!(
        !plugins.join("probe/escaped.txt").exists(),
        "a plugin wrote into its own (read-only) directory"
    );
    let deadline = std::time::Instant::now() + T;
    loop {
        let out = env.run(&["msg", "plugin.list"]);
        let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
        let connected = list
            .as_array()
            .and_then(|l| l.iter().find(|p| p["name"] == "hello"))
            .is_some_and(|p| p["connected"] == true);
        if connected {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sandboxed plugin never connected: {list}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(env.run(&["kill-server"]).status.success());
}
