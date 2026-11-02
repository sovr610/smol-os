// SPDX-License-Identifier: GPL-3.0-or-later
//! The router end to end: models in a cache, backends started on demand
//! (a stand-in for llama-server, a configured server, candle for an
//! embedding model), the token, the memory budget's evictions, streaming.

// The stand-in servers are Python scripts run by their shebang; the
// embedding model needs candle, and the client needs TLS support compiled
// in (D-065).
#![cfg(all(unix, feature = "candle", feature = "tls"))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use termos_core::config::models::{ByteSize, LlamaBuild, ModelsConfig, ServerConfig};
use termos_hw::accel::Accelerators;
use termos_models::cache::Cache;
use termos_models::gguf::{self, Value as G};
use termos_models::llama::Source;
use termos_models::router::{Options, Router, serve_tcp};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn fake() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_openai.py")
}

/// Puts a file into the cache as a pull would.
fn put(cache: &Cache, repo: &str, path: &str, content: &[u8]) {
    let file = cache.snapshot_dir(repo, COMMIT).join(path);
    std::fs::create_dir_all(file.parent().expect("dir")).expect("mkdir");
    std::fs::write(file, content).expect("write");
}

fn tiny_gguf(name: &str) -> Vec<u8> {
    let mut v = gguf::build(
        &[
            ("general.architecture", G::Str("llama".into())),
            ("general.name", G::Str(name.into())),
            ("llama.block_count", G::U64(2)),
            ("llama.context_length", G::U64(512)),
            ("llama.embedding_length", G::U64(64)),
            ("llama.attention.head_count", G::U64(4)),
            ("tokenizer.chat_template", G::Str("{{ messages }}".into())),
        ],
        0,
    );
    // Some "weights", so models have sizes.
    v.extend(std::iter::repeat_n(7u8, 200_000));
    v
}

/// Whether a process with `needle` in its command line runs.
fn running(needle: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "args="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.contains(needle))
}

async fn post(base: &str, token: &str, path: &str, body: serde_json::Value) -> (u16, String) {
    let r = termos_models::hub::client()
        .expect("client")
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("request");
    let status = r.status().as_u16();
    (status, r.text().await.expect("body"))
}

#[tokio::test(flavor = "current_thread")]
async fn models_start_on_demand_and_make_room() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        panic!("these tests need python3 for the stand-in server");
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = Cache::new(dir.path().to_owned());
    put(
        &cache,
        "org/alpha-GGUF",
        "alpha-Q4_K_M.gguf",
        &tiny_gguf("alpha"),
    );
    put(
        &cache,
        "org/beta-GGUF",
        "beta-Q8_0.gguf",
        &tiny_gguf("beta"),
    );
    put(
        &cache,
        "org/beta-GGUF",
        "beta-Q4_0.gguf",
        &tiny_gguf("beta small"),
    );
    termos_models::embed::write_tiny_model(&cache.snapshot_dir("org/tiny-bert", COMMIT))
        .expect("bert");
    put(&cache, "org/sidecar-model", "config.json", b"{}");

    let mut config = ModelsConfig {
        // Room for one llama model at a time (each is estimated at ~270 MiB).
        memory_budget: Some(ByteSize(400 << 20)),
        ..ModelsConfig::default()
    };
    config.servers.insert(
        "fake".into(),
        ServerConfig {
            command: vec![
                "python3".into(),
                fake().display().to_string(),
                "--port".into(),
                "{port}".into(),
                "--api-key".into(),
                "{token}".into(),
                "--alias".into(),
                "{repo}".into(),
            ],
            models: vec!["org/sidecar-model".into()],
            ..ServerConfig::default()
        },
    );
    let router = Arc::new(Router::new(Options {
        cache: cache.clone(),
        config,
        hw: Accelerators {
            memory_total: 64 << 30,
            memory_available: 32 << 30,
            ..Accelerators::default()
        },
        llama: Some(Source::Path(fake())),
        build: LlamaBuild::Cpu,
        token: "router-token".into(),
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let served = tokio::spawn(serve_tcp(Arc::clone(&router), listener));

    // The token is required.
    let (status, _) = post(
        &base,
        "wrong",
        "/v1/chat/completions",
        serde_json::json!({"model": "x"}),
    )
    .await;
    assert_eq!(status, 401);
    let list: serde_json::Value = termos_models::hub::client()
        .expect("client")
        .get(format!("{base}/v1/models"))
        .bearer_auth("router-token")
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    let mut ids: Vec<String> = list["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|m| m["id"].as_str().expect("id").to_owned())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "org/alpha-GGUF",
            "org/beta-GGUF:q4_0",
            "org/beta-GGUF:q8_0",
            "org/sidecar-model",
            "org/tiny-bert"
        ]
    );

    // A GGUF model starts its llama-server (the stand-in) and streams.
    let (status, text) = post(&base, "router-token", "/v1/chat/completions", serde_json::json!({
        "model": "org/alpha-GGUF", "stream": true, "messages": [{"role": "user", "content": "hi"}]
    })).await;
    assert_eq!(status, 200, "{text}");
    assert!(text.starts_with("data: "), "{text}");
    assert!(
        text.contains("served org/alpha-GGUF as org/alpha-GGUF"),
        "{text}"
    );
    assert!(
        text.contains("--ctx-size 512")
            && text.contains("--n-gpu-layers 0")
            && text.contains("--jinja"),
        "{text}"
    );
    assert!(
        !text.contains("--api-key"),
        "the key stays off the command line: {text}"
    );
    assert_eq!(router.loaded().await, ["org/alpha-GGUF"]);

    // Another one does not fit beside it: the first is stopped.
    let (status, text) = post(
        &base,
        "router-token",
        "/v1/chat/completions",
        serde_json::json!({
            "model": "org/beta-GGUF:q8_0", "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert!(text.contains("served org/beta-GGUF:q8_0"), "{text}");
    assert_eq!(router.loaded().await, ["org/beta-GGUF:q8_0"]);
    let alpha = cache
        .snapshot_dir("org/alpha-GGUF", COMMIT)
        .join("alpha-Q4_K_M.gguf");
    assert!(
        !running(&alpha.display().to_string()),
        "the evicted server has exited"
    );

    // The embedding model runs in-process with candle.
    let (status, text) = post(
        &base,
        "router-token",
        "/v1/embeddings",
        serde_json::json!({
            "model": "org/tiny-bert", "input": ["hello world", "local models"]
        }),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["data"].as_array().map(Vec::len), Some(2));
    assert_eq!(v["data"][0]["embedding"].as_array().map(Vec::len), Some(8));
    assert_eq!(v["usage"]["prompt_tokens"], 8);
    let (status, _) = post(
        &base,
        "router-token",
        "/v1/chat/completions",
        serde_json::json!({"model": "org/tiny-bert"}),
    )
    .await;
    assert_eq!(status, 400, "chat with an embedding model");

    // A configured server gets its own token and the repository's name.
    let (status, text) = post(
        &base,
        "router-token",
        "/v1/chat/completions",
        serde_json::json!({
            "model": "org/sidecar-model", "messages": []
        }),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert!(
        text.contains("served org/sidecar-model as org/sidecar-model"),
        "{text}"
    );

    let (status, text) = post(
        &base,
        "router-token",
        "/v1/chat/completions",
        serde_json::json!({"model": "org/missing"}),
    )
    .await;
    assert_eq!(status, 404);
    assert!(text.contains("termos model pull org/missing"), "{text}");

    let (status, _) = post(&base, "router-token", "/admin/stop", serde_json::json!({})).await;
    assert_eq!(status, 200);
    served.await.expect("stopped");
    assert!(router.loaded().await.is_empty());
    assert!(
        !running(&dir.path().display().to_string()),
        "every backend has exited"
    );
}
