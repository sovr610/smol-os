// SPDX-License-Identifier: GPL-3.0-or-later
//! Named desktop commands, shared by key bindings, the command palette and
//! (from M3) the JSON-RPC control API.
//!
//! Every command has a canonical text form, `name [argument]`, e.g.
//! `pane.split-right`, `pane.resize-left 5`, `workspace.select 2`,
//! `layout.select tiled`.

use std::fmt;
use std::str::FromStr;

use termos_render::theme::ThemeBase;
use termos_wm::layout::{Direction, Preset};
use termos_wm::workspace::{Snap, SplitHow};

/// Something the desktop can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Split(SplitHow),
    NewFloat,
    KillPane,
    Focus(Direction),
    FocusNext,
    FocusPrev,
    FocusLast,
    SwapNext,
    SwapPrev,
    Resize(Direction, u16),
    Zoom,
    ToggleFloat,
    Minimize,
    RestoreMinimized,
    SnapFloat(Snap),
    /// Name the focused pane (a prompt).
    PaneRename,
    /// Maximize the focused floating window, or give it its size back (a
    /// tiled pane zooms).
    MaximizeToggle,
    LayoutNext,
    LayoutSelect(Preset),
    WorkspaceNew,
    WorkspaceNext,
    WorkspacePrev,
    WorkspaceLast,
    WorkspaceSelect(usize),
    WorkspaceRename,
    WorkspaceKill,
    CopyMode,
    CopyModePageUp,
    Paste,
    Palette,
    /// The TermOS menu (D-085).
    Menu,
    Keys,
    Notifications,
    Tutorial,
    /// About TermOS (D-089).
    AboutOpen,
    ReloadConfig,
    SendPrefix,
    Quit,
    /// Detach this client; the session keeps running.
    Detach,
    SessionNew,
    SessionNext,
    SessionPrev,
    SessionSwitch(String),
    SessionRename,
    /// Restore saved sessions that are not running.
    SessionRestore,
    /// Switch this client's theme (all clients' from the control API).
    ThemeSelect(ThemeBase),
    /// Open an image in the viewer (a path, which may contain spaces).
    ImageOpen(String),
    /// Open a markdown document in the viewer.
    MarkdownOpen(String),
    /// Open the file manager in a folder (empty: the focused pane's).
    FilesOpen(String),
    /// Open a file with what suits it: folders in the file manager,
    /// pictures and markdown in their viewers, the rest in the editor.
    FileOpen(String),
    /// Edit a file with `$VISUAL` or `$EDITOR` in a new pane.
    FileEdit(String),
    MonitorOpen,
    NetworkOpen,
    /// A window running `termos update install` (D-077).
    UpdateOpen,
    SettingsOpen,
    /// Run a project task (build, run, test, or one from termos.toml) in
    /// a panel.
    Task(String),
    /// The Git UI (lazygit, D-053).
    GitOpen,
    /// The clipboard history (D-050).
    ClipboardOpen,
    /// Forget the clipboard history (pinned entries stay).
    ClipboardClear,
    /// A new agent window (a profile name, or empty: the default).
    AgentOpen(String),
    /// Resume a saved agent conversation (empty: pick one).
    AgentResume(String),
    /// An external CLI agent (Claude Code, Codex…) in a terminal window.
    AgentExternal(String),
    /// Install Claude Code or Codex CLI (`termos agent install`, D-094).
    AgentInstall(String),
    /// Update the CLI agents TermOS installed.
    AgentUpdate,
    /// Make an account the one a CLI uses: "claude work" (D-097).
    AccountUse(String),
    /// An account task in a window of its own (`termos account …`):
    /// "add claude", "sign-in claude work", "budget claude work",
    /// "statusline claude on" (D-097).
    AccountTask(String),
    /// The usage app (D-097).
    UsageOpen,
    /// The coding harness, planning a goal if given (D-100).
    HarnessOpen(String),
    /// Continue the focused agent window's conversation as another
    /// account (D-097).
    AgentSwap(String),
    /// The agents' audit logs (empty: the newest).
    AuditOpen(String),
    /// The command center (D-093).
    CenterOpen,
}

/// Static description of a command name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    /// Example argument shown in the palette, if the command takes one.
    pub arg: Option<&'static str>,
}

const fn info(name: &'static str, description: &'static str) -> CommandInfo {
    CommandInfo {
        name,
        description,
        arg: None,
    }
}

/// Every command name, in palette order.
pub const COMMANDS: &[CommandInfo] = &[
    info("pane.split-right", "Split: new pane to the right"),
    info("pane.split-down", "Split: new pane below"),
    info("pane.split-auto", "Split along the longer side (BSP)"),
    info("pane.new-float", "New floating shell window"),
    info("pane.kill", "Close the focused pane"),
    info("pane.focus-left", "Focus the pane to the left"),
    info("pane.focus-right", "Focus the pane to the right"),
    info("pane.focus-up", "Focus the pane above"),
    info("pane.focus-down", "Focus the pane below"),
    info("pane.next", "Focus the next pane"),
    info("pane.prev", "Focus the previous pane"),
    info("pane.last", "Focus the previously focused pane"),
    info("pane.swap-next", "Swap with the next pane"),
    info("pane.swap-prev", "Swap with the previous pane"),
    CommandInfo {
        name: "pane.resize-left",
        description: "Move the border left",
        arg: Some("1"),
    },
    CommandInfo {
        name: "pane.resize-right",
        description: "Move the border right",
        arg: Some("1"),
    },
    CommandInfo {
        name: "pane.resize-up",
        description: "Move the border up",
        arg: Some("1"),
    },
    CommandInfo {
        name: "pane.resize-down",
        description: "Move the border down",
        arg: Some("1"),
    },
    info("pane.zoom", "Zoom (maximize) the pane, or unzoom"),
    info("pane.float", "Float the pane, or tile it again"),
    info("pane.minimize", "Minimize the floating window"),
    info("pane.restore", "Restore the last minimized window"),
    info(
        "pane.snap-left",
        "Snap the floating window to the left half",
    ),
    info(
        "pane.snap-right",
        "Snap the floating window to the right half",
    ),
    info("pane.snap-top", "Snap the floating window to the top half"),
    info(
        "pane.snap-bottom",
        "Snap the floating window to the bottom half",
    ),
    info("pane.snap-max", "Maximize the floating window"),
    info("pane.snap-center", "Center the floating window"),
    info("layout.next", "Next layout preset"),
    CommandInfo {
        name: "layout.select",
        description: "Layout preset",
        arg: Some("tiled"),
    },
    info("workspace.new", "New workspace"),
    info("workspace.next", "Next workspace"),
    info("workspace.prev", "Previous workspace"),
    info("workspace.last", "Previously used workspace"),
    CommandInfo {
        name: "workspace.select",
        description: "Go to workspace",
        arg: Some("0"),
    },
    info("pane.rename", "Name this pane"),
    info(
        "pane.maximize",
        "Maximize the floating window, or restore it",
    ),
    info("workspace.rename", "Rename the workspace"),
    info("workspace.kill", "Close the workspace and its panes"),
    info("copy-mode", "Scroll back, select and copy (vi keys)"),
    info("copy-mode.page-up", "Copy mode, one page up"),
    info("paste", "Paste the copy buffer"),
    info("palette", "Command palette"),
    info("menu", "The TermOS menu"),
    info("keys", "Show all key bindings"),
    info("notifications", "Notification center"),
    info("tutorial", "Open the tutorial"),
    info("about.open", "About TermOS: version, files, licence"),
    info("config.reload", "Reload config.toml"),
    info("send-prefix", "Send the prefix key to the pane"),
    info("session.quit", "Close this session and all its panes"),
    info("client.detach", "Detach (the session keeps running)"),
    info("session.new", "New session"),
    info("session.next", "Next session"),
    info("session.prev", "Previous session"),
    CommandInfo {
        name: "session.switch",
        description: "Switch to session",
        arg: Some("main"),
    },
    info("session.rename", "Rename this session"),
    info("session.restore", "Restore saved sessions"),
    CommandInfo {
        name: "theme.select",
        description: "Theme",
        arg: Some("auto"),
    },
    CommandInfo {
        name: "image.open",
        description: "Open an image",
        arg: Some("photo.png"),
    },
    CommandInfo {
        name: "markdown.open",
        description: "Open a markdown document",
        arg: Some("README.md"),
    },
    info("files.open", "Files: browse this folder"),
    info("monitor.open", "System monitor"),
    info("task.build", "Build this project"),
    info("task.run", "Run this project"),
    info("task.test", "Test this project"),
    CommandInfo {
        name: "task",
        description: "Run a project task",
        arg: Some("lint"),
    },
    info("git.open", "Git (lazygit)"),
    info("network.open", "Network: connections and Wi-Fi"),
    info("update.open", "Updates: install a newer TermOS"),
    info("settings.open", "Settings"),
    info("clipboard.open", "Clipboard history"),
    info("clipboard.clear", "Forget the clipboard history"),
    info("agent.open", "AI agent: a new conversation"),
    CommandInfo {
        name: "agent.open",
        description: "AI agent with a profile",
        arg: Some("default"),
    },
    info("agent.resume", "AI agent: resume a conversation"),
    CommandInfo {
        name: "agent.external",
        description: "External agent (Claude Code, Codex…)",
        arg: Some("claude"),
    },
    CommandInfo {
        name: "agent.install",
        description: "Install Claude Code or Codex CLI (checked download)",
        arg: Some("claude"),
    },
    info("agent.update", "Update Claude Code and Codex CLI"),
    CommandInfo {
        name: "account.use",
        description: "Use another Claude Code or Codex account",
        arg: Some("claude work"),
    },
    info(
        "usage.open",
        "Usage and accounts: every Claude Code and Codex account",
    ),
    info(
        "harness.open",
        "Harness: plan a goal over Claude Code, Codex and API models",
    ),
    CommandInfo {
        name: "harness.open",
        description: "Harness: plan this goal",
        arg: Some("add a --verbose flag"),
    },
    CommandInfo {
        name: "account.add",
        description: "Add a Claude Code or Codex account",
        arg: Some("claude"),
    },
    CommandInfo {
        name: "account.sign-in",
        description: "Sign in to an account",
        arg: Some("claude work"),
    },
    CommandInfo {
        name: "account.budget",
        description: "Token budgets for an account (shown in the usage app)",
        arg: Some("claude work"),
    },
    CommandInfo {
        name: "account.statusline",
        description: "Claude Code's plan limits in the usage app (on or off)",
        arg: Some("claude on"),
    },
    CommandInfo {
        name: "agent.swap",
        description: "Continue this agent window as another account",
        arg: Some("work"),
    },
    info("audit.open", "AI agent audit logs"),
    info(
        "center.open",
        "The command center: AI and system, live and over time",
    ),
    CommandInfo {
        name: "file.open",
        description: "Open a file",
        arg: Some("notes.txt"),
    },
    CommandInfo {
        name: "file.edit",
        description: "Edit a file",
        arg: Some("notes.txt"),
    },
];

/// Commands whose argument is the rest of the line: a path, spaces and
/// all. The flag says whether the path may be left out.
const PATH_COMMANDS: [(&str, bool); 5] = [
    ("image.open", false),
    ("markdown.open", false),
    ("files.open", true),
    ("file.open", false),
    ("file.edit", false),
];

fn path_command(name: &str, path: String) -> Option<Command> {
    Some(match name {
        "image.open" => Command::ImageOpen(path),
        "markdown.open" => Command::MarkdownOpen(path),
        "files.open" => Command::FilesOpen(path),
        "file.open" => Command::FileOpen(path),
        "file.edit" => Command::FileEdit(path),
        _ => return None,
    })
}

/// Error for an unknown or malformed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError(pub String);

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CommandError {}

fn snap_name(s: Snap) -> &'static str {
    match s {
        Snap::LeftHalf => "left",
        Snap::RightHalf => "right",
        Snap::TopHalf => "top",
        Snap::BottomHalf => "bottom",
        Snap::Maximize => "max",
        Snap::Center => "center",
    }
}

fn dir_name(d: Direction) -> &'static str {
    match d {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

impl FromStr for Command {
    type Err = CommandError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Account commands take the CLI and more words (D-097).
        for (name, words, example) in [
            ("account.use", 2, "claude work"),
            ("account.add", 1, "claude"),
            ("account.sign-in", 2, "claude work"),
            ("account.budget", 2, "claude work"),
            ("account.statusline", 2, "claude on"),
        ] {
            if let Some(rest) = s.trim_start().strip_prefix(name)
                && (rest.is_empty() || rest.starts_with(char::is_whitespace))
            {
                let given: Vec<&str> = rest.split_whitespace().collect();
                if given.len() != words {
                    return Err(CommandError(format!("`{name}` needs {example}")));
                }
                let arg = given.join(" ");
                return Ok(match name {
                    "account.use" => Command::AccountUse(arg),
                    _ => Command::AccountTask(format!(
                        "{} {arg}",
                        name.trim_start_matches("account.")
                    )),
                });
            }
        }
        // A goal is the rest of the line too (D-100).
        if let Some(rest) = s.trim_start().strip_prefix("harness.open")
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            return Ok(Command::HarnessOpen(rest.trim().to_owned()));
        }
        // A path is the rest of the line, spaces and all.
        for (name, optional) in PATH_COMMANDS {
            if let Some(rest) = s.trim_start().strip_prefix(name)
                && (rest.is_empty() || rest.starts_with(char::is_whitespace))
            {
                let path = rest.trim();
                if path.is_empty() && !optional {
                    return Err(CommandError(format!("`{name}` needs a file")));
                }
                return path_command(name, path.to_owned())
                    .ok_or_else(|| CommandError(format!("`{name}`: unknown command")));
            }
        }
        let mut parts = s.split_whitespace();
        let name = parts.next().unwrap_or("");
        let arg = parts.next();
        if parts.next().is_some() {
            return Err(CommandError(format!("`{s}`: too many arguments")));
        }
        let no_arg = |c: Command| -> Result<Command, CommandError> {
            match arg {
                None => Ok(c),
                Some(_) => Err(CommandError(format!("`{name}` takes no argument"))),
            }
        };
        let cells = || -> Result<u16, CommandError> {
            arg.map_or(Ok(1), |a| {
                a.parse::<u16>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| CommandError(format!("`{name}`: `{a}` is not a cell count")))
            })
        };
        let dir = |d: &str| match d {
            "left" => Some(Direction::Left),
            "right" => Some(Direction::Right),
            "up" => Some(Direction::Up),
            "down" => Some(Direction::Down),
            _ => None,
        };
        if let Some(d) = name.strip_prefix("pane.focus-").and_then(dir) {
            return no_arg(Command::Focus(d));
        }
        if let Some(d) = name.strip_prefix("pane.resize-").and_then(dir) {
            return Ok(Command::Resize(d, cells()?));
        }
        if let Some(snap) = name.strip_prefix("pane.snap-") {
            let snap = match snap {
                "left" => Snap::LeftHalf,
                "right" => Snap::RightHalf,
                "top" => Snap::TopHalf,
                "bottom" => Snap::BottomHalf,
                "max" => Snap::Maximize,
                "center" => Snap::Center,
                _ => return Err(CommandError(format!("unknown command `{name}`"))),
            };
            return no_arg(Command::SnapFloat(snap));
        }
        match name {
            "pane.split-right" => no_arg(Command::Split(SplitHow::Right)),
            "pane.split-down" => no_arg(Command::Split(SplitHow::Down)),
            "pane.split-auto" => no_arg(Command::Split(SplitHow::Auto)),
            "pane.new-float" => no_arg(Command::NewFloat),
            "pane.kill" => no_arg(Command::KillPane),
            "pane.next" => no_arg(Command::FocusNext),
            "pane.prev" => no_arg(Command::FocusPrev),
            "pane.last" => no_arg(Command::FocusLast),
            "pane.swap-next" => no_arg(Command::SwapNext),
            "pane.swap-prev" => no_arg(Command::SwapPrev),
            "pane.zoom" => no_arg(Command::Zoom),
            "pane.float" => no_arg(Command::ToggleFloat),
            "pane.minimize" => no_arg(Command::Minimize),
            "pane.restore" => no_arg(Command::RestoreMinimized),
            "layout.next" => no_arg(Command::LayoutNext),
            "layout.select" => {
                let a = arg.ok_or_else(|| {
                    CommandError(
                        "`layout.select` needs a preset: even-horizontal, even-vertical, main-vertical, main-horizontal or tiled".into(),
                    )
                })?;
                Preset::from_name(a)
                    .map(Command::LayoutSelect)
                    .ok_or_else(|| CommandError(format!("unknown layout preset `{a}`")))
            }
            "workspace.new" => no_arg(Command::WorkspaceNew),
            "workspace.next" => no_arg(Command::WorkspaceNext),
            "workspace.prev" => no_arg(Command::WorkspacePrev),
            "workspace.last" => no_arg(Command::WorkspaceLast),
            "workspace.select" => {
                let a = arg.ok_or_else(|| {
                    CommandError("`workspace.select` needs a workspace number".into())
                })?;
                a.parse::<usize>()
                    .map(Command::WorkspaceSelect)
                    .map_err(|_| CommandError(format!("`{a}` is not a workspace number")))
            }
            "workspace.rename" => no_arg(Command::WorkspaceRename),
            "workspace.kill" => no_arg(Command::WorkspaceKill),
            "copy-mode" => no_arg(Command::CopyMode),
            "copy-mode.page-up" => no_arg(Command::CopyModePageUp),
            "paste" => no_arg(Command::Paste),
            "palette" => no_arg(Command::Palette),
            "menu" => no_arg(Command::Menu),
            "pane.rename" => no_arg(Command::PaneRename),
            "pane.maximize" => no_arg(Command::MaximizeToggle),
            "keys" => no_arg(Command::Keys),
            "notifications" => no_arg(Command::Notifications),
            "tutorial" => no_arg(Command::Tutorial),
            "about.open" => no_arg(Command::AboutOpen),
            "monitor.open" => no_arg(Command::MonitorOpen),
            "task.build" => no_arg(Command::Task("build".into())),
            "task.run" => no_arg(Command::Task("run".into())),
            "task.test" => no_arg(Command::Task("test".into())),
            "task" => match arg {
                Some(t) => Ok(Command::Task(t.to_owned())),
                None => Err(CommandError("`task` needs a task name".into())),
            },
            "git.open" => no_arg(Command::GitOpen),
            "agent.open" => Ok(Command::AgentOpen(arg.unwrap_or("").to_owned())),
            "agent.resume" => Ok(Command::AgentResume(arg.unwrap_or("").to_owned())),
            "agent.external" => match arg {
                Some(a) => Ok(Command::AgentExternal(a.to_owned())),
                None => Err(CommandError(
                    "`agent.external` needs a name: claude, codex, gemini, aider, opencode or one from [agents.external]".into(),
                )),
            },
            "agent.install" => match arg {
                Some(a) => Ok(Command::AgentInstall(a.to_owned())),
                None => Err(CommandError(
                    "`agent.install` needs a name: claude or codex".into(),
                )),
            },
            "agent.update" => no_arg(Command::AgentUpdate),
            "usage.open" => no_arg(Command::UsageOpen),
            "agent.swap" => match arg {
                Some(a) => Ok(Command::AgentSwap(a.trim().to_owned())),
                None => Err(CommandError("`agent.swap` needs an account name".into())),
            },
            "audit.open" => Ok(Command::AuditOpen(arg.unwrap_or("").to_owned())),
            "center.open" => no_arg(Command::CenterOpen),
            "network.open" => no_arg(Command::NetworkOpen),
            "update.open" => no_arg(Command::UpdateOpen),
            "settings.open" => no_arg(Command::SettingsOpen),
            "clipboard.open" => no_arg(Command::ClipboardOpen),
            "clipboard.clear" => no_arg(Command::ClipboardClear),
            "config.reload" => no_arg(Command::ReloadConfig),
            "send-prefix" => no_arg(Command::SendPrefix),
            "session.quit" => no_arg(Command::Quit),
            "client.detach" => no_arg(Command::Detach),
            "session.new" => no_arg(Command::SessionNew),
            "session.next" => no_arg(Command::SessionNext),
            "session.prev" => no_arg(Command::SessionPrev),
            "session.rename" => no_arg(Command::SessionRename),
            "session.restore" => no_arg(Command::SessionRestore),
            "session.switch" => arg
                .map(|a| Command::SessionSwitch(a.to_owned()))
                .ok_or_else(|| CommandError("`session.switch` needs a session name".into())),
            "theme.select" => {
                let a = arg.ok_or_else(|| {
                    CommandError(
                        "`theme.select` needs a theme: auto, dark, light or high-contrast".into(),
                    )
                })?;
                a.parse::<ThemeBase>()
                    .map(Command::ThemeSelect)
                    .map_err(CommandError)
            }
            "" => Err(CommandError("empty command".into())),
            _ => Err(CommandError(format!("unknown command `{name}`"))),
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Split(SplitHow::Right) => f.write_str("pane.split-right"),
            Command::Split(SplitHow::Down) => f.write_str("pane.split-down"),
            Command::Split(SplitHow::Auto) => f.write_str("pane.split-auto"),
            Command::NewFloat => f.write_str("pane.new-float"),
            Command::KillPane => f.write_str("pane.kill"),
            Command::Focus(d) => write!(f, "pane.focus-{}", dir_name(*d)),
            Command::FocusNext => f.write_str("pane.next"),
            Command::FocusPrev => f.write_str("pane.prev"),
            Command::FocusLast => f.write_str("pane.last"),
            Command::SwapNext => f.write_str("pane.swap-next"),
            Command::SwapPrev => f.write_str("pane.swap-prev"),
            Command::Resize(d, 1) => write!(f, "pane.resize-{}", dir_name(*d)),
            Command::Resize(d, n) => write!(f, "pane.resize-{} {n}", dir_name(*d)),
            Command::Zoom => f.write_str("pane.zoom"),
            Command::ToggleFloat => f.write_str("pane.float"),
            Command::Minimize => f.write_str("pane.minimize"),
            Command::RestoreMinimized => f.write_str("pane.restore"),
            Command::SnapFloat(s) => write!(f, "pane.snap-{}", snap_name(*s)),
            Command::LayoutNext => f.write_str("layout.next"),
            Command::LayoutSelect(p) => write!(f, "layout.select {}", p.name()),
            Command::WorkspaceNew => f.write_str("workspace.new"),
            Command::WorkspaceNext => f.write_str("workspace.next"),
            Command::WorkspacePrev => f.write_str("workspace.prev"),
            Command::WorkspaceLast => f.write_str("workspace.last"),
            Command::WorkspaceSelect(n) => write!(f, "workspace.select {n}"),
            Command::WorkspaceRename => f.write_str("workspace.rename"),
            Command::WorkspaceKill => f.write_str("workspace.kill"),
            Command::CopyMode => f.write_str("copy-mode"),
            Command::CopyModePageUp => f.write_str("copy-mode.page-up"),
            Command::Paste => f.write_str("paste"),
            Command::Palette => f.write_str("palette"),
            Command::Menu => f.write_str("menu"),
            Command::PaneRename => f.write_str("pane.rename"),
            Command::MaximizeToggle => f.write_str("pane.maximize"),
            Command::Keys => f.write_str("keys"),
            Command::Notifications => f.write_str("notifications"),
            Command::Tutorial => f.write_str("tutorial"),
            Command::AboutOpen => f.write_str("about.open"),
            Command::ReloadConfig => f.write_str("config.reload"),
            Command::SendPrefix => f.write_str("send-prefix"),
            Command::Quit => f.write_str("session.quit"),
            Command::Detach => f.write_str("client.detach"),
            Command::SessionNew => f.write_str("session.new"),
            Command::SessionNext => f.write_str("session.next"),
            Command::SessionPrev => f.write_str("session.prev"),
            Command::SessionSwitch(name) => write!(f, "session.switch {name}"),
            Command::SessionRename => f.write_str("session.rename"),
            Command::SessionRestore => f.write_str("session.restore"),
            Command::ThemeSelect(b) => write!(f, "theme.select {b}"),
            Command::ImageOpen(p) => write!(f, "image.open {p}"),
            Command::MarkdownOpen(p) => write!(f, "markdown.open {p}"),
            Command::FilesOpen(p) if p.is_empty() => f.write_str("files.open"),
            Command::FilesOpen(p) => write!(f, "files.open {p}"),
            Command::FileOpen(p) => write!(f, "file.open {p}"),
            Command::FileEdit(p) => write!(f, "file.edit {p}"),
            Command::MonitorOpen => f.write_str("monitor.open"),
            Command::Task(t) if matches!(t.as_str(), "build" | "run" | "test") => {
                write!(f, "task.{t}")
            }
            Command::Task(t) => write!(f, "task {t}"),
            Command::GitOpen => f.write_str("git.open"),
            Command::NetworkOpen => f.write_str("network.open"),
            Command::UpdateOpen => f.write_str("update.open"),
            Command::SettingsOpen => f.write_str("settings.open"),
            Command::ClipboardOpen => f.write_str("clipboard.open"),
            Command::ClipboardClear => f.write_str("clipboard.clear"),
            Command::AgentOpen(p) if p.is_empty() => f.write_str("agent.open"),
            Command::AgentOpen(p) => write!(f, "agent.open {p}"),
            Command::AgentResume(id) if id.is_empty() => f.write_str("agent.resume"),
            Command::AgentResume(id) => write!(f, "agent.resume {id}"),
            Command::AgentExternal(n) => write!(f, "agent.external {n}"),
            Command::AgentInstall(n) => write!(f, "agent.install {n}"),
            Command::AgentUpdate => f.write_str("agent.update"),
            Command::AccountUse(a) => write!(f, "account.use {a}"),
            Command::AccountTask(t) => write!(f, "account.{t}"),
            Command::UsageOpen => f.write_str("usage.open"),
            Command::HarnessOpen(g) if g.is_empty() => f.write_str("harness.open"),
            Command::HarnessOpen(g) => write!(f, "harness.open {g}"),
            Command::AgentSwap(a) => write!(f, "agent.swap {a}"),
            Command::AuditOpen(id) if id.is_empty() => f.write_str("audit.open"),
            Command::AuditOpen(id) => write!(f, "audit.open {id}"),
            Command::CenterOpen => f.write_str("center.open"),
        }
    }
}

impl Command {
    /// A short human description (for which-key and the palette).
    pub fn description(&self) -> String {
        let s = self.to_string();
        let name = s.split_whitespace().next().unwrap_or("");
        let base = COMMANDS
            .iter()
            .find(|c| c.name == name)
            .map_or("", |c| c.description);
        match self {
            Command::Resize(_, n) if *n > 1 => format!("{base} by {n}"),
            Command::LayoutSelect(p) => format!("{base}: {}", p.name()),
            Command::ThemeSelect(b) => format!("{base}: {b}"),
            Command::WorkspaceSelect(n) => format!("{base} {n}"),
            Command::SessionSwitch(name) => format!("{base} {name}"),
            _ => base.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_command_parses_and_round_trips() {
        for c in COMMANDS {
            let text = match c.arg {
                Some(a) => format!("{} {a}", c.name),
                None => c.name.to_owned(),
            };
            let cmd: Command = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
            let again: Command = cmd.to_string().parse().expect("display parses");
            assert_eq!(again, cmd, "{text}");
            assert!(!cmd.description().is_empty(), "{text} has no description");
        }
    }

    #[test]
    fn a_harness_goal_is_the_rest_of_the_line() {
        assert_eq!(
            "harness.open add a --verbose flag to `run`".parse::<Command>(),
            Ok(Command::HarnessOpen("add a --verbose flag to `run`".into()))
        );
        assert_eq!(
            "harness.open".parse::<Command>(),
            Ok(Command::HarnessOpen(String::new()))
        );
        assert_eq!(
            Command::HarnessOpen("fix it".into()).to_string(),
            "harness.open fix it"
        );
    }

    #[test]
    fn arguments() {
        assert_eq!(
            "pane.resize-left 5".parse(),
            Ok(Command::Resize(Direction::Left, 5))
        );
        assert_eq!(
            "pane.resize-up".parse(),
            Ok(Command::Resize(Direction::Up, 1))
        );
        assert_eq!(
            "layout.select main-vertical".parse(),
            Ok(Command::LayoutSelect(Preset::MainVertical))
        );
        assert_eq!(
            "workspace.select 3".parse(),
            Ok(Command::WorkspaceSelect(3))
        );
        assert_eq!(
            "image.open /tmp/my photos/a.png".parse(),
            Ok(Command::ImageOpen("/tmp/my photos/a.png".into()))
        );
        assert_eq!(
            "markdown.open  notes/To Do.md ".parse(),
            Ok(Command::MarkdownOpen("notes/To Do.md".into()))
        );
        assert!("image.open".parse::<Command>().is_err());
        assert!("image.opener x".parse::<Command>().is_err());
        for bad in [
            "",
            "nope",
            "pane.zoom 1",
            "pane.resize-left 0",
            "pane.resize-left x",
            "layout.select",
            "layout.select spiral",
            "workspace.select",
            "pane.snap-diagonal",
            "pane.kill now please",
        ] {
            assert!(bad.parse::<Command>().is_err(), "accepted {bad:?}");
        }
    }
}
