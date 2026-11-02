// SPDX-License-Identifier: GPL-3.0-or-later
//! LLM providers behind one trait (ARCHITECTURE.md §11, D-058).
//!
//! A provider lists its models and streams a chat turn. Model names are
//! never hardcoded: they come from [`Provider::list_models`], or the user
//! types one when a provider cannot list them.
//!
//! The conversation is provider-neutral ([`Message`], [`Block`]), except
//! for [`Block::Opaque`]: blocks a provider needs back unchanged on later
//! turns (Anthropic's `thinking` blocks with their signatures), kept
//! verbatim. Histories are append-only, so what was sent once is sent the
//! same way again.

pub mod anthropic;
pub mod http;
pub mod openai;

use std::fmt;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use termos_core::config::agents::{Price, ProviderConfig, ProviderKind};

/// A model a provider offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    /// A display name, when the provider gives one.
    pub name: Option<String>,
    /// Context window in tokens, when listed.
    pub context: Option<u64>,
    /// Most output tokens per reply, when listed.
    pub max_output: Option<u64>,
    /// Prices from the provider's own list (OpenRouter has them).
    pub price: Option<ListedPrice>,
}

/// Prices a provider lists, in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ListedPrice {
    pub input: f64,
    pub output: f64,
}

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One piece of a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    /// The model asks to run a tool.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// What a tool returned, in the next user message.
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// A provider-specific block sent back exactly as received.
    Opaque {
        /// The provider kind that produced it (`anthropic`, `kimi`…).
        provider: String,
        raw: Value,
        /// Readable text for the window (a reasoning summary), if any.
        #[serde(default)]
        shown: Option<String>,
    },
}

/// One message of a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![Block::Text { text: text.into() }],
        }
    }

    /// The text blocks joined.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the input object.
    pub schema: Value,
}

/// One chat turn to stream.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Arc<Vec<Message>>,
    pub tools: Arc<Vec<ToolSpec>>,
    /// Most tokens for the reply; `None`: the provider's default.
    pub max_tokens: Option<u64>,
}

/// Tokens a turn used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input not read from the prompt cache.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    /// Cost in US dollars at `price` (per million tokens).
    pub fn cost(&self, price: &Price) -> f64 {
        let per = |tokens: u64, usd: f64| tokens as f64 * usd / 1_000_000.0;
        per(self.input, price.input)
            + per(self.output, price.output)
            + per(self.cache_read, price.cache_read.unwrap_or(price.input))
            + per(self.cache_write, price.cache_write.unwrap_or(price.input))
    }
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "kebab-case")]
pub enum StopReason {
    /// It finished its answer.
    EndTurn,
    /// It wants tools run.
    ToolUse,
    /// It hit the token limit.
    MaxTokens,
    /// It declined (with the provider's category or explanation).
    Refusal(Option<String>),
    Other(String),
}

/// Streamed pieces, for showing progress.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    /// Readable reasoning (when the provider shows any).
    Thinking(String),
    /// A tool call started (its input is still streaming).
    ToolStart {
        name: String,
    },
}

/// A finished turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// The assistant message, complete, to append to the history.
    pub message: Message,
    pub stop: StopReason,
    pub usage: Usage,
    /// Tool calls whose input was not valid JSON: id → problem. Their
    /// [`Block::ToolUse`] carries an empty object instead.
    pub bad_inputs: Vec<(String, String)>,
}

/// What went wrong talking to a provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The key is missing or refused (401/403).
    Auth,
    /// Too many requests, or the service is overloaded: try later.
    RateLimited,
    /// The provider failed (5xx).
    Server,
    /// The request was refused (4xx): a bad model name, too long…
    Request,
    /// Could not connect, or the connection broke.
    Network,
    /// The answer was not what the API documents.
    Protocol,
    /// Not configured well enough to talk to the provider.
    Config,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Worth trying the same request again after a pause.
    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::RateLimited | ErrorKind::Server | ErrorKind::Network
        )
    }
}

/// Streams a chat turn. `on_delta` sees progress; the future resolves to
/// the whole reply. Dropping the future cancels the request.
pub type ChatFuture<'a> = BoxFuture<'a, Result<Reply, ProviderError>>;

/// An LLM API.
pub trait Provider: Send + Sync {
    /// The provider's name in the config.
    fn name(&self) -> &str;
    fn kind(&self) -> ProviderKind;
    /// The models it offers, newest first where the API says.
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, ProviderError>>;
    fn chat<'a>(
        &'a self,
        req: &'a ChatRequest,
        on_delta: &'a mut (dyn FnMut(Delta) + Send),
    ) -> ChatFuture<'a>;
}

impl fmt::Debug for dyn Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Provider({} {})", self.name(), self.kind())
    }
}

/// The address a provider kind uses when the config names none.
pub fn default_base_url(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::Anthropic => Some("https://api.anthropic.com"),
        ProviderKind::Openai => Some("https://api.openai.com/v1"),
        ProviderKind::Kimi => Some("https://api.moonshot.ai/v1"),
        // TODO(M7): find the model router's address and token by itself once `termos models serve` exists.
        ProviderKind::OpenaiCompatible | ProviderKind::Local => None,
    }
}

/// Builds the provider `name` from its config, with its key (if it has
/// one) already read from the secret store.
pub fn build(
    name: &str,
    cfg: &ProviderConfig,
    key: Option<SecretString>,
) -> Result<Arc<dyn Provider>, ProviderError> {
    let base = match (&cfg.base_url, default_base_url(cfg.kind)) {
        (Some(url), _) => url.trim_end_matches('/').to_owned(),
        (None, Some(url)) => url.to_owned(),
        (None, None) => {
            return Err(ProviderError::new(
                ErrorKind::Config,
                format!(
                    "provider {name}: a {} provider needs `base-url` in [agents.providers.{name}]",
                    cfg.kind
                ),
            ));
        }
    };
    if base.starts_with("https://") && !http::tls_available() {
        return Err(ProviderError::new(
            ErrorKind::Config,
            format!("provider {name}: this build of TermOS has no TLS, so it cannot reach {base}"),
        ));
    }
    match cfg.kind {
        ProviderKind::Anthropic => {
            let key = key.ok_or_else(|| missing_key(name))?;
            Ok(Arc::new(anthropic::Anthropic::new(
                name,
                base,
                key,
                cfg.max_tokens,
            )))
        }
        ProviderKind::Openai | ProviderKind::Kimi => {
            let key = key.ok_or_else(|| missing_key(name))?;
            Ok(Arc::new(openai::OpenAi::new(
                name,
                cfg.kind,
                base,
                Some(key),
                cfg.max_tokens,
            )))
        }
        ProviderKind::OpenaiCompatible | ProviderKind::Local => Ok(Arc::new(openai::OpenAi::new(
            name,
            cfg.kind,
            base,
            key,
            cfg.max_tokens,
        ))),
    }
}

fn missing_key(name: &str) -> ProviderError {
    ProviderError::new(
        ErrorKind::Config,
        format!(
            "provider {name} needs an API key: add `key = \"keyring:{name}\"` to \
             [agents.providers.{name}] and run `termos secret set {name}`"
        ),
    )
}

/// The price for `model`: the config's, else the provider's list.
pub fn price_for(cfg: &ProviderConfig, model: &str, listed: Option<&ModelInfo>) -> Option<Price> {
    cfg.prices.get(model).copied().or_else(|| {
        listed.and_then(|m| m.price).map(|p| Price {
            input: p.input,
            output: p.output,
            cache_read: None,
            cache_write: None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_uses_cache_prices_when_given() {
        let usage = Usage {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_write: 0,
        };
        let plain = Price {
            input: 3.0,
            output: 15.0,
            cache_read: None,
            cache_write: None,
        };
        assert!((usage.cost(&plain) - (3.0 + 7.5 + 6.0)).abs() < 1e-9);
        let cached = Price {
            cache_read: Some(0.3),
            ..plain
        };
        assert!((usage.cost(&cached) - (3.0 + 7.5 + 0.6)).abs() < 1e-9);
    }

    #[test]
    fn history_round_trips_through_json() {
        let m = Message {
            role: Role::Assistant,
            content: vec![
                Block::Opaque {
                    provider: "anthropic".into(),
                    raw: serde_json::json!({"type": "thinking", "thinking": "", "signature": "sig"}),
                    shown: None,
                },
                Block::Text { text: "hi".into() },
                Block::ToolUse {
                    id: "t1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "a"}),
                },
            ],
        };
        let json = serde_json::to_string(&m).expect("json");
        assert_eq!(serde_json::from_str::<Message>(&json).expect("back"), m);
    }
}
