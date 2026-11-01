//! Iter 25: OpenRouter-native interface.
//!
//! Native SDK paths under `/api/v1/…` work through the gateway: the two
//! routed calls alias to the same handler (rewritten to `/v1/…`
//! upstream), account/catalog paths proxy without doubling `/api`, and
//! management writes fail closed unless explicitly allowed.

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
const CREDITS: &[u8] = br#"{"total_credits":10.5,"total_usage":1.25}"#;
const KEY_INFO: &[u8] = br#"{"label":"test-key","limit":null}"#;
const NATIVE_MODELS: &[u8] = br#"{"data":[{"id":"openai/gpt-4o"}]}"#;
const CHAT_OK: &[u8] = br#"{"id":"chatcmpl-1","object":"chat.completion","choices":[]}"#;
const RESPONSES_OK: &[u8] = br#"{"id":"resp-1","object":"response","status":"completed"}"#;
const EMBEDDING_OK: &[u8] = br#"{"object":"list","data":[]}"#;

#[derive(Default)]
struct Seen {
    hits: u64,
    method: Option<String>,
    path: Option<String>,
    query: Option<String>,
    auth: Option<String>,
    body: Option<Vec<u8>>,
}

fn record(seen: &Mutex<Seen>, method: &str, uri: &Uri, headers: &HeaderMap, body: Option<&[u8]>) {
    let mut seen = seen.lock().unwrap();
    seen.hits += 1;
    seen.method = Some(method.to_string());
    seen.path = Some(uri.path().to_string());
    seen.query = uri.query().map(str::to_string);
    seen.auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(body) = body {
        seen.body = Some(body.to_vec());
    }
}

async fn spawn_mock(seen: Arc<Mutex<Seen>>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/api/v1/credits",
            get({
                let seen = seen.clone();
                move |uri: Uri, headers: HeaderMap| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, "GET", &uri, &headers, None);
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            CREDITS.to_vec(),
                        )
                    }
                }
            }),
        )
        .route(
            "/api/v1/auth/key",
            get({
                let seen = seen.clone();
                move |uri: Uri, headers: HeaderMap| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, "GET", &uri, &headers, None);
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            KEY_INFO.to_vec(),
                        )
                    }
                }
            }),
        )
        .route(
            "/api/v1/models",
            get({
                let seen = seen.clone();
                move |uri: Uri, headers: HeaderMap| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, "GET", &uri, &headers, None);
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            NATIVE_MODELS.to_vec(),
                        )
                    }
                }
            }),
        )
        .route(
            "/api/v1/keys",
            post({
                let seen = seen.clone();
                move |uri: Uri, headers: HeaderMap, body: Bytes| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, "POST", &uri, &headers, Some(&body));
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            KEY_INFO.to_vec(),
                        )
                    }
                }
            }),
        )
        .route(
            "/v1/chat/completions",
            post({
                let seen = seen.clone();
                move |uri: Uri, headers: HeaderMap, body: Bytes| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, "POST", &uri, &headers, Some(&body));
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            CHAT_OK.to_vec(),
                        )
                    }
                }
            }),
        )
        .route(
            "/v1/responses",
            post({
                let seen = seen.clone();
                move |uri: Uri, headers: HeaderMap, body: Bytes| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, "POST", &uri, &headers, Some(&body));
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            RESPONSES_OK.to_vec(),
                        )
                    }
                }
            }),
        )
        .route(
            "/v1/embeddings",
            post({
                let seen = seen.clone();
                move |uri: Uri, headers: HeaderMap, body: Bytes| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, "POST", &uri, &headers, Some(&body));
                        (
                            StatusCode::OK,
                            [("content-type", "application/json")],
                            EMBEDDING_OK.to_vec(),
                        )
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn spawn_gateway(state: pennyrouter_gateway::AppState) -> SocketAddr {
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
async fn native_account_path_proxies_without_doubling() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!("http://{mock}"))).await;

    let res = client()
        .get(format!("http://{gw}/api/v1/credits?limit=1"))
        .header("authorization", API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.bytes().await.unwrap().as_ref(), CREDITS);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.hits, 1);
    assert_eq!(seen.method.as_deref(), Some("GET"));
    assert_eq!(seen.path.as_deref(), Some("/api/v1/credits"));
    assert_eq!(seen.query.as_deref(), Some("limit=1"));
    assert_eq!(seen.auth.as_deref(), Some(API_KEY));
}

#[tokio::test]
async fn native_path_strips_configured_api_suffix() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    // Default-style base carrying the `/api` route prefix: without the
    // root derivation this composes to `/api/api/v1/credits` (mock 404).
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!(
        "http://{mock}/api"
    )))
    .await;

    let res = client()
        .get(format!("http://{gw}/api/v1/credits"))
        .header("authorization", API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.hits, 1);
    assert_eq!(seen.path.as_deref(), Some("/api/v1/credits"));
}

#[tokio::test]
async fn native_models_proxies_upstream_catalog() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!("http://{mock}"))).await;

    let res = client()
        .get(format!("http://{gw}/api/v1/models"))
        .header("authorization", API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // The real catalog, not the curated six-id `/v1/models` menu.
    assert_eq!(res.bytes().await.unwrap().as_ref(), NATIVE_MODELS);
    assert_eq!(seen.lock().unwrap().path.as_deref(), Some("/api/v1/models"));
}

#[tokio::test]
async fn chat_alias_rewrites_to_v1_upstream() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!("http://{mock}"))).await;

    let res = client()
        .post(format!("http://{gw}/api/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(r#"{"model":"m1","messages":[]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.bytes().await.unwrap().as_ref(), CHAT_OK);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.hits, 1);
    assert_eq!(seen.path.as_deref(), Some("/v1/chat/completions"));
    assert_eq!(seen.auth.as_deref(), Some(API_KEY));
    let body = String::from_utf8(seen.body.clone().unwrap()).unwrap();
    assert!(body.contains("\"m1\""), "{body}");
}

#[tokio::test]
async fn responses_alias_rewrites_to_v1_upstream() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!("http://{mock}"))).await;

    let res = client()
        .post(format!("http://{gw}/api/v1/responses"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(r#"{"model":"m1","input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.bytes().await.unwrap().as_ref(), RESPONSES_OK);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.hits, 1);
    assert_eq!(seen.path.as_deref(), Some("/v1/responses"));
}

#[tokio::test]
async fn non_native_paths_proxy_unchanged() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!("http://{mock}"))).await;

    let res = client()
        .post(format!("http://{gw}/v1/embeddings"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(r#"{"model":"m1","input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.hits, 1);
    assert_eq!(seen.path.as_deref(), Some("/v1/embeddings"));
}

#[tokio::test]
async fn management_key_write_denied_by_default() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!("http://{mock}"))).await;

    let res = client()
        .post(format!("http://{gw}/api/v1/keys"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(r#"{"name":"new-key"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body = res.text().await.unwrap();
    assert!(body.contains("management_proxy_disabled"), "{body}");
    assert_eq!(seen.lock().unwrap().hits, 0);
}

#[tokio::test]
async fn management_money_writes_denied_by_default() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!("http://{mock}"))).await;

    for path in [
        "/api/v1/credits/coinbase",
        "/api/v1/workspaces/w1/budgets",
        "/api/v1/auth/keys",
    ] {
        let res = client()
            .post(format!("http://{gw}{path}"))
            .header("authorization", API_KEY)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(seen.lock().unwrap().hits, 0);
}

#[tokio::test]
async fn management_reads_still_proxy() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(pennyrouter_gateway::AppState::new(format!("http://{mock}"))).await;

    let res = client()
        .get(format!("http://{gw}/api/v1/auth/key"))
        .header("authorization", API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.bytes().await.unwrap().as_ref(), KEY_INFO);
}

#[tokio::test]
async fn management_write_allowed_with_flag() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(
        pennyrouter_gateway::AppState::new(format!("http://{mock}")).with_management_proxy(true),
    )
    .await;

    let res = client()
        .post(format!("http://{gw}/api/v1/keys"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(r#"{"name":"new-key"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.hits, 1);
    assert_eq!(seen.path.as_deref(), Some("/api/v1/keys"));
}
