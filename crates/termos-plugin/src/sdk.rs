// SPDX-License-Identifier: GPL-3.0-or-later
//! A blocking plugin client over the session socket (a Windows named pipe
//! on Windows).

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::conn::Conn;
use crate::proto::{
    CommandInvoked, CommandRegister, Line, NotifySend, PROTOCOL_VERSION, StatusSet, WindowDraw,
    WindowKey, WindowMouse, WindowOpen, WindowRef, WindowResize, env, methods,
};

/// What can go wrong talking to the server.
#[derive(Debug)]
pub enum Error {
    /// A variable the server sets is missing (not launched by TermOS?).
    Env(&'static str),
    Io(io::Error),
    /// The server refused a request.
    Rpc {
        code: i64,
        message: String,
    },
    /// The server sent something this SDK does not understand.
    Protocol(String),
    /// The server closed the connection.
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Env(var) => write!(
                f,
                "{var} is not set (plugins are started by the TermOS server)"
            ),
            Error::Io(e) => write!(f, "cannot talk to the TermOS server: {e}"),
            Error::Rpc { code, message } => write!(f, "{message} (error {code})"),
            Error::Protocol(m) => write!(f, "unexpected message from the server: {m}"),
            Error::Closed => f.write_str("the TermOS server closed the connection"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Protocol(e.to_string())
    }
}

/// Something the server told the plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A key pressed in one of the plugin's windows (config notation).
    Key { window: u64, key: String },
    /// A click or wheel turn in a window.
    Mouse {
        window: u64,
        kind: String,
        x: u16,
        y: u16,
    },
    /// A window's content area changed size.
    Resize { window: u64, cols: u16, rows: u16 },
    /// A window was closed (by the user or the server).
    Closed { window: u64 },
    /// A registered command was run.
    Command {
        name: String,
        session: Option<String>,
    },
    /// A notification this SDK does not know yet.
    Other { method: String, params: Value },
}

fn params<T: DeserializeOwned>(v: &Value) -> Option<T> {
    serde_json::from_value(v.get("params").cloned().unwrap_or(Value::Null)).ok()
}

fn parse_event(msg: &Value) -> Event {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let known = match method {
        methods::WINDOW_KEY => params::<WindowKey>(msg).map(|p| Event::Key {
            window: p.window,
            key: p.key,
        }),
        methods::WINDOW_MOUSE => params::<WindowMouse>(msg).map(|p| Event::Mouse {
            window: p.window,
            kind: p.kind,
            x: p.x,
            y: p.y,
        }),
        methods::WINDOW_RESIZE => params::<WindowResize>(msg).map(|p| Event::Resize {
            window: p.window,
            cols: p.cols,
            rows: p.rows,
        }),
        methods::WINDOW_CLOSED => {
            params::<WindowRef>(msg).map(|p| Event::Closed { window: p.window })
        }
        methods::COMMAND_INVOKED => params::<CommandInvoked>(msg).map(|p| Event::Command {
            name: p.name,
            session: p.session,
        }),
        _ => None,
    };
    known.unwrap_or_else(|| Event::Other {
        method: method.to_owned(),
        params: msg.get("params").cloned().unwrap_or(Value::Null),
    })
}

/// A connected plugin.
pub struct Plugin {
    name: String,
    conn: Box<dyn Conn>,
    /// Bytes of a line not complete yet (kept across read timeouts).
    partial: Vec<u8>,
    next_id: u64,
    queued: VecDeque<Event>,
}

impl fmt::Debug for Plugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plugin")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// Connects using the variables the server sets when it launches the
    /// plugin (`TERMOS_SOCKET`, `TERMOS_PLUGIN_TOKEN`, `TERMOS_PLUGIN_NAME`).
    pub fn connect_from_env() -> Result<Self, Error> {
        let socket = std::env::var_os(env::SOCKET).ok_or(Error::Env(env::SOCKET))?;
        let token = std::env::var(env::TOKEN).map_err(|_| Error::Env(env::TOKEN))?;
        let name = std::env::var(env::NAME).unwrap_or_else(|_| "plugin".into());
        Self::connect(Path::new(&socket), &token, &name)
    }

    /// Connects to `socket` and says hello with `token`.
    pub fn connect(socket: &Path, token: &str, name: &str) -> Result<Self, Error> {
        let mut p = Self {
            name: name.to_owned(),
            conn: crate::conn::connect(socket)?,
            partial: Vec::new(),
            next_id: 0,
            queued: VecDeque::new(),
        };
        p.call(
            methods::HELLO,
            json!({
                "version": PROTOCOL_VERSION,
                "kind": "plugin",
                "client": format!("{name} (termos-plugin {})", env!("CARGO_PKG_VERSION")),
                "token": token,
            }),
        )?;
        Ok(p)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sends a request and waits for its result. Notifications arriving
    /// meanwhile are kept for [`Plugin::next_event`].
    pub fn call(&mut self, method: &str, params: impl Serialize) -> Result<Value, Error> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_vec(&req)?;
        line.push(b'\n');
        self.conn.write_all(&line)?;
        loop {
            let msg = self.read_message(None)?.ok_or(Error::Closed)?;
            if msg.get("method").is_some() {
                self.queued.push_back(parse_event(&msg));
                continue;
            }
            if msg.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                return Err(Error::Rpc {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// One JSON message; `None` at end of stream. A read timeout leaves a
    /// partial line buffered for the next call.
    fn read_message(&mut self, timeout: Option<Duration>) -> Result<Option<Value>, Error> {
        loop {
            let n = self.conn.read_line(&mut self.partial, timeout)?;
            if n == 0 || !self.partial.ends_with(b"\n") {
                return Ok(None);
            }
            let line = std::mem::take(&mut self.partial);
            let text = line.trim_ascii();
            if text.is_empty() {
                continue;
            }
            return serde_json::from_slice(text).map(Some).map_err(Error::from);
        }
    }

    /// Waits for the next event. `Ok(None)` when the server closed the
    /// connection (the plugin should exit).
    pub fn next_event(&mut self) -> Result<Option<Event>, Error> {
        if let Some(e) = self.queued.pop_front() {
            return Ok(Some(e));
        }
        match self.read_message(None)? {
            Some(msg) => Ok(Some(parse_event(&msg))),
            None => Ok(None),
        }
    }

    /// Waits up to `timeout` for the next event: `Ok(None)` if none came
    /// (handy for redrawing a clock), [`Error::Closed`] if the server hung
    /// up.
    pub fn next_event_timeout(&mut self, timeout: Duration) -> Result<Option<Event>, Error> {
        if let Some(e) = self.queued.pop_front() {
            return Ok(Some(e));
        }
        match self.read_message(Some(timeout.max(Duration::from_millis(1)))) {
            Ok(Some(msg)) => Ok(Some(parse_event(&msg))),
            Ok(None) => Err(Error::Closed),
            Err(Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Opens a floating window in the most recently used session.
    pub fn open_window(&mut self, title: &str, lines: &[Line]) -> Result<u64, Error> {
        self.open_window_in(None, title, lines)
    }

    /// Opens a floating window in `session` (default: the most recently
    /// used one).
    pub fn open_window_in(
        &mut self,
        session: Option<&str>,
        title: &str,
        lines: &[Line],
    ) -> Result<u64, Error> {
        let v = self.call(
            methods::WINDOW_OPEN,
            WindowOpen {
                title: title.to_owned(),
                lines: lines.to_vec(),
                session: session.map(str::to_owned),
            },
        )?;
        let r: WindowRef = serde_json::from_value(v)?;
        Ok(r.window)
    }

    /// Replaces a window's content.
    pub fn draw(&mut self, window: u64, lines: &[Line]) -> Result<(), Error> {
        self.call(
            methods::WINDOW_DRAW,
            WindowDraw {
                window,
                lines: lines.to_vec(),
                title: None,
            },
        )
        .map(drop)
    }

    /// Replaces a window's title and content.
    pub fn draw_titled(&mut self, window: u64, title: &str, lines: &[Line]) -> Result<(), Error> {
        self.call(
            methods::WINDOW_DRAW,
            WindowDraw {
                window,
                lines: lines.to_vec(),
                title: Some(title.to_owned()),
            },
        )
        .map(drop)
    }

    pub fn close_window(&mut self, window: u64) -> Result<(), Error> {
        self.call(methods::WINDOW_CLOSE, WindowRef { window })
            .map(drop)
    }

    /// Sets the plugin's status-line widget.
    pub fn set_status(&mut self, text: impl Into<Line>) -> Result<(), Error> {
        self.call(
            methods::STATUS_SET,
            StatusSet {
                text: Some(text.into()),
            },
        )
        .map(drop)
    }

    /// Removes the plugin's status-line widget.
    pub fn clear_status(&mut self) -> Result<(), Error> {
        self.call(methods::STATUS_SET, StatusSet { text: None })
            .map(drop)
    }

    /// Adds `<plugin>.<name>` to the palette and the control API.
    pub fn register_command(&mut self, name: &str, description: &str) -> Result<(), Error> {
        self.call(
            methods::COMMAND_REGISTER,
            CommandRegister {
                name: name.to_owned(),
                description: description.to_owned(),
            },
        )
        .map(drop)
    }

    /// Shows a notification in every session.
    pub fn notify(&mut self, text: &str) -> Result<(), Error> {
        self.call(
            methods::NOTIFY_SEND,
            NotifySend {
                text: text.to_owned(),
                level: None,
            },
        )
        .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::{BufRead as _, BufReader, Write as _};
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;

    /// A fake server: answers every request with its id, sends one
    /// notification before answering the second request, then a late one.
    /// (On Windows the SDK is tested against the real server, in
    /// termos-shell's server tests.)
    #[cfg(unix)]
    #[test]
    fn requests_responses_and_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut out = stream.try_clone().expect("clone");
            let mut lines = BufReader::new(stream).lines();
            let mut seen = Vec::new();
            for n in 0..3 {
                let line = lines.next().expect("a request").expect("read");
                let req: Value = serde_json::from_str(&line).expect("json");
                seen.push(req["method"].as_str().unwrap_or_default().to_owned());
                if n == 1 {
                    writeln!(
                        out,
                        r#"{{"jsonrpc":"2.0","method":"window.key","params":{{"window":7,"key":"q"}}}}"#
                    )
                    .expect("write");
                }
                let reply = match n {
                    1 => json!({"jsonrpc": "2.0", "id": req["id"], "result": {"window": 7}}),
                    2 => {
                        json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32003, "message": "no"}})
                    }
                    _ => json!({"jsonrpc": "2.0", "id": req["id"], "result": {}}),
                };
                writeln!(out, "{reply}").expect("write");
            }
            writeln!(
                out,
                r#"{{"jsonrpc":"2.0","method":"command.invoked","params":{{"name":"go","session":"main"}}}}"#
            )
            .expect("write");
            seen
        });
        let mut p = Plugin::connect(&path, "secret", "t").expect("connect");
        assert_eq!(p.open_window("w", &[Line::from("x")]).expect("open"), 7);
        match p.notify("hi") {
            Err(Error::Rpc { code, .. }) => assert_eq!(code, -32003),
            other => panic!("expected an RPC error, got {other:?}"),
        }
        // The key arrived while waiting for the open response.
        assert_eq!(
            p.next_event().expect("event"),
            Some(Event::Key {
                window: 7,
                key: "q".into()
            })
        );
        assert_eq!(
            p.next_event_timeout(Duration::from_secs(5)).expect("event"),
            Some(Event::Command {
                name: "go".into(),
                session: Some("main".into())
            })
        );
        let seen = server.join().expect("server thread");
        assert_eq!(seen, ["hello", "window.open", "notify.send"]);
        assert!(matches!(
            p.next_event_timeout(Duration::from_millis(50)),
            Err(Error::Closed)
        ));
        assert_eq!(p.next_event().expect("eof"), None);
    }

    #[test]
    fn missing_environment_is_explained() {
        // Only meaningful outside a TermOS-launched plugin.
        if std::env::var_os(env::SOCKET).is_none() {
            let err = Plugin::connect_from_env().expect_err("not launched by TermOS");
            assert!(err.to_string().contains("TERMOS_SOCKET"));
        }
    }
}
