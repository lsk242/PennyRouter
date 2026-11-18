//! Iter 9: bounded upstream concurrency with explicit load-shedding.
//!
//! A gateway configured with one upstream slot and no queue room sheds
//! the second concurrent request with 503 (never reaching upstream),
//! while a gateway with queue room serves it once the slot frees.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use std::{net::SocketAddr, sync::Arc, time::Duration};

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
    spawn_gateway_with_timeouts(upstream, limits, pennyrouter_gateway::Timeouts::default()).await
}

async fn spawn_gateway_with_timeouts(
    upstream: SocketAddr,
    limits: pennyrouter_gateway::ConcurrencyLimits,
    timeouts: pennyrouter_gateway::Timeouts,
) -> SocketAddr {
    let state =
        pennyrouter_gateway::AppState::with_limits(format!("http://{upstream}"), timeouts, limits);
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

/// SSE mock: sends headers + one chunk, then holds the stream open
/// until `release` fires. Proves whether the gateway's upstream slot
/// stays held for the whole live stream (C01) or drops at headers.
fn hanging_sse_mock(release: Arc<tokio::sync::Notify>) -> Router {
    Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let release = release.clone();
            async move {
                let stream = futures_util::stream::unfold(false, move |done| {
                    let release = release.clone();
                    async move {
                        if !done {
                            Some((
                                Ok::<_, std::convert::Infallible>(Bytes::from(
                                    "data: {\"c\":1}\n\n",
                                )),
                                true,
                            ))
                        } else {
                            release.notified().await;
                            None
                        }
                    }
                });
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }
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

/// C01 probe: with one upstream slot and no queue room, a second
/// request arriving while an SSE stream is still open must shed —
/// the slot stays held for the whole live stream, not just until
/// headers. After the first stream closes, a third request proceeds.
#[tokio::test]
async fn open_sse_stream_holds_the_upstream_slot() {
    let release = Arc::new(tokio::sync::Notify::new());
    let mock = spawn_mock(hanging_sse_mock(release.clone())).await;
    let gw = spawn_gateway(
        mock,
        pennyrouter_gateway::ConcurrencyLimits {
            max_upstream: 1,
            max_queued: 0,
        },
    )
    .await;

    let mut first = post_chat(gw).await;
    // First chunk through the gateway: upstream headers returned, the
    // stream is open.
    assert!(first.chunk().await.unwrap().is_some(), "stream starts");

    let shed = post_chat(gw).await;
    assert_eq!(
        shed.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "second request sheds while the stream is open"
    );

    // Close the first stream, drain it so the slot releases, then the
    // next request must proceed.
    release.notify_one();
    while first.chunk().await.unwrap().is_some() {}
    let third = post_chat(gw).await;
    assert_eq!(third.status(), StatusCode::OK);
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

/// Q04-A: a queued request that never gets a slot sheds with 503
/// inside the queue deadline (here a short injected timeout standing
/// in for the 30s first-chunk budget) instead of waiting forever.
#[tokio::test]
async fn queued_request_sheds_after_deadline() {
    let release = Arc::new(tokio::sync::Notify::new());
    let mock = spawn_mock(hanging_sse_mock(release.clone())).await;
    let timeouts = pennyrouter_gateway::Timeouts {
        first_chunk: Duration::from_millis(300),
        ..pennyrouter_gateway::Timeouts::default()
    };
    let gw = spawn_gateway_with_timeouts(
        mock,
        pennyrouter_gateway::ConcurrencyLimits {
            max_upstream: 1,
            max_queued: 1,
        },
        timeouts,
    )
    .await;

    // Occupy the only slot with a stream that never ends (headers
    // arrive, body hangs on `release`).
    let first = tokio::spawn(post_chat(gw));
    tokio::time::sleep(Duration::from_millis(150)).await;
    // Queue room exists, so this waits — then sheds at the deadline.
    let start = std::time::Instant::now();
    let shed = post_chat(gw).await;
    let waited = start.elapsed();

    assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(shed.headers().get("retry-after").unwrap(), "1");
    assert!(
        waited < Duration::from_secs(10),
        "queued request must shed, not wait forever (waited {waited:?})"
    );
    release.notify_one();
    let _ = first.await;
}
