// SPDX-License-Identifier: GPL-3.0-or-later
//! A mock HTTP server for provider contract tests: it records every
//! request and answers from a closure, streaming bodies in pieces so the
//! clients' incremental parsing is exercised. CI never uses live keys.

#![allow(dead_code, unreachable_pub)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A request as the server saw it.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("a JSON body")
    }
}

/// How to answer.
pub enum Answer {
    Json(u16, String),
    /// An event stream, written in these pieces.
    Stream(Vec<String>),
}

impl Answer {
    /// An event stream from a fixture, cut into pieces of `size` bytes
    /// (cuts can fall inside lines and characters).
    pub fn stream_in_pieces(text: &str, size: usize) -> Self {
        let bytes = text.as_bytes();
        let pieces = bytes.chunks(size.max(1)).map(as_latin1).collect();
        Self::Stream(pieces)
    }
}

/// Pieces travel as bytes; they are kept as Latin-1-mapped strings so a cut
/// inside a UTF-8 character stays a cut.
fn as_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

type Responder = dyn Fn(&Recorded) -> Answer + Send + Sync;

pub struct MockServer {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    pub async fn start(respond: impl Fn(&Recorded) -> Answer + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let respond: Arc<Responder> = Arc::new(respond);
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let respond = Arc::clone(&respond);
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let Some(req) = read_request(&mut sock).await else {
                        return;
                    };
                    recorded.lock().expect("lock").push(req.clone());
                    match respond(&req) {
                        Answer::Json(status, body) => {
                            let head = format!(
                                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = sock.write_all(head.as_bytes()).await;
                            let _ = sock.write_all(body.as_bytes()).await;
                        }
                        Answer::Stream(pieces) => {
                            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
                            let _ = sock.write_all(head.as_bytes()).await;
                            for p in pieces {
                                let bytes: Vec<u8> = p.chars().map(|c| c as u32 as u8).collect();
                                if sock.write_all(&bytes).await.is_err() {
                                    return;
                                }
                                let _ = sock.flush().await;
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            }
                        }
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self {
            base: format!("http://{addr}"),
            requests,
        }
    }

    /// Starts a server on a thread of its own (for tests whose code under
    /// test runs its own runtime).
    pub fn start_on_thread(respond: impl Fn(&Recorded) -> Answer + Send + Sync + 'static) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async move {
                let server = MockServer::start(respond).await;
                tx.send(server).expect("send");
                std::future::pending::<()>().await;
            });
        });
        rx.recv().expect("the server started")
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("lock").clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}
