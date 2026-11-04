// SPDX-License-Identifier: GPL-3.0-or-later
//! TermOS's MCP server end to end (D-094): the real `termos mcp serve`,
//! started by an MCP client library the way Claude Code and Codex CLI
//! start it, against a real TermOS server: the handshake, its tools, a
//! command typed into another pane and read back, a denied command, the
//! audit log and the command center's history.

mod common;

use std::time::{Duration, Instant};

use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, ContentBlock};
use serde_json::{Value, json};
use termos_core::journal::{Event, Journal};

fn pane_ids(env: &common::Env) -> Vec<u64> {
    let out = env.run(&["msg", "pane.list"]);
    serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.get("id").and_then(Value::as_u64))
        .collect()
}

type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

async fn call(client: &Client, tool: &str, args: Value) -> (String, bool) {
    let mut params = CallToolRequestParams::new(tool.to_owned());
    params.arguments = args.as_object().cloned();
    let r = client.call_tool(params).await.expect("tools/call");
    let text = r
        .content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (text, r.is_error.unwrap_or(false))
}

#[test]
fn agents_use_termos_through_its_mcp_server() {
    let t = common::Termos::start("");
    t.wait_ready();
    let env = &t.env;
    assert!(env.run(&["msg", "pane.split-right"]).status.success());
    let end = Instant::now() + Duration::from_secs(20);
    while pane_ids(env).len() < 2 {
        assert!(Instant::now() < end, "two panes");
        std::thread::sleep(Duration::from_millis(100));
    }
    let ids = pane_ids(env);
    // The agent runs in the first pane; the second is a shell it uses.
    let (own, other) = (ids[0], ids[1]);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut cmd = env.command_in(env.path(), &["mcp", "serve"]);
        cmd.env("TERMOS_PANE", own.to_string());
        let (transport, _) =
            rmcp::transport::TokioChildProcess::builder(tokio::process::Command::from(cmd))
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("start termos mcp serve");
        let client: Client = ().serve(transport).await.expect("MCP handshake");
        let info = client.peer_info().expect("server info");
        let server = info.server_info.as_ref().expect("serverInfo");
        assert_eq!(server.name, "termos");

        let tools = client.list_all_tools().await.expect("tools/list");
        let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, termos_core::config::agents::TERMOS_MCP_TOOLS);

        let (text, err) = call(&client, "list_panes", json!({})).await;
        assert!(!err, "{text}");
        let panes: Value = serde_json::from_str(&text).expect("JSON");
        let mine: Vec<&Value> = panes
            .as_array()
            .expect("array")
            .iter()
            .filter(|p| p["this"] == true)
            .collect();
        assert_eq!(mine.len(), 1, "{text}");
        assert_eq!(mine[0]["id"], own);

        // A command typed into the other pane, and its output read back.
        let line = if cfg!(windows) {
            "echo from^-mcp"
        } else {
            "echo from''-mcp"
        };
        let (text, err) = call(
            &client,
            "send_to_pane",
            json!({ "pane": other, "text": line, "enter": true }),
        )
        .await;
        assert!(!err, "{text}");
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let (text, err) = call(&client, "read_pane", json!({ "pane": other })).await;
            assert!(!err, "{text}");
            if text.lines().any(|l| l.trim() == "from-mcp") {
                break;
            }
            assert!(Instant::now() < end, "the output never showed: {text}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let (text, err) = call(&client, "send_to_pane", json!({ "pane": own, "text": "x" })).await;
        assert!(err && text.contains("your own pane"), "{text}");

        // A command TermOS's rules deny never runs; an allowed one does.
        let (text, err) = call(&client, "run_command", json!({ "command": "session.quit" })).await;
        assert!(err && text.contains("deny-commands"), "{text}");
        let (text, err) = call(
            &client,
            "run_command",
            json!({ "command": "layout.select tiled" }),
        )
        .await;
        assert!(!err, "{text}");
        let (text, err) = call(&client, "notify", json!({ "text": "tests pass" })).await;
        assert!(!err, "{text}");
        client.cancel().await.expect("closed");
    });
    assert!(env.server_alive(), "the session is still there");

    // Every call is in a verified audit log, and the history has the tool
    // calls (names and decisions, not what was typed).
    let audit = env.run(&["audit", "list"]);
    let listed = String::from_utf8_lossy(&audit.stdout).into_owned();
    assert!(listed.contains("mcp-"), "{listed}");
    let verified = env.run(&["audit", "verify"]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stdout)
    );
    let journal = Journal::new(&env.paths().state_dir, 7);
    let records = journal.read(0);
    let tools: Vec<(&str, &str)> = records
        .iter()
        .filter_map(|r| match &r.event {
            Event::Tool { tool, decision, .. } => Some((tool.as_str(), decision.as_str())),
            _ => None,
        })
        .collect();
    assert!(tools.contains(&("run_command", "denied")), "{tools:?}");
    assert!(tools.contains(&("send_to_pane", "allowed")), "{tools:?}");
    assert!(records.iter().any(|r| matches!(&r.event,
        Event::Session { profile, state, .. } if profile == "termos-mcp" && state == "ended")));
}

/// `termos mcp register` with stand-ins for the two CLIs: Claude Code is
/// asked with `claude mcp add`; Codex's config.toml is edited in place.
#[cfg(unix)]
#[test]
fn registers_with_claude_code_and_codex() {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("claude-calls");
    let claude = bin.path().join("claude");
    std::fs::write(
        &claude,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\n", calls.display()),
    )
    .expect("write");
    let codex = bin.path().join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").expect("write");
    for p in [&claude, &codex] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let codex_home = bin.path().join("codex-home");
    std::fs::create_dir_all(&codex_home).expect("mkdir");
    std::fs::write(
        codex_home.join("config.toml"),
        "model = \"fixture-model\"\n",
    )
    .expect("write");
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars(
        "",
        vec![("PATH", path.into()), ("CODEX_HOME", codex_home.clone())],
    );
    let out = env.run(&["mcp", "register"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let asked = std::fs::read_to_string(&calls).expect("claude ran");
    let add = asked.lines().last().expect("a line");
    assert!(
        add.starts_with("mcp add --scope user termos -- /") && add.ends_with(" mcp serve"),
        "{asked}"
    );
    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("config");
    assert!(config.starts_with("model = \"fixture-model\""), "{config}");
    assert!(config.contains("[mcp_servers.termos]"), "{config}");
    assert!(config.contains("args = [\"mcp\", \"serve\"]"), "{config}");
    let status = env.run(&["agent", "status"]);
    let status = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(status.contains("also on PATH"), "{status}");
    assert!(
        status.contains("TermOS's MCP server: registered"),
        "{status}"
    );

    let out = env.run(&["mcp", "unregister", "codex"]);
    assert!(out.status.success());
    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("config");
    assert!(!config.contains("termos"), "{config}");
    assert!(env.run(&["mcp", "register", "gemini"]).status.code() != Some(0));
}
