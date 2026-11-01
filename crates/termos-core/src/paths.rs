// SPDX-License-Identifier: GPL-3.0-or-later
//! Well-known locations, following the XDG base directory spec on every
//! platform (like tmux and most terminal tools, also on macOS).

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Resolved TermOS directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/termos` (default `~/.config/termos`).
    pub config_dir: PathBuf,
    /// `$XDG_STATE_HOME/termos` (default `~/.local/state/termos`).
    pub state_dir: PathBuf,
    /// Where server sockets live: `$XDG_RUNTIME_DIR/termos`, or
    /// `$TMPDIR/termos-<uid>` (default `/tmp`) without a runtime dir.
    pub runtime_dir: PathBuf,
}

/// The current user's uid.
pub fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
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
        let runtime_dir = match get("XDG_RUNTIME_DIR").map(PathBuf::from) {
            Some(p) if p.is_absolute() => p.join("termos"),
            _ => {
                let tmp = get("TMPDIR")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .unwrap_or_else(|| PathBuf::from("/tmp"));
                tmp.join(format!("termos-{}", current_uid()))
            }
        };
        Self {
            config_dir: xdg("XDG_CONFIG_HOME", &[".config"]).join("termos"),
            state_dir: xdg("XDG_STATE_HOME", &[".local", "state"]).join("termos"),
            runtime_dir,
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

    /// The socket of server `name` (`-L name`; default `default`).
    pub fn socket_path(&self, name: &str) -> PathBuf {
        self.runtime_dir.join(format!("{name}.sock"))
    }

    /// Session snapshots of server `name`.
    pub fn sessions_dir(&self, server: &str) -> PathBuf {
        self.state_dir.join("sessions").join(server)
    }

    /// Installed plugins, one directory each.
    pub fn plugins_dir(&self) -> PathBuf {
        self.config_dir.join("plugins")
    }
}

/// Creates `dir` (and parents) if needed and makes sure only the current
/// user can use it: owned by us and mode 0700. A directory owned by someone
/// else is an error (it could be a trap planted in a shared /tmp).
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            // Another process created it first; verify it below.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    if meta.uid() != current_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} belongs to another user (uid {}); refusing to use it",
                dir.display(),
                meta.uid()
            ),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

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
    fn runtime_dir_and_sockets() {
        let p = paths(&[("HOME", "/home/u"), ("XDG_RUNTIME_DIR", "/run/user/1000")]);
        assert_eq!(p.runtime_dir, PathBuf::from("/run/user/1000/termos"));
        assert_eq!(
            p.socket_path("work"),
            PathBuf::from("/run/user/1000/termos/work.sock")
        );
        let p = paths(&[("HOME", "/home/u"), ("TMPDIR", "/var/tmp")]);
        assert_eq!(
            p.runtime_dir,
            PathBuf::from(format!("/var/tmp/termos-{}", current_uid()))
        );
        assert_eq!(
            p.sessions_dir("default"),
            PathBuf::from("/home/u/.local/state/termos/sessions/default")
        );
    }

    #[test]
    fn private_dirs_are_created_and_tightened() {
        let base = tempfile::tempdir().expect("tempdir");
        let dir = base.path().join("a").join("termos");
        ensure_private_dir(&dir).expect("created");
        let mode = std::fs::metadata(&dir).expect("meta").mode() & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        ensure_private_dir(&dir).expect("tightened");
        assert_eq!(std::fs::metadata(&dir).expect("meta").mode() & 0o777, 0o700);
        let file = base.path().join("file");
        std::fs::write(&file, "x").expect("write");
        assert!(ensure_private_dir(&file).is_err());
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
