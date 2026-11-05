// SPDX-License-Identifier: GPL-3.0-or-later
//! Best-of-N (M12a, D-114): one prompt to several backends at once
//! (`claude:2,codex:1`), each attempt in a worktree of its own made from
//! the branch the project is on. Each attempt's changes are committed
//! there, and the project's test task runs on them. Their diffs, test
//! results, time and tokens are shown side by side; the attempt kept stays
//! a worktree to review, merge or open as a pull request, and the others
//! are discarded.

use super::{
    Arc, BTreeSet, Check, Context, Instant, Mutex, Outcome, Phase, RunView, Shared, TaskView,
    attempt_all, blocking, events, open_records, planned_routes, watch,
};
use crate::plan::Task;
use crate::route;
use termos_core::config::Config;
use termos_core::config::harness::TaskKind;
use termos_devkit::worktrees::{self, NewWorktree, Repo, Store, Worktree};
use tokio::sync::mpsc;

/// The most attempts at once.
pub const MAX: usize = 8;

/// `claude:2,codex:1` (or `claude,codex`): each backend and how many
/// attempts it makes. Names are the harness's backends (`claude`,
/// `codex`, an API backend's name).
pub fn parse_spec(spec: &str, config: &Config) -> Result<Vec<(String, usize)>, String> {
    let names: Vec<String> = route::backends(config)
        .into_iter()
        .map(|b| b.name)
        .collect();
    let mut out: Vec<(String, usize)> = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, n) = match part.split_once(':') {
            Some((name, n)) => (
                name.trim(),
                n.trim()
                    .parse::<usize>()
                    .map_err(|_| format!("{part:?}: the count after ':' is a number"))?,
            ),
            None => (part, 1),
        };
        if !names.iter().any(|x| x == name) {
            return Err(format!(
                "no backend named {name:?} (there are {})",
                names.join(", ")
            ));
        }
        if n == 0 {
            return Err(format!("{part:?}: at least one attempt"));
        }
        match out.iter_mut().find(|(x, _)| x == name) {
            Some((_, m)) => *m += n,
            None => out.push((name.to_owned(), n)),
        }
    }
    let total: usize = out.iter().map(|(_, n)| n).sum();
    if total < 2 {
        return Err("best-of-N needs at least two attempts (`claude:2` or `claude,codex`)".into());
    }
    if total > MAX {
        return Err(format!("at most {MAX} attempts at once"));
    }
    Ok(out)
}

/// The attempts: one task per attempt, pinned to its backend.
fn attempts(prompt: &str, spec: &[(String, usize)]) -> Vec<Task> {
    let mut tasks = Vec::new();
    for (name, n) in spec {
        for k in 1..=*n {
            tasks.push(Task {
                id: format!("a{}", tasks.len() + 1),
                kind: TaskKind::Coding,
                title: if *n > 1 {
                    format!("{name} ({k} of {n})")
                } else {
                    name.clone()
                },
                prompt: prompt.to_owned(),
                depends_on: Vec::new(),
                backend: Some(name.clone()),
            });
        }
    }
    tasks
}

/// Runs a best-of-N: every attempt at once, then their checks.
pub(super) async fn run(
    ctx: &Arc<Context>,
    shared: &Shared,
    rx: &mut mpsc::UnboundedReceiver<super::Command>,
    prompt: String,
    spec: Vec<(String, usize)>,
) {
    let id = termos_ai::history::new_id();
    let tasks = attempts(&prompt, &spec);
    let routes = planned_routes(ctx, &tasks);
    let summary = format!(
        "Best of {}: {}",
        tasks.len(),
        spec.iter()
            .map(|(b, n)| if *n > 1 {
                format!("{b} ×{n}")
            } else {
                b.clone()
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    shared.update(|v| {
        *v = RunView {
            notes: std::mem::take(&mut v.notes),
            ..RunView::new()
        };
        v.id.clone_from(&id);
        v.goal.clone_from(&prompt);
        v.summary.clone_from(&summary);
        v.best = true;
        v.tasks = tasks
            .iter()
            .cloned()
            .zip(routes)
            .map(|(t, r)| TaskView::new(t, r))
            .collect();
        v.phase = Phase::Running;
    });
    let records = open_records(ctx, &id);
    records.note(format!(
        "best of {}: {}",
        tasks.len(),
        events::one_line(&prompt, 300)
    ));
    let Some(data_dir) = ctx.data_dir.clone() else {
        shared.update(|v| {
            v.phase =
                Phase::Failed("best-of-N needs worktrees, which this build does not keep".into())
        });
        return;
    };
    let (project, config) = (ctx.project.clone(), ctx.config.worktrees.clone());
    let setup = blocking(move || {
        let repo = Repo::find(&project)?;
        let from = worktrees::current_branch_at(&project);
        Ok((repo, Store::new(&data_dir, &config), from))
    })
    .await;
    let (repo, store, from) = match setup {
        Ok(s) => s,
        Err(e) => {
            shared.update(|v| {
                v.phase = Phase::Failed(format!(
                    "best-of-N works in a git repository, each attempt in a worktree: {e}"
                ));
            });
            return;
        }
    };
    if worktrees::has_changes(&ctx.project) {
        shared.update(|v| {
            v.notes.push(format!(
                "Changes not committed in {} are not in the attempts' worktrees.",
                ctx.project.display()
            ));
        });
    }
    let exhausted = Arc::new(Mutex::new(BTreeSet::new()));
    let mut stops: Vec<watch::Sender<bool>> =
        tasks.iter().map(|_| watch::channel(false).0).collect();
    let mut running: tokio::task::JoinSet<(usize, Outcome, Option<route::Choice>)> =
        tokio::task::JoinSet::new();
    for (i, task) in tasks.into_iter().enumerate() {
        shared.update(|v| {
            v.tasks[i].state = super::TaskState::Running;
            v.tasks[i].started = Some(Instant::now());
        });
        let one = Attempt {
            ctx: Arc::clone(ctx),
            shared: shared.clone(),
            index: i,
            task,
            id: id.clone(),
            repo: repo.clone(),
            store: store.clone(),
            from: from.clone(),
            exhausted: Arc::clone(&exhausted),
            records: records.clone(),
        };
        let stop = stops[i].subscribe();
        running.spawn_local(async move {
            let (out, choice) = one.run(stop).await;
            (one.index, out, choice)
        });
    }
    loop {
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
                    if let Some(c) = &choice {
                        t.by = Some(c.describe());
                        t.backend = Some(c.backend.name.clone());
                    }
                    t.result = Some(out.text.clone());
                    t.state = if out.ok {
                        super::TaskState::Done
                    } else if out.stopped {
                        super::TaskState::Stopped
                    } else {
                        super::TaskState::Failed(events::one_line(&out.text, 200))
                    };
                });
            }
            cmd = rx.recv() => match cmd {
                Some(super::Command::Stop(None)) | None => {
                    for s in &stops {
                        let _ = s.send(true);
                    }
                }
                Some(super::Command::Stop(Some(task))) => {
                    let found = shared.read(|v| v.tasks.iter().position(|t| t.task.id == task)).flatten();
                    if let Some(i) = found {
                        let _ = stops[i].send(true);
                    }
                }
                Some(_) => {}
            },
        }
    }
    stops.clear();
    let failed = shared
        .read(|v| {
            v.tasks
                .iter()
                .filter(|t| t.state != super::TaskState::Done)
                .count()
        })
        .unwrap_or(0);
    records.note(format!("finished: {failed} attempts did not finish"));
    shared.update(|v| v.phase = Phase::Finished { failed });
}

/// One attempt and what it needs.
struct Attempt {
    ctx: Arc<Context>,
    shared: Shared,
    index: usize,
    task: Task,
    id: String,
    repo: Repo,
    store: Store,
    from: Option<String>,
    exhausted: Arc<Mutex<BTreeSet<String>>>,
    records: super::Records,
}

impl Attempt {
    fn note(&self, line: String) {
        self.shared.update(|v| v.tasks[self.index].note(line));
    }

    async fn run(&self, stop: watch::Receiver<bool>) -> (Outcome, Option<route::Choice>) {
        let (store, repo) = (self.store.clone(), self.repo.clone());
        let req = NewWorktree {
            name: Some(format!("best-{}-{}", self.id, self.task.id)),
            from: self.from.clone(),
            branch: None,
            title: events::one_line(&self.task.prompt, 120),
            origin: format!("best:{}", self.id),
            ports: true,
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
        self.shared
            .update(|v| v.tasks[self.index].worktree = Some(wt.name.clone()));
        self.note(format!("worktree {}", wt.name));
        let prompt = format!(
            "{}\n\nProject folder: {}\n",
            self.task.prompt,
            wt.path.display()
        );
        let (out, choice) = attempt_all(
            &self.ctx,
            &self.shared,
            Some(self.index),
            &self.task.id,
            self.task.kind,
            self.task.backend.as_deref(),
            prompt,
            &self.exhausted,
            &self.records,
            &[],
            stop,
            &wt.path,
        )
        .await;
        if !out.ok {
            let (store, gone) = (self.store.clone(), wt.clone());
            let _ = blocking(move || {
                let gone = store.discard(&gone)?;
                store.forget(&gone)
            })
            .await;
            self.shared.update(|v| v.tasks[self.index].worktree = None);
            return (out, choice);
        }
        let by = choice.as_ref().map_or_else(String::new, |c| c.describe());
        let message = format!(
            "{} (best of N, {})",
            events::one_line(&self.task.prompt, 60),
            by
        );
        let w = wt.clone();
        let changed = blocking(move || {
            worktrees::commit_all(&w, &message)?;
            worktrees::changes(&w)
        })
        .await;
        if let Ok(c) = &changed {
            let stat = (c.added(), c.removed(), c.files.len());
            self.shared
                .update(|v| v.tasks[self.index].changes = Some(stat));
        }
        if let Some(check) = check(&self.ctx, &wt).await {
            self.note(format!(
                "tests {}: {}",
                if check.passed { "passed" } else { "failed" },
                check.command
            ));
            self.shared
                .update(|v| v.tasks[self.index].check = Some(check));
        }
        (out, choice)
    }
}

/// The project's test task (`termos task test`), run in the attempt's
/// worktree with its ports; `None` when the project has none.
async fn check(ctx: &Context, wt: &Worktree) -> Option<Check> {
    let command = termos_devkit::tasks::find(&wt.path)
        .ok()
        .flatten()?
        .tasks
        .get("test")?
        .clone();
    let (program, args) = termos_devkit::tasks::shell(&command);
    let limit = std::time::Duration::from_secs(ctx.config.harness.task_minutes.max(1) * 60);
    let run = tokio::process::Command::new(program)
        .args(args)
        .current_dir(&wt.path)
        .envs(wt.env())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let (passed, output) = match tokio::time::timeout(limit, run).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), text)
        }
        Ok(Err(e)) => (false, format!("cannot run it: {e}")),
        Err(_) => (
            false,
            format!("still running after {} minutes", limit.as_secs() / 60),
        ),
    };
    let lines: Vec<&str> = output.lines().collect();
    let tail = lines[lines.len().saturating_sub(30)..].join("\n");
    Some(Check {
        passed,
        command,
        output: tail,
    })
}

/// Keeps attempt `index`: the other attempts' worktrees are discarded and
/// forgotten; the kept one stays, to review and merge (D-114).
pub(super) async fn keep(ctx: &Arc<Context>, shared: &Shared, index: usize) {
    let Some((best, finished, names, kept)) = shared.read(|v| {
        (
            v.best,
            matches!(v.phase, Phase::Finished { .. }),
            v.tasks
                .iter()
                .map(|t| t.worktree.clone())
                .collect::<Vec<_>>(),
            v.kept,
        )
    }) else {
        return;
    };
    let chosen = names.get(index).cloned().flatten();
    if !best || !finished || kept.is_some() || chosen.is_none() {
        return;
    }
    let Some(data_dir) = ctx.data_dir.clone() else {
        return;
    };
    let (project, config) = (ctx.project.clone(), ctx.config.worktrees.clone());
    let others: Vec<String> = names
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .filter_map(|(_, n)| n.clone())
        .collect();
    let gone = blocking(move || {
        let repo = Repo::find(&project).ok();
        let store = Store::new(&data_dir, &config);
        let mut gone = Vec::new();
        for name in others {
            let w = store.get(repo.as_ref(), &name)?;
            let w = store.discard(&w)?;
            store.forget(&w)?;
            gone.push(name);
        }
        Ok(gone)
    })
    .await;
    shared.update(|v| match &gone {
        Ok(names) => {
            for t in &mut v.tasks {
                if t.worktree.as_ref().is_some_and(|w| names.contains(w)) {
                    t.worktree = None;
                }
            }
            v.kept = Some(index);
        }
        Err(e) => v
            .notes
            .push(format!("the other attempts could not all be removed: {e}")),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_name_backends_and_counts() {
        let c = Config::default();
        assert_eq!(
            parse_spec("claude:2, codex:1", &c).expect("ok"),
            [("claude".to_owned(), 2), ("codex".to_owned(), 1)]
        );
        assert_eq!(
            parse_spec("claude,codex,claude", &c).expect("ok"),
            [("claude".to_owned(), 2), ("codex".to_owned(), 1)]
        );
        assert!(
            parse_spec("claude", &c)
                .unwrap_err()
                .contains("at least two")
        );
        assert!(
            parse_spec("aider:2", &c)
                .unwrap_err()
                .contains("no backend named")
        );
        assert!(parse_spec("claude:x", &c).is_err());
        assert!(parse_spec("claude:0,codex:2", &c).is_err());
        assert!(parse_spec("claude:9", &c).unwrap_err().contains("at most"));
        let tasks = attempts("do it", &[("claude".into(), 2), ("codex".into(), 1)]);
        let ids: Vec<(&str, &str, Option<&str>)> = tasks
            .iter()
            .map(|t| (t.id.as_str(), t.title.as_str(), t.backend.as_deref()))
            .collect();
        assert_eq!(
            ids,
            [
                ("a1", "claude (1 of 2)", Some("claude")),
                ("a2", "claude (2 of 2)", Some("claude")),
                ("a3", "codex", Some("codex"))
            ]
        );
    }
}
