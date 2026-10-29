//! Iter 11 hardening: upload deadlines, header preservation, upstream
//! body bounds, chat query forwarding, redirect passthrough.

use axum::{
    body::Bytes,
    extract::Request,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

const API_KEY: &str = "Bearer [REDACTED]";

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

fn chat_body() -> &'static str {
    r#"{"model": "test-model", "messages": []}"#
}

/// F03: compressed bytes pass through with their encoding intact, and
/// end-to-end headers (here Retry-After on an error) survive.
#[tokio::test]
async fn gzip_and_error_headers_survive() {
    // Gzip for {"ok":true}: precomputed so the test needs no flate2.
    let gzip_body: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xab, 0x56, 0x4a, 0xcb, 0xcf,
        0x57, 0xb2, 0x52, 0x28, 0x2e, 0x29, 0xca, 0xcf, 0x4b, 0xb1, 0x51, 0xaa, 0x05, 0x00, 0x20,
        0x97, 0xdf, 0xc2, 0x0b, 0x00, 0x00, 0x00,
    ];
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            (
                StatusCode::OK,
                [
                    ("content-type", "application/json"),
                    ("content-encoding", "gzip"),
                ],
                gzip_body.to_vec(),
            )
        }),
    ))
    .await;
    let gw = spawn_gateway(mock, pennyrouter_gateway::Timeouts::default()).await;

    let res = client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-encoding").unwrap(),
        "gzip",
        "encoding metadata must match the untouched bytes",
    );
    assert_eq!(res.bytes().await.unwrap().as_ref(), gzip_body);

    // Error path: OpenAI-shaped 429 keeps its Retry-After.
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("content-type", "application/json"), ("retry-after", "7")],
                r#"{"error":{"message":"slow down","type":"server_error","code":"rate_limited"}}"#,
            )
        }),
    ))
    .await;
    let gw = spawn_gateway(mock, pennyrouter_gateway::Timeouts::default()).await;
    let res = client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res.headers().get("retry-after").unwrap(), "7");
}

/// Hop-by-hop and proxy credentials never travel upstream.
#[tokio::test]
async fn hop_by_hop_headers_stripped_upstream() {
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap| async move {
            let mut guard = capture.lock().unwrap();
            for (name, value) in headers.iter() {
                guard.push((name.to_string(), value.to_str().unwrap().to_string()));
            }
            (StatusCode::OK, "{}")
        }),
    ))
    .await;
    let gw = spawn_gateway(mock, pennyrouter_gateway::Timeouts::default()).await;

    let res = client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .header("connection", "keep-alive")
        .header("proxy-authorization", "Bearer [REDACTED]")
        .body(chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let guard = seen.lock().unwrap();
    assert!(
        guard.iter().all(|(name, _)| name != "proxy-authorization"),
        "proxy credentials must not travel upstream: {guard:?}",
    );
    assert!(
        guard.iter().any(|(name, _)| name == "authorization"),
        "real auth still forwarded: {guard:?}",
    );
}

/// Redirects pass through with Location instead of being followed with
/// the caller's credentials.
#[tokio::test]
async fn redirect_not_followed() {
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            (
                StatusCode::FOUND,
                [("location", "https://elsewhere.example/next")],
                "moved",
            )
        }),
    ))
    .await;
    let gw = spawn_gateway(mock, pennyrouter_gateway::Timeouts::default()).await;

    // The test client must not follow redirects either: the assertion
    // is that the gateway passes the 302 through untouched.
    let res = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(
        res.headers().get("location").unwrap(),
        "https://elsewhere.example/next",
    );
}

/// F05: a huge buffered upstream body maps to 502 instead of unbounded
/// memory (17 MiB over the 16 MiB bound).
#[tokio::test]
async fn oversize_upstream_body_bounded() {
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            let big = vec![b'x'; 17 * 1024 * 1024];
            (StatusCode::OK, [("content-type", "application/json")], big)
        }),
    ))
    .await;
    let gw = spawn_gateway(mock, pennyrouter_gateway::Timeouts::default()).await;

    let res = client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(
        body.pointer("/error/code").and_then(|code| code.as_str()),
        Some("upstream_too_large"),
        "got {body}",
    );
}

/// F02: a stalled chunked upload hits the upload deadline (408) and
/// never reaches upstream. A raw socket writes one chunk, then holds
/// the connection open in silence past both upload deadlines.
#[tokio::test]
async fn stalled_upload_times_out_before_backend() {
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    let timeouts = pennyrouter_gateway::Timeouts {
        upload_idle: Duration::from_millis(100),
        upload_total: Duration::from_millis(300),
        ..pennyrouter_gateway::Timeouts::default()
    };
    let gw = spawn_gateway(mock, timeouts).await;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let probe = (|| -> Result<(), String> {
            let mut sock = std::net::TcpStream::connect(gw).map_err(|e| format!("connect: {e}"))?;
            sock.set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| format!("read timeout: {e}"))?;
            sock.write_all(
                b"POST /v1/chat/completions HTTP/1.1\r\nhost: x\r\nauthorization: Bearer test-key\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n1\r\n{\r\n",
            )
            .map_err(|e| format!("write: {e}"))?;
            // Silence past the upload deadlines; the gateway must answer
            // 408 on the half-open connection.
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match sock.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => raw.extend_from_slice(&chunk[..n]),
                    Err(e) => return Err(format!("read: {e}")),
                }
                if raw.len() > 65536 {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&raw).into_owned();
            if !text.starts_with("HTTP/1.1 408") {
                return Err(format!("expected 408, got {text:?}"));
            }
            if !text.contains("upload_timeout") {
                return Err(format!("expected upload_timeout code, got {text:?}"));
            }
            Ok(())
        })();
        let _ = tx.send(probe);
    });
    let probe = tokio::task::spawn_blocking(move || rx.recv().unwrap())
        .await
        .unwrap();
    assert!(probe.is_ok(), "{probe:?}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "stalled upload must never reach upstream",
    );
}

/// A01: chat keeps its query string upstream.
#[tokio::test]
async fn chat_query_forwarded_upstream() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    let mock = spawn_mock(Router::new().route(
        "/v1/chat/completions",
        post(move |request: Request| async move {
            capture.lock().unwrap().push(
                request
                    .uri()
                    .path_and_query()
                    .map(|pq| pq.as_str().to_string())
                    .unwrap_or_default(),
            );
            (StatusCode::OK, "{}")
        }),
    ))
    .await;
    let gw = spawn_gateway(mock, pennyrouter_gateway::Timeouts::default()).await;

    let res = client()
        .post(format!("http://{gw}/v1/chat/completions?foo=bar&x=1"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(chat_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["/v1/chat/completions?foo=bar&x=1".to_string()]
    );
}
