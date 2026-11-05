// SPDX-License-Identifier: GPL-3.0-or-later
//! `[worktrees]`: a git worktree per task (M12a, D-113, D-114).
//!
//! Each worktree is a branch checked out in a folder of its own, with a
//! block of ports and a TermOS workspace of panes (the agent, a shell, the
//! project's servers). What a project sets up in a new worktree is in its
//! `termos.toml` (`[worktree]`); these are the user's settings.

use std::path::PathBuf;

use serde::Deserialize;

/// What opens in a worktree's agent pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeAgent {
    /// The first of Claude Code, Codex and Gemini CLI that is installed;
    /// none when none is.
    #[default]
    Auto,
    Claude,
    Codex,
    Gemini,
    /// TermOS's own agent window.
    Termos,
    /// No agent pane.
    None,
}

impl WorktreeAgent {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Termos => "termos",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Raw")]
pub struct WorktreesConfig {
    /// Where worktrees are made (`<root>/<repository>/<name>`); `None`:
    /// `worktrees` in TermOS's data folder.
    pub root: Option<PathBuf>,
    /// New branches are named this plus the worktree's name.
    pub branch_prefix: String,
    /// The ports handed out, first and last (inclusive). The default stays
    /// below every system's range for outgoing connections.
    pub port_range: (u16, u16),
    /// Ports each worktree gets (`PORT`, `TERMOS_PORT_0`…).
    pub ports: u16,
    pub agent: WorktreeAgent,
    /// After a merge, the worktree's folder is removed and its branch
    /// deleted (it can be restored from the archive).
    pub archive_merged: bool,
}

impl Default for WorktreesConfig {
    fn default() -> Self {
        Self {
            root: None,
            branch_prefix: "termos/".to_owned(),
            port_range: (20000, 29999),
            ports: 10,
            agent: WorktreeAgent::Auto,
            archive_merged: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct Raw {
    root: Option<PathBuf>,
    branch_prefix: String,
    port_range: [u16; 2],
    ports: u16,
    agent: WorktreeAgent,
    archive_merged: bool,
}

impl Default for Raw {
    fn default() -> Self {
        let d = WorktreesConfig::default();
        Self {
            root: d.root,
            branch_prefix: d.branch_prefix,
            port_range: [d.port_range.0, d.port_range.1],
            ports: d.ports,
            agent: d.agent,
            archive_merged: d.archive_merged,
        }
    }
}

impl TryFrom<Raw> for WorktreesConfig {
    type Error = String;

    fn try_from(r: Raw) -> Result<Self, String> {
        let [first, last] = r.port_range;
        if first == 0 || first > last {
            return Err(format!(
                "worktrees.port-range must be [first, last] with 0 < first <= last, not [{first}, {last}]"
            ));
        }
        let room = u32::from(last) - u32::from(first) + 1;
        if r.ports == 0 || u32::from(r.ports) > room {
            return Err(format!(
                "worktrees.ports must be between 1 and the {room} ports of port-range"
            ));
        }
        let bad = |c: char| c.is_whitespace() || "~^:?*[\\".contains(c) || c.is_control();
        if r.branch_prefix.chars().any(bad) || r.branch_prefix.contains("..") {
            return Err(format!(
                "worktrees.branch-prefix {:?} is not usable in a branch name",
                r.branch_prefix
            ));
        }
        Ok(Self {
            root: r.root,
            branch_prefix: r.branch_prefix,
            port_range: (first, last),
            ports: r.ports,
            agent: r.agent,
            archive_merged: r.archive_merged,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<WorktreesConfig, String> {
        #[derive(Deserialize)]
        struct W {
            worktrees: WorktreesConfig,
        }
        toml::from_str::<W>(src)
            .map(|w| w.worktrees)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn settings_parse_and_are_checked() {
        let c = parse(
            "[worktrees]\nport-range = [30000, 30099]\nports = 5\nagent = \"codex\"\nbranch-prefix = \"wt/\"\n",
        )
        .expect("valid");
        assert_eq!(c.port_range, (30000, 30099));
        assert_eq!(c.ports, 5);
        assert_eq!(c.agent, WorktreeAgent::Codex);
        assert_eq!(c.branch_prefix, "wt/");
        assert!(c.archive_merged);
        assert_eq!(
            parse("[worktrees]\n").expect("defaults"),
            WorktreesConfig::default()
        );
        assert!(parse("[worktrees]\nport-range = [3000, 2000]\n").is_err());
        assert!(parse("[worktrees]\nport-range = [3000, 3004]\nports = 6\n").is_err());
        assert!(parse("[worktrees]\nports = 0\n").is_err());
        assert!(parse("[worktrees]\nbranch-prefix = \"a b/\"\n").is_err());
        assert!(parse("[worktrees]\nflavour = 1\n").is_err());
    }
}
