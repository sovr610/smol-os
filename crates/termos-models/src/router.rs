// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos models serve`: one OpenAI-compatible endpoint in front of every
//! local model (D-021, D-068).
//!
//! - `GET /v1/models` lists the downloaded models; `POST /v1/chat/completions`,
//!   `/v1/completions` and `/v1/embeddings` go to the model the body names.
//! - Every request carries the router's token (`Authorization: Bearer`);
//!   it listens on localhost (or a Unix socket) only.
//! - A model's backend starts on its first request: `llama-server` for
//!   GGUF files, candle in-process for embedding models, a configured
//!   server (the Python sidecar) for the repositories it serves. When
//!   loading one would pass the memory budget, the least recently used
//!   others are stopped first.
//! - Responses are passed through as they stream.

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt as _;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Method, Request, Response, StatusCode};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use termos_core::config::models::{LlamaBuild, ModelsConfig};
use termos_hw::accel::Accelerators;
use tokio::sync::{Mutex, watch};

use crate::cache::{self, Cache};
use crate::catalog::{self, Kind, LocalModel};
use crate::fit::{self, Estimate};
use crate::llama;

type Body = BoxBody<Bytes, std::io::Error>;

/// The largest request body (a long conversation).
const MAX_BODY: usize = 32 << 20;

/// What the router runs with.
pub struct Options {
    pub cache: Cache,
    pub config: ModelsConfig,
    pub hw: Accelerators,
    /// Where `llama-server` comes from (`None`: GGUF models cannot run).
    pub llama: Option<llama::Source>,
    pub build: LlamaBuild,
    /// The token clients send.
    pub token: SecretString,
}

/// A running backend.
// One per loaded model, always behind an `Arc`: the size difference
// between the variants costs nothing.
#[allow(clippy::large_enum_variant)]
enum Backend {
    /// An OpenAI-compatible server on a local port.
    Http {
        base: String,
        key: SecretString,
        /// The model name it expects in requests.
        name: String,
        child: Mutex<tokio::process::Child>,
    },
    #[cfg(feature = "candle")]
    Embed(Arc<crate::embed::Embedder>),
}

struct Loaded {
    backend: Arc<Backend>,
    bytes: u64,
    used: Instant,
}

/// The router.
pub struct Router {
    opts: Options,
    loaded: Mutex<BTreeMap<String, Loaded>>,
    /// Starts happen one at a time.
    starting: Mutex<()>,
    /// Becomes true when the router is told to stop.
    stopping: watch::Sender<bool>,
    budget: u64,
}

fn json_response(status: StatusCode, v: &Value) -> Response<Body> {
    let body = Full::new(Bytes::from(v.to_string()))
        .map_err(|never: Infallible| match never {})
        .boxed();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body)
        .unwrap_or_else(|_| {
            Response::new(
                Full::new(Bytes::new())
                    .map_err(|never: Infallible| match never {})
                    .boxed(),
            )
        })
}

fn error(status: StatusCode, message: impl Into<String>) -> Response<Body> {
    let kind = match status.as_u16() {
        401 => "authentication_error",
        404 => "not_found_error",
        400 => "invalid_request_error",
        _ => "server_error",
    };
    json_response(
        status,
        &json!({"error": {"message": message.into(), "type": kind}}),
    )
}

/// Compares in time independent of where the inputs differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 128 random bits as hex.
pub fn token() -> Result<SecretString, String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(SecretString::from(
        b.iter().map(|x| format!("{x:02x}")).collect::<String>(),
    ))
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port())
}

impl Router {
    pub fn new(opts: Options) -> Self {
        let gpu: u64 = opts.hw.gpus.iter().filter_map(|g| g.total).sum();
        let auto = (opts.hw.memory_total + if opts.hw.unified_memory { 0 } else { gpu }) / 10 * 9;
        let budget = opts.config.memory_budget.map_or(auto, |b| b.0);
        Self {
            opts,
            loaded: Mutex::new(BTreeMap::new()),
            starting: Mutex::new(()),
            stopping: watch::Sender::new(false),
            budget,
        }
    }

    fn models(&self) -> Vec<LocalModel> {
        catalog::scan(&self.opts.cache, &self.opts.config.servers)
    }

    /// Tells every listener to stop (and then every backend).
    pub fn stop(&self) {
        self.stopping.send_replace(true);
    }

    /// Returns once the router is told to stop.
    pub async fn stopped(&self) {
        let mut rx = self.stopping.subscribe();
        let _ = rx.wait_for(|stop| *stop).await;
    }

    /// The ids of the models loaded now.
    pub async fn loaded(&self) -> Vec<String> {
        self.loaded.lock().await.keys().cloned().collect()
    }

    /// Handles one request.
    pub async fn handle(&self, req: Request<Incoming>) -> Response<Body> {
        let path = req.uri().path().to_owned();
        let method = req.method().clone();
        if method == Method::GET && path == "/health" {
            return json_response(StatusCode::OK, &json!({"status": "ok"}));
        }
        let auth = req
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !same(auth.as_bytes(), self.opts.token.expose_secret().as_bytes()) {
            return error(
                StatusCode::UNAUTHORIZED,
                "a valid router token is required (Authorization: Bearer …)",
            );
        }
        match (method, path.as_str()) {
            (Method::GET, "/v1/models") => {
                let loaded = self.loaded().await;
                let data: Vec<Value> = self
                    .models()
                    .iter()
                    .map(|m| {
                        json!({
                            "id": m.id, "object": "model", "created": 0, "owned_by": "termos",
                            "termos": {
                                "repo": m.repo,
                                "kind": match &m.kind { Kind::Gguf { .. } => "llama.cpp", Kind::Embedding => "candle", Kind::Server { .. } => "server" },
                                "size": m.size,
                                "loaded": loaded.contains(&m.id),
                            }
                        })
                    })
                    .collect();
                json_response(StatusCode::OK, &json!({"object": "list", "data": data}))
            }
            (Method::POST, "/admin/stop") => {
                self.stop();
                json_response(StatusCode::OK, &json!({"stopping": true}))
            }
            (Method::POST, "/v1/chat/completions" | "/v1/completions" | "/v1/embeddings") => {
                let body = match Limited::new(req.into_body(), MAX_BODY).collect().await {
                    Ok(b) => b.to_bytes(),
                    Err(_) => {
                        return error(StatusCode::PAYLOAD_TOO_LARGE, "the request is too large");
                    }
                };
                self.forward(&path, body).await
            }
            _ => error(StatusCode::NOT_FOUND, format!("no such endpoint: {path}")),
        }
    }

    async fn forward(&self, path: &str, body: Bytes) -> Response<Body> {
        let mut v: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    format!("the body is not JSON: {e}"),
                );
            }
        };
        let Some(id) = v.get("model").and_then(Value::as_str).map(str::to_owned) else {
            return error(StatusCode::BAD_REQUEST, "the body names no `model`");
        };
        let models = self.models();
        let Some(model) = catalog::find(&models, &id).cloned() else {
            return error(
                StatusCode::NOT_FOUND,
                format!("no local model `{id}`: `termos model pull {id}` fetches it"),
            );
        };
        let backend = match self.ensure(&model).await {
            Ok(b) => b,
            Err(e) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("{}: {e}", model.id),
                );
            }
        };
        match &*backend {
            Backend::Http {
                base, key, name, ..
            } => {
                v["model"] = json!(name);
                self.proxy(&format!("{base}{path}"), key, v.to_string())
                    .await
            }
            #[cfg(feature = "candle")]
            Backend::Embed(e) => {
                if path != "/v1/embeddings" {
                    return error(
                        StatusCode::BAD_REQUEST,
                        format!("{} is an embedding model: use /v1/embeddings", model.id),
                    );
                }
                let texts: Vec<String> = match v.get("input") {
                    Some(Value::String(s)) => vec![s.clone()],
                    Some(Value::Array(a)) if a.iter().all(Value::is_string) => a
                        .iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect(),
                    _ => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            "`input` must be a string or a list of strings",
                        );
                    }
                };
                let e = Arc::clone(e);
                match tokio::task::spawn_blocking(move || e.embed(&texts)).await {
                    Ok(Ok((vectors, tokens))) => {
                        let data: Vec<Value> = vectors
                            .into_iter()
                            .enumerate()
                            .map(|(i, emb)| json!({"object": "embedding", "index": i, "embedding": emb}))
                            .collect();
                        json_response(
                            StatusCode::OK,
                            &json!({
                                "object": "list", "data": data, "model": model.id,
                                "usage": {"prompt_tokens": tokens, "total_tokens": tokens}
                            }),
                        )
                    }
                    Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
                    Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                }
            }
        }
    }

    async fn proxy(&self, url: &str, key: &SecretString, body: String) -> Response<Body> {
        let client = match crate::hub::client() {
            Ok(c) => c,
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        let resp = match client
            .post(url)
            .bearer_auth(key.expose_secret())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return error(
                    StatusCode::BAD_GATEWAY,
                    format!("the backend did not answer: {e}"),
                );
            }
        };
        let status =
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_owned();
        let stream = resp
            .bytes_stream()
            .map(|r| r.map(Frame::data).map_err(std::io::Error::other));
        Response::builder()
            .status(status)
            .header("content-type", content_type)
            .body(BodyExt::boxed(StreamBody::new(stream)))
            .unwrap_or_else(|_| {
                error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "cannot build the response",
                )
            })
    }

    /// The model's backend, started if needed.
    async fn ensure(&self, model: &LocalModel) -> Result<Arc<Backend>, String> {
        if let Some(l) = self.loaded.lock().await.get_mut(&model.id) {
            l.used = Instant::now();
            return Ok(Arc::clone(&l.backend));
        }
        let _one = self.starting.lock().await;
        if let Some(l) = self.loaded.lock().await.get_mut(&model.id) {
            l.used = Instant::now();
            return Ok(Arc::clone(&l.backend));
        }
        let header = model.gguf_path().and_then(|p| catalog::local_header(&p));
        let context = u64::from(self.opts.config.context);
        let estimate = Estimate::new(
            model.size,
            header.as_ref().and_then(|h| h.shape()).as_ref(),
            context,
        );
        let bytes = estimate.total();
        self.make_room(bytes).await;
        tracing::info!("starting {} ({} bytes estimated)", model.id, bytes);
        let backend = match &model.kind {
            Kind::Gguf { .. } => self.start_llama(model, header.as_ref(), &estimate).await?,
            Kind::Server { name } => self.start_server(model, name).await?,
            Kind::Embedding => self.start_embedder(model).await?,
        };
        cache::touch(&model.dir);
        let backend = Arc::new(backend);
        self.loaded.lock().await.insert(
            model.id.clone(),
            Loaded {
                backend: Arc::clone(&backend),
                bytes,
                used: Instant::now(),
            },
        );
        Ok(backend)
    }

    /// Stops the least recently used models until `bytes` more fit.
    async fn make_room(&self, bytes: u64) {
        let mut victims = Vec::new();
        {
            let mut loaded = self.loaded.lock().await;
            loop {
                let used: u64 = loaded.values().map(|l| l.bytes).sum();
                if used + bytes <= self.budget || loaded.is_empty() {
                    break;
                }
                let Some(oldest) = loaded
                    .iter()
                    .min_by_key(|(_, l)| l.used)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                tracing::info!("unloading {oldest} to make room");
                if let Some(l) = loaded.remove(&oldest) {
                    victims.push(l.backend);
                }
            }
        }
        // Its memory is free once it has exited.
        for b in victims {
            unload(&b).await;
        }
    }

    async fn start_llama(
        &self,
        model: &LocalModel,
        header: Option<&crate::gguf::Header>,
        est: &Estimate,
    ) -> Result<Backend, String> {
        let Some(source) = self.opts.llama.clone() else {
            return Err("llama.cpp is not available to this router".into());
        };
        let binary = tokio::task::spawn_blocking(move || source.binary())
            .await
            .map_err(|e| e.to_string())??;
        let placement = fit::place(est, &self.opts.hw, llama::uses_gpu(self.opts.build));
        let arch = header.and_then(|h| h.architecture()).unwrap_or("");
        let embeddings =
            header.is_some_and(|h| h.meta.contains_key(&format!("{arch}.pooling_type")));
        let port = free_port().map_err(|e| e.to_string())?;
        let key = token()?;
        let launch = llama::Launch {
            model: model.gguf_path().ok_or("not a GGUF model")?,
            alias: model.id.clone(),
            port,
            context: est.context,
            gpu_layers: placement.gpu_layers(),
            jinja: header.is_some_and(crate::gguf::Header::has_chat_template),
            embeddings,
        };
        let mut cmd = tokio::process::Command::new(&binary);
        cmd.args(launch.args());
        // The key goes in the environment, not on the command line, where
        // every local user could read it.
        cmd.env(llama::KEY_VAR, key.expose_secret());
        if let Some(dir) = binary.parent() {
            // Its shared libraries sit beside it.
            cmd.current_dir(dir);
            let var = if cfg!(target_os = "macos") {
                "DYLD_LIBRARY_PATH"
            } else {
                "LD_LIBRARY_PATH"
            };
            if cfg!(unix) {
                let mut paths = vec![dir.to_owned()];
                paths.extend(std::env::var_os(var).iter().flat_map(std::env::split_paths));
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env(var, joined);
                }
            }
        }
        let base = format!("http://127.0.0.1:{port}");
        let mut child = spawn_and_wait(
            cmd,
            &base,
            "/health",
            &key,
            Duration::from_secs(600),
            &model.id,
        )
        .await?;
        // A llama-server that ignored the key would serve anyone on this
        // machine: check that it refuses a request without it.
        let client = crate::hub::client().map_err(|e| e.to_string())?;
        let unauthorized = client
            .post(format!("{base}/v1/chat/completions"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .map(|r| r.status().as_u16())
            .map_err(|e| e.to_string());
        if unauthorized != Ok(401) {
            let _ = child.start_kill();
            return Err(format!(
                "llama-server did not take its API key from {} (a request without it got {unauthorized:?}); \
                 use a newer llama.cpp",
                llama::KEY_VAR
            ));
        }
        Ok(Backend::Http {
            base,
            key,
            name: model.id.clone(),
            child: Mutex::new(child),
        })
    }

    async fn start_server(&self, model: &LocalModel, name: &str) -> Result<Backend, String> {
        let cfg = self
            .opts
            .config
            .servers
            .get(name)
            .ok_or_else(|| format!("no [models.servers.{name}]"))?;
        if cfg.command.is_empty() {
            return Err(format!("[models.servers.{name}] has no command"));
        }
        let port = free_port().map_err(|e| e.to_string())?;
        let key = token()?;
        let fill = |a: &str| {
            a.replace("{model}", &model.dir.display().to_string())
                .replace("{repo}", &model.repo)
                .replace("{port}", &port.to_string())
                .replace("{cache}", &self.opts.cache.dir.display().to_string())
                .replace("{token}", key.expose_secret())
        };
        let argv: Vec<String> = cfg.command.iter().map(|a| fill(a)).collect();
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        // Hugging Face libraries find the models where TermOS put them,
        // and do not download behind its back.
        cmd.env("HF_HUB_CACHE", &self.opts.cache.dir)
            .env("HF_HUB_OFFLINE", "1");
        for (k, v) in &cfg.env {
            cmd.env(k, fill(v));
        }
        let base = format!("http://127.0.0.1:{port}");
        let wait = Duration::from_secs(cfg.start_timeout.unwrap_or(900));
        let child = spawn_and_wait(cmd, &base, "/v1/models", &key, wait, &model.id).await?;
        Ok(Backend::Http {
            base,
            key,
            name: model.repo.clone(),
            child: Mutex::new(child),
        })
    }

    async fn start_embedder(&self, model: &LocalModel) -> Result<Backend, String> {
        #[cfg(feature = "candle")]
        {
            let dir = model.dir.clone();
            let e = tokio::task::spawn_blocking(move || crate::embed::Embedder::load(&dir))
                .await
                .map_err(|e| e.to_string())??;
            Ok(Backend::Embed(Arc::new(e)))
        }
        #[cfg(not(feature = "candle"))]
        {
            let _ = model;
            Err("this build of TermOS has no candle backend".into())
        }
    }

    /// Stops every backend, and waits for them to exit.
    pub async fn stop_all(&self) {
        let all = std::mem::take(&mut *self.loaded.lock().await);
        for l in all.into_values() {
            unload(&l.backend).await;
        }
    }
}

/// Stops a backend's process and waits (a few seconds at most) for it to
/// exit. A request still streaming from it ends there.
async fn unload(backend: &Backend) {
    match backend {
        Backend::Http { child, .. } => {
            let mut c = child.lock().await;
            let _ = c.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
        }
        // Dropped with the last reference.
        #[cfg(feature = "candle")]
        Backend::Embed(_) => {}
    }
}

/// Starts `cmd` and waits until `base + probe` answers 200, the process
/// exits (the error carries its last output), or `timeout` passes.
async fn spawn_and_wait(
    mut cmd: tokio::process::Command,
    base: &str,
    probe: &str,
    key: &SecretString,
    timeout: Duration,
    what: &str,
) -> Result<tokio::process::Child, String> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start the backend: {e}"))?;
    let tail: Arc<std::sync::Mutex<VecDeque<String>>> = Arc::default();
    if let Some(err) = child.stderr.take() {
        let tail = Arc::clone(&tail);
        let what = what.to_owned();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt as _;
            let mut lines = tokio::io::BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                tracing::debug!(model = %what, "{l}");
                if let Ok(mut t) = tail.lock() {
                    if t.len() >= 20 {
                        t.pop_front();
                    }
                    t.push_back(l);
                }
            }
        });
    }
    let client = crate::hub::client().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let last = tail
                .lock()
                .map(|t| t.iter().cloned().collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            return Err(format!(
                "the backend exited ({status}) while starting:\n{last}"
            ));
        }
        if let Ok(r) = client
            .get(format!("{base}{probe}"))
            .bearer_auth(key.expose_secret())
            .send()
            .await
            && r.status().is_success()
        {
            return Ok(child);
        }
        if Instant::now() > deadline {
            return Err(format!(
                "the backend did not start within {}s",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Serves `router` on `listener` until it is told to stop.
pub async fn serve_tcp(router: Arc<Router>, listener: tokio::net::TcpListener) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                spawn_connection(Arc::clone(&router), stream);
            }
            () = router.stopped() => break,
        }
    }
    router.stop_all().await;
}

/// Serves `router` on a Unix socket until it is told to stop.
#[cfg(unix)]
pub async fn serve_unix(router: Arc<Router>, listener: tokio::net::UnixListener) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                spawn_connection(Arc::clone(&router), stream);
            }
            () = router.stopped() => break,
        }
    }
    router.stop_all().await;
}

fn spawn_connection<S>(router: Arc<Router>, stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let svc = hyper::service::service_fn(move |req| {
            let r = Arc::clone(&router);
            async move { Ok::<_, Infallible>(r.handle(req).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(hyper_util::rt::TokioIo::new(stream), svc)
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_comparison() {
        let t = token().expect("token");
        assert_eq!(t.expose_secret().len(), 32);
        assert_ne!(t.expose_secret(), token().expect("another").expose_secret());
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
    }
}
