// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTP client every provider, `fetch_url` and MCP share.
//!
//! - TLS is rustls with ring, installed once (the `tls` feature, D-058).
//! - No idle connections are pooled: hyper's pool would keep a timer
//!   ticking while nothing happens (ARCHITECTURE.md §5.5). A turn costs a
//!   new connection instead.
//! - Requests and headers are never logged: they carry keys.

use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt as _;
use serde_json::Value;

use super::{ErrorKind, ProviderError};
use crate::sse::{SseEvent, SseParser};

/// Whether this build can make HTTPS requests.
pub(crate) fn tls_available() -> bool {
    cfg!(feature = "tls")
}

/// The shared client.
pub(crate) fn client() -> Result<&'static reqwest::Client, ProviderError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            #[cfg(feature = "tls")]
            {
                // Fails only if another provider is already installed,
                // which is just as good.
                let _ = rustls::crypto::ring::default_provider().install_default();
            }
            reqwest::Client::builder()
                .user_agent(concat!("termos/", env!("CARGO_PKG_VERSION")))
                .pool_max_idle_per_host(0)
                .connect_timeout(Duration::from_secs(20))
                // Providers send keep-alive pings while a model thinks.
                .read_timeout(Duration::from_secs(300))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| ProviderError::new(ErrorKind::Config, format!("cannot set up HTTP: {e}")))
}

/// Maps a transport error.
pub(crate) fn network(e: &reqwest::Error) -> ProviderError {
    // reqwest's text names the URL but never headers or bodies.
    let mut message = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        message.push_str(": ");
        message.push_str(&s.to_string());
        source = s.source();
    }
    ProviderError::new(ErrorKind::Network, message)
}

/// Turns a non-success response into an error, with the API's own
/// message when its body has one (`{"error": {"message": …}}`).
pub(crate) async fn status_error(resp: reqwest::Response) -> ProviderError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            let err = v.get("error")?;
            err.get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| err.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    let kind = match status.as_u16() {
        401 | 403 => ErrorKind::Auth,
        408 | 409 | 429 | 529 => ErrorKind::RateLimited,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::Request,
    };
    ProviderError::new(kind, format!("HTTP {}: {}", status.as_u16(), detail.trim()))
}

/// Reads a response body as server-sent events, handing each to `on`
/// until it returns `false` or the stream ends.
pub(crate) async fn for_each_event(
    resp: reqwest::Response,
    mut on: impl FnMut(SseEvent) -> Result<bool, ProviderError>,
) -> Result<(), ProviderError> {
    let mut parser = SseParser::new();
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| network(&e))?;
        let events = parser
            .feed(&chunk)
            .map_err(|e| ProviderError::new(ErrorKind::Protocol, e.to_string()))?;
        for ev in events {
            if !on(ev)? {
                return Ok(());
            }
        }
    }
    Ok(())
}
