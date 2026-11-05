// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos worktree`: a git worktree per task (M12a, D-113, D-114).
//!
//! Each command takes the worktree's name, or works on the worktree it is
//! run in. What goes to git runs through the user's own `git` (hooks,
//! editor, credentials); pull requests through `gh` or `glab` when they
//! are installed, else the forge's page for one.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context as _, bail};
use clap::Subcommand;
use termos_core::config::Config;
use termos_core::paths::Paths;
use termos_devkit::worktrees::{
    self, Forge, Merged, NewWorktree, ProjectSettings, Repo, State, Store, Worktree,
};

#[derive(Debug, Subcommand)]
pub(crate) enum WorktreeCmd {
    /// Make a worktree: a branch from the one you are on, its own folder
    /// and ports, ignored files such as `.env` copied in, then the
    /// project's setup (`[worktree]` in termos.toml).
    New {
        /// Its name (and its branch's, after `[worktrees] branch-prefix`);
        /// default: from the title, else task-<n>.
        name: Option<String>,
        /// What it is for.
        #[arg(long, short)]
        title: Option<String>,
        /// Start from this branch or commit instead of the current one.
        #[arg(long)]
        from: Option<String>,
        /// Check out this existing branch instead of making one.
        #[arg(long)]
        branch: Option<String>,
        /// No ports for it.
        #[arg(long)]
        no_ports: bool,
        /// Skip the setup commands.
        #[arg(long)]
        no_setup: bool,
        /// Then open it as a workspace in this TermOS session.
        #[arg(long)]
        open: bool,
    },
    /// The worktrees TermOS made: name, branch, ports, changes.
    List {
        /// Archived and discarded ones too.
        #[arg(long)]
        all: bool,
        /// Every repository's, not only this one's.
        #[arg(long)]
        everywhere: bool,
    },
    /// What a worktree changed against the branch it started from.
    Status { name: Option<String> },
    /// The whole diff against the base, new files included.
    Diff { name: Option<String> },
    /// Its environment (`PORT`, `TERMOS_PORT_0` …), as NAME=value lines.
    Env { name: Option<String> },
    /// Copies the ignored files again and runs the setup commands.
    Setup { name: Option<String> },
    /// Commits every change (`git add -A`, then git's own commit, with its
    /// hooks and editor).
    Commit {
        name: Option<String>,
        /// The message; without it, git opens your editor.
        #[arg(long, short)]
        message: Option<String>,
    },
    /// Merges its branch into the one it started from, then archives it
    /// (`[worktrees] archive-merged`).
    Merge {
        name: Option<String>,
        /// Keep the worktree after merging.
        #[arg(long)]
        keep: bool,
    },
    /// Pushes its branch and opens a pull request (`gh`, `glab`), or shows
    /// the page to open one.
    Pr {
        name: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        draft: bool,
    },
    /// Removes its folder and keeps its branch.
    Archive {
        name: Option<String>,
        /// Even with changes not committed.
        #[arg(long)]
        force: bool,
    },
    /// Removes its folder and its branch, changes included.
    Discard {
        name: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Checks an archived or discarded worktree out again.
    Restore { name: String },
    /// Forgets an archived or discarded worktree for good.
    Forget { name: String },
    /// Opens it as a workspace in this TermOS session (agent, shell,
    /// servers).
    Open { name: Option<String> },
}

pub(crate) fn worktree_command(
    action: &WorktreeCmd,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    let config = crate::agents::load_config(config_path)?;
    let store = Store::new(&paths.data_dir, &config.worktrees);
    let cwd = std::env::current_dir().context("cannot read this folder")?;
    match action {
        WorktreeCmd::New {
            name,
            title,
            from,
            branch,
            no_ports,
            no_setup,
            open,
        } => {
            let made = (|| -> anyhow::Result<()> {
                let repo = Repo::find(&cwd)?;
                let wt = store.create(
                    &repo,
                    &NewWorktree {
                        name: name.clone(),
                        from: from.clone(),
                        branch: branch.clone(),
                        title: title.clone().unwrap_or_default(),
                        origin: String::new(),
                        ports: !no_ports,
                    },
                )?;
                println!(
                    "termos: worktree {} on {} (from {}) in {}",
                    wt.name,
                    wt.branch,
                    short(&wt.base),
                    wt.path.display()
                );
                if let Some((first, count)) = wt.ports {
                    println!(
                        "termos: ports {first}-{} (PORT={first})",
                        first + count.saturating_sub(1)
                    );
                }
                if !no_setup {
                    set_up(&store, &wt)?;
                }
                if *open {
                    open_in_termos(&wt)?;
                }
                Ok(())
            })();
            // Made for a TermOS window: keep it open on a problem.
            crate::agents::pause(*open && made.is_err(), made.map(|()| ExitCode::SUCCESS))
        }
        WorktreeCmd::List { all, everywhere } => {
            list(&store, &cwd, *all, *everywhere)?;
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Status { name } => {
            let wt = resolve(&store, &cwd, name.as_deref())?;
            status(&wt)?;
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Diff { name } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            print!("{}", worktrees::diff_text(&wt)?);
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Env { name } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            for (k, v) in wt.env() {
                println!("{k}={v}");
            }
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Setup { name } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            set_up(&store, &wt)?;
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Commit { name, message } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            commit(&wt, message.as_deref())
        }
        WorktreeCmd::Merge { name, keep } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            merge(&store, &config, &wt, *keep)?;
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Pr { name, title, draft } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            pull_request(&store, &wt, title.as_deref(), *draft)?;
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Archive { name, force } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            let w = store.archive(&wt, *force)?;
            println!(
                "termos: archived {} (its branch {} stays; `termos worktree restore {}`)",
                w.name, w.branch, w.name
            );
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Discard { name, yes } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            if !yes
                && !confirm(&format!(
                    "Discard {}: its folder and branch {}, changes included?",
                    wt.name, wt.branch
                ))?
            {
                println!("termos: kept");
                return Ok(ExitCode::FAILURE);
            }
            let w = store.discard(&wt)?;
            println!(
                "termos: discarded {} (`termos worktree restore {}` brings it back)",
                w.name, w.name
            );
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Restore { name } => {
            let wt = resolve(&store, &cwd, Some(name))?;
            let w = store.restore(&wt)?;
            println!(
                "termos: restored {} in {}{}",
                w.name,
                w.path.display(),
                if w.ports.is_some() {
                    format!(", ports {}", w.ports_text())
                } else {
                    String::new()
                }
            );
            set_up(&store, &w)?;
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Forget { name } => {
            let wt = resolve(&store, &cwd, Some(name))?;
            store.forget(&wt)?;
            println!("termos: forgot {}", wt.name);
            Ok(ExitCode::SUCCESS)
        }
        WorktreeCmd::Open { name } => {
            let wt = active(&store, &cwd, name.as_deref())?;
            open_in_termos(&wt)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// A commit or branch for a line: commits shortened.
fn short(base: &str) -> &str {
    if base.len() == 40 && base.chars().all(|c| c.is_ascii_hexdigit()) {
        &base[..10]
    } else {
        base
    }
}

/// `name`, else the worktree `cwd` is in.
fn resolve(store: &Store, cwd: &Path, name: Option<&str>) -> anyhow::Result<Worktree> {
    match name {
        Some(n) => Ok(store.get(Repo::find(cwd).ok().as_ref(), n)?),
        None => store.containing(cwd).ok_or_else(|| {
            anyhow::anyhow!(
                "this folder is not in a worktree TermOS made; name one (`termos worktree list`)"
            )
        }),
    }
}

fn active(store: &Store, cwd: &Path, name: Option<&str>) -> anyhow::Result<Worktree> {
    let wt = resolve(store, cwd, name)?;
    if !wt.active() {
        bail!(
            "{} is {}; `termos worktree restore {}` first",
            wt.name,
            wt.state.name(),
            wt.name
        );
    }
    Ok(wt)
}

/// Copies the ignored files and runs the setup, as the project says.
fn set_up(store: &Store, wt: &Worktree) -> anyhow::Result<()> {
    let settings = ProjectSettings::load(wt)?;
    let copied = worktrees::copy_ignored(wt, &settings.copy)?;
    if !copied.is_empty() {
        println!("termos: copied {}", copied.join(", "));
    }
    if !settings.setup.is_empty() {
        println!("termos: setting up (`[worktree] setup` in termos.toml)");
        worktrees::run_setup(wt, &settings.setup, &mut std::io::stdout())?;
    }
    store.mark_set_up(wt)?;
    Ok(())
}

fn list(store: &Store, cwd: &Path, all: bool, everywhere: bool) -> anyhow::Result<()> {
    let repo = Repo::find(cwd).ok();
    let mut shown: Vec<Worktree> = store
        .list()?
        .into_iter()
        .filter(|w| all || w.active())
        .filter(|w| {
            everywhere
                || repo
                    .as_ref()
                    .is_none_or(|r| worktrees::same_path(&w.repo, &r.root))
        })
        .collect();
    shown.sort_by(|a, b| (&a.repo, a.created).cmp(&(&b.repo, b.created)));
    if shown.is_empty() {
        println!(
            "No worktrees{}. Make one with `termos worktree new`.",
            if repo.is_some() && !everywhere {
                " in this repository"
            } else {
                ""
            }
        );
        return Ok(());
    }
    let here = store.containing(cwd).map(|w| w.name);
    let mut last_repo: Option<PathBuf> = None;
    for w in shown {
        if (repo.is_none() || everywhere) && last_repo.as_ref() != Some(&w.repo) {
            println!("{}", w.repo.display());
            last_repo = Some(w.repo.clone());
        }
        let mark = if here.as_deref() == Some(w.name.as_str()) {
            '●'
        } else {
            ' '
        };
        let what = if w.active() {
            match worktrees::changes(&w) {
                Ok(c) => {
                    let mut parts = Vec::new();
                    if c.ahead > 0 {
                        parts.push(format!(
                            "{} commit{}",
                            c.ahead,
                            if c.ahead == 1 { "" } else { "s" }
                        ));
                    }
                    if c.uncommitted {
                        parts.push("changes".to_owned());
                    }
                    if c.behind > 0 {
                        parts.push(format!("{} behind", c.behind));
                    }
                    if parts.is_empty() {
                        "no changes".to_owned()
                    } else {
                        parts.join(", ")
                    }
                }
                Err(e) => format!("({e})"),
            }
        } else {
            w.state.name().to_owned()
        };
        println!(
            "{mark} {:<20} {:<28} {:<12} {}{}",
            w.name,
            w.branch,
            w.ports_text(),
            what,
            w.pr.as_deref()
                .map(|p| format!("  {p}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn status(wt: &Worktree) -> anyhow::Result<()> {
    println!("{}  {} → {}", wt.name, wt.branch, short(&wt.base));
    if !wt.title.is_empty() {
        println!("  {}", wt.title);
    }
    println!("  {}", wt.path.display());
    if wt.ports.is_some() {
        println!("  ports {}", wt.ports_text());
    }
    if let Some(pr) = &wt.pr {
        println!("  pull request {pr}");
    }
    if !wt.active() {
        println!("  {}", wt.state.name());
        return Ok(());
    }
    let c = worktrees::changes(wt)?;
    println!(
        "  {} commit{} ahead, {} behind{}; +{} -{}",
        c.ahead,
        if c.ahead == 1 { "" } else { "s" },
        c.behind,
        if c.uncommitted {
            ", changes not committed"
        } else {
            ""
        },
        c.added(),
        c.removed()
    );
    for f in &c.files {
        let n = match (f.added, f.removed) {
            (Some(a), Some(r)) => format!("+{a} -{r}"),
            _ => "binary".to_owned(),
        };
        let name = match &f.old_path {
            Some(o) => format!("{o} → {}", f.path),
            None => f.path.clone(),
        };
        println!("  {} {name}  {n}", f.status);
    }
    Ok(())
}

fn git_here(wt: &Worktree) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(&wt.path);
    c
}

fn commit(wt: &Worktree, message: Option<&str>) -> anyhow::Result<ExitCode> {
    let s = git_here(wt)
        .args(["add", "-A"])
        .status()
        .context("cannot run git")?;
    if !s.success() {
        bail!("git add failed");
    }
    let staged = git_here(wt)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .context("cannot run git")?;
    if staged.success() {
        println!("termos: nothing to commit in {}", wt.name);
        return Ok(ExitCode::SUCCESS);
    }
    let mut c = git_here(wt);
    c.arg("commit");
    match message {
        Some(m) => {
            c.args(["-m", m]);
        }
        None if !wt.title.is_empty() => {
            // The title to start from, in git's editor.
            c.args(["-e", "-m", &wt.title]);
        }
        None => {}
    }
    let s = c.status().context("cannot run git")?;
    Ok(if s.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn merge(store: &Store, config: &Config, wt: &Worktree, keep: bool) -> anyhow::Result<()> {
    match worktrees::merge(wt)? {
        Merged::UpToDate => println!(
            "termos: {} already has everything in {}",
            wt.base, wt.branch
        ),
        Merged::FastForward(c) => {
            println!(
                "termos: {} moved forward to {} ({})",
                wt.base,
                wt.branch,
                &c[..10.min(c.len())]
            );
        }
        Merged::Commit(c) => {
            println!(
                "termos: merged {} into {} ({})",
                wt.branch,
                wt.base,
                &c[..10.min(c.len())]
            );
        }
    }
    if config.worktrees.archive_merged && !keep {
        let w = store.archive(wt, false)?;
        let deleted = store.delete_merged_branch(&w).is_ok();
        println!(
            "termos: archived {}{}",
            w.name,
            if deleted { ", its branch deleted" } else { "" }
        );
    }
    Ok(())
}

/// Where a program is on `PATH`.
fn on_path(name: &str) -> Option<PathBuf> {
    termos_devkit::mise::which(name, std::env::var_os("PATH").as_ref())
}

fn pull_request(
    store: &Store,
    wt: &Worktree,
    title: Option<&str>,
    draft: bool,
) -> anyhow::Result<()> {
    let c = worktrees::changes(wt)?;
    if c.uncommitted {
        bail!(
            "{} has changes not committed; `termos worktree commit {}` first",
            wt.name,
            wt.name
        );
    }
    if c.ahead == 0 {
        bail!(
            "{} has no commits that {} lacks",
            wt.branch,
            short(&wt.base)
        );
    }
    let repo = Repo {
        root: wt.repo.clone(),
    };
    if !repo.branch_exists(&wt.base) {
        bail!(
            "{} started from a commit, not a branch; push it and choose a base on the forge",
            wt.name
        );
    }
    let (remote, url) = worktrees::remote_of(wt)?;
    println!("termos: pushing {} to {remote}", wt.branch);
    let s = git_here(wt)
        .args(["push", "-u", &remote, &wt.branch])
        .status()
        .context("cannot run git")?;
    if !s.success() {
        bail!("the push failed");
    }
    let subject = || {
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .args(["log", "-1", "--format=%s"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    let title = title
        .map(str::to_owned)
        .or_else(|| (!wt.title.is_empty()).then(|| wt.title.clone()))
        .unwrap_or_else(subject);
    let body = worktrees::pr_body(wt);
    let forge = worktrees::forge_of(&url);
    let tool = match &forge {
        Forge::GitHub { .. } => on_path("gh").map(|p| (p, "gh")),
        Forge::GitLab { .. } => on_path("glab").map(|p| (p, "glab")),
        Forge::Other => None,
    };
    if let Some((program, kind)) = tool {
        let mut c = Command::new(program);
        if kind == "gh" {
            c.args([
                "pr", "create", "--base", &wt.base, "--head", &wt.branch, "--title", &title,
                "--body", &body,
            ]);
        } else {
            c.args([
                "mr",
                "create",
                "--source-branch",
                &wt.branch,
                "--target-branch",
                &wt.base,
                "--title",
                &title,
                "--description",
                &body,
                "--yes",
            ]);
        }
        if draft {
            c.arg("--draft");
        }
        let out = c
            .current_dir(&wt.path)
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()
            .with_context(|| format!("cannot run {kind}"))?;
        let text = String::from_utf8_lossy(&out.stdout);
        print!("{text}");
        if !out.status.success() {
            bail!("{kind} did not open it");
        }
        if let Some(link) = text
            .split_whitespace()
            .rev()
            .find(|w| w.starts_with("https://"))
        {
            store.set_pr(wt, link)?;
            println!("termos: pull request {link}");
        }
        return Ok(());
    }
    match worktrees::compare_link(&forge, &wt.base, &wt.branch) {
        Some(link) => {
            println!("termos: open the pull request here: {link}");
            let missing = if matches!(forge, Forge::GitLab { .. }) {
                "glab"
            } else {
                "gh"
            };
            println!("        (with `{missing}` installed and signed in, TermOS opens it for you)");
        }
        None => println!(
            "termos: pushed {}; open a pull request on your forge",
            wt.branch
        ),
    }
    Ok(())
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        bail!("{question} Pass --yes to answer from a script.");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// Asks the TermOS session this runs in to open the worktree.
fn open_in_termos(wt: &Worktree) -> anyhow::Result<()> {
    if wt.state != State::Active {
        bail!("{} is {}", wt.name, wt.state.name());
    }
    crate::tos::run_here(&format!("worktree.open {}", wt.name)).with_context(|| {
        format!(
            "cannot open {} in TermOS; run this in a TermOS pane, or open it with `worktree.open {}`",
            wt.name, wt.name
        )
    })
}
