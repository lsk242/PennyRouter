//! Iter 2: token guard, upstream error mapping, and timeouts.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

const API_KEY: &str = "Bearer sk-test-redacted";

async fn spawn_mock(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn spawn_gateway(
    upstream: SocketAddr,
    timeouts: pennyrouter_gateway::Timeouts,
) -> SocketAddr {
    let state =
        pennyrouter_gateway::AppState::with_timeouts(format!("http://{upstream}"), timeouts);
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[tokio::test]
async fn oversize_request_rejected_before_backend() {
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(move |_body: Bytes| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                (StatusCode::OK, "must never be reached")
            }
        }),
    ))
    .await;
    let gw = spawn_gateway(mock, pennyrouter_gateway::Timeouts::default()).await;

    // 5 MiB of text estimates ~1.25M tokens at 4 bytes/token: over the cap.
    let res = client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(vec![b'x'; 5 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let raw = res.bytes().await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(json["error"]["code"], "payload_too_large");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn upstream_plain_text_error_is_wrapped() {
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [("content-type", "text/plain")],
                "Bad things happened upstream",
            )
        }),
    ))
    .await;
    let gw = spawn_gateway(mock, pennyrouter_gateway::Timeouts::default()).await;

    let res = client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .body(r#"{"model":"m"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );
    let raw = res.bytes().await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(json["error"]["type"], "server_error");
    assert_eq!(json["error"]["code"], "upstream_error");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Bad things happened upstream"),
        "wrapped message keeps the upstream text: {json}"
    );
}

#[tokio::test]
async fn slow_upstream_headers_yield_504() {
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            (StatusCode::OK, "too late")
        }),
    ))
    .await;
    let timeouts = pennyrouter_gateway::Timeouts {
        first_chunk: Duration::from_millis(200),
        ..pennyrouter_gateway::Timeouts::default()
    };
    let gw = spawn_gateway(mock, timeouts).await;

    let res = client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .body(r#"{"model":"m"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
    let raw = res.bytes().await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(json["error"]["code"], "gateway_timeout");
}
