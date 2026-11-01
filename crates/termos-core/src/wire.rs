// SPDX-License-Identifier: GPL-3.0-or-later
//! The wire format of the session socket (DECISIONS.md D-010, D-031).
//!
//! Every connection opens with one newline-terminated JSON-RPC 2.0 request,
//! `hello`, naming the protocol version and the connection kind. After the
//! reply:
//! - **frontends** exchange binary frames: a little-endian `u32` length
//!   followed by a postcard-encoded [`ClientMsg`](crate::proto::ClientMsg) or
//!   [`ServerMsg`](crate::proto::ServerMsg), at most [`MAX_FRAME`] bytes;
//! - **control** clients and **plugins** keep exchanging newline-delimited
//!   JSON-RPC 2.0 messages.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::proto::PROTOCOL_VERSION;

/// Largest accepted frame body. Frames are screen diffs; a full 500×200
/// screen is well under this.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Largest accepted JSON-RPC line.
pub const MAX_LINE: usize = 4 * 1024 * 1024;

/// Errors decoding the wire format.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireError {
    #[error("frame of {0} bytes exceeds the {MAX_FRAME}-byte limit")]
    TooLarge(usize),
    #[error("malformed frame: {0}")]
    Malformed(String),
}

/// Encodes one message as a length-prefixed postcard frame.
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, WireError> {
    let body = postcard::to_stdvec(msg).map_err(|e| WireError::Malformed(e.to_string()))?;
    if body.len() > MAX_FRAME {
        return Err(WireError::TooLarge(body.len()));
    }
    let len = u32::try_from(body.len()).map_err(|_| WireError::TooLarge(body.len()))?;
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Incremental frame decoder: push bytes as they arrive, pop whole messages.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete message, `Ok(None)` if more bytes are needed.
    pub fn next_frame<T: DeserializeOwned>(&mut self) -> Result<Option<T>, WireError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
        let len = usize::try_from(len).map_err(|_| WireError::TooLarge(usize::MAX))?;
        if len > MAX_FRAME {
            return Err(WireError::TooLarge(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let msg = postcard::from_bytes(&self.buf[4..4 + len])
            .map_err(|e| WireError::Malformed(e.to_string()));
        self.buf.drain(..4 + len);
        msg.map(Some)
    }
}

/// What a connection is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnKind {
    /// A terminal (or KMS) frontend showing a session.
    Frontend,
    /// Scripts and `termos msg`: request/response JSON-RPC.
    Control,
    /// An out-of-process plugin launched by the server.
    Plugin,
}

/// Parameters of the opening `hello` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub kind: ConnKind,
    /// Software name and version, for diagnostics.
    #[serde(default)]
    pub client: String,
    /// Frontends: the session to show (default: the server picks).
    #[serde(default)]
    pub session: Option<String>,
    /// Frontends: create the session if it does not exist.
    #[serde(default)]
    pub create: bool,
    /// Plugins: the token the server gave the plugin process.
    #[serde(default)]
    pub token: Option<String>,
}

impl Hello {
    pub fn new(kind: ConnKind) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            kind,
            client: format!("termos {}", env!("CARGO_PKG_VERSION")),
            session: None,
            create: false,
            token: None,
        }
    }
}

/// The server's answer to `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    pub version: u32,
    pub server: String,
    pub pid: u32,
    /// Frontends: the session they are attached to.
    #[serde(default)]
    pub session: Option<String>,
}

/// JSON-RPC 2.0 error codes used by TermOS.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// The client speaks a different protocol version.
    pub const VERSION_MISMATCH: i64 = -32001;
    /// The first message was not `hello`.
    pub const HELLO_REQUIRED: i64 = -32002;
    /// Not allowed (wrong user, missing plugin permission, bad token).
    pub const FORBIDDEN: i64 = -32003;
    /// A named session, pane, window or plugin does not exist.
    pub const NOT_FOUND: i64 = -32004;
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message} (code {code})")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(codes::NOT_FOUND, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(codes::FORBIDDEN, message)
    }
}

/// A JSON-RPC request (with `id`) or notification (without).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: impl Into<Value>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.into()),
            method: method.into(),
            params,
        }
    }

    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: method.into(),
            params,
        }
    }

    /// The params deserialized as `T` (missing params read as `{}`).
    pub fn params_as<T: DeserializeOwned>(&self) -> Result<T, RpcError> {
        let v = self
            .params
            .clone()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
    }
}

/// A JSON-RPC response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn into_result(self) -> Result<Value, RpcError> {
        match (self.result, self.error) {
            (_, Some(e)) => Err(e),
            (Some(v), None) => Ok(v),
            (None, None) => Ok(Value::Null),
        }
    }
}

/// Any JSON-RPC message read from a peer.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request(Request),
    Response(Response),
}

/// Parses one JSON-RPC line.
pub fn parse_message(line: &str) -> Result<Message, RpcError> {
    let v: Value = serde_json::from_str(line)
        .map_err(|e| RpcError::new(codes::PARSE_ERROR, format!("not JSON: {e}")))?;
    let Value::Object(obj) = &v else {
        return Err(RpcError::new(
            codes::INVALID_REQUEST,
            "expected a JSON object",
        ));
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RpcError::new(
            codes::INVALID_REQUEST,
            "missing \"jsonrpc\": \"2.0\"",
        ));
    }
    if obj.contains_key("method") {
        serde_json::from_value(v)
            .map(Message::Request)
            .map_err(|e| RpcError::new(codes::INVALID_REQUEST, e.to_string()))
    } else if obj.contains_key("result") || obj.contains_key("error") {
        serde_json::from_value(v)
            .map(Message::Response)
            .map_err(|e| RpcError::new(codes::INVALID_REQUEST, e.to_string()))
    } else {
        Err(RpcError::new(
            codes::INVALID_REQUEST,
            "neither a request nor a response",
        ))
    }
}

/// Serializes a JSON-RPC message as one line (with the trailing newline).
pub fn to_line<T: Serialize>(msg: &T) -> String {
    // Our message types always serialize; fall back to an error object.
    let mut s = serde_json::to_string(msg).unwrap_or_else(|e| {
        format!(r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"{e}"}}}}"#)
    });
    s.push('\n');
    s
}

/// Validates a `hello` request: it must be the method `hello` with valid
/// params and the server's protocol version.
pub fn check_hello(req: &Request) -> Result<Hello, RpcError> {
    if req.method != "hello" {
        return Err(RpcError::new(
            codes::HELLO_REQUIRED,
            format!("the first request must be `hello`, not `{}`", req.method),
        ));
    }
    let hello: Hello = req.params_as()?;
    if hello.version != PROTOCOL_VERSION {
        return Err(RpcError::new(
            codes::VERSION_MISMATCH,
            format!(
                "protocol version mismatch: this server speaks {PROTOCOL_VERSION}, the client speaks {}; restart the server (`termos kill-server`) after upgrading",
                hello.version
            ),
        ));
    }
    Ok(hello)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyEvent;
    use crate::proto::{ClientMsg, FrameUpdate, ServerMsg};
    use termos_render::cell::Style;
    use termos_render::diff::full;
    use termos_render::grid::Grid;

    #[test]
    fn frames_round_trip_and_split_anywhere() {
        let mut g = Grid::new(10, 2);
        g.put_str(0, 0, "hi 界", Style::new().bold(), 10);
        let msgs = vec![
            ServerMsg::Frame(FrameUpdate {
                cols: 10,
                rows: 2,
                clear: true,
                spans: full(&g),
                cursor: None,
            }),
            ServerMsg::Bell,
            ServerMsg::Detached {
                session: "main".into(),
            },
        ];
        let mut bytes = Vec::new();
        for m in &msgs {
            bytes.extend(encode_frame(m).expect("encodes"));
        }
        // Feed one byte at a time: the decoder must reassemble.
        let mut d = FrameDecoder::new();
        let mut out = Vec::new();
        for b in bytes {
            d.push(&[b]);
            while let Some(m) = d.next_frame::<ServerMsg>().expect("valid") {
                out.push(m);
            }
        }
        assert_eq!(out, msgs);
    }

    #[test]
    fn oversized_and_garbage_frames_are_errors() {
        let mut d = FrameDecoder::new();
        d.push(&u32::MAX.to_le_bytes());
        assert!(matches!(
            d.next_frame::<ClientMsg>(),
            Err(WireError::TooLarge(_))
        ));
        let mut d = FrameDecoder::new();
        d.push(&[3, 0, 0, 0, 0xff, 0xff, 0xff]);
        assert!(matches!(
            d.next_frame::<ClientMsg>(),
            Err(WireError::Malformed(_))
        ));
        let mut d = FrameDecoder::new();
        d.push(&encode_frame(&ClientMsg::Key(KeyEvent::ctrl('b'))).expect("encodes"));
        assert_eq!(
            d.next_frame::<ClientMsg>(),
            Ok(Some(ClientMsg::Key(KeyEvent::ctrl('b'))))
        );
        assert_eq!(d.next_frame::<ClientMsg>(), Ok(None));
    }

    #[test]
    fn json_rpc_parsing() {
        let req = Request::new(1, "session.list", None);
        match parse_message(to_line(&req).trim()) {
            Ok(Message::Request(r)) => assert_eq!(r, req),
            other => panic!("{other:?}"),
        }
        let resp = Response::err(Value::from(7), RpcError::not_found("no such session"));
        match parse_message(&to_line(&resp)) {
            Ok(Message::Response(r)) => {
                assert_eq!(r.into_result(), Err(RpcError::not_found("no such session")));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_message("nope").map_err(|e| e.code),
            Err(codes::PARSE_ERROR)
        );
        assert_eq!(
            parse_message(r#"{"method":"x"}"#).map_err(|e| e.code),
            Err(codes::INVALID_REQUEST)
        );
        assert_eq!(
            parse_message(r#"{"jsonrpc":"2.0","id":1}"#).map_err(|e| e.code),
            Err(codes::INVALID_REQUEST)
        );
    }

    #[test]
    fn hello_checks() {
        let mut hello = Hello::new(ConnKind::Frontend);
        hello.session = Some("main".into());
        let req = Request::new(0, "hello", serde_json::to_value(&hello).ok());
        assert_eq!(check_hello(&req), Ok(hello.clone()));

        let mut old = hello.clone();
        old.version = 999;
        let req = Request::new(0, "hello", serde_json::to_value(&old).ok());
        let err = check_hello(&req).expect_err("mismatch");
        assert_eq!(err.code, codes::VERSION_MISMATCH);
        assert!(err.message.contains("999"));

        let req = Request::new(0, "session.list", None);
        assert_eq!(
            check_hello(&req).map_err(|e| e.code),
            Err(codes::HELLO_REQUIRED)
        );

        let req = Request::new(
            0,
            "hello",
            Some(serde_json::json!({"version": 1, "kind": "toaster"})),
        );
        assert_eq!(
            check_hello(&req).map_err(|e| e.code),
            Err(codes::INVALID_PARAMS)
        );
    }
}
