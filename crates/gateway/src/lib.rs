use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{
        header::{CONTENT_TYPE, LOCATION},
        HeaderMap, Method, StatusCode,
    },
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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

/// Concurrency bounds (Iter 9): at most `max_upstream` requests hold an
/// upstream slot at once; overflow waits on `max_queued` queue slots, and
/// anything beyond sheds with 503 instead of piling up unboundedly.
const DEFAULT_MAX_UPSTREAM: usize = 64;
const DEFAULT_MAX_QUEUED: usize = 128;

/// Catalog reload cadence (Iter 10): how often the gateway re-resolves
/// `CATALOG_DB` and atomically publishes the new snapshot. Zero disables
/// the background reload (load-once behavior).
const DEFAULT_RELOAD_INTERVAL_SECS: u64 = 60;

/// Upstream time budgets. Buffered (non-streaming) responses are bounded by
/// `total`; streams are bounded per chunk by `idle`, so legitimately long
/// generations can run as long as chunks keep flowing. Uploads are bounded
/// per chunk-read by `upload_idle` and overall by `upload_total` (Iter 11,
/// F02), so incomplete below-cap uploads cannot be retained indefinitely.
#[derive(Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    pub first_chunk: Duration,
    pub idle: Duration,
    pub total: Duration,
    pub upload_idle: Duration,
    pub upload_total: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            first_chunk: Duration::from_secs(30),
            idle: Duration::from_secs(60),
            total: Duration::from_secs(300),
            upload_idle: Duration::from_secs(30),
            upload_total: Duration::from_secs(60),
        }
    }
}

/// Upstream response bodies are collected only up to this size (Iter 11,
/// F05). Streams pass chunk-by-chunk and need no bound; anything that
/// must be buffered is capped here instead of growing without limit.
const MAX_UPSTREAM_BYTES: usize = 16 * 1024 * 1024;

/// Dropped log lines (Iter 12): rendered by metrics; `pub(crate)` so
/// only the metrics renderer reads it.
pub(crate) static LOG_DROPPED: AtomicU64 = AtomicU64::new(0);

/// Buffered request logging (Iter 12): lines travel a bounded channel
/// to a single writer thread instead of one syscall per request. A
/// full channel drops lines (counted in
/// `pennyrouter_log_dropped_total`) rather than blocking the request
/// path; the writer flushes at least once a second.
const LOG_CHANNEL: usize = 1024;

static LOG_SENDER: OnceLock<std::sync::mpsc::SyncSender<String>> = OnceLock::new();

fn log_line(line: String) {
    let sender = LOG_SENDER.get_or_init(spawn_log_writer);
    if sender.try_send(line).is_err() {
        LOG_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

fn spawn_log_writer() -> std::sync::mpsc::SyncSender<String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(LOG_CHANNEL);
    std::thread::Builder::new()
        .name("pennyrouter-log".to_string())
        .spawn(move || {
            // The stdout lock is taken per batch and released: holding it
            // for the thread's lifetime would starve every other stdout
            // writer in the process (including test harnesses).
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(line) => {
                        let mut batch = line;
                        batch.push('\n');
                        while let Ok(line) = rx.try_recv() {
                            batch.push_str(&line);
                            batch.push('\n');
                            if batch.len() >= 64 * 1024 {
                                break;
                            }
                        }
                        let _ = std::io::stdout().lock().write_all(batch.as_bytes());
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("spawn log writer");
    tx
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// Concurrency bounds (Iter 9). `max_upstream` caps simultaneous
/// upstream calls; `max_queued` caps how many requests wait for a slot.
/// Anything beyond sheds with 503.
#[derive(Clone, Copy, Debug)]
pub struct ConcurrencyLimits {
    pub max_upstream: usize,
    pub max_queued: usize,
}

impl Default for ConcurrencyLimits {
    fn default() -> Self {
        Self {
            max_upstream: DEFAULT_MAX_UPSTREAM,
            max_queued: DEFAULT_MAX_QUEUED,
        }
    }
}

/// Shared gateway state: where chat completions are forwarded upstream,
// plus shared metrics and the optional provider router.
#[derive(Clone)]
pub struct AppState {
    upstream_base_url: String,
    extra_upstreams: HashMap<String, String>,
    client: reqwest::Client,
    timeouts: Timeouts,
    metrics: Arc<Metrics>,
    router: Option<Arc<routing::Router>>,
    upstream_slots: Arc<Semaphore>,
    queue_slots: Arc<Semaphore>,
}

impl AppState {
    pub fn new(upstream_base_url: impl Into<String>) -> Self {
        Self::with_timeouts(upstream_base_url, Timeouts::default())
    }

    pub fn with_timeouts(upstream_base_url: impl Into<String>, timeouts: Timeouts) -> Self {
        Self::with_limits(upstream_base_url, timeouts, ConcurrencyLimits::default())
    }

    pub fn with_limits(
        upstream_base_url: impl Into<String>,
        timeouts: Timeouts,
        limits: ConcurrencyLimits,
    ) -> Self {
        let base = upstream_base_url.into();
        let client = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            // The gateway is a reverse proxy: redirects pass through to
            // the client (with Location preserved) instead of being
            // followed with the caller's credentials (Iter 11).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        // An upstream bound of zero would deadlock every request; a queue
        // bound of zero (no waiting room) is a legitimate configuration.
        let max_upstream = limits.max_upstream.max(1);
        Self {
            upstream_base_url: base.trim_end_matches('/').to_string(),
            extra_upstreams: HashMap::new(),
            client,
            timeouts,
            metrics: Arc::new(Metrics::new()),
            router: None,
            upstream_slots: Arc::new(Semaphore::new(max_upstream)),
            queue_slots: Arc::new(Semaphore::new(limits.max_queued)),
        }
    }

    /// Register a provider-pinned upstream (Iter 16): `modal/<model>`
    /// and `baseten/<model>` forward there instead of OpenRouter.
    ///
    /// Bases carry no route path (like the OpenRouter base): the gateway
    /// appends the full incoming `/v1/…` path itself, so a trailing
    /// `/v1` is stripped — otherwise requests double it (`/v1/v1/…`)
    /// and the upstream 404s.
    pub fn with_upstream(mut self, provider: &str, base_url: &str) -> Self {
        self.extra_upstreams
            .insert(provider.to_string(), normalize_pinned_base(base_url));
        self
    }

    /// Base URL for a pinned provider, if configured.
    pub fn pinned_upstream(&self, provider: &str) -> Option<&str> {
        self.extra_upstreams.get(provider).map(String::as_str)
    }

    /// Default (OpenRouter) upstream base URL (Iter 17: landing page).
    pub fn upstream_base(&self) -> &str {
        &self.upstream_base_url
    }

    /// Configured provider-pinned upstreams, sorted (Iter 17).
    pub fn extra_upstream_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.extra_upstreams.keys().cloned().collect();
        names.sort();
        names
    }

    /// State with provider routing: recognized models are rewritten to
    /// the stripped model id with a forced provider.
    pub fn with_router(mut self, router: Arc<routing::Router>) -> Self {
        self.router = Some(router);
        self
    }

    /// Router handle for the background reload task (Iter 10).
    pub fn router_handle(&self) -> Option<Arc<routing::Router>> {
        self.router.clone()
    }

    /// Metrics handle for the background reload task (Iter 10).
    pub fn metrics_handle(&self) -> Arc<Metrics> {
        self.metrics.clone()
    }

    /// State for the real server: upstream from `OPENROUTER_BASE_URL`,
    /// defaulting to OpenRouter; routing from `CATALOG_DB` when set and
    /// loadable, otherwise plain passthrough. Concurrency bounds from
    /// `UPSTREAM_MAX_CONCURRENT` / `UPSTREAM_MAX_QUEUED`, defaulting to
    /// the compiled defaults on missing or unparsable values.
    pub fn from_env() -> Self {
        let base = std::env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api".to_string());
        let limits = ConcurrencyLimits {
            max_upstream: std::env::var("UPSTREAM_MAX_CONCURRENT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_MAX_UPSTREAM),
            max_queued: std::env::var("UPSTREAM_MAX_QUEUED")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_MAX_QUEUED),
        };
        let mut state = Self::with_limits(base, Timeouts::default(), limits);
        // Provider-pinned upstreams (Iter 16): unset means the
        // provider's routes fail closed with provider_not_configured.
        // Baseten defaults to its public inference host (same host the
        // catalog judge uses, without the judge's `/v1` route prefix —
        // the gateway appends the incoming `/v1/…` path itself; an
        // explicitly configured trailing `/v1` is stripped at
        // registration). Modal has no shared deployment, so it stays
        // unset until configured.
        if let Ok(url) = std::env::var("MODAL_BASE_URL") {
            if !url.is_empty() {
                state = state.with_upstream("modal", &url);
            }
        }
        if let Ok(url) = std::env::var("BASETEN_BASE_URL") {
            if !url.is_empty() {
                state = state.with_upstream("baseten", &url);
            }
        } else {
            state = state.with_upstream("baseten", "https://inference.baseten.co");
        }
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

/// Normalize a pinned-provider base URL: strip trailing slashes and
/// any trailing `/v1` segments. The gateway appends the full incoming
/// `/v1/…` path itself, so a base that already ends in `/v1` would
/// double it (`/v1/v1/…`) and 404 upstream. Idempotent; other paths
/// (including `/v10`) pass through untouched.
fn normalize_pinned_base(base_url: &str) -> String {
    let mut base = base_url.trim_end_matches('/').to_string();
    while let Some(stripped) = base.strip_suffix("/v1") {
        base = stripped.trim_end_matches('/').to_string();
    }
    base
}

pub fn app() -> Router {
    app_with_state(AppState::from_env())
}

/// Reload cadence from `RELOAD_INTERVAL_SECS` (default 60s, 0 disables).
pub fn reload_interval_from_env() -> Duration {
    Duration::from_secs(
        std::env::var("RELOAD_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_RELOAD_INTERVAL_SECS),
    )
}

/// Periodic catalog refresh (Iter 10): re-resolve the router source and
/// atomically publish the new snapshot, keeping the last good one on
/// failure. Runs until the handle is aborted (e.g. at shutdown).
pub fn spawn_reload_task(
    router: Arc<routing::Router>,
    interval: Duration,
    metrics: Arc<Metrics>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // A slow or failed reload never delays the next scheduled one.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            match router.reload() {
                Ok(()) => metrics.reload_report(true),
                Err(e) => {
                    metrics.reload_report(false);
                    eprintln!("gateway: catalog reload failed ({e}), keeping last good snapshot");
                }
            }
        }
    })
}

/// Binary version and build hash for the landing page (Iter 17).
/// The hash comes from `build.rs` (`git rev-parse --short HEAD`) and
/// degrades to `unknown` in checkouts without git metadata.
pub fn build_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn build_hash() -> &'static str {
    option_env!("GIT_HASH").unwrap_or("unknown")
}

/// Debug UI flag (Iter 17): `GATEWAY_UI=1` (also `true`/`yes`)
/// registers `GET /ui`. Anything else — including unset — leaves the
/// path unregistered: no handler, no middleware branch, zero
/// request-path cost.
fn ui_enabled() -> bool {
    std::env::var("GATEWAY_UI")
        .map(|value| matches!(value.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub fn app_with_state(state: AppState) -> Router {
    let metrics = state.metrics.clone();
    let router = Router::new()
        .route("/", get(landing))
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/v1/chat/completions", post(routed_completion))
        // Responses bodies carry the same `model` field, so they get
        // the same routing, budget, and floor policy (Iter 14).
        .route("/v1/responses", post(routed_completion))
        // Curated menu (Iter 13): clients discover exactly the virtual
        // models; this path used to proxy upstream.
        .route("/v1/models", get(models_list));
    // Debug UI (Iter 17): registered only when enabled. Disabled
    // means no route at all — the path falls through to the
    // fallback proxy like any unknown path.
    let router = if ui_enabled() {
        router.route("/ui", get(ui_page))
    } else {
        router
    };
    router
        .fallback(fallback_proxy)
        .layer(middleware::from_fn_with_state(metrics, track))
        .with_state(state)
}

/// Fixed landing page (Iter 17): binary version + build hash and
/// display/config status with links. Always registered — it answers
/// only its own path, so API traffic is unaffected.
async fn landing(State(state): State<AppState>) -> Response {
    fn esc(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let mut pinned = state.extra_upstream_names();
    if pinned.is_empty() {
        pinned.push("(none)".to_string());
    }
    let ui = if ui_enabled() {
        "<a href=\"/ui\">debug UI</a> (enabled)".to_string()
    } else {
        "debug UI (disabled — set GATEWAY_UI=1)".to_string()
    };
    let page = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
        <title>PennyRouter gateway</title>\
        <style>body{{font-family:system-ui,sans-serif;margin:2em;max-width:72em}}</style>\
        </head><body><h1>PennyRouter gateway</h1>\
        <p>version <strong>{}</strong> (build <strong>{}</strong>)</p>\
        <h2>Config</h2><ul>\
        <li>routing: {}</li>\
        <li>debug UI: {}</li>\
        <li>default upstream: {}</li>\
        <li>pinned upstreams: {}</li></ul>\
        <h2>Links</h2><ul>\
        <li><a href=\"/ui\">/ui</a> ranking + metrics (needs the flag)</li>\
        <li><a href=\"/metrics\">/metrics</a> Prometheus text</li>\
        <li><a href=\"/health\">/health</a> liveness</li>\
        <li><a href=\"/v1/models\">/v1/models</a> virtual menu</li></ul>\
        </body></html>",
        esc(build_version()),
        esc(build_hash()),
        if state.router_handle().is_some() {
            "on (catalog)"
        } else {
            "off (passthrough)"
        },
        ui,
        esc(state.upstream_base()),
        esc(&pinned.join(", ")),
    );
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        page,
    )
        .into_response()
}

/// Debug UI (Iter 17): read-only operator page. Snapshot version
/// and counts, virtual menu, per-model offerings with the ranking's
/// default-orientation provider order, free offers, and the raw
/// metrics text. Rendered on demand from short read locks; nothing
/// here runs on the request path.
async fn ui_page(State(state): State<AppState>) -> Response {
    fn esc(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    fn money(value: Option<f64>) -> String {
        value
            .map(|v| format!("{v:.7}"))
            .unwrap_or_else(|| "-".to_string())
    }
    fn text(value: &Option<String>) -> String {
        value.as_deref().map(esc).unwrap_or_else(|| "-".to_string())
    }
    let mut page = String::from(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
        <title>PennyRouter gateway</title>\
        <style>body{font-family:system-ui,sans-serif;margin:2em;max-width:72em}\
        table{border-collapse:collapse;margin:1em 0}\
        th,td{border:1px solid #999;padding:.3em .6em;text-align:left;font-size:.9em}\
        th{background:#eee}pre{background:#f4f4f4;padding:1em;overflow:auto}</style>\
        </head><body><h1>PennyRouter gateway</h1>",
    );
    match state.router_handle() {
        None => page.push_str("<p>Routing: <strong>off</strong> (no catalog).</p>"),
        Some(router) => {
            let summary = router.ui_summary();
            let models = summary.models.len();
            let offerings: usize = summary.models.iter().map(|(_, o)| o.len()).sum();
            page.push_str(&format!(
                "<p>Snapshot: <strong>{}</strong> — {models} models, {offerings} offerings.</p>",
                summary
                    .artifact_version
                    .as_deref()
                    .map(esc)
                    .unwrap_or_else(|| "unversioned".to_string()),
            ));
            page.push_str("<h2>Virtual menu</h2><ul>");
            for id in routing::VIRTUAL_MODELS {
                page.push_str(&format!("<li>{}</li>", esc(id)));
            }
            page.push_str("</ul><h2>Models</h2>");
            for (model_id, entries) in &summary.models {
                page.push_str(&format!("<h3>{}</h3>", esc(model_id)));
                page.push_str(
                    "<table><tr><th>provider</th><th>tag</th>\
                    <th>prompt $/tok</th><th>completion $/tok</th><th>class</th>\
                    <th>tier</th><th>score</th></tr>",
                );
                for entry in entries {
                    page.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
                        <td>{}</td><td>{}</td><td>{}</td></tr>",
                        esc(&entry.provider),
                        text(&entry.tag),
                        money(entry.price_prompt),
                        money(entry.price_completion),
                        text(&entry.precision_class),
                        text(&entry.quality_tier),
                        entry
                            .quality_score
                            .map(|s| format!("{s:.3}"))
                            .unwrap_or_else(|| "-".to_string()),
                    ));
                }
                page.push_str("</table>");
                if let Some((_, classes)) = summary.ranking.iter().find(|(id, _)| id == model_id) {
                    page.push_str("<p>Ranking order (default): ");
                    let orders: Vec<String> = classes
                        .iter()
                        .map(|rank| {
                            format!(
                                "{}: {}",
                                rank.class.as_deref().unwrap_or("any"),
                                rank.order
                                    .iter()
                                    .map(|name| esc(name))
                                    .collect::<Vec<_>>()
                                    .join(" &gt; ")
                            )
                        })
                        .collect();
                    page.push_str(&orders.join("<br>"));
                    page.push_str("</p>");
                }
            }
            if !summary.free_offers.is_empty() {
                page.push_str("<h2>Free offers</h2><table>\
                    <tr><th>model</th><th>provider</th><th>tag</th><th>valid until (unix)</th></tr>");
                for (model_id, provider, tag, until) in &summary.free_offers {
                    page.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                        esc(model_id),
                        esc(provider),
                        tag.as_deref().map(esc).unwrap_or_else(|| "-".to_string()),
                        until
                            .map(|u| u.to_string())
                            .unwrap_or_else(|| "permanent".to_string()),
                    ));
                }
                page.push_str("</table>");
            }
        }
    }
    page.push_str("<h2>Metrics</h2><pre>");
    page.push_str(&esc(&state.metrics.render()));
    page.push_str("</pre></body></html>");
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        page,
    )
        .into_response()
}

/// Curated virtual-model menu (Iter 13): exactly the six ids, OpenAI
/// list shape, served locally instead of proxied.
async fn models_list() -> Response {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "object": "list",
            "data": crate::routing::VIRTUAL_MODELS
                .iter()
                .map(|id| serde_json::json!({
                    "id": id,
                    "object": "model",
                    "created": now,
                    "owned_by": "pennyrouter",
                }))
                .collect::<Vec<_>>(),
        })),
    )
        .into_response()
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
    // The header-latency guard drops at return; SSE bodies hold their
    // own in-flight slot until the stream ends (Iter 11, A04).
    let _in_flight = metrics.in_flight_guard();
    let start = Instant::now();
    let mut response = next.run(request).await;
    let latency = start.elapsed();
    let status = response.status().as_u16();
    metrics.observe(route, status, latency);
    if let Ok(value) = request_id.parse() {
        response.headers_mut().insert("x-request-id", value);
    }
    log_line(format!(
        "{}",
        serde_json::json!({
            "request_id": request_id,
            "method": method,
            "route": route.label(),
            "status": status,
            "latency_ms": (latency.as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0,
            "outcome": outcome(status),
        })
    ));
    response
}

fn outcome(status: u16) -> &'static str {
    match status {
        200..=299 => "ok",
        413 => "guard_rejection",
        502 => "upstream_error",
        503 => "load_shed",
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

/// Routed completions (`/v1/chat/completions` and `/v1/responses`):
/// admit, guard, optionally route (stripped model + forced provider),
/// then forward; streams pass chunk-by-chunk, buffered responses
/// byte-for-byte.
///
/// Admission happens before body collection (Iter 11, F02), so the
/// concurrency bound covers retained upload buffers too. Unlike the
/// fallback, these endpoints keep their query string (Iter 11, A01).
async fn routed_completion(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    let permit = match acquire_upstream_slot(&state).await {
        Some(permit) => permit,
        None => {
            state.metrics.load_shed();
            return service_unavailable();
        }
    };
    // Token guard first: count incrementally so oversize work is rejected
    // before anything is sent upstream.
    let buffered = match read_capped_body(&state, body).await {
        Ok(buffered) => buffered,
        Err(response) => return *response,
    };

    // Provider-pinned direct routes (Iter 16): `modal/<model>` and
    // `baseten/<model>` bypass the catalog and address that
    // provider's upstream, with or without a router. Pure BYOK: the
    // caller's Authorization header is forwarded unchanged.
    if let Some(model) = raw_model(&buffered) {
        if let Some((provider, _)) = routing::split_pinned(&model) {
            return forward_pinned(&state, permit, &headers, uri, buffered, &model, provider).await;
        }
    }
    // Virtual requests keep a copy for one retry (Iter 13): the permit
    // is per-attempt, the body is reusable.
    let retry_body = is_virtual_request(&buffered).then(|| buffered.clone());
    // Routing rewrites recognized requests; anything unrecognized (or no
    // router configured) forwards untouched. Virtual-model rejections
    // fail closed (Iter 14): a gateway-owned id must never leak upstream,
    // where it would 404 opaquely.
    let (forwarded_body, decision) = match &state.router {
        Some(router) => match router.route_request_or_reject(&headers, &buffered) {
            Ok(Some((rewritten, decision))) => (rewritten, Some(decision)),
            Ok(None) => (buffered, None),
            Err(rejection) => {
                if is_virtual_request(&buffered) {
                    return rejection_response(&rejection);
                }
                (buffered, None)
            }
        },
        None => (buffered, None),
    };
    let path = uri
        .path_and_query()
        .map(|path_and_query| path_and_query.as_str().to_string())
        .unwrap_or_else(|| "/v1/chat/completions".to_string());
    let default_base = state.upstream_base_url.clone();
    let mut response = forward_upstream(
        &state,
        &default_base,
        permit,
        &Method::POST,
        &path,
        &headers,
        forwarded_body,
    )
    .await;
    if let Some(decision) = &decision {
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            // 429 history steers later virtual picks elsewhere.
            if let Some(router) = &state.router {
                router.note_throttled(&decision.model_id, &decision.provider);
            }
            state.metrics.throttled();
        }
    }
    // Availability (Iter 13): one retry on a different provider for
    // virtual requests when the first attempt is retryable. Non-virtual
    // requests keep exactly-once semantics.
    // `retry_body` exists only for virtual requests (see above), so
    // reaching here already implies virtual + retryable.
    if is_retryable(response.status()) {
        if let (Some(router), Some(retry_body), Some(decision)) =
            (&state.router, retry_body, &decision)
        {
            if let Some((rewritten, retry_decision)) =
                router.route_request_retry(&headers, &retry_body, &decision.provider)
            {
                let permit = match acquire_upstream_slot(&state).await {
                    Some(permit) => permit,
                    None => {
                        state.metrics.load_shed();
                        return service_unavailable();
                    }
                };
                // Retries only happen for virtual requests, which
                // resolve inside the OpenRouter catalog: default base.
                let retry_base = state.upstream_base_url.clone();
                response = forward_upstream(
                    &state,
                    &retry_base,
                    permit,
                    &Method::POST,
                    &path,
                    &headers,
                    rewritten,
                )
                .await;
                if let Ok(provider) = retry_decision.provider.parse() {
                    response
                        .headers_mut()
                        .insert(routing::PROVIDER_HEADER, provider);
                }
                return response;
            }
        }
    }
    if let Some(decision) = decision {
        if let Ok(value) = decision.provider.parse() {
            response
                .headers_mut()
                .insert(routing::PROVIDER_HEADER, value);
        }
    }
    response
}

/// True for virtual-model chat bodies (Iter 13), from the raw `model`
/// field — routing may already have rewritten it by the time we ask.
fn is_virtual_request(buffered: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(buffered)
        .ok()
        .and_then(|json| json.get("model")?.as_str().map(str::to_string))
        .and_then(|model| routing::parse_virtual(&model))
        .is_some()
}

/// Retryable upstream outcomes (Iter 13): rate limits, bad gateways,
/// and timeouts. Other statuses (including 500s, which may be
/// deterministic) stand as-is.
fn is_retryable(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

/// Drop-in fallback (Iter 8): any standard endpoint without a custom
/// handler is proxied upstream untouched and logged with its full path,
/// so no client call ever goes silent. No routing or rewriting applies.
async fn fallback_proxy(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map(|path_and_query| path_and_query.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    log_line(format!(
        "{}",
        serde_json::json!({
            "fallback_proxy": true,
            "method": parts.method.to_string(),
            "path": redact_query(&path),
        })
    ));
    let permit = match acquire_upstream_slot(&state).await {
        Some(permit) => permit,
        None => {
            state.metrics.load_shed();
            return service_unavailable();
        }
    };
    let buffered = match read_capped_body(&state, body).await {
        Ok(buffered) => buffered,
        Err(response) => return *response,
    };
    let fallback_base = state.upstream_base_url.clone();
    forward_upstream(
        &state,
        &fallback_base,
        permit,
        &parts.method,
        &path,
        &parts.headers,
        buffered,
    )
    .await
}

/// Log paths without query values (Iter 11): tokens and private
/// identifiers in query strings must not enter logs.
fn redact_query(path: &str) -> String {
    match path.find('?') {
        Some(i) => format!("{}?[redacted]", &path[..i]),
        None => path.to_string(),
    }
}

/// Read a request body up to the token-guard cap, rejecting oversize
/// payloads before anything is sent upstream. Chunk reads are bounded
/// by the upload-idle deadline and the whole read by the upload-total
/// deadline (Iter 11, F02): stalled or trickling uploads fail instead
/// of retaining buffers indefinitely.
async fn read_capped_body(state: &AppState, body: Body) -> Result<Vec<u8>, Box<Response>> {
    use futures_util::StreamExt;
    let read = async {
        let mut stream = Box::pin(body.into_data_stream());
        let mut buffered = Vec::new();
        loop {
            let next = match tokio::time::timeout(state.timeouts.upload_idle, stream.next()).await {
                Ok(next) => next,
                Err(_) => return Err(Box::new(upload_timeout())),
            };
            match next {
                None => return Ok(buffered),
                Some(Err(_)) => {
                    return Err(Box::new(error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        "read_error",
                        "failed to read request body".to_string(),
                    )));
                }
                Some(Ok(data)) => {
                    if buffered.len() + data.len() > MAX_REQUEST_BYTES {
                        state.metrics.guard_rejected();
                        return Err(Box::new(error_response(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "invalid_request_error",
                            "payload_too_large",
                            format!(
                                "request body exceeds the {MAX_TOKENS}-token limit \
                                 (~{} MiB at {BYTES_PER_TOKEN} bytes/token)",
                                MAX_REQUEST_BYTES / (1024 * 1024),
                            ),
                        )));
                    }
                    buffered.extend_from_slice(&data);
                }
            }
        }
    };
    match tokio::time::timeout(state.timeouts.upload_total, read).await {
        Ok(result) => result,
        Err(_) => Err(Box::new(upload_timeout())),
    }
}

fn upload_timeout() -> Response {
    error_response(
        StatusCode::REQUEST_TIMEOUT,
        "invalid_request_error",
        "upload_timeout",
        "request body upload timed out".to_string(),
    )
}

/// One held upstream slot, plus the queue slot when the request had to
/// wait for its turn. Dropping both releases the capacity back.
struct UpstreamPermit {
    _slot: OwnedSemaphorePermit,
    _queued: Option<OwnedSemaphorePermit>,
}

/// Admit one request to the upstream path (Iter 9): the fast path takes a
/// free slot immediately; overflow takes a bounded queue slot and waits
/// for its turn; anything beyond sheds with `None` (caller answers 503).
async fn acquire_upstream_slot(state: &AppState) -> Option<UpstreamPermit> {
    if let Ok(slot) = state.upstream_slots.clone().try_acquire_owned() {
        return Some(UpstreamPermit {
            _slot: slot,
            _queued: None,
        });
    }
    let queued = state.queue_slots.clone().try_acquire_owned().ok()?;
    match state.upstream_slots.clone().acquire_owned().await {
        Ok(slot) => Some(UpstreamPermit {
            _slot: slot,
            _queued: Some(queued),
        }),
        // Unreachable: the semaphore is never closed while state lives.
        Err(_) => None,
    }
}

/// End-to-end response headers preserved on every path (Iter 11,
/// F03): representation and negotiation metadata stay coherent with
/// the bytes (transparent passthrough — success bodies are never
/// decoded), so compressed bodies, ranges, auth challenges,
/// validators, and redirects survive. Everything else (framing,
/// hop-by-hop) is rebuilt by our own stack. Error bodies are the
/// exception: the gateway reads them, so gzipped errors decode
/// first and drop the encoding claim (Iter 20).
const FORWARDED_RESPONSE_HEADERS: [&str; 7] = [
    "content-type",
    "content-encoding",
    "content-range",
    "retry-after",
    "www-authenticate",
    "etag",
    "last-modified",
];

/// Copy the preserved response headers from an upstream response.
fn forwarded_response_headers(upstream: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in FORWARDED_RESPONSE_HEADERS {
        if let Some(value) = upstream.get(name) {
            out.insert(axum::http::HeaderName::from_static(name), value.clone());
        }
    }
    // Redirects pass through with their target intact.
    if let Some(location) = upstream.get(LOCATION) {
        out.insert(LOCATION, location.clone());
    }
    out
}

/// Request headers never forwarded upstream (Iter 11): framing the
/// client stack owns, plus proxy credentials and connection-nominated
/// fields per RFC 9110 §7.6.1.
fn is_hop_by_hop(name: &axum::http::HeaderName, nominated: &[String]) -> bool {
    const FIXED: [&str; 10] = [
        "host",
        "content-length",
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ];
    if FIXED.contains(&name.as_str()) {
        return true;
    }
    nominated.iter().any(|field| field == name.as_str())
}

/// Forward one buffered body upstream with the given method and path,
/// then map the response (streams chunk-by-chunk, buffered bodies
/// byte-for-byte, errors wrapped OpenAI-style). The caller holds an
/// admission permit for the whole call (Iter 11, F02).
/// Raw `model` field of a buffered body, if it parses as JSON with a
/// string model (Iter 16: pinned dispatch needs it before routing).
fn raw_model(buffered: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(buffered)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

/// Unconfigured pinned provider (Iter 16): explicit 400, never a
/// leak to the default upstream (which would 404 opaquely) and never
/// a guess at the provider's URL.
fn provider_not_configured(provider: &str) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request_error",
        "provider_not_configured",
        format!(
            "provider {provider:?} has no upstream configured (set {}_BASE_URL)",
            provider.to_uppercase()
        ),
    )
}

/// Forward a provider-pinned request (Iter 16): stripped model,
/// OpenRouter-specific fields dropped, caller's headers (including
/// Authorization) unchanged, answered from that provider's upstream.
#[allow(clippy::too_many_arguments)]
async fn forward_pinned(
    state: &AppState,
    permit: UpstreamPermit,
    headers: &HeaderMap,
    uri: axum::http::Uri,
    buffered: Vec<u8>,
    model: &str,
    provider: &str,
) -> Response {
    let Some(base) = state.pinned_upstream(provider) else {
        return provider_not_configured(provider);
    };
    let base = base.to_string();
    let path = uri
        .path_and_query()
        .map(|path_and_query| path_and_query.as_str().to_string())
        .unwrap_or_else(|| "/v1/chat/completions".to_string());
    let json: serde_json::Value = serde_json::from_slice(&buffered).unwrap_or_default();
    let (forwarded_body, decision) = routing::route_pinned(model, json)
        .map(|(body, decision)| (body, Some(decision)))
        .unwrap_or((buffered, None));
    let mut response = forward_upstream(
        state,
        &base,
        permit,
        &Method::POST,
        &path,
        headers,
        forwarded_body,
    )
    .await;
    if let Some(decision) = decision {
        if let Ok(value) = decision.provider.parse() {
            response
                .headers_mut()
                .insert(routing::PROVIDER_HEADER, value);
        }
    }
    response
}

async fn forward_upstream(
    state: &AppState,
    base_url: &str,
    _permit: UpstreamPermit,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> Response {
    let url = format!("{base_url}{path}");
    let nominated: Vec<String> = headers
        .get(axum::http::header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(|connection| {
            connection
                .split(',')
                .map(|field| field.trim().to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let mut forwarded = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        if is_hop_by_hop(name, &nominated) {
            continue;
        }
        forwarded.append(name.clone(), value.clone());
    }
    let upstream = match tokio::time::timeout(
        state.timeouts.first_chunk,
        state
            .client
            .request(method.clone(), url)
            .headers(forwarded)
            .body(body)
            .send(),
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
    let headers = forwarded_response_headers(upstream.headers());
    let bytes = match read_capped_upstream(upstream, state.timeouts.total, &state.metrics).await {
        Ok(bytes) => bytes,
        Err(response) => return *response,
    };
    let mut response = Response::builder().status(status);
    for (name, value) in headers.iter() {
        response = response.header(name, value);
    }
    response
        .body(Body::from(bytes))
        .unwrap_or_else(|_| bad_gateway())
}

/// Collect a buffered upstream body up to the byte bound (Iter 11,
/// F05). Oversize or timed-out bodies map to 502, never to unbounded
/// memory.
async fn read_capped_upstream(
    upstream: reqwest::Response,
    total: Duration,
    metrics: &Metrics,
) -> Result<Bytes, Box<Response>> {
    let read = async {
        let mut res = upstream;
        let mut buffered = Vec::new();
        loop {
            match res.chunk().await {
                Err(_) => {
                    metrics.upstream_error(UpstreamErrorKind::Transport);
                    return Err(Box::new(bad_gateway()));
                }
                Ok(None) => return Ok(Bytes::from(buffered)),
                Ok(Some(data)) => {
                    if buffered.len() + data.len() > MAX_UPSTREAM_BYTES {
                        metrics.upstream_error(UpstreamErrorKind::Status5xx);
                        return Err(Box::new(error_response(
                            StatusCode::BAD_GATEWAY,
                            "server_error",
                            "upstream_too_large",
                            format!(
                                "upstream response exceeds the {} MiB bound",
                                MAX_UPSTREAM_BYTES / (1024 * 1024),
                            ),
                        )));
                    }
                    buffered.extend_from_slice(&data);
                }
            }
        }
    };
    match tokio::time::timeout(total, read).await {
        Ok(result) => result,
        Err(_) => {
            metrics.upstream_error(UpstreamErrorKind::Timeout);
            Err(Box::new(gateway_timeout()))
        }
    }
}

/// Terminal SSE event emitted when the upstream stream breaks mid-body
/// (Iter 11, F06). Headers already went out as 200, so a clean EOF would
/// masquerade truncation as success; the explicit error event lets
/// clients tell interruption apart from completion.
const SSE_INTERRUPTED_EVENT: &[u8] = b"event: error\ndata: {\"error\":{\"message\":\"upstream stream interrupted\",\"type\":\"stream_error\",\"code\":\"upstream_interrupted\"}}\n\n";

/// SSE passthrough: forward each chunk as it arrives, preserving bytes and
/// order. Clean upstream EOF ends the stream unchanged; a stall or break
/// emits one terminal error event first, so truncation never looks like
/// success. The in-flight slot is held for the whole body lifetime
/// (Iter 11, A04) — the middleware counts headers, the stream counts
/// completion.
fn stream_response(
    status: StatusCode,
    upstream: reqwest::Response,
    idle: Duration,
    metrics: Arc<Metrics>,
) -> Response {
    let preserved = forwarded_response_headers(upstream.headers());
    let guard = metrics.in_flight_owned();
    let stream = futures_util::stream::unfold(
        (upstream, metrics, guard, false),
        move |(mut res, metrics, guard, failed)| async move {
            if failed {
                return None;
            }
            match tokio::time::timeout(idle, res.chunk()).await {
                Ok(Ok(Some(bytes))) => Some((
                    Ok::<Bytes, reqwest::Error>(bytes),
                    (res, metrics, guard, false),
                )),
                Ok(Ok(None)) => None,
                Err(_) => {
                    metrics.upstream_error(UpstreamErrorKind::Timeout);
                    eprintln!("gateway: upstream stream stalled, ending with error event");
                    Some((
                        Ok::<Bytes, reqwest::Error>(Bytes::from_static(SSE_INTERRUPTED_EVENT)),
                        (res, metrics, guard, true),
                    ))
                }
                Ok(Err(_)) => {
                    metrics.upstream_error(UpstreamErrorKind::Transport);
                    eprintln!("gateway: upstream stream broke, ending with error event");
                    Some((
                        Ok::<Bytes, reqwest::Error>(Bytes::from_static(SSE_INTERRUPTED_EVENT)),
                        (res, metrics, guard, true),
                    ))
                }
            }
        },
    );
    let mut response = Response::builder().status(status);
    for (name, value) in preserved.iter() {
        // Content-Type stays event-stream: the body is SSE either way.
        if name != CONTENT_TYPE {
            response = response.header(name, value);
        }
    }
    response
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
    // Error bodies are bounded like success bodies (Iter 11, F05):
    // the 200-char snippet is taken after a capped collection.
    let preserved = forwarded_response_headers(upstream.headers());
    let raw = match read_capped_upstream(upstream, total, metrics).await {
        Ok(bytes) => bytes,
        Err(response) => return *response,
    };
    // Upstreams may gzip error bodies even when the client
    // negotiated identity (seen live: gzipped 4xx). Decode so the
    // JSON detect and the snippet below read text, not binary.
    let bytes = decode_error_body(&raw);
    if serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|json| json.get("error").cloned())
        .is_some()
    {
        let mut response = Response::builder().status(status);
        for (name, value) in preserved.iter() {
            // Decoded bytes must not keep the upstream encoding
            // claim — it would lie about them (same F03 reasoning
            // as the rewritten branch below).
            if bytes != raw && name == axum::http::header::CONTENT_ENCODING {
                continue;
            }
            response = response.header(name, value);
        }
        return response
            .body(Body::from(bytes))
            .unwrap_or_else(|_| bad_gateway());
    }
    let kind = if status.is_client_error() {
        "invalid_request_error"
    } else {
        "server_error"
    };
    let mut response = error_response(
        status,
        kind,
        "upstream_error",
        format!(
            "upstream error (http {}): {}",
            status.as_u16(),
            snippet(&bytes)
        ),
    );
    // Negotiation metadata survives; representation headers do not —
    // the body is rewritten JSON, so upstream content-type/encoding
    // would lie about it (Iter 11, F03).
    for (name, value) in preserved.iter() {
        if name != CONTENT_TYPE
            && name != axum::http::header::CONTENT_ENCODING
            && name != axum::http::header::CONTENT_RANGE
        {
            response.headers_mut().insert(name.clone(), value.clone());
        }
    }
    response
}

/// Decode a gzipped upstream error body (Iter 20). Plain bodies
/// pass through untouched; corrupt gzip falls back to raw bytes
/// rather than failing the error mapping. Decode is capped: error
/// snippets need 200 chars, so anything past 1 MiB of output is
/// either corrupt or hostile.
fn decode_error_body(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read as _;
    const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
    if bytes.len() < 2 || bytes[0..2] != GZIP_MAGIC {
        return bytes.to_vec();
    }
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    let mut out = Vec::new();
    let decoded = std::io::Read::take(&mut decoder, 1024 * 1024).read_to_end(&mut out);
    match decoded {
        Ok(n) if n > 0 => out,
        _ => bytes.to_vec(),
    }
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

/// Virtual-model rejection (Iter 14): fail closed with an
/// OpenAI-shaped error instead of forwarding a gateway-owned id.
/// Budget violations are caller-fixable (400); unmet floors and empty
/// tiers are gateway-side availability (503 with `Retry-After`, since a
/// catalog reload or cool-down expiry may restore service).
fn rejection_response(rejection: &routing::RouteRejection) -> Response {
    match rejection.code {
        "budget_exceeded" => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            rejection.code,
            rejection.message.clone(),
        ),
        "invalid_body" => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            rejection.code,
            rejection.message.clone(),
        ),
        _ => {
            let mut response = error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_error",
                rejection.code,
                rejection.message.clone(),
            );
            response
                .headers_mut()
                .insert("retry-after", axum::http::HeaderValue::from_static("1"));
            response
        }
    }
}

/// Load-shed response (Iter 9): the gateway is at capacity, so the
/// request never reached upstream. OpenAI-shaped with `Retry-After`.
fn service_unavailable() -> Response {
    let mut response = error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "server_error",
        "service_unavailable",
        "gateway at capacity, retry later".to_string(),
    );
    response
        .headers_mut()
        .insert("retry-after", axum::http::HeaderValue::from_static("1"));
    response
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
