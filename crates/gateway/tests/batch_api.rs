//! Iter 38: OpenRouter Batch API against a mock backend.
//!
//! Tests batch submission (POST /api/v1/batches) returns 202 with batch object,
//! and batch status polling (GET /api/v1/batches/{id}) returns the batch status.
//! (Named `batch_api` because `batch.rs` already covers batch-tier routing.)
//!
//! The gateway-facing paths keep the native `/api` prefix; upstream they
//! forward stripped (`/v1/batches…`) so the configured `/api` base never
//! doubles it (Iter 25 convention, like the other native handlers).

use axum::{
    body::Bytes,
    extract::Path,
    http::{HeaderMap, StatusCode, Uri},
    routing::{get, post},
    Router,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const BATCH_SUBMIT_REQUEST: &str = r#"{"input_file_id":"file-abc123","endpoint":"/v1/chat/completions","completion_window":"24h","metadata":{"description":"test batch"}}"#;
const API_KEY: &str = "Bearer test-batch-key-0001";

const BATCH_SUBMIT_RESPONSE: &[u8] = br#"{"id":"batch_abc123","object":"batch","endpoint":"/v1/chat/completions","status":"validating","input_file_id":"file-abc123","completion_window":"24h","created_at":1699999999,"metadata":{"description":"test batch"}}"#;

const BATCH_STATUS_RESPONSE: &[u8] = br#"{"id":"batch_abc123","object":"batch","endpoint":"/v1/chat/completions","status":"completed","input_file_id":"file-abc123","completion_window":"24h","created_at":1699999999,"completed_at":1700000000,"output_file_id":"file-xyz789","error_file_id":null,"metadata":{"description":"test batch"}}"#;

#[derive(Default)]
struct Seen {
    method: Option<String>,
    path: Option<String>,
    query: Option<String>,
    auth: Option<String>,
    body: Option<Vec<u8>>,
}

fn auth_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn spawn_batch_mock(seen: Arc<Mutex<Seen>>) -> SocketAddr {
    let seen_post = seen.clone();
    let seen_get = seen.clone();
    let app = Router::new()
        .route(
            "/v1/batches",
            post(move |headers: HeaderMap, uri: Uri, req_body: Bytes| {
                let seen = seen_post.clone();
                async move {
                    *seen.lock().unwrap() = Seen {
                        method: Some("POST".to_string()),
                        path: Some(uri.path().to_string()),
                        query: uri.query().map(str::to_string),
                        auth: auth_of(&headers),
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
            "/v1/batches/{id}",
            get(
                move |headers: HeaderMap, uri: Uri, Path(id): Path<String>| {
                    let seen = seen_get.clone();
                    async move {
                        assert_eq!(uri.path(), format!("/v1/batches/{id}"));
                        *seen.lock().unwrap() = Seen {
                            method: Some("GET".to_string()),
                            path: Some(uri.path().to_string()),
                            query: uri.query().map(str::to_string),
                            auth: auth_of(&headers),
                            body: None,
                        };
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            BATCH_STATUS_RESPONSE.to_vec(),
                        )
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Mock for a default-style upstream base carrying the `/api` route
/// prefix: the stripped gateway path composes back to the full
/// native upstream path (`{base}/v1/batches/…`).
async fn spawn_native_base_mock(seen: Arc<Mutex<Seen>>) -> SocketAddr {
    let app = Router::new().route(
        "/api/v1/batches/{id}",
        get(
            move |headers: HeaderMap, uri: Uri, Path(id): Path<String>| {
                let seen = seen.clone();
                async move {
                    assert_eq!(uri.path(), format!("/api/v1/batches/{id}"));
                    *seen.lock().unwrap() = Seen {
                        method: Some("GET".to_string()),
                        path: Some(uri.path().to_string()),
                        query: uri.query().map(str::to_string),
                        auth: auth_of(&headers),
                        body: None,
                    };
                    (
                        StatusCode::OK,
                        [("content-type", "application/json")],
                        BATCH_STATUS_RESPONSE.to_vec(),
                    )
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn spawn_gateway(upstream: SocketAddr) -> SocketAddr {
    spawn_gateway_with_base(format!("http://{upstream}")).await
}

async fn spawn_gateway_with_base(base: String) -> SocketAddr {
    let state = pennyrouter_gateway::AppState::new(base);
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
    get_batch_status_with_query(addr, id, None).await
}

async fn get_batch_status_with_query(
    addr: SocketAddr,
    id: &str,
    query: Option<&str>,
) -> reqwest::Response {
    let mut url = format!("http://{addr}/api/v1/batches/{id}");
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    reqwest::Client::new()
        .get(url)
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
    assert_eq!(seen.path.as_deref(), Some("/v1/batches"));
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
    assert_eq!(seen.path.as_deref(), Some("/v1/batches/batch_abc123"));
    assert_eq!(seen.auth.as_deref(), Some(API_KEY));
    assert!(seen.body.is_none());
}

#[tokio::test]
async fn batch_status_preserves_query_string() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_batch_mock(seen.clone()).await;
    let gw = spawn_gateway(mock).await;

    let res = get_batch_status_with_query(gw, "batch_abc123", Some("verbose=true")).await;
    assert_eq!(res.status(), StatusCode::OK);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.method.as_deref(), Some("GET"));
    assert_eq!(seen.path.as_deref(), Some("/v1/batches/batch_abc123"));
    assert_eq!(seen.query.as_deref(), Some("verbose=true"));
}

#[tokio::test]
async fn batch_status_strips_configured_api_suffix() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_native_base_mock(seen.clone()).await;
    // Default-style base carrying the `/api` route prefix: without the
    // strip this composes to `/api/api/v1/batches/…` (mock 404).
    let gw = spawn_gateway_with_base(format!("http://{mock}/api")).await;

    let res = get_batch_status(gw, "batch_abc123").await;
    assert_eq!(res.status(), StatusCode::OK);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.method.as_deref(), Some("GET"));
    assert_eq!(seen.path.as_deref(), Some("/api/v1/batches/batch_abc123"));
    assert_eq!(seen.auth.as_deref(), Some(API_KEY));
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
