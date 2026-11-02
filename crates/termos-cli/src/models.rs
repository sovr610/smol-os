// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos model …` (find, fetch and manage local models) and
//! `termos models …` (the router) (M7, ARCHITECTURE.md §12).

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Subcommand;
use secrecy::{ExposeSecret as _, SecretString};
use termos_core::config::Config;
use termos_core::config::models::human_bytes;
use termos_core::models_endpoint::{self, ModelsEndpoint};
use termos_core::paths::Paths;
use termos_hw::accel::{self, Accelerators};
use termos_models::cache::{Cache, Progress};
use termos_models::catalog::{self, Kind, ModelRef};
use termos_models::fit;
use termos_models::hub::Hub;
use termos_models::llama;

#[derive(Debug, Subcommand)]
pub(crate) enum ModelCmd {
    /// Search the Hugging Face Hub (GGUF models; --all for every kind).
    Search {
        query: String,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 15)]
        limit: u32,
    },
    /// A repository's files, how each would fit this machine, and its
    /// model's shape (nothing is downloaded).
    Info {
        /// `org/name`, or `org/name:q4_k_m` / `org/name:path/file.gguf`.
        model: String,
        #[arg(long)]
        revision: Option<String>,
    },
    /// Download a model into the Hugging Face cache (resumes, checks
    /// checksums). Shows the memory estimate first.
    Pull {
        model: String,
        #[arg(long)]
        revision: Option<String>,
        /// Do not ask before downloading.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Downloaded models and the ids the router serves them as.
    #[command(alias = "ls")]
    List,
    /// Remove a downloaded repository (`org/name`) or one file of it
    /// (`org/name:q4_k_m`).
    #[command(alias = "remove")]
    Rm { model: String },
    /// Remove interrupted downloads, unused files and, over the size limit
    /// (`--max`, else `[models] cache-limit`), the least recently used
    /// models.
    Gc {
        #[arg(long)]
        max: Option<String>,
    },
    /// What this machine can run; with a query, the Hub's matching models
    /// that fit best.
    Recommend { query: Option<String> },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelsCmd {
    /// Serve the downloaded models on an OpenAI-compatible endpoint
    /// (localhost, token-protected; agents' `local` provider uses it).
    Serve {
        /// The port (default `[models] port`, else a free one).
        #[arg(long)]
        port: Option<u16>,
        /// Also listen on this Unix socket.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Detach from the terminal (how agents start it).
        #[arg(long, hide = true)]
        daemon: bool,
    },
    /// Whether the router runs, where, and what it has loaded.
    Status,
    /// Stop the router and every model it runs.
    Stop,
    /// Install llama.cpp for this machine now (otherwise the first GGUF
    /// model does it).
    Setup,
    /// Set up a Python sidecar (vLLM or transformers in a uv venv) and
    /// print the `[models.servers]` entry to add.
    Sidecar {
        /// `vllm` or `transformers`.
        kind: String,
        /// The repositories it should serve.
        #[arg(long = "model")]
        models: Vec<String>,
    },
}

fn load_config(path: &Path) -> anyhow::Result<Config> {
    Config::load(path).map_err(|e| anyhow::anyhow!("{e}"))
}

fn rt() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

fn cache_of(config: &Config) -> Cache {
    let get = |k: &str| std::env::var(k).ok();
    let configured = config
        .models
        .cache
        .as_deref()
        .map(|p| termos_ai::policy::expand_home(p, home().as_deref()));
    Cache::new(termos_models::cache::default_dir(
        configured.as_deref(),
        &get,
    ))
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The Hugging Face token: `[models] token` from the secret store, else
/// `HF_TOKEN`.
fn hf_token(config: &Config, paths: &Paths) -> anyhow::Result<Option<SecretString>> {
    if let Some(r) = &config.models.token {
        let store = termos_ai::secrets::SecretStore::open(config.secrets.store, &paths.data_dir);
        if store.is_locked() {
            if !std::io::stdin().is_terminal() {
                bail!("the secrets file is locked: run this in a terminal to enter its passphrase");
            }
            let pass =
                crate::agents::read_hidden(&format!("Passphrase for {}", store.file().display()))?;
            store.unlock(pass)?;
        }
        return Ok(Some(store.get(r.name())?));
    }
    Ok(std::env::var("HF_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .map(SecretString::from))
}

fn hardware() -> Accelerators {
    accel::detect()
}

fn gib(b: u64) -> String {
    human_bytes(b)
}

fn print_estimate(e: &fit::Estimate, p: fit::Placement) {
    println!(
        "  needs about {} ({} weights + {} KV cache at {} tokens + {} overhead): {}",
        gib(e.total()),
        gib(e.weights),
        gib(e.kv_cache),
        e.context,
        gib(e.overhead),
        p.describe()
    );
}

pub(crate) fn model_command(
    action: &ModelCmd,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    let config = load_config(config_path)?;
    let cache = cache_of(&config);
    let context = u64::from(config.models.context);
    match action {
        ModelCmd::Search { query, all, limit } => {
            let hub = Hub::new(hf_token(&config, paths)?)?;
            let found = rt()?.block_on(hub.search(query, !all, *limit))?;
            if found.is_empty() {
                println!("nothing found for `{query}`");
            }
            for f in found {
                let task = f
                    .pipeline_tag
                    .map(|t| format!("  [{t}]"))
                    .unwrap_or_default();
                println!(
                    "{:<60} {:>10} downloads {:>6} likes{task}",
                    f.id, f.downloads, f.likes
                );
            }
        }
        ModelCmd::Info { model, revision } => {
            let r = ModelRef::parse(model, revision.as_deref()).map_err(|e| anyhow::anyhow!(e))?;
            let hub = Hub::new(hf_token(&config, paths)?)?;
            let hw = hardware();
            let build = llama::choose(config.models.llama_cpp, &hw);
            let rt = rt()?;
            let plan = rt.block_on(catalog::plan(
                &hub,
                &r,
                &hw,
                llama::uses_gpu(build),
                context,
            ))?;
            println!(
                "{} at {}",
                plan.repo,
                &plan.commit[..12.min(plan.commit.len())]
            );
            if let Kind::Gguf { file } = &plan.kind
                && let Some(h) =
                    rt.block_on(catalog::remote_header(&hub, &plan.repo, &plan.commit, file))
            {
                let s = h.shape();
                println!(
                    "  {} ({}): {} layers, trained context {}",
                    h.name().unwrap_or("?"),
                    h.architecture().unwrap_or("?"),
                    s.map_or_else(|| "?".into(), |s| s.layers.to_string()),
                    s.and_then(|s| s.context)
                        .map_or_else(|| "?".into(), |c| c.to_string())
                );
            }
            println!("on this machine (llama.cpp {build} build):");
            for c in &plan.choices {
                let mark = if matches!(&plan.kind, Kind::Gguf { file } if *file == c.file) {
                    "→"
                } else {
                    " "
                };
                println!(
                    " {mark} {:<55} {:>10}  {}",
                    c.file,
                    gib(c.estimate.weights),
                    c.placement.describe()
                );
            }
            if plan.choices.is_empty()
                && let (Some(e), Some(p)) = (plan.estimate, plan.placement)
            {
                println!(
                    "  an embedding model for candle, {} to download",
                    gib(plan.bytes())
                );
                print_estimate(&e, p);
            }
        }
        ModelCmd::Pull {
            model,
            revision,
            yes,
        } => {
            let r = ModelRef::parse(model, revision.as_deref()).map_err(|e| anyhow::anyhow!(e))?;
            let hub = Hub::new(hf_token(&config, paths)?)?;
            let hw = hardware();
            let build = llama::choose(config.models.llama_cpp, &hw);
            let rt = rt()?;
            let plan = rt.block_on(catalog::plan(
                &hub,
                &r,
                &hw,
                llama::uses_gpu(build),
                context,
            ))?;
            let what = match &plan.kind {
                Kind::Gguf { file } => file.clone(),
                Kind::Embedding => "an embedding model".into(),
                Kind::Server { name } => format!("files for {name}"),
            };
            println!("{} ({what}): {} to download", plan.repo, gib(plan.bytes()));
            if let (Some(e), Some(p)) = (plan.estimate, plan.placement) {
                print_estimate(&e, p);
            }
            if !yes && std::io::stdin().is_terminal() {
                eprint!("Download? [Y/n] ");
                let _ = std::io::stderr().flush();
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim().eq_ignore_ascii_case("n") {
                    return Ok(ExitCode::FAILURE);
                }
            }
            let tty = std::io::stderr().is_terminal();
            for f in &plan.files {
                let mut last = Instant::now() - Duration::from_secs(10);
                let mut on = |p: Progress| {
                    let now = Instant::now();
                    if now.duration_since(last)
                        < Duration::from_millis(if tty { 200 } else { 5000 })
                        && p.done != p.total
                    {
                        return;
                    }
                    last = now;
                    let pct = (p.done * 100).checked_div(p.total).unwrap_or(100);
                    let resumed = if p.resumed > 0 && p.resumed < p.total {
                        format!(" (resumed at {})", gib(p.resumed))
                    } else {
                        String::new()
                    };
                    if tty {
                        eprint!("\r  {} {pct:>3}% of {}{resumed}   ", f.path, gib(p.total));
                    } else {
                        eprintln!("  {} {pct}% of {}{resumed}", f.path, gib(p.total));
                    }
                };
                rt.block_on(cache.pull(&hub, &plan.repo, &plan.commit, f, &mut on))?;
                if tty {
                    eprintln!();
                }
            }
            cache.set_ref(&plan.repo, &plan.revision, &plan.commit)?;
            let models = catalog::scan(&cache, &config.models.servers);
            let ids: Vec<&str> = models
                .iter()
                .filter(|m| m.repo == plan.repo)
                .map(|m| m.id.as_str())
                .collect();
            println!(
                "pulled into {}",
                cache.snapshot_dir(&plan.repo, &plan.commit).display()
            );
            println!("serve it as: {}", ids.join(", "));
        }
        ModelCmd::List => {
            let models = catalog::scan(&cache, &config.models.servers);
            if models.is_empty() {
                println!(
                    "no models in {} (`termos model pull <repo>`)",
                    cache.dir.display()
                );
            }
            for m in &models {
                let kind = match &m.kind {
                    Kind::Gguf { file } => format!("llama.cpp, {file}"),
                    Kind::Embedding => "candle, embeddings".into(),
                    Kind::Server { name } => format!("server {name}"),
                };
                println!("{:<60} {:>10}  {kind}", m.id, gib(m.size));
            }
            println!("cache: {} in {}", gib(cache.size()), cache.dir.display());
        }
        ModelCmd::Rm { model } => {
            let r = ModelRef::parse(model, None).map_err(|e| anyhow::anyhow!(e))?;
            match &r.selector {
                None => {
                    let freed = cache.remove(&r.repo)?;
                    if freed == 0 {
                        println!("{} is not downloaded", r.repo);
                    } else {
                        println!("removed {} ({})", r.repo, gib(freed));
                    }
                }
                Some(_) => {
                    let models = catalog::scan(&cache, &config.models.servers);
                    let Some(m) = catalog::find(&models, model) else {
                        bail!("no downloaded model `{model}` (`termos model list` shows them)");
                    };
                    let Some(path) = m.gguf_path() else {
                        bail!("{model} is not a single file: remove {} instead", m.repo);
                    };
                    std::fs::remove_file(&path)
                        .with_context(|| format!("cannot remove {}", path.display()))?;
                    let (freed, _) = cache.gc(None);
                    println!("removed {model} ({})", gib(freed));
                }
            }
        }
        ModelCmd::Gc { max } => {
            let limit = match max {
                Some(m) => Some(
                    termos_core::config::models::ByteSize::parse(m)
                        .map_err(|e| anyhow::anyhow!(e))?
                        .0,
                ),
                None => config.models.cache_limit.map(|b| b.0),
            };
            let (freed, removed) = cache.gc(limit);
            for r in &removed {
                println!("removed {r}");
            }
            println!(
                "freed {}; the cache holds {}",
                gib(freed),
                gib(cache.size())
            );
        }
        ModelCmd::Recommend { query } => {
            let hw = hardware();
            let build = llama::choose(config.models.llama_cpp, &hw);
            println!("This machine:");
            for l in hw.describe() {
                println!("  {l}");
            }
            println!("llama.cpp build: {build}");
            for l in accelerator_notes(&hw) {
                println!("  {l}");
            }
            // The largest weights that fit, for a typical model shape at the
            // configured context (8 KV heads of 128, 32 layers).
            let typical = fit::Shape {
                layers: 32,
                kv_heads: 8,
                key_dim: 128,
                value_dim: 128,
                context: None,
            };
            let largest = |gpu: bool| {
                let (mut lo, mut hi) = (0u64, 1u64 << 42);
                while hi - lo > (64 << 20) {
                    let mid = lo + (hi - lo) / 2;
                    let e = fit::Estimate::new(mid, Some(&typical), context);
                    let p = fit::place(&e, &hw, gpu);
                    let ok = if gpu {
                        p == fit::Placement::Gpu
                    } else {
                        p != fit::Placement::TooBig
                    };
                    if ok { lo = mid } else { hi = mid }
                }
                lo
            };
            if llama::uses_gpu(build) && (hw.best_gpu_free().is_some() || hw.metal) {
                println!(
                    "Fully on the GPU: models with up to about {} of weights (at {context} tokens of context).",
                    gib(largest(true))
                );
            }
            println!(
                "At all (CPU, or split with the GPU): up to about {} of weights.",
                gib(largest(false))
            );
            println!(
                "As a guide, Q4_K_M files take about 0.6 GB per billion parameters, Q8_0 about 1.1 GB."
            );
            if let Some(q) = query {
                let hub = Hub::new(hf_token(&config, paths)?)?;
                let rt = rt()?;
                let found = rt.block_on(hub.search(q, true, 8))?;
                println!("\nGGUF models on the Hub for `{q}`, the best file each:");
                for f in found {
                    let r = ModelRef {
                        repo: f.id.clone(),
                        selector: None,
                        revision: "main".into(),
                    };
                    match rt.block_on(catalog::plan(
                        &hub,
                        &r,
                        &hw,
                        llama::uses_gpu(build),
                        context,
                    )) {
                        Ok(plan) => match plan.choices.first() {
                            Some(c) => println!(
                                "  {}:{:<10} {:>10}  {}",
                                f.id,
                                c.quant
                                    .clone()
                                    .unwrap_or_else(|| c.file.clone())
                                    .to_ascii_lowercase(),
                                gib(c.estimate.weights),
                                c.placement.describe()
                            ),
                            None => println!("  {}  (no GGUF files)", f.id),
                        },
                        Err(e) => println!("  {}  ({e})", f.id),
                    }
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// What each accelerator found can and cannot do (docs/models.md).
fn accelerator_notes(hw: &Accelerators) -> Vec<String> {
    let mut out = Vec::new();
    if !hw.coral.is_empty() {
        out.push("Coral Edge TPU: runs small int8 TFLite models compiled for it (vision, audio); it cannot run language models.".into());
    }
    if !hw.hailo.is_empty() {
        out.push("Hailo: runs compiled HEF models (mostly vision) through Hailo's own runtime; TermOS does not run language models on it.".into());
    }
    if hw.tpu_chips > 0 {
        out.push("Cloud TPU: use a vLLM sidecar with TPU support (`termos models sidecar vllm`, then vLLM's TPU install); llama.cpp does not use TPUs.".into());
    }
    if hw.pi_model.is_some() {
        out.push("Raspberry Pi: llama.cpp on the CPU; small models (up to ~3B parameters at Q4) are practical.".into());
    }
    if hw.rocm && hw.os == "linux" {
        out.push("ROCm: llama.cpp's official Linux builds have no ROCm (HIP) build; Vulkan is used, or set [models] llama-server to a HIP build of your own.".into());
    }
    if hw.cuda && hw.os == "linux" {
        out.push("CUDA: llama.cpp's official Linux builds have no CUDA build; Vulkan is used, or set [models] llama-server to a CUDA build of your own.".into());
    }
    out
}

/// Where `llama-server` will come from.
fn llama_source(
    config: &Config,
    paths: &Paths,
    hw: &Accelerators,
    build: termos_core::config::models::LlamaBuild,
) -> Result<llama::Source, String> {
    if let Some(p) = &config.models.llama_server {
        return Ok(llama::Source::Path(termos_ai::policy::expand_home(
            p,
            home().as_deref(),
        )));
    }
    if let Some(p) = std::env::var_os("TERMOS_LLAMA_SERVER").filter(|v| !v.is_empty()) {
        return Ok(llama::Source::Path(PathBuf::from(p)));
    }
    let tool = llama::mise_tool(build, &hw.os, &hw.arch, "latest")?;
    let mise = termos_devkit::mise::ensure(&paths.data_dir, &mut |m| tracing::info!("{m}"))
        .map_err(|e| format!("cannot get mise to install llama.cpp: {e}"))?;
    Ok(llama::Source::Mise {
        mise: mise.path,
        tool,
    })
}

pub(crate) fn models_command(
    action: &ModelsCmd,
    paths: &Paths,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    let config = load_config(config_path)?;
    match action {
        ModelsCmd::Serve {
            port,
            socket,
            daemon,
        } => serve(&config, paths, *port, socket.as_deref(), *daemon),
        ModelsCmd::Status => {
            let Some(e) = running(paths) else {
                println!("the model router is not running (`termos models serve`)");
                return Ok(ExitCode::FAILURE);
            };
            let rt = rt()?;
            let v: serde_json::Value = rt.block_on(async {
                termos_models::hub::client()?
                    .get(format!("{}/models", e.url))
                    .bearer_auth(e.token.expose_secret())
                    .send()
                    .await
                    .map_err(|e| termos_models::hub::HubError::Network(e.to_string()))?
                    .json()
                    .await
                    .map_err(|e| termos_models::hub::HubError::Protocol(e.to_string()))
            })?;
            println!("the model router runs at {} (pid {})", e.url, e.pid);
            for m in v["data"].as_array().into_iter().flatten() {
                let loaded = if m["termos"]["loaded"] == true {
                    "loaded"
                } else {
                    ""
                };
                println!(
                    "  {:<60} {:<10} {loaded}",
                    m["id"].as_str().unwrap_or("?"),
                    m["termos"]["kind"].as_str().unwrap_or("")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        ModelsCmd::Stop => {
            let Some(e) = running(paths) else {
                println!("the model router is not running");
                return Ok(ExitCode::SUCCESS);
            };
            rt()?.block_on(async {
                let _ = termos_models::hub::client()?
                    .post(format!("{}/admin/stop", e.url.trim_end_matches("/v1")))
                    .bearer_auth(e.token.expose_secret())
                    .send()
                    .await;
                Ok::<_, termos_models::hub::HubError>(())
            })?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while models_endpoint::read(paths).is_some() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            println!("stopped the model router");
            Ok(ExitCode::SUCCESS)
        }
        ModelsCmd::Setup => {
            let hw = hardware();
            let build = llama::choose(config.models.llama_cpp, &hw);
            println!("llama.cpp build for this machine: {build}");
            let source =
                llama_source(&config, paths, &hw, build).map_err(|e| anyhow::anyhow!(e))?;
            let bin = source.binary().map_err(|e| anyhow::anyhow!(e))?;
            println!("llama-server: {}", bin.display());
            Ok(ExitCode::SUCCESS)
        }
        ModelsCmd::Sidecar { kind, models } => sidecar(kind, models, paths),
    }
}

/// The running router, if its endpoint file names a live one.
fn running(paths: &Paths) -> Option<ModelsEndpoint> {
    let e = models_endpoint::read(paths)?;
    let health = format!("{}/health", e.url.trim_end_matches("/v1"));
    let ok = rt().ok()?.block_on(async {
        let c = termos_models::hub::client().ok()?;
        Some(
            c.get(health)
                .timeout(Duration::from_secs(3))
                .send()
                .await
                .ok()?
                .status()
                .is_success(),
        )
    });
    ok.unwrap_or(false).then_some(e)
}

fn serve(
    config: &Config,
    paths: &Paths,
    port: Option<u16>,
    socket: Option<&Path>,
    daemon: bool,
) -> anyhow::Result<ExitCode> {
    if daemon {
        #[cfg(unix)]
        let _ = rustix::process::setsid();
    }
    if let Some(e) = running(paths) {
        bail!("a model router already runs at {} (pid {})", e.url, e.pid);
    }
    if let Err(e) = termos_core::log::init_file_logging(&paths.log_dir(), "models") {
        eprintln!("termos: logging disabled: {e}");
    }
    let hw = hardware();
    let build = llama::choose(config.models.llama_cpp, &hw);
    let llama_src = match llama_source(config, paths, &hw, build) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("GGUF models cannot run: {e}");
            if !daemon {
                eprintln!("termos: GGUF models cannot run: {e}");
            }
            None
        }
    };
    let token = termos_models::router::token().map_err(|e| anyhow::anyhow!(e))?;
    let router = Arc::new(termos_models::router::Router::new(
        termos_models::router::Options {
            cache: cache_of(config),
            config: config.models.clone(),
            hw,
            llama: llama_src,
            build,
            token: token.clone(),
        },
    ));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let port = port.unwrap_or(config.models.port);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.context("cannot listen")?;
        let addr = listener.local_addr()?;
        let endpoint = ModelsEndpoint { url: format!("http://{addr}/v1"), token, pid: std::process::id() };
        models_endpoint::write(paths, &endpoint).context("cannot write the endpoint file")?;
        tracing::info!("the model router listens on {addr} ({build} llama.cpp)");
        if !daemon {
            println!("the model router listens on http://{addr}/v1 (llama.cpp {build} build); the token is in {}", models_endpoint::path(paths).display());
        }
        #[cfg(unix)]
        if let Some(sock) = socket {
            let _ = std::fs::remove_file(sock);
            let unix = tokio::net::UnixListener::bind(sock).with_context(|| format!("cannot listen on {}", sock.display()))?;
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600));
            }
            tokio::spawn(termos_models::router::serve_unix(Arc::clone(&router), unix));
        }
        #[cfg(not(unix))]
        if socket.is_some() {
            bail!("--socket needs a Unix system");
        }
        let stop = Arc::clone(&router);
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            stop.stop();
        });
        #[cfg(unix)]
        {
            let stop = Arc::clone(&router);
            if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                tokio::spawn(async move {
                    term.recv().await;
                    stop.stop();
                });
            }
        }
        termos_models::router::serve_tcp(router, listener).await;
        models_endpoint::remove(paths, std::process::id());
        if let Some(sock) = socket {
            let _ = std::fs::remove_file(sock);
        }
        tracing::info!("the model router stopped");
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(ExitCode::SUCCESS)
}

fn sidecar(kind: &str, models: &[String], paths: &Paths) -> anyhow::Result<ExitCode> {
    let (package, command): (&str, Vec<&str>) = match kind {
        // vLLM reads its key from VLLM_API_KEY (set below), so it stays
        // off the command line.
        "vllm" => (
            "vllm",
            vec![
                "bin/vllm",
                "serve",
                "{model}",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
                "--served-model-name",
                "{repo}",
            ],
        ),
        "transformers" => (
            "transformers[serving]",
            vec![
                "bin/transformers",
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                "{port}",
            ],
        ),
        other => bail!("unknown sidecar `{other}`: vllm or transformers"),
    };
    if cfg!(windows) {
        bail!(
            "the Python sidecar is for Linux machines with an NVIDIA or AMD GPU (vLLM has no Windows build)"
        );
    }
    let venv = paths.data_dir.join("models").join("sidecar").join(kind);
    let mise = termos_devkit::mise::ensure(&paths.data_dir, &mut |m| println!("{m}"))?;
    let uv = |args: &[&str]| -> anyhow::Result<()> {
        let status = std::process::Command::new(&mise.path)
            .args(["exec", "uv@latest", "--", "uv"])
            .args(args)
            .status()
            .context("cannot run uv through mise")?;
        if !status.success() {
            bail!("uv {} failed", args.join(" "));
        }
        Ok(())
    };
    println!("creating {} …", venv.display());
    uv(&["venv", "--python", "3.12", &venv.display().to_string()])?;
    println!("installing {package} (large; several minutes) …");
    let python = venv.join("bin").join("python");
    uv(&[
        "pip",
        "install",
        "--python",
        &python.display().to_string(),
        package,
    ])?;
    let argv: Vec<String> = command
        .iter()
        .map(|a| {
            if a.starts_with("bin/") {
                venv.join(a).display().to_string()
            } else {
                (*a).to_owned()
            }
        })
        .collect();
    let quoted: Vec<String> = argv.iter().map(|a| format!("{a:?}")).collect();
    let served: Vec<String> = models.iter().map(|m| format!("{m:?}")).collect();
    println!("\nAdd this to config.toml (and `termos model pull` the repositories it serves):\n");
    println!(
        "[models.servers.{kind}]\ncommand = [{}]\nmodels = [{}]",
        quoted.join(", "),
        served.join(", ")
    );
    if kind == "vllm" {
        println!("env = {{ VLLM_API_KEY = \"{{token}}\" }}");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerator_notes_say_what_they_cannot_do() {
        let hw = Accelerators {
            coral: vec!["USB (1a6e:089a)".into()],
            hailo: vec!["/dev/hailo0".into()],
            tpu_chips: 4,
            ..Accelerators::default()
        };
        let notes = accelerator_notes(&hw).join("\n");
        assert!(notes.contains("cannot run language models"), "{notes}");
        assert!(notes.contains("Hailo"), "{notes}");
        assert!(notes.contains("vLLM sidecar with TPU"), "{notes}");
    }
}
