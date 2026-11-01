// SPDX-License-Identifier: GPL-3.0-or-later
//! Well-known locations.
//!
//! On Unix they follow the XDG base directory spec (like tmux and most
//! terminal tools, also on macOS). On Windows the defaults are
//! `%APPDATA%\termos` (config) and `%LOCALAPPDATA%\termos` (state), and
//! servers listen on named pipes. `XDG_*` variables override the defaults
//! on every platform, which keeps tests isolated.

use std::io;
use std::path::{Path, PathBuf};

/// Resolved TermOS directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/termos` (default `~/.config/termos`; Windows
    /// `%APPDATA%\termos`).
    pub config_dir: PathBuf,
    /// `$XDG_STATE_HOME/termos` (default `~/.local/state/termos`; Windows
    /// `%LOCALAPPDATA%\termos`).
    pub state_dir: PathBuf,
    /// Where server sockets live: `$XDG_RUNTIME_DIR/termos`, or
    /// `$TMPDIR/termos-<uid>` (default `/tmp`) without a runtime dir. On
    /// Windows it is never created: it only names the server's pipes.
    pub runtime_dir: PathBuf,
}

/// The current user's uid.
#[cfg(unix)]
pub fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Default directories before the `termos` component.
struct Defaults {
    config: PathBuf,
    state: PathBuf,
    /// Complete (already ends in a TermOS-specific component).
    runtime: PathBuf,
}

#[cfg(unix)]
fn defaults(get: &dyn Fn(&str) -> Option<String>) -> Defaults {
    let home = get("HOME")
        .filter(|h| !h.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let tmp = get("TMPDIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Defaults {
        config: home.join(".config"),
        state: home.join(".local").join("state"),
        runtime: tmp.join(format!("termos-{}", current_uid())),
    }
}

#[cfg(windows)]
fn defaults(get: &dyn Fn(&str) -> Option<String>) -> Defaults {
    let abs = |var: &str| get(var).map(PathBuf::from).filter(|p| p.is_absolute());
    let profile = abs("USERPROFILE").unwrap_or_else(|| PathBuf::from(r"C:\"));
    let roaming = abs("APPDATA").unwrap_or_else(|| profile.join("AppData").join("Roaming"));
    let local = abs("LOCALAPPDATA").unwrap_or_else(|| profile.join("AppData").join("Local"));
    Defaults {
        config: roaming,
        runtime: local.join("termos").join("run"),
        state: local,
    }
}

impl Paths {
    /// Resolves paths using `get` to read environment variables. Relative
    /// XDG values are ignored, as the spec requires.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let defaults = defaults(&get);
        let abs = |var: &str| get(var).map(PathBuf::from).filter(|p| p.is_absolute());
        Self {
            config_dir: abs("XDG_CONFIG_HOME")
                .unwrap_or(defaults.config)
                .join("termos"),
            state_dir: abs("XDG_STATE_HOME")
                .unwrap_or(defaults.state)
                .join("termos"),
            runtime_dir: abs("XDG_RUNTIME_DIR").map_or(defaults.runtime, |p| p.join("termos")),
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

    /// The endpoint of server `name` (`-L name`; default `default`): a
    /// socket in the runtime directory on Unix, a named pipe on Windows.
    pub fn socket_path(&self, name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(windows_pipe_name(&self.runtime_dir, name))
        } else {
            self.runtime_dir.join(format!("{name}.sock"))
        }
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

/// FNV-1a, 64 bits: a hash that never changes between Rust versions (so
/// an old server and a new client agree on pipe names).
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The Windows pipe of server `name`:
/// `\\.\pipe\termos-<hash of the runtime dir>-<name>`. The runtime
/// directory is per user (it is under the user's profile) and changes with
/// `XDG_RUNTIME_DIR`, so test servers do not meet real ones.
pub fn windows_pipe_name(runtime_dir: &Path, name: &str) -> String {
    let key = runtime_dir.to_string_lossy().to_lowercase();
    format!(r"\\.\pipe\termos-{:016x}-{name}", fnv1a64(key.as_bytes()))
}

/// The server name of an endpoint: the socket file's stem, or the name in
/// a TermOS pipe name.
pub fn server_name(endpoint: &Path) -> Option<String> {
    let text = endpoint.to_string_lossy();
    if let Some(pipe) = text.strip_prefix(r"\\.\pipe\") {
        let (hash, name) = pipe.strip_prefix("termos-")?.split_once('-')?;
        let is_hash = hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        return is_hash.then(|| name.to_owned());
    }
    endpoint.file_stem()?.to_str().map(str::to_owned)
}

/// Creates `dir` (and parents) if needed and makes sure only the current
/// user can use it: owned by us and mode 0700. A directory owned by someone
/// else is an error (it could be a trap planted in a shared /tmp).
#[cfg(unix)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
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

/// Creates `dir` (and parents) if needed. On Windows the directories live
/// under the user's profile, whose ACLs already keep other users out.
#[cfg(windows)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if std::fs::symlink_metadata(dir)?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )))
    }
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
    fn pipe_names_are_stable_and_isolated() {
        // FNV-1a test vectors.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        let a = windows_pipe_name(Path::new(r"C:\Users\u\AppData\Local\termos\run"), "default");
        let b = windows_pipe_name(Path::new(r"c:\users\U\appdata\local\termos\run"), "default");
        assert_eq!(a, b, "Windows paths are case-insensitive");
        assert!(
            a.starts_with(r"\\.\pipe\termos-") && a.ends_with("-default"),
            "{a}"
        );
        assert_ne!(a, windows_pipe_name(Path::new(r"C:\tmp\t\run"), "default"));
        assert_eq!(server_name(Path::new(&a)).as_deref(), Some("default"));
        let dashed = windows_pipe_name(Path::new(r"C:\x"), "my-server");
        assert_eq!(
            server_name(Path::new(&dashed)).as_deref(),
            Some("my-server")
        );
        assert_eq!(
            server_name(Path::new("/run/user/1000/termos/work.sock")).as_deref(),
            Some("work")
        );
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn private_dirs_are_created_and_tightened() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
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

    #[cfg(unix)]
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

    #[cfg(windows)]
    #[test]
    fn windows_defaults_and_overrides() {
        let p = paths(&[
            ("USERPROFILE", r"C:\Users\u"),
            ("APPDATA", r"C:\Users\u\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
        ]);
        assert_eq!(
            p.config_dir,
            PathBuf::from(r"C:\Users\u\AppData\Roaming\termos")
        );
        assert_eq!(
            p.state_dir,
            PathBuf::from(r"C:\Users\u\AppData\Local\termos")
        );
        assert_eq!(
            p.runtime_dir,
            PathBuf::from(r"C:\Users\u\AppData\Local\termos\run")
        );
        assert!(
            p.socket_path("default")
                .to_string_lossy()
                .starts_with(r"\\.\pipe\termos-")
        );
        let p = paths(&[
            ("USERPROFILE", r"C:\Users\u"),
            ("XDG_CONFIG_HOME", r"D:\cfg"),
        ]);
        assert_eq!(p.config_dir, PathBuf::from(r"D:\cfg\termos"));
        assert_eq!(
            p.state_dir,
            PathBuf::from(r"C:\Users\u\AppData\Local\termos")
        );
        let dir = tempfile::tempdir().expect("tempdir");
        ensure_private_dir(&dir.path().join("a").join("b")).expect("created");
    }
}
