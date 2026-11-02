// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the local model router listens (D-068).
//!
//! `termos models serve` writes `<runtime>/models.json` (mode 0600, in the
//! owner-only runtime folder) with its address and a per-start token, and
//! removes it when it stops. Agents' `local` provider reads it, so neither
//! side depends on the other's crate.

use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;

/// The router's address and token.
#[derive(Clone)]
pub struct ModelsEndpoint {
    /// `http://127.0.0.1:<port>/v1`.
    pub url: String,
    /// Sent as a bearer token. Per start, 128 bits from the OS generator.
    pub token: SecretString,
    pub pid: u32,
}

/// The file's form.
#[derive(Serialize, Deserialize)]
struct File<'a> {
    url: std::borrow::Cow<'a, str>,
    token: std::borrow::Cow<'a, str>,
    pid: u32,
}

impl std::fmt::Debug for ModelsEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelsEndpoint")
            .field("url", &self.url)
            .field("token", &"[redacted]")
            .field("pid", &self.pid)
            .finish()
    }
}

/// The endpoint file.
pub fn path(paths: &Paths) -> PathBuf {
    paths.runtime_dir.join("models.json")
}

/// Reads the endpoint file, if a router wrote one.
pub fn read(paths: &Paths) -> Option<ModelsEndpoint> {
    let bytes = std::fs::read(path(paths)).ok()?;
    let f: File<'_> = serde_json::from_slice(&bytes).ok()?;
    Some(ModelsEndpoint {
        url: f.url.into_owned(),
        token: SecretString::from(f.token.into_owned()),
        pid: f.pid,
    })
}

/// Writes the endpoint file, readable only by us.
pub fn write(paths: &Paths, endpoint: &ModelsEndpoint) -> std::io::Result<()> {
    let file = path(paths);
    let dir = file
        .parent()
        .ok_or_else(|| std::io::Error::other("no runtime folder"))?;
    create_private_dir(dir)?;
    let tmp = dir.join(format!(".models.json.{}", std::process::id()));
    let json = serde_json::to_vec(&File {
        url: endpoint.url.as_str().into(),
        token: endpoint.token.expose_secret().into(),
        pid: endpoint.pid,
    })
    .map_err(std::io::Error::other)?;
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&json)?;
    }
    std::fs::rename(&tmp, &file)
}

/// Removes the endpoint file if it is ours (`pid`).
pub fn remove(paths: &Paths, pid: u32) {
    if read(paths).is_some_and(|e| e.pid == pid) {
        let _ = std::fs::remove_file(path(paths));
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_only_ours_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            config_dir: dir.path().join("c"),
            state_dir: dir.path().join("s"),
            runtime_dir: dir.path().join("run"),
            data_dir: dir.path().join("d"),
        };
        assert!(read(&paths).is_none());
        let e = ModelsEndpoint {
            url: "http://127.0.0.1:1/v1".into(),
            token: SecretString::from("secret-token"),
            pid: 7,
        };
        write(&paths, &e).expect("write");
        let back = read(&paths).expect("read");
        assert_eq!(
            (back.url.as_str(), back.token.expose_secret(), back.pid),
            (e.url.as_str(), "secret-token", 7)
        );
        assert!(!format!("{e:?}").contains("secret-token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path(&paths))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        remove(&paths, 8);
        assert!(read(&paths).is_some(), "another router's file stays");
        remove(&paths, 7);
        assert!(read(&paths).is_none());
    }
}
