// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin protocol: method names, parameters and styled text.

use serde::{Deserialize, Serialize};

/// The handshake version (the server's protocol version; must match).
pub const PROTOCOL_VERSION: u32 = 1;

/// Environment variables the server sets for a plugin process.
pub mod env {
    /// Path of the session socket.
    pub const SOCKET: &str = "TERMOS_SOCKET";
    /// The per-launch token that identifies the plugin in its `hello`.
    pub const TOKEN: &str = "TERMOS_PLUGIN_TOKEN";
    /// The plugin's name (its directory name).
    pub const NAME: &str = "TERMOS_PLUGIN_NAME";
}

/// Method names.
pub mod methods {
    // Plugin → server requests.
    pub const HELLO: &str = "hello";
    pub const WINDOW_OPEN: &str = "window.open";
    pub const WINDOW_DRAW: &str = "window.draw";
    pub const WINDOW_CLOSE: &str = "window.close";
    pub const STATUS_SET: &str = "status.set";
    pub const COMMAND_REGISTER: &str = "command.register";
    pub const NOTIFY_SEND: &str = "notify.send";
    // Server → plugin notifications.
    pub const WINDOW_KEY: &str = "window.key";
    pub const WINDOW_MOUSE: &str = "window.mouse";
    pub const WINDOW_RESIZE: &str = "window.resize";
    pub const WINDOW_CLOSED: &str = "window.closed";
    pub const COMMAND_INVOKED: &str = "command.invoked";
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A run of text with one style. Colours are written like in the config
/// file: `"default"`, `"#rrggbb"`, `"colourN"` or names like `"brightblue"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dim: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reverse: bool,
}

impl Span {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn fg(mut self, color: impl Into<String>) -> Self {
        self.fg = Some(color.into());
        self
    }

    pub fn bg(mut self, color: impl Into<String>) -> Self {
        self.bg = Some(color.into());
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
}

/// A line of text: a plain string, or styled spans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Line {
    Plain(String),
    Styled(Vec<Span>),
}

impl Line {
    /// The spans of the line (a plain line is one unstyled span).
    pub fn spans(&self) -> Vec<Span> {
        match self {
            Line::Plain(t) => vec![Span::new(t.clone())],
            Line::Styled(s) => s.clone(),
        }
    }
}

impl From<&str> for Line {
    fn from(s: &str) -> Self {
        Line::Plain(s.to_owned())
    }
}

impl From<String> for Line {
    fn from(s: String) -> Self {
        Line::Plain(s)
    }
}

impl From<Vec<Span>> for Line {
    fn from(s: Vec<Span>) -> Self {
        Line::Styled(s)
    }
}

/// `window.open`: a new floating window in the session (default: the most
/// recently used one). Returns [`WindowRef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowOpen {
    pub title: String,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// Names a window (`window.close`, the result of `window.open`, and the
/// `window.closed` notification).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRef {
    pub window: u64,
}

/// `window.draw`: replaces a window's content (and title, if given).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowDraw {
    pub window: u64,
    pub lines: Vec<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// `status.set`: the plugin's status-line widget (`null` removes it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSet {
    pub text: Option<Line>,
}

/// `command.register`: adds `<plugin>.<name>` to the palette and the
/// control API; running it sends `command.invoked`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRegister {
    pub name: String,
    pub description: String,
}

/// `notify.send`: a notification in every session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifySend {
    pub text: String,
    /// `info` (default), `warn` or `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

/// `window.key`: a key pressed in the plugin's focused window, in the
/// config file's notation (`a`, `C-x`, `Enter`, `M-Left`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowKey {
    pub window: u64,
    pub key: String,
}

/// `window.mouse`: a click or wheel turn inside a window (`kind` is
/// `down`, `up`, `drag`, `scroll-up` or `scroll-down`; cells from the
/// window's top left).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowMouse {
    pub window: u64,
    pub kind: String,
    pub x: u16,
    pub y: u16,
}

/// `window.resize`: the window's content area changed size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowResize {
    pub window: u64,
    pub cols: u16,
    pub rows: u16,
}

/// `command.invoked`: a registered command was run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInvoked {
    /// The name as registered (without the plugin prefix).
    pub name: String,
    /// The session it was run from.
    #[serde(default)]
    pub session: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_are_strings_or_spans() {
        let plain: Line = serde_json::from_value(json!("hi")).expect("plain");
        assert_eq!(plain, Line::from("hi"));
        let styled: Line = serde_json::from_value(
            json!([{"text": "a", "fg": "red", "bold": true}, {"text": "b"}]),
        )
        .expect("styled");
        assert_eq!(
            styled,
            Line::Styled(vec![Span::new("a").fg("red").bold(), Span::new("b")])
        );
        assert_eq!(
            serde_json::to_value(Span::new("x")).expect("json"),
            json!({"text": "x"}),
            "defaults are left out"
        );
        assert_eq!(Line::from("p").spans(), vec![Span::new("p")]);
    }

    #[test]
    fn params_round_trip() {
        let open = WindowOpen {
            title: "t".into(),
            lines: vec![Line::from("x")],
            session: None,
        };
        let v = serde_json::to_value(&open).expect("json");
        assert_eq!(v, json!({"title": "t", "lines": ["x"]}));
        let back: WindowOpen = serde_json::from_value(v).expect("back");
        assert_eq!(back, open);
        let status: StatusSet = serde_json::from_value(json!({"text": null})).expect("null");
        assert_eq!(status.text, None);
    }
}
