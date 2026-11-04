// SPDX-License-Identifier: GPL-3.0-or-later
//! TermOS's MCP server (D-094): any line from a client gets at most one
//! reply, which is JSON, and handling it never panics.
#![no_main]

use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};
use termos_ai::mcp_server::{Context, Control, Server};
use termos_core::config::agents::TermosMcpConfig;

/// A TermOS that answers every call.
struct Echo;

impl Control for Echo {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "pane.list" => Ok(json!([{ "id": 1 }, { "id": 2 }])),
            "pane.read" => Ok(json!({ "lines": [params.to_string()] })),
            "pane.open" => Ok(json!({ "pane": 3 })),
            _ => Ok(json!({})),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut server = Server::new(
        Box::new(Echo),
        Context {
            pane: Some(2),
            session: None,
            cwd: std::env::temp_dir(),
        },
        TermosMcpConfig::default(),
        None,
        None,
    );
    for line in text.lines() {
        if let Some(reply) = server.handle_line(line) {
            assert!(serde_json::from_str::<Value>(&reply).is_ok(), "{reply}");
        }
    }
});
