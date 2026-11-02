// SPDX-License-Identifier: GPL-3.0-or-later
//! Anthropic's Messages API, streamed (API version 2026-10-21).
//!
//! - Models come from `GET /v1/models` (paginated with `after_id`), with
//!   each model's context window and output limit.
//! - A turn is `POST /v1/messages` with `stream: true`. Tools stream their
//!   input as it is generated (`eager_input_streaming`), so the input is
//!   checked here before anything runs.
//! - `thinking` and `redacted_thinking` blocks (and any block type this
//!   client does not know) are kept verbatim, signatures included, and
//!   sent back unchanged: the API checks that earlier turns were not
//!   edited.
//! - No sampling parameters are sent (the API version refuses them), and
//!   no `thinking` setting: each model's own default applies.
//! - Prompt caching is on (`cache_control` at the top level): an agent
//!   resends its whole conversation every turn.

use std::collections::BTreeMap;

use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value, json};
use termos_core::config::agents::ProviderKind;

use super::http::{client, for_each_event, network, status_error};
use super::{
    Block, ChatFuture, ChatRequest, Delta, ErrorKind, Message, ModelInfo, Provider, ProviderError,
    Reply, Role, StopReason, Usage,
};

/// The API version TermOS speaks.
pub const API_VERSION: &str = "2026-10-21";

/// Output tokens per reply when neither the config nor the model list
/// says.
const DEFAULT_MAX_TOKENS: u64 = 32_000;
/// The most output tokens asked for by default (a streamed reply can be
/// longer, but rarely needs to).
const MAX_DEFAULT: u64 = 64_000;

pub struct Anthropic {
    name: String,
    base: String,
    key: SecretString,
    max_tokens: Option<u32>,
    /// Output limits from the model list, by id.
    limits: std::sync::Mutex<BTreeMap<String, u64>>,
}

impl Anthropic {
    pub fn new(name: &str, base: String, key: SecretString, max_tokens: Option<u32>) -> Self {
        Self {
            name: name.to_owned(),
            base,
            key,
            max_tokens,
            limits: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        let mut key =
            reqwest::header::HeaderValue::from_str(self.key.expose_secret()).map_err(|_| {
                ProviderError::new(
                    ErrorKind::Config,
                    "the API key has characters a header cannot carry",
                )
            })?;
        key.set_sensitive(true);
        Ok(client()?
            .request(method, format!("{}{path}", self.base))
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION))
    }

    async fn list(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        // Bounded: a provider that always says "more" cannot loop us.
        for _ in 0..50 {
            let mut path = "/v1/models?limit=1000".to_owned();
            if let Some(id) = &after {
                path.push_str("&after_id=");
                path.push_str(&url_escape(id));
            }
            let resp = self
                .request(reqwest::Method::GET, &path)?
                .send()
                .await
                .map_err(|e| network(&e))?;
            if !resp.status().is_success() {
                return Err(status_error(resp).await);
            }
            let page: Value = resp
                .json()
                .await
                .map_err(|e| ProviderError::new(ErrorKind::Protocol, e.to_string()))?;
            let (models, more, last) = parse_models_page(&page)?;
            out.extend(models);
            if !more || last.is_none() {
                break;
            }
            after = last;
        }
        if let Ok(mut limits) = self.limits.lock() {
            for m in &out {
                if let Some(max) = m.max_output {
                    limits.insert(m.id.clone(), max);
                }
            }
        }
        Ok(out)
    }

    fn max_tokens_for(&self, req: &ChatRequest) -> u64 {
        if let Some(n) = req.max_tokens {
            return n;
        }
        if let Some(n) = self.max_tokens {
            return u64::from(n);
        }
        self.limits
            .lock()
            .ok()
            .and_then(|l| l.get(&req.model).copied())
            .map_or(DEFAULT_MAX_TOKENS, |n| n.min(MAX_DEFAULT))
    }

    async fn turn(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<Reply, ProviderError> {
        let body = request_body(req, self.max_tokens_for(req));
        let resp = self
            .request(reqwest::Method::POST, "/v1/messages")?
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| network(&e))?;
        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }
        let mut state = StreamState::default();
        for_each_event(resp, |ev| state.event(&ev.event, &ev.data, on_delta)).await?;
        state.finish()
    }
}

impl Provider for Anthropic {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, ProviderError>> {
        Box::pin(self.list())
    }

    fn chat<'a>(
        &'a self,
        req: &'a ChatRequest,
        on_delta: &'a mut (dyn FnMut(Delta) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(self.turn(req, on_delta))
    }
}

fn url_escape(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// One page of `GET /v1/models`: the models, whether there are more, and
/// the id to continue after.
pub(crate) fn parse_models_page(
    page: &Value,
) -> Result<(Vec<ModelInfo>, bool, Option<String>), ProviderError> {
    let data = page
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::new(ErrorKind::Protocol, "model list without `data`"))?;
    let models = data
        .iter()
        .filter_map(|m| {
            Some(ModelInfo {
                id: m.get("id")?.as_str()?.to_owned(),
                name: m
                    .get("display_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                context: m.get("max_input_tokens").and_then(Value::as_u64),
                max_output: m.get("max_tokens").and_then(Value::as_u64),
                price: None,
            })
        })
        .collect();
    let more = page
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let last = page
        .get("last_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((models, more, last))
}

/// The request body for a turn.
pub(crate) fn request_body(req: &ChatRequest, max_tokens: u64) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("max_tokens".into(), json!(max_tokens));
    body.insert("stream".into(), json!(true));
    if !req.system.is_empty() {
        body.insert(
            "system".into(),
            json!([{"type": "text", "text": req.system}]),
        );
    }
    body.insert(
        "messages".into(),
        Value::Array(req.messages.iter().map(message_json).collect()),
    );
    if !req.tools.is_empty() {
        let tools = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.schema,
                    "eager_input_streaming": true,
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    body.insert("cache_control".into(), json!({"type": "ephemeral"}));
    Value::Object(body)
}

fn message_json(m: &Message) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let content: Vec<Value> = m
        .content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(json!({"type": "text", "text": text})),
            Block::ToolUse { id, name, input } => {
                Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
            }
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut v =
                    json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content});
                if *is_error {
                    v["is_error"] = json!(true);
                }
                Some(v)
            }
            Block::Opaque { provider, raw, .. } if provider == "anthropic" => Some(raw.clone()),
            // Another provider's private blocks mean nothing here.
            Block::Opaque { .. } => None,
        })
        .collect();
    json!({"role": role, "content": content})
}

/// A content block being streamed.
enum Building {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
    Thinking {
        raw: Map<String, Value>,
        text: String,
        signature: String,
    },
    /// Anything else (`redacted_thinking`, newer types): as it started.
    Raw(Value),
}

#[derive(Default)]
struct StreamState {
    blocks: BTreeMap<u64, Building>,
    usage: Usage,
    stop: Option<StopReason>,
    stopped: bool,
}

fn proto(msg: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Protocol, msg)
}

impl StreamState {
    /// Handles one event; `false` once the message has ended.
    fn event(
        &mut self,
        name: &str,
        data: &str,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<bool, ProviderError> {
        if name == "ping" {
            return Ok(true);
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| proto(format!("a `{name}` event that is not JSON: {e}")))?;
        let kind = v.get("type").and_then(Value::as_str).unwrap_or(name);
        match kind {
            "message_start" => {
                if let Some(u) = v.pointer("/message/usage") {
                    self.read_usage(u);
                }
            }
            "content_block_start" => {
                let index = index_of(&v)?;
                let block = v
                    .get("content_block")
                    .ok_or_else(|| proto("content_block_start without a block"))?;
                let building = match block.get("type").and_then(Value::as_str) {
                    Some("text") => Building::Text(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    ),
                    Some("tool_use") => {
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        on_delta(Delta::ToolStart { name: name.clone() });
                        Building::Tool {
                            id: block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            name,
                            json: String::new(),
                        }
                    }
                    Some("thinking") => {
                        let raw = block.as_object().cloned().unwrap_or_default();
                        Building::Thinking {
                            text: raw
                                .get("thinking")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            signature: raw
                                .get("signature")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            raw,
                        }
                    }
                    _ => Building::Raw(block.clone()),
                };
                self.blocks.insert(index, building);
            }
            "content_block_delta" => {
                let index = index_of(&v)?;
                let delta = v
                    .get("delta")
                    .ok_or_else(|| proto("a delta without `delta`"))?;
                let text = |field: &str| delta.get(field).and_then(Value::as_str).unwrap_or("");
                match (
                    self.blocks.get_mut(&index),
                    delta.get("type").and_then(Value::as_str),
                ) {
                    (Some(Building::Text(t)), Some("text_delta")) => {
                        let piece = text("text");
                        t.push_str(piece);
                        on_delta(Delta::Text(piece.to_owned()));
                    }
                    (Some(Building::Tool { json, .. }), Some("input_json_delta")) => {
                        json.push_str(text("partial_json"));
                    }
                    (Some(Building::Thinking { text: t, .. }), Some("thinking_delta")) => {
                        let piece = text("thinking");
                        t.push_str(piece);
                        on_delta(Delta::Thinking(piece.to_owned()));
                    }
                    (Some(Building::Thinking { signature, .. }), Some("signature_delta")) => {
                        signature.push_str(text("signature"));
                    }
                    (None, _) => return Err(proto(format!("a delta for unknown block {index}"))),
                    (Some(_), other) => {
                        tracing::debug!("anthropic: ignoring a {other:?} delta");
                    }
                }
            }
            "content_block_stop" => {}
            "message_delta" => {
                if let Some(reason) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = Some(stop_reason(reason, &v));
                }
                if let Some(u) = v.get("usage") {
                    self.read_usage(u);
                }
            }
            "message_stop" => {
                self.stopped = true;
                return Ok(false);
            }
            "error" => {
                let kind_name = v
                    .pointer("/error/type")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                let message = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the stream reported an error");
                let kind = match kind_name {
                    "overloaded_error" | "rate_limit_error" => ErrorKind::RateLimited,
                    "api_error" => ErrorKind::Server,
                    "authentication_error" | "permission_error" => ErrorKind::Auth,
                    _ => ErrorKind::Request,
                };
                return Err(ProviderError::new(kind, format!("{kind_name}: {message}")));
            }
            other => tracing::debug!("anthropic: ignoring a {other} event"),
        }
        Ok(true)
    }

    fn read_usage(&mut self, u: &Value) {
        let get = |k: &str| u.get(k).and_then(Value::as_u64);
        // message_start carries the input side; message_delta carries the
        // running output count (and, on some versions, the input again).
        if let Some(n) = get("input_tokens") {
            self.usage.input = n;
        }
        if let Some(n) = get("cache_read_input_tokens") {
            self.usage.cache_read = n;
        }
        if let Some(n) = get("cache_creation_input_tokens") {
            self.usage.cache_write = n;
        }
        if let Some(n) = get("output_tokens") {
            self.usage.output = n;
        }
    }

    fn finish(self) -> Result<Reply, ProviderError> {
        if !self.stopped {
            return Err(ProviderError::new(
                ErrorKind::Network,
                "the reply stopped before it was complete",
            ));
        }
        let mut content = Vec::new();
        let mut bad_inputs = Vec::new();
        for (_, b) in self.blocks {
            match b {
                Building::Text(text) => content.push(Block::Text { text }),
                Building::Tool { id, name, json } => {
                    let input = if json.trim().is_empty() {
                        Ok(json!({}))
                    } else {
                        serde_json::from_str::<Value>(&json)
                            .map_err(|e| e.to_string())
                            .and_then(|v| {
                                if v.is_object() {
                                    Ok(v)
                                } else {
                                    Err("the input is not a JSON object".into())
                                }
                            })
                    };
                    let input = input.unwrap_or_else(|e| {
                        bad_inputs.push((id.clone(), e));
                        json!({})
                    });
                    content.push(Block::ToolUse { id, name, input });
                }
                Building::Thinking {
                    mut raw,
                    text,
                    signature,
                } => {
                    raw.insert("thinking".into(), json!(text));
                    raw.insert("signature".into(), json!(signature));
                    content.push(Block::Opaque {
                        provider: "anthropic".into(),
                        raw: Value::Object(raw),
                        shown: (!text.is_empty()).then_some(text),
                    });
                }
                Building::Raw(raw) => content.push(Block::Opaque {
                    provider: "anthropic".into(),
                    raw,
                    shown: None,
                }),
            }
        }
        Ok(Reply {
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop: self
                .stop
                .unwrap_or_else(|| StopReason::Other("none given".into())),
            usage: self.usage,
            bad_inputs,
        })
    }
}

fn index_of(v: &Value) -> Result<u64, ProviderError> {
    v.get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| proto("a block event without `index`"))
}

fn stop_reason(reason: &str, event: &Value) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
        "refusal" => {
            let details = event.pointer("/delta/stop_details");
            let category = details
                .and_then(|d| d.get("category"))
                .and_then(Value::as_str);
            let explanation = details
                .and_then(|d| d.get("explanation"))
                .and_then(Value::as_str);
            StopReason::Refusal(match (category, explanation) {
                (Some(c), Some(e)) => Some(format!("{c}: {e}")),
                (Some(c), None) => Some(c.to_owned()),
                (None, e) => e.map(str::to_owned),
            })
        }
        other => StopReason::Other(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::provider::ToolSpec;
    use crate::sse::SseParser;

    /// Runs a recorded stream through the parser.
    pub(crate) fn replay(stream: &str) -> (Result<Reply, ProviderError>, Vec<Delta>) {
        let mut deltas = Vec::new();
        let mut state = StreamState::default();
        let mut parser = SseParser::new();
        let mut on = |d: Delta| deltas.push(d);
        let mut result = Ok(());
        'outer: for ev in parser.feed(stream.as_bytes()).expect("sse") {
            match state.event(&ev.event, &ev.data, &mut on) {
                Ok(true) => {}
                Ok(false) => break 'outer,
                Err(e) => {
                    result = Err(e);
                    break 'outer;
                }
            }
        }
        (result.and_then(|()| state.finish()), deltas)
    }

    #[test]
    fn a_text_and_tool_turn() {
        let (reply, deltas) = replay(include_str!("../../tests/fixtures/anthropic/tool_turn.sse"));
        let reply = reply.expect("reply");
        assert_eq!(reply.stop, StopReason::ToolUse);
        assert_eq!(
            reply.usage,
            Usage {
                input: 412,
                output: 87,
                cache_read: 1800,
                cache_write: 0
            }
        );
        let blocks = &reply.message.content;
        assert!(
            matches!(&blocks[0], Block::Opaque { raw, .. } if raw["signature"] == "EqQBCkYIBBgCKkB")
        );
        assert_eq!(
            blocks[1],
            Block::Text {
                text: "I'll run the tests first.".into()
            }
        );
        assert_eq!(
            blocks[2],
            Block::ToolUse {
                id: "toolu_01A".into(),
                name: "run_command".into(),
                input: json!({"command": "cargo test", "timeout": 120}),
            }
        );
        assert!(deltas.contains(&Delta::ToolStart {
            name: "run_command".into()
        }));
        assert_eq!(
            deltas
                .iter()
                .filter(|d| matches!(d, Delta::Text(_)))
                .count(),
            2
        );
    }

    #[test]
    fn refusals_errors_and_cut_streams() {
        let (reply, _) = replay(include_str!("../../tests/fixtures/anthropic/refusal.sse"));
        assert_eq!(
            reply.expect("reply").stop,
            StopReason::Refusal(Some("cyber".into()))
        );

        let (reply, _) = replay(include_str!(
            "../../tests/fixtures/anthropic/overloaded.sse"
        ));
        let err = reply.expect_err("overloaded");
        assert_eq!(err.kind, ErrorKind::RateLimited);
        assert!(err.retryable());

        let (reply, _) =
            replay("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n");
        assert_eq!(reply.expect_err("cut").kind, ErrorKind::Network);
    }

    #[test]
    fn bad_tool_input_is_reported_not_run() {
        let stream = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"write_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"a\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":9}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let reply = replay(stream).0.expect("reply");
        assert_eq!(reply.stop, StopReason::MaxTokens);
        assert_eq!(reply.bad_inputs.len(), 1);
        assert_eq!(reply.bad_inputs[0].0, "t");
    }

    #[test]
    fn request_body_shape() {
        let req = ChatRequest {
            model: "fixture-model".into(),
            system: "be brief".into(),
            messages: Arc::new(vec![
                Message::user_text("hi"),
                Message {
                    role: Role::Assistant,
                    content: vec![
                        Block::Opaque {
                            provider: "anthropic".into(),
                            raw: json!({"type": "thinking", "thinking": "", "signature": "s"}),
                            shown: None,
                        },
                        Block::Opaque {
                            provider: "kimi".into(),
                            raw: json!({"reasoning_content": "x"}),
                            shown: None,
                        },
                        Block::ToolUse {
                            id: "t".into(),
                            name: "list_dir".into(),
                            input: json!({}),
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![Block::ToolResult {
                        tool_use_id: "t".into(),
                        content: "a\nb".into(),
                        is_error: true,
                    }],
                },
            ]),
            tools: Arc::new(vec![ToolSpec {
                name: "list_dir".into(),
                description: "List a folder".into(),
                schema: json!({"type": "object"}),
            }]),
            max_tokens: None,
        };
        insta::assert_snapshot!(
            serde_json::to_string_pretty(&request_body(&req, 1000)).expect("json")
        );
    }

    #[test]
    fn model_pages() {
        let page = json!({
            "data": [{"id": "fixture-a", "display_name": "Fixture A", "max_input_tokens": 1000000, "max_tokens": 128000, "type": "model"}],
            "has_more": true, "first_id": "fixture-a", "last_id": "fixture-a"
        });
        let (models, more, last) = parse_models_page(&page).expect("page");
        assert_eq!(models[0].context, Some(1_000_000));
        assert_eq!(models[0].max_output, Some(128_000));
        assert!(more);
        assert_eq!(last.as_deref(), Some("fixture-a"));
        assert_eq!(url_escape("a b/c"), "a%20b%2Fc");
    }
}
