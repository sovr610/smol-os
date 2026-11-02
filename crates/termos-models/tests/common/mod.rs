// SPDX-License-Identifier: GPL-3.0-or-later
//! A mock Hugging Face Hub: the revision and tree APIs, file downloads
//! with HTTP ranges, and switches to cut a transfer short or serve the
//! wrong bytes.

#![allow(dead_code, unreachable_pub)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use sha2::Digest as _;

pub const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Default)]
pub struct State {
    /// repo → path → content.
    pub files: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    /// Paths whose next download stops after this many bytes.
    pub cut_after: BTreeMap<String, usize>,
    /// Paths served with other bytes than listed.
    pub corrupt: Vec<String>,
    /// Every request line (method and path) and its Range header.
    pub log: Vec<(String, Option<String>)>,
    /// Paths that need this bearer token.
    pub token: Option<String>,
}

pub struct MockHub {
    pub base: String,
    pub state: Arc<Mutex<State>>,
}

pub fn sha256(b: &[u8]) -> String {
    sha2::Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

pub fn git_oid(b: &[u8]) -> String {
    use sha1::Digest as _;
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", b.len()).as_bytes());
    h.update(b);
    h.finalize().iter().map(|x| format!("{x:02x}")).collect()
}

impl MockHub {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let state = Arc::new(Mutex::new(State::default()));
        let st = Arc::clone(&state);
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let st = Arc::clone(&st);
                std::thread::spawn(move || serve(s, &st));
            }
        });
        Self { base, state }
    }

    pub fn add(&self, repo: &str, path: &str, content: &[u8]) {
        self.state
            .lock()
            .expect("lock")
            .files
            .entry(repo.to_owned())
            .or_default()
            .insert(path.to_owned(), content.to_vec());
    }
}

fn reply(s: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &[u8]) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
}

fn serve(mut s: TcpStream, st: &Mutex<State>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let path = first.split(' ').nth(1).unwrap_or("").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let header = |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let range = header("range");
    let mut st = st.lock().expect("lock");
    st.log.push((path.clone(), range.clone()));
    if let Some(t) = &st.token
        && header("authorization").as_deref() != Some(&format!("Bearer {t}"))
    {
        reply(
            &mut s,
            "401 Unauthorized",
            &[],
            br#"{"error":"Access to model is restricted (gated)"}"#,
        );
        return;
    }
    let (path, _query) = path.split_once('?').unwrap_or((&path, ""));
    if let Some(rest) = path.strip_prefix("/api/models/") {
        if let Some((repo, _rev)) = rest.split_once("/revision/") {
            if st.files.contains_key(repo) {
                reply(
                    &mut s,
                    "200 OK",
                    &[],
                    format!(r#"{{"id":"{repo}","sha":"{COMMIT}"}}"#).as_bytes(),
                );
            } else {
                reply(
                    &mut s,
                    "404 Not Found",
                    &[],
                    br#"{"error":"Repository not found"}"#,
                );
            }
            return;
        }
        if let Some((repo, _)) = rest.split_once("/tree/") {
            let Some(files) = st.files.get(repo) else {
                reply(
                    &mut s,
                    "404 Not Found",
                    &[],
                    br#"{"error":"Repository not found"}"#,
                );
                return;
            };
            let items: Vec<serde_json::Value> = files
                .iter()
                .map(|(p, c)| {
                    let mut v = serde_json::json!({"type": "file", "path": p, "size": c.len(), "oid": git_oid(c)});
                    if p.ends_with(".gguf") || p.ends_with(".safetensors") {
                        v["lfs"] = serde_json::json!({"oid": sha256(c), "size": c.len(), "pointerSize": 134});
                        v["oid"] = serde_json::json!("1111111111111111111111111111111111111111");
                    }
                    v
                })
                .collect();
            reply(
                &mut s,
                "200 OK",
                &[],
                serde_json::to_string(&items).expect("json").as_bytes(),
            );
            return;
        }
        if path == "/api/models" {
            let found: Vec<serde_json::Value> = st
                .files
                .keys()
                .map(|r| serde_json::json!({"id": r, "downloads": 5}))
                .collect();
            reply(
                &mut s,
                "200 OK",
                &[],
                serde_json::to_string(&found).expect("json").as_bytes(),
            );
            return;
        }
    }
    // /<org>/<name>/resolve/<commit>/<path>
    let Some((repo, file)) = path.trim_start_matches('/').split_once("/resolve/") else {
        reply(&mut s, "404 Not Found", &[], b"");
        return;
    };
    let file = file
        .split_once('/')
        .map(|(_, f)| f)
        .unwrap_or("")
        .replace("%20", " ");
    let Some(mut content) = st.files.get(repo).and_then(|f| f.get(&file)).cloned() else {
        reply(&mut s, "404 Not Found", &[], b"");
        return;
    };
    if st.corrupt.contains(&file)
        && let Some(b) = content.last_mut()
    {
        *b ^= 0xff;
    }
    let start: usize = range
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split('-').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let end: usize = range
        .as_deref()
        .and_then(|r| r.split('-').nth(1))
        .and_then(|n| n.parse::<usize>().ok())
        .map_or(content.len(), |e| (e + 1).min(content.len()));
    if start >= content.len() && !content.is_empty() {
        reply(&mut s, "416 Range Not Satisfiable", &[], b"");
        return;
    }
    let body = &content[start..end];
    let status = if range.is_some() {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let headers = [(
        "content-range",
        format!("bytes {start}-{}/{}", end.saturating_sub(1), content.len()),
    )];
    if let Some(cut) = st.cut_after.remove(&file) {
        // Promise the whole body, send part of it, hang up.
        let mut head = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        head.push_str("");
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(&body[..cut.min(body.len())]);
        let _ = s.flush();
        drop(st);
        let _ = s.shutdown(std::net::Shutdown::Both);
        return;
    }
    reply(&mut s, status, &headers, body);
}
