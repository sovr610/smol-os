// SPDX-License-Identifier: GPL-3.0-or-later
//! An agent session from start to finish against a scripted mock
//! provider: it reads a file, edits it (approved), runs a check
//! (approved) and stops. Then the file, the audit chain, the saved
//! conversation and the absence of the key are checked.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Answer, MockServer};
use secrecy::SecretString;
use termos_ai::agent::{self, Input, Item, Launch, Phase, SecretSlot, ToolState};
use termos_ai::audit;
use termos_ai::history::{self, Saved};
use termos_ai::secrets::{Backend, SecretStore};
use termos_core::config::agents::{
    AgentsConfig, ProfileConfig, ProviderConfig, ProviderKind, SecretRef,
};
use termos_core::paths::Paths;

const CANARY: &str = "sk-canary-0000-1111-2222-3333-4444";

fn chunk(delta: serde_json::Value, finish: Option<&str>) -> String {
    let v = serde_json::json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    format!("data: {v}\n\n")
}

fn tool_call(id: &str, name: &str, args: serde_json::Value) -> Answer {
    Answer::Stream(vec![
        chunk(
            serde_json::json!({"content": format!("Calling {name}.")}),
            None,
        ),
        chunk(
            serde_json::json!({"tool_calls": [{"index": 0, "id": id, "type": "function",
                "function": {"name": name, "arguments": args.to_string()}}]}),
            Some("tool_calls"),
        ),
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10}}\n\n"
            .into(),
        "data: [DONE]\n\n".into(),
    ])
}

/// The scripted model: what it says depends on how many tool results it
/// has seen.
fn script(req: &common::Recorded) -> Answer {
    if req.path.ends_with("/models") {
        return Answer::Json(
            200,
            r#"{"data":[{"id":"fixture-coder","created":1},{"id":"fixture-other","created":0}]}"#
                .into(),
        );
    }
    let body = req.json();
    let results = body["messages"]
        .as_array()
        .map_or(0, |m| m.iter().filter(|x| x["role"] == "tool").count());
    match results {
        0 => tool_call("c1", "read_file", serde_json::json!({"path": "src/lib.rs"})),
        1 => tool_call(
            "c2",
            "edit_file",
            serde_json::json!({"path": "src/lib.rs", "old_text": "a - b", "new_text": "a + b"}),
        ),
        2 => {
            let check = if cfg!(windows) {
                r#"findstr /C:"a + b" src\lib.rs"#
            } else {
                "grep -c 'a + b' src/lib.rs"
            };
            tool_call("c3", "run_command", serde_json::json!({"command": check}))
        }
        _ => Answer::Stream(vec![
            chunk(
                serde_json::json!({"content": "Fixed: add() now adds."}),
                Some("stop"),
            ),
            "data: [DONE]\n\n".into(),
        ]),
    }
}

fn wait_until(handle: &agent::Handle, what: &str, f: impl Fn(&agent::View) -> bool) {
    let start = Instant::now();
    loop {
        {
            let v = handle.view.lock().expect("view");
            if f(&v) {
                return;
            }
            if let Phase::Failed(e) = &v.phase {
                panic!("the session failed while waiting for {what}: {e}");
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "timed out waiting for {what}: {:?} {}",
                v.phase,
                v.status
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_session_fixes_a_bug_with_approvals() {
    let server = MockServer::start_on_thread(script);
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().join("project");
    std::fs::create_dir_all(ws.join("src")).expect("ws");
    std::fs::write(
        ws.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
    )
    .expect("file");
    let paths = Paths {
        config_dir: dir.path().join("config"),
        state_dir: dir.path().join("state"),
        runtime_dir: dir.path().join("run"),
        data_dir: dir.path().join("data"),
    };
    let store = SecretStore::with_backend(
        Backend::File,
        termos_ai::secrets::file_path(&paths.data_dir),
    )
    .with_work_factor(2);
    store
        .create(SecretString::from("pass".to_owned()))
        .expect("create");
    store
        .set("lab", SecretString::from(CANARY.to_owned()))
        .expect("set");
    store.relock();

    let mut agents = AgentsConfig::default();
    agents.providers.insert(
        "lab".into(),
        ProviderConfig {
            kind: ProviderKind::OpenaiCompatible,
            base_url: Some(format!("{}/v1", server.base)),
            key: Some(SecretRef::parse("keyring:lab").expect("ref")),
            max_tokens: None,
            prices: [(
                "fixture-coder".to_owned(),
                termos_core::config::agents::Price {
                    input: 1.0,
                    output: 2.0,
                    cache_read: None,
                    cache_write: None,
                },
            )]
            .into(),
        },
    );
    agents
        .profiles
        .insert("default".into(), ProfileConfig::default());
    let handle = agent::start(Launch {
        agents,
        secrets: Arc::new(SecretSlot::with(store)),
        paths: paths.clone(),
        profile: "default".into(),
        workspace: ws.clone(),
        resume: None,
        helper: None,
        home: Some(dir.path().join("home")),
    })
    .expect("start");

    // The secrets file is locked: a wrong passphrase, then the right one.
    wait_until(&handle, "the passphrase", |v| {
        matches!(v.phase, Phase::NeedPassphrase { .. })
    });
    handle.send(Input::Passphrase(SecretString::from("wrong".to_owned())));
    wait_until(&handle, "the error", |v| {
        matches!(&v.phase, Phase::NeedPassphrase { error: Some(_) })
    });
    handle.send(Input::Passphrase(SecretString::from("pass".to_owned())));

    // The model list comes from the provider.
    wait_until(
        &handle,
        "the model list",
        |v| matches!(&v.phase, Phase::PickModel { models, .. } if models.len() == 2),
    );
    handle.send(Input::Model("fixture-coder".into()));
    wait_until(&handle, "ready", |v| v.phase == Phase::Ready);

    handle.send(Input::Send(format!(
        "add() subtracts; fix it. (my key is {CANARY})"
    )));
    // read_file runs without asking; edit_file asks, with a diff.
    wait_until(
        &handle,
        "the edit to ask",
        |v| matches!(&v.phase, Phase::Confirm(p) if p.tool == "edit_file"),
    );
    {
        let v = handle.view.lock().expect("view");
        let Phase::Confirm(p) = &v.phase else {
            unreachable!()
        };
        let termos_ai::tools::Preview::Diff { diff, .. } = &p.preview else {
            panic!("a diff: {:?}", p.preview)
        };
        assert!(diff.contains("+    a + b"), "{diff}");
        assert!(p.remember);
    }
    handle.send(Input::Approve { remember: false });
    wait_until(
        &handle,
        "the command to ask",
        |v| matches!(&v.phase, Phase::Confirm(p) if p.tool == "run_command"),
    );
    handle.send(Input::Approve { remember: false });
    wait_until(&handle, "the end of the turn", |v| {
        v.phase == Phase::Ready
            && v.items
                .iter()
                .any(|i| matches!(i, Item::Assistant { text, .. } if text.contains("Fixed")))
    });

    let (session_id, cost) = {
        let v = handle.view.lock().expect("view");
        let states: Vec<&ToolState> = v
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Tool(t) => Some(&t.state),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            [&ToolState::Done, &ToolState::Done, &ToolState::Done]
        );
        // The command ran on a PTY: its terminal is there.
        let cmd = v
            .items
            .iter()
            .find_map(|i| match i {
                Item::Tool(t) if t.tool == "run_command" => Some(t),
                _ => None,
            })
            .expect("command");
        assert!(cmd.term.is_some());
        assert_eq!(cmd.result.as_deref(), Some("exit code 0"));
        assert_eq!(v.usage.input, 300);
        (v.session_id.clone(), v.cost)
    };
    assert!(cost.is_some_and(|c| c > 0.0));
    assert!(
        std::fs::read_to_string(ws.join("src/lib.rs"))
            .expect("read")
            .contains("a + b")
    );

    handle.send(Input::Close);
    // The saved conversation names the audit head, and the chain checks.
    let hist = history::dir(&paths.state_dir);
    let start = Instant::now();
    let log = audit::dir(&paths.state_dir).join(format!("{session_id}.jsonl"));
    // The session writes its last audit note, then saves the conversation
    // with the new head: wait for both.
    let saved = loop {
        let s = Saved::load(&hist, &session_id).expect("saved");
        let closed = audit::read(&log)
            .expect("audit")
            .iter()
            .any(|e| matches!(&e.event, audit::Event::Note { text } if text.contains("closed")));
        if closed && audit::verify_with_head(&log, s.audit_head.as_deref()).is_ok() {
            break s;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the session did not end cleanly"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    audit::verify(&log).expect("the chain verifies");
    let entries = audit::read(&log).expect("audit");
    let decisions: Vec<(String, String)> = entries
        .iter()
        .filter_map(|e| match &e.event {
            audit::Event::Tool { tool, decision, .. } => Some((tool.clone(), decision.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        [
            ("read_file".into(), "allowed".into()),
            ("edit_file".into(), "approved".into()),
            ("run_command".into(), "approved".into())
        ]
    );
    assert_eq!(saved.messages.len(), 8, "user, 3 × (call, result), answer");

    // The canary key appears nowhere TermOS wrote.
    let mut written = Vec::new();
    for root in [&paths.state_dir, &paths.config_dir, &paths.runtime_dir] {
        collect(root, &mut written);
    }
    assert!(!written.is_empty());
    for f in written {
        let text = std::fs::read(&f).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&text).contains(CANARY),
            "{} holds the key",
            f.display()
        );
    }
    // The model never saw it either: it was redacted from the user's text.
    for r in server.requests() {
        assert!(!r.body.contains(CANARY), "a request body held the key");
    }
}

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}
