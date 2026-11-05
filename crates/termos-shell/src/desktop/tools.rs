// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace tools (M13b, D-120): layout files, the drop-down terminal,
//! typing into several panes, saved commands, SSH hosts and pane
//! recordings.

use std::collections::BTreeSet;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Instant;

use termos_core::cast;
use termos_core::ids::PaneId;
use termos_core::input::{KeyEvent, decode::KeyAlternates, decode::KeyKind};
use termos_core::layouts::{self, Layout, PaneSpec, Split, WorkspaceSpec};
use termos_core::snippets::{self, Snippet, Where};
use termos_core::ssh_config;
use termos_render::grid::Rect;
use termos_term::input::{encode_key_event, encode_paste};
use termos_wm::layout::{Dir, Node};
use termos_wm::workspace::{MIN_FLOAT_H, SplitHow, Workspace};

use super::{Desktop, Pane, PaneKind, home_dir};
use crate::anim;
use crate::notify::Level;
use crate::prompt::Prompt;
use crate::snapshot::{self, ShellKind};

/// SSH hosts remembered as recent.
const RECENT_HOSTS: usize = 20;

/// A pane being recorded.
pub(super) struct Recording {
    writer: cast::Writer<BufWriter<std::fs::File>>,
    pub(super) path: PathBuf,
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// The program and arguments that run `command` with `shell` (a layout
/// pane's `exec`).
fn shell_run(shell: &str, command: &str) -> (String, Vec<String>) {
    let args = match ShellKind::of(shell) {
        ShellKind::Posix => vec!["-c".to_owned(), command.to_owned()],
        ShellKind::PowerShell => vec![
            "-NoLogo".to_owned(),
            "-Command".to_owned(),
            command.to_owned(),
        ],
        ShellKind::Cmd => vec!["/C".to_owned(), command.to_owned()],
    };
    (shell.to_owned(), args)
}

/// The first pane of a tiling subtree.
fn first_leaf(n: &Node) -> Option<PaneId> {
    match n {
        Node::Leaf(p) => Some(*p),
        Node::Split { children, .. } => children.first().and_then(|(c, _)| first_leaf(c)),
    }
}

/// A tiling tree as layout panes, each split from one made before it: the
/// order layout files open in. The tree's first pane must be in `out`.
fn tree_panes(n: &Node, out: &mut Vec<(PaneId, PaneSpec)>) {
    let Node::Split { dir, children } = n else {
        return;
    };
    let weights: Vec<f64> = children.iter().map(|(_, w)| w.max(0.0)).collect();
    let suffix = |k: usize| -> f64 { weights[k..].iter().sum() };
    for k in 1..children.len() {
        let (Some(prev), Some(leaf)) = (first_leaf(&children[k - 1].0), first_leaf(&children[k].0))
        else {
            continue;
        };
        let prev_at = out.iter().position(|(p, _)| *p == prev);
        let of = prev_at.filter(|i| *i + 1 != out.len()).map(|i| i + 1);
        let room = suffix(k - 1);
        let share = if room > 0.0 { suffix(k) / room } else { 0.5 };
        let pct = (share * 100.0).round().clamp(5.0, 95.0);
        out.push((
            leaf,
            PaneSpec {
                split: Some(if *dir == Dir::Horizontal {
                    Split::Right
                } else {
                    Split::Down
                }),
                of,
                size: Some(format!("{pct}%")),
                ..PaneSpec::default()
            },
        ));
    }
    for (c, _) in children {
        tree_panes(c, out);
    }
}

/// `path` relative to `base` where it is inside it.
fn relative(path: &Path, base: &Path) -> Option<String> {
    if path == base {
        return None;
    }
    Some(
        path.strip_prefix(base)
            .map_or_else(|_| path.display().to_string(), |r| r.display().to_string()),
    )
}

impl Desktop {
    // ----- layout files ---------------------------------------------------

    /// `layout.open NAME [DIR]`: the layout's workspaces, the first one
    /// shown.
    pub(super) fn open_layout(&mut self, arg: &str) {
        let (name, dir) = match arg.trim().split_once(char::is_whitespace) {
            Some((n, d)) => (n, Some(self.resolve_path(d.trim()))),
            None => (arg.trim(), None),
        };
        let base = dir
            .or_else(|| self.focused_cwd())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let (layout, root) = match layouts::find(name, &self.opts.paths.layouts_dir(), &base) {
            Ok(found) => found,
            Err(e) => {
                self.notify(Level::Error, e.to_string());
                return;
            }
        };
        let mut first = None;
        for spec in &layout.workspaces {
            if let Some(idx) = self.build_workspace(spec, &root, &layout.name, None) {
                first.get_or_insert(idx);
            }
        }
        if let Some(idx) = first {
            self.session.select(idx);
            self.after_focus_change();
        }
        self.layout_changed = true;
        self.dirty = true;
    }

    /// Builds a workspace from `spec` (folders relative to `root`) and
    /// adds it; `first` takes the place of its first pane (a worktree's
    /// agent). Layout files and worktree workspaces both come here, so
    /// they cannot drift apart. The new workspace's index.
    pub(super) fn build_workspace(
        &mut self,
        spec: &WorkspaceSpec,
        root: &Path,
        default_name: &str,
        first: Option<PaneId>,
    ) -> Option<usize> {
        let area = self.layout_area();
        let ws_dir = layouts::resolve_dir(root, spec.cwd.as_deref());
        let size = (area.w / 2, area.h / 2);
        let shell = self.config.shell_program(self.opts.env_shell.clone());
        let specs = if spec.pane.is_empty() {
            vec![PaneSpec::default()]
        } else {
            spec.pane.clone()
        };
        let mut ids: Vec<PaneId> = Vec::new();
        let mut ws: Option<Workspace> = None;
        let mut focus = None;
        let name = spec.name.clone().unwrap_or_else(|| default_name.to_owned());
        for (i, p) in specs.iter().enumerate() {
            let dir = layouts::resolve_dir(&ws_dir, p.cwd.as_deref());
            let made = match (i, first) {
                (0, Some(id)) => Ok(id),
                _ if p.exec => {
                    let command = p.command.clone().unwrap_or_default();
                    self.spawn_pane(Some(dir), size, Some(shell_run(&shell, &command)))
                }
                _ => self.spawn_term(Some(dir), size),
            };
            let id = match made {
                Ok(id) => id,
                Err(e) => {
                    self.notify(Level::Error, format!("cannot start pane {}: {e}", i + 1));
                    continue;
                }
            };
            match ws.as_mut() {
                None => ws = Some(Workspace::new(name.clone(), id)),
                Some(w) => {
                    let anchor =
                        p.of.and_then(|n| ids.get(n.saturating_sub(1)))
                            .or(ids.last())
                            .copied();
                    if let Some(a) = anchor {
                        w.set_focus(a);
                    }
                    let placed = match p.split {
                        Some(Split::Float) => {
                            w.add_float(id, area);
                            true
                        }
                        Some(Split::Right) => w.add_split(id, SplitHow::Right, area),
                        Some(Split::Down) => w.add_split(id, SplitHow::Down, area),
                        None => w.add_split(id, SplitHow::Auto, area),
                    };
                    if !placed {
                        self.close_pane(id);
                        let what = p
                            .command
                            .as_deref()
                            .map_or_else(|| format!("pane {}", i + 1), |c| format!("`{c}`"));
                        self.notify(
                            Level::Warn,
                            format!("no room for {what}; start it yourself"),
                        );
                        continue;
                    }
                    if p.split != Some(Split::Float)
                        && let Some(f) = p.size.as_deref().and_then(layouts::parse_size)
                    {
                        w.layout.set_share(id, f);
                    }
                }
            }
            if let Some(n) = &p.name
                && let Some(pane) = self.panes.get_mut(&id)
            {
                pane.name = Some(n.clone());
            }
            if !p.exec
                && let Some(cmd) = p.command.as_deref().filter(|c| !c.trim().is_empty())
            {
                // Typed into the shell, so it can be run again after it stops.
                let _ = self.send_text(Some(id), &format!("{cmd}\r"));
            }
            if p.focus {
                focus = Some(id);
            }
            ids.push(id);
        }
        let mut ws = ws?;
        ws.auto_name = false;
        ws.set_focus(focus.or_else(|| ids.first().copied())?);
        Some(self.session.add(ws))
    }

    /// The active workspace as a layout: its tree, folders, names and the
    /// programs on `restore-programs`; what was not saved, for a comment.
    fn workspace_spec(&self) -> (WorkspaceSpec, Vec<String>) {
        let ws = self.session.active();
        let mut order: Vec<(PaneId, PaneSpec)> = Vec::new();
        if let Some(root) = ws.layout.root() {
            if let Some(first) = first_leaf(root) {
                order.push((first, PaneSpec::default()));
            }
            tree_panes(root, &mut order);
        }
        for f in &ws.floats {
            order.push((
                f.pane,
                PaneSpec {
                    split: Some(Split::Float),
                    ..PaneSpec::default()
                },
            ));
        }
        let cfg = &self.config.session;
        let shell = self.config.shell_program(self.opts.env_shell.clone());
        let shell_name = snapshot::program_name(&shell).to_owned();
        let kind = ShellKind::of(&shell);
        let dir_of = |id: PaneId| -> Option<PathBuf> {
            let p = self.panes.get(&id)?;
            match &p.kind {
                PaneKind::Term { pty, .. } => pty.foreground_cwd().or_else(|| p.cwd.clone()),
                PaneKind::App(_) => None,
            }
        };
        let base = order.iter().find_map(|(id, _)| dir_of(*id));
        let mut skipped = Vec::new();
        let n = order.len();
        let panes = order
            .into_iter()
            .enumerate()
            .map(|(i, (id, mut spec))| {
                if let (Some(dir), Some(base)) = (dir_of(id), base.as_ref()) {
                    spec.cwd = relative(&dir, base);
                }
                spec.focus = ws.focus() == Some(id) && n > 1;
                if let Some(p) = self.panes.get(&id) {
                    spec.name.clone_from(&p.name);
                    match &p.kind {
                        PaneKind::Term { pty, .. } => {
                            if let Some(argv) = pty.foreground_command()
                                && let Some(prog) = argv.first().map(|a| snapshot::program_name(a))
                                && prog != shell_name
                            {
                                match snapshot::allowed(&argv, &cfg.restore_programs)
                                    .then(|| snapshot::command_line(&argv, kind))
                                    .flatten()
                                {
                                    Some(line) => spec.command = Some(line),
                                    None => skipped.push(format!(
                                        "pane {} ran {prog} (not on [session] restore-programs)",
                                        i + 1
                                    )),
                                }
                            }
                        }
                        PaneKind::App(app) => skipped.push(format!(
                            "pane {} is the app window {} (a shell here)",
                            i + 1,
                            app.title()
                        )),
                    }
                }
                spec
            })
            .collect();
        (
            WorkspaceSpec {
                name: Some(ws.name.clone()),
                cwd: base.map(|b| b.display().to_string()),
                pane: panes,
            },
            skipped,
        )
    }

    /// `layout.save NAME`: the active workspace into `layouts/NAME.toml`.
    pub(super) fn save_layout(&mut self, name: &str) {
        if !layouts::valid_name(name) {
            self.notify(Level::Warn, format!("`{name}` is not a layout name"));
            return;
        }
        let (spec, skipped) = self.workspace_spec();
        let layout = Layout {
            name: name.to_owned(),
            workspaces: vec![spec],
        };
        let mut text = format!(
            "# TermOS layout `{name}`, saved from workspace {}.\n# Open it with `layout.open {name}` (the palette) or `termos layout open {name}`.\n",
            self.session.active().name
        );
        for s in &skipped {
            text.push_str(&format!("# Not saved: {s}.\n"));
        }
        text.push_str(&layout.to_toml());
        let dir = self.opts.paths.layouts_dir();
        let file = dir.join(format!("{name}.toml"));
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&file, text));
        match written {
            Ok(()) => self.notify(
                Level::Info,
                format!("saved the layout {name} ({})", file.display()),
            ),
            Err(e) => self.notify(Level::Error, format!("cannot save {}: {e}", file.display())),
        }
    }

    /// The layouts `layout.open` finds from the focused pane's folder.
    pub(super) fn layout_names(&self) -> Vec<String> {
        let dir = self
            .focused_cwd()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        layouts::names(&self.opts.paths.layouts_dir(), &dir)
    }

    // ----- the drop-down terminal ----------------------------------------

    /// Where the drop-down terminal goes: full width, from the top.
    fn quick_rect(&self) -> Rect {
        let area = self.layout_area();
        let h = (u32::from(area.h) * u32::from(self.config.quick.height) / 100)
            .try_into()
            .unwrap_or(area.h)
            .clamp(MIN_FLOAT_H.min(area.h), area.h);
        Rect::new(area.x, area.y, area.w, h)
    }

    /// Whether `id` is the drop-down terminal.
    pub(super) fn is_quick(&self, id: PaneId) -> bool {
        self.quick == Some(id)
    }

    /// `quick.toggle`: shows the drop-down terminal over the active
    /// workspace (bringing it from another, or starting it), or hides it
    /// and gives the focus back.
    pub(super) fn toggle_quick(&mut self) {
        let area = self.layout_area();
        let rect = self.quick_rect();
        let now = Instant::now();
        if let Some(id) = self.quick.filter(|id| self.panes.contains_key(id)) {
            let active = self.session.active_index();
            let here = self.session.find(id);
            let minimized = self.session.active().minimized().contains(&id);
            if here == Some(active) && !minimized {
                self.session.active_mut().toggle_minimize(id);
                if let Some(back) = self.quick_back.take()
                    && self.session.active().contains(back)
                {
                    self.session.active_mut().set_focus(back);
                }
                self.after_focus_change();
                return;
            }
            self.quick_back = self.focused().filter(|f| *f != id);
            if here == Some(active) {
                self.session.active_mut().toggle_minimize(id);
            } else {
                if let Some(i) = here {
                    self.session.workspaces[i].remove(id);
                    // A workspace it was alone in goes.
                    if self.session.workspaces[i].is_empty() {
                        self.session.remove(i);
                        self.workspace_removed(i);
                    }
                }
                self.session.active_mut().add_float(id, area);
            }
            let ws = self.session.active_mut();
            ws.set_float_rect(id, rect, area);
            ws.set_focus(id);
            self.animate(anim::Kind::FloatOpen(id), now);
            self.after_focus_change();
            self.layout_changed = true;
            return;
        }
        let cwd = self.focused_cwd();
        let inner = (rect.w.saturating_sub(2), rect.h.saturating_sub(2));
        match self.spawn_term(cwd, inner) {
            Ok(id) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.name = Some("quick".into());
                }
                self.quick_back = self.focused();
                self.quick = Some(id);
                let ws = self.session.active_mut();
                ws.add_float(id, area);
                ws.set_float_rect(id, rect, area);
                self.animate(anim::Kind::FloatOpen(id), now);
                self.after_focus_change();
                self.layout_changed = true;
            }
            Err(e) => self.notify(
                Level::Error,
                format!("cannot start the drop-down terminal: {e}"),
            ),
        }
    }

    // ----- typing into several panes ---------------------------------------

    fn is_term(&self, id: PaneId) -> bool {
        matches!(
            self.panes.get(&id).map(|p| &p.kind),
            Some(PaneKind::Term { .. })
        )
    }

    /// `workspace.sync`: keys go to every terminal pane of the workspace,
    /// or, when they already go to several, to one again.
    pub(super) fn toggle_workspace_sync(&mut self) {
        self.dirty = true;
        if !self.sync.is_empty() {
            self.sync.clear();
            self.notify(Level::Info, "typing goes to one pane again");
            return;
        }
        let panes: BTreeSet<PaneId> = self
            .session
            .active()
            .panes()
            .into_iter()
            .filter(|p| self.is_term(*p))
            .collect();
        if panes.len() < 2 {
            self.notify(Level::Info, "this workspace has one terminal pane");
            return;
        }
        let n = panes.len();
        self.sync = panes;
        self.notify(
            Level::Info,
            format!("typing into {n} panes (prefix S again stops)"),
        );
    }

    /// `pane.sync`: the focused pane joins the panes typed into at once,
    /// or leaves them.
    pub(super) fn toggle_pane_sync(&mut self) {
        let Some(id) = self.focused().filter(|id| self.is_term(*id)) else {
            self.notify(Level::Info, "only terminal panes are typed into");
            return;
        };
        if !self.sync.remove(&id) {
            self.sync.insert(id);
        }
        let n = self.sync.len();
        self.notify(
            Level::Info,
            match n {
                0 => "typing goes to one pane again".to_owned(),
                1 => "this pane is ready: add others with pane.sync".to_owned(),
                n => format!("typing into {n} panes"),
            },
        );
        self.dirty = true;
    }

    /// The other panes what is typed into `id` also goes to.
    fn sync_targets(&self, id: PaneId) -> Vec<PaneId> {
        if !self.sync.contains(&id) {
            return Vec::new();
        }
        self.sync
            .iter()
            .copied()
            .filter(|p| *p != id && self.is_term(*p))
            .collect()
    }

    /// A key typed into `id`, sent to the panes synced with it, each in
    /// its program's encoding.
    pub(super) fn sync_key(
        &mut self,
        id: PaneId,
        k: KeyEvent,
        kind: KeyKind,
        alternates: &KeyAlternates,
    ) {
        for other in self.sync_targets(id) {
            let bytes = match self.panes.get(&other) {
                Some(Pane {
                    kind: PaneKind::Term { vt, .. },
                    ..
                }) => encode_key_event(k, kind, alternates, &vt.modes()),
                _ => continue,
            };
            if !bytes.is_empty() {
                self.write_to_pane(other, bytes);
            }
        }
    }

    /// A paste into `id`, for the panes synced with it.
    pub(super) fn sync_paste(&mut self, id: PaneId, text: &str) {
        for other in self.sync_targets(id) {
            let bytes = match self.panes.get(&other) {
                Some(Pane {
                    kind: PaneKind::Term { vt, .. },
                    ..
                }) => encode_paste(text, &vt.modes()),
                _ => continue,
            };
            self.write_to_pane(other, bytes);
        }
    }

    // ----- saved commands --------------------------------------------------

    /// The config's snippets, then the focused pane's project's.
    pub(super) fn all_snippets(&self) -> Vec<Snippet> {
        let mut all = self.config.snippets.clone();
        if let Some(dir) = self.focused_cwd() {
            for s in snippets::project_snippets(&dir) {
                if !all.iter().any(|a| a.name == s.name) {
                    all.push(s);
                }
            }
        }
        all
    }

    /// `snippet.run NAME`: asks for its parameters, then types it.
    pub(super) fn run_snippet(&mut self, name: &str) {
        match self.all_snippets().into_iter().find(|s| s.name == name) {
            Some(s) => self.ask_snippet(s, Vec::new()),
            None => self.notify(Level::Warn, format!("no saved command named {name}")),
        }
    }

    /// Asks for the next parameter not given yet, or types the command.
    fn ask_snippet(&mut self, s: Snippet, values: Vec<(String, String)>) {
        let next = snippets::params(&s.command)
            .into_iter()
            .find(|p| !values.iter().any(|(n, _)| *n == p.name));
        match next {
            Some(p) => {
                self.ui.prompt = Some(Prompt::snippet_param(s, values, &p));
                self.dirty = true;
            }
            None => self.type_snippet(&s, &values),
        }
    }

    /// The answer to a snippet's parameter prompt.
    pub(super) fn snippet_answer(
        &mut self,
        s: Snippet,
        mut values: Vec<(String, String)>,
        text: String,
    ) {
        if let Some(p) = snippets::params(&s.command)
            .into_iter()
            .find(|p| !values.iter().any(|(n, _)| *n == p.name))
        {
            values.push((p.name, text));
        }
        self.ask_snippet(s, values);
    }

    /// Types the filled-in command where the snippet says, pasted (so a
    /// shell takes it as text), and presses Enter if it says `run`.
    fn type_snippet(&mut self, s: &Snippet, values: &[(String, String)]) {
        let line = snippets::fill(&s.command, values);
        let area = self.layout_area();
        let target = match s.place {
            Where::Pane => self.focused().filter(|id| self.is_term(*id)),
            Where::NewPane | Where::Float => {
                let cwd = self.focused_cwd();
                match self.spawn_term(cwd, (area.w / 2, area.h / 2)) {
                    Ok(id) => {
                        let placed = if s.place == Where::Float {
                            self.session.active_mut().add_float(id, area);
                            true
                        } else {
                            self.session
                                .active_mut()
                                .add_split(id, SplitHow::Auto, area)
                        };
                        if placed {
                            self.layout_changed = true;
                            Some(id)
                        } else {
                            self.close_pane(id);
                            None
                        }
                    }
                    Err(e) => {
                        self.notify(Level::Error, format!("cannot start a shell: {e}"));
                        return;
                    }
                }
            }
        };
        let Some(id) = target else {
            self.notify(Level::Warn, "no terminal pane to type the command into");
            return;
        };
        self.paste_into(id, &line);
        if s.run {
            self.write_to_pane(id, b"\r".to_vec());
        }
        self.dirty = true;
    }

    // ----- SSH hosts -------------------------------------------------------

    /// The hosts of `~/.ssh/config`, the ones opened lately first.
    pub(super) fn ssh_hosts(&self) -> Vec<String> {
        let recent: Vec<String> = std::fs::read_to_string(self.opts.paths.ssh_recent_file())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        let hosts = ssh_config::user_hosts()
            .into_iter()
            .filter(|h| ssh_config::valid_host(h))
            .collect();
        ssh_config::recent_first(hosts, &recent)
    }

    /// `ssh.open HOST`: a new pane running the user's own `ssh HOST`.
    pub(super) fn open_ssh(&mut self, host: &str) {
        if !ssh_config::valid_host(host) {
            self.notify(Level::Warn, format!("`{host}` is not a host"));
            return;
        }
        let area = self.layout_area();
        let cwd = self.focused_cwd();
        match self.spawn_pane(
            cwd,
            (area.w / 2, area.h),
            Some(("ssh".to_owned(), vec![host.to_owned()])),
        ) {
            Ok(id) => {
                if self
                    .session
                    .active_mut()
                    .add_split(id, SplitHow::Auto, area)
                {
                    if let Some(p) = self.panes.get_mut(&id) {
                        p.name = Some(host.to_owned());
                    }
                    self.remember_host(host);
                    self.layout_changed = true;
                } else {
                    self.close_pane(id);
                    self.notify(Level::Warn, "no space for new pane");
                }
            }
            Err(e) => self.notify(Level::Error, format!("cannot start ssh: {e}")),
        }
    }

    fn remember_host(&self, host: &str) {
        let file = self.opts.paths.ssh_recent_file();
        let mut recent: Vec<String> = std::fs::read_to_string(&file)
            .unwrap_or_default()
            .lines()
            .filter(|h| *h != host && ssh_config::valid_host(h))
            .map(str::to_owned)
            .collect();
        recent.insert(0, host.to_owned());
        recent.truncate(RECENT_HOSTS);
        let written = termos_core::paths::ensure_private_dir(&self.opts.paths.state_dir)
            .and_then(|()| std::fs::write(&file, recent.join("\n") + "\n"));
        if let Err(e) = written {
            tracing::warn!("cannot remember the SSH host: {e}");
        }
    }

    // ----- recording a pane ------------------------------------------------

    /// Where recordings go: `[record] dir` (`~` is the home folder,
    /// relative to the config's folder), else the state folder's.
    fn recordings_dir(&self) -> PathBuf {
        match &self.config.record.dir {
            Some(d) => {
                let text = d.to_string_lossy();
                match text.strip_prefix('~') {
                    Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => home_dir()
                        .map_or_else(
                            || d.clone(),
                            |h| h.join(rest.trim_start_matches(['/', '\\'])),
                        ),
                    _ => self.config_relative(d),
                }
            }
            None => self.opts.paths.recordings_dir(),
        }
    }

    /// Whether `id` is being recorded.
    pub(super) fn is_recording(&self, id: PaneId) -> bool {
        self.recordings.contains_key(&id)
    }

    /// `pane.record`: starts recording the focused pane's output, or
    /// stops.
    pub(super) fn toggle_record(&mut self) {
        let Some(id) = self.focused() else {
            return;
        };
        if self.recordings.contains_key(&id) {
            self.stop_recording(id);
            return;
        }
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let PaneKind::Term { vt, .. } = &pane.kind else {
            self.notify(Level::Info, "only terminal panes are recorded");
            return;
        };
        let (width, height) = vt.size();
        let title = pane.display_name();
        let slug: String = title
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .take(40)
            .collect();
        let dir = self.recordings_dir();
        let stamp = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
        let path = dir.join(format!("{slug}-{stamp}.cast"));
        let opened = termos_core::paths::ensure_private_dir(&dir).and_then(|()| {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                o.mode(0o600);
            }
            o.open(&path)
        });
        let header = cast::Header {
            version: 2,
            width,
            height,
            timestamp: Some(snapshot::now_secs()),
            title: Some(title),
        };
        let writer = opened.and_then(|f| cast::Writer::new(BufWriter::new(f), &header));
        match writer {
            Ok(writer) => {
                self.recordings.insert(
                    id,
                    Recording {
                        writer,
                        path: path.clone(),
                    },
                );
                self.notify(
                    Level::Info,
                    format!("recording to {} (prefix R stops)", path.display()),
                );
            }
            Err(e) => self.notify(
                Level::Error,
                format!("cannot record to {}: {e}", path.display()),
            ),
        }
        self.dirty = true;
    }

    /// Output of a pane being recorded.
    pub(super) fn record_output(&mut self, id: PaneId, bytes: &[u8]) {
        let Some(r) = self.recordings.get_mut(&id) else {
            return;
        };
        if let Err(e) = r.writer.output(bytes) {
            tracing::warn!("recording pane {id}: {e}");
            self.stop_recording(id);
        }
    }

    /// A pane being recorded changed size.
    pub(super) fn record_resize(&mut self, id: PaneId, cols: u16, rows: u16) {
        if let Some(r) = self.recordings.get_mut(&id)
            && let Err(e) = r.writer.resize(cols, rows)
        {
            tracing::warn!("recording pane {id}: {e}");
        }
    }

    /// Ends a recording, saying where it is.
    pub(super) fn stop_recording(&mut self, id: PaneId) {
        let Some(r) = self.recordings.remove(&id) else {
            return;
        };
        let path = r.path.clone();
        match r.writer.finish() {
            Ok(_) => self.notify(
                Level::Info,
                format!("saved the recording: {}", path.display()),
            ),
            Err(e) => self.notify(
                Level::Error,
                format!("the recording {}: {e}", path.display()),
            ),
        }
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_wm::layout::Layout as Tree;

    #[test]
    fn a_tree_becomes_panes_split_one_from_another() {
        // [1 | [2 / 3]], the right column 40% wide.
        let mut t = Tree::single(PaneId(1));
        t.split(Some(PaneId(1)), PaneId(2), Dir::Horizontal);
        assert!(t.set_share(PaneId(2), 0.4));
        t.split(Some(PaneId(2)), PaneId(3), Dir::Vertical);
        let root = t.root().expect("a tree");
        let mut out = vec![(PaneId(1), PaneSpec::default())];
        tree_panes(root, &mut out);
        type Row = (u64, Option<Split>, Option<usize>, Option<String>);
        let got: Vec<Row> = out
            .iter()
            .map(|(p, s)| (p.0, s.split, s.of, s.size.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, None, None, None),
                (2, Some(Split::Right), None, Some("40%".into())),
                (3, Some(Split::Down), None, Some("50%".into())),
            ]
        );
    }

    #[test]
    fn exec_runs_through_the_shell() {
        assert_eq!(
            shell_run("/bin/bash", "make test"),
            (
                "/bin/bash".to_owned(),
                vec!["-c".to_owned(), "make test".to_owned()]
            )
        );
        assert_eq!(shell_run("pwsh", "ls").1[1], "-Command");
        assert_eq!(shell_run("cmd.exe", "dir").1[0], "/C");
        assert_eq!(
            relative(Path::new("/p/srv"), Path::new("/p")).as_deref(),
            Some("srv")
        );
        assert_eq!(relative(Path::new("/p"), Path::new("/p")), None);
    }
}
