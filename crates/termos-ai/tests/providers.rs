// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider contract tests: the real HTTP clients against a mock server
//! answering with recorded-format fixtures (no live keys, ever).

mod common;

use common::{Answer, MockServer};
use secrecy::SecretString;
use termos_ai::provider::{
    self, Block, ChatRequest, Delta, ErrorKind, Message, StopReason, ToolSpec,
};
use termos_core::config::agents::{ProviderConfig, ProviderKind, SecretRef};

const KEY: &str = "sk-fixture-canary-0123456789";

fn config(kind: ProviderKind, base: &str) -> ProviderConfig {
    ProviderConfig {
        kind,
        base_url: Some(base.to_owned()),
        key: Some(SecretRef::parse("keyring:test").expect("ref")),
        max_tokens: None,
        prices: Default::default(),
    }
}

fn key() -> Option<SecretString> {
    Some(SecretString::from(KEY.to_owned()))
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        system: "You are a test.".into(),
        messages: std::sync::Arc::new(vec![Message::user_text("run the tests")]),
        tools: std::sync::Arc::new(vec![ToolSpec {
            name: "run_command".into(),
            description: "Run a command".into(),
            schema: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }]),
        max_tokens: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_lists_every_page_and_streams_a_tool_turn() {
    let server = MockServer::start(|req| {
        if req.path.starts_with("/v1/models") && !req.path.contains("after_id") {
            Answer::Json(200, r#"{"data":[{"id":"fixture-b","display_name":"Fixture B","max_input_tokens":200000,"max_tokens":64000,"type":"model"}],"has_more":true,"first_id":"fixture-b","last_id":"fixture-b"}"#.into())
        } else if req.path.starts_with("/v1/models") {
            Answer::Json(200, r#"{"data":[{"id":"fixture-a","display_name":"Fixture A","max_input_tokens":200000,"max_tokens":8000,"type":"model"}],"has_more":false,"first_id":"fixture-a","last_id":"fixture-a"}"#.into())
        } else {
            Answer::stream_in_pieces(include_str!("fixtures/anthropic/tool_turn.sse"), 37)
        }
    })
    .await;
    let p = provider::build(
        "anthropic",
        &config(ProviderKind::Anthropic, &server.base),
        key(),
    )
    .expect("provider");
    let models = p.list_models().await.expect("models");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["fixture-b", "fixture-a"]);

    let mut deltas = Vec::new();
    let mut on = |d: Delta| deltas.push(d);
    let reply = p.chat(&request("fixture-a"), &mut on).await.expect("reply");
    assert_eq!(reply.stop, StopReason::ToolUse);
    assert!(
        matches!(&reply.message.content[2], Block::ToolUse { name, .. } if name == "run_command")
    );

    let reqs = server.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[1].path, "/v1/models?limit=1000&after_id=fixture-b");
    for r in &reqs {
        assert_eq!(r.header("x-api-key"), Some(KEY));
        assert_eq!(r.header("anthropic-version"), Some("2026-10-21"));
    }
    let body = reqs[2].json();
    assert_eq!(reqs[2].method, "POST");
    assert_eq!(reqs[2].path, "/v1/messages");
    assert_eq!(body["model"], "fixture-a");
    assert_eq!(body["stream"], true);
    // The listed output limit is used when nothing else says.
    assert_eq!(body["max_tokens"], 8000);
    assert_eq!(body["tools"][0]["eager_input_streaming"], true);
    assert!(body.get("temperature").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn errors_carry_the_api_message_and_never_the_key() {
    let server = MockServer::start(|req| {
        if req.path.starts_with("/v1/models") {
            Answer::Json(401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.into())
        } else {
            Answer::Json(529, r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#.into())
        }
    })
    .await;
    let p = provider::build(
        "anthropic",
        &config(ProviderKind::Anthropic, &server.base),
        key(),
    )
    .expect("provider");
    let err = p.list_models().await.expect_err("401");
    assert_eq!(err.kind, ErrorKind::Auth);
    assert_eq!(err.message, "HTTP 401: invalid x-api-key");
    let mut on = |_d: Delta| {};
    let err = p
        .chat(&request("fixture-a"), &mut on)
        .await
        .expect_err("529");
    assert!(err.retryable());
    for shown in [err.to_string(), format!("{err:?}"), format!("{p:?}")] {
        assert!(!shown.contains(KEY), "{shown}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn openai_compatible_lists_and_streams_with_a_bearer_key() {
    let server = MockServer::start(|req| {
        if req.path == "/v1/models" {
            Answer::Json(
                200,
                include_str!("fixtures/openai/models_openrouter.json").into(),
            )
        } else {
            Answer::stream_in_pieces(include_str!("fixtures/openai/tool_turn.sse"), 23)
        }
    })
    .await;
    let base = format!("{}/v1", server.base);
    let p = provider::build(
        "router",
        &config(ProviderKind::OpenaiCompatible, &base),
        key(),
    )
    .expect("provider");
    let models = p.list_models().await.expect("models");
    assert_eq!(models.len(), 3);
    let mut text = String::new();
    let mut on = |d: Delta| {
        if let Delta::Text(t) = d {
            text.push_str(&t);
        }
    };
    let reply = p
        .chat(&request("fixture/priced"), &mut on)
        .await
        .expect("reply");
    assert_eq!(text, "Checking both.");
    assert_eq!(reply.message.content.len(), 3);
    let reqs = server.requests();
    assert_eq!(reqs[1].path, "/v1/chat/completions");
    assert_eq!(
        reqs[1].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    let body = reqs[1].json();
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert!(body.get("max_tokens").is_none() && body.get("max_completion_tokens").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_local_server_needs_no_key() {
    let server = MockServer::start(|_| {
        Answer::Stream(vec![
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n".into(),
            "data: [DONE]\n\n".into(),
        ])
    })
    .await;
    let mut cfg = config(ProviderKind::Local, &server.base);
    cfg.key = None;
    let p = provider::build("local", &cfg, None).expect("provider");
    let mut on = |_d: Delta| {};
    let reply = p
        .chat(&request("fixture-local"), &mut on)
        .await
        .expect("reply");
    assert_eq!(reply.message.text(), "hi");
    assert_eq!(reply.stop, StopReason::EndTurn);
    assert!(server.requests()[0].header("authorization").is_none());
}

#[test]
fn configuration_problems_say_what_to_do() {
    let mut cfg = config(ProviderKind::OpenaiCompatible, "x");
    cfg.base_url = None;
    let err = provider::build("lab", &cfg, None).expect_err("no url");
    assert!(err.message.contains("base-url"), "{}", err.message);
    let cfg = config(ProviderKind::Anthropic, "http://127.0.0.1:9");
    let err = provider::build("anthropic", &cfg, None).expect_err("no key");
    assert!(
        err.message.contains("termos secret set anthropic"),
        "{}",
        err.message
    );
}
