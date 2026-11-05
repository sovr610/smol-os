// SPDX-License-Identifier: GPL-3.0-or-later
//! `[harness]`: the coding harness (M11c, D-096, D-099, D-100).
//!
//! A plan is made of typed tasks; each task type goes to a tier
//! (frontier, balanced, fast), and each backend (Claude Code, Codex CLI,
//! an API provider through TermOS's agent) says which model fills each
//! tier. No model is named here: the user fills the tiers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The kinds of task a plan is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    Planning,
    Coding,
    Review,
    Tests,
    Search,
    SmallEdits,
    Docs,
}

impl TaskKind {
    pub const ALL: [Self; 7] = [
        Self::Planning,
        Self::Coding,
        Self::Review,
        Self::Tests,
        Self::Search,
        Self::SmallEdits,
        Self::Docs,
    ];

    /// The name in settings and plans (`small-edits`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Coding => "coding",
            Self::Review => "review",
            Self::Tests => "tests",
            Self::Search => "search",
            Self::SmallEdits => "small-edits",
            Self::Docs => "docs",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.name() == name.trim().to_ascii_lowercase().replace('_', "-"))
    }

    /// What it is for, as the planner is told.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Planning => {
                "designing an approach or breaking down a hard problem, without changing files"
            }
            Self::Coding => "writing or changing code for a feature or a fix",
            Self::Review => "reviewing changes or code for bugs and risks, without changing files",
            Self::Tests => "writing or fixing tests, and running them",
            Self::Search => {
                "finding where things are and how they work in the codebase, without changing files"
            }
            Self::SmallEdits => {
                "a small, well-defined change: a rename, a config value, a one-line fix"
            }
            Self::Docs => "writing or updating documentation and comments",
        }
    }

    /// Whether tasks of this kind change files (and so run one at a time).
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Coding | Self::Tests | Self::SmallEdits | Self::Docs
        )
    }

    /// The tier the research guide suggests (docs/model-guide.md): the
    /// strongest models where mistakes cost most (deciding the approach,
    /// catching bugs), a balanced one for the bulk of the code, fast ones
    /// for reading around and small, clear changes.
    pub fn default_tier(self) -> Tier {
        match self {
            Self::Planning | Self::Review => Tier::Frontier,
            Self::Coding | Self::Tests => Tier::Balanced,
            Self::Search | Self::SmallEdits | Self::Docs => Tier::Fast,
        }
    }
}

/// How capable (and costly) a model a task gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    Frontier,
    Balanced,
    Fast,
}

impl Tier {
    pub const ALL: [Self; 3] = [Self::Frontier, Self::Balanced, Self::Fast];

    pub fn name(self) -> &'static str {
        match self {
            Self::Frontier => "frontier",
            Self::Balanced => "balanced",
            Self::Fast => "fast",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name.trim())
    }
}

/// What runs a backend's tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    /// Claude Code, headless (`claude -p`), under the user's plan.
    Claude,
    /// Codex CLI, headless (`codex exec`), under the user's plan.
    Codex,
    /// TermOS's own agent with a provider from `[agents.providers]`.
    Api,
}

/// The model for each tier; an unset tier uses the backend's default.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TierModels {
    pub frontier: Option<String>,
    pub balanced: Option<String>,
    pub fast: Option<String>,
}

impl TierModels {
    pub fn get(&self, tier: Tier) -> Option<&str> {
        match tier {
            Tier::Frontier => self.frontier.as_deref(),
            Tier::Balanced => self.balanced.as_deref(),
            Tier::Fast => self.fast.as_deref(),
        }
        .filter(|m| !m.trim().is_empty())
    }

    pub fn is_empty(&self) -> bool {
        Tier::ALL.iter().all(|t| self.get(*t).is_none())
    }
}

/// `[harness.backends.<name>]`. `claude` and `codex` exist without one;
/// every provider under `[agents.providers]` is an API backend of the
/// same name.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct BackendConfig {
    /// Default: `claude` and `codex` by name, else `api`.
    pub kind: Option<BackendKind>,
    /// Claude Code and Codex: the account (default: the one in use).
    pub account: Option<String>,
    /// API: the provider (default: the backend's name).
    pub provider: Option<String>,
    /// The model behind each tier.
    pub models: TierModels,
    /// Never used while set.
    pub disabled: bool,
}

/// `[harness.routes]`: a task type's tier, and optionally the backends to
/// try, in order (`coding = "balanced"` or
/// `review = { tier = "frontier", backends = ["codex", "claude"] }`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum RouteConfig {
    Tier(Tier),
    Full {
        tier: Tier,
        #[serde(default)]
        backends: Vec<String>,
    },
}

impl RouteConfig {
    pub fn tier(&self) -> Tier {
        match self {
            Self::Tier(t) | Self::Full { tier: t, .. } => *t,
        }
    }

    pub fn backends(&self) -> &[String] {
        match self {
            Self::Tier(_) => &[],
            Self::Full { backends, .. } => backends,
        }
    }
}

/// `[harness]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct HarnessConfig {
    /// Backends in the order they are tried (default: Claude Code, Codex,
    /// then the API backends: subscriptions first, D-099).
    pub order: Vec<String>,
    /// The backend that makes plans (default: the planning route's).
    pub planner: Option<String>,
    /// Try the next backend when one fails or reaches its plan limit.
    pub fallback: bool,
    /// Reviews go first to a backend other than the one that wrote the
    /// code they review (models favour their own work; docs/model-guide.md).
    pub cross_review: bool,
    /// Plans of at most this many tasks start without approval (0: always
    /// ask).
    pub auto_run: usize,
    /// Tasks that may run at the same time: read-only ones, and in
    /// worktrees editing ones too.
    pub parallel: usize,
    /// In a git repository, each editing task runs in a worktree of its
    /// own, merged into the plan's branch, which is reviewed at the end
    /// (D-113). `false`: tasks edit the project in place, one at a time.
    pub worktrees: bool,
    /// The most tasks a plan may have.
    pub max_tasks: usize,
    /// US dollars the API backends may spend on one plan (0: no limit).
    /// Subscriptions have no per-token cost.
    pub budget_usd: f64,
    /// Minutes a task may run before it is stopped.
    pub task_minutes: u64,
    /// Commands the agents may run without asking, besides the project's
    /// build, test and lint tasks (patterns; `*` matches anything).
    pub allow_commands: Vec<String>,
    /// Allow the project's own build, test and lint tasks (`termos task`).
    pub allow_project_tasks: bool,
    pub backends: BTreeMap<String, BackendConfig>,
    pub routes: BTreeMap<TaskKind, RouteConfig>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            order: Vec::new(),
            planner: None,
            fallback: true,
            cross_review: true,
            auto_run: 0,
            parallel: 3,
            worktrees: true,
            max_tasks: 8,
            budget_usd: 0.0,
            task_minutes: 30,
            allow_commands: Vec::new(),
            allow_project_tasks: true,
            backends: BTreeMap::new(),
            routes: BTreeMap::new(),
        }
    }
}

impl HarnessConfig {
    /// A task type's tier: the route's, else the default.
    pub fn tier(&self, kind: TaskKind) -> Tier {
        self.routes
            .get(&kind)
            .map_or_else(|| kind.default_tier(), RouteConfig::tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Wrap {
        harness: HarnessConfig,
    }

    #[test]
    fn settings_parse() {
        let w: Wrap = toml::from_str(
            r#"
[harness]
order = ["codex", "claude", "openrouter"]
budget-usd = 1.5
allow-commands = ["make check"]
[harness.backends.claude]
account = "work"
models = { frontier = "fixture-big", fast = "fixture-small" }
[harness.backends.openrouter]
models.balanced = "fixture/coder"
[harness.routes]
coding = "frontier"
review = { tier = "frontier", backends = ["codex"] }
"#,
        )
        .expect("parses");
        let h = w.harness;
        assert_eq!(h.order, ["codex", "claude", "openrouter"]);
        assert_eq!(h.tier(TaskKind::Coding), Tier::Frontier);
        assert_eq!(h.tier(TaskKind::Search), Tier::Fast, "the default");
        assert_eq!(h.routes[&TaskKind::Review].backends(), ["codex"]);
        let claude = &h.backends["claude"];
        assert_eq!(claude.account.as_deref(), Some("work"));
        assert_eq!(claude.models.get(Tier::Fast), Some("fixture-small"));
        assert_eq!(claude.models.get(Tier::Balanced), None);
        assert!(h.fallback && h.allow_project_tasks);
        assert!(toml::from_str::<Wrap>("[harness.routes]\nplaning = \"fast\"").is_err());
        assert!(toml::from_str::<Wrap>("[harness.routes]\ncoding = \"huge\"").is_err());
    }

    #[test]
    fn kinds_have_names_and_tiers() {
        for k in TaskKind::ALL {
            assert_eq!(TaskKind::from_name(k.name()), Some(k));
        }
        assert_eq!(
            TaskKind::from_name("small_edits"),
            Some(TaskKind::SmallEdits)
        );
        assert!(TaskKind::Coding.writes() && !TaskKind::Review.writes());
    }
}
