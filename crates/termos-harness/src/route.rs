// SPDX-License-Identifier: GPL-3.0-or-later
//! Routing: which backend and model each task goes to (D-096, D-099,
//! D-100).
//!
//! A task's kind gives its tier (`[harness.routes]`, else the default
//! from the research guide). The backends are tried in order: those the
//! route names, else every backend in `[harness] order` (by default the
//! subscriptions, Claude Code then Codex CLI, then the API backends).
//! Each backend fills the tier with its model; the CLIs fall back to their
//! own default model, an API backend to its nearest tier that has one.

use termos_core::config::Config;
use termos_core::config::harness::{BackendKind, HarnessConfig, TaskKind, Tier, TierModels};

/// Something that runs tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    /// Its name in settings (`claude`, `codex`, `openrouter`).
    pub name: String,
    pub kind: BackendKind,
    /// Claude Code and Codex: the account (`None`: the one in use).
    pub account: Option<String>,
    /// API: the provider under `[agents.providers]`.
    pub provider: Option<String>,
    pub models: TierModels,
}

impl Backend {
    /// "Claude Code", "Codex CLI · work", "openrouter (API)".
    pub fn title(&self) -> String {
        let base = match self.kind {
            BackendKind::Claude => "Claude Code".to_owned(),
            BackendKind::Codex => "Codex CLI".to_owned(),
            BackendKind::Api => format!("{} (API)", self.name),
        };
        let custom = matches!(self.kind, BackendKind::Claude | BackendKind::Codex)
            && self.name != "claude"
            && self.name != "codex";
        match (&self.account, custom) {
            (Some(a), _) => format!("{base} · {a}"),
            (None, true) => format!("{base} ({})", self.name),
            (None, false) => base,
        }
    }

    /// Runs under a subscription (no per-token cost).
    pub fn is_subscription(&self) -> bool {
        matches!(self.kind, BackendKind::Claude | BackendKind::Codex)
    }

    /// The model for `tier`: its own, else (API only) the nearest tier
    /// that has one; `None` for a CLI means its default model.
    pub fn model(&self, tier: Tier) -> Result<Option<String>, String> {
        if let Some(m) = self.models.get(tier) {
            return Ok(Some(m.to_owned()));
        }
        if self.is_subscription() {
            return Ok(None);
        }
        let nearest: &[Tier] = match tier {
            Tier::Frontier => &[Tier::Balanced, Tier::Fast],
            Tier::Balanced => &[Tier::Frontier, Tier::Fast],
            Tier::Fast => &[Tier::Balanced, Tier::Frontier],
        };
        nearest
            .iter()
            .find_map(|t| self.models.get(*t))
            .map(|m| Some(m.to_owned()))
            .ok_or_else(|| {
                format!(
                    "no model is set for it ([harness.backends.{}.models])",
                    self.name
                )
            })
    }
}

/// Every backend, in the order they are tried: `[harness] order`, then
/// Claude Code, Codex CLI, the backends of `[harness.backends]` and one
/// API backend for each provider no backend uses.
pub fn backends(config: &Config) -> Vec<Backend> {
    let h = &config.harness;
    let mut all: Vec<Backend> = Vec::new();
    let named = |name: &str, kind: BackendKind| {
        let c = h.backends.get(name);
        Backend {
            name: name.to_owned(),
            kind,
            account: c.and_then(|c| c.account.clone()).filter(|a| !a.is_empty()),
            provider: None,
            models: c.map(|c| c.models.clone()).unwrap_or_default(),
        }
    };
    all.push(named("claude", BackendKind::Claude));
    all.push(named("codex", BackendKind::Codex));
    for (name, c) in &h.backends {
        if name == "claude" || name == "codex" {
            continue;
        }
        let kind = c.kind.unwrap_or(BackendKind::Api);
        all.push(Backend {
            name: name.clone(),
            kind,
            account: c.account.clone().filter(|a| !a.is_empty()),
            provider: (kind == BackendKind::Api)
                .then(|| c.provider.clone().unwrap_or_else(|| name.clone())),
            models: c.models.clone(),
        });
    }
    // Providers no backend uses yet are API backends of their own name.
    for p in config.agents.providers.keys() {
        let used = all
            .iter()
            .any(|b| b.provider.as_deref() == Some(p.as_str()) || b.name == *p);
        if !used {
            all.push(Backend {
                name: p.clone(),
                kind: BackendKind::Api,
                account: None,
                provider: Some(p.clone()),
                models: TierModels::default(),
            });
        }
    }
    all.retain(|b| !h.backends.get(&b.name).is_some_and(|c| c.disabled));
    // The settings' order first.
    let mut ordered: Vec<Backend> = Vec::new();
    for name in &h.order {
        if let Some(i) = all.iter().position(|b| &b.name == name) {
            ordered.push(all.remove(i));
        }
    }
    ordered.extend(all);
    ordered
}

/// Names in `[harness]` that match no backend, for `termos harness
/// status` and the window.
pub fn unknown_names(config: &Config) -> Vec<String> {
    let all = backends(config);
    let known = |n: &String| all.iter().any(|b| &b.name == n);
    let h = &config.harness;
    let mut out: Vec<String> = h.order.iter().filter(|n| !known(n)).cloned().collect();
    for r in h.routes.values() {
        out.extend(r.backends().iter().filter(|n| !known(n)).cloned());
    }
    if let Some(p) = h.planner.as_ref().filter(|n| !known(n)) {
        out.push(p.clone());
    }
    out.sort();
    out.dedup();
    out
}

/// Whether a backend can take a task now.
pub trait Availability {
    /// Why `b` cannot be used now, if it cannot.
    fn unavailable(&self, b: &Backend) -> Option<String>;
}

/// Everything is available (planning a route before anything runs).
pub struct Anything;

impl Availability for Anything {
    fn unavailable(&self, _: &Backend) -> Option<String> {
        None
    }
}

/// One backend a task may go to, with the model it would use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub backend: Backend,
    /// `None`: the CLI's own default model.
    pub model: Option<String>,
}

impl Choice {
    /// "Claude Code (the CLI's default model)", "openrouter · coder-x".
    pub fn describe(&self) -> String {
        match &self.model {
            Some(m) => format!("{} · {m}", self.backend.title()),
            None => format!("{} (its default model)", self.backend.title()),
        }
    }
}

/// Where a task goes: the backends to try, in order, and those passed
/// over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub kind: TaskKind,
    pub tier: Tier,
    pub choices: Vec<Choice>,
    /// Backends left out, and why.
    pub skipped: Vec<(String, String)>,
}

impl Route {
    pub fn first(&self) -> Option<&Choice> {
        self.choices.first()
    }

    /// "coding → balanced: Claude Code (its default model), then Codex
    /// CLI · …".
    pub fn explain(&self) -> String {
        let mut s = format!("{} → {}: ", self.kind.name(), self.tier.name());
        if self.choices.is_empty() {
            s.push_str("no backend can take it");
        } else {
            let tries: Vec<String> = self.choices.iter().map(Choice::describe).collect();
            s.push_str(&tries.join(", then "));
        }
        for (name, why) in &self.skipped {
            s.push_str(&format!("; not {name}: {why}"));
        }
        s
    }
}

/// The route for a task of `kind` (`pinned`: the backend the user chose
/// for it).
pub fn route(
    h: &HarnessConfig,
    all: &[Backend],
    kind: TaskKind,
    pinned: Option<&str>,
    avail: &dyn Availability,
) -> Route {
    let tier = h.tier(kind);
    let names: Vec<String> = match (pinned, h.routes.get(&kind)) {
        (Some(p), _) => vec![p.to_owned()],
        (None, Some(r)) if !r.backends().is_empty() => r.backends().to_vec(),
        (None, _) => all.iter().map(|b| b.name.clone()).collect(),
    };
    let mut choices = Vec::new();
    let mut skipped = Vec::new();
    for name in names {
        let Some(b) = all.iter().find(|b| b.name == name) else {
            skipped.push((name, "no backend has this name".to_owned()));
            continue;
        };
        if let Some(why) = avail.unavailable(b) {
            skipped.push((b.title(), why));
            continue;
        }
        match b.model(tier) {
            Ok(model) => choices.push(Choice {
                backend: b.clone(),
                model,
            }),
            Err(why) => skipped.push((b.title(), why)),
        }
    }
    if !h.fallback {
        choices.truncate(1);
    }
    Route {
        kind,
        tier,
        choices,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> Config {
        Config::parse(toml, std::path::Path::new("config.toml")).expect("config")
    }

    struct Missing(&'static [&'static str]);

    impl Availability for Missing {
        fn unavailable(&self, b: &Backend) -> Option<String> {
            self.0
                .contains(&b.name.as_str())
                .then(|| "not installed".to_owned())
        }
    }

    const SETTINGS: &str = r#"
[agents.providers.openrouter]
kind = "openai-compatible"
base-url = "http://127.0.0.1:1/v1"
[agents.providers.local]
kind = "local"
[harness.backends.claude]
models = { frontier = "fixture-big" }
[harness.backends.openrouter]
models = { balanced = "fixture/coder", fast = "fixture/small" }
"#;

    #[test]
    fn subscriptions_come_first_then_the_api_backends() {
        let c = config(SETTINGS);
        let all = backends(&c);
        let names: Vec<&str> = all.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["claude", "codex", "openrouter", "local"]);
        assert!(all[0].is_subscription() && !all[3].is_subscription());

        // Coding is balanced: Claude Code has no balanced model, so its
        // own default; `local` has no model at all and is passed over.
        let r = route(&c.harness, &all, TaskKind::Coding, None, &Anything);
        assert_eq!(r.tier, Tier::Balanced);
        let models: Vec<(&str, Option<&str>)> = r
            .choices
            .iter()
            .map(|c| (c.backend.name.as_str(), c.model.as_deref()))
            .collect();
        assert_eq!(
            models,
            [
                ("claude", None),
                ("codex", None),
                ("openrouter", Some("fixture/coder"))
            ]
        );
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].1.contains("[harness.backends.local.models]"));
        // Review is frontier: Claude Code's frontier model; openrouter's
        // nearest tier.
        let r = route(&c.harness, &all, TaskKind::Review, None, &Anything);
        assert_eq!(r.choices[0].model.as_deref(), Some("fixture-big"));
        assert_eq!(r.choices[2].model.as_deref(), Some("fixture/coder"));
        assert!(
            r.explain()
                .starts_with("review → frontier: Claude Code · fixture-big, then")
        );
    }

    #[test]
    fn routes_order_and_availability_decide() {
        let c = config(&format!(
            "{SETTINGS}[harness]\norder = [\"openrouter\"]\nfallback = true\n[harness.routes]\nsearch = {{ tier = \"fast\", backends = [\"codex\", \"nope\"] }}\ndocs = \"frontier\"\n"
        ));
        let all = backends(&c);
        assert_eq!(all[0].name, "openrouter", "the settings' order first");
        let r = route(&c.harness, &all, TaskKind::Search, None, &Anything);
        assert_eq!(r.choices.len(), 1);
        assert_eq!(r.choices[0].backend.name, "codex");
        assert_eq!(r.skipped[0].0, "nope");
        assert_eq!(r.kind.name(), "search");
        assert_eq!(c.harness.tier(TaskKind::Docs), Tier::Frontier);
        // Codex missing: the docs route skips it and says so.
        let r = route(&c.harness, &all, TaskKind::Docs, None, &Missing(&["codex"]));
        assert!(
            r.skipped
                .iter()
                .any(|(n, w)| n == "Codex CLI" && w == "not installed")
        );
        assert_eq!(
            r.first().map(|c| c.backend.name.as_str()),
            Some("openrouter")
        );
        // A pinned backend is the only one.
        let r = route(&c.harness, &all, TaskKind::Docs, Some("claude"), &Anything);
        assert_eq!(r.choices.len(), 1);
        assert_eq!(r.choices[0].backend.name, "claude");
        assert!(unknown_names(&c).contains(&"nope".to_owned()));
    }

    #[test]
    fn named_backends_accounts_and_no_fallback() {
        let c = config(
            r#"
[harness]
fallback = false
[harness.backends.claude]
account = "work"
[harness.backends.claude-play]
kind = "claude"
account = "play"
disabled = true
[harness.backends.codex]
disabled = true
"#,
        );
        let all = backends(&c);
        let names: Vec<&str> = all.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["claude"], "disabled ones are left out");
        assert_eq!(all[0].title(), "Claude Code · work");
        let r = route(&c.harness, &all, TaskKind::Coding, None, &Anything);
        assert_eq!(r.choices.len(), 1);
    }
}
