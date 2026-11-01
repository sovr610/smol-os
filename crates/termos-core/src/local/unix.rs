// SPDX-License-Identifier: GPL-3.0-or-later
//! Unix-domain sockets: a 0600 socket in a 0700, owner-checked directory,
//! and the peer's uid from the kernel (SO_PEERCRED / getpeereid).

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use crate::paths::ensure_private_dir;

pub(super) type Peer = u32;
pub(super) type Read = OwnedReadHalf;
pub(super) type Write = OwnedWriteHalf;

pub(super) fn current_peer() -> io::Result<Peer> {
    Ok(rustix::process::getuid().as_raw())
}

pub(super) fn nobody() -> Peer {
    // (uid_t)-1 is not a valid uid.
    u32::MAX
}

#[derive(Debug)]
pub(super) struct Listener(UnixListener);

impl Listener {
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            ensure_private_dir(dir)?;
        }
        if path.exists() {
            if probe(path) {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("a TermOS server is already running at {}", path.display()),
                ));
            }
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }

    pub(super) async fn accept(&mut self) -> io::Result<Stream> {
        self.0.accept().await.map(|(s, _)| Stream(s))
    }
}

#[derive(Debug)]
pub(super) struct Stream(UnixStream);

impl Stream {
    pub(super) async fn connect(path: &Path) -> io::Result<Self> {
        UnixStream::connect(path).await.map(Self)
    }

    pub(super) fn peer(&self) -> io::Result<Peer> {
        self.0.peer_cred().map(|c| c.uid())
    }

    pub(super) fn into_split(self) -> (Read, Write) {
        self.0.into_split()
    }
}

pub(super) fn probe(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

pub(super) fn exists(path: &Path) -> bool {
    path.exists()
}

pub(super) fn remove_endpoint(path: &Path) {
    let _ = std::fs::remove_file(path);
}
