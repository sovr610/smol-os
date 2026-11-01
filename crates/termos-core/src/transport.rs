// SPDX-License-Identifier: GPL-3.0-or-later
//! The link between a frontend and the server.
//!
//! The terminal client always talks to a [`ClientEnd`] (a pair of channels).
//! [`in_process`] connects it directly to a server in the same process
//! (tests); [`connect_frontend`] bridges it to a server over the session
//! socket (DECISIONS.md D-007, D-031). [`control_call`] makes one JSON-RPC
//! call, as `termos msg` does.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::local::{LocalRead, LocalStream, LocalWrite};
use crate::proto::{ClientMsg, ServerMsg};
use crate::wire::{
    self, ConnKind, FrameDecoder, Hello, HelloReply, MAX_LINE, Message, Request, RpcError,
};

/// The frontend's half: send input, receive frames.
#[derive(Debug)]
pub struct ClientEnd {
    pub tx: mpsc::UnboundedSender<ClientMsg>,
    pub rx: mpsc::UnboundedReceiver<ServerMsg>,
}

/// The server's half for one in-process client: receive input, send frames.
#[derive(Debug)]
pub struct ServerEnd {
    pub rx: mpsc::UnboundedReceiver<ClientMsg>,
    pub tx: mpsc::UnboundedSender<ServerMsg>,
}

/// A connected in-process pair.
pub fn in_process() -> (ClientEnd, ServerEnd) {
    let (client_tx, server_rx) = mpsc::unbounded_channel();
    let (server_tx, client_rx) = mpsc::unbounded_channel();
    (
        ClientEnd {
            tx: client_tx,
            rx: client_rx,
        },
        ServerEnd {
            rx: server_rx,
            tx: server_tx,
        },
    )
}

/// Why connecting to a server failed.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// Nothing listens on the socket (no server running).
    #[error("no TermOS server is running at {}", .0.display())]
    NoServer(PathBuf),
    #[error("cannot talk to the server: {0}")]
    Io(#[from] io::Error),
    /// The server answered with a JSON-RPC error (refused, version mismatch…).
    #[error("{}", .0.message)]
    Rpc(RpcError),
    #[error("the server sent something unexpected: {0}")]
    Protocol(String),
}

/// Reads one `\n`-terminated line of at most [`MAX_LINE`] bytes. `Ok(None)`
/// at end of stream.
pub async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = (&mut *r)
        .take(u64::try_from(MAX_LINE).unwrap_or(u64::MAX) + 1)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > MAX_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    String::from_utf8(buf)
        .map(|s| Some(s.trim_end_matches(['\r', '\n']).to_owned()))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "line is not UTF-8"))
}

/// Connects to the server's endpoint and performs the `hello` handshake.
pub async fn handshake(
    path: &Path,
    hello: &Hello,
) -> Result<(HelloReply, BufReader<LocalRead>, LocalWrite), ConnectError> {
    let stream = match LocalStream::connect(path).await {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(ConnectError::NoServer(path.to_owned()));
        }
        Err(e) => return Err(e.into()),
    };
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let req = Request::new(0, "hello", serde_json::to_value(hello).ok());
    write.write_all(wire::to_line(&req).as_bytes()).await?;
    let line = read_line(&mut reader)
        .await?
        .ok_or_else(|| ConnectError::Protocol("the server closed the connection".into()))?;
    match wire::parse_message(&line) {
        Ok(Message::Response(resp)) => {
            let value = resp.into_result().map_err(ConnectError::Rpc)?;
            let reply: HelloReply =
                serde_json::from_value(value).map_err(|e| ConnectError::Protocol(e.to_string()))?;
            Ok((reply, reader, write))
        }
        Ok(Message::Request(r)) => Err(ConnectError::Protocol(format!(
            "expected a reply to hello, got `{}`",
            r.method
        ))),
        Err(e) => Err(ConnectError::Protocol(e.message)),
    }
}

/// Connects a frontend and bridges the socket to a [`ClientEnd`]: one task
/// decodes server frames into `rx`, another encodes `tx` into frames.
/// Must be called inside a tokio runtime.
pub async fn connect_frontend(
    path: &Path,
    hello: &Hello,
) -> Result<(HelloReply, ClientEnd), ConnectError> {
    let (reply, mut reader, mut write) = handshake(path, hello).await?;
    let (client_tx, mut server_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (server_tx, client_rx) = mpsc::unbounded_channel::<ServerMsg>();
    tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            loop {
                match decoder.next_frame::<ServerMsg>() {
                    Ok(Some(msg)) => {
                        if server_tx.send(msg).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("bad frame from server: {e}");
                        return;
                    }
                }
            }
            // Reading through the BufReader also drains anything it buffered
            // during the handshake.
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => decoder.push(&buf[..n]),
            }
        }
    });
    tokio::spawn(async move {
        while let Some(msg) = server_rx.recv().await {
            let Ok(bytes) = wire::encode_frame(&msg) else {
                continue;
            };
            if write.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });
    Ok((
        reply,
        ClientEnd {
            tx: client_tx,
            rx: client_rx,
        },
    ))
}

/// Makes one control call (`termos msg`): hello as a control client, one
/// request, one response.
pub async fn control_call(
    path: &Path,
    method: &str,
    params: Option<Value>,
) -> Result<Value, ConnectError> {
    let (_, mut reader, mut write) = handshake(path, &Hello::new(ConnKind::Control)).await?;
    let req = Request::new(1, method, params);
    write.write_all(wire::to_line(&req).as_bytes()).await?;
    let line = read_line(&mut reader)
        .await?
        .ok_or_else(|| ConnectError::Protocol("the server closed the connection".into()))?;
    match wire::parse_message(&line) {
        Ok(Message::Response(resp)) => resp.into_result().map_err(ConnectError::Rpc),
        Ok(Message::Request(r)) => Err(ConnectError::Protocol(format!(
            "expected a response, got `{}`",
            r.method
        ))),
        Err(e) => Err(ConnectError::Protocol(e.message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_flow_both_ways() {
        let (mut client, mut server) = in_process();
        client
            .tx
            .send(ClientMsg::Focus(true))
            .expect("server end alive");
        assert_eq!(server.rx.recv().await, Some(ClientMsg::Focus(true)));
        server.tx.send(ServerMsg::Bell).expect("client end alive");
        assert_eq!(client.rx.recv().await, Some(ServerMsg::Bell));
        drop(server);
        assert_eq!(client.rx.recv().await, None);
    }

    #[tokio::test]
    async fn missing_server_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("none.sock");
        let err = control_call(&path, "server.info", None)
            .await
            .expect_err("no server");
        assert!(matches!(err, ConnectError::NoServer(_)), "{err:?}");
    }

    #[tokio::test]
    async fn line_reader_limits_and_eof() {
        let data: &[u8] = b"one\r\ntwo\nlast";
        let mut r = BufReader::new(data);
        assert_eq!(
            read_line(&mut r).await.ok().flatten().as_deref(),
            Some("one")
        );
        assert_eq!(
            read_line(&mut r).await.ok().flatten().as_deref(),
            Some("two")
        );
        assert_eq!(
            read_line(&mut r).await.ok().flatten().as_deref(),
            Some("last")
        );
        assert_eq!(read_line(&mut r).await.ok().flatten(), None);
    }
}
