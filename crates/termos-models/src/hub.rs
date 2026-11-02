// SPDX-License-Identifier: GPL-3.0-or-later
//! The Hugging Face Hub's API: search, a revision's commit, a repository's
//! files with their sizes and checksums, and file downloads (D-066).
//!
//! `HF_ENDPOINT` changes the address, as for `huggingface_hub` (tests use
//! it for a mock Hub). A token is sent only to the Hub's own address.

use std::sync::OnceLock;
use std::time::Duration;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::Value;

/// Why the Hub could not be used.
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error("{0}")]
    Network(String),
    #[error("{repo} is not on the Hub (or is private: set [models] token or HF_TOKEN)")]
    NotFound { repo: String },
    #[error(
        "{repo} needs a token with access: accept its license at {url} and set [models] token or HF_TOKEN"
    )]
    Gated { repo: String, url: String },
    #[error("the Hub answered HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("the Hub's answer was not what its API documents: {0}")]
    Protocol(String),
    #[error("{0}")]
    Config(String),
}

/// A search result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Found {
    pub id: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub likes: u64,
    #[serde(default)]
    pub pipeline_tag: Option<String>,
}

/// A file in a repository at a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFile {
    pub path: String,
    pub size: u64,
    /// SHA-256 of the content (LFS files).
    pub sha256: Option<String>,
    /// The git blob id (SHA-1 of `blob <size>\0<content>`).
    pub git_oid: String,
}

impl RepoFile {
    /// The name the cache stores it under: the SHA-256 for LFS files, the
    /// git blob id otherwise (as `huggingface_hub` does).
    pub fn etag(&self) -> &str {
        self.sha256.as_deref().unwrap_or(&self.git_oid)
    }
}

/// A Hub client.
#[derive(Debug, Clone)]
pub struct Hub {
    base: String,
    /// For gated and private repositories (D-017: a `secrecy` type).
    token: Option<SecretString>,
}

/// The HTTP client the Hub, the router and its tests share. reqwest is
/// built with rustls but no crypto provider; the `tls` feature installs
/// ring. Without it no client can be built at all (such builds exist only
/// for cross-target checks), and saying so is the error.
pub fn client() -> Result<&'static reqwest::Client, HubError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            if !cfg!(feature = "tls") {
                return Err("this build of TermOS has no TLS support (the `tls` feature)".into());
            }
            #[cfg(feature = "tls")]
            {
                let _ = rustls::crypto::ring::default_provider().install_default();
            }
            reqwest::Client::builder()
                .user_agent(concat!("termos/", env!("CARGO_PKG_VERSION")))
                .pool_max_idle_per_host(0)
                .connect_timeout(Duration::from_secs(20))
                .read_timeout(Duration::from_secs(120))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| HubError::Config(format!("cannot set up HTTP: {e}")))
}

pub(crate) fn net(e: &reqwest::Error) -> HubError {
    let mut message = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        message.push_str(": ");
        message.push_str(&s.to_string());
        source = s.source();
    }
    HubError::Network(message)
}

/// Percent-encodes a path segment.
fn enc(s: &str) -> String {
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

/// A repository id: `org/name` (or a bare name), safe characters only.
pub fn valid_repo(repo: &str) -> bool {
    let parts: Vec<&str> = repo.split('/').collect();
    !repo.is_empty()
        && parts.len() <= 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 96
                && *p != "."
                && *p != ".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

impl Hub {
    /// The Hub at `HF_ENDPOINT` (default `https://huggingface.co`).
    pub fn new(token: Option<SecretString>) -> Result<Self, HubError> {
        let base = std::env::var("HF_ENDPOINT")
            .ok()
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| "https://huggingface.co".into());
        Self::at(&base, token)
    }

    pub fn at(base: &str, token: Option<SecretString>) -> Result<Self, HubError> {
        let base = base.trim_end_matches('/').to_owned();
        if base.starts_with("https://") && !cfg!(feature = "tls") {
            return Err(HubError::Config(format!(
                "this build of TermOS has no TLS, so it cannot reach {base}"
            )));
        }
        Ok(Self {
            base,
            token: token.filter(|t| !t.expose_secret().is_empty()),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub(crate) fn get(&self, url: &str) -> Result<reqwest::RequestBuilder, HubError> {
        let mut req = client()?.get(url);
        if let Some(t) = &self.token {
            let mut v =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", t.expose_secret()))
                    .map_err(|_| {
                        HubError::Config(
                            "the Hugging Face token has characters a header cannot carry".into(),
                        )
                    })?;
            v.set_sensitive(true);
            req = req.header(reqwest::header::AUTHORIZATION, v);
        }
        Ok(req)
    }

    pub(crate) async fn check(
        &self,
        resp: reqwest::Response,
        repo: &str,
    ) -> Result<reqwest::Response, HubError> {
        let status = resp.status().as_u16();
        if resp.status().is_success() || status == 206 {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| body.chars().take(200).collect());
        Err(match status {
            401 | 403 if message.to_lowercase().contains("gated") || self.token.is_none() => {
                HubError::Gated {
                    repo: repo.to_owned(),
                    url: format!("{}/{repo}", self.base),
                }
            }
            404 | 401 => HubError::NotFound {
                repo: repo.to_owned(),
            },
            _ => HubError::Http { status, message },
        })
    }

    async fn json(&self, url: &str, repo: &str) -> Result<(Value, Option<String>), HubError> {
        let resp = self.get(url)?.send().await.map_err(|e| net(&e))?;
        let resp = self.check(resp, repo).await?;
        let next = resp
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|v| v.to_str().ok())
            .and_then(next_link);
        let v = resp
            .json()
            .await
            .map_err(|e| HubError::Protocol(e.to_string()))?;
        Ok((v, next))
    }

    /// Models matching `query`; `gguf` keeps GGUF repositories only.
    pub async fn search(
        &self,
        query: &str,
        gguf: bool,
        limit: u32,
    ) -> Result<Vec<Found>, HubError> {
        let mut url = format!(
            "{}/api/models?search={}&sort=downloads&direction=-1&limit={limit}",
            self.base,
            enc(query)
        );
        if gguf {
            url.push_str("&filter=gguf");
        }
        let (v, _) = self.json(&url, query).await?;
        serde_json::from_value(v).map_err(|e| HubError::Protocol(e.to_string()))
    }

    /// The commit a revision (a branch, tag or commit) points to.
    pub async fn commit(&self, repo: &str, revision: &str) -> Result<String, HubError> {
        let url = format!("{}/api/models/{repo}/revision/{}", self.base, enc(revision));
        let (v, _) = self.json(&url, repo).await?;
        v.get("sha")
            .and_then(Value::as_str)
            .filter(|s| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_owned)
            .ok_or_else(|| HubError::Protocol("the revision has no commit".into()))
    }

    /// Every file of `repo` at `commit`.
    pub async fn files(&self, repo: &str, commit: &str) -> Result<Vec<RepoFile>, HubError> {
        let mut url = format!(
            "{}/api/models/{repo}/tree/{commit}?recursive=true",
            self.base
        );
        let mut out = Vec::new();
        for _ in 0..100 {
            let (v, next) = self.json(&url, repo).await?;
            let items = v
                .as_array()
                .ok_or_else(|| HubError::Protocol("the file list is not a list".into()))?;
            for it in items {
                if it.get("type").and_then(Value::as_str) != Some("file") {
                    continue;
                }
                let (Some(path), Some(size), Some(oid)) = (
                    it.get("path").and_then(Value::as_str),
                    it.get("size").and_then(Value::as_u64),
                    it.get("oid").and_then(Value::as_str),
                ) else {
                    continue;
                };
                if !safe_path(path) {
                    return Err(HubError::Protocol(format!("an unsafe file path: {path}")));
                }
                out.push(RepoFile {
                    path: path.to_owned(),
                    size: it
                        .pointer("/lfs/size")
                        .and_then(Value::as_u64)
                        .unwrap_or(size),
                    sha256: it
                        .pointer("/lfs/oid")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    git_oid: oid.to_owned(),
                });
            }
            match next {
                Some(n) => url = n,
                None => break,
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// The address a file is downloaded from.
    pub fn file_url(&self, repo: &str, commit: &str, path: &str) -> String {
        let path: Vec<String> = path.split('/').map(enc).collect();
        format!("{}/{repo}/resolve/{commit}/{}", self.base, path.join("/"))
    }

    /// The first `len` bytes of a file (a GGUF header before downloading).
    pub async fn head_bytes(
        &self,
        repo: &str,
        commit: &str,
        path: &str,
        len: u64,
    ) -> Result<Vec<u8>, HubError> {
        let resp = self
            .get(&self.file_url(repo, commit, path))?
            .header(
                reqwest::header::RANGE,
                format!("bytes=0-{}", len.saturating_sub(1)),
            )
            .send()
            .await
            .map_err(|e| net(&e))?;
        let resp = self.check(resp, repo).await?;
        let mut out = Vec::new();
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt as _;
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.map_err(|e| net(&e))?);
            if out.len() as u64 >= len {
                out.truncate(usize::try_from(len).unwrap_or(usize::MAX));
                break;
            }
        }
        Ok(out)
    }
}

/// The `rel="next"` address of a `Link` header.
fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, rest) = part.split_once(';')?;
        rest.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    })
}

/// A relative path with no `..`, `.` or empty parts and no backslashes.
pub fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\\')
        && p.split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && !s.contains(':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_paths_links() {
        assert!(valid_repo("ggml-org/models"));
        assert!(valid_repo("gpt2"));
        assert!(!valid_repo("a/b/c"));
        assert!(!valid_repo("../x"));
        assert!(!valid_repo("a/b c"));
        assert!(safe_path("tinyllamas/stories260K.gguf"));
        assert!(!safe_path("../etc/passwd"));
        assert!(!safe_path("a//b"));
        assert!(!safe_path("C:/x"));
        assert_eq!(
            next_link(r#"<https://hf.example/api/models/a/b/tree/main?cursor=xyz>; rel="next""#),
            Some("https://hf.example/api/models/a/b/tree/main?cursor=xyz".into())
        );
        let hub = Hub::at(
            "http://hub.test/",
            Some(SecretString::from("hf_secret".to_owned())),
        )
        .expect("hub");
        assert_eq!(
            hub.file_url("o/r", "abc123", "dir/a b.gguf"),
            "http://hub.test/o/r/resolve/abc123/dir/a%20b.gguf"
        );
        assert!(!format!("{hub:?}").contains("hf_secret"));
    }
}
