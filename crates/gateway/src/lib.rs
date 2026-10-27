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
}

impl AppState {
    pub fn new(upstream_base_url: impl Into<String>) -> Self {
        let base = upstream_base_url.into();
        Self {
            upstream_base_url: base.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
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

/// Non-streaming passthrough (Iter 1b): forward the request untouched and
/// return the upstream status + body byte-for-byte. No routing or DB reads.
async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
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
    let upstream = match state
        .client
        .post(url)
        .headers(forwarded)
        .body(body)
        .send()
        .await
    {
        Ok(res) => res,
        Err(_) => return bad_gateway(),
    };
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream.headers().get(CONTENT_TYPE).cloned();
    let bytes = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => return bad_gateway(),
    };
    let mut response = Response::builder().status(status);
    if let Some(content_type) = content_type {
        response = response.header(CONTENT_TYPE, content_type);
    }
    response
        .body(Body::from(bytes))
        .unwrap_or_else(|_| bad_gateway())
}

/// Minimal failure response. Iter 2 adds timeouts and full upstream error
/// mapping; until then fail closed with a generic OpenAI-shaped 502.
fn bad_gateway() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({
            "error": {
                "message": "upstream request failed",
                "type": "gateway_error",
                "code": "bad_gateway",
            }
        })),
    )
        .into_response()
}
