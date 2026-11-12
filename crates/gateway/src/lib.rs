use axum::{
    body::{Body, Bytes},
    extract::{Query, Request, State},
    http::{
        header::{CONTENT_TYPE, LOCATION},
        HeaderMap, Method, StatusCode,
    },
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
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

/// Readiness (Q07-A): 200 only once a catalog snapshot has loaded,
/// 503 while the gateway is standby passthrough. `/health` stays
/// pure liveness and never depends on the catalog, so container
/// checks keep working during reload gaps.
async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    if state
        .router
        .as_ref()
        .is_some_and(|router| router.is_ready())
    {
        (StatusCode::OK, Json(Health { status: "ready" }))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Health {
                status: "not_ready",
            }),
        )
    }
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
///
/// Q08: no stored key, ever. The classifier leg authenticates with the
/// caller's `Authorization` header (pure BYOK), optionally overridden
/// per-request by `x-pennyrouter-classifier-key`.
#[derive(Clone, Debug)]
pub struct ClassifierConfig {
    pub model: String,
    pub base_url: String,
    pub timeout: Duration,
    /// Tier served when the classifier leg fails: `low` or `lowest`.
    pub fallback_tier: String,
    pub max_tokens: u32,
    /// Classifier input budget in serialized-JSON characters.
    pub max_input_chars: usize,
}

/// Per-request classifier credential (Q08): the
/// `x-pennyrouter-classifier-key` header wins when present (raw key,
/// `Bearer`-wrapped unless already prefixed); otherwise the caller's
/// `Authorization` header rides along verbatim; otherwise the call
/// goes out unauthenticated (e.g. a local classifier with no auth).
/// Nothing is read from env and nothing is stored.
pub fn classifier_auth(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(key) = headers
        .get("x-pennyrouter-classifier-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        if key.starts_with("Bearer ") {
            return Some(key.to_string());
        }
        return Some(format!("Bearer {key}"));
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
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
    allow_management_proxy: bool,
    extra_upstreams: HashMap<String, String>,
    client: reqwest::Client,
    timeouts: Timeouts,
    metrics: Arc<Metrics>,
    router: Option<Arc<routing::Router>>,
    classifier: Option<ClassifierConfig>,
    upstream_slots: Arc<Semaphore>,
    queue_slots: Arc<Semaphore>,
    modal_models: Vec<String>,
    baseten_models: Vec<String>,
}

/// Comma-separated env model list (`MODAL_MODELS`, `BASETEN_MODELS`):
/// operator-nominated ids advertised on the menu when the provider
/// is configured. Empty entries dropped, never guessed.
fn env_model_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .map(|raw| {
            raw.split(',')
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default()
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
            allow_management_proxy: false,
            extra_upstreams: HashMap::new(),
            client,
            timeouts,
            metrics: Arc::new(Metrics::new()),
            router: None,
            classifier: None,
            upstream_slots: Arc::new(Semaphore::new(max_upstream)),
            queue_slots: Arc::new(Semaphore::new(limits.max_queued)),
            modal_models: Vec::new(),
            baseten_models: Vec::new(),
        }
    }

    /// Operator-nominated Modal ids for the menu (test seam; production
    /// reads `MODAL_MODELS` in `from_env`).
    pub fn with_modal_models(mut self, models: Vec<String>) -> Self {
        self.modal_models = models;
        self
    }

    /// Operator-nominated Baseten ids for the menu (test seam; production
    /// reads `BASETEN_MODELS` in `from_env`).
    pub fn with_baseten_models(mut self, models: Vec<String>) -> Self {
        self.baseten_models = models;
        self
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

    /// Allow proxying management writes (Iter 25): key creation,
    /// credit purchases, workspace budget changes. Denied by default;
    /// tests and explicit opt-ins use this instead of the env flag.
    pub fn with_management_proxy(mut self, allow: bool) -> Self {
        self.allow_management_proxy = allow;
        self
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
        warn_insecure_base("OPENROUTER_BASE_URL", &base);
        if std::env::var("CLASSIFIER_BASE_URL")
            .ok()
            .is_some_and(|url| !url.is_empty())
        {
            if let Some(config) = &classifier {
                warn_insecure_base("CLASSIFIER_BASE_URL", &config.base_url);
            }
        }
        let mut state = Self::with_limits(base, Timeouts::default(), limits);
        state.classifier = classifier;
        // Provider-pinned upstreams (Iter 16): unset means the
        // provider's routes fail closed with provider_not_configured.
        // Baseten defaults to its public inference host (same host the
        // catalog score uses, without the score's `/v1` route prefix —
        // the gateway appends the incoming `/v1/…` path itself; an
        // explicitly configured trailing `/v1` is stripped at
        // registration). Modal has no shared deployment, so it stays
        // unset until configured.
        if let Ok(url) = std::env::var("MODAL_BASE_URL") {
            if !url.is_empty() {
                warn_insecure_base("MODAL_BASE_URL", &url);
                state = state.with_upstream("modal", &url);
            }
        }
        if let Ok(url) = std::env::var("BASETEN_BASE_URL") {
            if !url.is_empty() {
                warn_insecure_base("BASETEN_BASE_URL", &url);
                state = state.with_upstream("baseten", &url);
            }
        } else {
            state = state.with_upstream("baseten", "https://inference.baseten.co");
        }
        // Management writes (Iter 25): proxied only on explicit opt-in,
        // otherwise denied with 403 before touching upstream.
        state.allow_management_proxy = env_flag("ALLOW_MANAGEMENT_PROXY");
        // Menu-nominated provider models: advertised on `/v1/models`
        // only when the provider itself is configured.
        state.modal_models = env_model_list("MODAL_MODELS");
        state.baseten_models = env_model_list("BASETEN_MODELS");
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
pub(crate) fn normalize_pinned_base(base_url: &str) -> String {
    let mut base = base_url.trim_end_matches('/').to_string();
    while let Some(stripped) = base.strip_suffix("/v1") {
        base = stripped.trim_end_matches('/').to_string();
    }
    base
}

/// S09: true when a base URL would send bearer keys over cleartext
/// HTTP to a non-loopback host. `starts_with` checks only — no URL
/// parser dependency. Callers warn at startup, never reject:
/// rejection would break local-dev mocks.
fn insecure_base_url(base_url: &str) -> bool {
    // Schemes are case-insensitive; only cleartext http can warn.
    let rest = match base_url.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("http://") => &base_url[7..],
        _ => return false,
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !(host == "localhost"
        || host.starts_with("localhost:")
        || host == "127.0.0.1"
        || host.starts_with("127.0.0.1:")
        || host == "[::1]"
        || host.starts_with("[::1]:"))
}

/// Startup warning for one configured base URL (S09).
fn warn_insecure_base(env_name: &str, base_url: &str) {
    if insecure_base_url(base_url) {
        eprintln!(
            "gateway: {env_name}={base_url:?} uses cleartext http to a non-loopback host: \
             bearer keys travel unencrypted. Use https outside local dev."
        );
    }
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
    // Zero disables: return a handle that is already done instead of
    // constructing `interval(0)`, which panics. `main` also guards,
    // but embedders and tests must not be able to panic here.
    if interval.is_zero() {
        let _ = (&router, &metrics);
        return tokio::spawn(async {});
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // A slow or failed reload never delays the next scheduled one.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            // Q05: catalog loads are blocking SQLite/file I/O — run
            // them on the blocking pool so a slow disk never stalls
            // the 1-CPU async worker. The reload itself still swaps
            // atomically (see `Router::reload`).
            let loader = router.clone();
            match tokio::task::spawn_blocking(move || loader.reload())
                .await
                .unwrap_or_else(|e| Err(routing::RouteError(format!("reload task panicked: {e}"))))
            {
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
/// Truthy env flags: `1`/`true`/`yes` (any case) opt in; anything
/// else — including unset — leaves the feature off.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Whether `GET /ui` is registered. Read once at startup (route
/// registration) and per request by the landing page; the two agree
/// in production, where the process env is fixed for life.
pub fn ui_enabled() -> bool {
    env_flag("GATEWAY_UI")
}

pub fn app_with_state(state: AppState) -> Router {
    let metrics = state.metrics.clone();
    let router = Router::new()
        .route("/", get(landing))
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics_handler))
        .route("/v1/chat/completions", post(routed_completion))
        // Responses bodies carry the same `model` field, so they get
        // the same routing, budget, and floor policy (Iter 14).
        .route("/v1/responses", post(routed_completion))
        // OpenRouter-native aliases (Iter 25): native SDKs address the
        // same routed handler under `/api/v1/…`. The upstream path is
        // rewritten to `/v1/…` before forwarding, so the configured
        // `/api` base never doubles it.
        .route("/api/v1/chat/completions", post(routed_completion))
        .route("/api/v1/responses", post(routed_completion))
        // Decisions bodies (`model` + `questions`/`state`) route over
        // decisions-capable offerings and forward to the upstream
        // alpha decisions endpoint (Iter 27). Same Iter 25 native
        // alias as chat/responses: the handler ignores the incoming
        // path (it always forwards to the alpha endpoint), so no
        // prefix strip is needed.
        .route("/v1/decisions", post(routed_decisions))
        .route("/api/v1/decisions", post(routed_decisions))
        // Curated menu (Iter 13): clients discover exactly the virtual
        // models; this path used to proxy upstream.
        .route("/v1/models", get(models_list));
    // Debug UI (Iter 17): registered only when enabled. Disabled
    // means no route at all — the path falls through to the
    // fallback proxy like any unknown path.
    let router = if ui_enabled() {
        router
            .route("/ui", get(ui_page))
            .route("/ui/preview", get(ui_preview))
    } else {
        router
    };
    router
        .fallback(fallback_proxy)
        .layer(middleware::from_fn_with_state(metrics, track))
        .with_state(state)
}

/// Static landing template: one self-contained HTML file with inline
/// CSS — no external assets, no extra requests. `{{…}}` placeholders
/// carry the few dynamic values (version, build hash, config status);
/// everything else is fixed markup. Always registered — it answers
/// only its own path, so API traffic is unaffected.
const LANDING_TEMPLATE: &str = include_str!("../templates/landing.html");

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
    let (routing_class, routing_state, routing_text) =
        if state.router_handle().is_some_and(|r| r.is_ready()) {
            ("ok", "on", "on (catalog)")
        } else {
            ("warn", "off", "off (passthrough)")
        };
    let (ui_class, ui_state, ui_detail, ui_note) = if ui_enabled() {
        (
            "ok",
            "enabled",
            "<a href=\"/ui\">debug UI</a> (enabled)".to_string(),
            "enabled",
        )
    } else {
        (
            "warn",
            "disabled",
            "debug UI (disabled — set GATEWAY_UI=1)".to_string(),
            "requires GATEWAY_UI=1",
        )
    };
    // Longer placeholder names first: `{{routing_text}}` must not match
    // inside `{{routing_pill_text}}` (it cannot — the `_pill_` infix
    // breaks the match — but explicit ordering keeps that obvious).
    let page = LANDING_TEMPLATE
        .replace("{{version}}", &esc(build_version()))
        .replace("{{build}}", &esc(build_hash()))
        .replace("{{routing_pill_class}}", routing_class)
        .replace("{{routing_pill_text}}", routing_state)
        .replace("{{routing_text}}", routing_text)
        .replace("{{ui_pill_class}}", ui_class)
        .replace("{{ui_pill_text}}", ui_state)
        .replace("{{ui_detail}}", &ui_detail)
        .replace("{{upstream}}", &esc(state.upstream_base()))
        .replace("{{pinned}}", &esc(&pinned.join(", ")))
        .replace("{{ui_endpoint_note}}", ui_note);
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        page,
    )
        .into_response()
}

/// Preview query (Iter 29): `model` is the raw `model` field value to
/// inspect, `top` caps rows (default 3, clamped 1..=20). The session
/// key travels as the `Authorization` header — same value real
/// requests pin with — so it never lands in URLs or logs.
#[derive(Debug, Deserialize)]
struct PreviewParams {
    model: Option<String>,
    top: Option<usize>,
}

/// Read-only routing preview JSON (Iter 29): ordered top-N candidates
/// with every stored column plus sticky-vs-fresh picks. Never stores
/// a pin; the raw Authorization value is hashed in memory only.
async fn ui_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<PreviewParams>,
) -> Response {
    let Some(model_field) = params.model.filter(|m| !m.trim().is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": {
                "message": "missing ?model=<model-id>",
                "type": "invalid_request_error",
                "code": "invalid_model",
            }})),
        )
            .into_response();
    };
    let Some(router) = state.router_handle() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": {
                "message": "routing off: no catalog loaded",
                "type": "invalid_request_error",
                "code": "no_router",
            }})),
        )
            .into_response();
    };
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let preview = router.preview(&model_field, auth, params.top.unwrap_or(3));
    (StatusCode::OK, Json(preview_json(&preview))).into_response()
}

/// Preview JSON shape: stored columns plus computed routing facts.
fn preview_json(preview: &routing::Preview) -> serde_json::Value {
    let decision = |d: &Option<routing::PreviewDecision>| {
        d.as_ref().map(|d| {
            serde_json::json!({
                "model_id": d.model_id,
                "provider": d.provider,
                "tag": d.tag,
                "precision_class": d.precision_class,
                "sticky": d.sticky,
            })
        })
    };
    serde_json::json!({
        "model": preview.model_field,
        "session_namespace": preview.session_namespace,
        "session_key_hash": preview.session_key_hash,
        "session_key_hash_hex": preview.session_key_hash.map(|h| format!("{h:016x}")),
        "fresh": decision(&preview.fresh),
        "sticky": decision(&preview.sticky),
        "sticky_hit": preview.sticky_hit,
        "rejection": preview.rejection.as_ref().map(|r| serde_json::json!({
            "code": r.code, "message": r.message,
        })),
        "note": preview.note,
        "candidates": preview.candidates.iter().map(|c| serde_json::json!({
            "model_id": c.model_id,
            "provider": c.provider,
            "tag": c.tag,
            "price_prompt": c.price_prompt,
            "price_completion": c.price_completion,
            "price_cache_read": c.price_cache_read,
            "price_cache_write": c.price_cache_write,
            "effective_cost": c.effective_cost,
            "promo_discount": c.promo_discount,
            "precision_class": c.precision_class,
            "quality_tier": c.quality_tier,
            "quality_score": c.quality_score,
            "supports_tools": c.supports_tools,
            "supports_decisions": c.supports_decisions,
            "free_eligible": c.free_eligible,
            "throttled": c.throttled,
            "availability": if c.throttled { "cool-down" } else { "available" },
            "rank_index": c.rank_index,
            "sticky_match": c.sticky_match,
        })).collect::<Vec<_>>(),
    })
}

/// Static debug-UI shell: a tab bar over five panels (overview,
/// models, inspector, classifier, metrics) plus the tab-switching
/// script. `{{…}}` placeholders carry the server-rendered panels; the
/// inspector script stays with its panel builder below, next to the
/// fixed-combo ids it must match by index. Read-only, rendered on
/// demand from short read locks; nothing here runs on the request
/// path.
const UI_TEMPLATE: &str = include_str!("../templates/ui.html");

async fn ui_page(State(state): State<AppState>) -> Response {
    fn esc(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    /// Class display: unclassified ("unknown") renders as `-`, like missing.
    fn class(value: &Option<String>) -> String {
        match value.as_deref() {
            None | Some("unknown") => "-".to_string(),
            Some(v) => esc(v),
        }
    }

    /// Display unit is $/M tokens (×1e6): two decimals by default,
    /// extra decimals only when needed — shared with catalog-ui.
    fn money(value: Option<f64>) -> String {
        match value {
            None => "-".to_string(),
            Some(v) => pennyrouter_catalog::fmt_price_per_m(v),
        }
    }
    fn text(value: &Option<String>) -> String {
        value.as_deref().map(esc).unwrap_or_else(|| "-".to_string())
    }
    // One buffer per tab panel; each region below shadows `page` to
    // its buffer so the existing builders move over unchanged.
    let mut overview = String::new();
    let mut models_panel = String::new();
    let mut inspector = String::new();
    let mut classifier = String::new();
    let mut metrics_panel = String::new();
    let router = state.router_handle();
    {
        let page = &mut overview;
        match router.as_ref() {
            None => {
                page.push_str("<p>Routing: <strong>off</strong> (no catalog).</p>");
                models_panel.push_str("<p>Routing off: no catalog loaded.</p>");
            }
            Some(router) => {
                let summary = router.ui_summary();
                let models = summary.models.len();
                let offerings: usize = summary.models.iter().map(|(_, _, o)| o.len()).sum();
                page.push_str(&format!(
                "<p>Snapshot: <strong>{}</strong> — {models} models, {offerings} offerings.</p>",
                summary
                    .artifact_version
                    .as_deref()
                    .map(esc)
                    .unwrap_or_else(|| "unversioned".to_string()),
            ));
                page.push_str("<h3>Virtual menu</h3><ul>");
                for id in routing::VIRTUAL_MODELS {
                    page.push_str(&format!("<li>{}</li>", esc(id)));
                }
                page.push_str("</ul>");
                // Popularity pins: top OpenRouter models (menu pins, served
                // passthrough) and hot apps, with the citation timestamp.
                // The Data API publishes no per-app models, so apps are
                // informational only — the gateway never keys off caller
                // identity (no Referer/title collection).
                let (popular, apps, as_of) = router.popularity();
                if !popular.is_empty() || !apps.is_empty() {
                    page.push_str("<h3>Popular</h3>");
                    if !popular.is_empty() {
                        page.push_str("<p>Top models by tokens, trailing week:</p><ul>");
                        for slug in &popular {
                            page.push_str(&format!("<li>openrouter/{}</li>", esc(slug)));
                        }
                        page.push_str("</ul>");
                    }
                    if !apps.is_empty() {
                        page.push_str(
                            "<p>Hot apps (informational — no per-app models published):</p><ul>",
                        );
                        for app in &apps {
                            page.push_str(&format!("<li>{}</li>", esc(app)));
                        }
                        page.push_str("</ul>");
                    }
                    page.push_str(&format!(
                        "<p><small>Popularity: OpenRouter Data API{}.</small></p>",
                        as_of
                            .as_deref()
                            .map(|when| format!(", as of {}", esc(when)))
                            .unwrap_or_default(),
                    ));
                }
                {
                    let page = &mut models_panel;
                    for (model_id, coding, entries) in &summary.models {
                        let coding = coding.map(|n| format!(" · coding {n}")).unwrap_or_default();
                        page.push_str(&format!("<h3>{}{}</h3>", esc(model_id), coding));
                        page.push_str(
                            "<table><tr><th>provider</th><th>tag</th>\
                    <th>prompt $/M</th><th>completion $/M</th><th>class</th>\
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
                                class(&entry.precision_class),
                                text(&entry.quality_tier),
                                entry
                                    .quality_score
                                    .map(|s| format!("{s:.3}"))
                                    .unwrap_or_else(|| "-".to_string()),
                            ));
                        }
                        page.push_str("</table>");
                        if let Some((_, classes)) =
                            summary.ranking.iter().find(|(id, _)| id == model_id)
                        {
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
                }
                if !summary.free_offers.is_empty() {
                    page.push_str("<h3>Free offers</h3><table>\
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
    }
    {
        let page = &mut inspector;
        page.push_str(
            "<p>Paste the exact <code>Authorization</code> header value a client sends \
        (for example <code>Bearer sk-…</code>). The page sends it as the \
        <code>Authorization</code> header to <code>/ui/preview</code>, where the gateway \
        hashes it with the same seeded hash the request path pins with. The raw key is \
        never logged and never stored; previews never create pins. Empty means anonymous \
        (no session, always the fresh pick). Plain-request view: tools, budget, and \
        only/ignore constraints are not applied, so a capped request can fail closed \
        where the preview shows a pick.</p>",
        );
        page.push_str(
        "<p><label>Session key: \
        <input type=\"password\" id=\"inspector-key\" size=\"40\" autocomplete=\"off\" \
        placeholder=\"Bearer sk-…\"> </label>\
        <button id=\"inspector-refresh\" type=\"button\" class=\"action\">Refresh fixed with key</button> \
        <span id=\"inspector-hash\"></span></p>",
    );
        page.push_str("<h3>Fixed combinations (top 3)</h3>");
        page.push_str("<div class=\"table-scroll\">");
        if let Some(router) = router.as_ref() {
            fn bool3(value: Option<bool>) -> &'static str {
                match value {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "-",
                }
            }
            for (i, id) in routing::FIXED_PREVIEW_IDS.iter().enumerate() {
                let preview = router.preview(id, None, 3);
                page.push_str(&format!("<h4>{}</h4>", esc(id)));
                match &preview.fresh {
                    Some(fresh) => page.push_str(&format!(
                        "<p id=\"fixed-{i}-pick\">Fresh pick: <strong>{}</strong> ({})</p>",
                        esc(&fresh.provider),
                        esc(&fresh.model_id),
                    )),
                    None => page.push_str(&format!(
                        "<p id=\"fixed-{i}-pick\">No eligible offering{}</p>",
                        preview
                            .rejection
                            .as_ref()
                            .map(|r| format!(" ({})", esc(r.code)))
                            .unwrap_or_default(),
                    )),
                }
                page.push_str(
                "<table><thead><tr><th>#</th><th>provider</th><th>backend model</th><th>tag</th>\
                <th>prompt $/M</th><th>completion $/M</th><th>cache-read $/M</th>\
                <th>cache-write $/M</th><th>effective $/M</th><th>promo</th><th>class</th>\
                <th>tier</th><th>score</th><th>tools</th><th>decisions</th>\
                <th>free</th><th>availability</th><th>rank</th></tr></thead>",
            );
                page.push_str(&format!("<tbody id=\"fixed-{i}-rows\">"));
                for (n, c) in preview.candidates.iter().enumerate() {
                    page.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
                    <td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
                    <td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
                    <td>{}</td><td>{}</td><td>{}</td></tr>",
                        n + 1,
                        esc(&c.provider),
                        esc(&c.model_id),
                        c.tag.as_deref().map(esc).unwrap_or_else(|| "-".to_string()),
                        money(c.price_prompt),
                        money(c.price_completion),
                        money(c.price_cache_read),
                        money(c.price_cache_write),
                        money(c.effective_cost),
                        c.promo_discount
                            .map(|d| format!("{d:.2}"))
                            .unwrap_or_else(|| "-".to_string()),
                        class(&c.precision_class),
                        text(&c.quality_tier),
                        c.quality_score
                            .map(|s| format!("{s:.3}"))
                            .unwrap_or_else(|| "-".to_string()),
                        bool3(c.supports_tools),
                        bool3(c.supports_decisions),
                        if c.free_eligible { "yes" } else { "no" },
                        if c.throttled {
                            "cool-down"
                        } else {
                            "available"
                        },
                        c.rank_index
                            .map(|r| r.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                    ));
                }
                page.push_str("</tbody></table>");
            }
        } else {
            page.push_str("<p>Routing off: no catalog loaded.</p>");
        }
        page.push_str("</div>");
        page.push_str(
            "<h3>Custom model</h3>\
        <p><label>Model: <input id=\"inspector-model\" size=\"48\" \
        placeholder=\"openrouter/pair/low\"></label> \
        <label>Top: <input id=\"inspector-top\" size=\"4\" value=\"5\"></label> \
        <button id=\"inspector-go\" type=\"button\" class=\"action\">Inspect</button></p>\
        <div id=\"inspector-result\"></div>",
        );
        page.push_str(
        "<script>\
        function insEsc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;')\
        .replace(/>/g,'&gt;').replace(/\"/g,'&quot;');}\
        function insMoney(v){if(v===null||v===undefined)return '-';var m=Number(v)*1e6;if(m===0)return '0';return Math.abs(m)>=0.0001?m.toFixed(4):m.toExponential(2);}\
        function insBool(v){return (v===null||v===undefined)?'-':(v?'yes':'no');}\
        async function insFetch(model,top){\
        var key=document.getElementById('inspector-key').value;\
        var headers={};if(key){headers['Authorization']=key;}\
        var res=await fetch('/ui/preview?model='+encodeURIComponent(model)+'&top='+top,{headers:headers});\
        return res.json();}\
        function insRow(c,i){\
        return '<tr><td>'+(i+1)+'</td><td>'+insEsc(c.provider)+'</td><td>'+insEsc(c.model_id)+'</td><td>'\
        +(c.tag?insEsc(c.tag):'-')+'</td><td>'+insMoney(c.price_prompt)+'</td><td>'+insMoney(c.price_completion)\
        +'</td><td>'+insMoney(c.price_cache_read)+'</td><td>'+insMoney(c.price_cache_write)+'</td><td>'\
        +insMoney(c.effective_cost)+'</td><td>'+((c.promo_discount===null||c.promo_discount===undefined)?'-':Number(c.promo_discount).toFixed(2))\
        +'</td><td>'+(c.precision_class&&c.precision_class!=='unknown'?insEsc(c.precision_class):'-')+'</td><td>'\
        +(c.quality_tier?insEsc(c.quality_tier):'-')+'</td><td>'\
        +((c.quality_score===null||c.quality_score===undefined)?'-':Number(c.quality_score).toFixed(3))\
        +'</td><td>'+insBool(c.supports_tools)+'</td><td>'+insBool(c.supports_decisions)+'</td><td>'\
        +(c.free_eligible?'yes':'no')+'</td><td>'+insEsc(c.availability)+'</td><td>'\
        +((c.rank_index===null||c.rank_index===undefined)?'-':c.rank_index)+'</td><td>'\
        +(c.sticky_match?'pinned':'-')+'</td></tr>';}\
        function insTable(data){\
        var h='<table><thead><tr><th>#</th><th>provider</th><th>backend model</th><th>tag</th>'\
        +'<th>prompt $/M</th><th>completion $/M</th><th>cache-read $/M</th><th>cache-write $/M</th>'\
        +'<th>effective $/M</th><th>promo</th><th>class</th><th>tier</th><th>score</th><th>tools</th>'\
        +'<th>decisions</th><th>free</th><th>availability</th><th>rank</th><th>session</th></tr></thead><tbody>';\
        for(var i=0;i<data.candidates.length;i++){h+=insRow(data.candidates[i],i);}\
        return h+'</tbody></table>';}\
        function insHead(data){\
        var h='<p>Namespace <code>'+insEsc(data.session_namespace)+'</code>';\
        if(data.session_key_hash_hex){h+=' — session hash <code>'+insEsc(data.session_key_hash_hex)+'</code>';}\
        h+='</p>';\
        if(data.fresh){h+='<p>Fresh (no session): <strong>'+insEsc(data.fresh.provider)+'</strong> ('+insEsc(data.fresh.model_id)+')</p>';}\
        if(data.sticky){h+='<p>With this key'+(data.sticky_hit?' (sticky pin wins)':' (no pin, same as fresh)')\
        +': <strong>'+insEsc(data.sticky.provider)+'</strong> ('+insEsc(data.sticky.model_id)+')</p>';}\
        if(data.rejection){h+='<p>Rejected: <code>'+insEsc(data.rejection.code)+'</code> — '+insEsc(data.rejection.message)+'</p>';}\
        if(data.note){h+='<p>'+insEsc(data.note)+'</p>';}\
        return h;}\
        document.getElementById('inspector-go').onclick=async function(){\
        var model=document.getElementById('inspector-model').value;\
        var top=parseInt(document.getElementById('inspector-top').value,10)||5;\
        var box=document.getElementById('inspector-result');\
        try{var data=await insFetch(model,top);box.innerHTML=insHead(data)+insTable(data);\
        var hash=document.getElementById('inspector-hash');\
        hash.textContent=data.session_key_hash_hex?('hash '+data.session_key_hash_hex):'';}\
        catch(e){box.textContent='preview failed: '+e;}};\
        document.getElementById('inspector-refresh').onclick=async function(){\
        var ids=[];var els=document.querySelectorAll('[id^=\"fixed-\"][id$=\"-rows\"]');\
        for(var k=0;k<els.length;k++){ids.push(k);}\
        var models=['openrouter/pair/free','openrouter/pair/low','openrouter/pair/think',\
        'openrouter/pair/decision','openrouter/pair/decision/free','openrouter/pair/privacy/free',\
        'openrouter/pair/privacy/low','openrouter/pair/privacy/think','openrouter/pair/privacy/decision',\
        'openrouter/pair/privacy/decision/free'];\
        for(var i=0;i<models.length;i++){\
        try{var data=await insFetch(models[i],3);\
        var rows=document.getElementById('fixed-'+i+'-rows');\
        var pick=document.getElementById('fixed-'+i+'-pick');\
        if(rows){var h='';for(var j=0;j<data.candidates.length;j++){var c=data.candidates[j];\
        h+='<tr><td>'+(j+1)+'</td><td>'+insEsc(c.provider)+'</td><td>'+insEsc(c.model_id)+'</td><td>'\
        +(c.tag?insEsc(c.tag):'-')+'</td><td>'+insMoney(c.price_prompt)+'</td><td>'+insMoney(c.price_completion)\
        +'</td><td>'+insMoney(c.price_cache_read)+'</td><td>'+insMoney(c.price_cache_write)+'</td><td>'\
        +insMoney(c.effective_cost)+'</td><td>'+((c.promo_discount===null||c.promo_discount===undefined)?'-':Number(c.promo_discount).toFixed(2))\
        +'</td><td>'+(c.precision_class&&c.precision_class!=='unknown'?insEsc(c.precision_class):'-')+'</td><td>'\
        +(c.quality_tier?insEsc(c.quality_tier):'-')+'</td><td>'\
        +((c.quality_score===null||c.quality_score===undefined)?'-':Number(c.quality_score).toFixed(3))\
        +'</td><td>'+insBool(c.supports_tools)+'</td><td>'+insBool(c.supports_decisions)+'</td><td>'\
        +(c.free_eligible?'yes':'no')+'</td><td>'+insEsc(c.availability)+'</td><td>'\
        +((c.rank_index===null||c.rank_index===undefined)?'-':c.rank_index)+'</td></tr>';}\
        rows.innerHTML=h;}\
        if(pick){if(data.sticky){pick.innerHTML='With this key'+(data.sticky_hit?' (sticky)':' (fresh)')\
        +': <strong>'+insEsc(data.sticky.provider)+'</strong> ('+insEsc(data.sticky.model_id)+')';}}\
        if(data.session_key_hash_hex){document.getElementById('inspector-hash').textContent='hash '+data.session_key_hash_hex;}}\
        catch(e){}}};\
        </script>",
    );
    }
    {
        let page = &mut classifier;
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
    }
    {
        let page = &mut metrics_panel;
        page.push_str("<pre>");
        page.push_str(&esc(&state.metrics.render()));
        page.push_str("</pre>");
    }
    let body = UI_TEMPLATE
        .replace("{{overview}}", &overview)
        .replace("{{models}}", &models_panel)
        .replace("{{inspector}}", &inspector)
        .replace("{{classifier}}", &classifier)
        .replace("{{metrics}}", &metrics_panel);
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Extensive model menu in OpenAI list shape, served locally instead
/// of proxied: the static virtual ids first (small tiers first), then
/// popular OpenRouter pins (`openrouter/<slug>`, top tokens over the
/// trailing week — served passthrough, no routing change), then
/// operator-nominated `modal/…` / `baseten/…` ids, each only when its
/// provider is configured. Virtual picks are priced and filtered
/// live, so the virtual section stays static by design; pins ride the
/// catalog artifact (empty on old or keyless syncs).
async fn models_list(State(state): State<AppState>) -> Response {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut ids: Vec<String> = crate::routing::VIRTUAL_MODELS
        .iter()
        .map(|id| id.to_string())
        .collect();
    if let Some(router) = state.router.as_ref() {
        let (popular, _, _) = router.popularity();
        ids.extend(popular.iter().map(|slug| format!("openrouter/{slug}")));
    }
    for (provider, models) in [
        ("modal", &state.modal_models),
        ("baseten", &state.baseten_models),
    ] {
        if state.pinned_upstream(provider).is_some() {
            ids.extend(models.iter().map(|id| format!("{provider}/{id}")));
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "object": "list",
            "data": ids
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

/// Routed decisions (`POST /v1/decisions`, Iter 25): admit, guard,
/// optionally route (model rewritten to the picked backend id —
/// nothing else; the decisions API owns its schema), then forward
/// to the upstream alpha decisions endpoint. Same resilience shape
/// as chat — one retry on another capable provider, then the
/// configured fallback leg — except tier fallback stays within the
/// decisions shape: only the `decision` target is served, any other
/// fallback target fails closed once the free leg actually needs it.
async fn routed_decisions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
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

    // I04: parse once — virtual + fallback facts derive from this
    // one parse instead of re-parsing the body per question.
    let parsed: Option<serde_json::Value> = serde_json::from_slice(&buffered).ok();
    let model: Option<String> = parsed.as_ref().and_then(|json| {
        json.get("model")
            .and_then(|model| model.as_str())
            .map(str::to_string)
    });
    let virtual_model = model
        .as_ref()
        .and_then(|model| routing::parse_virtual(model));
    // An unusable `minindex` suffix (Iter 33) is still a gateway-owned
    // id: it must fail closed, never pass through.
    let is_virtual = virtual_model.is_some()
        || model
            .as_deref()
            .is_some_and(|model| routing::invalid_virtual(model).is_some());
    // Q06-B: privacy virtual ids fail closed on the decisions route.
    // The decisions API has no verified zero-retention field, so
    // serving would send the payload on a promise the gateway cannot
    // keep. Pin a model directly (`modal/…`, `baseten/…`, or
    // `openrouter/pair/model/…`) when the caller accepts the
    // upstream's default policy — explicit choice, not a silent one.
    if virtual_model.as_ref().is_some_and(|v| v.privacy) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "privacy_unsupported_on_decisions",
            "privacy virtual ids are not served on the decisions route: \
             no verified zero-retention field exists for the decisions API; \
             pin a model directly if you accept the upstream default policy"
                .to_string(),
        );
    }
    let fallback_request: Option<(String, String)> = match (&model, &virtual_model) {
        (Some(model), Some(virtual_model)) => virtual_model
            .fallback
            .clone()
            .map(|target| (model.clone(), target)),
        _ => None,
    };
    // Routed virtual requests keep the original body for one retry
    // (Iter 13): moved, never cloned (the routed arm drops `buffered`
    // anyway).
    let (forwarded_body, decision, retry_body) = match &state.router {
        Some(router) => match router.route_decision_request(&headers, &buffered) {
            Ok(Some((rewritten, decision))) => {
                note_minindex(&state, model.as_deref().unwrap_or_default(), "ok");
                let retry_body = is_virtual.then_some(buffered);
                (rewritten, Some(decision), retry_body)
            }
            Ok(None) => (buffered, None, None),
            Err(rejection) => {
                if is_virtual {
                    note_minindex(
                        &state,
                        model.as_deref().unwrap_or_default(),
                        if rejection.code == "invalid_model" {
                            "invalid"
                        } else {
                            "no_eligible"
                        },
                    );
                    return rejection_response(&rejection);
                }
                (buffered, None, None)
            }
        },
        None => (buffered, None, None),
    };
    // The decisions API lives outside `/v1`: the alpha path appends
    // to the API root (a custom base with a trailing `/v1` is
    // normalized first, so it never doubles).
    let decisions_root = normalize_pinned_base(&state.upstream_base_url);
    let decisions_base = decisions_root.trim_end_matches('/');
    let decisions_path = "/alpha/decisions";
    let mut response = forward_upstream(
        &state,
        decisions_base,
        permit,
        &Method::POST,
        decisions_path,
        &headers,
        forwarded_body,
    )
    .await;
    if let Some(decision) = &decision {
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            if let Some(router) = &state.router {
                note_throttled_pair(router, &decision.model_id, &decision.provider, &response);
            }
            state.metrics.throttled();
        }
    }
    // Owned copy for the post-retry leg: the retry block moves
    // `retry_body`. Cloned only when a fallback leg can actually run
    // (retryable response + configured target) — never eagerly.
    let fallback_body: Option<Vec<u8>> =
        if is_retryable(response.status()) && fallback_request.is_some() {
            retry_body.clone()
        } else {
            None
        };
    // Tracks whether the retry leg below ran: a retryable retry
    // response falls through to the fallback leg instead of returning.
    let mut retried = false;
    if is_retryable(response.status()) {
        if let (Some(router), Some(retry_body), Some(decision)) =
            (&state.router, retry_body, &decision)
        {
            if let Some((rewritten, retry_decision)) =
                router.route_decision_retry(&headers, &retry_body, &decision.provider)
            {
                let permit = match acquire_upstream_slot(&state).await {
                    Some(permit) => permit,
                    None => {
                        state.metrics.load_shed();
                        return service_unavailable();
                    }
                };
                response = forward_upstream(
                    &state,
                    decisions_base,
                    permit,
                    &Method::POST,
                    decisions_path,
                    &headers,
                    rewritten,
                )
                .await;
                response = with_routed_headers(response, &retry_decision);
                if response.status() == StatusCode::TOO_MANY_REQUESTS {
                    note_throttled_pair(
                        router,
                        &retry_decision.model_id,
                        &retry_decision.provider,
                        &response,
                    );
                    state.metrics.throttled();
                }
                retried = true;
                if !(is_retryable(response.status()) && fallback_request.is_some()) {
                    return response;
                }
            }
        }
    }
    if is_retryable(response.status()) {
        if let (Some(router), Some((model_field, target)), Some(fallback_body)) =
            (&state.router, &fallback_request, &fallback_body)
        {
            let auth = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            let ignore = decision.as_ref().map(|d| d.provider.as_str());
            match router.route_decision_fallback(fallback_body, model_field, target, auth, ignore) {
                Err(rejection) => return rejection_response(&rejection),
                Ok(Some((rewritten, fb_decision))) => {
                    let permit = match acquire_upstream_slot(&state).await {
                        Some(permit) => permit,
                        None => {
                            state.metrics.load_shed();
                            return service_unavailable();
                        }
                    };
                    response = forward_upstream(
                        &state,
                        decisions_base,
                        permit,
                        &Method::POST,
                        decisions_path,
                        &headers,
                        rewritten,
                    )
                    .await;
                    if response.status() == StatusCode::TOO_MANY_REQUESTS {
                        note_throttled_pair(
                            router,
                            &fb_decision.model_id,
                            &fb_decision.provider,
                            &response,
                        );
                        state.metrics.throttled();
                    }
                    response = with_routed_headers(response, &fb_decision);
                    return response;
                }
                Ok(None) => {}
            }
            // No fallback route after a retry: the retry response (and
            // its provider header) stands — falling through would stamp
            // the first leg's provider onto another leg's response.
            if retried {
                return response;
            }
        }
    }
    if let Some(decision) = decision {
        response = with_routed_headers(response, &decision);
    }
    response
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

    // I04: parse once — every model fact below (pinned, smart,
    // virtual, fallback target) derives from this one parse instead
    // of re-parsing the body per question.
    let parsed: Option<serde_json::Value> = serde_json::from_slice(&buffered).ok();
    let model: Option<String> = parsed.as_ref().and_then(|json| {
        json.get("model")
            .and_then(|model| model.as_str())
            .map(str::to_string)
    });
    let virtual_model = model
        .as_ref()
        .and_then(|model| routing::parse_virtual(model));
    // An unusable `minindex` suffix (Iter 33) is still a gateway-owned
    // id: it must fail closed, never pass through.
    let is_virtual = virtual_model.is_some()
        || model
            .as_deref()
            .is_some_and(|model| routing::invalid_virtual(model).is_some());
    // Free-fallback chains (Iter 21) resolve here: (original model id,
    // fallback tier) for the post-retry leg below.
    let fallback_request: Option<(String, String)> = match (&model, &virtual_model) {
        (Some(model), Some(virtual_model)) => virtual_model
            .fallback
            .clone()
            .map(|target| (model.clone(), target)),
        _ => None,
    };
    // Provider-pinned direct routes (Iter 16): `modal/<model>` and
    // `baseten/<model>` bypass the catalog and address that
    // provider's upstream, with or without a router. Pure BYOK: the
    // caller's Authorization header is forwarded unchanged.
    if let Some(model) = &model {
        // B16/Q01-A: a pinned prefix with no model fails closed —
        // never proxied untouched to the default upstream. Checked
        // before `split_pinned` so a whitespace remainder can't slip
        // through as a model id either.
        if routing::is_empty_pinned(model) {
            return rejection_response(&routing::RouteRejection {
                code: "model_not_provided",
                message: format!("{model:?} names a pinned provider but no model"),
            });
        }
        if let Some((provider, _)) = routing::split_pinned(model) {
            return forward_pinned(&state, permit, &headers, uri, buffered, model, provider).await;
        }
    }
    // Smart classifier ids (Iter 26): the `/fallback/` suffix never
    // composes with smart — fail closed instead of leaking a
    // gateway-owned id upstream. Other smart ids run the classifier
    // leg first, then the normal tier dispatch with no session pin.
    if let Some(model) = &model {
        if routing::is_smart_fallback(model) {
            return rejection_response(&routing::RouteRejection {
                code: "invalid_model",
                message: format!(
                    "{model:?} is not a virtual model: smart ids take no /fallback/ suffix"
                ),
            });
        }
        if let Some(smart) = routing::parse_smart(model) {
            // Same native-alias strip as the normal path below: smart
            // ids on `/api/v1/…` forward as `/v1/…`, never doubled.
            let path = uri
                .path_and_query()
                .map(|path_and_query| strip_api_prefix(path_and_query.as_str()))
                .unwrap_or_else(|| "/v1/chat/completions".to_string());
            return smart_completion(&state, &headers, permit, &path, buffered, model, smart).await;
        }
    }
    // Routing rewrites recognized requests; anything unrecognized (or no
    // router configured) forwards untouched. Virtual-model rejections
    // fail closed (Iter 14): a gateway-owned id must never leak upstream,
    // where it would 404 opaquely. Routed virtual requests keep the
    // original body for one retry (Iter 13): the permit is
    // per-attempt, the body is reusable — moved, never cloned (the
    // routed arm drops `buffered` anyway).
    let (forwarded_body, decision, retry_body) = match &state.router {
        Some(router) => match router.route_request_or_reject(&headers, &buffered) {
            Ok(Some((rewritten, decision))) => {
                note_minindex(&state, model.as_deref().unwrap_or_default(), "ok");
                let retry_body = is_virtual.then_some(buffered);
                (rewritten, Some(decision), retry_body)
            }
            Ok(None) => (buffered, None, None),
            Err(rejection) => {
                if is_virtual {
                    note_minindex(
                        &state,
                        model.as_deref().unwrap_or_default(),
                        if rejection.code == "invalid_model" {
                            "invalid"
                        } else {
                            "no_eligible"
                        },
                    );
                    return rejection_response(&rejection);
                }
                (buffered, None, None)
            }
        },
        None => (buffered, None, None),
    };
    // OpenRouter-native aliases (Iter 25) arrive as `/api/v1/…`; the
    // upstream keeps the `/v1/…` shape so the `/api` base never doubles.
    let path = uri
        .path_and_query()
        .map(|path_and_query| strip_api_prefix(path_and_query.as_str()))
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
                note_throttled_pair(router, &decision.model_id, &decision.provider, &response);
            }
            state.metrics.throttled();
        }
    }
    // Availability (Iter 13): one retry on a different provider for
    // virtual requests when the first attempt is retryable. Non-virtual
    // requests keep exactly-once semantics.
    // `retry_body` exists only for routed virtual requests (see
    // above), so reaching the retry block already implies virtual.
    //
    // Owned copy for the post-retry leg: the retry block moves
    // `retry_body`. Cloned only when a fallback leg can actually run
    // (retryable response + configured target) — never eagerly.
    let fallback_body: Option<Vec<u8>> =
        if is_retryable(response.status()) && fallback_request.is_some() {
            retry_body.clone()
        } else {
            None
        };
    // Tracks whether the retry leg below ran: a retryable retry
    // response falls through to the fallback leg instead of returning.
    let mut retried = false;
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
                response = with_routed_headers(response, &retry_decision);
                if response.status() == StatusCode::TOO_MANY_REQUESTS {
                    note_throttled_pair(
                        router,
                        &retry_decision.model_id,
                        &retry_decision.provider,
                        &response,
                    );
                    state.metrics.throttled();
                }
                retried = true;
                if !(is_retryable(response.status()) && fallback_request.is_some()) {
                    return response;
                }
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
                    note_throttled_pair(
                        router,
                        &fb_decision.model_id,
                        &fb_decision.provider,
                        &response,
                    );
                    state.metrics.throttled();
                }
                response = with_routed_headers(response, &fb_decision);
                return response;
            }
            // No fallback route after a retry: the retry response (and
            // its provider header) stands — falling through would stamp
            // the first leg's provider onto another leg's response.
            if retried {
                return response;
            }
        }
    }
    if let Some(decision) = decision {
        response = with_routed_headers(response, &decision);
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
    // Auth for this leg only (Q08, BYOK-only): the per-request
    // classifier key, else the caller's key, else unauthenticated.
    // A foreign `CLASSIFIER_BASE_URL` therefore receives the caller's
    // key by model-choice consent, like pinned routes.
    let auth = classifier_auth(headers);
    let url = classifier::chat_url(&config.base_url);
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
        let bytes = collect_capped_body(response, MAX_CLASSIFIER_BYTES).await?;
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
        Ok(Err("too_large")) => {
            return fail(
                ClassifierResult::Invalid,
                "classifier reply exceeds the size bound".to_string(),
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
            return fail(
                ClassifierResult::AuthError,
                format!("classifier upstream rejected the request credentials (HTTP {code})"),
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
    // `bytes` is already capped by the collector above; anything past
    // the bound failed as `Invalid` before the status check.
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
                        note_minindex(state, model_field, "no_eligible");
                        return with_smart_headers(rejection_response(&rejection), &ctx);
                    }
                };
            note_minindex(state, model_field, "ok");
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
                        with_routed_headers(response, &retry_decision),
                        &ctx,
                    );
                }
            }
            with_smart_headers(with_routed_headers(response, &decision), &ctx)
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
                    with_smart_headers(with_routed_headers(response, &decision), &ctx)
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

/// Count a floored request (Iter 33): bounded labels only.
fn note_minindex(state: &AppState, model_field: &str, result: &'static str) {
    if let Some(floor) = routing::minindex_of(model_field) {
        state
            .metrics
            .minindex_request(floor, routing::tier_label(model_field), result);
    }
}

/// Attach the routed diagnostic headers: the serving provider, plus
/// the AA coding-index floor when the request carried one (Iter 33).
fn with_routed_headers(mut response: Response, decision: &routing::Decision) -> Response {
    if let Ok(value) = decision.provider.parse() {
        response
            .headers_mut()
            .insert(routing::PROVIDER_HEADER, value);
    }
    if let Some(floor) = decision.minindex {
        if let Ok(value) = floor.to_string().parse() {
            response.headers_mut().insert(
                axum::http::HeaderName::from_static(routing::MININDEX_HEADER),
                value,
            );
        }
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

/// Record a 429 cool-down for the serving (model, provider) pair from
/// the upstream's own headers (Iter 21). Shared by the first, retry, and
/// fallback legs of both chat and decisions paths; callers count the
/// metric themselves so no-router passthrough still counts.
fn note_throttled_pair(
    router: &routing::Router,
    model_id: &str,
    provider: &str,
    response: &Response,
) {
    let headers = response.headers();
    router.note_throttled(
        model_id,
        provider,
        routing::throttle_until(
            headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            headers
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok()),
        ),
    );
}

/// Drop-in fallback (Iter 8): any standard endpoint without a custom
/// handler is proxied upstream untouched and logged with its full path,
/// so no client call ever goes silent. No routing or rewriting applies.
///
/// OpenRouter-native paths (Iter 25, `/api/…`) compose against the
/// upstream root — the configured base without its `/api` route
/// prefix — so account/catalog paths stop doubling `/api`. Management
/// writes stay denied unless `ALLOW_MANAGEMENT_PROXY=1`.
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
    if !state.allow_management_proxy && !is_proxy_allowed(&parts.method, &path) {
        return management_forbidden();
    }
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
    // Native paths keep their full path and compose against the
    // upstream root (`base` minus `/api`); everything else keeps the
    // historical `base + path` composition.
    let (fallback_base, upstream_path) = if is_openrouter_native_path(&path) {
        (upstream_root(&state.upstream_base_url), path)
    } else {
        (state.upstream_base_url.clone(), path)
    };
    forward_upstream(
        &state,
        &fallback_base,
        permit,
        &parts.method,
        &upstream_path,
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

/// Path portion of a `path_and_query` string (drops `?...`).
fn request_path(path_and_query: &str) -> &str {
    match path_and_query.find('?') {
        Some(i) => &path_and_query[..i],
        None => path_and_query,
    }
}

/// True for OpenRouter-native gateway paths: `/api` with a `/`
/// boundary (`/api/v1/credits`, never `/apix`). The query string is
/// not part of the decision.
fn is_openrouter_native_path(path_and_query: &str) -> bool {
    let path = request_path(path_and_query).trim_end_matches('/');
    path == "/api" || path.starts_with("/api/")
}

/// Rewrite an OpenRouter-native gateway path for upstream: strip the
/// `/api` prefix, keep the query string. Anything else passes through
/// unchanged (Iter 25).
fn strip_api_prefix(path_and_query: &str) -> String {
    if !is_openrouter_native_path(path_and_query) {
        return path_and_query.to_string();
    }
    let (path, query) = match path_and_query.find('?') {
        Some(i) => (&path_and_query[..i], &path_and_query[i..]),
        None => (path_and_query, ""),
    };
    match path.strip_prefix("/api") {
        Some("") | None => format!("/{query}"),
        Some(rest) => format!("{rest}{query}"),
    }
}

/// Upstream root for native paths (Iter 25): the configured base
/// without its trailing `/api` route prefix. The default
/// `https://openrouter.ai/api` becomes `https://openrouter.ai`, so
/// `root + stripped path` lands on the real endpoint; bases without
/// the suffix (mocks, Ollama) pass through untouched.
fn upstream_root(base: &str) -> String {
    let base = base.trim_end_matches('/');
    base.strip_suffix("/api").unwrap_or(base).to_string()
}

/// Hex digit value for percent-decoding paths (Q10-B). Non-hex
/// after `%` is left literal — it can never match the allowlist, so
/// malformed encodings fail closed.
fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Canonicalize a request path once (Q10-B): percent-decode, collapse
/// repeated `/`, drop a trailing `/`. `..` segments stay literal —
/// resolving them is unnecessary because anything outside the
/// allowlist below is denied either way.
fn canonical_path(path_and_query: &str) -> String {
    let raw = request_path(path_and_query);
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                decoded.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8_lossy(&decoded);
    let mut out = String::with_capacity(decoded.len());
    for part in decoded.split('/').filter(|part| !part.is_empty()) {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// Plain OpenAI-compatible inference writes (Q10-B). These move no
/// money or trust, so the fallback proxy serves them without
/// `ALLOW_MANAGEMENT_PROXY=1`. Compared in bare form: the leading
/// `/api` (native SDK paths) is stripped before matching, so both
/// spellings converge — anything else fails closed.
fn is_allowlisted_write(path: &str) -> bool {
    matches!(
        path,
        "/v1/embeddings"
            | "/v1/completions"
            | "/v1/images/generations"
            | "/v1/audio/speech"
            | "/v1/audio/transcriptions"
            | "/v1/audio/translations"
            | "/v1/moderations"
    )
}

/// True when the fallback proxy may serve the request (Q10-B).
/// Reads always pass through; writes must be a known-safe inference
/// shape (canonicalized first, so `%2e`, `//`, trailing-`/` variants
/// match exactly like the plain form). Unknown writes — key
/// rotation, credit purchases, budget changes, and anything invented
/// later — proxy only with `ALLOW_MANAGEMENT_PROXY=1`.
fn is_proxy_allowed(method: &Method, path_and_query: &str) -> bool {
    if *method == Method::GET || *method == Method::HEAD || *method == Method::OPTIONS {
        return true;
    }
    if *method != Method::POST {
        return false;
    }
    let canonical = canonical_path(path_and_query);
    let bare = canonical
        .strip_prefix("/api")
        .filter(|rest| rest.starts_with('/'))
        .unwrap_or(&canonical);
    is_allowlisted_write(bare)
}

fn management_forbidden() -> Response {
    error_response(
        StatusCode::FORBIDDEN,
        "invalid_request_error",
        "management_proxy_disabled",
        "management writes are disabled by default (set ALLOW_MANAGEMENT_PROXY=1 to proxy them)"
            .to_string(),
    )
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
    // Q04-A: queued requests wait at most the first-chunk budget
    // (30s default), then shed via the existing load-shed path —
    // never indefinitely. Retry/fallback legs re-enter here, so they
    // share the same deadline instead of stacking waits.
    match tokio::time::timeout(
        state.timeouts.first_chunk,
        state.upstream_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(slot)) => Some(UpstreamPermit {
            _slot: slot,
            _queued: Some(queued),
        }),
        // Timeout, or the semaphore closed (never while state lives).
        _ => None,
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
    // Pinned bases carry no route prefix either, so native aliases get
    // the same `/api` strip as the default path (Iter 25).
    let path = uri
        .path_and_query()
        .map(|path_and_query| strip_api_prefix(path_and_query.as_str()))
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
        response = with_routed_headers(response, &decision);
    }
    response
}

async fn forward_upstream(
    state: &AppState,
    base_url: &str,
    permit: UpstreamPermit,
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
        return stream_response(
            status,
            upstream,
            state.timeouts.idle,
            state.metrics.clone(),
            permit,
        );
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

/// Collect a response body chunk by chunk up to `bound` bytes,
/// stopping at the first chunk that overflows instead of buffering
/// the whole body first. Errors are plain tags so callers map them
/// to their own failure shape.
async fn collect_capped_body(
    response: reqwest::Response,
    bound: usize,
) -> Result<Bytes, &'static str> {
    let mut res = response;
    let mut buffered = Vec::new();
    loop {
        match res.chunk().await {
            Err(_) => return Err("transport"),
            Ok(None) => return Ok(Bytes::from(buffered)),
            Ok(Some(data)) => {
                if buffered.len() + data.len() > bound {
                    return Err("too_large");
                }
                buffered.extend_from_slice(&data);
            }
        }
    }
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
/// completion. The upstream concurrency permit rides the same state:
/// the slot stays held until the body ends or errors (C01), so
/// `UPSTREAM_MAX_CONCURRENT` bounds live streams, not just headers.
fn stream_response(
    status: StatusCode,
    upstream: reqwest::Response,
    idle: Duration,
    metrics: Arc<Metrics>,
    permit: UpstreamPermit,
) -> Response {
    let preserved = forwarded_response_headers(upstream.headers());
    let guard = metrics.in_flight_owned();
    let stream = futures_util::stream::unfold(
        (upstream, metrics, guard, false, permit),
        move |(mut res, metrics, guard, failed, permit)| async move {
            if failed {
                return None;
            }
            match tokio::time::timeout(idle, res.chunk()).await {
                Ok(Ok(Some(bytes))) => Some((
                    Ok::<Bytes, reqwest::Error>(bytes),
                    (res, metrics, guard, false, permit),
                )),
                Ok(Ok(None)) => None,
                Err(_) => {
                    metrics.upstream_error(UpstreamErrorKind::Timeout);
                    eprintln!("gateway: upstream stream stalled, ending with error event");
                    Some((
                        Ok::<Bytes, reqwest::Error>(Bytes::from_static(SSE_INTERRUPTED_EVENT)),
                        (res, metrics, guard, true, permit),
                    ))
                }
                Ok(Err(_)) => {
                    metrics.upstream_error(UpstreamErrorKind::Transport);
                    eprintln!("gateway: upstream stream broke, ending with error event");
                    Some((
                        Ok::<Bytes, reqwest::Error>(Bytes::from_static(SSE_INTERRUPTED_EVENT)),
                        (res, metrics, guard, true, permit),
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
        // Long-context hardcap is caller-fixable like a budget
        // violation: shorten the input or pin a model (Q13-C).
        "context_exceeded" => error_response(
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
        // Unsupported id shapes (e.g. smart with a fallback suffix,
        // pinned prefix with no model) are caller-fixable like a bad
        // body (Iter 26, B16).
        "invalid_model" | "model_not_provided" => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            rejection.code,
            rejection.message.clone(),
        ),
        // The classifier leg rejected the credentials: caller-fixable
        // (BYOK-only since Q08 — caller key or classifier-key header),
        // reported as a 502 like other upstream credential failures
        // (Iter 26).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_base_warn_predicate() {
        // S09: cleartext http to a non-loopback host warns.
        assert!(insecure_base_url("http://192.168.1.10:8080/api"));
        assert!(insecure_base_url("http://example.com/api"));
        assert!(insecure_base_url("HTTP://example.com/api"));
        // https and loopback stay silent (local-dev mocks keep working).
        assert!(!insecure_base_url("https://openrouter.ai/api"));
        assert!(!insecure_base_url("http://127.0.0.1:8080/api"));
        assert!(!insecure_base_url("http://127.0.0.1/api"));
        assert!(!insecure_base_url("http://localhost:8080/api"));
        assert!(!insecure_base_url("http://localhost/api"));
        assert!(!insecure_base_url("http://[::1]:8080/api"));
    }

    #[test]
    fn proxy_allowlist_predicate() {
        // Q10-B: reads always pass, whatever the path.
        for method in [&Method::GET, &Method::HEAD, &Method::OPTIONS] {
            assert!(is_proxy_allowed(method, "/api/v1/keys"));
            assert!(is_proxy_allowed(method, "/v1/anything?x=1"));
        }
        // Known-safe inference writes pass in both spellings.
        for path in [
            "/v1/embeddings",
            "/v1/completions",
            "/v1/images/generations",
            "/v1/audio/speech",
            "/v1/audio/transcriptions",
            "/v1/audio/translations",
            "/v1/moderations",
            "/api/v1/embeddings",
            "/api/v1/moderations",
        ] {
            assert!(is_proxy_allowed(&Method::POST, path), "{path}");
        }
        // Canonicalization: encoded, doubled, trailing-slash, and
        // query-string variants decide exactly like the plain form.
        assert!(is_proxy_allowed(&Method::POST, "/v1/%65mbeddings"));
        assert!(is_proxy_allowed(&Method::POST, "//v1//embeddings/"));
        assert!(is_proxy_allowed(&Method::POST, "/v1/embeddings?limit=2"));
        // Everything else fails closed: management shapes, unknown
        // writes, traversal, and non-POST writes.
        for path in [
            "/api/v1/keys",
            "/v1/keys",
            "/api/v1/keys/",
            "/api/v1/%6beys",
            "/api/v1/credits/coinbase",
            "/api/v1/workspaces/w1/budgets",
            "/v1/some-new-thing",
            "/api/v1/keys/%2e%2e",
            "/v1/embeddings/extra",
        ] {
            assert!(!is_proxy_allowed(&Method::POST, path), "{path}");
        }
        assert!(!is_proxy_allowed(&Method::PUT, "/v1/embeddings"));
        assert!(!is_proxy_allowed(&Method::DELETE, "/api/v1/keys/x"));
    }
}
