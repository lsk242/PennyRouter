//! Iter 2: SSE streaming passthrough against a mock backend.
//!
//! The gateway keys off the upstream content type, not the request's
//! `stream` flag: an event-stream response is forwarded chunk-by-chunk
//! with bytes and order preserved, never buffered.

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

const STREAM_REQUEST: &str =
    r#"{"model":"openai/gpt-oss-20b","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
const API_KEY: &str = "Bearer sk-test-redacted";

const STREAM: &[u8] = include_bytes!("fixtures/stream.sse");

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

#[tokio::test]
async fn sse_stream_is_byte_identical_and_request_forwarded() {
    let seen_auth = Arc::new(Mutex::new(None));
    let seen_body = Arc::new(Mutex::new(None));
    let auth = seen_auth.clone();
    let body_seen = seen_body.clone();
    let mock = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, req_body: Bytes| {
            let auth = auth.clone();
            let body_seen = body_seen.clone();
            async move {
                *auth.lock().unwrap() = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                *body_seen.lock().unwrap() = Some(req_body.to_vec());
                (
                    StatusCode::OK,
                    [("content-type", "text/event-stream")],
                    STREAM.to_vec(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

    let gw = spawn_gateway(mock_addr, pennyrouter_gateway::Timeouts::default()).await;
    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(STREAM_REQUEST)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    assert_eq!(res.bytes().await.unwrap().as_ref(), STREAM);

    assert_eq!(seen_auth.lock().unwrap().as_deref(), Some(API_KEY));
    assert_eq!(
        seen_body.lock().unwrap().as_deref(),
        Some(STREAM_REQUEST.as_bytes())
    );
}

#[tokio::test]
async fn sse_first_chunk_arrives_before_upstream_finishes() {
    // A buffering gateway would deliver nothing until the 3s upstream stall
    // ends; a streaming one delivers the first chunk in milliseconds.
    let first = "data: {\"delta\":\"one\"}\n\n";
    let rest = "data: {\"delta\":\"two\"}\n\ndata: [DONE]\n\n";
    let mock = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let (first, rest) = (first, rest);
            async move {
                let stream = futures_util::stream::unfold(0, move |step| {
                    let (first, rest) = (first, rest);
                    async move {
                        match step {
                            0 => Some((Ok::<_, std::convert::Infallible>(first), 1)),
                            1 => {
                                tokio::time::sleep(Duration::from_secs(3)).await;
                                Some((Ok(rest), 2))
                            }
                            _ => None,
                        }
                    }
                });
                (
                    StatusCode::OK,
                    [("content-type", "text/event-stream")],
                    Body::from_stream(stream),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

    let gw = spawn_gateway(mock_addr, pennyrouter_gateway::Timeouts::default()).await;
    let mut res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .body(STREAM_REQUEST)
        .send()
        .await
        .unwrap();
    let first_chunk = tokio::time::timeout(Duration::from_millis(1500), res.chunk())
        .await
        .expect("first chunk arrives well before the 3s upstream stall ends")
        .unwrap()
        .unwrap();
    assert!(
        first_chunk.starts_with(b"data: "),
        "stream starts with an SSE event, got {first_chunk:?}"
    );
    let mut received = first_chunk.to_vec();
    while let Some(chunk) = res.chunk().await.unwrap() {
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, format!("{first}{rest}").into_bytes());
}

#[tokio::test]
async fn idle_stall_ends_client_stream_with_prefix_intact() {
    let prefix = "data: {\"delta\":\"one\"}\n\n";
    let mock = Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            let stream = futures_util::stream::unfold(0, move |step| async move {
                match step {
                    0 => Some((Ok::<_, std::convert::Infallible>(prefix), 1)),
                    // Stall far past the test's idle budget.
                    _ => {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        None
                    }
                }
            });
            (
                StatusCode::OK,
                [("content-type", "text/event-stream")],
                Body::from_stream(stream),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

    let timeouts = pennyrouter_gateway::Timeouts {
        idle: Duration::from_millis(200),
        ..pennyrouter_gateway::Timeouts::default()
    };
    let gw = spawn_gateway(mock_addr, timeouts).await;
    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .body(STREAM_REQUEST)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = tokio::time::timeout(Duration::from_secs(5), res.bytes())
        .await
        .expect("stalled stream terminates promptly")
        .unwrap();
    // Iter 11 (F06): the prefix arrives intact, but truncation no longer
    // masquerades as clean EOF — a terminal error event closes the body.
    let body = std::str::from_utf8(body.as_ref()).unwrap();
    assert!(body.starts_with(prefix), "prefix intact, got {body:?}");
    assert!(
        body.ends_with("event: error\ndata: {\"error\":{\"message\":\"upstream stream interrupted\",\"type\":\"stream_error\",\"code\":\"upstream_interrupted\"}}\n\n"),
        "terminal error event present, got {body:?}"
    );
}
