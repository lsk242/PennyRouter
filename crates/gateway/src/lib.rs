use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE, HOST},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::Serialize;
use std::time::Duration;

/// Token guard (Iter 2): requests estimating over 1M tokens are rejected
/// before touching the backend. The estimate is bytes/4, the standard rough
/// heuristic — generous enough for JSON overhead, conservative enough to
/// bound memory. Precise token accounting is a later iteration.
const MAX_TOKENS: usize = 1_000_000;
const BYTES_PER_TOKEN: usize = 4;
const MAX_REQUEST_BYTES: usize = MAX_TOKENS * BYTES_PER_TOKEN;

/// Upstream error bodies are wrapped with at most this much upstream text.
const ERROR_SNIPPET_CHARS: usize = 200;

/// Upstream time budgets. Buffered (non-streaming) responses are bounded by
/// `total`; streams are bounded per chunk by `idle`, so legitimately long
/// generations can run as long as chunks keep flowing.
#[derive(Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    pub first_chunk: Duration,
    pub idle: Duration,
    pub total: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            first_chunk: Duration::from_secs(30),
            idle: Duration::from_secs(60),
            total: Duration::from_secs(300),
        }
    }
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Shared gateway state: where chat completions are forwarded upstream.
#[derive(Clone)]
pub struct AppState {
    upstream_base_url: String,
    client: reqwest::Client,
    timeouts: Timeouts,
}

impl AppState {
    pub fn new(upstream_base_url: impl Into<String>) -> Self {
        Self::with_timeouts(upstream_base_url, Timeouts::default())
    }

    pub fn with_timeouts(upstream_base_url: impl Into<String>, timeouts: Timeouts) -> Self {
        let base = upstream_base_url.into();
        let client = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            upstream_base_url: base.trim_end_matches('/').to_string(),
            client,
            timeouts,
        }
    }

    /// State for the real server: upstream from `OPENROUTER_BASE_URL`,
    /// defaulting to OpenRouter.
    pub fn from_env() -> Self {
        let base = std::env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai".to_string());
        Self::new(base)
    }
}

pub fn app() -> Router {
    app_with_state(AppState::from_env())
}

pub fn app_with_state(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state)
}

/// Chat completions passthrough: forward the request untouched, then either
/// stream an SSE response through chunk-by-chunk or return a buffered
/// response byte-for-byte. No routing or DB reads.
async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // Token guard first: count incrementally so oversize work is rejected
    // before anything is sent upstream.
    let buffered = {
        use futures_util::StreamExt;
        let mut stream = Box::pin(body.into_data_stream());
        let mut buffered = Vec::new();
        while let Some(result) = stream.next().await {
            let data = match result {
                Ok(data) => data,
                Err(_) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        "read_error",
                        "failed to read request body".to_string(),
                    );
                }
            };
            if buffered.len() + data.len() > MAX_REQUEST_BYTES {
                return error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "invalid_request_error",
                    "payload_too_large",
                    format!(
                        "request body exceeds the {MAX_TOKENS}-token limit \
                         (~{} MiB at {BYTES_PER_TOKEN} bytes/token)",
                        MAX_REQUEST_BYTES / (1024 * 1024),
                    ),
                );
            }
            buffered.extend_from_slice(&data);
        }
        buffered
    };

    let url = format!("{}/v1/chat/completions", state.upstream_base_url);
    let mut forwarded = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        // The client sets host and content-length itself.
        if *name == HOST || *name == CONTENT_LENGTH {
            continue;
        }
        forwarded.append(name.clone(), value.clone());
    }
    let upstream = match tokio::time::timeout(
        state.timeouts.first_chunk,
        state
            .client
            .post(url)
            .headers(forwarded)
            .body(buffered)
            .send(),
    )
    .await
    {
        Ok(Ok(res)) => res,
        Ok(Err(_)) => return bad_gateway(),
        Err(_) => return gateway_timeout(),
    };
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !status.is_success() {
        return upstream_error(status, upstream, state.timeouts.total).await;
    }
    let is_stream = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("text/event-stream"));
    if is_stream {
        return stream_response(status, upstream, state.timeouts.idle);
    }
    let content_type = upstream.headers().get(CONTENT_TYPE).cloned();
    let bytes = match tokio::time::timeout(state.timeouts.total, upstream.bytes()).await {
        Ok(Ok(bytes)) => bytes,
        _ => return gateway_timeout(),
    };
    let mut response = Response::builder().status(status);
    if let Some(content_type) = content_type {
        response = response.header(CONTENT_TYPE, content_type);
    }
    response
        .body(Body::from(bytes))
        .unwrap_or_else(|_| bad_gateway())
}

/// SSE passthrough: forward each chunk as it arrives, preserving bytes and
/// order. A stalled or broken stream ends the client stream — the status was
/// already sent, so there is nothing left to map it to.
fn stream_response(status: StatusCode, upstream: reqwest::Response, idle: Duration) -> Response {
    let stream = futures_util::stream::unfold(upstream, move |mut res| async move {
        match tokio::time::timeout(idle, res.chunk()).await {
            Ok(Ok(Some(bytes))) => Some((Ok::<Bytes, reqwest::Error>(bytes), res)),
            _ => None,
        }
    });
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| bad_gateway())
}

/// Upstream error mapping: OpenAI-shaped JSON errors pass through untouched;
/// anything else is wrapped in an OpenAI-shaped error with the same status.
async fn upstream_error(
    status: StatusCode,
    upstream: reqwest::Response,
    total: Duration,
) -> Response {
    let bytes = match tokio::time::timeout(total, upstream.bytes()).await {
        Ok(Ok(bytes)) => bytes,
        _ => return gateway_timeout(),
    };
    if serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|json| json.get("error").cloned())
        .is_some()
    {
        return Response::builder()
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| bad_gateway());
    }
    let kind = if status.is_client_error() {
        "invalid_request_error"
    } else {
        "server_error"
    };
    error_response(
        status,
        kind,
        "upstream_error",
        format!(
            "upstream error (http {}): {}",
            status.as_u16(),
            snippet(&bytes)
        ),
    )
}

fn snippet(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out: String = text.chars().take(ERROR_SNIPPET_CHARS).collect();
    if text.chars().count() > ERROR_SNIPPET_CHARS {
        out.push('…');
    }
    out
}

fn bad_gateway() -> Response {
    error_response(
        StatusCode::BAD_GATEWAY,
        "gateway_error",
        "bad_gateway",
        "upstream request failed".to_string(),
    )
}

fn gateway_timeout() -> Response {
    error_response(
        StatusCode::GATEWAY_TIMEOUT,
        "gateway_error",
        "gateway_timeout",
        "upstream timed out".to_string(),
    )
}

fn error_response(status: StatusCode, kind: &str, code: &str, message: String) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": message,
                "type": kind,
                "code": code,
            }
        })),
    )
        .into_response()
}
