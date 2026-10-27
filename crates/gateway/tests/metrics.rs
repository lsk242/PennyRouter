//! Iter 3: /metrics endpoint, request counting, and request IDs.

use axum::{http::StatusCode, routing::post, Router};
use std::net::SocketAddr;

const CHAT_COMPLETION: &[u8] = include_bytes!("fixtures/chat_completion.json");
const ERROR_401: &[u8] = include_bytes!("fixtures/error_401.json");

async fn spawn_mock(status: StatusCode, body: &'static [u8]) -> SocketAddr {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            (
                status,
                [("content-type", "application/json")],
                body.to_vec(),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn spawn_gateway(upstream: SocketAddr) -> SocketAddr {
    let state = pennyrouter_gateway::AppState::new(format!("http://{upstream}"));
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn metrics_text(gw: SocketAddr) -> String {
    let res = reqwest::Client::new()
        .get(format!("http://{gw}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    res.text().await.unwrap()
}

#[tokio::test]
async fn metrics_reports_requests_and_latency() {
    let mock = spawn_mock(StatusCode::OK, CHAT_COMPLETION).await;
    let gw = spawn_gateway(mock).await;
    let client = reqwest::Client::new();

    client
        .get(format!("http://{gw}/health"))
        .send()
        .await
        .unwrap();
    for _ in 0..2 {
        client
            .post(format!("http://{gw}/v1/chat/completions"))
            .header("authorization", "Bearer sk-test-redacted")
            .body(r#"{"model":"m"}"#)
            .send()
            .await
            .unwrap();
    }

    let text = metrics_text(gw).await;
    assert!(
        text.contains("pennyrouter_requests_total{route=\"health\",status=\"200\"} 1"),
        "{text}"
    );
    assert!(
        text.contains("pennyrouter_requests_total{route=\"chat\",status=\"200\"} 2"),
        "{text}"
    );
    assert!(
        text.contains("pennyrouter_request_latency_seconds_count{route=\"chat\"} 2"),
        "{text}"
    );
    assert!(
        text.contains("pennyrouter_request_latency_seconds_bucket{route=\"chat\",le=\"+Inf\"} 2"),
        "{text}"
    );
    // The scrape observes itself: exactly the scraping request is in flight.
    assert!(text.contains("pennyrouter_requests_in_flight 1"), "{text}");
}

#[tokio::test]
async fn guard_rejection_and_upstream_error_counted() {
    let mock = spawn_mock(StatusCode::UNAUTHORIZED, ERROR_401).await;
    let gw = spawn_gateway(mock).await;
    let client = reqwest::Client::new();

    let res = client
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", "Bearer sk-test-redacted")
        .body(vec![b'x'; 5 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let res = client
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", "Bearer sk-test-redacted")
        .body(r#"{"model":"m"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let text = metrics_text(gw).await;
    assert!(
        text.contains("pennyrouter_guard_rejections_total 1"),
        "{text}"
    );
    assert!(
        text.contains("pennyrouter_upstream_errors_total{kind=\"status_4xx\"} 1"),
        "{text}"
    );
    assert!(
        text.contains("pennyrouter_requests_total{route=\"chat\",status=\"413\"} 1"),
        "{text}"
    );
}

#[tokio::test]
async fn request_id_propagated_or_generated() {
    let mock = spawn_mock(StatusCode::OK, CHAT_COMPLETION).await;
    let gw = spawn_gateway(mock).await;
    let client = reqwest::Client::new();

    let res = client
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", "Bearer sk-test-redacted")
        .header("x-request-id", "abc-123")
        .body(r#"{"model":"m"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.headers().get("x-request-id").unwrap(), "abc-123");

    let res = client
        .get(format!("http://{gw}/health"))
        .send()
        .await
        .unwrap();
    assert!(
        res.headers().get("x-request-id").is_some(),
        "gateway generates a request id when the caller sends none"
    );
}
