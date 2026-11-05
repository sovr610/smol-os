// SPDX-License-Identifier: GPL-3.0-or-later
//! The harness: planning, approving and running a plan (D-099, D-100).
//!
//! A [`Harness`] owns a worker thread (its own single-threaded tokio
//! runtime, as agent sessions have) and a shared [`RunView`] that the
//! window and `termos harness run` read. The worker:
//!
//! - **plans:** the planning route's backend, read-only, answers with a
//!   JSON plan ([`crate::plan`]); the plan then waits for approval, unless
//!   it is small enough for `[harness] auto-run`;
//! - **runs:** a task starts when the tasks it depends on are done, up to
//!   `[harness] parallel` at a time. In a git repository (D-113) the plan
//!   gets a worktree and branch of its own; each task that changes files
//!   runs in a worktree of its own made from that branch, and its changes
//!   are committed and merged into it when it finishes (a conflict runs
//!   the task again on top, once; a second stops the plan). Read-only
//!   tasks look at the plan's worktree. The result is reviewed there.
//!   Elsewhere (or with `[harness] worktrees = false`) tasks that change
//!   files run one at a time in the project, after every earlier one;
//! - **routes each task when it starts:** a backend that cannot run, has
//!   spent the plan's API budget, or whose CLI reached its plan limit in
//!   this run is passed over. A CLI that reaches its limit is not used
//!   again in the plan under any account (D-096). A failed attempt falls
//!   back to the next backend (`[harness] fallback`);
//! - **passes results on:** a task's prompt ends with the results of the
//!   tasks it depends on;
//! - **records every hand-off** in a hash-chained audit log
//!   (`harness-<plan>` in the audit folder) and in the command center's
//!   journal.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use termos_ai::audit::{self, AuditLog};
use termos_core::app::AppWaker;
use termos_core::config::Config;
use termos_core::config::harness::{BackendKind, TaskKind};
use termos_core::journal::{self, Journal};
use termos_devkit::worktrees::{self, NewWorktree, Repo, Store, Worktree, WorktreeError};
use tokio::sync::{mpsc, watch};

use crate::backends::{self, ApiContext, Attempt, Outcome, Programs, Progress};
use crate::events::{self, Tokens};
use crate::plan::{self, Plan, Task};
use crate::route::{self, Availability, Backend, Choice, Route};

/// Everything the harness works with.
#[derive(Clone)]
pub struct Context {
    pub config: Config,
    pub programs: Arc<dyn Programs>,
    /// TermOS's agent, for API backends (`None`: none here).
    pub api: Option<ApiContext>,
    /// Where the audit logs go (`<state>/audit`).
    pub state_dir: PathBuf,
    pub journal: Option<Arc<Journal>>,
    /// The project folder.
    pub project: PathBuf,
    /// TermOS's data folder, where worktrees are recorded (`None`: tasks
    /// edit the project in place).
    pub data_dir: Option<PathBuf>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// Where a task is.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
    Waiting,
    Running,
    Done,
    Failed(String),
    /// Not run: why.
    Skipped(String),
    Stopped,
}

impl TaskState {
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Waiting | Self::Running)
    }
}

/// A task and what happened to it.
#[derive(Debug, Clone)]
pub struct TaskView {
    pub task: Task,
    /// Where it would go now (planned), or went.
    pub route: Route,
    pub state: TaskState,
    /// The backend running it, or that did it ("Claude Code · fixture").
    pub by: Option<String>,
    /// That backend's name (`claude`).
    pub backend: Option<String>,
    /// Earlier attempts that failed: backend and why.
    pub tried: Vec<String>,
    /// What it did, newest last.
    pub log: VecDeque<String>,
    /// Its answer, or the error.
    pub result: Option<String>,
    pub tokens: Tokens,
    /// US dollars charged (API backends).
    pub cost: Option<f64>,
    /// A CLI's own estimate (not charged under a plan).
    pub estimate: Option<f64>,
    pub session: Option<String>,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
}

impl TaskView {
    pub(crate) fn new(task: Task, route: Route) -> Self {
        Self {
            task,
            route,
            state: TaskState::Waiting,
            by: None,
            backend: None,
            tried: Vec::new(),
            log: VecDeque::new(),
            result: None,
            tokens: Tokens::default(),
            cost: None,
            estimate: None,
            session: None,
            started: None,
            finished: None,
        }
    }

    /// How long it ran (or has been running).
    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        let start = self.started?;
        Some(
            self.finished
                .unwrap_or(now)
                .saturating_duration_since(start),
        )
    }

    fn note(&mut self, line: String) {
        if self.log.len() >= 200 {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }
}

/// Where the harness is.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// Waiting for a goal.
    Idle,
    /// The planner is at work (on this backend).
    Planning(String),
    /// A plan waits for approval.
    Review,
    Running,
    /// The plan ran: how many tasks failed or did not run.
    Finished {
        failed: usize,
    },
    /// Planning failed.
    Failed(String),
}

/// What the window shows.
#[derive(Debug, Clone)]
pub struct RunView {
    pub phase: Phase,
    /// The plan's id (its audit log is `harness-<id>`).
    pub id: String,
    pub goal: String,
    pub summary: String,
    pub tasks: Vec<TaskView>,
    /// API dollars spent on this plan.
    pub spent: f64,
    /// Who planned it.
    pub planner: Option<String>,
    /// The planner's answer when it was not a plan.
    pub raw_plan: Option<String>,
    /// Warnings about settings.
    pub notes: Vec<String>,
    /// The plan's worktree, where its result is (D-113).
    pub worktree: Option<PlanTree>,
    /// Bumped on every change.
    pub generation: u64,
}

/// The worktree a plan's changes are merged into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTree {
    pub name: String,
    pub branch: String,
    pub path: PathBuf,
}

impl RunView {
    pub(crate) fn new() -> Self {
        Self {
            phase: Phase::Idle,
            id: String::new(),
            goal: String::new(),
            summary: String::new(),
            tasks: Vec::new(),
            spent: 0.0,
            planner: None,
            raw_plan: None,
            notes: Vec::new(),
            worktree: None,
            generation: 0,
        }
    }

    /// The plan as it stands (after the user's edits).
    pub fn plan(&self) -> Plan {
        Plan {
            goal: self.goal.clone(),
            summary: self.summary.clone(),
            tasks: self.tasks.iter().map(|t| t.task.clone()).collect(),
        }
    }

    pub fn tokens(&self) -> Tokens {
        let mut t = Tokens::default();
        for v in &self.tasks {
            t.add(v.tokens);
        }
        t
    }
}

/// What the window asks the worker.
#[derive(Debug)]
enum Command {
    Plan(String),
    Run,
    Stop(Option<String>),
}

/// The window's (or the command line's) end of the harness.
#[derive(Clone)]
pub struct Harness {
    pub view: Arc<Mutex<RunView>>,
    ctx: Arc<Context>,
    tx: mpsc::UnboundedSender<Command>,
    waker: Arc<Mutex<Option<AppWaker>>>,
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Harness")
    }
}

/// The parts the worker and the tasks share.
#[derive(Clone)]
struct Shared {
    view: Arc<Mutex<RunView>>,
    waker: Arc<Mutex<Option<AppWaker>>>,
}

impl Shared {
    fn update<T>(&self, f: impl FnOnce(&mut RunView) -> T) -> Option<T> {
        let out = self.view.lock().ok().map(|mut v| {
            let out = f(&mut v);
            v.generation += 1;
            out
        });
        if let Ok(w) = self.waker.lock()
            && let Some(w) = w.as_ref()
        {
            w.wake();
        }
        out
    }

    fn read<T>(&self, f: impl FnOnce(&RunView) -> T) -> Option<T> {
        self.view.lock().ok().map(|v| f(&v))
    }
}

impl Harness {
    /// Starts the worker.
    pub fn start(ctx: Context) -> std::io::Result<Self> {
        let ctx = Arc::new(ctx);
        let view = Arc::new(Mutex::new(RunView::new()));
        let waker: Arc<Mutex<Option<AppWaker>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Shared {
            view: Arc::clone(&view),
            waker: Arc::clone(&waker),
        };
        let notes = settings_notes(&ctx.config);
        shared.update(|v| v.notes = notes);
        let worker_ctx = Arc::clone(&ctx);
        std::thread::Builder::new()
            .name("termos-harness".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        shared.update(|v| v.phase = Phase::Failed(format!("cannot start: {e}")));
                        return;
                    }
                };
                // Tasks run on this thread: TermOS's agent sessions are
                // not `Send`.
                let local = tokio::task::LocalSet::new();
                rt.block_on(local.run_until(worker(worker_ctx, shared, rx)));
            })?;
        Ok(Self {
            view,
            ctx,
            tx,
            waker,
        })
    }

    pub fn set_waker(&self, w: AppWaker) {
        if let Ok(mut g) = self.waker.lock() {
            *g = Some(w);
        }
    }

    fn shared(&self) -> Shared {
        Shared {
            view: Arc::clone(&self.view),
            waker: Arc::clone(&self.waker),
        }
    }

    pub fn project(&self) -> &Path {
        &self.ctx.project
    }

    pub fn config(&self) -> &Config {
        &self.ctx.config
    }

    /// Makes a plan for `goal` (replacing one not yet run).
    pub fn plan(&self, goal: &str) {
        let _ = self.tx.send(Command::Plan(goal.trim().to_owned()));
    }

    /// Uses `plan` as it is (`termos harness run --plan`), for review.
    pub fn load(&self, plan: Plan) -> Result<(), String> {
        plan.check(self.ctx.config.harness.max_tasks)
            .map_err(|e| e.to_string())?;
        let routes = planned_routes(&self.ctx, &plan.tasks);
        self.shared().update(|v| {
            *v = RunView {
                notes: std::mem::take(&mut v.notes),
                ..RunView::new()
            };
            v.id = termos_ai::history::new_id();
            v.goal.clone_from(&plan.goal);
            v.summary.clone_from(&plan.summary);
            v.tasks = plan
                .tasks
                .into_iter()
                .zip(routes)
                .map(|(t, r)| TaskView::new(t, r))
                .collect();
            v.phase = Phase::Review;
        });
        Ok(())
    }

    /// Runs the plan under review.
    pub fn run(&self) {
        let _ = self.tx.send(Command::Run);
    }

    /// Stops one task (`Some(id)`) or the whole run.
    pub fn stop(&self, task: Option<&str>) {
        let _ = self.tx.send(Command::Stop(task.map(str::to_owned)));
    }

    /// Changes the plan under review; the routes are worked out again.
    /// Refused (with why) when the result is not a valid plan.
    pub fn edit(&self, f: impl FnOnce(&mut Plan)) -> Result<(), String> {
        let mut plan = self
            .shared()
            .read(|v| (v.phase == Phase::Review).then(|| v.plan()))
            .flatten()
            .ok_or("only a plan waiting to run can be changed")?;
        f(&mut plan);
        plan.check(self.ctx.config.harness.max_tasks)
            .map_err(|e| e.to_string())?;
        let routes = planned_routes(&self.ctx, &plan.tasks);
        self.shared().update(|v| {
            v.tasks = plan
                .tasks
                .into_iter()
                .zip(routes)
                .map(|(t, r)| TaskView::new(t, r))
                .collect();
        });
        Ok(())
    }

    /// Forgets the plan.
    pub fn clear(&self) {
        let busy = self
            .shared()
            .read(|v| matches!(v.phase, Phase::Planning(_) | Phase::Running))
            .unwrap_or(false);
        if !busy {
            self.shared().update(|v| {
                *v = RunView {
                    notes: std::mem::take(&mut v.notes),
                    ..RunView::new()
                };
            });
        }
    }
}

/// Settings that point at nothing.
fn settings_notes(config: &Config) -> Vec<String> {
    route::unknown_names(config)
        .into_iter()
        .map(|n| format!("[harness] names `{n}`, which is not a backend"))
        .collect()
}

/// What blocks a backend now.
struct Live<'a> {
    ctx: &'a Context,
    /// CLIs that reached their plan limit in this run.
    exhausted: &'a BTreeSet<String>,
    spent: f64,
}

fn kind_key(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Claude => "claude",
        BackendKind::Codex => "codex",
        BackendKind::Api => "api",
    }
}

impl Availability for Live<'_> {
    fn unavailable(&self, b: &Backend) -> Option<String> {
        match b.kind {
            BackendKind::Claude | BackendKind::Codex => {
                if self.exhausted.contains(kind_key(b.kind)) {
                    return Some(
                        "it reached its plan's limit in this run (TermOS does not switch accounts by itself)"
                            .into(),
                    );
                }
                self.ctx.programs.cli(b.kind, b.account.as_deref()).err()
            }
            BackendKind::Api => {
                if self.ctx.api.is_none() {
                    return Some("TermOS's agent is not available here".into());
                }
                let provider = b.provider.as_deref().unwrap_or(&b.name);
                if !self.ctx.config.agents.providers.contains_key(provider) {
                    return Some(format!(
                        "no provider named {provider} in [agents.providers]"
                    ));
                }
                let budget = self.ctx.config.harness.budget_usd;
                (budget > 0.0 && self.spent >= budget).then(|| {
                    format!("the plan's API budget (${budget:.2}, [harness] budget-usd) is spent")
                })
            }
        }
    }
}

/// Where each task would go if the plan ran now.
fn planned_routes(ctx: &Context, tasks: &[Task]) -> Vec<Route> {
    let all = route::backends(&ctx.config);
    let none = BTreeSet::new();
    let live = Live {
        ctx,
        exhausted: &none,
        spent: 0.0,
    };
    tasks
        .iter()
        .map(|t| {
            route::route(
                &ctx.config.harness,
                &all,
                t.kind,
                t.backend.as_deref(),
                &live,
            )
        })
        .collect()
}

/// The commands tasks that change files may run: the settings' and the
/// project's own build, test, lint, check and format tasks.
pub fn allowed_commands(config: &Config, project: &Path) -> Vec<String> {
    let mut out: Vec<String> = config.harness.allow_commands.clone();
    if config.harness.allow_project_tasks
        && let Ok(Some(p)) = termos_devkit::tasks::find(project)
    {
        for (name, command) in &p.tasks {
            if !["build", "test", "lint", "check", "fmt", "format"].contains(&name.as_str()) {
                continue;
            }
            for seg in termos_ai::policy::classify(command).segments {
                out.push(seg.clone());
                out.push(format!("{seg} *"));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A task's prompt: its own, what plan it belongs to, and the results it
/// depends on.
fn task_prompt(goal: &str, project: &Path, task: &Task, inputs: &[(String, String)]) -> String {
    let mut p = task.prompt.clone();
    p.push_str(&format!(
        "\n\nThis is task {} ({}) of a plan for: {goal}\nProject folder: {}\n",
        task.id,
        task.kind.name(),
        project.display()
    ));
    if !task.kind.writes() {
        p.push_str("Do not change any files.\n");
    }
    if !inputs.is_empty() {
        p.push_str("\nResults of the tasks this one builds on:\n");
        for (head, result) in inputs {
            let short: String = result.chars().take(1500).collect();
            let cut = if result.chars().count() > 1500 {
                " […]"
            } else {
                ""
            };
            p.push_str(&format!("- {head}: {short}{cut}\n"));
        }
    }
    p
}

/// The worker: one command at a time; a run listens for stops.
async fn worker(ctx: Arc<Context>, shared: Shared, mut rx: mpsc::UnboundedReceiver<Command>) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Plan(goal) => {
                let busy = shared
                    .read(|v| matches!(v.phase, Phase::Running))
                    .unwrap_or(true);
                if busy || goal.is_empty() {
                    continue;
                }
                let start = make_plan(&ctx, &shared, &goal).await;
                if start {
                    execute(&ctx, &shared, &mut rx).await;
                }
            }
            Command::Run => {
                let ready = shared
                    .read(|v| v.phase == Phase::Review && !v.tasks.is_empty())
                    .unwrap_or(false);
                if ready {
                    execute(&ctx, &shared, &mut rx).await;
                }
            }
            Command::Stop(_) => {}
        }
    }
}

/// Opens the plan's audit log (continuing it: planning and running
/// write to the same chain).
fn open_records(ctx: &Context, id: &str) -> Records {
    let log = match AuditLog::open(&audit::dir(&ctx.state_dir), &format!("harness-{id}")) {
        Ok(mut l) => {
            if l.head() == audit::GENESIS {
                let _ = l.append(audit::Event::Session {
                    profile: "harness".into(),
                    provider: "harness".into(),
                    model: String::new(),
                    workspace: ctx.project.display().to_string(),
                    sandbox: "each backend's own".into(),
                });
            }
            Some(Arc::new(Mutex::new(l)))
        }
        Err(e) => {
            tracing::warn!("harness: no audit log: {e}");
            None
        }
    };
    Records {
        log,
        journal: ctx.journal.clone(),
        plan: id.to_owned(),
    }
}

/// Where the hand-off records go.
#[derive(Clone)]
struct Records {
    log: Option<Arc<Mutex<AuditLog>>>,
    journal: Option<Arc<Journal>>,
    plan: String,
}

/// One hand-off: a task, the backend it went to and what came of it.
struct Handoff<'a> {
    task: &'a str,
    kind: TaskKind,
    /// The backend's name, model and account (empty when none).
    backend: &'a str,
    model: &'a str,
    account: &'a str,
}

impl Handoff<'_> {
    fn of<'a>(
        task: &'a str,
        kind: TaskKind,
        c: &'a Choice,
        account: Option<&'a str>,
    ) -> Handoff<'a> {
        Handoff {
            task,
            kind,
            backend: &c.backend.name,
            model: c.model.as_deref().unwrap_or(""),
            account: account.or(c.backend.account.as_deref()).unwrap_or(""),
        }
    }
}

impl Records {
    /// Writes a hand-off to the audit log and the journal.
    fn write(&self, h: &Handoff<'_>, outcome: &str, detail: &str) {
        let detail = termos_ai::redact::Redactor::new().text(&events::one_line(detail, 300));
        if let Some(l) = &self.log
            && let Ok(mut l) = l.lock()
            && let Err(e) = l.append(audit::Event::Handoff {
                task: h.task.to_owned(),
                task_kind: h.kind.name().to_owned(),
                backend: h.backend.to_owned(),
                model: h.model.to_owned(),
                account: h.account.to_owned(),
                outcome: outcome.to_owned(),
                detail,
            })
        {
            tracing::warn!("harness: cannot write the audit log: {e}");
        }
        if let Some(j) = &self.journal {
            j.record(journal::Event::Handoff {
                plan: self.plan.clone(),
                task: h.task.to_owned(),
                task_kind: h.kind.name().to_owned(),
                backend: h.backend.to_owned(),
                model: h.model.to_owned(),
                outcome: outcome.to_owned(),
            });
        }
    }

    fn note(&self, text: String) {
        if let Some(l) = &self.log
            && let Ok(mut l) = l.lock()
        {
            let _ = l.append(audit::Event::Note { text });
        }
    }
}

/// Runs `prompt` on a route's backends in turn until one answers.
#[allow(clippy::too_many_arguments)]
async fn attempt_all(
    ctx: &Arc<Context>,
    shared: &Shared,
    index: Option<usize>,
    task_id: &str,
    kind: TaskKind,
    pinned: Option<&str>,
    prompt: String,
    exhausted: &Arc<Mutex<BTreeSet<String>>>,
    records: &Records,
    // Backends to try after the others (a review's authors).
    others_first: &[String],
    stop: watch::Receiver<bool>,
    // Where it works: the project, or a worktree.
    project: &Path,
) -> (Outcome, Option<Choice>) {
    let all = route::backends(&ctx.config);
    let mut tried: Vec<String> = Vec::new();
    let mut last = Outcome {
        text: "no backend can take it".into(),
        ..Outcome::default()
    };
    // The route is worked out afresh before each try: an earlier try
    // may have spent the budget or reached a plan limit.
    loop {
        let spent = shared.read(|v| v.spent).unwrap_or(0.0);
        let ex = exhausted.lock().map(|e| e.clone()).unwrap_or_default();
        let mut r = route::route(
            &ctx.config.harness,
            &all,
            kind,
            pinned,
            &Live {
                ctx,
                exhausted: &ex,
                spent,
            },
        );
        if !others_first.is_empty() {
            // Stable: the authors keep their order, after the rest.
            r.choices
                .sort_by_key(|c| others_first.contains(&c.backend.name));
        }
        let Some(choice) = r
            .choices
            .iter()
            .find(|c| !tried.contains(&c.backend.name))
            .cloned()
        else {
            if tried.is_empty() {
                last.text = r.explain();
            }
            return (last, None);
        };
        if !tried.is_empty() && !ctx.config.harness.fallback {
            return (last, None);
        }
        tried.push(choice.backend.name.clone());
        let what = choice.describe();
        if let Some(i) = index {
            shared.update(|v| {
                let t = &mut v.tasks[i];
                t.route = r.clone();
                t.state = TaskState::Running;
                t.by = Some(what.clone());
                t.started.get_or_insert_with(Instant::now);
                t.note(format!("→ {what}"));
            });
        } else {
            shared.update(|v| v.phase = Phase::Planning(what.clone()));
        }
        records.write(
            &Handoff::of(task_id, kind, &choice, None),
            "started",
            &r.explain(),
        );
        let progress_shared = shared.clone();
        let progress: Arc<dyn Fn(Progress) + Send + Sync> = Arc::new(move |p| {
            let line = match p {
                Progress::Tool { name, detail } if detail.is_empty() => format!("· {name}"),
                Progress::Tool { name, detail } => format!("· {name} {detail}"),
                Progress::Text(t) => format!("  {t}"),
            };
            if let Some(i) = index {
                progress_shared.update(|v| v.tasks[i].note(line));
            }
        });
        let attempt = Attempt {
            choice: choice.clone(),
            prompt: prompt.clone(),
            writes: kind.writes(),
            project: project.to_owned(),
            commands: if kind.writes() {
                allowed_commands(&ctx.config, project)
            } else {
                Vec::new()
            },
            timeout: Duration::from_secs(ctx.config.harness.task_minutes.max(1) * 60),
        };
        let out = backends::run(
            &attempt,
            ctx.programs.as_ref(),
            ctx.api.as_ref(),
            progress,
            stop.clone(),
        )
        .await;
        if let Some(c) = out.cost {
            shared.update(|v| v.spent += c);
        }
        let account = out.account.clone();
        if out.ok || out.stopped {
            let outcome = if out.ok { "done" } else { "stopped" };
            records.write(
                &Handoff::of(task_id, kind, &choice, account.as_deref()),
                outcome,
                &out.text,
            );
            return (out, Some(choice));
        }
        if out.limit
            && choice.backend.is_subscription()
            && let Ok(mut e) = exhausted.lock()
        {
            e.insert(kind_key(choice.backend.kind).to_owned());
        }
        records.write(
            &Handoff::of(task_id, kind, &choice, account.as_deref()),
            "failed",
            &out.text,
        );
        let line = format!(
            "{}: {}",
            choice.backend.title(),
            events::one_line(&out.text, 200)
        );
        if let Some(i) = index {
            shared.update(|v| {
                v.tasks[i].tried.push(line.clone());
                v.tasks[i].note(format!("✗ {line}"));
                v.tasks[i].tokens.add(out.tokens);
            });
        }
        last = out;
        if *stop.borrow() {
            last.stopped = true;
            return (last, None);
        }
    }
}

/// Plans `goal`; true when the plan may start at once.
async fn make_plan(ctx: &Arc<Context>, shared: &Shared, goal: &str) -> bool {
    let id = termos_ai::history::new_id();
    shared.update(|v| {
        *v = RunView {
            notes: std::mem::take(&mut v.notes),
            ..RunView::new()
        };
        v.id.clone_from(&id);
        v.goal = goal.to_owned();
        v.phase = Phase::Planning(String::new());
    });
    let records = open_records(ctx, &id);
    records.note(format!("goal: {}", events::one_line(goal, 300)));
    let h = &ctx.config.harness;
    let prompt = plan::planner_prompt(goal, &ctx.project, h.max_tasks);
    let exhausted = Arc::new(Mutex::new(BTreeSet::new()));
    let (_never, stop) = watch::channel(false);
    let (out, choice) = attempt_all(
        ctx,
        shared,
        None,
        "plan",
        TaskKind::Planning,
        h.planner.as_deref(),
        prompt,
        &exhausted,
        &records,
        &[],
        stop,
        &ctx.project,
    )
    .await;
    if !out.ok {
        shared.update(|v| v.phase = Phase::Failed(format!("planning failed: {}", out.text)));
        return false;
    }
    match plan::parse(goal, &out.text, h.max_tasks) {
        Ok(p) => {
            let routes = planned_routes(ctx, &p.tasks);
            let auto = h.auto_run > 0 && p.tasks.len() <= h.auto_run;
            records.note(format!(
                "plan: {} ({})",
                p.summary,
                p.tasks
                    .iter()
                    .map(|t| format!("{} {}", t.id, t.kind.name()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            shared.update(|v| {
                v.summary.clone_from(&p.summary);
                v.planner = choice.map(|c| c.describe());
                v.tasks = p
                    .tasks
                    .into_iter()
                    .zip(routes)
                    .map(|(t, r)| TaskView::new(t, r))
                    .collect();
                v.spent += out.cost.unwrap_or(0.0);
                v.phase = Phase::Review;
            });
            auto
        }
        Err(e) => {
            shared.update(|v| {
                v.raw_plan = Some(out.text.clone());
                v.phase = Phase::Failed(e.to_string());
            });
            false
        }
    }
}

/// A plan's worktree, and where its tasks' worktrees come from (D-113).
struct PlanTrees {
    store: Store,
    repo: Repo,
    plan: Worktree,
    /// Merges into the plan's branch, one at a time.
    merging: tokio::sync::Mutex<()>,
}

/// Runs `f` (git's work) off the worker's thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, WorktreeError> + Send + 'static,
) -> Result<T, WorktreeError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(WorktreeError::Invalid(format!("git's work stopped: {e}"))))
}

/// A worktree name for a plan's task (`plan-<id>-<task>`, `-2` for its
/// second attempt).
fn tree_name(plan: &str, task: &str, attempt: usize) -> String {
    let task: String = worktrees::name_from(task).chars().take(12).collect();
    let task = if task.is_empty() {
        "task".to_owned()
    } else {
        task
    };
    if attempt == 0 {
        format!("{plan}-{task}")
    } else {
        format!("{plan}-{task}-{}", attempt + 1)
    }
}

/// The plan's worktree, when the project is in a git repository and
/// `[harness] worktrees` is on; else the tasks edit the project in place.
async fn plan_trees(
    ctx: &Arc<Context>,
    shared: &Shared,
    id: &str,
    goal: &str,
    records: &Records,
) -> Option<Arc<PlanTrees>> {
    if !ctx.config.harness.worktrees {
        return None;
    }
    let data_dir = ctx.data_dir.clone()?;
    let project = ctx.project.clone();
    let config = ctx.config.worktrees.clone();
    let (name, title, origin) = (
        format!("plan-{id}"),
        events::one_line(goal, 120),
        format!("harness:{id}"),
    );
    let made = blocking(move || {
        let repo = Repo::find(&project)?;
        let store = Store::new(&data_dir, &config);
        let dirty = worktrees::has_changes(&project);
        let from = worktrees::current_branch_at(&project);
        let plan = store.create(
            &repo,
            &NewWorktree {
                name: Some(name),
                from,
                branch: None,
                title,
                origin,
                ports: false,
            },
        )?;
        Ok((repo, store, plan, dirty))
    })
    .await;
    match made {
        Ok((repo, store, plan, dirty)) => {
            records.note(format!(
                "worktree: {} on {} from {}",
                plan.name, plan.branch, plan.base
            ));
            shared.update(|v| {
                if dirty {
                    v.notes.push(format!(
                        "Changes not committed in {} are not in the plan's worktree: it starts from {}'s last commit.",
                        ctx.project.display(),
                        plan.base
                    ));
                }
                v.worktree = Some(PlanTree {
                    name: plan.name.clone(),
                    branch: plan.branch.clone(),
                    path: plan.path.clone(),
                });
            });
            Some(Arc::new(PlanTrees {
                store,
                repo,
                plan,
                merging: tokio::sync::Mutex::new(()),
            }))
        }
        // Not a repository: in place, as always.
        Err(WorktreeError::NotARepo(_)) => None,
        Err(e) => {
            records.note(format!("no worktree ({e}); editing in place"));
            shared.update(|v| {
                v.notes.push(format!(
                    "The plan's worktree could not be made ({e}); its tasks edit the project in place."
                ));
            });
            None
        }
    }
}

/// What a task in a worktree needs from the run.
struct TreeRun<'a> {
    ctx: &'a Arc<Context>,
    shared: &'a Shared,
    index: usize,
    task: &'a Task,
    goal: &'a str,
    inputs: &'a [(String, String)],
    exhausted: &'a Arc<Mutex<BTreeSet<String>>>,
    records: &'a Records,
    authors: &'a [String],
}

/// A task that changes files, in a worktree of its own made from the
/// plan's branch: run, committed, merged into the plan's branch. A
/// conflict runs it again on top of the branch as it is now, once; a
/// second leaves the plan to the user (D-113).
async fn run_in_tree(
    r: &TreeRun<'_>,
    trees: &Arc<PlanTrees>,
    stop: watch::Receiver<bool>,
) -> (Outcome, Option<Choice>) {
    let note = |line: String| {
        r.shared.update(|v| v.tasks[r.index].note(line));
    };
    let mut again = String::new();
    for attempt in 0..2 {
        let name = tree_name(&trees.plan.name, &r.task.id, attempt);
        let (store, repo) = (trees.store.clone(), trees.repo.clone());
        let req = NewWorktree {
            name: Some(name),
            from: Some(trees.plan.branch.clone()),
            branch: None,
            title: r.task.title.clone(),
            origin: trees.plan.origin.clone(),
            ports: false,
        };
        let wt = match blocking(move || store.create(&repo, &req)).await {
            Ok(w) => w,
            Err(e) => {
                let out = Outcome {
                    text: format!("its worktree could not be made: {e}"),
                    ..Outcome::default()
                };
                return (out, None);
            }
        };
        note(format!("worktree {}", wt.name));
        let mut prompt = task_prompt(r.goal, &wt.path, r.task, r.inputs);
        prompt.push_str(&again);
        let (mut out, choice) = attempt_all(
            r.ctx,
            r.shared,
            Some(r.index),
            &r.task.id,
            r.task.kind,
            r.task.backend.as_deref(),
            prompt,
            r.exhausted,
            r.records,
            r.authors,
            stop.clone(),
            &wt.path,
        )
        .await;
        let store = trees.store.clone();
        if !out.ok {
            // Kept for a look (`termos worktree restore`).
            let gone = wt.clone();
            let _ = blocking(move || store.discard(&gone)).await;
            return (out, choice);
        }
        let message = format!("{}: {}", r.task.id, r.task.title);
        let committed = {
            let w = wt.clone();
            blocking(move || worktrees::commit_all(&w, &message)).await
        };
        let merged = match committed {
            Ok(None) => Ok(None),
            Ok(Some(_)) => {
                let _one = trees.merging.lock().await;
                let w = wt.clone();
                blocking(move || worktrees::merge(&w).map(Some)).await
            }
            Err(e) => Err(e),
        };
        let gone = wt.clone();
        let tidy = trees.store.clone();
        let _ = blocking(move || {
            tidy.discard(&gone)?;
            tidy.forget(&gone)
        })
        .await;
        match merged {
            Ok(None) => {
                note("no changes to merge".into());
                return (out, choice);
            }
            Ok(Some(_)) => {
                note(format!("merged into {}", trees.plan.branch));
                return (out, choice);
            }
            Err(e @ WorktreeError::Conflict { .. }) if attempt == 0 => {
                note(format!("{e}; again on top of {}", trees.plan.branch));
                again = format!(
                    "\n\nA first attempt at this task conflicted with changes that other tasks made meanwhile ({e}). \
                     The folder now has their changes: do the task on top of them.\n"
                );
            }
            Err(e) => {
                out.ok = false;
                out.conflict = matches!(e, WorktreeError::Conflict { .. });
                out.text = format!(
                    "its changes could not be merged into {}: {e}",
                    trees.plan.branch
                );
                return (out, choice);
            }
        }
    }
    let out = Outcome {
        text: "it conflicted twice".into(),
        conflict: true,
        ..Outcome::default()
    };
    (out, None)
}

/// What a task waits for: its dependencies, and (editing in place) every
/// earlier task that changes files.
fn needs(tasks: &[TaskView], i: usize, in_place: bool) -> Vec<usize> {
    let mut out: Vec<usize> = tasks[i]
        .task
        .depends_on
        .iter()
        .filter_map(|d| tasks[..i].iter().position(|t| &t.task.id == d))
        .collect();
    for (j, t) in tasks[..i].iter().enumerate() {
        if in_place && t.task.kind.writes() && !out.contains(&j) {
            out.push(j);
        }
    }
    out.sort_unstable();
    out
}

/// Runs the plan under review.
async fn execute(ctx: &Arc<Context>, shared: &Shared, rx: &mut mpsc::UnboundedReceiver<Command>) {
    let Some((id, goal, tasks)) = shared.read(|v| {
        (
            v.id.clone(),
            v.goal.clone(),
            v.tasks.iter().map(|t| t.task.clone()).collect::<Vec<_>>(),
        )
    }) else {
        return;
    };
    shared.update(|v| v.phase = Phase::Running);
    let records = open_records(ctx, &id);
    records.note(format!(
        "run: {}",
        tasks
            .iter()
            .map(|t| format!("{} {}", t.id, t.kind.name()))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let trees = plan_trees(ctx, shared, &id, &goal, &records).await;
    let exhausted = Arc::new(Mutex::new(BTreeSet::new()));
    let parallel = ctx.config.harness.parallel.max(1);
    let mut stops: Vec<watch::Sender<bool>> =
        tasks.iter().map(|_| watch::channel(false).0).collect();
    let mut running: tokio::task::JoinSet<(usize, Outcome, Option<Choice>)> =
        tokio::task::JoinSet::new();
    let mut stopping = false;
    loop {
        // Start what can start.
        let snapshot = shared.read(|v| v.tasks.clone()).unwrap_or_default();
        let writes_running = snapshot
            .iter()
            .any(|t| t.state == TaskState::Running && t.task.kind.writes());
        let mut reads_running = snapshot
            .iter()
            .filter(|t| t.state == TaskState::Running && !t.task.kind.writes())
            .count();
        let mut started_write = false;
        let mut running_now = snapshot
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .count();
        for i in 0..snapshot.len() {
            if snapshot[i].state != TaskState::Waiting {
                continue;
            }
            if stopping {
                shared.update(|v| v.tasks[i].state = TaskState::Stopped);
                continue;
            }
            let deps = needs(&snapshot, i, trees.is_none());
            if let Some(&d) = deps.iter().find(|&&d| {
                matches!(
                    snapshot[d].state,
                    TaskState::Failed(_) | TaskState::Skipped(_) | TaskState::Stopped
                )
            }) {
                let why = format!("it needs {}, which did not finish", snapshot[d].task.id);
                shared.update(|v| {
                    v.tasks[i].state = TaskState::Skipped(why.clone());
                });
                let h = Handoff {
                    task: &snapshot[i].task.id,
                    kind: snapshot[i].task.kind,
                    backend: "",
                    model: "",
                    account: "",
                };
                records.write(&h, "skipped", &why);
                continue;
            }
            if !deps.iter().all(|&d| snapshot[d].state == TaskState::Done) {
                continue;
            }
            let writes = snapshot[i].task.kind.writes();
            if trees.is_some() {
                // Each in its own worktree: as many as `parallel`.
                if running_now >= parallel {
                    continue;
                }
                running_now += 1;
            } else if (writes && (writes_running || started_write))
                || (!writes && reads_running >= parallel)
            {
                continue;
            }
            if writes {
                started_write = true;
            } else {
                reads_running += 1;
            }
            // Mark it running before the next look, then start it.
            shared.update(|v| {
                v.tasks[i].state = TaskState::Running;
                v.tasks[i].started = Some(Instant::now());
            });
            let task = snapshot[i].task.clone();
            let inputs: Vec<(String, String)> = deps
                .iter()
                .filter(|&&d| {
                    task.depends_on.contains(&snapshot[d].task.id) || snapshot[d].task.kind.writes()
                })
                .map(|&d| {
                    let t = &snapshot[d];
                    (
                        format!("{} ({}, {})", t.task.id, t.task.kind.name(), t.task.title),
                        t.result.clone().unwrap_or_default(),
                    )
                })
                .collect();
            let goal2 = goal.clone();
            // A review goes first to someone other than its authors, unless
            // the user or the route chose the backend.
            let authors: Vec<String> = if task.kind == TaskKind::Review
                && ctx.config.harness.cross_review
                && task.backend.is_none()
                && ctx
                    .config
                    .harness
                    .routes
                    .get(&task.kind)
                    .is_none_or(|r| r.backends().is_empty())
            {
                deps.iter()
                    .filter(|&&d| snapshot[d].task.kind.writes())
                    .filter_map(|&d| snapshot[d].backend.clone())
                    .collect()
            } else {
                Vec::new()
            };
            let (ctx2, shared2, exhausted2, records2) = (
                Arc::clone(ctx),
                shared.clone(),
                Arc::clone(&exhausted),
                records.clone(),
            );
            let stop = stops[i].subscribe();
            let trees2 = trees.clone();
            running.spawn_local(async move {
                let (out, choice) = match &trees2 {
                    Some(t) if task.kind.writes() => {
                        let run = TreeRun {
                            ctx: &ctx2,
                            shared: &shared2,
                            index: i,
                            task: &task,
                            goal: &goal2,
                            inputs: &inputs,
                            exhausted: &exhausted2,
                            records: &records2,
                            authors: &authors,
                        };
                        run_in_tree(&run, t, stop).await
                    }
                    _ => {
                        // Read-only tasks look at the plan's worktree.
                        let project = trees2
                            .as_ref()
                            .map_or_else(|| ctx2.project.clone(), |t| t.plan.path.clone());
                        let prompt = task_prompt(&goal2, &project, &task, &inputs);
                        attempt_all(
                            &ctx2,
                            &shared2,
                            Some(i),
                            &task.id,
                            task.kind,
                            task.backend.as_deref(),
                            prompt,
                            &exhausted2,
                            &records2,
                            &authors,
                            stop,
                            &project,
                        )
                        .await
                    }
                };
                (i, out, choice)
            });
        }
        if running.is_empty() {
            break;
        }
        tokio::select! {
            joined = running.join_next() => {
                let Some(Ok((i, out, choice))) = joined else { continue };
                shared.update(|v| {
                    let t = &mut v.tasks[i];
                    t.finished = Some(Instant::now());
                    t.tokens.add(out.tokens);
                    t.cost = out.cost.or(t.cost);
                    t.estimate = out.estimate.or(t.estimate);
                    t.session.clone_from(&out.session);
                    for d in &out.denied {
                        t.note(format!("! not allowed: {d}"));
                    }
                    if let Some(c) = &choice {
                        t.by = Some(c.describe());
                        t.backend = Some(c.backend.name.clone());
                    }
                    t.result = Some(out.text.clone());
                    t.state = if out.ok {
                        TaskState::Done
                    } else if out.stopped {
                        TaskState::Stopped
                    } else {
                        TaskState::Failed(events::one_line(&out.text, 200))
                    };
                });
                if out.conflict && !stopping {
                    // Two attempts conflicted: the user decides (D-113).
                    stopping = true;
                    records.note(format!("stopped: {}", events::one_line(&out.text, 300)));
                }
            }
            cmd = rx.recv() => match cmd {
                Some(Command::Stop(None)) | None => {
                    stopping = true;
                    for s in &stops {
                        let _ = s.send(true);
                    }
                }
                Some(Command::Stop(Some(task))) => {
                    let found = shared.read(|v| v.tasks.iter().position(|t| t.task.id == task)).flatten();
                    if let Some(i) = found {
                        let _ = stops[i].send(true);
                        let waiting = shared.read(|v| v.tasks[i].state == TaskState::Waiting).unwrap_or(false);
                        if waiting {
                            shared.update(|v| v.tasks[i].state = TaskState::Stopped);
                        }
                    }
                }
                // Planning or running again waits for this run.
                Some(Command::Plan(_) | Command::Run) => {}
            },
        }
    }
    stops.clear();
    let failed = shared
        .read(|v| {
            v.tasks
                .iter()
                .filter(|t| t.state != TaskState::Done)
                .count()
        })
        .unwrap_or(0);
    records.note(format!(
        "finished: {failed} of {} tasks did not finish",
        tasks.len()
    ));
    if let Some(t) = &trees {
        records.note(format!(
            "result: branch {} in worktree {} ({})",
            t.plan.branch,
            t.plan.name,
            t.plan.path.display()
        ));
    }
    shared.update(|v| v.phase = Phase::Finished { failed });
}
