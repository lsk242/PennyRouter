use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE, HOST},
        HeaderMap, StatusCode,
    },
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::Serialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

mod metrics;
pub mod routing;

pub use metrics::{Metrics, Route, UpstreamErrorKind};

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

/// Shared gateway state: where chat completions are forwarded upstream,
// plus shared metrics and the optional provider router.
#[derive(Clone)]
pub struct AppState {
    upstream_base_url: String,
    client: reqwest::Client,
    timeouts: Timeouts,
    metrics: Arc<Metrics>,
    router: Option<Arc<routing::Router>>,
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
            metrics: Arc::new(Metrics::new()),
            router: None,
        }
    }

    /// State with provider routing: recognized models are rewritten to
    /// the stripped model id with a forced provider.
    pub fn with_router(mut self, router: Arc<routing::Router>) -> Self {
        self.router = Some(router);
        self
    }

    /// State for the real server: upstream from `OPENROUTER_BASE_URL`,
    /// defaulting to OpenRouter; routing from `CATALOG_DB` when set and
    /// loadable, otherwise plain passthrough.
    pub fn from_env() -> Self {
        let base = std::env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api".to_string());
        let state = Self::new(base);
        match std::env::var("CATALOG_DB") {
            Err(_) => state,
            Ok(path) => match routing::Router::load(&path) {
                Ok(router) => state.with_router(Arc::new(router)),
                Err(e) => {
                    eprintln!("gateway: routing disabled ({e}), passing through");
                    state
                }
            },
        }
    }
}

pub fn app() -> Router {
    app_with_state(AppState::from_env())
}

pub fn app_with_state(state: AppState) -> Router {
    let metrics = state.metrics.clone();
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/v1/chat/completions", post(chat_completions))
        .layer(middleware::from_fn_with_state(metrics, track))
        .with_state(state)
}

/// Observability middleware (Iter 3): request ID, latency histogram,
/// per-status counts, and one JSON log line per request.
async fn track(State(metrics): State<Arc<Metrics>>, request: Request, next: Next) -> Response {
    let route = Route::from_path(request.uri().path());
    let method = request.method().to_string();
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{:x}", metrics.next_request_id()));
    let _in_flight = metrics.in_flight_guard();
    let start = Instant::now();
    let mut response = next.run(request).await;
    let latency = start.elapsed();
    let status = response.status().as_u16();
    metrics.observe(route, status, latency);
    if let Ok(value) = request_id.parse() {
        response.headers_mut().insert("x-request-id", value);
    }
    println!(
        "{}",
        serde_json::json!({
            "request_id": request_id,
            "method": method,
            "route": route.label(),
            "status": status,
            "latency_ms": (latency.as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0,
            "outcome": outcome(status),
        })
    );
    response
}

fn outcome(status: u16) -> &'static str {
    match status {
        200..=299 => "ok",
        413 => "guard_rejection",
        502 => "upstream_error",
        504 => "upstream_timeout",
        400..=499 => "client_error",
        _ => "error",
    }
}

async fn metrics_handler(State(state): State<AppState>) -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        state.metrics.render(),
    )
        .into_response()
}

/// Chat completions: guard, optionally route (stripped model + forced
/// provider), then forward; streams pass chunk-by-chunk, buffered
/// responses byte-for-byte.
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
                state.metrics.guard_rejected();
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

    // Routing rewrites recognized requests; anything unrecognized (or no
    // router configured) forwards untouched.
    let (forwarded_body, provider) = match &state.router {
        Some(router) => match router.route_request(&headers, &buffered) {
            Some((rewritten, decision)) => (rewritten, Some(decision.provider)),
            None => (buffered, None),
        },
        None => (buffered, None),
    };
    let mut response = forward_chat_completions(&state, &headers, forwarded_body).await;
    if let Some(provider) = provider {
        if let Ok(value) = provider.parse() {
            response
                .headers_mut()
                .insert(routing::PROVIDER_HEADER, value);
        }
    }
    response
}

/// Forward one buffered body upstream and map the response.
async fn forward_chat_completions(
    state: &AppState,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> Response {
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
        state.client.post(url).headers(forwarded).body(body).send(),
    )
    .await
    {
        Ok(Ok(res)) => res,
        Ok(Err(_)) => {
            state.metrics.upstream_error(UpstreamErrorKind::Transport);
            return bad_gateway();
        }
        Err(_) => {
            state.metrics.upstream_error(UpstreamErrorKind::Timeout);
            return gateway_timeout();
        }
    };
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !status.is_success() {
        return upstream_error(status, upstream, state.timeouts.total, &state.metrics).await;
    }
    let is_stream = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("text/event-stream"));
    if is_stream {
        return stream_response(status, upstream, state.timeouts.idle, state.metrics.clone());
    }
    let content_type = upstream.headers().get(CONTENT_TYPE).cloned();
    let bytes = match tokio::time::timeout(state.timeouts.total, upstream.bytes()).await {
        Ok(Ok(bytes)) => bytes,
        _ => {
            state.metrics.upstream_error(UpstreamErrorKind::Timeout);
            return gateway_timeout();
        }
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
fn stream_response(
    status: StatusCode,
    upstream: reqwest::Response,
    idle: Duration,
    metrics: Arc<Metrics>,
) -> Response {
    let stream =
        futures_util::stream::unfold((upstream, metrics), move |(mut res, metrics)| async move {
            match tokio::time::timeout(idle, res.chunk()).await {
                Ok(Ok(Some(bytes))) => Some((Ok::<Bytes, reqwest::Error>(bytes), (res, metrics))),
                Err(_) => {
                    metrics.upstream_error(UpstreamErrorKind::Timeout);
                    None
                }
                Ok(Err(_)) => {
                    metrics.upstream_error(UpstreamErrorKind::Transport);
                    None
                }
                Ok(Ok(None)) => None,
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
    metrics: &Metrics,
) -> Response {
    metrics.upstream_error(if status.is_client_error() {
        UpstreamErrorKind::Status4xx
    } else {
        UpstreamErrorKind::Status5xx
    });
    let bytes = match tokio::time::timeout(total, upstream.bytes()).await {
        Ok(Ok(bytes)) => bytes,
        _ => {
            metrics.upstream_error(UpstreamErrorKind::Timeout);
            return gateway_timeout();
        }
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
