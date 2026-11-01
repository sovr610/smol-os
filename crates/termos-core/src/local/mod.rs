// SPDX-License-Identifier: GPL-3.0-or-later
//! Local IPC endpoints: the session socket on Unix, a named pipe on
//! Windows (DECISIONS.md D-031, D-036).
//!
//! The rest of TermOS sees only these types:
//!
//! - [`LocalListener`]: the server side, bound for the current user only;
//! - [`LocalStream`]: one connection, split into [`LocalRead`] and
//!   [`LocalWrite`] halves (tokio `AsyncRead`/`AsyncWrite`);
//! - [`PeerId`]: who is on the other end (a uid on Unix, a SID on
//!   Windows), checked by the server before it talks to anyone;
//! - [`probe`]: whether a server answers at an endpoint (for the CLI).
//!
//! An endpoint is a path: a socket file on Unix, `\\.\pipe\…` on Windows.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;

/// A bound endpoint that accepts connections.
#[derive(Debug)]
pub struct LocalListener {
    inner: sys::Listener,
    endpoint: PathBuf,
}

impl LocalListener {
    /// Binds `endpoint` so that only the current user can connect. A stale
    /// endpoint left by a crashed server is replaced; a live one is refused
    /// with [`io::ErrorKind::AddrInUse`]. Must be called inside a tokio
    /// runtime.
    pub fn bind(endpoint: &Path) -> io::Result<Self> {
        Ok(Self {
            inner: sys::Listener::bind(endpoint)?,
            endpoint: endpoint.to_owned(),
        })
    }

    /// Waits for the next connection.
    pub async fn accept(&mut self) -> io::Result<LocalStream> {
        self.inner.accept().await.map(|inner| LocalStream { inner })
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }
}

/// One connection to or from a server.
#[derive(Debug)]
pub struct LocalStream {
    inner: sys::Stream,
}

impl LocalStream {
    /// Connects to the server at `endpoint`. A missing server is reported
    /// as [`io::ErrorKind::NotFound`] or [`io::ErrorKind::ConnectionRefused`].
    pub async fn connect(endpoint: &Path) -> io::Result<Self> {
        sys::Stream::connect(endpoint)
            .await
            .map(|inner| Self { inner })
    }

    /// Who is on the other end.
    pub fn peer(&self) -> io::Result<PeerId> {
        self.inner.peer().map(PeerId)
    }

    pub fn into_split(self) -> (LocalRead, LocalWrite) {
        let (r, w) = self.inner.into_split();
        (LocalRead(r), LocalWrite(w))
    }
}

/// The reading half of a [`LocalStream`].
#[derive(Debug)]
pub struct LocalRead(sys::Read);

/// The writing half of a [`LocalStream`].
#[derive(Debug)]
pub struct LocalWrite(sys::Write);

impl AsyncRead for LocalRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for LocalWrite {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// The identity of a local user: a uid on Unix, a SID on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerId(sys::Peer);

impl PeerId {
    /// The user running this process.
    pub fn current() -> io::Result<Self> {
        sys::current_peer().map(Self)
    }

    /// An identity no real user has (tests use it to check refusals).
    pub fn nobody() -> Self {
        Self(sys::nobody())
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Whether a server answers at `endpoint` (blocking; for the CLI).
pub fn probe(endpoint: &Path) -> bool {
    sys::probe(endpoint)
}

/// Whether the endpoint still exists (a socket file on Unix; a pipe that
/// answers on Windows).
pub fn exists(endpoint: &Path) -> bool {
    sys::exists(endpoint)
}

/// Removes what [`LocalListener::bind`] left behind (the socket file on
/// Unix; nothing on Windows, where pipes vanish with their server).
pub fn remove_endpoint(endpoint: &Path) {
    sys::remove_endpoint(endpoint);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn connect_talk_and_identify() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = crate::paths::Paths {
            config_dir: dir.path().join("c"),
            state_dir: dir.path().join("s"),
            runtime_dir: dir.path().join("run"),
        }
        .socket_path("t");
        assert!(!probe(&endpoint));
        let mut listener = LocalListener::bind(&endpoint).expect("bind");
        assert!(probe(&endpoint), "a bound endpoint answers");
        assert_eq!(
            LocalListener::bind(&endpoint)
                .expect_err("a second server is refused")
                .kind(),
            io::ErrorKind::AddrInUse
        );
        let server = tokio::spawn(async move {
            // The probes above connected and hung up; skip those.
            loop {
                let stream = listener.accept().await.expect("accept");
                let peer = stream.peer().expect("peer");
                let (mut r, mut w) = stream.into_split();
                let mut buf = [0u8; 4];
                if r.read_exact(&mut buf).await.is_err() {
                    continue;
                }
                w.write_all(b"pong").await.expect("write");
                return (peer, buf);
            }
        });
        let stream = LocalStream::connect(&endpoint).await.expect("connect");
        let (mut r, mut w) = stream.into_split();
        w.write_all(b"ping").await.expect("write");
        let mut buf = [0u8; 4];
        r.read_exact(&mut buf).await.expect("read");
        assert_eq!(&buf, b"pong");
        let (peer, got) = server.await.expect("server task");
        assert_eq!(&got, b"ping");
        assert_eq!(peer, PeerId::current().expect("current user"));
        assert_ne!(peer, PeerId::nobody());
        remove_endpoint(&endpoint);
        assert!(!exists(&endpoint));
        let err = LocalStream::connect(&endpoint)
            .await
            .expect_err("no server");
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ),
            "{err:?}"
        );
    }
}
