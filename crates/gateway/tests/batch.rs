//! Iter 19: OpenRouter Batch API against a mock backend.
//!
//! Tests batch submission (POST /api/v1/batches) returns 202 with batch object,
//! and batch status polling (GET /api/v1/batches/{id}) returns the batch status.

use axum::{
    body::Bytes,
    extract::Path,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Router,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const BATCH_SUBMIT_REQUEST: &str = r#"{"input_file_id":"file-abc123","endpoint":"/v1/chat/completions","completion_window":"24h","metadata":{"description":"test batch"}}"#;
const API_KEY: &str = "Bearer «redacted:sk-…»";

const BATCH_SUBMIT_RESPONSE: &[u8] = br#"{"id":"batch_abc123","object":"batch","endpoint":"/v1/chat/completions","status":"validating","input_file_id":"file-abc123","completion_window":"24h","created_at":1699999999,"metadata":{"description":"test batch"}}"#;

const BATCH_STATUS_RESPONSE: &[u8] = br#"{"id":"batch_abc123","object":"batch","endpoint":"/v1/chat/completions","status":"completed","input_file_id":"file-abc123","completion_window":"24h","created_at":1699999999,"completed_at":1700000000,"output_file_id":"file-xyz789","error_file_id":null,"metadata":{"description":"test batch"}}"#;

#[derive(Default)]
struct Seen {
    method: Option<String>,
    path: Option<String>,
    auth: Option<String>,
    body: Option<Vec<u8>>,
}

async fn spawn_batch_mock(seen: Arc<Mutex<Seen>>) -> SocketAddr {
    let seen_post = seen.clone();
    let seen_get = seen.clone();
    let app = Router::new()
        .route(
            "/api/v1/batches",
            post(move |headers: HeaderMap, req_body: Bytes| {
                let seen = seen_post.clone();
                async move {
                    *seen.lock().unwrap() = Seen {
                        method: Some("POST".to_string()),
                        path: Some("/api/v1/batches".to_string()),
                        auth: headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string),
                        body: Some(req_body.to_vec()),
                    };
                    (
                        StatusCode::ACCEPTED,
                        [("content-type", "application/json")],
                        BATCH_SUBMIT_RESPONSE.to_vec(),
                    )
                }
            }),
        )
        .route(
            "/api/v1/batches/:id",
            get(move |headers: HeaderMap, Path(id): Path<String>| {
                let seen = seen_get.clone();
                async move {
                    *seen.lock().unwrap() = Seen {
                        method: Some("GET".to_string()),
                        path: Some(format!("/api/v1/batches/{id}")),
                        auth: headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string),
                        body: None,
                    };
                    (
                        StatusCode::OK,
                        [("content-type", "application/json")],
                        BATCH_STATUS_RESPONSE.to_vec(),
                    )
                }
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

async fn post_batch(addr: SocketAddr) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/batches"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(BATCH_SUBMIT_REQUEST)
        .send()
        .await
        .unwrap()
}

async fn get_batch_status(addr: SocketAddr, id: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("http://{addr}/api/v1/batches/{id}"))
        .header("authorization", API_KEY)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn batch_submit_returns_202_with_batch_object() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_batch_mock(seen.clone()).await;
    let gw = spawn_gateway(mock).await;

    let res = post_batch(gw).await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let body = res.bytes().await.unwrap();
    assert_eq!(body.as_ref(), BATCH_SUBMIT_RESPONSE);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.method.as_deref(), Some("POST"));
    assert_eq!(seen.path.as_deref(), Some("/api/v1/batches"));
    assert_eq!(seen.auth.as_deref(), Some(API_KEY));
    assert_eq!(seen.body.as_deref(), Some(BATCH_SUBMIT_REQUEST.as_bytes()));
}

#[tokio::test]
async fn batch_status_poll_returns_batch_object() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_batch_mock(seen.clone()).await;
    let gw = spawn_gateway(mock).await;

    let res = get_batch_status(gw, "batch_abc123").await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.bytes().await.unwrap();
    assert_eq!(body.as_ref(), BATCH_STATUS_RESPONSE);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.method.as_deref(), Some("GET"));
    assert_eq!(seen.path.as_deref(), Some("/api/v1/batches/batch_abc123"));
    assert_eq!(seen.auth.as_deref(), Some(API_KEY));
    assert!(seen.body.is_none());
}

#[tokio::test]
async fn batch_endpoints_appear_in_metrics() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_batch_mock(seen.clone()).await;
    let gw = spawn_gateway(mock).await;

    // Submit a batch
    let _ = post_batch(gw).await;
    // Poll status
    let _ = get_batch_status(gw, "batch_abc123").await;

    // Check metrics
    let metrics_text = reqwest::get(format!("http://{gw}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    // Both routes should have recorded requests
    assert!(
        metrics_text.contains("pennyrouter_requests_total{route=\"batch_submit\""),
        "batch_submit route not in metrics: {metrics_text}"
    );
    assert!(
        metrics_text.contains("pennyrouter_requests_total{route=\"batch_status\""),
        "batch_status route not in metrics: {metrics_text}"
    );
}
