// SPDX-License-Identifier: GPL-3.0-or-later
//! A git worktree per task (M12a, D-113, D-114).
//!
//! A worktree is a branch checked out in a folder of its own
//! (`<root>/<repository>-<id>/<name>`), made with `git worktree add` from
//! the branch the user is on. TermOS records each one (its repository,
//! branch, base, block of ports, where it came from) in
//! `<data>/worktrees/registry.toml`, under a lock, so the server, the
//! harness and `termos worktree` can all make and remove them.
//!
//! - **Setting up:** the project's `termos.toml` says what a new worktree
//!   needs (`[worktree]`: `setup` commands, ignored files to `copy` from
//!   the main checkout, `servers` to start in their own panes).
//! - **Ports:** each worktree gets a block of free ports from
//!   `[worktrees] port-range`, recorded so two never share one, passed to
//!   its programs as `PORT` and `TERMOS_PORT_<n>` ([`Worktree::env`]).
//! - **Review:** [`changes`] lists what changed against the base (commits
//!   and edits not yet committed, untracked files too) and [`file_diff`]
//!   gives a file's diff as numbered lines.
//! - **Finishing:** [`commit_all`], [`merge`] (in the checkout of the base
//!   branch, or in memory with `git merge-tree` when it is not checked
//!   out), [`Store::archive`] (the folder removed, the branch kept),
//!   [`Store::discard`] (the branch deleted too), [`Store::restore`]. An
//!   archived or discarded worktree keeps its last commit reachable as
//!   `refs/termos/archive/<name>`.
//!
//! Everything goes through the `git` program the user has, so their
//! configuration, hooks and credentials apply.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use termos_core::config::WorktreesConfig;

/// The registry, in `<data>/worktrees/`.
pub const REGISTRY: &str = "registry.toml";
/// Hidden refs that keep archived and discarded worktrees' commits.
pub const ARCHIVE_REFS: &str = "refs/termos/archive/";

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("{0}")]
    NotARepo(String),
    #[error("git {args}: {message}")]
    Git { args: String, message: String },
    #[error("cannot run git ({0}); is it installed?")]
    NoGit(std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("no worktree named {0:?}")]
    Unknown(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Ports(String),
    #[error("merging {branch} into {base} conflicts in: {}", files.join(", "))]
    Conflict {
        branch: String,
        base: String,
        files: Vec<String>,
    },
}

type Result<T> = std::result::Result<T, WorktreeError>;

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> WorktreeError + '_ {
    move |source| WorktreeError::Io {
        path: path.to_owned(),
        source,
    }
}

// ---------------------------------------------------------------- git

/// `git -C dir`, never asking on the terminal.
fn git_cmd(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    c
}

/// Runs git; its standard output, or its error.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_env(dir, args, &[])
}

fn git_env(dir: &Path, args: &[&str], env: &[(String, String)]) -> Result<String> {
    let out = git_cmd(dir)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .output()
        .map_err(WorktreeError::NoGit)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(git_failed(args, &out))
    }
}

fn git_failed(args: &[&str], out: &std::process::Output) -> WorktreeError {
    let err = String::from_utf8_lossy(&out.stderr);
    let message = err
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("hint:"))
        .collect::<Vec<_>>()
        .join("; ");
    WorktreeError::Git {
        args: args.join(" "),
        message: if message.is_empty() {
            format!("exit status {}", out.status)
        } else {
            message
        },
    }
}

/// Whether git exits successfully (a question, not an action).
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git_cmd(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// An entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub path: PathBuf,
    pub head: String,
    /// `refs/heads/…` stripped; `None` when detached.
    pub branch: Option<String>,
    pub bare: bool,
}

/// Parses `git worktree list --porcelain`.
pub fn parse_worktree_list(text: &str) -> Vec<Listed> {
    let mut out = Vec::new();
    let mut cur: Option<Listed> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            cur = Some(Listed {
                path: PathBuf::from(p),
                head: String::new(),
                branch: None,
                bare: false,
            });
        } else if let Some(c) = cur.as_mut() {
            if let Some(h) = line.strip_prefix("HEAD ") {
                c.head = h.to_owned();
            } else if let Some(b) = line.strip_prefix("branch ") {
                c.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_owned());
            } else if line == "bare" {
                c.bare = true;
            }
        }
    }
    out.extend(cur);
    out
}

/// The same folder, however it is spelled (macOS's `/var` is
/// `/private/var`; Windows' drive letters and separators).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    a == b || canon(a) == canon(b)
}

// ---------------------------------------------------------------- repo

/// A repository, by its main checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The main worktree's folder.
    pub root: PathBuf,
}

impl Repo {
    /// The repository `dir` is in (from any of its worktrees).
    pub fn find(dir: &Path) -> Result<Self> {
        let list = git(dir, &["worktree", "list", "--porcelain"]).map_err(|e| match e {
            WorktreeError::Git { message, .. } => WorktreeError::NotARepo(format!(
                "{} is not in a git repository ({message})",
                dir.display()
            )),
            other => other,
        })?;
        let main = parse_worktree_list(&list)
            .into_iter()
            .next()
            .ok_or_else(|| WorktreeError::NotARepo(format!("{}: no worktrees", dir.display())))?;
        if main.bare {
            return Err(WorktreeError::NotARepo(format!(
                "{} is a bare repository; worktrees need a main checkout",
                main.path.display()
            )));
        }
        Ok(Self { root: main.path })
    }

    /// The folder name for its worktrees: its own name and a short hash of
    /// where it is, so two clones of one project stay apart.
    pub fn slug(&self) -> String {
        use sha2::Digest as _;
        let name: String = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_owned())
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .take(32)
            .collect();
        let hash = sha2::Sha256::digest(self.root.to_string_lossy().as_bytes());
        let short: String = hash.iter().take(4).map(|b| format!("{b:02x}")).collect();
        format!("{name}-{short}")
    }

    /// The branch the main checkout is on (`None`: detached).
    pub fn current_branch(&self) -> Option<String> {
        git(&self.root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    pub fn worktrees(&self) -> Result<Vec<Listed>> {
        Ok(parse_worktree_list(&git(
            &self.root,
            &["worktree", "list", "--porcelain"],
        )?))
    }

    pub fn branch_exists(&self, branch: &str) -> bool {
        git_ok(
            &self.root,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
    }
}

// ---------------------------------------------------------------- records

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    #[default]
    Active,
    /// Its folder removed, its branch kept.
    Archived,
    /// Its folder and branch removed.
    Discarded,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
            Self::Discarded => "discarded",
        }
    }
}

/// A worktree TermOS made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Worktree {
    pub name: String,
    /// The repository's main checkout.
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    /// The branch it started from, which it merges back into (or a commit,
    /// when it started from one).
    pub base: String,
    pub base_commit: String,
    /// What it is for (the task, the prompt).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Its ports: the first and how many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<(u16, u16)>,
    pub created: u64,
    /// Who made it: empty for the user, `harness:<plan>`, `best:<run>`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
    #[serde(default)]
    pub state: State,
    /// Its last commit, once archived or discarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The pull request opened for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    /// Its `setup` commands finished.
    #[serde(default)]
    pub set_up: bool,
}

impl Worktree {
    /// What its programs get: where it is, its branch, its ports.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("TERMOS_WORKTREE".to_owned(), self.name.clone()),
            ("TERMOS_WORKTREE_BRANCH".to_owned(), self.branch.clone()),
            ("TERMOS_WORKTREE_BASE".to_owned(), self.base.clone()),
            (
                "TERMOS_REPO".to_owned(),
                self.repo.to_string_lossy().into_owned(),
            ),
        ];
        if let Some((first, count)) = self.ports {
            env.push(("PORT".to_owned(), first.to_string()));
            for i in 0..count {
                env.push((format!("TERMOS_PORT_{i}"), (first + i).to_string()));
            }
            env.push((
                "TERMOS_PORTS".to_owned(),
                format!("{first}-{}", first + count.saturating_sub(1)),
            ));
        }
        env
    }

    /// Its ports as text: `20010-20019`.
    pub fn ports_text(&self) -> String {
        match self.ports {
            Some((first, count)) => format!("{first}-{}", first + count.saturating_sub(1)),
            None => String::new(),
        }
    }

    pub fn active(&self) -> bool {
        self.state == State::Active
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default, rename = "worktree")]
    worktrees: Vec<Worktree>,
}

/// A name for a worktree and its branch.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        && !name.ends_with(".lock")
        && !name.contains("..");
    if ok {
        Ok(())
    } else {
        Err(WorktreeError::Invalid(format!(
            "{name:?} is not a worktree name: letters, digits, '.', '_' and '-', starting with a letter or digit, at most 48"
        )))
    }
}

/// A name from a title: `Fix the login form` → `fix-the-login-form`.
pub fn name_from(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// The first block of `count` ports in `range` that overlaps none of
/// `taken` (first, count) and whose ports are all `free`.
pub fn pick_ports(
    range: (u16, u16),
    count: u16,
    taken: &[(u16, u16)],
    free: impl Fn(u16) -> bool,
) -> Option<u16> {
    if count == 0 {
        return None;
    }
    let (lo, hi) = (u32::from(range.0), u32::from(range.1));
    let n = u32::from(count);
    let mut start = lo;
    while start + n - 1 <= hi {
        let end = start + n - 1;
        let clash = taken.iter().find(|(f, c)| {
            let (f, c) = (u32::from(*f), u32::from(*c));
            f <= end && start < f + c
        });
        if let Some((f, c)) = clash {
            // Past the block in the way.
            start = u32::from(*f) + u32::from(*c);
            continue;
        }
        // u16 holds every port in range (hi <= u16::MAX).
        let busy = (start..=end).find(|&p| !free(u16::try_from(p).unwrap_or(u16::MAX)));
        match busy {
            Some(p) => start = p + 1,
            None => return u16::try_from(start).ok(),
        }
    }
    None
}

/// Whether a local port can be listened on now.
pub fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// What to make.
#[derive(Debug, Clone, Default)]
pub struct NewWorktree {
    /// `None`: from the title, else `task-<n>`.
    pub name: Option<String>,
    /// What it starts from: a branch or commit (default: the main
    /// checkout's branch, else its commit).
    pub from: Option<String>,
    /// An existing branch to check out instead of a new one.
    pub branch: Option<String>,
    pub title: String,
    pub origin: String,
    /// Reserve a block of ports.
    pub ports: bool,
}

/// The worktrees TermOS made, and making and removing them.
#[derive(Debug, Clone)]
pub struct Store {
    /// `<data>/worktrees`: the registry, and the worktrees unless
    /// `[worktrees] root` says otherwise.
    dir: PathBuf,
    config: WorktreesConfig,
}

/// Holds the registry's lock while it lives.
struct Locked {
    _file: std::fs::File,
}

impl Store {
    pub fn new(data_dir: &Path, config: &WorktreesConfig) -> Self {
        Self {
            dir: data_dir.join("worktrees"),
            config: config.clone(),
        }
    }

    pub fn config(&self) -> &WorktreesConfig {
        &self.config
    }

    /// Where worktrees are made.
    pub fn root(&self) -> PathBuf {
        self.config.root.clone().unwrap_or_else(|| self.dir.clone())
    }

    fn registry(&self) -> PathBuf {
        self.dir.join(REGISTRY)
    }

    fn lock(&self) -> Result<Locked> {
        std::fs::create_dir_all(&self.dir).map_err(io_err(&self.dir))?;
        let path = self.dir.join("registry.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_err(&path))?;
        file.lock().map_err(io_err(&path))?;
        Ok(Locked { _file: file })
    }

    fn read(&self) -> Result<Vec<Worktree>> {
        let path = self.registry();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<RegistryFile>(&text)
                .map(|f| f.worktrees)
                .map_err(|e| WorktreeError::Invalid(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(io_err(&path)(e)),
        }
    }

    fn write(&self, list: &[Worktree]) -> Result<()> {
        let path = self.registry();
        let text = toml::to_string(&RegistryFile {
            worktrees: list.to_vec(),
        })
        .map_err(|e| WorktreeError::Invalid(e.to_string()))?;
        let tmp = self
            .dir
            .join(format!("{REGISTRY}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, text).map_err(io_err(&tmp))?;
        std::fs::rename(&tmp, &path).map_err(io_err(&path))
    }

    /// Changes the registry under its lock.
    fn edit<R>(&self, f: impl FnOnce(&mut Vec<Worktree>) -> Result<R>) -> Result<R> {
        let _lock = self.lock()?;
        let mut list = self.read()?;
        let r = f(&mut list)?;
        self.write(&list)?;
        Ok(r)
    }

    /// Every worktree TermOS made (all repositories, archived ones too).
    pub fn list(&self) -> Result<Vec<Worktree>> {
        self.read()
    }

    /// `name`: in `repo` when given, else the only one so named.
    pub fn get(&self, repo: Option<&Repo>, name: &str) -> Result<Worktree> {
        let list = self.read()?;
        let mut found: Vec<&Worktree> = list.iter().filter(|w| w.name == name).collect();
        if let Some(r) = repo {
            let here: Vec<&Worktree> = found
                .iter()
                .copied()
                .filter(|w| same_path(&w.repo, &r.root))
                .collect();
            if !here.is_empty() {
                found = here;
            }
        }
        match found.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(WorktreeError::Unknown(name.to_owned())),
            many => Err(WorktreeError::Invalid(format!(
                "{} repositories have a worktree named {name:?} ({}); run this inside the one you mean",
                many.len(),
                many.iter()
                    .map(|w| w.repo.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// The active worktree `path` is in.
    pub fn containing(&self, path: &Path) -> Option<Worktree> {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        let path = canon(path);
        self.read()
            .ok()?
            .into_iter()
            .filter(Worktree::active)
            .find(|w| path.starts_with(canon(&w.path)))
    }

    /// Makes a worktree: its branch, its folder (`git worktree add`), its
    /// ports, its record. Copying files and the setup come after
    /// ([`copy_ignored`], [`ProjectSettings::setup`]).
    pub fn create(&self, repo: &Repo, req: &NewWorktree) -> Result<Worktree> {
        self.edit(|list| {
            let in_repo: Vec<&Worktree> = list
                .iter()
                .filter(|w| same_path(&w.repo, &repo.root))
                .collect();
            let folder = self.root().join(repo.slug());
            let taken_name =
                |n: &str| in_repo.iter().any(|w| w.name == n) || folder.join(n).exists();
            let name = match &req.name {
                Some(n) => n.clone(),
                None => {
                    let from_title = name_from(&req.title);
                    if !from_title.is_empty()
                        && check_name(&from_title).is_ok()
                        && !taken_name(&from_title)
                    {
                        from_title
                    } else {
                        (1..)
                            .map(|i| format!("task-{i}"))
                            .find(|n| !taken_name(n))
                            .unwrap_or_else(|| "task".to_owned())
                    }
                }
            };
            check_name(&name)?;
            if in_repo.iter().any(|w| w.name == name) {
                return Err(WorktreeError::Invalid(format!(
                    "{} already has a worktree named {name:?} (`termos worktree list --all`)",
                    repo.root.display()
                )));
            }
            let path = folder.join(&name);
            if path.exists() {
                return Err(WorktreeError::Invalid(format!(
                    "{} already exists",
                    path.display()
                )));
            }
            let from = match &req.from {
                Some(f) => f.clone(),
                None => repo.current_branch().unwrap_or_else(|| "HEAD".to_owned()),
            };
            let base_commit = git(
                &repo.root,
                &["rev-parse", "--verify", &format!("{from}^{{commit}}")],
            )
            .map_err(|_| {
                WorktreeError::Invalid(format!("{from:?} is not a branch or commit here"))
            })?
            .trim()
            .to_owned();
            let base = if repo.branch_exists(&from) {
                from.clone()
            } else {
                base_commit.clone()
            };
            let ports = if req.ports && self.config.ports > 0 {
                let taken: Vec<(u16, u16)> = list
                    .iter()
                    .filter(|w| w.active())
                    .filter_map(|w| w.ports)
                    .collect();
                let first =
                    pick_ports(self.config.port_range, self.config.ports, &taken, port_free)
                        .ok_or_else(|| {
                            WorktreeError::Ports(format!(
                                "no {} free ports left in {}-{} ([worktrees] port-range)",
                                self.config.ports,
                                self.config.port_range.0,
                                self.config.port_range.1
                            ))
                        })?;
                Some((first, self.config.ports))
            } else {
                None
            };
            std::fs::create_dir_all(&folder).map_err(io_err(&folder))?;
            let path_s = path.to_string_lossy().into_owned();
            let branch = match &req.branch {
                Some(b) => {
                    if !repo.branch_exists(b) {
                        return Err(WorktreeError::Invalid(format!("there is no branch {b:?}")));
                    }
                    git(&repo.root, &["worktree", "add", &path_s, b])?;
                    b.clone()
                }
                None => {
                    let b = format!("{}{name}", self.config.branch_prefix);
                    if repo.branch_exists(&b) {
                        return Err(WorktreeError::Invalid(format!(
                            "branch {b:?} already exists (`--branch {b}` checks it out)"
                        )));
                    }
                    git(
                        &repo.root,
                        &["worktree", "add", "-b", &b, &path_s, &base_commit],
                    )?;
                    b
                }
            };
            let wt = Worktree {
                name,
                repo: repo.root.clone(),
                path,
                branch,
                base,
                base_commit,
                title: req.title.clone(),
                ports,
                created: crate::devroot::now(),
                origin: req.origin.clone(),
                state: State::Active,
                head: None,
                pr: None,
                set_up: false,
            };
            list.push(wt.clone());
            Ok(wt)
        })
    }

    fn update(&self, wt: &Worktree, f: impl FnOnce(&mut Worktree)) -> Result<Worktree> {
        self.edit(|list| {
            let w = list
                .iter_mut()
                .find(|w| w.name == wt.name && same_path(&w.repo, &wt.repo))
                .ok_or_else(|| WorktreeError::Unknown(wt.name.clone()))?;
            f(w);
            Ok(w.clone())
        })
    }

    pub fn mark_set_up(&self, wt: &Worktree) -> Result<Worktree> {
        self.update(wt, |w| w.set_up = true)
    }

    pub fn set_pr(&self, wt: &Worktree, url: &str) -> Result<Worktree> {
        self.update(wt, |w| w.pr = Some(url.to_owned()))
    }

    /// Removes its folder and keeps its branch; its ports go back.
    /// Refused while it has changes or untracked files, unless `force`.
    pub fn archive(&self, wt: &Worktree, force: bool) -> Result<Worktree> {
        self.put_away(wt, force, State::Archived, false)
    }

    /// Removes its folder and its branch (changes and untracked files
    /// included). Its last commit stays reachable for
    /// [`Store::restore`].
    pub fn discard(&self, wt: &Worktree) -> Result<Worktree> {
        self.put_away(wt, true, State::Discarded, true)
    }

    fn put_away(
        &self,
        wt: &Worktree,
        force: bool,
        state: State,
        delete_branch: bool,
    ) -> Result<Worktree> {
        if !wt.active() {
            return Err(WorktreeError::Invalid(format!(
                "{} is already {}",
                wt.name,
                wt.state.name()
            )));
        }
        let head = if wt.path.exists() {
            if !force {
                let status = git(&wt.path, &["status", "--porcelain"])?;
                if !status.trim().is_empty() {
                    return Err(WorktreeError::Invalid(format!(
                        "{} has changes not committed, or untracked files; commit them, or discard it",
                        wt.name
                    )));
                }
            }
            git(&wt.path, &["rev-parse", "HEAD"])?.trim().to_owned()
        } else {
            git(
                &wt.repo,
                &["rev-parse", &format!("refs/heads/{}", wt.branch)],
            )
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| wt.base_commit.clone())
        };
        git(
            &wt.repo,
            &["update-ref", &format!("{ARCHIVE_REFS}{}", wt.name), &head],
        )?;
        if wt.path.exists() {
            let p = wt.path.to_string_lossy().into_owned();
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&p);
            git(&wt.repo, &args)?;
        } else {
            git(&wt.repo, &["worktree", "prune"])?;
        }
        if delete_branch && wt.repo.exists() {
            // Gone already is fine.
            let _ = git(&wt.repo, &["branch", "-D", &wt.branch]);
        }
        self.update(wt, |w| {
            w.state = state;
            w.head = Some(head);
            w.ports = None;
            w.set_up = false;
        })
    }

    /// Deletes the branch of a merged worktree (`git branch -d`, which
    /// refuses an unmerged one).
    pub fn delete_merged_branch(&self, wt: &Worktree) -> Result<()> {
        git(&wt.repo, &["branch", "-d", &wt.branch]).map(|_| ())
    }

    /// Checks an archived or discarded worktree out again, in its folder,
    /// with new ports.
    pub fn restore(&self, wt: &Worktree) -> Result<Worktree> {
        if wt.active() {
            return Err(WorktreeError::Invalid(format!(
                "{} is not archived",
                wt.name
            )));
        }
        let repo = Repo {
            root: wt.repo.clone(),
        };
        let path_s = wt.path.to_string_lossy().into_owned();
        if repo.branch_exists(&wt.branch) {
            git(&repo.root, &["worktree", "add", &path_s, &wt.branch])?;
        } else {
            let from = format!("{ARCHIVE_REFS}{}", wt.name);
            git(
                &repo.root,
                &["worktree", "add", "-b", &wt.branch, &path_s, &from],
            )?;
        }
        let config = self.config.clone();
        self.edit(|list| {
            let taken: Vec<(u16, u16)> = list
                .iter()
                .filter(|w| w.active())
                .filter_map(|w| w.ports)
                .collect();
            let ports = pick_ports(config.port_range, config.ports, &taken, port_free)
                .map(|f| (f, config.ports));
            let w = list
                .iter_mut()
                .find(|w| w.name == wt.name && same_path(&w.repo, &wt.repo))
                .ok_or_else(|| WorktreeError::Unknown(wt.name.clone()))?;
            w.state = State::Active;
            w.ports = ports;
            w.head = None;
            Ok(w.clone())
        })
    }

    /// Forgets an archived or discarded worktree for good: its record and
    /// the ref that kept its commit.
    pub fn forget(&self, wt: &Worktree) -> Result<()> {
        if wt.active() {
            return Err(WorktreeError::Invalid(format!(
                "{} is still checked out; archive or discard it first",
                wt.name
            )));
        }
        if wt.repo.exists() {
            let _ = git(
                &wt.repo,
                &["update-ref", "-d", &format!("{ARCHIVE_REFS}{}", wt.name)],
            );
        }
        self.edit(|list| {
            list.retain(|w| !(w.name == wt.name && same_path(&w.repo, &wt.repo)));
            Ok(())
        })
    }
}

// ---------------------------------------------------------------- project

/// `[worktree]` in a project's `termos.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProjectSettings {
    /// Commands run in a new worktree, in order (`npm install`).
    #[serde(deserialize_with = "one_or_many")]
    pub setup: Vec<String>,
    /// Ignored files (and folders) copied from the main checkout, as
    /// globs relative to it.
    pub copy: Vec<String>,
    /// Commands that each run in a pane of their own when the worktree
    /// opens (`npm run dev`).
    #[serde(deserialize_with = "one_or_many")]
    pub servers: Vec<String>,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            setup: Vec::new(),
            copy: vec!["**/.env".to_owned(), "**/.env.*".to_owned()],
            servers: Vec::new(),
        }
    }
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(String),
        Many(Vec<String>),
    }
    Ok(match V::deserialize(d)? {
        V::One(s) => vec![s],
        V::Many(v) => v,
    })
}

impl ProjectSettings {
    /// From the worktree's `termos.toml`, else the main checkout's.
    pub fn load(wt: &Worktree) -> Result<Self> {
        match Self::read(&wt.path)? {
            Some(s) => Ok(s),
            None => Ok(Self::read(&wt.repo)?.unwrap_or_default()),
        }
    }

    /// `[worktree]` in `dir/termos.toml`; `None` when the file is not
    /// there.
    pub fn read(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join("termos.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(&path)(e)),
        };
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            worktree: Option<ProjectSettings>,
        }
        // Other tables ([tasks]) belong to others.
        let table: toml::Table = toml::from_str(&text)
            .map_err(|e| WorktreeError::Invalid(format!("{}: {e}", path.display())))?;
        let only = toml::Table::from_iter(
            table
                .get("worktree")
                .map(|v| ("worktree".to_owned(), v.clone())),
        );
        let f: File = toml::Value::Table(only)
            .try_into()
            .map_err(|e| WorktreeError::Invalid(format!("{}: [worktree]: {e}", path.display())))?;
        Ok(Some(f.worktree.unwrap_or_default()))
    }
}

/// Copies the ignored files and folders of the main checkout that match
/// `patterns` into the worktree; what it copied. A pattern matches an
/// ignored file, or an ignored folder as a whole, as git lists them.
pub fn copy_ignored(wt: &Worktree, patterns: &[String]) -> Result<Vec<String>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let mut set = globset::GlobSetBuilder::new();
    for p in patterns {
        let g = globset::GlobBuilder::new(p)
            .literal_separator(true)
            .build()
            .map_err(|e| WorktreeError::Invalid(format!("[worktree] copy: {p:?}: {e}")))?;
        set.add(g);
    }
    let set = set
        .build()
        .map_err(|e| WorktreeError::Invalid(e.to_string()))?;
    let listed = git(
        &wt.repo,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )?;
    let mut copied = Vec::new();
    for entry in listed.split('\0').filter(|e| !e.is_empty()) {
        let rel = entry.trim_end_matches('/');
        if !set.is_match(rel) {
            continue;
        }
        let from = wt.repo.join(rel);
        let to = wt.path.join(rel);
        if to.exists() {
            continue;
        }
        copy_tree(&from, &to)?;
        copied.push(rel.to_owned());
    }
    Ok(copied)
}

/// Copies a file, a link or a folder (its contents).
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(from).map_err(io_err(from))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            let target = std::fs::read_link(from).map_err(io_err(from))?;
            std::os::unix::fs::symlink(target, to).map_err(io_err(to))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::copy(from, to).map_err(io_err(to))?;
        }
    } else if meta.is_dir() {
        std::fs::create_dir_all(to).map_err(io_err(to))?;
        for e in std::fs::read_dir(from).map_err(io_err(from))? {
            let e = e.map_err(io_err(from))?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
    } else if meta.is_file() {
        std::fs::copy(from, to).map_err(io_err(to))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- review

/// A changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    /// Where a renamed file was.
    pub old_path: Option<String>,
    /// `A`dded, `M`odified, `D`eleted, `R`enamed, `?` untracked, …
    pub status: char,
    /// Lines added and removed (`None`: binary).
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// What a worktree changed against its base.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    pub files: Vec<Change>,
    /// Commits on its branch that the base does not have, and the other
    /// way round.
    pub ahead: u32,
    pub behind: u32,
    /// Edits (or untracked files) not committed yet.
    pub uncommitted: bool,
    /// Where the diff starts.
    pub merge_base: String,
}

impl Changes {
    pub fn added(&self) -> u32 {
        self.files.iter().filter_map(|f| f.added).sum()
    }
    pub fn removed(&self) -> u32 {
        self.files.iter().filter_map(|f| f.removed).sum()
    }
}

/// Where `wt`'s changes start: the merge base of its base and `HEAD`.
fn merge_base(wt: &Worktree) -> Result<String> {
    let base = if git_ok(&wt.path, &["rev-parse", "--verify", "--quiet", &wt.base]) {
        wt.base.as_str()
    } else {
        wt.base_commit.as_str()
    };
    Ok(git(&wt.path, &["merge-base", base, "HEAD"])
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| wt.base_commit.clone()))
}

/// Parses `git diff --name-status -z -M` and `--numstat -z -M` together.
pub fn parse_changes(name_status: &str, numstat: &str) -> Vec<Change> {
    let mut files = Vec::new();
    let mut it = name_status.split('\0').filter(|s| !s.is_empty());
    while let Some(code) = it.next() {
        let status = code.chars().next().unwrap_or('M');
        let (old_path, path) = if matches!(status, 'R' | 'C') {
            let old = it.next().unwrap_or_default().to_owned();
            (Some(old), it.next().unwrap_or_default().to_owned())
        } else {
            (None, it.next().unwrap_or_default().to_owned())
        };
        files.push(Change {
            path,
            old_path,
            status,
            added: None,
            removed: None,
        });
    }
    // numstat -z: "a\tr\tpath\0", or for a rename "a\tr\t\0old\0new\0".
    let mut parts = numstat.split('\0');
    while let Some(head) = parts.next() {
        if head.is_empty() {
            continue;
        }
        let mut cols = head.splitn(3, '\t');
        let (a, r, p) = (
            cols.next().unwrap_or_default(),
            cols.next().unwrap_or_default(),
            cols.next().unwrap_or_default(),
        );
        let path = if p.is_empty() {
            let _old = parts.next();
            parts.next().unwrap_or_default().to_owned()
        } else {
            p.to_owned()
        };
        if let Some(f) = files.iter_mut().find(|f| f.path == path) {
            f.added = a.parse().ok();
            f.removed = r.parse().ok();
        }
    }
    files
}

/// Lines in a text file (`None`: binary or too large to show).
fn text_lines(path: &Path) -> Option<Vec<String>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 512 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return None;
    }
    Some(
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_owned)
            .collect(),
    )
}

/// What `wt` changed against its base: its commits and its edits.
pub fn changes(wt: &Worktree) -> Result<Changes> {
    let mb = merge_base(wt)?;
    let ns = git(&wt.path, &["diff", "--name-status", "-z", "-M", &mb])?;
    let num = git(&wt.path, &["diff", "--numstat", "-z", "-M", &mb])?;
    let mut files = parse_changes(&ns, &num);
    let untracked = git(
        &wt.path,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?;
    for p in untracked.split('\0').filter(|s| !s.is_empty()) {
        let lines = text_lines(&wt.path.join(p));
        files.push(Change {
            path: p.to_owned(),
            old_path: None,
            status: '?',
            added: lines.map(|l| u32::try_from(l.len()).unwrap_or(u32::MAX)),
            removed: Some(0),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let base = if git_ok(&wt.path, &["rev-parse", "--verify", "--quiet", &wt.base]) {
        wt.base.clone()
    } else {
        wt.base_commit.clone()
    };
    let counts = git(
        &wt.path,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base}...HEAD"),
        ],
    )
    .unwrap_or_default();
    let mut n = counts
        .split_whitespace()
        .map(|x| x.parse::<u32>().unwrap_or(0));
    let behind = n.next().unwrap_or(0);
    let ahead = n.next().unwrap_or(0);
    let uncommitted = !git(&wt.path, &["status", "--porcelain"])?.trim().is_empty();
    Ok(Changes {
        files,
        ahead,
        behind,
        uncommitted,
        merge_base: mb,
    })
}

/// What a diff line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// `diff --git`, `index`, `---`, `+++`.
    Header,
    /// `@@ -a,b +c,d @@`.
    Hunk,
    Add,
    Del,
    Context,
    /// A note in place of a diff (binary, too large).
    Note,
}

/// A line of a file's diff, with its numbers in the old and new file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    pub old: Option<u32>,
    pub new: Option<u32>,
}

/// Parses a unified diff into numbered lines.
pub fn parse_diff(text: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    for line in text.lines() {
        if line.starts_with("@@") {
            // @@ -a[,b] +c[,d] @@
            let nums: Vec<u32> = line
                .split_whitespace()
                .skip(1)
                .take(2)
                .filter_map(|t| t[1..].split(',').next()?.parse().ok())
                .collect();
            if let [a, c] = nums[..] {
                old = a;
                new = c;
            }
            in_hunk = true;
            out.push(DiffLine {
                kind: LineKind::Hunk,
                text: line.to_owned(),
                old: None,
                new: None,
            });
        } else if !in_hunk || line.starts_with("diff --git") {
            in_hunk = false;
            out.push(DiffLine {
                kind: LineKind::Header,
                text: line.to_owned(),
                old: None,
                new: None,
            });
        } else if let Some(t) = line.strip_prefix('+') {
            out.push(DiffLine {
                kind: LineKind::Add,
                text: t.to_owned(),
                old: None,
                new: Some(new),
            });
            new += 1;
        } else if let Some(t) = line.strip_prefix('-') {
            out.push(DiffLine {
                kind: LineKind::Del,
                text: t.to_owned(),
                old: Some(old),
                new: None,
            });
            old += 1;
        } else if line.starts_with('\\') {
            out.push(DiffLine {
                kind: LineKind::Note,
                text: line.to_owned(),
                old: None,
                new: None,
            });
        } else {
            let t = line.strip_prefix(' ').unwrap_or(line);
            out.push(DiffLine {
                kind: LineKind::Context,
                text: t.to_owned(),
                old: Some(old),
                new: Some(new),
            });
            old += 1;
            new += 1;
        }
    }
    out
}

/// A changed file's diff against the base.
pub fn file_diff(wt: &Worktree, change: &Change, merge_base: &str) -> Result<Vec<DiffLine>> {
    if change.status == '?' {
        let p = wt.path.join(&change.path);
        return Ok(match text_lines(&p) {
            Some(lines) => lines
                .into_iter()
                .zip(1u32..)
                .map(|(text, n)| DiffLine {
                    kind: LineKind::Add,
                    text,
                    old: None,
                    new: Some(n),
                })
                .collect(),
            None => vec![DiffLine {
                kind: LineKind::Note,
                text: "a new binary or large file".to_owned(),
                old: None,
                new: None,
            }],
        });
    }
    let mut args = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        "-M",
        merge_base,
        "--",
    ];
    if let Some(o) = &change.old_path {
        args.push(o);
    }
    args.push(&change.path);
    let text = git(&wt.path, &args)?;
    let lines = parse_diff(&text);
    if change.added.is_none() && !lines.iter().any(|l| l.kind == LineKind::Hunk) {
        let mut l = lines;
        l.push(DiffLine {
            kind: LineKind::Note,
            text: "a binary file".to_owned(),
            old: None,
            new: None,
        });
        return Ok(l);
    }
    Ok(lines)
}

/// The whole diff against the base, as text: committed and not, new
/// files included (written here, so the index is left alone).
pub fn diff_text(wt: &Worktree) -> Result<String> {
    let mb = merge_base(wt)?;
    let mut text = git(
        &wt.path,
        &["diff", "--no-color", "--no-ext-diff", "-M", &mb],
    )?;
    let untracked = git(
        &wt.path,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?;
    for p in untracked.split('\0').filter(|s| !s.is_empty()) {
        text.push_str(&format!(
            "diff --git a/{p} b/{p}\nnew file\n--- /dev/null\n+++ b/{p}\n"
        ));
        match text_lines(&wt.path.join(p)) {
            Some(lines) => {
                text.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
                for l in lines {
                    text.push('+');
                    text.push_str(&l);
                    text.push('\n');
                }
            }
            None => text.push_str("Binary or large file\n"),
        }
    }
    Ok(text)
}

// ---------------------------------------------------------------- finishing

/// Who commits when the user's git has no name: harness and best-of-N
/// commits in their own worktrees, merged later.
fn identity(dir: &Path) -> Vec<(String, String)> {
    let has = git(dir, &["config", "user.email"]).is_ok_and(|s| !s.trim().is_empty());
    if has {
        Vec::new()
    } else {
        ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"]
            .into_iter()
            .map(|k| (k.to_owned(), "TermOS".to_owned()))
            .chain(
                ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"]
                    .into_iter()
                    .map(|k| (k.to_owned(), "termos@localhost".to_owned())),
            )
            .collect()
    }
}

/// Commits every change in the worktree (`git add -A`); the new commit,
/// or `None` when there was nothing to commit. Without a configured name
/// the commit is TermOS's.
pub fn commit_all(wt: &Worktree, message: &str) -> Result<Option<String>> {
    git(&wt.path, &["add", "-A"])?;
    if git_ok(&wt.path, &["diff", "--cached", "--quiet"]) {
        return Ok(None);
    }
    git_env(
        &wt.path,
        &["commit", "--no-verify", "-q", "-m", message],
        &identity(&wt.path),
    )?;
    Ok(Some(
        git(&wt.path, &["rev-parse", "HEAD"])?.trim().to_owned(),
    ))
}

/// How a merge went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merged {
    /// The base already had it all.
    UpToDate,
    /// The base moved forward to the branch.
    FastForward(String),
    /// A merge commit.
    Commit(String),
}

/// Merges the worktree's branch into its base: in the folder where the
/// base is checked out (which must be clean), else without a checkout
/// (`git merge-tree`). Refused while the worktree has changes not
/// committed. A conflict leaves everything as it was.
pub fn merge(wt: &Worktree) -> Result<Merged> {
    let repo = Repo {
        root: wt.repo.clone(),
    };
    if !repo.branch_exists(&wt.base) {
        return Err(WorktreeError::Invalid(format!(
            "{} started from {}, which is not a branch; push it or open a pull request instead",
            wt.name, wt.base
        )));
    }
    if wt.path.exists() && !git(&wt.path, &["status", "--porcelain"])?.trim().is_empty() {
        return Err(WorktreeError::Invalid(format!(
            "{} has changes not committed; commit them first",
            wt.name
        )));
    }
    let head = git(
        &repo.root,
        &["rev-parse", &format!("refs/heads/{}", wt.branch)],
    )?
    .trim()
    .to_owned();
    let base_ref = format!("refs/heads/{}", wt.base);
    let old = git(&repo.root, &["rev-parse", &base_ref])?
        .trim()
        .to_owned();
    if git_ok(&repo.root, &["merge-base", "--is-ancestor", &head, &old]) {
        return Ok(Merged::UpToDate);
    }
    let checkout = repo
        .worktrees()?
        .into_iter()
        .find(|l| l.branch.as_deref() == Some(wt.base.as_str()));
    if let Some(w) = checkout {
        let tracked = git(&w.path, &["status", "--porcelain", "--untracked-files=no"])?;
        if !tracked.trim().is_empty() {
            return Err(WorktreeError::Invalid(format!(
                "{} is checked out in {}, which has changes not committed; commit or stash them first",
                wt.base,
                w.path.display()
            )));
        }
        let msg = merge_message(wt);
        let out = git_cmd(&w.path)
            .args(["merge", "--no-edit", "-m", &msg, &wt.branch])
            .output()
            .map_err(WorktreeError::NoGit)?;
        if !out.status.success() {
            let files: Vec<String> = git(&w.path, &["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            if git_ok(&w.path, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]) {
                let _ = git(&w.path, &["merge", "--abort"]);
            }
            if files.is_empty() {
                return Err(git_failed(&["merge", &wt.branch], &out));
            }
            return Err(WorktreeError::Conflict {
                branch: wt.branch.clone(),
                base: wt.base.clone(),
                files,
            });
        }
        let new = git(&w.path, &["rev-parse", "HEAD"])?.trim().to_owned();
        let parents = git(&w.path, &["rev-list", "--parents", "-n", "1", "HEAD"])?;
        return Ok(if parents.split_whitespace().count() > 2 {
            Merged::Commit(new)
        } else {
            Merged::FastForward(new)
        });
    }
    // Not checked out anywhere: move the branch, or merge in memory.
    if git_ok(&repo.root, &["merge-base", "--is-ancestor", &old, &head]) {
        git(&repo.root, &["update-ref", &base_ref, &head, &old])?;
        return Ok(Merged::FastForward(head));
    }
    let out = git_cmd(&repo.root)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            &old,
            &head,
        ])
        .output()
        .map_err(WorktreeError::NoGit)?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    match out.status.code() {
        Some(0) => {}
        Some(1) => {
            return Err(WorktreeError::Conflict {
                branch: wt.branch.clone(),
                base: wt.base.clone(),
                files: text
                    .lines()
                    .skip(1)
                    .filter(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect(),
            });
        }
        _ => {
            let e = git_failed(&["merge-tree", "--write-tree"], &out);
            return Err(WorktreeError::Invalid(format!(
                "{e} (merging into a branch that is not checked out needs git 2.38 or newer; check {} out and merge there)",
                wt.base
            )));
        }
    }
    let tree = text.lines().next().unwrap_or_default().trim().to_owned();
    let msg = merge_message(wt);
    let commit = git(
        &repo.root,
        &["commit-tree", &tree, "-p", &old, "-p", &head, "-m", &msg],
    )?
    .trim()
    .to_owned();
    git(&repo.root, &["update-ref", &base_ref, &commit, &old])?;
    Ok(Merged::Commit(commit))
}

fn merge_message(wt: &Worktree) -> String {
    if wt.title.is_empty() {
        format!("Merge branch '{}'", wt.branch)
    } else {
        format!("Merge branch '{}': {}", wt.branch, wt.title)
    }
}

// ---------------------------------------------------------------- forges

/// Where a repository's pull requests are opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forge {
    /// `web`: `https://github.com/owner/repo`.
    GitHub {
        web: String,
    },
    GitLab {
        web: String,
    },
    Other,
}

/// The forge of a remote URL (`git@github.com:o/r.git`,
/// `https://gitlab.example.com/group/r`, `ssh://git@host:22/o/r`).
pub fn forge_of(url: &str) -> Forge {
    let url = url.trim();
    let (host, path) = if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
        let rest = rest.split_once('@').map_or(rest, |(_, r)| r);
        match rest.split_once('/') {
            Some((h, p)) => (h.split(':').next().unwrap_or(h), p),
            None => return Forge::Other,
        }
    } else if let Some((user_host, p)) = url.split_once(':') {
        (user_host.rsplit('@').next().unwrap_or(user_host), p)
    } else {
        return Forge::Other;
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    if path.is_empty() || host.is_empty() {
        return Forge::Other;
    }
    let web = format!("https://{host}/{path}");
    let host = host.to_ascii_lowercase();
    if host == "github.com" || host.starts_with("github.") {
        Forge::GitHub { web }
    } else if host == "gitlab.com" || host.contains("gitlab") {
        Forge::GitLab { web }
    } else {
        Forge::Other
    }
}

/// The page that opens a pull request of `branch` into `base`.
pub fn compare_link(forge: &Forge, base: &str, branch: &str) -> Option<String> {
    match forge {
        Forge::GitHub { web } => Some(format!("{web}/compare/{base}...{branch}?expand=1")),
        Forge::GitLab { web } => Some(format!(
            "{web}/-/merge_requests/new?merge_request%5Bsource_branch%5D={branch}&merge_request%5Btarget_branch%5D={base}"
        )),
        Forge::Other => None,
    }
}

/// The remote a worktree's branch goes to: its upstream's, else `origin`,
/// else the only one. `(name, url)`.
pub fn remote_of(wt: &Worktree) -> Result<(String, String)> {
    let names: Vec<String> = git(&wt.repo, &["remote"])?
        .lines()
        .map(str::to_owned)
        .collect();
    let upstream = git(
        &wt.repo,
        &["config", &format!("branch.{}.remote", wt.branch)],
    )
    .ok()
    .map(|s| s.trim().to_owned());
    let name = upstream
        .filter(|u| names.contains(u))
        .or_else(|| names.iter().find(|n| *n == "origin").cloned())
        .or_else(|| (names.len() == 1).then(|| names[0].clone()))
        .ok_or_else(|| {
            WorktreeError::Invalid(if names.is_empty() {
                "the repository has no remote to push to".to_owned()
            } else {
                format!(
                    "which remote? ({}); set branch.{}.remote",
                    names.join(", "),
                    wt.branch
                )
            })
        })?;
    let url = git(&wt.repo, &["remote", "get-url", &name])?
        .trim()
        .to_owned();
    Ok((name, url))
}

/// A pull request's body: the commits it brings.
pub fn pr_body(wt: &Worktree) -> String {
    let log = git(
        &wt.repo,
        &[
            "log",
            "--reverse",
            "--format=- %s",
            &format!("{}..{}", wt.base, wt.branch),
        ],
    )
    .unwrap_or_default();
    let mut body = String::new();
    if !wt.title.is_empty() {
        body.push_str(&wt.title);
        body.push_str("\n\n");
    }
    body.push_str(log.trim_end());
    body
}

/// Writes the setup's output to `out` as it runs, each command in a shell
/// in the worktree with its environment; stops at the first that fails.
pub fn run_setup(wt: &Worktree, commands: &[String], out: &mut dyn std::io::Write) -> Result<()> {
    for c in commands {
        let _ = writeln!(out, "$ {c}");
        let (program, args) = crate::tasks::shell(c);
        let status = Command::new(&program)
            .args(&args)
            .current_dir(&wt.path)
            .envs(wt.env())
            .status()
            .map_err(io_err(Path::new(&program)))?;
        if !status.success() {
            return Err(WorktreeError::Invalid(format!(
                "setup: `{c}` failed ({status})"
            )));
        }
    }
    Ok(())
}

/// Names of the worktrees in `list` that belong to `repo`.
pub fn names_in(list: &[Worktree], repo: &Repo) -> BTreeSet<String> {
    list.iter()
        .filter(|w| same_path(&w.repo, &repo.root))
        .map(|w| w.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with one commit on `main`, its own identity, and no
    /// global settings in the way.
    fn repo() -> (tempfile::TempDir, Repo) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("proj");
        std::fs::create_dir_all(&root).expect("mkdir");
        let g = |args: &[&str]| git(&root, args).expect("git");
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.name", "Fixture"]);
        g(&["config", "user.email", "fixture@example.com"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").expect("write");
        std::fs::write(root.join(".gitignore"), ".env\nnode_modules/\n").expect("write");
        g(&["add", "-A"]);
        g(&["commit", "-q", "-m", "first"]);
        let repo = Repo::find(&root).expect("repo");
        (dir, repo)
    }

    fn store(dir: &Path) -> Store {
        let config = WorktreesConfig {
            port_range: (41000, 41099),
            ports: 5,
            ..WorktreesConfig::default()
        };
        Store::new(&dir.join("data"), &config)
    }

    #[test]
    fn worktree_lists_parse() {
        let text = "worktree /p/main\nHEAD abc\nbranch refs/heads/main\n\nworktree /p/wt\nHEAD def\ndetached\n\nworktree /p/bare\nbare\n";
        let l = parse_worktree_list(text);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].branch.as_deref(), Some("main"));
        assert_eq!(l[1].branch, None);
        assert_eq!(l[1].head, "def");
        assert!(l[2].bare);
    }

    #[test]
    fn names_and_ports() {
        assert!(check_name("fix-login_2.x").is_ok());
        for bad in ["", "-x", "a b", "a/b", "x.lock", "a..b", &"x".repeat(49)] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(name_from("Fix the login form!"), "fix-the-login-form");
        assert_eq!(name_from("  ** "), "");
        // Blocks skip what is taken and what is busy.
        let all = |_| true;
        assert_eq!(pick_ports((100, 199), 10, &[], all), Some(100));
        assert_eq!(
            pick_ports((100, 199), 10, &[(100, 10), (110, 10)], all),
            Some(120)
        );
        assert_eq!(pick_ports((100, 199), 10, &[(105, 10)], all), Some(115));
        assert_eq!(pick_ports((100, 199), 10, &[], |p| p != 103), Some(104));
        assert_eq!(pick_ports((100, 109), 10, &[(100, 1)], all), None);
        assert_eq!(pick_ports((65530, 65535), 6, &[], all), Some(65530));
        assert_eq!(pick_ports((1, 5), 0, &[], all), None);
    }

    #[test]
    fn forges_and_links() {
        let gh = forge_of("git@github.com:owner/repo.git");
        assert_eq!(
            gh,
            Forge::GitHub {
                web: "https://github.com/owner/repo".into()
            }
        );
        assert_eq!(forge_of("https://github.com/owner/repo"), gh);
        assert_eq!(forge_of("ssh://git@github.com:22/owner/repo.git"), gh);
        assert_eq!(
            forge_of("https://user@gitlab.example.com/group/sub/r.git"),
            Forge::GitLab {
                web: "https://gitlab.example.com/group/sub/r".into()
            }
        );
        assert_eq!(forge_of("/srv/git/r.git"), Forge::Other);
        assert_eq!(forge_of("https://example.com/r.git"), Forge::Other);
        assert_eq!(
            compare_link(&gh, "main", "termos/x").as_deref(),
            Some("https://github.com/owner/repo/compare/main...termos/x?expand=1")
        );
        assert_eq!(compare_link(&Forge::Other, "a", "b"), None);
    }

    #[test]
    fn diffs_parse_with_line_numbers() {
        let d = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n\\ No newline at end of file\n";
        let l = parse_diff(d);
        let kinds: Vec<LineKind> = l.iter().map(|x| x.kind).collect();
        use LineKind::*;
        assert_eq!(
            kinds,
            [
                Header, Header, Header, Header, Hunk, Context, Del, Add, Context, Note
            ]
        );
        assert_eq!((l[5].old, l[5].new), (Some(1), Some(1)));
        assert_eq!((l[6].old, l[6].new), (Some(2), None));
        assert_eq!((l[7].old, l[7].new), (None, Some(2)));
        assert_eq!((l[8].old, l[8].new), (Some(3), Some(3)));
        let changes = parse_changes(
            "M\0a.txt\0R090\0old.rs\0new.rs\0A\0img.png\0",
            concat!(
                "1\t1\ta.txt\0",
                "3\t0\t\0old.rs\0new.rs\0",
                "-\t-\timg.png\0"
            ),
        );
        assert_eq!(changes.len(), 3);
        assert_eq!((changes[0].added, changes[0].removed), (Some(1), Some(1)));
        assert_eq!(changes[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!((changes[1].status, changes[1].added), ('R', Some(3)));
        assert_eq!(changes[2].added, None, "binary");
    }

    #[test]
    fn project_settings_read_their_table_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(ProjectSettings::read(dir.path()).expect("none"), None);
        std::fs::write(
            dir.path().join("termos.toml"),
            "[tasks]\nbuild = \"make\"\n[worktree]\nsetup = \"npm ci\"\nservers = [\"npm run dev\", \"npm run api\"]\n",
        )
        .expect("write");
        let s = ProjectSettings::read(dir.path())
            .expect("ok")
            .expect("some");
        assert_eq!(s.setup, ["npm ci"]);
        assert_eq!(s.servers.len(), 2);
        assert_eq!(s.copy, ProjectSettings::default().copy, "default copy");
        std::fs::write(dir.path().join("termos.toml"), "[worktree]\nport = 3\n").expect("write");
        assert!(ProjectSettings::read(dir.path()).is_err());
    }

    #[test]
    fn a_worktree_is_made_reviewed_merged_and_archived() {
        let (dir, repo) = repo();
        let s = store(dir.path());
        std::fs::write(repo.root.join(".env"), "SECRET=fixture\n").expect("write");
        std::fs::create_dir_all(repo.root.join("node_modules/x")).expect("mkdir");
        std::fs::write(repo.root.join("node_modules/x/i.js"), "x").expect("write");

        let wt = s
            .create(
                &repo,
                &NewWorktree {
                    title: "Fix the login form".into(),
                    ports: true,
                    ..NewWorktree::default()
                },
            )
            .expect("create");
        assert_eq!(wt.name, "fix-the-login-form");
        assert_eq!(wt.branch, "termos/fix-the-login-form");
        assert_eq!(wt.base, "main");
        assert!(wt.path.join("a.txt").exists());
        assert_eq!(wt.ports.map(|p| p.1), Some(5));
        let env = wt.env();
        assert!(env.iter().any(|(k, _)| k == "TERMOS_PORT_4"));
        assert!(
            env.iter()
                .any(|(k, v)| k == "PORT" && v.parse::<u16>().is_ok())
        );
        // Its folder is inside the repository's folder under the root.
        assert!(wt.path.starts_with(s.root().join(repo.slug())));
        // Another gets the next free block and a numbered name.
        let second = s
            .create(
                &repo,
                &NewWorktree {
                    ports: true,
                    ..NewWorktree::default()
                },
            )
            .expect("second");
        assert_eq!(second.name, "task-1");
        let (a, b) = (wt.ports.expect("ports"), second.ports.expect("ports"));
        assert!(b.0 >= a.0 + a.1, "{a:?} {b:?}");
        assert!(
            s.create(
                &repo,
                &NewWorktree {
                    name: Some("task-1".into()),
                    ..NewWorktree::default()
                }
            )
            .is_err()
        );

        // Ignored files matching the patterns come along; nothing else.
        let copied = copy_ignored(&wt, &ProjectSettings::default().copy).expect("copy");
        assert_eq!(copied, [".env"]);
        assert!(wt.path.join(".env").exists());
        assert!(!wt.path.join("node_modules").exists());
        let copied = copy_ignored(&second, &["node_modules".to_owned()]).expect("copy");
        assert_eq!(copied, ["node_modules"]);
        assert!(second.path.join("node_modules/x/i.js").exists());

        // Found from inside, by name.
        assert_eq!(
            s.containing(&wt.path.join("sub")).map(|w| w.name),
            Some(wt.name.clone())
        );
        assert_eq!(s.get(Some(&repo), "task-1").expect("get").name, "task-1");
        assert!(matches!(
            s.get(None, "nope"),
            Err(WorktreeError::Unknown(_))
        ));

        // Changes: an edit, a new file, a commit.
        std::fs::write(wt.path.join("a.txt"), "one\nTWO\nthree\n").expect("write");
        std::fs::write(wt.path.join("new.txt"), "hello\nworld\n").expect("write");
        let c = changes(&wt).expect("changes");
        assert!(c.uncommitted);
        assert_eq!(c.ahead, 0);
        let paths: Vec<(&str, char)> = c
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status))
            .collect();
        assert_eq!(paths, [("a.txt", 'M'), ("new.txt", '?')]);
        assert_eq!((c.added(), c.removed()), (3, 1));
        let d = file_diff(&wt, &c.files[0], &c.merge_base).expect("diff");
        assert!(
            d.iter()
                .any(|l| l.kind == LineKind::Add && l.text == "TWO" && l.new == Some(2))
        );
        let d = file_diff(&wt, &c.files[1], &c.merge_base).expect("diff");
        assert_eq!(d.len(), 2);
        assert!(diff_text(&wt).expect("text").contains("+hello"));

        assert!(merge(&wt).is_err(), "not committed yet");
        let sha = commit_all(&wt, "Fix it")
            .expect("commit")
            .expect("a commit");
        assert_eq!(commit_all(&wt, "again").expect("commit"), None);
        let c = changes(&wt).expect("changes");
        assert!(!c.uncommitted);
        assert_eq!(c.ahead, 1);
        assert_eq!(c.files.len(), 2);

        // Merged into main, which is checked out and clean.
        let m = merge(&wt).expect("merge");
        assert_eq!(m, Merged::FastForward(sha.clone()));
        assert_eq!(
            git(&repo.root, &["rev-parse", "HEAD"])
                .expect("head")
                .trim(),
            sha
        );
        assert!(repo.root.join("new.txt").exists());
        assert_eq!(merge(&wt).expect("again"), Merged::UpToDate);

        // Archived: folder gone, ports back, branch kept; restored.
        let archived = s.archive(&wt, false).expect("archive");
        assert_eq!(archived.state, State::Archived);
        assert!(!wt.path.exists());
        assert!(repo.branch_exists(&wt.branch));
        assert_eq!(archived.ports, None);
        assert!(s.containing(&wt.path).is_none());
        let back = s.restore(&archived).expect("restore");
        assert!(back.active() && back.path.join("new.txt").exists() && back.ports.is_some());

        // Discarded: branch gone too, still restorable from its ref.
        std::fs::write(back.path.join("scratch.txt"), "x").expect("write");
        assert!(s.archive(&back, false).is_err(), "untracked file");
        let gone = s.discard(&back).expect("discard");
        assert_eq!(gone.state, State::Discarded);
        assert!(!repo.branch_exists(&wt.branch));
        let again = s.restore(&gone).expect("restore");
        assert!(repo.branch_exists(&wt.branch) && again.path.exists());
        let gone = s.discard(&again).expect("discard");
        s.forget(&gone).expect("forget");
        assert!(matches!(
            s.get(Some(&repo), &wt.name),
            Err(WorktreeError::Unknown(_))
        ));
        assert_eq!(
            names_in(&s.list().expect("list"), &repo),
            BTreeSet::from(["task-1".to_owned()])
        );
    }

    #[test]
    fn merges_into_a_base_not_checked_out_and_conflicts_stop() {
        let (dir, repo) = repo();
        let s = store(dir.path());
        let g = |args: &[&str]| git(&repo.root, args).expect("git");
        // The base, `dev`, is not checked out anywhere.
        g(&["branch", "dev"]);
        let one = s
            .create(
                &repo,
                &NewWorktree {
                    name: Some("one".into()),
                    from: Some("dev".into()),
                    ..NewWorktree::default()
                },
            )
            .expect("one");
        let two = s
            .create(
                &repo,
                &NewWorktree {
                    name: Some("two".into()),
                    from: Some("dev".into()),
                    ..NewWorktree::default()
                },
            )
            .expect("two");
        assert_eq!(one.base, "dev");
        std::fs::write(one.path.join("b.txt"), "b\n").expect("write");
        commit_all(&one, "b").expect("commit");
        std::fs::write(two.path.join("c.txt"), "c\n").expect("write");
        commit_all(&two, "c").expect("commit");
        assert!(matches!(merge(&one).expect("ff"), Merged::FastForward(_)));
        match merge(&two) {
            Ok(Merged::Commit(sha)) => {
                let parents = g(&["rev-list", "--parents", "-n", "1", &sha]);
                assert_eq!(parents.split_whitespace().count(), 3);
                let tree = g(&["ls-tree", "--name-only", "dev"]);
                assert!(tree.contains("b.txt") && tree.contains("c.txt"), "{tree}");
            }
            Err(WorktreeError::Invalid(e)) if e.contains("2.38") => {} // an old git
            other => panic!("{other:?}"),
        }
        // Both change the same line: a conflict, and `dev` stays.
        let three = s
            .create(
                &repo,
                &NewWorktree {
                    name: Some("three".into()),
                    from: Some("main".into()),
                    ..NewWorktree::default()
                },
            )
            .expect("three");
        let four = s
            .create(
                &repo,
                &NewWorktree {
                    name: Some("four".into()),
                    from: Some("main".into()),
                    ..NewWorktree::default()
                },
            )
            .expect("four");
        std::fs::write(three.path.join("a.txt"), "one\nTHREE\nthree\n").expect("write");
        commit_all(&three, "3").expect("commit");
        std::fs::write(four.path.join("a.txt"), "one\nFOUR\nthree\n").expect("write");
        commit_all(&four, "4").expect("commit");
        merge(&three).expect("merge three");
        let before = g(&["rev-parse", "main"]);
        match merge(&four) {
            Err(WorktreeError::Conflict { files, .. }) => assert_eq!(files, ["a.txt"]),
            other => panic!("{other:?}"),
        }
        assert_eq!(g(&["rev-parse", "main"]), before);
        assert!(
            g(&["status", "--porcelain"]).trim().is_empty(),
            "the checkout is as it was"
        );
    }
}
