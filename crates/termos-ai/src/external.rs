// SPDX-License-Identifier: GPL-3.0-or-later
//! External CLI agents (Claude Code, Codex CLI, …) in terminal windows
//! (ARCHITECTURE.md §11, D-063).
//!
//! `agent.external NAME` opens a float running `termos agent external
//! NAME`, which starts the preset's program in the focused pane's folder,
//! under the sandbox when the preset asks for it: the workspace and the
//! agent's own settings folders writable, credentials hidden, the network
//! on (they talk to their APIs). Their own keys and settings are theirs:
//! the environment is passed on unchanged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use termos_core::config::agents::ExternalAgentConfig;

/// A ready-to-run preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: String,
    pub command: Vec<String>,
    pub description: String,
    pub sandbox: bool,
    pub network: bool,
    pub write_paths: Vec<PathBuf>,
    /// Where to get it, for the "not installed" message.
    pub install: Option<&'static str>,
}

/// The built-in presets: name, program, description, its settings
/// folders, where to get it.
const BUILTIN: &[(&str, &str, &str, &[&str], &str)] = &[
    (
        "claude",
        "claude",
        "Claude Code",
        &["~/.claude", "~/.claude.json"],
        "https://code.claude.com/docs",
    ),
    (
        "codex",
        "codex",
        "Codex CLI",
        &["~/.codex"],
        "https://github.com/openai/codex",
    ),
    (
        "gemini",
        "gemini",
        "Gemini CLI",
        &["~/.gemini"],
        "https://github.com/google-gemini/gemini-cli",
    ),
    (
        "aider",
        "aider",
        "aider",
        &["~/.aider"],
        "https://aider.chat",
    ),
    (
        "opencode",
        "opencode",
        "opencode",
        &["~/.local/share/opencode", "~/.config/opencode"],
        "https://opencode.ai",
    ),
];

/// Every preset: the built-in ones, then the config's (which add to or
/// replace them).
pub fn presets(config: &BTreeMap<String, ExternalAgentConfig>) -> BTreeMap<String, Preset> {
    let mut out: BTreeMap<String, Preset> = BUILTIN
        .iter()
        .map(|(name, program, description, writes, install)| {
            (
                (*name).to_owned(),
                Preset {
                    name: (*name).to_owned(),
                    command: vec![(*program).to_owned()],
                    description: (*description).to_owned(),
                    sandbox: false,
                    network: true,
                    write_paths: writes.iter().map(PathBuf::from).collect(),
                    install: Some(install),
                },
            )
        })
        .collect();
    for (name, c) in config {
        let base = out.remove(name);
        let preset = Preset {
            name: name.clone(),
            command: if c.command.is_empty() {
                base.as_ref().map(|b| b.command.clone()).unwrap_or_default()
            } else {
                c.command.clone()
            },
            description: c
                .description
                .clone()
                .or_else(|| base.as_ref().map(|b| b.description.clone()))
                .unwrap_or_else(|| name.clone()),
            sandbox: c.sandbox,
            network: c.network.unwrap_or(true),
            write_paths: if c.write_paths.is_empty() {
                base.as_ref()
                    .map(|b| b.write_paths.clone())
                    .unwrap_or_default()
            } else {
                c.write_paths.clone()
            },
            install: base.and_then(|b| b.install),
        };
        out.insert(name.clone(), preset);
    }
    out
}

/// The command line for `preset` in `workspace`: the program itself, or
/// the sandbox around it.
pub fn command_line(
    preset: &Preset,
    extra_args: &[String],
    workspace: &Path,
    home: Option<&Path>,
    caps: &crate::sandbox::Capabilities,
    helper: Option<&Path>,
) -> Result<Vec<String>, String> {
    if preset.command.is_empty() {
        return Err(format!(
            "[agents.external.{}] has no `command`",
            preset.name
        ));
    }
    let mut argv = preset.command.clone();
    argv.extend(extra_args.iter().cloned());
    if !preset.sandbox {
        return Ok(argv);
    }
    let writable: Vec<PathBuf> = preset
        .write_paths
        .iter()
        .map(|p| crate::policy::expand_home(p, home))
        .collect();
    let hidden: Vec<PathBuf> = home
        .iter()
        .flat_map(|h| {
            crate::policy::SENSITIVE_IN_HOME
                .iter()
                .map(move |s| h.join(s))
        })
        .filter(|h| {
            !writable
                .iter()
                .any(|w| w.starts_with(h) || h.starts_with(w))
        })
        .collect();
    let spec = crate::sandbox::Spec {
        workspace: workspace.to_owned(),
        writable,
        readable: Vec::new(),
        hidden,
        network: preset.network,
        cwd: workspace.to_owned(),
        keep_env: true,
    };
    crate::sandbox::wrap(
        &spec,
        caps,
        termos_core::config::agents::SandboxMode::Auto,
        helper,
        &argv,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_and_configured_presets() {
        let mut cfg = BTreeMap::new();
        cfg.insert(
            "claude".to_owned(),
            ExternalAgentConfig {
                sandbox: true,
                ..ExternalAgentConfig::default()
            },
        );
        cfg.insert(
            "mine".to_owned(),
            ExternalAgentConfig {
                command: vec!["my-agent".into(), "--fast".into()],
                ..ExternalAgentConfig::default()
            },
        );
        let p = presets(&cfg);
        assert_eq!(p["claude"].command, ["claude"]);
        assert!(p["claude"].sandbox && p["claude"].network);
        assert_eq!(
            p["claude"].write_paths,
            [PathBuf::from("~/.claude"), PathBuf::from("~/.claude.json")]
        );
        assert!(!p["codex"].sandbox);
        assert_eq!(p["mine"].command, ["my-agent", "--fast"]);
        assert_eq!(p["mine"].description, "mine");

        let caps = crate::sandbox::Capabilities::none();
        let line = command_line(
            &p["codex"],
            &["--help".into()],
            Path::new("/w"),
            None,
            &caps,
            None,
        )
        .expect("line");
        assert_eq!(line, ["codex", "--help"]);
    }
}
