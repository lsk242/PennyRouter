//! Iter 9: bounded upstream concurrency with explicit load-shedding.
//!
//! A gateway configured with one upstream slot and no queue room sheds
//! the second concurrent request with 503 (never reaching upstream),
//! while a gateway with queue room serves it once the slot frees.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use std::{net::SocketAddr, time::Duration};

const API_KEY: &str = "Bearer [REDACTED]";

async fn spawn_mock(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn spawn_gateway(
    upstream: SocketAddr,
    limits: pennyrouter_gateway::ConcurrencyLimits,
) -> SocketAddr {
    let state = pennyrouter_gateway::AppState::with_limits(
        format!("http://{upstream}"),
        pennyrouter_gateway::Timeouts::default(),
        limits,
    );
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn chat_body() -> serde_json::Value {
    serde_json::json!({"model": "test-model", "messages": []})
}

async fn post_chat(gw: SocketAddr) -> reqwest::Response {
    client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .json(&chat_body())
        .send()
        .await
        .unwrap()
}

/// Slow mock: holds the single upstream slot long enough for a second
/// request to arrive while it is occupied.
fn slow_mock() -> Router {
    Router::new().route(
        "/v1/chat/completions",
        post(|_body: Bytes| async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            (StatusCode::OK, r#"{"id":"slow","choices":[]}"#)
        }),
    )
}

#[tokio::test]
async fn shed_beyond_capacity_with_503() {
    let mock = spawn_mock(slow_mock()).await;
    let gw = spawn_gateway(
        mock,
        pennyrouter_gateway::ConcurrencyLimits {
            max_upstream: 1,
            max_queued: 0,
        },
    )
    .await;

    let first = tokio::spawn(post_chat(gw));
    // Let the first request occupy the only upstream slot.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let shed = post_chat(gw).await;

    assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        shed.headers().get("retry-after").unwrap(),
        "1",
        "shed responses carry Retry-After",
    );
    let body: serde_json::Value = shed.json().await.unwrap();
    assert_eq!(
        body.pointer("/error/code").and_then(|code| code.as_str()),
        Some("service_unavailable"),
        "shed body is OpenAI-shaped, got {body}",
    );

    // The first request was unaffected by the shed.
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);

    let metrics = client()
        .get(format!("http://{gw}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("pennyrouter_load_shed_total 1"),
        "one shed counted, got:\n{metrics}",
    );
}

#[tokio::test]
async fn queued_request_served_when_slot_frees() {
    let mock = spawn_mock(slow_mock()).await;
    let gw = spawn_gateway(
        mock,
        pennyrouter_gateway::ConcurrencyLimits {
            max_upstream: 1,
            max_queued: 4,
        },
    )
    .await;

    let first = tokio::spawn(post_chat(gw));
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Queue room exists, so this waits instead of shedding.
    let second = post_chat(gw).await;

    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);

    let metrics = client()
        .get(format!("http://{gw}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("pennyrouter_load_shed_total 0"),
        "nothing shed, got:\n{metrics}",
    );
}
