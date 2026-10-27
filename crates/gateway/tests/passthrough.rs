//! Iter 1b: non-streaming passthrough against a mock backend.
//!
//! The mock records what the gateway sent and the test asserts afterwards,
//! so failures point at the test rather than a dead handler task. Fixtures
//! are synthetic OpenRouter-shaped bodies and contain no secrets.

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const REQUEST_BODY: &str =
    r#"{"model":"openai/gpt-oss-20b","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
const API_KEY: &str = "Bearer sk-test-redacted";

const CHAT_COMPLETION: &[u8] = include_bytes!("fixtures/chat_completion.json");
const ERROR_401: &[u8] = include_bytes!("fixtures/error_401.json");

#[derive(Default)]
struct Seen {
    auth: Option<String>,
    body: Option<Vec<u8>>,
}

async fn spawn_mock(status: StatusCode, body: &'static [u8], seen: Arc<Mutex<Seen>>) -> SocketAddr {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, req_body: Bytes| {
            let seen = seen.clone();
            async move {
                *seen.lock().unwrap() = Seen {
                    auth: headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string),
                    body: Some(req_body.to_vec()),
                };
                (
                    status,
                    [("content-type", "application/json")],
                    body.to_vec(),
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

async fn post_chat(addr: SocketAddr) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(REQUEST_BODY)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn success_body_is_byte_identical_and_request_forwarded() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(StatusCode::OK, CHAT_COMPLETION, seen.clone()).await;
    let gw = spawn_gateway(mock).await;

    let res = post_chat(gw).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.bytes().await.unwrap().as_ref(), CHAT_COMPLETION);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.auth.as_deref(), Some(API_KEY));
    assert_eq!(seen.body.as_deref(), Some(REQUEST_BODY.as_bytes()));
}

#[tokio::test]
async fn upstream_error_status_and_body_are_preserved() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(StatusCode::UNAUTHORIZED, ERROR_401, seen.clone()).await;
    let gw = spawn_gateway(mock).await;

    let res = post_chat(gw).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(res.bytes().await.unwrap().as_ref(), ERROR_401);
}
