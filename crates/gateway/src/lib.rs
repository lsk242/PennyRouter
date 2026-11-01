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

pub mod classifier;
mod metrics;
pub mod routing;

pub use metrics::{
    ClassifierResult, ClassifierSummary, ClassifierTier, Metrics, Route, UpstreamErrorKind,
};

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

/// Smart classifier leg config (Iter 26): which decision model serves
/// `openrouter/pair/smart[/max]`, and on which upstream. `None` means
/// smart ids fail closed with `classifier_not_configured` — every other
/// id keeps the pure-local routing contract.
#[derive(Clone, Debug)]
pub struct ClassifierConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
    /// Tier served when the classifier leg fails: `low` or `lowest`.
    pub fallback_tier: String,
    pub max_tokens: u32,
    /// Classifier input budget in serialized-JSON characters.
    pub max_input_chars: usize,
}

/// Defaults for the classifier leg (Iter 26): one short call, small
/// output, fail over fast rather than stall the completion.
const DEFAULT_CLASSIFIER_TIMEOUT_MS: u64 = 8000;
const DEFAULT_CLASSIFIER_MAX_TOKENS: u32 = 64;
const DEFAULT_CLASSIFIER_MAX_INPUT_CHARS: usize = 32_000;
/// Upper bound on a classifier reply body: completions are capped at
/// `max_tokens`, so anything past this is not a verdict.
const MAX_CLASSIFIER_BYTES: usize = 64 * 1024;

impl ClassifierConfig {
    /// Load from `CLASSIFIER_*` (`None` when `CLASSIFIER_MODEL` is
    /// unset or empty: smart disabled). `openrouter_default` is the
    /// already-resolved `OPENROUTER_BASE_URL` value. An unknown
    /// fallback tier degrades to `low` with a stderr note — the
    /// classifier replaces the chain, so the fallback is one value,
    /// never recursive.
    pub fn from_env(openrouter_default: &str) -> Option<Self> {
        let model = std::env::var("CLASSIFIER_MODEL").ok()?;
        if model.is_empty() {
            return None;
        }
        let base_url = std::env::var("CLASSIFIER_BASE_URL")
            .ok()
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| openrouter_default.to_string());
        let fallback_tier = match std::env::var("CLASSIFIER_FALLBACK_TIER")
            .as_deref()
            .unwrap_or("low")
        {
            "lowest" => "lowest".to_string(),
            "low" => "low".to_string(),
            other => {
                eprintln!("gateway: unknown CLASSIFIER_FALLBACK_TIER {other:?}, using \"low\"");
                "low".to_string()
            }
        };
        Some(Self {
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: std::env::var("CLASSIFIER_API_KEY")
                .ok()
                .filter(|key| !key.is_empty()),
            timeout: Duration::from_millis(
                std::env::var("CLASSIFIER_TIMEOUT_MS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(DEFAULT_CLASSIFIER_TIMEOUT_MS),
            ),
            fallback_tier,
            max_tokens: std::env::var("CLASSIFIER_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_CLASSIFIER_MAX_TOKENS),
            max_input_chars: std::env::var("CLASSIFIER_MAX_INPUT_CHARS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_CLASSIFIER_MAX_INPUT_CHARS),
        })
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
    classifier: Option<ClassifierConfig>,
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
            classifier: None,
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

    /// State with a smart classifier leg (Iter 26): enables
    /// `openrouter/pair/smart[/max]`. Without it those ids fail closed.
    pub fn with_classifier(mut self, config: ClassifierConfig) -> Self {
        self.classifier = Some(config);
        self
    }

    /// Classifier leg config, if one is configured.
    pub fn classifier_config(&self) -> Option<ClassifierConfig> {
        self.classifier.clone()
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
    /// defaulting to OpenRouter; routing from `CATALOG_DB` when set.
    /// An unloadable catalog at boot still attaches a standby router
    /// (Iter 23) — requests pass through until the periodic reload
    /// heals it, instead of staying dark until a restart.
    /// Concurrency bounds from `UPSTREAM_MAX_CONCURRENT` /
    /// `UPSTREAM_MAX_QUEUED`, defaulting to the compiled defaults on
    /// missing or unparsable values.
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
        // Smart classifier leg (Iter 26): unset `CLASSIFIER_MODEL`
        // disables the smart ids (fail closed); anything else keeps the
        // pure-local routing contract for every other id.
        let classifier = ClassifierConfig::from_env(&base);
        let mut state = Self::with_limits(base, Timeouts::default(), limits);
        state.classifier = classifier;
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
                    eprintln!("gateway: routing standby ({e}), passing through until reload heals");
                    state.with_router(Arc::new(routing::Router::standby(&path)))
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
        if state.router_handle().is_some_and(|r| r.is_ready()) {
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
    page.push_str("<h2>Smart classifier</h2>");
    match state.classifier_config() {
        None => page.push_str("<p>Classifier: <strong>off</strong> (set CLASSIFIER_MODEL) — smart ids fail closed.</p>"),
        Some(config) => {
            page.push_str(&format!(
                "<p>Classifier: <strong>{}</strong> via {} — fallback tier {}, timeout {}ms, max tokens {}, input cap {} chars.</p>",
                esc(&config.model),
                esc(&config.base_url),
                esc(&config.fallback_tier),
                config.timeout.as_millis(),
                config.max_tokens,
                config.max_input_chars,
            ));
        }
    }
    {
        let summary = state.metrics.classifier_summary();
        page.push_str("<table><tr><th>result</th><th>tier</th><th>count</th></tr>");
        for (result, tier, count) in &summary.counts {
            if *count == 0 {
                continue;
            }
            page.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(result),
                esc(tier),
                count
            ));
        }
        page.push_str("</table>");
        let avg_ms = if summary.latency_count > 0 {
            summary.latency_sum_ms / summary.latency_count as f64
        } else {
            0.0
        };
        page.push_str(&format!(
            "<p>Legs: {} (avg {avg_ms:.1} ms) — fallbacks: low {} / lowest {}.</p>",
            summary.latency_count, summary.fallback_low, summary.fallback_lowest,
        ));
        if !summary.last_error.is_empty() {
            page.push_str(&format!("<p>Last error: {}</p>", esc(&summary.last_error)));
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
    // Smart classifier ids (Iter 26): the `/fallback/` suffix never
    // composes with smart — fail closed instead of leaking a
    // gateway-owned id upstream. Other smart ids run the classifier
    // leg first, then the normal tier dispatch with no session pin.
    if let Some(model) = raw_model(&buffered) {
        if routing::is_smart_fallback(&model) {
            return rejection_response(&routing::RouteRejection {
                code: "invalid_model",
                message: format!(
                    "{model:?} is not a virtual model: smart ids take no /fallback/ suffix"
                ),
            });
        }
        if let Some(smart) = routing::parse_smart(&model) {
            let path = uri
                .path_and_query()
                .map(|path_and_query| path_and_query.as_str().to_string())
                .unwrap_or_else(|| "/v1/chat/completions".to_string());
            return smart_completion(&state, &headers, permit, &path, buffered, &model, smart)
                .await;
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
            // 429 history steers later virtual picks elsewhere, for as
            // long as the upstream headers say (Iter 21).
            if let Some(router) = &state.router {
                let headers = response.headers();
                let retry_after = headers
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok());
                let reset_ms = headers
                    .get("x-ratelimit-reset")
                    .and_then(|v| v.to_str().ok());
                router.note_throttled(
                    &decision.model_id,
                    &decision.provider,
                    routing::throttle_until(retry_after, reset_ms),
                );
            }
            state.metrics.throttled();
        }
    }
    // Availability (Iter 13): one retry on a different provider for
    // virtual requests when the first attempt is retryable. Non-virtual
    // requests keep exactly-once semantics.
    // `retry_body` exists only for virtual requests (see above), so
    // reaching here already implies virtual + retryable.
    //
    // Free-fallback chains (Iter 21) resolve here too, before the
    // retry block moves `retry_body`: (original model id, fallback
    // tier) for the post-retry leg below.
    let fallback_request: Option<(String, String)> = retry_body.as_ref().and_then(|rb| {
        let model_field = serde_json::from_slice::<serde_json::Value>(rb)
            .ok()
            .and_then(|json| json.get("model")?.as_str().map(str::to_string))?;
        let target = routing::parse_virtual(&model_field)?.fallback?;
        Some((model_field, target))
    });
    // Owned copy for the post-retry leg: the retry block moves
    // `retry_body`. `None` for non-virtual requests (free).
    let fallback_body: Option<Vec<u8>> = retry_body.clone();
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
    // Free-fallback chains (Iter 21): the free attempt (and its single
    // retry, if any) failed with a retryable status, so resolve the
    // named tier and forward once. Anything else — including
    // auth/config errors — returns as-is. No further retry: this
    // response stands, headers included.
    if is_retryable(response.status()) {
        if let (Some(router), Some((model_field, target)), Some(fallback_body)) =
            (&state.router, &fallback_request, &fallback_body)
        {
            let auth = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            let ignore = decision.as_ref().map(|d| d.provider.as_str());
            if let Some((rewritten, fb_decision)) =
                router.route_fallback(fallback_body, model_field, target, auth, ignore)
            {
                let permit = match acquire_upstream_slot(&state).await {
                    Some(permit) => permit,
                    None => {
                        state.metrics.load_shed();
                        return service_unavailable();
                    }
                };
                let fallback_base = state.upstream_base_url.clone();
                response = forward_upstream(
                    &state,
                    &fallback_base,
                    permit,
                    &Method::POST,
                    &path,
                    &headers,
                    rewritten,
                )
                .await;
                if response.status() == StatusCode::TOO_MANY_REQUESTS {
                    let fb_headers = response.headers();
                    router.note_throttled(
                        &fb_decision.model_id,
                        &fb_decision.provider,
                        routing::throttle_until(
                            fb_headers
                                .get(reqwest::header::RETRY_AFTER)
                                .and_then(|v| v.to_str().ok()),
                            fb_headers
                                .get("x-ratelimit-reset")
                                .and_then(|v| v.to_str().ok()),
                        ),
                    );
                    state.metrics.throttled();
                }
                if let Ok(provider) = fb_decision.provider.parse() {
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

/// Diagnostic context for one smart request (Iter 26): what the
/// classifier chose (or fell back to), on which decision model, and how
/// long the leg took. Surfaced in `x-pennyrouter-*` headers and logs —
/// never the prompt.
struct SmartCtx {
    tier: String,
    classifier: String,
    confidence: Option<f64>,
    latency_ms: u64,
    fallback: bool,
    result: &'static str,
}

impl SmartCtx {
    fn new(
        tier: String,
        config: &ClassifierConfig,
        confidence: Option<f64>,
        latency: Duration,
        fallback: bool,
        result: &'static str,
    ) -> Self {
        Self {
            tier,
            classifier: config.model.clone(),
            confidence,
            latency_ms: latency.as_millis().min(u128::from(u64::MAX)) as u64,
            fallback,
            result,
        }
    }
}

/// Outcome of the classifier leg: a validated in-domain tier, or a
/// failure with its metric slot. Only `AuthError` answers the user
/// directly (502); every other failure serves the fallback tier.
enum ClassifierOutcome {
    Classified {
        tier: String,
        confidence: Option<f64>,
        latency: Duration,
    },
    Failed {
        result: ClassifierResult,
        detail: String,
        latency: Option<Duration>,
    },
}

/// Run the classifier leg (Iter 26): one short non-streaming chat call
/// to the configured decision model. No retry here — the completion
/// leg owns the single retry budget, and a failed classification falls
/// back immediately rather than stall the user request.
async fn call_classifier(
    state: &AppState,
    headers: &HeaderMap,
    json: &serde_json::Value,
    smart: &routing::SmartId,
    config: &ClassifierConfig,
) -> ClassifierOutcome {
    let fail =
        |result: ClassifierResult, detail: String, start: Instant| ClassifierOutcome::Failed {
            result,
            detail,
            latency: Some(start.elapsed()),
        };
    let start = Instant::now();
    let set = classifier::tier_set(smart.max);
    let Some(input) = classifier::extract_input(json) else {
        return ClassifierOutcome::Failed {
            result: ClassifierResult::NoInput,
            detail: "request carried no classifiable input".to_string(),
            latency: None,
        };
    };
    let input = classifier::truncate_input(input, config.max_input_chars);
    let request = classifier::build_request(
        &config.model,
        &set,
        &input,
        config.max_tokens,
        smart.privacy,
    );
    // Auth for this leg only: the operator key wins when set,
    // otherwise the caller's key rides along (BYOK preserved). With
    // neither, the call goes out unauthenticated (e.g. a local
    // classifier with no auth). Operators take note: without
    // `CLASSIFIER_API_KEY`, a foreign `CLASSIFIER_BASE_URL` receives
    // the caller's key — set the override for cross-vendor legs.
    let auth = config
        .api_key
        .clone()
        .map(|key| format!("Bearer {key}"))
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        });
    let url = format!("{}/chat/completions", config.base_url);
    let send = {
        let mut send = state.client.post(&url).json(&request);
        if let Some(auth) = auth {
            send = send.header(axum::http::header::AUTHORIZATION, auth);
        }
        send
    };
    let (status, bytes) = match tokio::time::timeout(config.timeout, async {
        let response = send.send().await.map_err(|_| "transport")?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|_| "transport")?;
        Ok::<_, &'static str>((status, bytes))
    })
    .await
    {
        Err(_) => {
            return fail(
                ClassifierResult::Timeout,
                format!(
                    "classifier timed out after {}ms",
                    config.timeout.as_millis()
                ),
                start,
            );
        }
        Ok(Err(_)) => {
            return fail(
                ClassifierResult::Unavailable,
                "classifier transport error".to_string(),
                start,
            )
        }
        Ok(Ok((status, bytes))) => (status, bytes),
    };
    let code = status.as_u16();
    match code {
        401 | 403 => {
            let whose = if config.api_key.is_some() {
                "the configured CLASSIFIER_API_KEY"
            } else {
                "the request credentials"
            };
            return fail(
                ClassifierResult::AuthError,
                format!("classifier upstream rejected {whose} (HTTP {code})"),
                start,
            );
        }
        429 | 500..=599 => {
            return fail(
                ClassifierResult::Unavailable,
                format!("classifier upstream answered HTTP {code}"),
                start,
            );
        }
        200..=299 => {}
        _ => {
            return fail(
                ClassifierResult::Invalid,
                format!("classifier upstream answered HTTP {code}"),
                start,
            );
        }
    }
    if bytes.len() > MAX_CLASSIFIER_BYTES {
        return fail(
            ClassifierResult::Invalid,
            "classifier reply exceeds the size bound".to_string(),
            start,
        );
    }
    let content = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|outer| {
            outer
                .get("choices")?
                .as_array()?
                .first()?
                .get("message")?
                .get("content")?
                .as_str()
                .map(str::to_string)
        });
    let Some(content) = content else {
        return fail(
            ClassifierResult::Invalid,
            "classifier reply had no message content".to_string(),
            start,
        );
    };
    match classifier::parse_verdict(&content, &set) {
        Ok(verdict) => ClassifierOutcome::Classified {
            tier: verdict.tier,
            confidence: verdict.confidence,
            latency: start.elapsed(),
        },
        Err(error) => fail(
            ClassifierResult::Invalid,
            format!("classifier verdict rejected: {error:?}"),
            start,
        ),
    }
}

/// Serve a smart request (Iter 26): classify, then dispatch through the
/// normal tier path. The admission permit is held across both legs, so
/// a smart request costs one slot for longer — bounded by the same
/// concurrency limits as everything else.
#[allow(clippy::too_many_arguments)]
async fn smart_completion(
    state: &AppState,
    headers: &HeaderMap,
    permit: UpstreamPermit,
    path: &str,
    buffered: Vec<u8>,
    model_field: &str,
    smart: routing::SmartId,
) -> Response {
    let json: serde_json::Value = match serde_json::from_slice(&buffered) {
        Ok(json) => json,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "invalid_body",
                "request body is not valid JSON".to_string(),
            );
        }
    };
    // Unconfigured means fail closed with no guess: a smart id must
    // never decay into passthrough of a gateway-owned id.
    let Some(config) = state.classifier_config() else {
        state
            .metrics
            .classifier_report(ClassifierResult::NotConfigured, ClassifierTier::None);
        state
            .metrics
            .classifier_error("smart classifier not configured (set CLASSIFIER_MODEL)");
        log_line(
            serde_json::json!({
                "smart": true,
                "model": model_field,
                "classifier_result": "not_configured",
            })
            .to_string(),
        );
        return rejection_response(&routing::RouteRejection {
            code: "classifier_not_configured",
            message: "smart routing is not configured on this gateway (set CLASSIFIER_MODEL)"
                .to_string(),
        });
    };
    match call_classifier(state, headers, &json, &smart, &config).await {
        ClassifierOutcome::Classified {
            tier,
            confidence,
            latency,
        } => {
            state
                .metrics
                .classifier_report(ClassifierResult::Ok, ClassifierTier::of(&tier));
            state.metrics.classifier_latency(latency);
            let ctx = SmartCtx::new(tier.clone(), &config, confidence, latency, false, "ok");
            log_smart(model_field, &ctx);
            let Some(router) = &state.router else {
                return with_smart_headers(
                    rejection_response(&routing::RouteRejection {
                        code: "no_eligible_offering",
                        message: "no catalog is loaded, so no smart tier can serve".to_string(),
                    }),
                    &ctx,
                );
            };
            // A rejected classified tier fails closed with the
            // classifier headers attached — never silently re-picked
            // into another tier.
            let (rewritten, decision) =
                match router.route_smart(&buffered, model_field, &tier, None) {
                    Ok(route) => route,
                    Err(rejection) => {
                        return with_smart_headers(rejection_response(&rejection), &ctx)
                    }
                };
            let base = state.upstream_base_url.clone();
            let mut response = forward_upstream(
                state,
                &base,
                permit,
                &Method::POST,
                path,
                headers,
                rewritten,
            )
            .await;
            record_throttle(state, router, &decision, &response);
            // One availability retry inside the classified tier only.
            if is_retryable(response.status()) {
                if let Ok((rewritten, retry_decision)) =
                    router.route_smart(&buffered, model_field, &tier, Some(&decision.provider))
                {
                    let permit = match acquire_upstream_slot(state).await {
                        Some(permit) => permit,
                        None => {
                            state.metrics.load_shed();
                            return with_smart_headers(service_unavailable(), &ctx);
                        }
                    };
                    let retry_base = state.upstream_base_url.clone();
                    response = forward_upstream(
                        state,
                        &retry_base,
                        permit,
                        &Method::POST,
                        path,
                        headers,
                        rewritten,
                    )
                    .await;
                    record_throttle(state, router, &retry_decision, &response);
                    return with_smart_headers(
                        with_provider_header(response, &retry_decision.provider),
                        &ctx,
                    );
                }
            }
            with_smart_headers(with_provider_header(response, &decision.provider), &ctx)
        }
        ClassifierOutcome::Failed {
            result,
            detail,
            latency,
        } => {
            state
                .metrics
                .classifier_report(result, ClassifierTier::None);
            if let Some(latency) = latency {
                state.metrics.classifier_latency(latency);
            }
            state.metrics.classifier_error(detail.clone());
            // Auth failures answer directly: no tier was chosen and no
            // fallback should launder a credential problem.
            if result == ClassifierResult::AuthError {
                let response = error_response(
                    StatusCode::BAD_GATEWAY,
                    "invalid_request_error",
                    "classifier_auth_error",
                    detail,
                );
                return with_classifier_header(response, &config.model);
            }
            // Every other failure serves the one configured fallback
            // tier. If that tier itself rejects, the rejection stands —
            // the fallback is a value, not a chain.
            let fallback = config.fallback_tier.clone();
            state.metrics.smart_fallback(fallback == "lowest");
            let ctx = SmartCtx::new(
                fallback.clone(),
                &config,
                None,
                latency.unwrap_or_default(),
                true,
                result.label(),
            );
            log_smart(model_field, &ctx);
            let Some(router) = &state.router else {
                return with_smart_headers(
                    rejection_response(&routing::RouteRejection {
                        code: "no_eligible_offering",
                        message: "no catalog is loaded, so no smart tier can serve".to_string(),
                    }),
                    &ctx,
                );
            };
            match router.route_smart(&buffered, model_field, &fallback, None) {
                Ok((rewritten, decision)) => {
                    let base = state.upstream_base_url.clone();
                    let response = forward_upstream(
                        state,
                        &base,
                        permit,
                        &Method::POST,
                        path,
                        headers,
                        rewritten,
                    )
                    .await;
                    record_throttle(state, router, &decision, &response);
                    with_smart_headers(with_provider_header(response, &decision.provider), &ctx)
                }
                Err(rejection) => with_smart_headers(rejection_response(&rejection), &ctx),
            }
        }
    }
}

/// Log one smart request: tier, decision model, latency, fallback flag.
/// The prompt, the verdict reasoning, and credentials never enter logs.
fn log_smart(model_field: &str, ctx: &SmartCtx) {
    log_line(
        serde_json::json!({
            "smart": true,
            "model": model_field,
            "classifier_model": ctx.classifier,
            "classifier_tier": ctx.tier,
            "classifier_confidence": ctx.confidence,
            "classifier_latency_ms": ctx.latency_ms,
            "classifier_fallback": ctx.fallback,
            "classifier_result": ctx.result,
        })
        .to_string(),
    );
}

/// Record 429 cool-down for a smart completion leg, mirroring the
/// normal virtual path.
fn record_throttle(
    state: &AppState,
    router: &routing::Router,
    decision: &routing::Decision,
    response: &Response,
) {
    if response.status() != StatusCode::TOO_MANY_REQUESTS {
        return;
    }
    let headers = response.headers();
    let retry_after = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok());
    let reset_ms = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok());
    router.note_throttled(
        &decision.model_id,
        &decision.provider,
        routing::throttle_until(retry_after, reset_ms),
    );
    state.metrics.throttled();
}

/// Attach the serving provider, like the normal routed path.
fn with_provider_header(mut response: Response, provider: &str) -> Response {
    if let Ok(value) = provider.parse() {
        response
            .headers_mut()
            .insert(routing::PROVIDER_HEADER, value);
    }
    response
}

/// Attach the smart diagnostic headers. All optional, none leak the
/// prompt: tier, decision model, numeric confidence, leg latency, and
/// the fallback marker (present only when the fallback fired).
fn with_smart_headers(mut response: Response, ctx: &SmartCtx) -> Response {
    let headers = response.headers_mut();
    if let Ok(value) = ctx.tier.parse() {
        headers.insert(
            axum::http::HeaderName::from_static("x-pennyrouter-smart-tier"),
            value,
        );
    }
    if let Ok(value) = ctx.classifier.parse() {
        headers.insert(
            axum::http::HeaderName::from_static("x-pennyrouter-smart-classifier"),
            value,
        );
    }
    if let Some(confidence) = ctx.confidence {
        if let Ok(value) = format!("{confidence:.2}").parse() {
            headers.insert(
                axum::http::HeaderName::from_static("x-pennyrouter-classifier-confidence"),
                value,
            );
        }
    }
    if let Ok(value) = ctx.latency_ms.to_string().parse() {
        headers.insert(
            axum::http::HeaderName::from_static("x-pennyrouter-classifier-latency-ms"),
            value,
        );
    }
    if ctx.fallback {
        headers.insert(
            axum::http::HeaderName::from_static("x-pennyrouter-classifier-fallback"),
            axum::http::HeaderValue::from_static("true"),
        );
    }
    response
}

/// Attach only the classifier identity (auth failures chose no tier).
fn with_classifier_header(mut response: Response, classifier: &str) -> Response {
    if let Ok(value) = classifier.parse() {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static("x-pennyrouter-smart-classifier"),
            value,
        );
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
const FORWARDED_RESPONSE_HEADERS: [&str; 10] = [
    "content-type",
    "content-encoding",
    "content-range",
    "retry-after",
    "www-authenticate",
    "etag",
    "last-modified",
    // Upstream quota signals ride 429s (Iter 21): upstreams only send
    // these on rejections, so preserving them never invents quota
    // state on success.
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
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
/// Budget and tool-support violations are caller-fixable (400); unmet
/// floors and empty tiers are gateway-side availability (503 with
/// `Retry-After`, since a catalog reload or cool-down expiry may
/// restore service).
fn rejection_response(rejection: &routing::RouteRejection) -> Response {
    match rejection.code {
        "budget_exceeded" => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            rejection.code,
            rejection.message.clone(),
        ),
        // Tool-starvation is caller-fixable (disable tools or pick a
        // tool-capable route), like a budget violation (Iter 22).
        "tools_unsupported" => error_response(
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
        // Unsupported id shapes (e.g. smart with a fallback suffix)
        // are caller-fixable like a bad body (Iter 26).
        "invalid_model" => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            rejection.code,
            rejection.message.clone(),
        ),
        // The classifier leg rejected the credentials: caller-fixable
        // (BYOK) or operator-fixable (CLASSIFIER_API_KEY), reported as
        // a 502 like other upstream credential failures (Iter 26).
        "classifier_auth_error" => error_response(
            StatusCode::BAD_GATEWAY,
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
