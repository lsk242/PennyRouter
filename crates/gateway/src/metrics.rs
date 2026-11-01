//! Allocation-lean request metrics (Iter 3).
//!
//! The request path touches atomics (and one tiny mutex for per-status
//! counts) only. Rendering allocates, but `/metrics` is scraped rarely.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

/// Histogram bucket upper bounds, in seconds.
const BUCKETS: [f64; 14] = [
    0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Fixed route slots keep labels bounded and the hot path free of maps.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Health,
    Chat,
    Responses,
    Ui,
    Metrics,
    Other,
}

impl Route {
    pub fn from_path(path: &str) -> Self {
        match path {
            "/" => Self::Ui,
            "/health" => Self::Health,
            "/v1/chat/completions" => Self::Chat,
            "/v1/responses" => Self::Responses,
            "/ui" => Self::Ui,
            "/metrics" => Self::Metrics,
            _ => Self::Other,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Ui => "ui",
            Self::Metrics => "metrics",
            Self::Other => "other",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Health => 0,
            Self::Chat => 1,
            Self::Responses => 2,
            Self::Ui => 3,
            Self::Metrics => 4,
            Self::Other => 5,
        }
    }
}

/// Upstream failure classes for `pennyrouter_upstream_errors_total`.
#[derive(Clone, Copy)]
pub enum UpstreamErrorKind {
    Transport,
    Timeout,
    Status4xx,
    Status5xx,
}

impl UpstreamErrorKind {
    fn label(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Timeout => "timeout",
            Self::Status4xx => "status_4xx",
            Self::Status5xx => "status_5xx",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Transport => 0,
            Self::Timeout => 1,
            Self::Status4xx => 2,
            Self::Status5xx => 3,
        }
    }
}

struct RouteMetrics {
    requests_by_status: Mutex<HashMap<u16, u64>>,
    buckets: [AtomicU64; 14],
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl RouteMetrics {
    fn new() -> Self {
        Self {
            requests_by_status: Mutex::new(HashMap::new()),
            buckets: [const { AtomicU64::new(0) }; 14],
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn observe(&self, status: u16, latency: Duration) {
        if let Ok(mut counts) = self.requests_by_status.lock() {
            *counts.entry(status).or_insert(0) += 1;
        }
        let secs = latency.as_secs_f64();
        for (bound, bucket) in BUCKETS.iter().zip(self.buckets.iter()) {
            if secs <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sum_micros
            .fetch_add(latency.as_micros() as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

/// Classifier outcomes (Iter 26): why a smart request did or did not
/// get a classified tier. Fixed slots keep the label set bounded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClassifierResult {
    Ok,
    Timeout,
    Invalid,
    AuthError,
    Unavailable,
    NotConfigured,
    NoInput,
}

impl ClassifierResult {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Timeout => "timeout",
            Self::Invalid => "invalid",
            Self::AuthError => "auth_error",
            Self::Unavailable => "unavailable",
            Self::NotConfigured => "not_configured",
            Self::NoInput => "no_input",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Ok => 0,
            Self::Timeout => 1,
            Self::Invalid => 2,
            Self::AuthError => 3,
            Self::Unavailable => 4,
            Self::NotConfigured => 5,
            Self::NoInput => 6,
        }
    }
}

/// Classified (or fallback) tier slot for classifier metrics. `None`
/// covers legs that never produced a tier (auth errors, missing
/// config, empty input).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClassifierTier {
    Free,
    Lowest,
    Low,
    Think,
    None,
}

impl ClassifierTier {
    pub fn of(tier: &str) -> Self {
        match tier {
            "free" => Self::Free,
            "lowest" => Self::Lowest,
            "low" => Self::Low,
            "think" => Self::Think,
            _ => Self::None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Lowest => "lowest",
            Self::Low => "low",
            Self::Think => "think",
            Self::None => "none",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Free => 0,
            Self::Lowest => 1,
            Self::Low => 2,
            Self::Think => 3,
            Self::None => 4,
        }
    }
}

const CLASSIFIER_RESULTS: usize = 7;
const CLASSIFIER_TIERS: usize = 5;

/// One read-only snapshot of classifier state for `GET /ui` (Iter 26):
/// counts only, no prompts or verdicts.
pub struct ClassifierSummary {
    pub counts: Vec<(&'static str, &'static str, u64)>,
    pub latency_count: u64,
    pub latency_sum_ms: f64,
    pub fallback_low: u64,
    pub fallback_lowest: u64,
    pub last_error: String,
}

pub struct Metrics {
    routes: [RouteMetrics; 6],
    upstream_errors: [AtomicU64; 4],
    guard_rejections: AtomicU64,
    load_shed: AtomicU64,
    throttled: AtomicU64,
    reload_ok: AtomicU64,
    reload_err: AtomicU64,
    in_flight: AtomicI64,
    request_ids: AtomicU64,
    classifier_total: [AtomicU64; CLASSIFIER_RESULTS * CLASSIFIER_TIERS],
    classifier_buckets: [AtomicU64; 14],
    classifier_sum_micros: AtomicU64,
    classifier_count: AtomicU64,
    smart_fallback: [AtomicU64; 2],
    classifier_last_error: Mutex<String>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            routes: [
                RouteMetrics::new(),
                RouteMetrics::new(),
                RouteMetrics::new(),
                RouteMetrics::new(),
                RouteMetrics::new(),
                RouteMetrics::new(),
            ],
            upstream_errors: [const { AtomicU64::new(0) }; 4],
            guard_rejections: AtomicU64::new(0),
            load_shed: AtomicU64::new(0),
            throttled: AtomicU64::new(0),
            reload_ok: AtomicU64::new(0),
            reload_err: AtomicU64::new(0),
            in_flight: AtomicI64::new(0),
            request_ids: AtomicU64::new(0),
            classifier_total: [const { AtomicU64::new(0) }; CLASSIFIER_RESULTS * CLASSIFIER_TIERS],
            classifier_buckets: [const { AtomicU64::new(0) }; 14],
            classifier_sum_micros: AtomicU64::new(0),
            classifier_count: AtomicU64::new(0),
            smart_fallback: [const { AtomicU64::new(0) }; 2],
            classifier_last_error: Mutex::new(String::new()),
        }
    }

    pub fn observe(&self, route: Route, status: u16, latency: Duration) {
        self.routes[route.index()].observe(status, latency);
    }

    pub fn guard_rejected(&self) {
        self.guard_rejections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn load_shed(&self) {
        self.load_shed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn throttled(&self) {
        self.throttled.fetch_add(1, Ordering::Relaxed);
    }

    pub fn reload_report(&self, ok: bool) {
        if ok {
            self.reload_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            self.reload_err.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn upstream_error(&self, kind: UpstreamErrorKind) {
        self.upstream_errors[kind.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn next_request_id(&self) -> u64 {
        self.request_ids.fetch_add(1, Ordering::Relaxed)
    }

    /// Record one classifier attempt (Iter 26): outcome plus the tier
    /// it produced (`None` when the leg never yielded one).
    pub fn classifier_report(&self, result: ClassifierResult, tier: ClassifierTier) {
        self.classifier_total[result.index() * CLASSIFIER_TIERS + tier.index()]
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record one classifier leg latency (attempts and timeouts alike).
    pub fn classifier_latency(&self, latency: Duration) {
        let secs = latency.as_secs_f64();
        for (bound, bucket) in BUCKETS.iter().zip(self.classifier_buckets.iter()) {
            if secs <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.classifier_sum_micros
            .fetch_add(latency.as_micros() as u64, Ordering::Relaxed);
        self.classifier_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Count one fallback to the configured tier after a classifier
    /// failure. Only `low`/`lowest` are legal fallback tiers, so the
    /// slot is a single bit.
    pub fn smart_fallback(&self, lowest: bool) {
        self.smart_fallback[usize::from(lowest)].fetch_add(1, Ordering::Relaxed);
    }

    /// Remember the latest classifier failure for `GET /ui` (bounded:
    /// first 200 chars, never the prompt or the verdict reasoning).
    pub fn classifier_error(&self, message: impl Into<String>) {
        let message: String = message.into().chars().take(200).collect();
        if let Ok(mut last) = self.classifier_last_error.lock() {
            *last = message;
        }
    }

    /// Read-only classifier snapshot for `GET /ui`.
    pub fn classifier_summary(&self) -> ClassifierSummary {
        let mut counts = Vec::with_capacity(CLASSIFIER_RESULTS * CLASSIFIER_TIERS);
        for result in [
            ClassifierResult::Ok,
            ClassifierResult::Timeout,
            ClassifierResult::Invalid,
            ClassifierResult::AuthError,
            ClassifierResult::Unavailable,
            ClassifierResult::NotConfigured,
            ClassifierResult::NoInput,
        ] {
            for tier in [
                ClassifierTier::Free,
                ClassifierTier::Lowest,
                ClassifierTier::Low,
                ClassifierTier::Think,
                ClassifierTier::None,
            ] {
                counts.push((
                    result.label(),
                    tier.label(),
                    self.classifier_total[result.index() * CLASSIFIER_TIERS + tier.index()]
                        .load(Ordering::Relaxed),
                ));
            }
        }
        ClassifierSummary {
            counts,
            latency_count: self.classifier_count.load(Ordering::Relaxed),
            latency_sum_ms: self.classifier_sum_micros.load(Ordering::Relaxed) as f64 / 1000.0,
            fallback_low: self.smart_fallback[0].load(Ordering::Relaxed),
            fallback_lowest: self.smart_fallback[1].load(Ordering::Relaxed),
            last_error: self
                .classifier_last_error
                .lock()
                .map(|last| last.clone())
                .unwrap_or_default(),
        }
    }

    pub fn in_flight_guard(&self) -> InFlightGuard<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlightGuard { metrics: self }
    }

    /// Owned in-flight slot for bodies that outlive the middleware
    /// (Iter 11, A04): SSE streams hold one until the body ends.
    pub fn in_flight_owned(self: &Arc<Self>) -> OwnedInFlightGuard {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        OwnedInFlightGuard {
            metrics: Arc::clone(self),
        }
    }

    /// Prometheus exposition format. Histograms, sums, and error counters
    /// always render (zeros are real); per-status request series appear
    /// once observed.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# HELP pennyrouter_requests_total Requests handled.\n");
        out.push_str("# TYPE pennyrouter_requests_total counter\n");
        for route in [Route::Health, Route::Chat, Route::Metrics, Route::Other] {
            let metrics = &self.routes[route.index()];
            let counts = metrics
                .requests_by_status
                .lock()
                .map(|counts| {
                    let mut pairs: Vec<(u16, u64)> =
                        counts.iter().map(|(status, n)| (*status, *n)).collect();
                    pairs.sort_unstable();
                    pairs
                })
                .unwrap_or_default();
            for (status, n) in counts {
                out.push_str(&format!(
                    "pennyrouter_requests_total{{route=\"{}\",status=\"{status}\"}} {n}\n",
                    route.label()
                ));
            }
        }
        out.push_str("# HELP pennyrouter_request_latency_seconds Request latency.\n");
        out.push_str("# TYPE pennyrouter_request_latency_seconds histogram\n");
        for route in [Route::Health, Route::Chat, Route::Metrics, Route::Other] {
            let metrics = &self.routes[route.index()];
            for (bound, bucket) in BUCKETS.iter().zip(metrics.buckets.iter()) {
                out.push_str(&format!(
                    "pennyrouter_request_latency_seconds_bucket{{route=\"{}\",le=\"{bound}\"}} {}\n",
                    route.label(),
                    bucket.load(Ordering::Relaxed)
                ));
            }
            let count = metrics.count.load(Ordering::Relaxed);
            let sum = metrics.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
            out.push_str(&format!(
                "pennyrouter_request_latency_seconds_bucket{{route=\"{}\",le=\"+Inf\"}} {count}\n\
                 pennyrouter_request_latency_seconds_sum{{route=\"{}\"}} {sum:.6}\n\
                 pennyrouter_request_latency_seconds_count{{route=\"{}\"}} {count}\n",
                route.label(),
                route.label(),
                route.label()
            ));
        }
        out.push_str("# HELP pennyrouter_upstream_errors_total Upstream failures.\n");
        out.push_str("# TYPE pennyrouter_upstream_errors_total counter\n");
        for kind in [
            UpstreamErrorKind::Transport,
            UpstreamErrorKind::Timeout,
            UpstreamErrorKind::Status4xx,
            UpstreamErrorKind::Status5xx,
        ] {
            out.push_str(&format!(
                "pennyrouter_upstream_errors_total{{kind=\"{}\"}} {}\n",
                kind.label(),
                self.upstream_errors[kind.index()].load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP pennyrouter_guard_rejections_total Requests rejected pre-backend.\n");
        out.push_str("# TYPE pennyrouter_guard_rejections_total counter\n");
        out.push_str(&format!(
            "pennyrouter_guard_rejections_total {}\n",
            self.guard_rejections.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP pennyrouter_load_shed_total Requests shed at capacity.\n");
        out.push_str("# TYPE pennyrouter_load_shed_total counter\n");
        out.push_str(&format!(
            "pennyrouter_load_shed_total {}\n",
            self.load_shed.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP pennyrouter_catalog_reload_total Catalog reload attempts.\n");
        out.push_str("# TYPE pennyrouter_catalog_reload_total counter\n");
        out.push_str(&format!(
            "pennyrouter_catalog_reload_total{{result=\"ok\"}} {}\n",
            self.reload_ok.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "pennyrouter_catalog_reload_total{{result=\"error\"}} {}\n",
            self.reload_err.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP pennyrouter_upstream_throttled_total Upstream 429s recorded for cool-down.\n",
        );
        out.push_str("# TYPE pennyrouter_upstream_throttled_total counter\n");
        out.push_str(&format!(
            "pennyrouter_upstream_throttled_total {}\n",
            self.throttled.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP pennyrouter_classifier_total Classifier attempts by result and tier.\n",
        );
        out.push_str("# TYPE pennyrouter_classifier_total counter\n");
        for result in [
            ClassifierResult::Ok,
            ClassifierResult::Timeout,
            ClassifierResult::Invalid,
            ClassifierResult::AuthError,
            ClassifierResult::Unavailable,
            ClassifierResult::NotConfigured,
            ClassifierResult::NoInput,
        ] {
            for tier in [
                ClassifierTier::Free,
                ClassifierTier::Lowest,
                ClassifierTier::Low,
                ClassifierTier::Think,
                ClassifierTier::None,
            ] {
                out.push_str(&format!(
                    "pennyrouter_classifier_total{{result=\"{}\",tier=\"{}\"}} {}\n",
                    result.label(),
                    tier.label(),
                    self.classifier_total[result.index() * CLASSIFIER_TIERS + tier.index()]
                        .load(Ordering::Relaxed)
                ));
            }
        }
        out.push_str("# HELP pennyrouter_classifier_latency_seconds Classifier leg latency.\n");
        out.push_str("# TYPE pennyrouter_classifier_latency_seconds histogram\n");
        for (bound, bucket) in BUCKETS.iter().zip(self.classifier_buckets.iter()) {
            out.push_str(&format!(
                "pennyrouter_classifier_latency_seconds_bucket{{le=\"{bound}\"}} {}\n",
                bucket.load(Ordering::Relaxed)
            ));
        }
        let classifier_count = self.classifier_count.load(Ordering::Relaxed);
        let classifier_sum =
            self.classifier_sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        out.push_str(&format!(
            "pennyrouter_classifier_latency_seconds_bucket{{le=\"+Inf\"}} {classifier_count}\n\
             pennyrouter_classifier_latency_seconds_sum {classifier_sum:.6}\n\
             pennyrouter_classifier_latency_seconds_count {classifier_count}\n"
        ));
        out.push_str("# HELP pennyrouter_smart_fallback_total Smart fallbacks by tier.\n");
        out.push_str("# TYPE pennyrouter_smart_fallback_total counter\n");
        out.push_str(&format!(
            "pennyrouter_smart_fallback_total{{tier=\"low\"}} {}\n",
            self.smart_fallback[0].load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "pennyrouter_smart_fallback_total{{tier=\"lowest\"}} {}\n",
            self.smart_fallback[1].load(Ordering::Relaxed)
        ));
        out.push_str("# HELP pennyrouter_log_dropped_total Log lines dropped on a full buffer.\n");
        out.push_str("# TYPE pennyrouter_log_dropped_total counter\n");
        out.push_str(&format!(
            "pennyrouter_log_dropped_total {}\n",
            crate::LOG_DROPPED.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP pennyrouter_requests_in_flight Requests in flight.\n");
        out.push_str("# TYPE pennyrouter_requests_in_flight gauge\n");
        out.push_str(&format!(
            "pennyrouter_requests_in_flight {}\n",
            self.in_flight.load(Ordering::Relaxed)
        ));
        out
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

pub struct InFlightGuard<'a> {
    metrics: &'a Metrics,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Owned variant of [`InFlightGuard`]: same counter, stream lifetime.
pub struct OwnedInFlightGuard {
    metrics: Arc<Metrics>,
}

impl Drop for OwnedInFlightGuard {
    fn drop(&mut self) {
        self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}
