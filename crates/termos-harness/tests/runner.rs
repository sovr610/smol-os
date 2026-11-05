// SPDX-License-Identifier: GPL-3.0-or-later
//! The harness end to end (D-099, D-100): a plan made by a stand-in Claude
//! Code, tasks run by stand-in CLIs (shell scripts printing their JSON
//! lines) and by TermOS's own agent against a mock provider; fallbacks,
//! plan limits, the order tasks run in, stopping, and the audit trail.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use termos_ai::agent::SecretSlot;
use termos_core::config::Config;
use termos_core::config::agents::SecretStoreKind;
use termos_core::config::harness::BackendKind;
use termos_core::paths::Paths;
use termos_harness::backends::{ApiContext, Program, Programs};
use termos_harness::plan::Plan;
#[cfg(unix)]
use termos_harness::runner::TaskState;
use termos_harness::runner::{Context, Harness, Phase, RunView};

const T: Duration = Duration::from_secs(30);

fn wait(h: &Harness, what: &str, f: impl Fn(&RunView) -> bool) -> RunView {
    let start = Instant::now();
    loop {
        {
            let v = h.view.lock().expect("view");
            if f(&v) {
                return v.clone();
            }
            assert!(
                start.elapsed() < T,
                "timed out waiting for {what}: {:?} {:#?}",
                v.phase,
                v.tasks
                    .iter()
                    .map(|t| (&t.task.id, &t.state, &t.log))
                    .collect::<Vec<_>>()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn paths(dir: &Path) -> Paths {
    Paths {
        config_dir: dir.join("config"),
        state_dir: dir.join("state"),
        runtime_dir: dir.join("run"),
        data_dir: dir.join("data"),
    }
}

fn config(toml: &str) -> Config {
    Config::parse(toml, Path::new("config.toml")).expect("config")
}

/// Stand-in CLIs in a folder: `claude` and `codex` scripts (shell
/// scripts, so Unix only).
#[cfg(unix)]
struct StandIns {
    bin: PathBuf,
}

#[cfg(unix)]
impl Programs for StandIns {
    fn cli(&self, kind: BackendKind, account: Option<&str>) -> Result<Program, String> {
        let name = match kind {
            BackendKind::Claude => "claude",
            BackendKind::Codex => "codex",
            BackendKind::Api => return Err("not a CLI".into()),
        };
        let path = self.bin.join(name);
        if !path.exists() {
            return Err(format!("{name} is not installed"));
        }
        Ok(Program {
            path,
            env: vec![(
                "STAND_IN_ACCOUNT".into(),
                account.unwrap_or("default").into(),
            )],
            unset: Vec::new(),
            secrets: Default::default(),
            account: account.map(str::to_owned),
        })
    }
}

/// No CLIs at all.
struct NoClis;

impl Programs for NoClis {
    fn cli(&self, _: BackendKind, _: Option<&str>) -> Result<Program, String> {
        Err("not installed".into())
    }
}

fn context(dir: &Path, config: Config, programs: Arc<dyn Programs>, api: bool) -> Context {
    let p = paths(dir);
    let project = dir.join("project");
    std::fs::create_dir_all(&project).expect("project");
    Context {
        api: api.then(|| ApiContext {
            agents: config.agents.clone(),
            secrets: Arc::new(SecretSlot::new(SecretStoreKind::File, p.data_dir.clone())),
            paths: p.clone(),
            helper: None,
            config_file: None,
            home: Some(dir.join("home")),
            journal: None,
        }),
        config,
        programs,
        state_dir: p.state_dir.clone(),
        data_dir: Some(p.data_dir.clone()),
        journal: Some(Arc::new(termos_core::journal::Journal::new(
            &p.state_dir,
            7,
        ))),
        project,
    }
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, format!("#!/bin/sh\n{body}")).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A stand-in that logs its arguments, keeps the prompt, and answers
/// with the file whose marker the prompt holds (`answers/<marker>`).
#[cfg(unix)]
fn stand_in(bin: &Path, name: &str, markers: &[&str]) {
    let mut cases = String::new();
    for m in markers {
        cases.push_str(&format!("  *'{m}'*) a='{m}';;\n"));
    }
    script(
        &bin.join(name),
        &format!(
            r#"dir="$(dirname "$0")"
prompt="$(cat)"
a=default
case "$prompt" in
{cases}esac
echo "{name} $a $STAND_IN_ACCOUNT $*" >> "$dir/calls"
printf '%s\n=====\n' "$prompt" >> "$dir/prompts-{name}"
[ -x "$dir/do-{name}-$a" ] && "$dir/do-{name}-$a"
cat "$dir/answers/{name}-$a"
exit "$(cat "$dir/exit-{name}-$a" 2>/dev/null || echo 0)"
"#
        ),
    );
}

#[cfg(unix)]
fn claude_result(text: &str, error: bool) -> String {
    format!(
        "{}\n{}\n",
        serde_json::json!({"type": "system", "subtype": "init", "session_id": "cs-1", "model": "fixture"}),
        serde_json::json!({"type": "result", "subtype": if error { "error_during_execution" } else { "success" },
            "is_error": error, "result": text, "total_cost_usd": 0.01,
            "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}})
    )
}

#[cfg(unix)]
fn codex_result(text: &str) -> String {
    format!(
        "{}\n{}\n{}\n{}\n",
        serde_json::json!({"type": "thread.started", "thread_id": "th-1"}),
        serde_json::json!({"type": "item.completed", "item": {"type": "file_change", "changes": [{"path": "notes.txt", "kind": "update"}]}}),
        serde_json::json!({"type": "item.completed", "item": {"type": "agent_message", "text": text}}),
        serde_json::json!({"type": "turn.completed", "usage": {"input_tokens": 100, "cached_input_tokens": 40, "output_tokens": 7}}),
    )
}

#[cfg(unix)]
#[test]
fn a_plan_runs_falls_back_past_a_plan_limit_and_is_audited() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(bin.join("answers")).expect("bin");
    stand_in(
        &bin,
        "claude",
        &["Answer with only this JSON", "task t1 ", "task t2 "],
    );
    stand_in(&bin, "codex", &["task t2 ", "task t3 "]);
    let plan = serde_json::json!({"summary": "Add a greeting", "tasks": [
        {"id": "t1", "kind": "search", "title": "Find the notes", "prompt": "Find where notes are kept."},
        {"id": "t2", "kind": "coding", "title": "Add a line", "prompt": "Add a greeting to notes.txt.", "depends_on": ["t1"]},
        {"id": "t3", "kind": "review", "title": "Review it", "prompt": "Review the greeting.", "depends_on": ["t2"]}
    ]})
    .to_string();
    let answers = bin.join("answers");
    std::fs::write(
        answers.join("claude-Answer with only this JSON"),
        claude_result(&format!("Here you go:\n```json\n{plan}\n```"), false),
    )
    .expect("w");
    std::fs::write(
        answers.join("claude-task t1 "),
        claude_result("They are in notes.txt.", false),
    )
    .expect("w");
    // Claude Code reaches its plan's limit on the coding task.
    std::fs::write(
        answers.join("claude-task t2 "),
        claude_result("Claude AI usage limit reached|1800000000", true),
    )
    .expect("w");
    std::fs::write(bin.join("exit-claude-task t2 "), "1").expect("w");
    // Codex does it, and the review (Claude Code is out for the plan).
    std::fs::write(
        answers.join("codex-task t2 "),
        codex_result("Added the greeting."),
    )
    .expect("w");
    script(&bin.join("do-codex-task t2 "), "echo hello >> notes.txt\n");
    std::fs::write(answers.join("codex-task t3 "), codex_result("Looks right.")).expect("w");

    let cfg = config(
        "[harness.backends.claude]\naccount = \"work\"\nmodels = { fast = \"fixture-small\" }\n",
    );
    let ctx = context(
        dir.path(),
        cfg,
        Arc::new(StandIns { bin: bin.clone() }),
        false,
    );
    let project = ctx.project.clone();
    let state = ctx.state_dir.clone();
    let h = Harness::start(ctx).expect("start");
    h.plan("say hello in the notes");
    let v = wait(&h, "the plan", |v| v.phase == Phase::Review);
    assert_eq!(v.summary, "Add a greeting");
    assert_eq!(v.tasks.len(), 3);
    assert!(
        v.planner
            .as_deref()
            .is_some_and(|p| p.starts_with("Claude Code · work"))
    );
    // Planned: search on Claude Code's fast model, coding on its default.
    assert_eq!(
        v.tasks[0].route.first().and_then(|c| c.model.as_deref()),
        Some("fixture-small")
    );
    assert!(
        v.tasks[1]
            .route
            .explain()
            .starts_with("coding → balanced: Claude Code · work")
    );

    h.run();
    let v = wait(&h, "the run", |v| matches!(v.phase, Phase::Finished { .. }));
    assert_eq!(v.phase, Phase::Finished { failed: 0 }, "{:#?}", v.tasks);
    let t = &v.tasks;
    assert_eq!(t[0].state, TaskState::Done);
    assert!(
        t[0].by
            .as_deref()
            .is_some_and(|b| b.starts_with("Claude Code"))
    );
    assert!(
        t[1].tried[0].contains("usage limit reached"),
        "{:?}",
        t[1].tried
    );
    assert!(
        t[1].by
            .as_deref()
            .is_some_and(|b| b.starts_with("Codex CLI"))
    );
    assert_eq!(t[1].result.as_deref(), Some("Added the greeting."));
    assert!(
        t[2].by
            .as_deref()
            .is_some_and(|b| b.starts_with("Codex CLI")),
        "{:?}",
        t[2]
    );
    assert_eq!(t[1].tokens.cache_read, 40);
    assert_eq!(
        std::fs::read_to_string(project.join("notes.txt")).expect("notes"),
        "hello\n"
    );

    // How each CLI was run: the account, read-only or not.
    let calls = std::fs::read_to_string(bin.join("calls")).expect("calls");
    let line = |marker: &str, cli: &str| {
        calls
            .lines()
            .find(|l| l.starts_with(&format!("{cli} {marker}")))
            .unwrap_or_else(|| panic!("no {cli} {marker} in {calls}"))
            .to_owned()
    };
    let t1 = line("task t1 ", "claude");
    assert!(
        t1.contains(" work ")
            && t1.contains("--disallowedTools")
            && t1.contains("--model fixture-small"),
        "{t1}"
    );
    assert!(line("task t2 ", "claude").contains("--permission-mode acceptEdits"));
    assert!(line("task t2 ", "codex").contains("--sandbox workspace-write"));
    assert!(line("task t3 ", "codex").contains("--sandbox read-only"));
    assert!(
        !calls.contains("claude task t3"),
        "Claude Code was not used after its limit"
    );
    // The coding task was told what the search found.
    let prompts = std::fs::read_to_string(bin.join("prompts-codex")).expect("prompts");
    assert!(
        prompts.contains("Add a greeting to notes.txt.")
            && prompts.contains("They are in notes.txt."),
        "{prompts}"
    );

    // Every hand-off, in a log that verifies.
    let log = state.join("audit").join(format!("harness-{}.jsonl", v.id));
    termos_ai::audit::verify(&log).expect("the chain verifies");
    let text = std::fs::read_to_string(&log).expect("log");
    for want in [
        "\"outcome\":\"started\"",
        "\"outcome\":\"failed\"",
        "\"outcome\":\"done\"",
        "usage limit",
    ] {
        assert!(text.contains(want), "{want} in {text}");
    }
    let journal = termos_core::journal::Journal::new(&state, 7);
    let handoffs = journal
        .read(0)
        .into_iter()
        .filter(|r| matches!(r.event, termos_core::journal::Event::Handoff { .. }))
        .count();
    assert!(handoffs >= 8, "{handoffs}");
}

#[cfg(unix)]
#[test]
fn edits_run_in_turn_reads_alongside_and_a_task_can_be_stopped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(bin.join("answers")).expect("bin");
    let order = bin.join("order");
    // Every task takes a moment and says when it starts and ends.
    script(
        &bin.join("claude"),
        &format!(
            r#"prompt="$(cat)"
id=$(printf '%s' "$prompt" | sed -n 's/^This is task \([a-z0-9]*\) .*/\1/p')
echo "start $id" >> '{order}'
case "$id" in
  slow) sleep 30;;
  *) sleep 1;;
esac
echo "end $id" >> '{order}'
printf '%s\n' '{done}'
"#,
            order = order.display(),
            done = serde_json::json!({"type": "result", "subtype": "success", "is_error": false, "result": "ok"})
        ),
    );
    let task = |id: &str, kind: &str, deps: &[&str]| serde_json::json!({"id": id, "kind": kind, "title": id, "prompt": format!("do {id}"), "depends_on": deps});
    let plan: Plan = serde_json::from_value(serde_json::json!({
        "goal": "g", "summary": "s",
        "tasks": [task("r1", "search", &[]), task("r2", "search", &[]), task("w1", "coding", &[]),
                  task("w2", "coding", &[]), task("r3", "review", &[]), task("slow", "search", &[])]
    }))
    .expect("plan");
    let ctx = context(
        dir.path(),
        config("[harness]\nparallel = 3\n"),
        Arc::new(StandIns { bin: bin.clone() }),
        false,
    );
    let h = Harness::start(ctx).expect("start");
    h.load(plan).expect("load");
    h.run();
    // `slow` runs after the edits (it comes after them); stop it.
    wait(&h, "slow to start", |v| {
        v.tasks[5].state == TaskState::Running
    });
    h.stop(Some("slow"));
    let v = wait(&h, "the run", |v| matches!(v.phase, Phase::Finished { .. }));
    assert_eq!(v.tasks[5].state, TaskState::Stopped);
    assert_eq!(v.phase, Phase::Finished { failed: 1 });
    let log = std::fs::read_to_string(&order).expect("order");
    let at = |e: &str| {
        log.lines()
            .position(|l| l == e)
            .unwrap_or_else(|| panic!("no {e} in {log}"))
    };
    // The two searches overlap, and the first edit runs alongside them.
    assert!(
        at("start r2") < at("end r1") && at("start r1") < at("end r2"),
        "{log}"
    );
    assert!(at("start w1") < at("end r1"), "{log}");
    // The edits take turns; the review waits for both.
    assert!(at("end w1") < at("start w2"), "{log}");
    assert!(at("end w2") < at("start r3"), "{log}");
    assert!(!log.contains("end slow"), "stopped: {log}");
}

/// A scripted OpenAI-compatible provider: `answer` gets each request's
/// JSON body and returns the event-stream pieces.
fn mock_provider(
    answer: fn(&serde_json::Value) -> Vec<String>,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let kept = Arc::clone(&kept);
            std::thread::spawn(move || {
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
                kept.lock().expect("lock").push(body.clone());
                let req: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
                );
                for piece in answer(&req) {
                    let _ = s.write_all(piece.as_bytes());
                }
            });
        }
    });
    (base, bodies)
}

fn chunk(delta: serde_json::Value, finish: Option<&str>) -> String {
    let v = serde_json::json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    format!("data: {v}\n\n")
}

/// The planner (no tools offered for writing) answers with a plan; the
/// coding task writes a file, tries a command it may not run, and ends.
fn scripted(req: &serde_json::Value) -> Vec<String> {
    let text = req.to_string();
    let tools: Vec<String> = req["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let results = req["messages"]
        .as_array()
        .map_or(0, |m| m.iter().filter(|x| x["role"] == "tool").count());
    let call = |id: &str, name: &str, args: serde_json::Value| {
        vec![
            chunk(
                serde_json::json!({"tool_calls": [{"index": 0, "id": id, "type": "function",
                    "function": {"name": name, "arguments": args.to_string()}}]}),
                Some("tool_calls"),
            ),
            "data: [DONE]\n\n".to_owned(),
        ]
    };
    let say = |s: &str| {
        vec![
            chunk(serde_json::json!({"content": s}), Some("stop")),
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10}}\n\n"
                .to_owned(),
            "data: [DONE]\n\n".to_owned(),
        ]
    };
    if text.contains("Answer with only this JSON") {
        assert!(
            !tools.contains(&"write_file".to_owned()),
            "the planner only reads"
        );
        return say(
            r#"{"summary": "Write a file", "tasks": [{"kind": "coding", "prompt": "Write hello into out.txt."}]}"#,
        );
    }
    match results {
        0 => call(
            "c1",
            "write_file",
            serde_json::json!({"path": "out.txt", "content": "hello from the api\n"}),
        ),
        1 => call(
            "c2",
            "run_command",
            serde_json::json!({"command": "ls -la"}),
        ),
        _ => say("Wrote out.txt."),
    }
}

#[test]
fn an_api_model_plans_and_edits_unattended() {
    let (base, bodies) = mock_provider(scripted);
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = config(&format!(
        "[agents.providers.lab]\nkind = \"openai-compatible\"\nbase-url = \"{base}/v1\"\n\
         [agents.providers.lab.prices.\"fixture-model\"]\ninput = 1.0\noutput = 2.0\n\
         [harness]\nauto-run = 3\nbudget-usd = 5.0\n\
         [harness.backends.lab]\nmodels = {{ balanced = \"fixture-model\" }}\n"
    ));
    let ctx = context(dir.path(), cfg, Arc::new(NoClis), true);
    let project = ctx.project.clone();
    let h = Harness::start(ctx).expect("start");
    h.plan("write a greeting");
    // One task: within auto-run, so it starts without approval.
    let v = wait(&h, "the run", |v| matches!(v.phase, Phase::Finished { .. }));
    assert_eq!(v.phase, Phase::Finished { failed: 0 }, "{:#?}", v.tasks);
    let t = &v.tasks[0];
    assert!(
        t.by.as_deref()
            .is_some_and(|b| b.starts_with("lab (API) · fixture-model")),
        "{:?}",
        t.by
    );
    assert_eq!(t.result.as_deref(), Some("Wrote out.txt."));
    assert!(
        t.log.iter().any(|l| l.contains("run_command")),
        "{:?}",
        t.log
    );
    assert!(t.cost.is_some_and(|c| c > 0.0) && v.spent > 0.0);
    assert_eq!(
        std::fs::read_to_string(project.join("out.txt")).expect("written"),
        "hello from the api\n"
    );
    // The command it may not run was refused, and the model was told.
    let sent = bodies.lock().expect("lock").clone();
    assert!(
        sent.iter()
            .any(|b| b.contains("not one of the commands allowed")),
        "{sent:?}"
    );
    // Routes skipped the CLIs, which are not there.
    assert!(
        t.route
            .skipped
            .iter()
            .any(|(n, w)| n == "Claude Code" && w == "not installed")
    );
}

#[test]
fn a_spent_budget_and_missing_backends_stop_a_task_with_reasons() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = context(dir.path(), config(""), Arc::new(NoClis), false);
    let h = Harness::start(ctx).expect("start");
    h.plan("anything");
    let v = wait(&h, "planning to fail", |v| {
        matches!(v.phase, Phase::Failed(_))
    });
    let Phase::Failed(why) = v.phase else {
        unreachable!()
    };
    assert!(why.contains("not Claude Code: not installed"), "{why}");
    // Editing a plan keeps it valid.
    let plan: Plan = serde_json::from_value(serde_json::json!({"goal": "g", "summary": "",
        "tasks": [{"id": "a", "kind": "coding", "title": "a", "prompt": "x"},
                  {"id": "b", "kind": "review", "title": "b", "prompt": "y", "depends_on": ["a"]}]}))
    .expect("plan");
    h.load(plan).expect("load");
    assert!(
        h.edit(|p| p.tasks.swap(0, 1)).is_err(),
        "b would come before what it needs"
    );
    h.edit(|p| p.tasks[1].prompt = "check it twice".into())
        .expect("edit");
    assert_eq!(
        h.view.lock().expect("v").tasks[1].task.prompt,
        "check it twice"
    );
}

#[cfg(unix)]
#[test]
fn a_review_goes_to_another_backend_than_its_author() {
    for cross in [true, false] {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(bin.join("answers")).expect("bin");
        stand_in(&bin, "claude", &["task t1 ", "task t2 "]);
        stand_in(&bin, "codex", &["task t2 "]);
        let answers = bin.join("answers");
        std::fs::write(
            answers.join("claude-task t1 "),
            claude_result("Wrote it.", false),
        )
        .expect("w");
        std::fs::write(
            answers.join("claude-task t2 "),
            claude_result("Mine looks fine.", false),
        )
        .expect("w");
        std::fs::write(
            answers.join("codex-task t2 "),
            codex_result("Found an off-by-one."),
        )
        .expect("w");
        let plan: Plan = serde_json::from_value(serde_json::json!({"goal": "g", "summary": "s", "tasks": [
            {"id": "t1", "kind": "coding", "title": "a", "prompt": "write"},
            {"id": "t2", "kind": "review", "title": "b", "prompt": "review", "depends_on": ["t1"]}]}))
        .expect("plan");
        let ctx = context(
            dir.path(),
            config(&format!("[harness]\ncross-review = {cross}\n")),
            Arc::new(StandIns { bin: bin.clone() }),
            false,
        );
        let h = Harness::start(ctx).expect("start");
        h.load(plan).expect("load");
        h.run();
        let v = wait(&h, "the run", |v| matches!(v.phase, Phase::Finished { .. }));
        assert_eq!(v.tasks[0].backend.as_deref(), Some("claude"));
        let reviewer = if cross { "codex" } else { "claude" };
        assert_eq!(
            v.tasks[1].backend.as_deref(),
            Some(reviewer),
            "cross-review = {cross}"
        );
    }
}

#[cfg(unix)]
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The project as a git repository with `files`, committed on `main`.
#[cfg(unix)]
fn repository(project: &Path, files: &[(&str, &str)]) {
    git(project, &["init", "-q", "-b", "main"]);
    git(project, &["config", "user.name", "Fixture"]);
    git(project, &["config", "user.email", "fixture@example.com"]);
    git(project, &["config", "commit.gpgsign", "false"]);
    for (name, body) in files {
        std::fs::write(project.join(name), body).expect("write");
    }
    git(project, &["add", "-A"]);
    git(project, &["commit", "-q", "-m", "first"]);
}

/// In a git repository (D-113): two edits at the same time, each in its
/// own worktree, merged into the plan's branch, which the review sees;
/// the project itself stays as it was.
#[cfg(unix)]
#[test]
fn edits_run_at_once_in_worktrees_and_land_on_the_plans_branch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(bin.join("answers")).expect("bin");
    stand_in(
        &bin,
        "claude",
        &[
            "Answer with only this JSON",
            "task t1 ",
            "task t2 ",
            "task t3 ",
        ],
    );
    let plan = serde_json::json!({"summary": "Two notes", "tasks": [
        {"id": "t1", "kind": "coding", "title": "First", "prompt": "Add to a.txt."},
        {"id": "t2", "kind": "coding", "title": "Second", "prompt": "Add to b.txt."},
        {"id": "t3", "kind": "review", "title": "Review", "prompt": "Review both.", "depends_on": ["t1", "t2"]}
    ]})
    .to_string();
    let answers = bin.join("answers");
    std::fs::write(
        answers.join("claude-Answer with only this JSON"),
        claude_result(&plan, false),
    )
    .expect("w");
    for t in ["t1", "t2", "t3"] {
        std::fs::write(
            answers.join(format!("claude-task {t} ")),
            claude_result("done", false),
        )
        .expect("w");
    }
    // Each waits a while for the other to start: they overlap only if
    // they run at the same time.
    let b = bin.display();
    script(
        &bin.join("do-claude-task t1 "),
        &format!(
            "touch '{b}/t1-on'; i=0; while [ $i -lt 50 ] && [ ! -e '{b}/t2-on' ]; do sleep 0.1; i=$((i+1)); done\n\
             [ -e '{b}/t2-on' ] && touch '{b}/overlapped'\necho first >> a.txt\npwd > '{b}/t1-dir'\n"
        ),
    );
    script(
        &bin.join("do-claude-task t2 "),
        &format!(
            "touch '{b}/t2-on'; i=0; while [ $i -lt 50 ] && [ ! -e '{b}/t1-on' ]; do sleep 0.1; i=$((i+1)); done\n\
             echo second >> b.txt\n"
        ),
    );
    script(
        &bin.join("do-claude-task t3 "),
        &format!("cat a.txt b.txt > '{b}/review-saw'\npwd > '{b}/t3-dir'\n"),
    );
    let ctx = context(
        dir.path(),
        config(""),
        Arc::new(StandIns { bin: bin.clone() }),
        false,
    );
    let project = ctx.project.clone();
    repository(&project, &[("a.txt", "a\n"), ("b.txt", "b\n")]);
    std::fs::write(project.join("scratch.txt"), "not committed\n").expect("w");
    let data = paths(dir.path()).data_dir;
    let h = Harness::start(ctx).expect("start");
    h.plan("two notes");
    wait(&h, "the plan", |v| v.phase == Phase::Review);
    h.run();
    let v = wait(&h, "the run", |v| matches!(v.phase, Phase::Finished { .. }));
    assert_eq!(v.phase, Phase::Finished { failed: 0 }, "{:#?}", v.tasks);
    assert!(
        bin.join("overlapped").exists(),
        "the edits ran at the same time"
    );
    let tree = v.worktree.clone().expect("the plan's worktree");
    assert!(tree.branch.starts_with("termos/plan-"), "{tree:?}");
    // Each edit worked in a worktree of its own; the review in the plan's.
    let t1_dir = std::fs::read_to_string(bin.join("t1-dir")).expect("t1");
    assert!(t1_dir.trim().ends_with("-t1"), "{t1_dir}");
    let t3_dir = std::fs::read_to_string(bin.join("t3-dir")).expect("t3");
    assert_eq!(Path::new(t3_dir.trim()).file_name(), tree.path.file_name());
    assert_eq!(
        std::fs::read_to_string(bin.join("review-saw")).expect("review"),
        "a\nfirst\nb\nsecond\n"
    );
    // Both on the plan's branch; the project as it was; the tasks'
    // worktrees gone.
    let log = git(&tree.path, &["log", "--format=%s"]);
    assert!(
        log.contains("t1: First") && log.contains("t2: Second"),
        "{log}"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).expect("a"),
        "a\n"
    );
    assert!(
        v.notes.iter().any(|n| n.contains("not committed")),
        "{:?}",
        v.notes
    );
    let store = termos_devkit::worktrees::Store::new(&data, &Default::default());
    let left: Vec<String> = store
        .list()
        .expect("list")
        .into_iter()
        .filter(|w| w.active())
        .map(|w| w.name)
        .collect();
    assert_eq!(left, std::slice::from_ref(&tree.name));
    assert!(
        v.tasks[0]
            .log
            .iter()
            .any(|l| l.starts_with("merged into termos/plan-"))
    );
}

/// Two edits of one line: the second to finish conflicts, runs again on
/// top of the first, and lands.
#[cfg(unix)]
#[test]
fn a_conflicting_edit_runs_again_on_top() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(bin.join("answers")).expect("bin");
    stand_in(
        &bin,
        "claude",
        &["Answer with only this JSON", "task t1 ", "task t2 "],
    );
    let plan = serde_json::json!({"summary": "One line, twice", "tasks": [
        {"id": "t1", "kind": "coding", "title": "One", "prompt": "Change the line."},
        {"id": "t2", "kind": "coding", "title": "Two", "prompt": "Change the line too."}
    ]})
    .to_string();
    let answers = bin.join("answers");
    std::fs::write(
        answers.join("claude-Answer with only this JSON"),
        claude_result(&plan, false),
    )
    .expect("w");
    for t in ["t1", "t2"] {
        std::fs::write(
            answers.join(format!("claude-task {t} ")),
            claude_result("done", false),
        )
        .expect("w");
        script(
            &bin.join(format!("do-claude-task {t} ")),
            &format!("printf 'one\\n{t}\\nthree\\n' > a.txt\n"),
        );
    }
    let ctx = context(
        dir.path(),
        config(""),
        Arc::new(StandIns { bin: bin.clone() }),
        false,
    );
    repository(&ctx.project, &[("a.txt", "one\ntwo\nthree\n")]);
    let h = Harness::start(ctx).expect("start");
    h.plan("one line");
    wait(&h, "the plan", |v| v.phase == Phase::Review);
    h.run();
    let v = wait(&h, "the run", |v| matches!(v.phase, Phase::Finished { .. }));
    assert_eq!(v.phase, Phase::Finished { failed: 0 }, "{:#?}", v.tasks);
    let again: Vec<&str> = v
        .tasks
        .iter()
        .filter(|t| t.log.iter().any(|l| l.contains("again on top")))
        .map(|t| t.task.id.as_str())
        .collect();
    assert_eq!(again.len(), 1, "{:#?}", v.tasks);
    let prompts = std::fs::read_to_string(bin.join("prompts-claude")).expect("prompts");
    assert!(prompts.contains("A first attempt at this task conflicted"));
    // The one that ran again is last on the line.
    let tree = v.worktree.expect("worktree");
    let a = std::fs::read_to_string(tree.path.join("a.txt")).expect("a");
    assert_eq!(a, format!("one\n{}\nthree\n", again[0]));
}
