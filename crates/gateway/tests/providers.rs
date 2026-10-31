//! Iter 16: provider-pinned direct routes (`modal/…`, `baseten/…`).
//!
//! Pinned ids bypass the catalog and address that provider's upstream
//! with the stripped model id, no OpenRouter-specific fields, and the
//! caller's Authorization forwarded unchanged (pure BYOK, single
//! token). No router is configured on purpose: pinned dispatch must
//! work without a catalog.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const TOKEN: &str = "Bearer single-token";

#[derive(Default)]
struct Seen {
    hits: u64,
    models: Vec<String>,
    providers: Vec<Option<String>>,
    auths: Vec<Option<String>>,
}

fn mock_app(seen: Arc<Mutex<Seen>>) -> Router {
    let handler = move |headers: axum::http::HeaderMap, body: Bytes| {
        let seen = seen.clone();
        async move {
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let mut seen = seen.lock().unwrap();
            seen.hits += 1;
            seen.models.push(
                json.get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or("?")
                    .to_string(),
            );
            seen.providers
                .push(json.get("provider").map(|p| p.to_string()));
            seen.auths.push(
                headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            );
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                r#"{"id":"ok"}"#.to_string(),
            )
        }
    };
    Router::new()
        .route("/v1/chat/completions", post(handler.clone()))
        .route("/v1/responses", post(handler))
}

async fn spawn_mock(seen: Arc<Mutex<Seen>>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mock_app(seen);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn spawn_gateway(default: SocketAddr, extra: &[(&str, SocketAddr)]) -> SocketAddr {
    let mut state = pennyrouter_gateway::AppState::new(format!("http://{default}"));
    for (provider, addr) in extra {
        state = state.with_upstream(provider, &format!("http://{addr}"));
    }
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn post_model(gw: SocketAddr, path: &str, body: &str) -> reqwest::Response {
    client()
        .post(format!("http://{gw}{path}"))
        .header("authorization", TOKEN)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn modal_chat_routes_to_modal_upstream() {
    let default = Arc::new(Mutex::new(Seen::default()));
    let modal = Arc::new(Mutex::new(Seen::default()));
    let baseten = Arc::new(Mutex::new(Seen::default()));
    let a = spawn_mock(default.clone()).await;
    let b = spawn_mock(modal.clone()).await;
    let c = spawn_mock(baseten.clone()).await;
    let gw = spawn_gateway(a, &[("modal", b), ("baseten", c)]).await;

    let res = post_model(
        gw,
        "/v1/chat/completions",
        r#"{"model":"modal/my-model","messages":[]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "modal"
    );
    let modal = modal.lock().unwrap();
    assert_eq!(modal.hits, 1);
    assert_eq!(modal.models, vec!["my-model".to_string()]);
    assert_eq!(modal.providers, vec![None], "no provider object sent");
    assert_eq!(modal.auths, vec![Some(TOKEN.to_string())]);
    assert_eq!(default.lock().unwrap().hits, 0);
    assert_eq!(baseten.lock().unwrap().hits, 0);
}

#[tokio::test]
async fn baseten_responses_routes_to_baseten_upstream() {
    let default = Arc::new(Mutex::new(Seen::default()));
    let modal = Arc::new(Mutex::new(Seen::default()));
    let baseten = Arc::new(Mutex::new(Seen::default()));
    let a = spawn_mock(default.clone()).await;
    let b = spawn_mock(modal.clone()).await;
    let c = spawn_mock(baseten.clone()).await;
    let gw = spawn_gateway(a, &[("modal", b), ("baseten", c)]).await;

    // Caller OpenRouter fields are dropped, not forwarded.
    let res = post_model(
        gw,
        "/v1/responses",
        r#"{"model":"baseten/other-model","input":"hi",
            "provider":{"order":["x"],"allow_fallbacks":false,"zdr":true}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "baseten"
    );
    let baseten = baseten.lock().unwrap();
    assert_eq!(baseten.hits, 1);
    assert_eq!(baseten.models, vec!["other-model".to_string()]);
    assert_eq!(baseten.providers, vec![None]);
    assert_eq!(baseten.auths, vec![Some(TOKEN.to_string())]);
    assert_eq!(default.lock().unwrap().hits, 0);
    assert_eq!(modal.lock().unwrap().hits, 0);
}

#[tokio::test]
async fn unprefixed_models_still_use_default_upstream() {
    let default = Arc::new(Mutex::new(Seen::default()));
    let modal = Arc::new(Mutex::new(Seen::default()));
    let a = spawn_mock(default.clone()).await;
    let b = spawn_mock(modal.clone()).await;
    let gw = spawn_gateway(a, &[("modal", b)]).await;

    let res = post_model(
        gw,
        "/v1/chat/completions",
        r#"{"model":"some-model","messages":[]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let default = default.lock().unwrap();
    assert_eq!(default.hits, 1);
    assert_eq!(default.models, vec!["some-model".to_string()]);
    assert_eq!(modal.lock().unwrap().hits, 0);
}

#[tokio::test]
async fn unconfigured_provider_fails_closed() {
    let default = Arc::new(Mutex::new(Seen::default()));
    let a = spawn_mock(default.clone()).await;
    // baseten configured, modal not.
    let gw = spawn_gateway(a, &[("baseten", a)]).await;

    let res = post_model(
        gw,
        "/v1/chat/completions",
        r#"{"model":"modal/my-model","messages":[]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "provider_not_configured");
    // No leak to the default upstream either.
    assert_eq!(default.lock().unwrap().hits, 0);
}

#[test]
fn with_upstream_strips_a_trailing_v1() {
    // The gateway appends the full incoming `/v1/…` path itself, so a
    // base that already ends in `/v1` (the documented Baseten form)
    // must be normalized at registration, not doubled per request.
    let state = pennyrouter_gateway::AppState::new("http://127.0.0.1:9")
        .with_upstream("baseten", "https://inference.baseten.co/v1");
    assert_eq!(
        state.pinned_upstream("baseten"),
        Some("https://inference.baseten.co")
    );
    let state = pennyrouter_gateway::AppState::new("http://127.0.0.1:9")
        .with_upstream("baseten", "https://inference.baseten.co/v1/");
    assert_eq!(
        state.pinned_upstream("baseten"),
        Some("https://inference.baseten.co")
    );
    // Other bases pass through untouched (trailing slash aside).
    let state = pennyrouter_gateway::AppState::new("http://127.0.0.1:9")
        .with_upstream("modal", "https://example.modal.direct/");
    assert_eq!(
        state.pinned_upstream("modal"),
        Some("https://example.modal.direct")
    );
    let state = pennyrouter_gateway::AppState::new("http://127.0.0.1:9")
        .with_upstream("modal", "https://example.modal.direct/v10");
    assert_eq!(
        state.pinned_upstream("modal"),
        Some("https://example.modal.direct/v10")
    );
}

#[tokio::test]
async fn v1_suffixed_pinned_base_does_not_double_the_path() {
    // End-to-end form of the above: with a `/v1`-suffixed base the
    // mock must see the single-prefix path (it has no `/v1/v1/…`
    // route, so a doubled path would 404, as Baseten did live).
    let baseten = Arc::new(Mutex::new(Seen::default()));
    let unused = Arc::new(Mutex::new(Seen::default()));
    let a = spawn_mock(unused.clone()).await;
    let c = spawn_mock(baseten.clone()).await;
    let state = pennyrouter_gateway::AppState::new(format!("http://{a}"))
        .with_upstream("baseten", &format!("http://{c}/v1"));
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let res = post_model(
        gw,
        "/v1/chat/completions",
        r#"{"model":"baseten/other-model","messages":[]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "baseten"
    );
    let baseten = baseten.lock().unwrap();
    assert_eq!(baseten.hits, 1);
    assert_eq!(baseten.models, vec!["other-model".to_string()]);
    assert_eq!(unused.lock().unwrap().hits, 0);
}

#[tokio::test]
async fn empty_remainder_is_not_a_pinned_route() {
    let default = Arc::new(Mutex::new(Seen::default()));
    let a = spawn_mock(default.clone()).await;
    let gw = spawn_gateway(a, &[]).await;

    // `modal/` with no model passes through untouched (no router).
    let res = post_model(gw, "/v1/chat/completions", r#"{"model":"modal/"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    let default = default.lock().unwrap();
    assert_eq!(default.hits, 1);
    assert_eq!(default.models, vec!["modal/".to_string()]);
}
