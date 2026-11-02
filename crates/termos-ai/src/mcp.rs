// SPDX-License-Identifier: GPL-3.0-or-later
//! MCP servers' tools for agents (ARCHITECTURE.md §11, D-063).
//!
//! A profile lists the servers it may use (`mcp-servers`); only those are
//! started, when a session starts, and stopped with it. A server with a
//! `command` runs as a child speaking MCP over stdio, inside the same
//! sandbox as commands (network only if `network = true`); one with a
//! `url` is reached over streamable HTTP, with a bearer token from the
//! secret store. Its tools reach the model as `mcp__<server>__<tool>`,
//! and every call goes through the permission engine.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use termos_core::config::agents::{McpServerConfig, SandboxMode};

use crate::provider::ToolSpec;
use crate::sandbox::{self, Capabilities, Spec};

type Client = RunningService<RoleClient, ()>;

/// Prefix of MCP tool names as the model sees them.
pub const PREFIX: &str = "mcp__";

/// The connected servers of one session.
#[derive(Default)]
pub struct Hub {
    servers: BTreeMap<String, Arc<Client>>,
    tools: Vec<(String, String, ToolSpec)>,
    /// Servers that did not start, and why.
    pub problems: Vec<String>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("servers", &self.servers.keys().collect::<Vec<_>>())
            .field("tools", &self.tools.len())
            .finish()
    }
}

/// How to start a local server.
pub struct Launch<'a> {
    pub caps: &'a Capabilities,
    pub mode: SandboxMode,
    pub helper: Option<&'a Path>,
    pub workspace: &'a Path,
    pub hidden: &'a [std::path::PathBuf],
}

impl Hub {
    /// Starts each named server; one that fails is reported in
    /// [`Hub::problems`] and left out.
    pub async fn start(
        names: &[String],
        configs: &BTreeMap<String, McpServerConfig>,
        keys: &BTreeMap<String, SecretString>,
        launch: &Launch<'_>,
    ) -> Self {
        let mut hub = Self::default();
        for name in names {
            let Some(cfg) = configs.get(name) else {
                hub.problems.push(format!(
                    "MCP server {name} is not configured under [agents.mcp]"
                ));
                continue;
            };
            let started = match (&cfg.url, cfg.command.is_empty()) {
                (Some(url), true) => connect_http(url, cfg, keys.get(name)).await,
                (None, false) => spawn_stdio(cfg, launch).await,
                (Some(_), false) => Err("give either `command` or `url`, not both".into()),
                (None, true) => Err("needs a `command` or a `url`".into()),
            };
            match started {
                Ok(client) => {
                    if let Err(e) = hub.add(name, client).await {
                        hub.problems.push(format!("MCP server {name}: {e}"));
                    }
                }
                Err(e) => hub.problems.push(format!("MCP server {name}: {e}")),
            }
        }
        hub
    }

    /// Adds a connected server, listing its tools.
    pub async fn add(&mut self, name: &str, client: Client) -> Result<(), String> {
        let tools = client.list_all_tools().await.map_err(|e| e.to_string())?;
        for t in tools {
            let full = format!("{PREFIX}{name}__{}", t.name);
            let schema = Value::Object((*t.input_schema).clone());
            self.tools.push((
                name.to_owned(),
                t.name.to_string(),
                ToolSpec {
                    name: full,
                    description: format!(
                        "[MCP server {name}] {}",
                        t.description.as_deref().unwrap_or("(no description)")
                    ),
                    schema,
                },
            ));
        }
        self.servers.insert(name.to_owned(), Arc::new(client));
        Ok(())
    }

    /// The tools to offer the model.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|(_, _, s)| s.clone()).collect()
    }

    /// Server and tool behind a model-facing name.
    pub fn lookup(&self, full: &str) -> Option<(&str, &str)> {
        self.tools
            .iter()
            .find(|(_, _, s)| s.name == full)
            .map(|(server, tool, _)| (server.as_str(), tool.as_str()))
    }

    /// Calls a tool; the text of its result, and whether it failed.
    pub async fn call(&self, server: &str, tool: &str, args: &Value) -> (String, bool) {
        let Some(client) = self.servers.get(server) else {
            return (format!("MCP server {server} is not connected"), true);
        };
        let mut params = CallToolRequestParams::new(tool.to_owned());
        params.arguments = args.as_object().cloned();
        match client.call_tool(params).await {
            Ok(result) => {
                let text = result
                    .content
                    .iter()
                    .map(|c| match c {
                        ContentBlock::Text(t) => t.text.clone(),
                        ContentBlock::Image(_) => "[an image]".into(),
                        ContentBlock::Audio(_) => "[audio]".into(),
                        ContentBlock::Resource(_) => "[an embedded resource]".into(),
                        ContentBlock::ResourceLink(r) => format!("[resource {}]", r.uri),
                        _ => "[content]".into(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let text = match (text.is_empty(), &result.structured_content) {
                    (true, Some(v)) => v.to_string(),
                    _ => text,
                };
                (text, result.is_error.unwrap_or(false))
            }
            Err(e) => (format!("MCP error: {e}"), true),
        }
    }

    /// Stops every server.
    pub async fn shutdown(self) {
        for (_, client) in self.servers {
            if let Ok(client) = Arc::try_unwrap(client) {
                let _ = client.cancel().await;
            }
        }
    }
}

async fn spawn_stdio(cfg: &McpServerConfig, launch: &Launch<'_>) -> Result<Client, String> {
    let spec = Spec {
        workspace: launch.workspace.to_owned(),
        writable: cfg.write_paths.clone(),
        hidden: launch.hidden.to_vec(),
        network: cfg.network,
        cwd: launch.workspace.to_owned(),
        keep_env: false,
    };
    let argv = sandbox::wrap(&spec, launch.caps, launch.mode, launch.helper, &cfg.command)
        .map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(launch.workspace);
    for k in sandbox::secret_env_names(std::env::vars()) {
        cmd.env_remove(k);
    }
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }
    let (transport, _stderr) = rmcp::transport::TokioChildProcess::builder(cmd)
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", cfg.command[0]))?;
    ().serve(transport).await.map_err(|e| e.to_string())
}

async fn connect_http(
    url: &str,
    cfg: &McpServerConfig,
    key: Option<&SecretString>,
) -> Result<Client, String> {
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    if cfg.key.is_some() && key.is_none() {
        return Err("its key is missing from the secret store".into());
    }
    let client = crate::provider::http::client()
        .map_err(|e| e.message)?
        .clone();
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
    config.auth_header = key.map(|k| k.expose_secret().to_owned());
    let transport = StreamableHttpClientTransport::with_client(client, config);
    ().serve(transport).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A minimal MCP server speaking JSON-RPC lines on a duplex pipe.
    async fn fake_server(stream: tokio::io::DuplexStream) {
        let (r, mut w) = tokio::io::split(stream);
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let msg: Value = serde_json::from_str(&line).expect("json");
            let Some(id) = msg.get("id").cloned() else {
                continue;
            };
            let result = match msg["method"].as_str() {
                Some("initialize") => serde_json::json!({
                    "protocolVersion": msg["params"]["protocolVersion"],
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "1"}
                }),
                Some("tools/list") => serde_json::json!({"tools": [{
                    "name": "shout",
                    "description": "Upper-cases text",
                    "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}
                }]}),
                Some("tools/call") => {
                    let text = msg["params"]["arguments"]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_uppercase();
                    serde_json::json!({"content": [{"type": "text", "text": text}], "isError": false})
                }
                _ => serde_json::json!({}),
            };
            let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
            let mut out = serde_json::to_vec(&reply).expect("json");
            out.push(b'\n');
            if w.write_all(&out).await.is_err() {
                return;
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tools_are_listed_named_and_called() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        tokio::spawn(fake_server(theirs));
        let (r, w) = tokio::io::split(ours);
        let client = ().serve((r, w)).await.expect("initialize");
        let mut hub = Hub::default();
        hub.add("demo", client).await.expect("tools");
        let specs = hub.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "mcp__demo__shout");
        assert!(
            specs[0]
                .description
                .starts_with("[MCP server demo] Upper-cases")
        );
        assert_eq!(hub.lookup("mcp__demo__shout"), Some(("demo", "shout")));
        let (text, failed) = hub
            .call("demo", "shout", &serde_json::json!({"text": "hi"}))
            .await;
        assert_eq!((text.as_str(), failed), ("HI", false));
        let (text, failed) = hub.call("nope", "x", &serde_json::json!({})).await;
        assert!(failed && text.contains("not connected"));
        hub.shutdown().await;
    }
}
