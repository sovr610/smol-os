// SPDX-License-Identifier: GPL-3.0-or-later
//! The OpenAI Chat Completions API, which OpenAI, Moonshot's Kimi, Ollama,
//! vLLM, LM Studio, OpenRouter and TermOS's own router (M7) all speak.
//!
//! - Models come from `GET <base>/models`. OpenRouter's list carries
//!   prices and context lengths, Moonshot's context lengths; both are
//!   read when present. A server that cannot list models leaves the user
//!   to type one.
//! - A turn is `POST <base>/chat/completions` with `stream: true` and
//!   usage reporting; tool calls arrive as indexed argument fragments.
//! - Reasoning text (`reasoning_content`, `reasoning`) is shown. Kimi wants
//!   it back with its tool calls, so for Kimi it is kept and resent; other
//!   servers do not take it back.

use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value, json};
use termos_core::config::agents::ProviderKind;

use super::http::{client, for_each_event, network, status_error};
use super::{
    Block, ChatFuture, ChatRequest, Delta, ErrorKind, ListedPrice, Message, ModelInfo, Provider,
    ProviderError, Reply, Role, StopReason, Usage,
};

pub struct OpenAi {
    name: String,
    kind: ProviderKind,
    base: String,
    key: Option<SecretString>,
    max_tokens: Option<u32>,
}

impl OpenAi {
    pub fn new(
        name: &str,
        kind: ProviderKind,
        base: String,
        key: Option<SecretString>,
        max_tokens: Option<u32>,
    ) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            base,
            key,
            max_tokens,
        }
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        let mut req = client()?.request(method, format!("{}{path}", self.base));
        if let Some(key) = &self.key {
            let mut value =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key.expose_secret()))
                    .map_err(|_| {
                        ProviderError::new(
                            ErrorKind::Config,
                            "the API key has characters a header cannot carry",
                        )
                    })?;
            value.set_sensitive(true);
            req = req.header(reqwest::header::AUTHORIZATION, value);
        }
        Ok(req)
    }

    async fn list(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let resp = self
            .request(reqwest::Method::GET, "/models")?
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
        parse_models(&page)
    }

    async fn turn(
        &self,
        req: &ChatRequest,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<Reply, ProviderError> {
        let body = request_body(
            req,
            self.kind,
            req.max_tokens.or(self.max_tokens.map(u64::from)),
        );
        let resp = self
            .request(reqwest::Method::POST, "/chat/completions")?
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| network(&e))?;
        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }
        let mut state = StreamState::new(self.kind);
        for_each_event(resp, |ev| state.event(&ev.data, on_delta)).await?;
        state.finish()
    }
}

impl Provider for OpenAi {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> ProviderKind {
        self.kind
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

/// Whether reasoning goes back with the assistant message.
fn echoes_reasoning(kind: ProviderKind) -> bool {
    kind == ProviderKind::Kimi
}

/// `GET /models`, newest first when the server says when models were
/// made, else by name.
pub(crate) fn parse_models(page: &Value) -> Result<Vec<ModelInfo>, ProviderError> {
    let data = page
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::new(ErrorKind::Protocol, "model list without `data`"))?;
    let per_million = |v: Option<&Value>| -> Option<f64> {
        let v = v?;
        let per_token = v.as_f64().or_else(|| v.as_str()?.parse().ok())?;
        (per_token >= 0.0).then_some(per_token * 1_000_000.0)
    };
    let mut models: Vec<(i64, ModelInfo)> = data
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_owned();
            let price = match (
                per_million(m.pointer("/pricing/prompt")),
                per_million(m.pointer("/pricing/completion")),
            ) {
                (Some(input), Some(output)) => Some(ListedPrice { input, output }),
                _ => None,
            };
            let info = ModelInfo {
                name: m
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| *n != id)
                    .map(str::to_owned),
                id,
                context: m
                    .get("context_length")
                    .or_else(|| m.get("context_window"))
                    .and_then(Value::as_u64),
                max_output: m
                    .pointer("/top_provider/max_completion_tokens")
                    .and_then(Value::as_u64),
                price,
            };
            Some((m.get("created").and_then(Value::as_i64).unwrap_or(0), info))
        })
        .collect();
    models.sort_by(|(ca, a), (cb, b)| cb.cmp(ca).then_with(|| a.id.cmp(&b.id)));
    Ok(models.into_iter().map(|(_, m)| m).collect())
}

/// The request body for a turn.
pub(crate) fn request_body(
    req: &ChatRequest,
    kind: ProviderKind,
    max_tokens: Option<u64>,
) -> Value {
    let mut messages = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({"role": "system", "content": req.system}));
    }
    for m in req.messages.iter() {
        messages.extend(message_json(m, kind));
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    if let Some(n) = max_tokens {
        // OpenAI's own API renamed the field; the others kept the old one.
        let field = if kind == ProviderKind::Openai {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body.insert(field.into(), json!(n));
    }
    if !req.tools.is_empty() {
        let tools = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {"name": t.name, "description": t.description, "parameters": t.schema},
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    Value::Object(body)
}

fn message_json(m: &Message, kind: ProviderKind) -> Vec<Value> {
    match m.role {
        Role::User => {
            // Tool results are messages of their own, before any text.
            let mut out: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => Some(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": if *is_error { format!("error: {content}") } else { content.clone() },
                    })),
                    _ => None,
                })
                .collect();
            let text = m.text();
            if !text.is_empty() {
                out.push(json!({"role": "user", "content": text}));
            }
            out
        }
        Role::Assistant => {
            let mut msg = Map::new();
            msg.insert("role".into(), json!("assistant"));
            let text = m.text();
            msg.insert(
                "content".into(),
                if text.is_empty() {
                    Value::Null
                } else {
                    json!(text)
                },
            );
            let calls: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, name, input } => Some(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": input.to_string()},
                    })),
                    _ => None,
                })
                .collect();
            if !calls.is_empty() {
                msg.insert("tool_calls".into(), Value::Array(calls));
            }
            if echoes_reasoning(kind) {
                for b in &m.content {
                    if let Block::Opaque { provider, raw, .. } = b
                        && provider == kind.name()
                        && let Some(r) = raw.get("reasoning_content")
                    {
                        msg.insert("reasoning_content".into(), r.clone());
                    }
                }
            }
            vec![Value::Object(msg)]
        }
    }
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    args: String,
}

struct StreamState {
    kind: ProviderKind,
    text: String,
    reasoning: String,
    calls: Vec<Call>,
    finish: Option<String>,
    usage: Usage,
    done: bool,
}

impl StreamState {
    fn new(kind: ProviderKind) -> Self {
        Self {
            kind,
            text: String::new(),
            reasoning: String::new(),
            calls: Vec::new(),
            finish: None,
            usage: Usage::default(),
            done: false,
        }
    }

    fn event(
        &mut self,
        data: &str,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<bool, ProviderError> {
        let data = data.trim();
        if data == "[DONE]" {
            self.done = true;
            return Ok(false);
        }
        let v: Value = serde_json::from_str(data).map_err(|e| {
            ProviderError::new(
                ErrorKind::Protocol,
                format!("a stream chunk that is not JSON: {e}"),
            )
        })?;
        if let Some(err) = v.get("error") {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| err.as_str())
                .unwrap_or("the stream reported an error");
            return Err(ProviderError::new(ErrorKind::Server, message));
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let get = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
            let cached = get("/prompt_tokens_details/cached_tokens");
            self.usage = Usage {
                input: get("/prompt_tokens").saturating_sub(cached),
                output: get("/completion_tokens"),
                cache_read: cached,
                cache_write: 0,
            };
        }
        let Some(choice) = v.pointer("/choices/0") else {
            return Ok(true);
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(t) = delta.get("content").and_then(Value::as_str)
                && !t.is_empty()
            {
                self.text.push_str(t);
                on_delta(Delta::Text(t.to_owned()));
            }
            let reasoning = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str);
            if let Some(r) = reasoning
                && !r.is_empty()
            {
                self.reasoning.push_str(r);
                on_delta(Delta::Thinking(r.to_owned()));
            }
            for tc in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let index = tc
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|i| usize::try_from(i).ok())
                    .unwrap_or(self.calls.len().saturating_sub(1));
                if index > 1024 {
                    return Err(ProviderError::new(
                        ErrorKind::Protocol,
                        "too many tool calls",
                    ));
                }
                while self.calls.len() <= index {
                    self.calls.push(Call::default());
                }
                let call = &mut self.calls[index];
                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                    id.clone_into(&mut call.id);
                }
                if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str)
                    && !name.is_empty()
                {
                    if call.name.is_empty() {
                        on_delta(Delta::ToolStart {
                            name: name.to_owned(),
                        });
                    }
                    call.name.push_str(name);
                }
                if let Some(args) = tc.pointer("/function/arguments").and_then(Value::as_str) {
                    call.args.push_str(args);
                }
            }
        }
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(f.to_owned());
        }
        Ok(true)
    }

    fn finish(self) -> Result<Reply, ProviderError> {
        if !self.done && self.finish.is_none() {
            return Err(ProviderError::new(
                ErrorKind::Network,
                "the reply stopped before it was complete",
            ));
        }
        let mut content = Vec::new();
        if !self.reasoning.is_empty() {
            content.push(Block::Opaque {
                provider: self.kind.name().to_owned(),
                raw: json!({"reasoning_content": self.reasoning}),
                shown: Some(self.reasoning.clone()),
            });
        }
        if !self.text.is_empty() {
            content.push(Block::Text { text: self.text });
        }
        let mut bad_inputs = Vec::new();
        for (i, call) in self.calls.into_iter().enumerate() {
            let id = if call.id.is_empty() {
                format!("call_{i}")
            } else {
                call.id
            };
            let input = if call.args.trim().is_empty() {
                json!({})
            } else {
                match serde_json::from_str::<Value>(&call.args) {
                    Ok(v) if v.is_object() => v,
                    Ok(_) => {
                        bad_inputs.push((id.clone(), "the input is not a JSON object".into()));
                        json!({})
                    }
                    Err(e) => {
                        bad_inputs.push((id.clone(), e.to_string()));
                        json!({})
                    }
                }
            };
            content.push(Block::ToolUse {
                id,
                name: call.name,
                input,
            });
        }
        let has_calls = content.iter().any(|b| matches!(b, Block::ToolUse { .. }));
        let stop = match self.finish.as_deref() {
            Some("tool_calls" | "function_call") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal(Some("content filter".into())),
            // Some servers say "stop" even when they called tools.
            _ if has_calls => StopReason::ToolUse,
            Some("stop") | None => StopReason::EndTurn,
            Some(other) => StopReason::Other(other.to_owned()),
        };
        Ok(Reply {
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop,
            usage: self.usage,
            bad_inputs,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::provider::ToolSpec;
    use crate::sse::SseParser;

    fn replay(kind: ProviderKind, stream: &str) -> (Result<Reply, ProviderError>, Vec<Delta>) {
        let mut deltas = Vec::new();
        let mut state = StreamState::new(kind);
        let mut on = |d: Delta| deltas.push(d);
        let mut parser = SseParser::new();
        for ev in parser.feed(stream.as_bytes()).expect("sse") {
            match state.event(&ev.data, &mut on) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => return (Err(e), deltas),
            }
        }
        (state.finish(), deltas)
    }

    #[test]
    fn tool_calls_in_fragments() {
        let (reply, deltas) = replay(
            ProviderKind::Openai,
            include_str!("../../tests/fixtures/openai/tool_turn.sse"),
        );
        let reply = reply.expect("reply");
        assert_eq!(reply.stop, StopReason::ToolUse);
        assert_eq!(
            reply.usage,
            Usage {
                input: 300,
                output: 40,
                cache_read: 1200,
                cache_write: 0
            }
        );
        assert_eq!(
            reply.message.content[0],
            Block::Text {
                text: "Checking both.".into()
            }
        );
        assert_eq!(
            reply.message.content[1],
            Block::ToolUse {
                id: "call_a".into(),
                name: "read_file".into(),
                input: json!({"path": "src/lib.rs"}),
            }
        );
        assert_eq!(
            reply.message.content[2],
            Block::ToolUse {
                id: "call_b".into(),
                name: "list_dir".into(),
                input: json!({"path": "."}),
            }
        );
        assert_eq!(
            deltas
                .iter()
                .filter(|d| matches!(d, Delta::ToolStart { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn kimi_reasoning_is_kept_and_sent_back() {
        let (reply, deltas) = replay(
            ProviderKind::Kimi,
            include_str!("../../tests/fixtures/openai/kimi_reasoning.sse"),
        );
        let reply = reply.expect("reply");
        assert!(deltas.contains(&Delta::Thinking("Look at the test first.".into())));
        let req = ChatRequest {
            model: "fixture-kimi".into(),
            system: String::new(),
            messages: Arc::new(vec![Message::user_text("fix it"), reply.message.clone()]),
            tools: Arc::new(Vec::new()),
            max_tokens: None,
        };
        let body = request_body(&req, ProviderKind::Kimi, None);
        assert_eq!(
            body["messages"][1]["reasoning_content"],
            "Look at the test first."
        );
        // Other servers do not get it back.
        let body = request_body(&req, ProviderKind::OpenaiCompatible, None);
        assert!(body["messages"][1].get("reasoning_content").is_none());
    }

    #[test]
    fn errors_and_cut_streams() {
        let (reply, _) = replay(
            ProviderKind::OpenaiCompatible,
            "data: {\"error\":{\"message\":\"model not loaded\"}}\n\n",
        );
        assert_eq!(reply.expect_err("error").message, "model not loaded");
        let (reply, _) = replay(
            ProviderKind::OpenaiCompatible,
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hal\"}}]}\n\n",
        );
        assert_eq!(reply.expect_err("cut").kind, ErrorKind::Network);
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
                        Block::Text {
                            text: "Looking.".into(),
                        },
                        Block::ToolUse {
                            id: "c1".into(),
                            name: "list_dir".into(),
                            input: json!({"path": "."}),
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![Block::ToolResult {
                        tool_use_id: "c1".into(),
                        content: "a\nb".into(),
                        is_error: false,
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
            serde_json::to_string_pretty(&request_body(&req, ProviderKind::Openai, Some(2048)))
                .expect("json")
        );
    }

    #[test]
    fn model_lists_with_and_without_prices() {
        let page: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/openai/models_openrouter.json"
        ))
        .expect("json");
        let models = parse_models(&page).expect("models");
        assert_eq!(models[0].id, "fixture/newer");
        let priced = models
            .iter()
            .find(|m| m.id == "fixture/priced")
            .expect("priced");
        let price = priced.price.expect("price");
        assert!((price.input - 3.0).abs() < 1e-9 && (price.output - 15.0).abs() < 1e-9);
        assert_eq!(priced.context, Some(200_000));
        assert!(
            models
                .iter()
                .find(|m| m.id == "fixture/newer")
                .and_then(|m| m.price)
                .is_none()
        );
    }
}
