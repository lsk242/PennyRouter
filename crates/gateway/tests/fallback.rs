//! Iter 8: drop-in fallback proxy against a mock backend.
//!
//! Paths without a custom handler are proxied upstream untouched (method,
//! path, query, headers, body) and answered byte-for-byte; oversize
//! bodies are still rejected pre-backend. Same record-then-assert shape
//! as the passthrough tests.

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode, Uri},
    routing::{get, post},
    Router,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const API_KEY: &str = "Bearer [REDACTED]";
const MODELS: &[u8] = br#"{"object":"list","data":[{"id":"m1","object":"model"}]}"#;
const EMBEDDING: &[u8] =
    br#"{"object":"list","data":[{"object":"embedding","embedding":[0.1],"index":0}]}"#;
const EMBEDDING_REQUEST: &str = r#"{"model":"m1","input":"hi"}"#;

#[derive(Default)]
struct Seen {
    hits: u64,
    method: Option<String>,
    path: Option<String>,
    query: Option<String>,
    auth: Option<String>,
    body: Option<Vec<u8>>,
}

async fn spawn_mock(seen: Arc<Mutex<Seen>>) -> SocketAddr {
    let seen_models = seen.clone();
    let seen_embeddings = seen.clone();
    let app = Router::new()
        .route(
            "/v1/models",
            get(move |uri: Uri, headers: HeaderMap| {
                let seen = seen_models.clone();
                async move {
                    let mut seen = seen.lock().unwrap();
                    seen.hits += 1;
                    seen.method = Some("GET".to_string());
                    seen.path = Some(uri.path().to_string());
                    seen.query = uri.query().map(str::to_string);
                    seen.auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    (
                        StatusCode::OK,
                        [("content-type", "application/json")],
                        MODELS.to_vec(),
                    )
                }
            }),
        )
        .route(
            "/v1/embeddings",
            post(move |uri: Uri, headers: HeaderMap, req_body: Bytes| {
                let seen = seen_embeddings.clone();
                async move {
                    let mut seen = seen.lock().unwrap();
                    seen.hits += 1;
                    seen.method = Some("POST".to_string());
                    seen.path = Some(uri.path().to_string());
                    seen.query = uri.query().map(str::to_string);
                    seen.auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    seen.body = Some(req_body.to_vec());
                    (
                        StatusCode::OK,
                        [("content-type", "application/json")],
                        EMBEDDING.to_vec(),
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

#[tokio::test]
async fn unknown_post_body_query_and_auth_are_forwarded() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(mock).await;

    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/embeddings?limit=2"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(EMBEDDING_REQUEST)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.bytes().await.unwrap().as_ref(), EMBEDDING);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.hits, 1);
    assert_eq!(seen.method.as_deref(), Some("POST"));
    assert_eq!(seen.path.as_deref(), Some("/v1/embeddings"));
    assert_eq!(seen.query.as_deref(), Some("limit=2"));
    assert_eq!(seen.auth.as_deref(), Some(API_KEY));
    assert_eq!(seen.body.as_deref(), Some(EMBEDDING_REQUEST.as_bytes()));
}

#[tokio::test]
async fn fallback_rejects_oversize_body_pre_upstream() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(mock).await;

    let big = vec![b'x'; 4 * 1024 * 1024 + 1];
    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/embeddings"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(seen.lock().unwrap().hits, 0);
}
