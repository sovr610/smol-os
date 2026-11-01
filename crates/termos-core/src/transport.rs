// SPDX-License-Identifier: GPL-3.0-or-later
//! The link between a frontend and the server.
//!
//! The server always talks to channel endpoints. In M1 the frontend holds the
//! other ends in the same process; in M3 a socket adapter task bridges a Unix
//! socket to the same channels (DECISIONS.md D-007, D-026), so the desktop
//! code does not change.

use tokio::sync::mpsc;

use crate::proto::{ClientMsg, ServerMsg};

/// The frontend's half: send input, receive frames.
#[derive(Debug)]
pub struct ClientEnd {
    pub tx: mpsc::UnboundedSender<ClientMsg>,
    pub rx: mpsc::UnboundedReceiver<ServerMsg>,
}

/// The server's half for one client: receive input, send frames.
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

// TODO(M3): Unix-socket adapter (postcard frames, SO_PEERCRED/getpeereid checks, handshake versioning).

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
}
