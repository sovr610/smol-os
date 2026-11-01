// SPDX-License-Identifier: GPL-3.0-or-later
//! Well-known locations, following the XDG base directory spec on every
//! platform (like tmux and most terminal tools, also on macOS).

use std::path::PathBuf;

/// Resolved TermOS directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/termos` (default `~/.config/termos`).
    pub config_dir: PathBuf,
    /// `$XDG_STATE_HOME/termos` (default `~/.local/state/termos`).
    pub state_dir: PathBuf,
}

impl Paths {
    /// Resolves paths using `get` to read environment variables. Relative
    /// XDG values are ignored, as the spec requires.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let home = get("HOME")
            .filter(|h| !h.is_empty())
            .map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let xdg = |var: &str, fallback: &[&str]| -> PathBuf {
            match get(var).map(PathBuf::from) {
                Some(p) if p.is_absolute() => p,
                _ => fallback
                    .iter()
                    .fold(home.clone(), |acc, part| acc.join(part)),
            }
        };
        Self {
            config_dir: xdg("XDG_CONFIG_HOME", &[".config"]).join("termos"),
            state_dir: xdg("XDG_STATE_HOME", &[".local", "state"]).join("termos"),
        }
    }

    /// Paths from the real environment.
    pub fn detect() -> Self {
        Self::from_env(|k| std::env::var(k).ok())
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.state_dir.join("log")
    }

    /// Created once the first-run tutorial has been shown.
    pub fn tutorial_marker(&self) -> PathBuf {
        self.state_dir.join("tutorial-shown")
    }
}

// TODO(M3): runtime directory for server sockets ($XDG_RUNTIME_DIR/termos, 0700, with a per-uid fallback).

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(vars: &[(&str, &str)]) -> Paths {
        Paths::from_env(|k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    #[test]
    fn defaults_under_home() {
        let p = paths(&[("HOME", "/home/u")]);
        assert_eq!(
            p.config_file(),
            PathBuf::from("/home/u/.config/termos/config.toml")
        );
        assert_eq!(
            p.log_dir(),
            PathBuf::from("/home/u/.local/state/termos/log")
        );
    }

    #[test]
    fn xdg_overrides_and_relative_values_ignored() {
        let p = paths(&[
            ("HOME", "/home/u"),
            ("XDG_CONFIG_HOME", "/cfg"),
            ("XDG_STATE_HOME", "relative/state"),
        ]);
        assert_eq!(p.config_dir, PathBuf::from("/cfg/termos"));
        assert_eq!(p.state_dir, PathBuf::from("/home/u/.local/state/termos"));
    }
}
