// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos harness`: the coding harness from a terminal (M11c, D-099,
//! D-100): make a plan, look at it, run it; and where each kind of task
//! would go.

use std::collections::HashMap;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Subcommand;
use termos_ai::agent::SecretSlot;
use termos_ai::secrets::SecretStore;
use termos_core::config::Config;
use termos_core::config::harness::{BackendKind, TaskKind};
use termos_core::paths::Paths;
use termos_devkit::cli_agents::Tool;
use termos_harness::backends::{ApiContext, Program, Programs};
use termos_harness::plan::Plan;
use termos_harness::route;
use termos_harness::runner::{Context, Harness, Phase, RunView, TaskState};

#[derive(Debug, Subcommand)]
pub(crate) enum HarnessCmd {
    /// Make a plan for a goal and print it (nothing runs).
    Plan {
        /// Print the plan as JSON (for `termos harness run --plan`).
        #[arg(long)]
        json: bool,
        /// The project folder (default: the current one).
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
        #[arg(required = true, trailing_var_arg = true)]
        goal: Vec<String>,
    },
    /// Plan a goal (or load a plan), show it, and run it once you agree.
    Run {
        /// Run without asking.
        #[arg(long, short)]
        yes: bool,
        /// Run this plan (JSON, as `termos harness plan --json` writes it)
        /// instead of making one.
        #[arg(long, value_name = "FILE")]
        plan: Option<PathBuf>,
        /// The project folder (default: the current one).
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
        #[arg(trailing_var_arg = true)]
        goal: Vec<String>,
    },
    /// The backends, their accounts and models, and where each kind of
    /// task goes.
    Status {
        /// The project folder (default: the current one).
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
    },
}

/// Claude Code and Codex as TermOS's shims would run them: TermOS's
/// install or the one on `PATH`, as the account.
pub(crate) struct ShimPrograms {
    pub paths: Paths,
}

impl Programs for ShimPrograms {
    fn cli(&self, kind: BackendKind, account: Option<&str>) -> Result<Program, String> {
        let tool = match kind {
            BackendKind::Claude => Tool::Claude,
            BackendKind::Codex => Tool::Codex,
            BackendKind::Api => return Err("not a CLI".into()),
        };
        let l = crate::shim::plan(&self.paths, tool, account).map_err(|e| format!("{e:#}"))?;
        Ok(Program {
            path: l.program,
            env: l.env,
            unset: l.unset,
            secrets: l.secrets,
            account: l.account.filter(|a| !a.is_default()).map(|a| a.title()),
        })
    }
}

/// The API backends' providers that need a key.
fn keyed_providers(config: &Config) -> bool {
    route::backends(config).iter().any(|b| {
        b.kind == BackendKind::Api
            && b.provider
                .as_ref()
                .and_then(|p| config.agents.providers.get(p))
                .is_some_and(|p| p.key.is_some())
    })
}

/// Everything the harness needs, for the terminal.
fn context(
    paths: &Paths,
    config_path: &Path,
    project: Option<&PathBuf>,
) -> anyhow::Result<Context> {
    let config = Config::load(config_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let project = match project {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let project = project
        .canonicalize()
        .with_context(|| format!("cannot open {}", project.display()))?;
    let store = SecretStore::open(config.secrets.store, &paths.data_dir);
    if keyed_providers(&config)
        && store.is_locked()
        && store.file_exists()
        && (std::io::stdin().is_terminal() || std::io::stderr().is_terminal())
    {
        store.unlock(crate::agents::read_hidden(&format!(
            "Passphrase for {} (for the API backends)",
            store.file().display()
        ))?)?;
    }
    let journal = Arc::new(termos_core::journal::Journal::new(
        &paths.state_dir,
        config.center.keep_days,
    ));
    Ok(Context {
        api: Some(ApiContext {
            agents: config.agents.clone(),
            secrets: Arc::new(SecretSlot::with(store)),
            paths: paths.clone(),
            helper: std::env::current_exe().ok(),
            config_file: Some(config_path.to_owned()),
            home: crate::coding_agents::home(),
            journal: Some(Arc::clone(&journal)),
        }),
        programs: Arc::new(ShimPrograms {
            paths: paths.clone(),
        }),
        state_dir: paths.state_dir.clone(),
        journal: Some(journal),
        project,
        config,
    })
}

/// Waits for the view to change (or a second to pass) until `done`.
fn follow(h: &Harness, rx: &std::sync::mpsc::Receiver<()>, mut on: impl FnMut(&RunView) -> bool) {
    loop {
        let finished = h.view.lock().map(|v| on(&v)).unwrap_or(true);
        if finished {
            return;
        }
        let _ = rx.recv_timeout(Duration::from_secs(1));
        while rx.try_recv().is_ok() {}
    }
}

fn print_plan(v: &RunView) {
    println!(
        "Plan: {}",
        if v.summary.is_empty() {
            &v.goal
        } else {
            &v.summary
        }
    );
    if let Some(p) = &v.planner {
        println!("  planned by {p}");
    }
    for t in &v.tasks {
        let deps = if t.task.depends_on.is_empty() {
            String::new()
        } else {
            format!("  (after {})", t.task.depends_on.join(", "))
        };
        println!(
            "  {:<4} {:<11} {}{deps}",
            t.task.id,
            t.task.kind.name(),
            t.task.title
        );
        let tries: Vec<String> = t.route.choices.iter().map(|c| c.describe()).collect();
        println!(
            "       → {}",
            if tries.is_empty() {
                format!("no backend can take it ({})", t.route.explain())
            } else {
                tries.join(", then ")
            }
        );
    }
    for n in &v.notes {
        println!("  note: {n}");
    }
}

fn ask_yes() -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("Run it? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    let a = answer.trim().to_ascii_lowercase();
    a.is_empty() || a == "y" || a == "yes"
}

fn seconds(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Plans `goal`, waiting; `Err` with the reason when planning failed.
fn plan_goal(h: &Harness, rx: &std::sync::mpsc::Receiver<()>, goal: &str) -> anyhow::Result<()> {
    h.plan(goal);
    let mut said = String::new();
    let mut failed = None;
    follow(h, rx, |v| match &v.phase {
        Phase::Planning(by) => {
            if !by.is_empty() && *by != said {
                eprintln!("termos: planning with {by}…");
                said.clone_from(by);
            }
            false
        }
        Phase::Failed(why) => {
            failed = Some((why.clone(), v.raw_plan.clone()));
            true
        }
        Phase::Idle => false,
        _ => true,
    });
    if let Some((why, raw)) = failed {
        if let Some(raw) = raw {
            eprintln!("The planner answered:\n{raw}");
        }
        bail!("{why}");
    }
    Ok(())
}

pub(crate) fn harness_command(
    action: &HarnessCmd,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    match action {
        HarnessCmd::Status { project } => status(paths, config_path, project.as_ref()),
        HarnessCmd::Plan {
            json,
            project,
            goal,
        } => {
            let (h, rx) = start(paths, config_path, project.as_ref())?;
            plan_goal(&h, &rx, &goal.join(" "))?;
            let v = h
                .view
                .lock()
                .map(|v| v.clone())
                .map_err(|_| anyhow::anyhow!("the plan is gone"))?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&v.plan())?);
            } else {
                print_plan(&v);
            }
            Ok(ExitCode::SUCCESS)
        }
        HarnessCmd::Run {
            yes,
            plan,
            project,
            goal,
        } => {
            let (h, rx) = start(paths, config_path, project.as_ref())?;
            match plan {
                Some(file) => {
                    let text = std::fs::read_to_string(file)
                        .with_context(|| format!("cannot read {}", file.display()))?;
                    let mut p: Plan = serde_json::from_str(&text)
                        .with_context(|| format!("{} is not a plan", file.display()))?;
                    if !goal.is_empty() {
                        p.goal = goal.join(" ");
                    }
                    h.load(p).map_err(|e| anyhow::anyhow!("{e}"))?;
                }
                None if goal.is_empty() => bail!("give a goal, or a plan with --plan FILE"),
                None => plan_goal(&h, &rx, &goal.join(" "))?,
            }
            let v = h
                .view
                .lock()
                .map(|v| v.clone())
                .map_err(|_| anyhow::anyhow!("the plan is gone"))?;
            print_plan(&v);
            if v.phase == Phase::Review {
                if !*yes && !ask_yes() {
                    println!("Not run (`--yes` runs without asking).");
                    return Ok(ExitCode::SUCCESS);
                }
                h.run();
            }
            run_to_end(&h, &rx)
        }
    }
}

fn start(
    paths: &Paths,
    config_path: &Path,
    project: Option<&PathBuf>,
) -> anyhow::Result<(Harness, std::sync::mpsc::Receiver<()>)> {
    let ctx = context(paths, config_path, project)?;
    let h = Harness::start(ctx)?;
    let (tx, rx) = std::sync::mpsc::channel();
    h.set_waker(termos_core::app::AppWaker::new(move || {
        let _ = tx.send(());
    }));
    // Ctrl-C stops the run (and the CLIs it started), then exits.
    let stopper = h.clone();
    std::thread::Builder::new()
        .name("termos-harness-ctrl-c".into())
        .spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(async {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        eprintln!("\ntermos: stopping…");
                        stopper.stop(None);
                    }
                });
            }
        })?;
    Ok((h, rx))
}

/// Prints the run as it goes; exits non-zero when a task did not finish.
fn run_to_end(h: &Harness, rx: &std::sync::mpsc::Receiver<()>) -> anyhow::Result<ExitCode> {
    // What was printed of each task: log lines and its state.
    let mut printed: HashMap<String, (usize, String)> = HashMap::new();
    let mut end = None;
    follow(h, rx, |v| {
        let now = Instant::now();
        for t in &v.tasks {
            let entry = printed.entry(t.task.id.clone()).or_default();
            for line in t.log.iter().skip(entry.0) {
                println!("[{}] {line}", t.task.id);
            }
            entry.0 = t.log.len();
            let state = format!("{:?}", t.state);
            if state != entry.1 {
                entry.1 = state;
                let took = t.elapsed(now).map(seconds).unwrap_or_default();
                match &t.state {
                    TaskState::Done => println!(
                        "[{}] done in {took}: {}",
                        t.task.id,
                        termos_harness::events::one_line(t.result.as_deref().unwrap_or(""), 160)
                    ),
                    TaskState::Failed(why) => {
                        println!("[{}] failed after {took}: {why}", t.task.id)
                    }
                    TaskState::Skipped(why) => println!("[{}] not run: {why}", t.task.id),
                    TaskState::Stopped => println!("[{}] stopped", t.task.id),
                    TaskState::Waiting | TaskState::Running => {}
                }
            }
        }
        match v.phase {
            Phase::Finished { failed } => {
                end = Some((failed, v.clone()));
                true
            }
            Phase::Failed(_) | Phase::Idle => {
                end = Some((usize::MAX, v.clone()));
                true
            }
            _ => false,
        }
    });
    let Some((failed, v)) = end else {
        bail!("the harness stopped");
    };
    let tokens = v.tokens();
    let estimate: f64 = v.tasks.iter().filter_map(|t| t.estimate).sum();
    println!();
    println!(
        "{} of {} tasks done · {} tokens · API ${:.4}{}",
        v.tasks.len() - failed.min(v.tasks.len()),
        v.tasks.len(),
        tokens.total(),
        v.spent,
        if estimate > 0.0 {
            format!(" (the CLIs' own estimate: ${estimate:.4}, under your plans)")
        } else {
            String::new()
        }
    );
    println!("Hand-offs: `termos audit show harness-{}`", v.id);
    for t in v.tasks.iter().filter(|t| t.state == TaskState::Done) {
        if let Some(r) = &t.result {
            println!();
            println!(
                "── {} {} ({})",
                t.task.id,
                t.task.title,
                t.by.as_deref().unwrap_or("")
            );
            println!("{}", r.trim());
        }
    }
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn status(
    paths: &Paths,
    config_path: &Path,
    project: Option<&PathBuf>,
) -> anyhow::Result<ExitCode> {
    let ctx = context(paths, config_path, project)?;
    let all = route::backends(&ctx.config);
    println!("Backends, in the order they are tried:");
    for b in &all {
        let state = match b.kind {
            BackendKind::Claude | BackendKind::Codex => {
                match ctx.programs.cli(b.kind, b.account.as_deref()) {
                    Ok(p) => format!("ready ({})", p.path.display()),
                    Err(e) => e,
                }
            }
            BackendKind::Api => {
                let p = b.provider.as_deref().unwrap_or(&b.name);
                if ctx.config.agents.providers.contains_key(p) {
                    format!("API, provider {p}")
                } else {
                    format!("no provider named {p} in [agents.providers]")
                }
            }
        };
        println!("  {:<14} {}", b.name, b.title());
        println!("  {:<14} {state}", "");
        let models: Vec<String> = termos_core::config::harness::Tier::ALL
            .iter()
            .map(|t| {
                format!(
                    "{} {}",
                    t.name(),
                    b.models.get(*t).unwrap_or(if b.is_subscription() {
                        "(its default)"
                    } else {
                        "(unset)"
                    })
                )
            })
            .collect();
        println!("  {:<14} {}", "", models.join(" · "));
    }
    println!();
    println!("Where each kind of task goes:");
    let live = LiveNow { ctx: &ctx };
    for kind in TaskKind::ALL {
        let r = route::route(&ctx.config.harness, &all, kind, None, &live);
        println!("  {}", r.explain());
    }
    let commands = termos_harness::runner::allowed_commands(&ctx.config, &ctx.project);
    println!();
    if commands.is_empty() {
        println!("Commands tasks may run: none (add some with [harness] allow-commands)");
    } else {
        println!("Commands tasks may run: {}", commands.join(", "));
    }
    for n in route::unknown_names(&ctx.config) {
        println!("note: [harness] names `{n}`, which is not a backend");
    }
    let h = &ctx.config.harness;
    println!(
        "Settings: fallback {} · {} reads at a time · at most {} tasks · {} minutes a task · API budget {}{}",
        if h.fallback { "on" } else { "off" },
        h.parallel,
        h.max_tasks,
        h.task_minutes,
        if h.budget_usd > 0.0 {
            format!("${:.2} a plan", h.budget_usd)
        } else {
            "none".into()
        },
        if h.auto_run > 0 {
            format!(
                " · plans of up to {} tasks start without asking",
                h.auto_run
            )
        } else {
            String::new()
        }
    );
    Ok(ExitCode::SUCCESS)
}

/// Availability as `termos harness status` sees it.
struct LiveNow<'a> {
    ctx: &'a Context,
}

impl route::Availability for LiveNow<'_> {
    fn unavailable(&self, b: &route::Backend) -> Option<String> {
        match b.kind {
            BackendKind::Claude | BackendKind::Codex => {
                self.ctx.programs.cli(b.kind, b.account.as_deref()).err()
            }
            BackendKind::Api => {
                let p = b.provider.as_deref().unwrap_or(&b.name);
                (!self.ctx.config.agents.providers.contains_key(p))
                    .then(|| format!("no provider named {p} in [agents.providers]"))
            }
        }
    }
}
