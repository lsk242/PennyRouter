//! Provider routing (Iter 5–6): model suffix filters, a good default,
//! and sticky providers per session.
//!
//! The router loads a catalog DB artifact into an in-memory snapshot at
//! startup; the request path is one HashMap lookup plus a tiny ordering
//! step over that model's offerings — no DB or network on the realtime
//! path. A published ranking artifact (Iter 6), when present, supplies
//! the ordering; otherwise the same orientation formulas run inline.
//!
//! Selection rules (see `docs/routing.md`):
//! - Split the `model` field by longest-prefix match against known model
//!   ids; the rest are tags. Unknown models pass through untouched.
//! - Orientation tags pick price (`price`, `low_cost`, `cost`, `cheap`)
//!   or quality (`quality`, `best`); the first recognized orientation
//!   wins and every other tag is ignored.
//! - The default compares within the model's most common precision class
//!   (ties keep load order) and prefers the best quality tier, then the
//!   cheapest effective cost. Precision is never substituted silently.
//! - "Cheapest" means effective cost under the balanced workload profile
//!   (see the catalog price model): prompt/completion/cache parts blended,
//!   catalog promo column applied. Unpriceable offerings sort last.
//! - A session is (authorization, model). The first decision pins the
//!   provider (and precision class); later requests in the session keep
//!   that provider, with orientation tags refining only which of its
//!   offerings serves. Anonymous requests route fresh every time.
//! - The `openrouter/free` meta-model routes to any free offering across
//!   models and rewrites to its backend id; a pin that stopped being free
//!   re-routes instead of misrouting at full price.
//! - Any lookup problem falls back to plain passthrough: routing never
//!   fails a request.

use pennyrouter_catalog::{
    compare_signals, endpoint_tag, load_ranking_doc, verify_ranking, Catalog, EndpointRow,
    RankOrientation, RankingDocument, Signals, SCHEMA_VERSION,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// One provider offering of a model, projected from the catalog snapshot.
#[derive(Debug, Clone)]
pub struct Offering {
    pub model_id: String,
    pub provider_name: String,
    pub tag: Option<String>,
    pub price_prompt: Option<f64>,
    pub price_completion: Option<f64>,
    pub price_cache_read: Option<f64>,
    pub price_cache_write: Option<f64>,
    pub promo_discount: Option<f64>,
    pub precision_class: Option<String>,
    pub quality_tier: Option<String>,
    pub quality_score: Option<f64>,
}

/// In-memory routing snapshot: model id to its offerings in load order.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub models: HashMap<String, Vec<Offering>>,
    pub artifact_version: Option<String>,
}

/// Routing orientation: explicit price/quality tags, or the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Default,
    Price,
    Quality,
}

/// A routing decision: the stripped model id plus the chosen offering.
#[derive(Debug, Clone)]
pub struct Decision {
    pub model_id: String,
    pub provider: String,
    pub tag: Option<String>,
    pub precision_class: Option<String>,
    pub orientation: Orientation,
    /// True when stickiness kept a previously pinned provider.
    pub sticky: bool,
}

/// Session pin: provider (and class) chosen by the session's first route.
#[derive(Debug, Clone)]
struct Pin {
    provider: String,
    precision_class: Option<String>,
}

/// Router load/resolution failure. Callers treat this as "no routing".
#[derive(Debug)]
pub struct RouteError(pub String);

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "routing: {}", self.0)
    }
}

impl std::error::Error for RouteError {}

/// Upper bound on pinned sessions. Past the cap, new sessions route
/// fresh without pinning (existing pins are kept).
const MAX_SESSIONS: usize = 16_384;

/// Sticky pins expire after this long without renewal (Iter 11): the
/// table cannot fill with stale entries, and keys stay opaque hashes.
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

/// Header carrying the chosen provider on routed responses.
pub const PROVIDER_HEADER: &str = "x-pennyrouter-provider";

/// Precision classes the gateway may forward as an OpenRouter
/// `quantizations` requirement (Iter 11, F04). Anything else —
/// including "unknown" — never constrains the wire request.
fn is_known_precision_class(class: &str) -> bool {
    matches!(class, "fp32" | "fp16" | "fp8" | "int8" | "int4")
}

/// Per-router hash seed from wall-clock nanos XOR the process id.
/// Unpredictable enough for opaque session keys; not a secret.
fn fresh_seed() -> u64 {
    use std::hash::{Hash, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    nanos.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    hasher.finish()
}

/// Meta-model: route to any free offering ("optimize for cost including
/// free tiers"). Rewritten to the chosen offering's backend model id.
pub const FREE_MODEL: &str = "openrouter/free";

/// Virtual tiers (Iter 13): `free` = zero-cost offering, `low` =
/// cheapest basic offering, `think` = best-quality offering. Tiers
/// compare by the documented price metric (blended input/output/cache
/// effective cost) with eligibility before price; FP resolution is a
/// separate class, never a silent substitute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualTier {
    Free,
    Low,
    Think,
}

/// A virtual model id: `<provider>/pair/[privacy/]<free|low|think>`.
/// The provider segment is first so future providers slot in; only
/// OpenRouter exists today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualModel {
    pub tier: VirtualTier,
    /// Privacy variant: zero-retention fields enforced on the wire
    /// until the index carries privacy signals (Iter 13).
    pub privacy: bool,
}

/// Parse a virtual model id. Anything else (including unknown
/// providers or tiers) returns `None` and passes through untouched.
pub fn parse_virtual(model_field: &str) -> Option<VirtualModel> {
    let rest = model_field.strip_prefix("openrouter/pair/")?;
    let (privacy, tier) = match rest.split_once('/') {
        Some(("privacy", tier)) => (true, tier),
        None => (false, rest),
        _ => return None,
    };
    let tier = match tier {
        "free" => VirtualTier::Free,
        "low" => VirtualTier::Low,
        "think" => VirtualTier::Think,
        _ => return None,
    };
    Some(VirtualModel { tier, privacy })
}

/// Canonical virtual ids served by `GET /v1/models` (Iter 13).
pub const VIRTUAL_MODELS: [&str; 6] = [
    "openrouter/pair/free",
    "openrouter/pair/low",
    "openrouter/pair/think",
    "openrouter/pair/privacy/free",
    "openrouter/pair/privacy/low",
    "openrouter/pair/privacy/think",
];

/// Throttled providers stay out of virtual picks for this long (Iter
/// 13): 429 history steers around exhausted free offerings without
/// any live rate-limit feed.
const THROTTLE_TTL: Duration = Duration::from_secs(60);

/// Upper bound on remembered throttles; past it, expiry is reaped and
/// only then is new information dropped.
const MAX_THROTTLES: usize = 4096;

/// Wall-clock seconds for promo-free expiry (Iter 11, F11).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Free by permanent catalog zeros or by an unexpired published promo
/// window (Iter 11, F11). An expired or missing fact is never free —
/// promo eligibility comes from the ranking document, not from a
/// stale snapshot column.
fn free_eligible(
    ranking: Option<&RankingDocument>,
    model_id: &str,
    offering: &Offering,
    now: u64,
) -> bool {
    if inline_cost(offering) == Some(0.0) {
        return true;
    }
    ranking
        .and_then(|ranking| ranking.free_offers.get(model_id))
        .is_some_and(|offers| {
            offers.iter().any(|offer| {
                offer.provider == offering.provider_name
                    && offer.tag == offering.tag
                    && offer.valid_until_unix.is_none_or(|end| now < end)
            })
        })
}

pub struct Router {
    source: PathBuf,
    snapshot: RwLock<Snapshot>,
    ranking: RwLock<Option<RankingDocument>>,
    /// Session pins keyed by an opaque credential hash plus model, with
    /// last-use timestamps for TTL eviction (Iter 11). Raw
    /// Authorization values are never retained.
    sessions: RwLock<HashMap<(u64, String), (Pin, Instant)>>,
    /// Per-router hash seed, so session keys differ across restarts.
    seed: u64,
    /// Recently rate-limited (model, provider) pairs with cool-down
    /// deadlines (Iter 13). Never blocks routing alone: when every
    /// candidate is throttled, the least-stale one still serves.
    throttled: RwLock<HashMap<(String, String), Instant>>,
}

/// Hard per-request price ceiling from the incoming `provider` object
/// (Iter 14): both legs must parse, else there is no cap. Columns are
/// raw per-token prices; unpriceable legs fail closed under a cap.
#[derive(Debug, Clone, Copy)]
pub struct BudgetCap {
    pub prompt: f64,
    pub completion: f64,
}

/// Why a virtual request could not route (Iter 14): the menu promises
/// a floor and a budget, so failure is explicit instead of silent
/// passthrough to an id the upstream would 404.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRejection {
    pub code: &'static str,
    pub message: String,
}

/// Caller provider restrictions (Iter 11, F01; budget Iter 14):
/// `only`/`ignore` lists plus an optional price ceiling from the
/// incoming `provider` object. Candidates and sticky pins must satisfy
/// them; violations fall back to untouched passthrough so the upstream
/// enforces the original restrictions (virtual ids fail explicitly —
/// no upstream knows them).
#[derive(Debug, Default, Clone)]
pub struct ProviderConstraints {
    pub only: Vec<String>,
    pub ignore: Vec<String>,
    pub max_price: Option<BudgetCap>,
}

impl ProviderConstraints {
    /// Provider names compare case-insensitively ignoring punctuation,
    /// so display names ("OpenAI") match slugs ("openai").
    fn norm(name: &str) -> String {
        name.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    }

    pub fn allows(&self, provider: &str) -> bool {
        let candidate = Self::norm(provider);
        if !self.only.is_empty() && !self.only.iter().any(|o| Self::norm(o) == candidate) {
            return false;
        }
        !self.ignore.iter().any(|i| Self::norm(i) == candidate)
    }

    /// Parse constraints from a chat-completions body. Non-string
    /// entries are ignored (never treated as wildcards). A malformed
    /// `max_price` is no cap at all — documented, never guessed.
    pub fn from_body(body: &serde_json::Value) -> Self {
        let strings = |key: &str| {
            body.get("provider")
                .and_then(|provider| provider.get(key))
                .and_then(serde_json::Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let number = |cap: &serde_json::Value, key: &str| match cap.get(key) {
            Some(serde_json::Value::Number(n)) => n.as_f64(),
            Some(serde_json::Value::String(s)) => s.parse().ok(),
            _ => None,
        };
        let max_price = body
            .get("provider")
            .and_then(|provider| provider.get("max_price"))
            .and_then(|cap| {
                Some(BudgetCap {
                    prompt: number(cap, "prompt")?,
                    completion: number(cap, "completion")?,
                })
            });
        Self {
            only: strings("only"),
            ignore: strings("ignore"),
            max_price,
        }
    }

    /// Price caps override everything, including affinity (Iter 14):
    /// raw columns must sit under both legs. Unpriceable legs fail
    /// closed while a cap is set.
    pub fn budget_allows(&self, offering: &Offering) -> bool {
        match self.max_price {
            None => true,
            Some(cap) => {
                offering.price_prompt.is_some_and(|p| p <= cap.prompt)
                    && offering
                        .price_completion
                        .is_some_and(|p| p <= cap.completion)
            }
        }
    }
}

impl Router {
    /// Load a router from a catalog artifact: a directory (newest verified
    /// manifest wins), a `*.manifest.json` file, or a `.db` file. A
    /// directory also picks up the newest verified ranking artifact when
    /// one exists; without it the router sorts inline.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, RouteError> {
        let path = path.as_ref();
        let snapshot = Self::load_snapshot(path)?;
        let ranking = Self::discover_ranking(path);
        Ok(Self {
            source: path.to_path_buf(),
            snapshot: RwLock::new(snapshot),
            ranking: RwLock::new(ranking),
            sessions: RwLock::new(HashMap::new()),
            seed: fresh_seed(),
            throttled: RwLock::new(HashMap::new()),
        })
    }

    /// Record a 429 from `(model_id, provider)` (Iter 13): virtual
    /// picks steer around it until the cool-down elapses.
    pub fn note_throttled(&self, model_id: &str, provider: &str) {
        let mut throttled = self.throttled.write().unwrap_or_else(|e| e.into_inner());
        if throttled.len() >= MAX_THROTTLES {
            let now = Instant::now();
            throttled.retain(|_, until| *until > now);
        }
        throttled.insert(
            (model_id.to_string(), provider.to_string()),
            Instant::now() + THROTTLE_TTL,
        );
    }

    /// Providers still in cool-down are skipped when alternatives
    /// exist; when every candidate is throttled the filter is a no-op
    /// so availability wins over strictness.
    fn skip_throttled<'a>(&self, candidates: Vec<&'a Offering>) -> Vec<&'a Offering> {
        let now = Instant::now();
        let throttled = self.throttled.read().unwrap_or_else(|e| e.into_inner());
        let fresh: Vec<&'a Offering> = candidates
            .iter()
            .filter(|o| {
                throttled
                    .get(&(o.model_id.clone(), o.provider_name.clone()))
                    .is_none_or(|until| *until <= now)
            })
            .copied()
            .collect();
        if fresh.is_empty() {
            candidates
        } else {
            fresh
        }
    }

    /// Opaque session key: seeded hash of the credential plus the model
    /// (Iter 11). Raw Authorization values never enter the table.
    fn session_key(&self, authorization: &str, model: &str) -> (u64, String) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.seed.hash(&mut hasher);
        authorization.hash(&mut hasher);
        (hasher.finish(), model.to_string())
    }

    /// Look up a live pin (Iter 12: shared read lock — lookups from
    /// concurrent requests no longer serialize). Expired pins read as
    /// missing and are reaped on sight or at the next store.
    fn lookup_pin(&self, key: &(u64, String)) -> Option<Pin> {
        let expired = {
            let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
            match sessions.get(key) {
                Some((pin, used)) if used.elapsed() < SESSION_TTL => return Some(pin.clone()),
                Some(_) => true,
                None => return None,
            }
        };
        if expired {
            self.sessions
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(key);
        }
        None
    }

    /// Store a pin, evicting expired entries first; past the cap plus
    /// no expired entries, the session routes fresh (never unbounded).
    fn store_pin(&self, key: (u64, String), pin: Pin) {
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        if sessions.len() >= MAX_SESSIONS {
            sessions.retain(|_, (_, used)| used.elapsed() < SESSION_TTL);
            if sessions.len() >= MAX_SESSIONS {
                return;
            }
        }
        sessions.insert(key, (pin, Instant::now()));
    }

    /// Load a router with an explicit ranking artifact (directory or
    /// manifest file). Fails when the ranking is unloadable.
    pub fn load_with_ranking(
        catalog_path: impl AsRef<Path>,
        ranking_path: impl AsRef<Path>,
    ) -> Result<Self, RouteError> {
        let router = Self::load(catalog_path)?;
        let ranking = Self::load_ranking(ranking_path.as_ref())?.ok_or_else(|| {
            RouteError(format!(
                "no ranking artifact at {}",
                ranking_path.as_ref().display()
            ))
        })?;
        *router.ranking.write().unwrap_or_else(|e| e.into_inner()) = Some(ranking);
        Ok(router)
    }

    /// Load a snapshot without building a router (hot-swap input).
    pub fn load_snapshot(path: impl AsRef<Path>) -> Result<Snapshot, RouteError> {
        let path = path.as_ref();
        let (db_path, artifact_version) = resolve_artifact(path)?;
        let catalog = Catalog::open_read_only(&db_path).map_err(|e| RouteError(e.to_string()))?;
        let stored = catalog
            .get_meta("schema_version")
            .map_err(|e| RouteError(e.to_string()))?;
        match stored.as_deref() {
            Some(v) if v == SCHEMA_VERSION.to_string() => {}
            other => {
                return Err(RouteError(format!(
                    "unsupported catalog schema {other:?}, need v{SCHEMA_VERSION}"
                )));
            }
        }
        let mut models: HashMap<String, Vec<Offering>> = HashMap::new();
        for ep in catalog
            .all_endpoints()
            .map_err(|e| RouteError(e.to_string()))?
        {
            models
                .entry(ep.model_id.clone())
                .or_default()
                .push(offering_from(&ep));
        }
        Ok(Snapshot {
            models,
            artifact_version,
        })
    }

    /// Re-resolve the original source and swap the snapshot atomically.
    /// On failure the last good snapshot keeps serving. The ranking
    /// refreshes best-effort alongside: a failed refresh keeps the last
    /// good ranking rather than dropping to inline sort.
    pub fn reload(&self) -> Result<(), RouteError> {
        let snapshot = Self::load_snapshot(&self.source)?;
        self.swap_snapshot(snapshot);
        if let Some(ranking) = Self::discover_ranking(&self.source) {
            self.swap_ranking(ranking);
        }
        Ok(())
    }

    /// Atomically replace the snapshot (catalog rebuild input).
    /// Sessions stay pinned; pins whose provider vanished re-route.
    pub fn swap_snapshot(&self, snapshot: Snapshot) {
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }

    /// Atomically replace the ranking (ranking rebuild input).
    pub fn swap_ranking(&self, ranking: RankingDocument) {
        *self.ranking.write().unwrap_or_else(|e| e.into_inner()) = Some(ranking);
    }

    /// Best-effort ranking discovery: a directory yields its newest
    /// verified ranking, anything else yields nothing.
    fn discover_ranking(path: &Path) -> Option<RankingDocument> {
        if !path.is_dir() {
            return None;
        }
        Self::load_ranking(path).ok().flatten()
    }

    /// Load a ranking from a directory (newest verified manifest wins;
    /// invalid candidates are skipped for older valid ones) or a
    /// manifest file. `Ok(None)` means "no ranking here".
    fn load_ranking(path: &Path) -> Result<Option<RankingDocument>, RouteError> {
        if path.is_dir() {
            if let Some(manifest_path) =
                newest_valid_manifests(path, "ranking-", &verify_ranking_ok)
                    .into_iter()
                    .next()
            {
                return Self::load_verified_ranking(&manifest_path).map(Some);
            }
            return Ok(None);
        }
        if !verify_ranking(path).map_err(|e| RouteError(e.to_string()))? {
            return Err(RouteError(format!(
                "ranking hash mismatch: {}",
                path.display()
            )));
        }
        Self::load_verified_ranking(path).map(Some)
    }

    fn load_verified_ranking(manifest_path: &Path) -> Result<RankingDocument, RouteError> {
        let raw = std::fs::read_to_string(manifest_path).map_err(|e| RouteError(e.to_string()))?;
        let manifest: pennyrouter_catalog::RankingManifest =
            serde_json::from_str(&raw).map_err(|e| RouteError(e.to_string()))?;
        let dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
        load_ranking_doc(&dir.join(&manifest.ranking_file)).map_err(|e| RouteError(e.to_string()))
    }

    /// Route one request. `model_field` is the raw `model` value,
    /// `authorization` the raw Authorization header if present. Returns
    /// `None` when the request should pass through untouched.
    pub fn decide(&self, model_field: &str, authorization: Option<&str>) -> Option<Decision> {
        self.decide_constrained(model_field, authorization, &ProviderConstraints::default())
    }

    /// Route one request honoring caller provider restrictions (Iter 11,
    /// F01). Candidates and sticky pins must satisfy `constraints`;
    /// anything unsatisfiable returns `None` (untouched passthrough, so
    /// the upstream enforces the original restrictions).
    pub fn decide_constrained(
        &self,
        model_field: &str,
        authorization: Option<&str>,
        constraints: &ProviderConstraints,
    ) -> Option<Decision> {
        let snapshot = self.snapshot.read().unwrap_or_else(|e| e.into_inner());
        // Virtual ids resolve before catalog lookup (Iter 13): they are
        // not catalog models, so longest-prefix split must not see them.
        if let Some(virtual_model) = parse_virtual(model_field) {
            return self
                .decide_virtual(
                    &snapshot,
                    &virtual_model,
                    model_field,
                    authorization,
                    constraints,
                )
                .ok();
        }
        if model_field == FREE_MODEL || model_field.starts_with(&format!("{FREE_MODEL}/")) {
            let tags = model_field[FREE_MODEL.len()..]
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            return self.decide_free(&snapshot, &tags, authorization, constraints, FREE_MODEL);
        }
        let (model_id, tags) = split_model(&snapshot, model_field)?;
        let offerings = snapshot.models.get(&model_id)?;
        if offerings.is_empty() {
            return None;
        }
        let orientation = orientation_from(&tags);
        let key = authorization.map(|auth| self.session_key(auth, &model_id));
        let ranking = self.ranking.read().unwrap_or_else(|e| e.into_inner());

        // Sticky path: keep the pinned provider when it still serves and
        // still satisfies the caller's restrictions.
        if let Some(key) = &key {
            if let Some(pin) = self.lookup_pin(key) {
                if constraints.allows(&pin.provider) {
                    let mine: Vec<&Offering> = offerings
                        .iter()
                        .filter(|o| {
                            o.provider_name == pin.provider
                                && o.precision_class == pin.precision_class
                        })
                        .collect();
                    if !mine.is_empty() {
                        let order = ordered(
                            ranking.as_ref(),
                            &model_id,
                            &pin.precision_class,
                            orientation,
                            &mine,
                        );
                        if let Some(pick) = order.into_iter().next() {
                            // Price caps override affinity: a pin above the
                            // ceiling re-routes instead of sticking (Iter 14).
                            if constraints.budget_allows(pick) {
                                return Some(Decision {
                                    model_id,
                                    provider: pick.provider_name.clone(),
                                    tag: pick.tag.clone(),
                                    precision_class: pick.precision_class.clone(),
                                    orientation,
                                    sticky: true,
                                });
                            }
                        }
                    }
                }
                // Pin unusable (provider/class gone, or newly disallowed):
                // fall through and re-route fresh, re-pinning below.
            }
        }

        // Fresh path: most common precision class, then oriented pick
        // among restriction- and budget-satisfying candidates.
        let class = majority_class(offerings)?;
        let candidates: Vec<&Offering> = offerings
            .iter()
            .filter(|o| {
                o.precision_class == class
                    && constraints.allows(&o.provider_name)
                    && constraints.budget_allows(o)
            })
            .collect();
        let pick = ordered(
            ranking.as_ref(),
            &model_id,
            &class,
            orientation,
            &candidates,
        )
        .into_iter()
        .next()?;
        if let Some(key) = key {
            self.store_pin(
                key,
                Pin {
                    provider: pick.provider_name.clone(),
                    precision_class: pick.precision_class.clone(),
                },
            );
        }
        Some(Decision {
            model_id,
            provider: pick.provider_name.clone(),
            tag: pick.tag.clone(),
            precision_class: pick.precision_class.clone(),
            orientation,
            sticky: false,
        })
    }

    /// Route an `openrouter/free` request to any free offering across all
    /// models. Sessions pin as usual, but a pin whose offering stopped
    /// being free re-routes instead of misrouting at full price. The
    /// decision carries the chosen offering's backend model id.
    /// Virtual dispatch (Iter 13): `free` shares the legacy free path
    /// under its own session namespace; `low` picks the cheapest
    /// priceable offering across models; `think` picks the best-quality
    /// offering across models. Throttled providers are skipped when
    /// alternatives exist.
    fn decide_virtual(
        &self,
        snapshot: &Snapshot,
        virtual_model: &VirtualModel,
        model_field: &str,
        authorization: Option<&str>,
        constraints: &ProviderConstraints,
    ) -> Result<Decision, RouteRejection> {
        match virtual_model.tier {
            VirtualTier::Free => self
                .decide_free(snapshot, &[], authorization, constraints, model_field)
                .ok_or_else(|| RouteRejection {
                    code: "no_eligible_offering",
                    message: "no free offering is currently eligible".to_string(),
                }),
            tier => {
                self.decide_ordered_virtual(snapshot, authorization, constraints, model_field, tier)
            }
        }
    }

    /// Quality-tier floor per virtual tier (Iter 14): `low` needs a
    /// judged tier (unknown quality promises nothing basic); `think`
    /// needs `high`. Unmet floors reject instead of silently serving
    /// below the promise.
    fn tier_floor(tier: VirtualTier) -> u8 {
        match tier {
            VirtualTier::Free => 0,
            VirtualTier::Low => 1,
            VirtualTier::Think => 3,
        }
    }

    /// Cross-model pick for the `low` (cheapest effective cost) and
    /// `think` (best quality signals) tiers. Eligibility filters
    /// before price compares: caller restrictions, priceable, and a
    /// known precision class (unknown-class offerings are not eligible
    /// for tiered routing, so FP resolution never substitutes
    /// silently).
    fn decide_ordered_virtual(
        &self,
        snapshot: &Snapshot,
        authorization: Option<&str>,
        constraints: &ProviderConstraints,
        session_ns: &str,
        tier: VirtualTier,
    ) -> Result<Decision, RouteRejection> {
        let tier_name = match tier {
            VirtualTier::Free => "free",
            VirtualTier::Low => "low",
            VirtualTier::Think => "think",
        };
        let base: Vec<&Offering> = snapshot
            .models
            .values()
            .flatten()
            .filter(|o| {
                constraints.allows(&o.provider_name)
                    // Tiers are disjoint by design: zero-cost serves `free`
                    // only, never `low`/`think`.
                    && inline_cost(o).is_some_and(|cost| cost > 0.0)
                    && is_known_precision_class(o.precision_class.as_deref().unwrap_or("unknown"))
            })
            .collect();
        if base.is_empty() {
            return Err(RouteRejection {
                code: "no_eligible_offering",
                message: format!("no offering is eligible for the {tier_name} tier"),
            });
        }
        let floored: Vec<&Offering> = base
            .into_iter()
            .filter(|o| Self::quality_rank(o.quality_tier.as_deref()) >= Self::tier_floor(tier))
            .collect();
        if floored.is_empty() {
            return Err(RouteRejection {
                code: "quality_floor_unmet",
                message: format!("no offering meets the {tier_name} tier quality floor"),
            });
        }
        let budgeted: Vec<&Offering> = floored
            .into_iter()
            .filter(|o| constraints.budget_allows(o))
            .collect();
        if budgeted.is_empty() {
            return Err(RouteRejection {
                code: "budget_exceeded",
                message: format!("no {tier_name} tier offering fits the price ceiling"),
            });
        }
        let candidates = self.skip_throttled(budgeted);
        let key = authorization.map(|auth| self.session_key(auth, session_ns));
        // Sticky path: the pinned provider serves and already passed
        // floor + budget (candidates are filtered), so keep it.
        if let Some(key) = &key {
            if let Some(pin) = self.lookup_pin(key) {
                if let Some(pick) = candidates.iter().find(|o| {
                    o.provider_name == pin.provider && o.precision_class == pin.precision_class
                }) {
                    return Ok(Decision {
                        model_id: pick.model_id.clone(),
                        provider: pick.provider_name.clone(),
                        tag: pick.tag.clone(),
                        precision_class: pick.precision_class.clone(),
                        orientation: Orientation::Default,
                        sticky: true,
                    });
                }
            }
        }
        let pick = match tier {
            VirtualTier::Low => candidates.into_iter().min_by(|a, b| {
                inline_cost(a)
                    .partial_cmp(&inline_cost(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.model_id.cmp(&b.model_id))
                    .then_with(|| a.provider_name.cmp(&b.provider_name))
            }),
            // Best-first comparator: the minimum sorts first.
            _ => candidates.into_iter().min_by(|a, b| {
                compare_signals(
                    &Self::virtual_signals(a),
                    &Self::virtual_signals(b),
                    RankOrientation::Quality,
                )
            }),
        }
        .ok_or_else(|| RouteRejection {
            code: "no_eligible_offering",
            message: format!("no offering is eligible for the {tier_name} tier"),
        })?;
        if let Some(key) = key {
            self.store_pin(
                key,
                Pin {
                    provider: pick.provider_name.clone(),
                    precision_class: pick.precision_class.clone(),
                },
            );
        }
        Ok(Decision {
            model_id: pick.model_id.clone(),
            provider: pick.provider_name.clone(),
            tag: pick.tag.clone(),
            precision_class: pick.precision_class.clone(),
            orientation: Orientation::Default,
            sticky: false,
        })
    }

    /// Quality-tier rank for floors (Iter 14): mirrors the catalog
    /// ordering (high 3, standard 2, low 1, unknown 0).
    fn quality_rank(tier: Option<&str>) -> u8 {
        match tier {
            Some("high") => 3,
            Some("standard") => 2,
            Some("low") => 1,
            _ => 0,
        }
    }

    /// Inline signals for one offering in cross-model comparison
    /// (Iter 13): identity plus its own cost/tier/score, no ranking.
    fn virtual_signals(offering: &Offering) -> Signals<'_> {
        Signals {
            provider: &offering.provider_name,
            tag: offering.tag.as_deref(),
            cost: inline_cost(offering),
            tier: offering.quality_tier.as_deref(),
            score: offering.quality_score,
            error_rate: None,
            latency_ms: None,
        }
    }

    fn decide_free(
        &self,
        snapshot: &Snapshot,
        tags: &[String],
        authorization: Option<&str>,
        constraints: &ProviderConstraints,
        session_ns: &str,
    ) -> Option<Decision> {
        let now = now_unix();
        let ranking = self.ranking.read().unwrap_or_else(|e| e.into_inner());
        let free: Vec<&Offering> = snapshot
            .models
            .iter()
            .flat_map(|(model_id, offerings)| {
                offerings.iter().filter(|o| {
                    constraints.allows(&o.provider_name)
                        && free_eligible(ranking.as_ref(), model_id, o, now)
                })
            })
            .collect();
        if free.is_empty() {
            return None;
        }
        let orientation = orientation_from(tags);
        let key = authorization.map(|auth| self.session_key(auth, session_ns));

        if let Some(key) = &key {
            if let Some(pin) = self.lookup_pin(key) {
                if !constraints.allows(&pin.provider) {
                    // Newly disallowed: fall through and re-route fresh.
                } else {
                    let mine: Vec<&Offering> = free
                        .iter()
                        .filter(|o| {
                            o.provider_name == pin.provider
                                && o.precision_class == pin.precision_class
                        })
                        .copied()
                        .collect();
                    let order = ordered(
                        ranking.as_ref(),
                        FREE_MODEL,
                        &pin.precision_class,
                        orientation,
                        &mine,
                    );
                    if let Some(pick) = order.into_iter().next() {
                        if free_eligible(ranking.as_ref(), &pick.model_id, pick, now) {
                            return Some(free_decision(pick, orientation, true));
                        }
                        // Pin stopped being free: fall through and re-route.
                    }
                }
            }
        }

        let owned: Vec<Offering> = free.into_iter().cloned().collect();
        let class = majority_class(&owned)?;
        let candidates: Vec<&Offering> = owned
            .iter()
            .filter(|o| o.precision_class == class)
            .collect();
        // Recently rate-limited providers yield to fresh ones when any
        // remain (Iter 13); all-throttled still serves (availability).
        let candidates = self.skip_throttled(candidates);
        let order = ordered(
            ranking.as_ref(),
            FREE_MODEL,
            &class,
            orientation,
            &candidates,
        );
        let pick = order.into_iter().next()?;
        // Candidates were pre-filtered to free offerings, so the pick is
        // free by construction.
        if let Some(key) = key {
            self.store_pin(
                key,
                Pin {
                    provider: pick.provider_name.clone(),
                    precision_class: pick.precision_class.clone(),
                },
            );
        }
        Some(free_decision(pick, orientation, false))
    }

    /// Route a buffered chat-completions body: returns the rewritten body
    /// (stripped model + forced provider) with its decision, or `None`
    /// to forward the body untouched.
    ///
    /// Caller provider restrictions survive (Iter 11, F01): every field
    /// of the incoming `provider` object is preserved — only `order`
    /// and `allow_fallbacks` are set by routing. When no candidate
    /// satisfies `only`/`ignore`, the body passes through untouched so
    /// the upstream enforces the original restrictions.
    pub fn route_request(
        &self,
        headers: &axum::http::HeaderMap,
        body: &[u8],
    ) -> Option<(Vec<u8>, Decision)> {
        self.route_inner(headers, body, None).ok().flatten()
    }

    /// Strict variant (Iter 14): virtual rejections surface as `Err`
    /// so the handler can fail closed instead of forwarding a
    /// gateway-owned id the upstream would 404.
    pub fn route_request_or_reject(
        &self,
        headers: &axum::http::HeaderMap,
        body: &[u8],
    ) -> Result<Option<(Vec<u8>, Decision)>, RouteRejection> {
        self.route_inner(headers, body, None)
    }

    /// Re-route after a failed attempt (Iter 13): same body, but the
    /// failed provider joins `ignore` so the retry lands elsewhere.
    /// Returns `None` when nothing else can serve (the first attempt's
    /// response stands).
    pub fn route_request_retry(
        &self,
        headers: &axum::http::HeaderMap,
        body: &[u8],
        failed_provider: &str,
    ) -> Option<(Vec<u8>, Decision)> {
        let mut extra = ProviderConstraints::default();
        extra.ignore.push(failed_provider.to_string());
        self.route_inner(headers, body, Some(&extra)).ok().flatten()
    }

    fn route_inner(
        &self,
        headers: &axum::http::HeaderMap,
        body: &[u8],
        extra: Option<&ProviderConstraints>,
    ) -> Result<Option<(Vec<u8>, Decision)>, RouteRejection> {
        let json: serde_json::Value = serde_json::from_slice(body).map_err(|_| RouteRejection {
            code: "invalid_body",
            message: "request body is not valid JSON".to_string(),
        })?;
        let model_field = json
            .get("model")
            .and_then(|model| model.as_str())
            .map(str::to_string)
            .ok_or_else(|| RouteRejection {
                code: "invalid_body",
                message: "request body has no string \"model\" field".to_string(),
            })?;
        let auth = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        let mut constraints = ProviderConstraints::from_body(&json);
        if let Some(extra) = extra {
            constraints.ignore.extend(extra.ignore.iter().cloned());
            constraints.only.extend(extra.only.iter().cloned());
        }
        // Virtual ids resolve here, not in `decide_constrained`: a
        // rejection must surface, never decay into untouched passthrough
        // of a gateway-owned id.
        if let Some(virtual_model) = parse_virtual(&model_field) {
            let snapshot = self.snapshot.read().unwrap_or_else(|e| e.into_inner());
            let decision =
                self.decide_virtual(&snapshot, &virtual_model, &model_field, auth, &constraints)?;
            return Ok(Self::finish_route(&model_field, json, decision));
        }
        let decision = match self.decide_constrained(&model_field, auth, &constraints) {
            Some(decision) => decision,
            None => return Ok(None),
        };
        Ok(Self::finish_route(&model_field, json, decision))
    }

    /// Rewrite the body for `decision`: real backend model id, forced
    /// provider order, best-effort precision pin, privacy fields.
    fn finish_route(
        model_field: &str,
        mut json: serde_json::Value,
        decision: Decision,
    ) -> Option<(Vec<u8>, Decision)> {
        json["model"] = serde_json::Value::String(decision.model_id.clone());
        let mut provider = json
            .get("provider")
            .and_then(|provider| provider.as_object())
            .cloned()
            .unwrap_or_default();
        provider.insert(
            "order".to_string(),
            serde_json::json!([decision.provider.clone()]),
        );
        provider.insert("allow_fallbacks".to_string(), serde_json::json!(false));
        // F04: pin the chosen precision class as a quantization
        // requirement (best-effort: classes are coarse), unless the
        // caller already constrained quantizations.
        if provider.get("quantizations").is_none() {
            if let Some(class) = decision.precision_class.as_deref() {
                if is_known_precision_class(class) {
                    provider.insert("quantizations".to_string(), serde_json::json!([class]));
                }
            }
        }
        // Privacy virtual models enforce zero-retention on the wire
        // unless the caller already set the fields (Iter 13).
        if parse_virtual(model_field).is_some_and(|v| v.privacy) {
            if provider.get("zdr").is_none() {
                provider.insert("zdr".to_string(), serde_json::json!(true));
            }
            if provider.get("data_collection").is_none() {
                provider.insert("data_collection".to_string(), serde_json::json!("deny"));
            }
        }
        json["provider"] = serde_json::Value::Object(provider);
        let rewritten = serde_json::to_vec(&json).ok()?;
        Some((rewritten, decision))
    }
}

/// Compact read-only projection for the debug UI (Iter 17): no
/// payloads, no sessions, no throttle map — only what an operator
/// needs to see current routing state.
#[derive(Debug, Clone)]
pub struct UiOffering {
    pub provider: String,
    pub tag: Option<String>,
    pub price_prompt: Option<f64>,
    pub price_completion: Option<f64>,
    pub precision_class: Option<String>,
    pub quality_tier: Option<String>,
    pub quality_score: Option<f64>,
}

/// Default-orientation provider order for one (model, class).
#[derive(Debug, Clone)]
pub struct UiClassRanking {
    pub class: Option<String>,
    pub order: Vec<String>,
}

/// Default-orientation provider order per (model, class), plus free
/// offers with expiry, as the ranking artifact records them.
#[derive(Debug, Clone)]
pub struct UiSummary {
    pub artifact_version: Option<String>,
    pub models: Vec<(String, Vec<UiOffering>)>,
    pub ranking: Vec<(String, Vec<UiClassRanking>)>,
    pub free_offers: Vec<(String, String, Option<String>, Option<u64>)>,
}

impl Router {
    /// Read-only snapshot projection for `GET /ui` (Iter 17). Takes
    /// the same short read locks as routing; never blocks writers
    /// beyond a clone.
    pub fn ui_summary(&self) -> UiSummary {
        let snapshot = self.snapshot.read().unwrap_or_else(|e| e.into_inner());
        let mut models: Vec<(String, Vec<UiOffering>)> = snapshot
            .models
            .iter()
            .map(|(model_id, offerings)| {
                let mut entries: Vec<UiOffering> = offerings
                    .iter()
                    .map(|o| UiOffering {
                        provider: o.provider_name.clone(),
                        tag: o.tag.clone(),
                        price_prompt: o.price_prompt,
                        price_completion: o.price_completion,
                        precision_class: o.precision_class.clone(),
                        quality_tier: o.quality_tier.clone(),
                        quality_score: o.quality_score,
                    })
                    .collect();
                entries.sort_by(|a, b| a.provider.cmp(&b.provider));
                (model_id.clone(), entries)
            })
            .collect();
        models.sort_by(|a, b| a.0.cmp(&b.0));
        let artifact_version = snapshot.artifact_version.clone();
        drop(snapshot);
        let ranking = self.ranking.read().unwrap_or_else(|e| e.into_inner());
        let mut rank_rows = Vec::new();
        let mut free_rows = Vec::new();
        if let Some(doc) = ranking.as_ref() {
            for (model_id, classes) in &doc.models {
                let mut class_rows = Vec::new();
                for class in classes {
                    let order: Vec<String> = class
                        .default
                        .iter()
                        .map(|entry| match &entry.tag {
                            Some(tag) => format!("{} ({tag})", entry.provider),
                            None => entry.provider.clone(),
                        })
                        .collect();
                    class_rows.push(UiClassRanking {
                        class: class.class.clone(),
                        order,
                    });
                }
                rank_rows.push((model_id.clone(), class_rows));
            }
            for (model_id, offers) in &doc.free_offers {
                for offer in offers {
                    free_rows.push((
                        model_id.clone(),
                        offer.provider.clone(),
                        offer.tag.clone(),
                        offer.valid_until_unix,
                    ));
                }
            }
        }
        UiSummary {
            artifact_version,
            models,
            ranking: rank_rows,
            free_offers: free_rows,
        }
    }
}

/// Providers with their own upstream, addressed by id prefix
/// (Iter 16). Anything else routes via OpenRouter as before.
pub const PINNED_PROVIDERS: &[&str] = &["modal", "baseten"];

/// Split `modal/<model>` / `baseten/<model>` into (provider, model).
/// Empty remainders and other prefixes are not pinned routes.
pub fn split_pinned(model_field: &str) -> Option<(&str, &str)> {
    let (provider, rest) = model_field.split_once('/')?;
    if PINNED_PROVIDERS.contains(&provider) && !rest.is_empty() {
        Some((provider, rest))
    } else {
        None
    }
}

/// Rewrite a pinned body (Iter 16): the model becomes the stripped
/// remainder and the OpenRouter-specific `provider` object (order
/// pins, quantizations, zdr) is dropped — foreign upstreams speak
/// plain OpenAI chat and must never see it. Pure BYOK: auth travels
/// in the caller's headers, untouched here.
pub fn route_pinned(model_field: &str, mut json: serde_json::Value) -> Option<(Vec<u8>, Decision)> {
    let (provider, rest) = split_pinned(model_field)?;
    json["model"] = serde_json::Value::String(rest.to_string());
    if let Some(object) = json.as_object_mut() {
        object.remove("provider");
    }
    let rewritten = serde_json::to_vec(&json).ok()?;
    Some((
        rewritten,
        Decision {
            model_id: rest.to_string(),
            provider: provider.to_string(),
            tag: None,
            precision_class: None,
            orientation: Orientation::Default,
            sticky: false,
        },
    ))
}

/// Resolve an artifact path to (db file, version). Directories take
/// the newest *verifying* manifest (invalid candidates are skipped for
/// older valid ones); manifest files are verified; `.db` files open
/// as-is.
fn resolve_artifact(path: &Path) -> Result<(PathBuf, Option<String>), RouteError> {
    if path.is_dir() {
        if let Some(manifest_path) = newest_valid_manifests(path, "catalog-", &catalog_manifest_ok)
            .into_iter()
            .next()
        {
            return resolve_manifest(&manifest_path);
        }
        return Err(RouteError(format!(
            "no verifying catalog manifest in {}",
            path.display()
        )));
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => resolve_manifest(path),
        _ => Ok((path.to_path_buf(), None)),
    }
}

fn catalog_manifest_ok(path: &Path) -> bool {
    pennyrouter_catalog::verify(path).unwrap_or(false)
}

fn verify_ranking_ok(path: &Path) -> bool {
    verify_ranking(path).unwrap_or(false)
}

/// Manifest candidates newest-first by `created_at` (Iter 11, F07).
/// Unparseable files are skipped with a log line instead of aborting
/// discovery: one truncated publish must not poison later good
/// artifacts. Both catalog and ranking manifests carry `created_at`.
fn newest_valid_manifests(
    dir: &Path,
    prefix: &str,
    verify: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("gateway: cannot read {}: {e}", dir.display());
            return Vec::new();
        }
    };
    let mut manifests: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                eprintln!("gateway: bad directory entry: {e}");
                continue;
            }
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if !name
            .as_deref()
            .is_some_and(|n| n.starts_with(prefix) && n.ends_with(".manifest.json"))
        {
            continue;
        }
        let created_at = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|manifest| {
                manifest
                    .get("created_at")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        manifests.push((created_at, path));
    }
    manifests.sort();
    manifests
        .into_iter()
        .rev()
        .map(|(_, path)| path)
        .filter(|path| {
            let ok = verify(path);
            if !ok {
                eprintln!("gateway: skipping invalid manifest {}", path.display());
            }
            ok
        })
        .collect()
}

fn resolve_manifest(manifest_path: &Path) -> Result<(PathBuf, Option<String>), RouteError> {
    if !pennyrouter_catalog::verify(manifest_path).map_err(|e| RouteError(e.to_string()))? {
        return Err(RouteError(format!(
            "artifact hash mismatch: {}",
            manifest_path.display()
        )));
    }
    let raw = std::fs::read_to_string(manifest_path).map_err(|e| RouteError(e.to_string()))?;
    let manifest: pennyrouter_catalog::Manifest =
        serde_json::from_str(&raw).map_err(|e| RouteError(e.to_string()))?;
    let dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    Ok((dir.join(&manifest.db_file), Some(manifest.version.clone())))
}

fn offering_from(ep: &EndpointRow) -> Offering {
    let tag = serde_json::from_str(&ep.payload)
        .ok()
        .and_then(|v: serde_json::Value| endpoint_tag(&v).map(str::to_string));
    Offering {
        model_id: ep.model_id.clone(),
        provider_name: ep.provider_name.clone(),
        tag,
        price_prompt: ep.price_prompt,
        price_completion: ep.price_completion,
        price_cache_read: ep.price_cache_read,
        price_cache_write: ep.price_cache_write,
        promo_discount: ep.promo_discount,
        precision_class: ep.precision_class.clone(),
        quality_tier: ep.quality_tier.clone(),
        quality_score: ep.quality_score,
    }
}

/// Free-model decision: the rewritten model is the chosen offering's
/// backend id, not the meta-model.
fn free_decision(pick: &Offering, orientation: Orientation, sticky: bool) -> Decision {
    Decision {
        model_id: pick.model_id.clone(),
        provider: pick.provider_name.clone(),
        tag: pick.tag.clone(),
        precision_class: pick.precision_class.clone(),
        orientation,
        sticky,
    }
}

/// Inline effective cost: balanced profile with the catalog discount
/// column (no promo-window expiry — the ranked path is authoritative).
fn inline_cost(offering: &Offering) -> Option<f64> {
    pennyrouter_catalog::effective_cost(
        offering.price_prompt,
        offering.price_completion,
        offering.price_cache_read,
        offering.price_cache_write,
        &pennyrouter_catalog::balanced_profile(),
        offering.promo_discount.unwrap_or(0.0),
    )
}

/// Split a raw `model` field into (model id, tags) by longest-prefix
/// match against known models. `None` means "not our model".
fn split_model(snapshot: &Snapshot, model_field: &str) -> Option<(String, Vec<String>)> {
    if snapshot.models.contains_key(model_field) {
        return Some((model_field.to_string(), Vec::new()));
    }
    let mut rest = model_field;
    let mut tags: Vec<String> = Vec::new();
    while let Some((head, tail)) = rest.rsplit_once('/') {
        if tail.is_empty() {
            return None;
        }
        tags.push(tail.to_string());
        if snapshot.models.contains_key(head) {
            tags.reverse();
            return Some((head.to_string(), tags));
        }
        rest = head;
    }
    None
}

fn orientation_from(tags: &[String]) -> Orientation {
    for tag in tags {
        match tag.as_str() {
            "price" | "low_cost" | "cost" | "cheap" => return Orientation::Price,
            "quality" | "best" => return Orientation::Quality,
            _ => {}
        }
    }
    Orientation::Default
}

/// Most common precision class; ties keep first-seen (load) order.
fn majority_class(offerings: &[Offering]) -> Option<Option<String>> {
    let mut counts: Vec<(Option<String>, usize)> = Vec::new();
    for o in offerings {
        match counts.iter_mut().find(|(c, _)| *c == o.precision_class) {
            Some((_, n)) => *n += 1,
            None => counts.push((o.precision_class.clone(), 1)),
        }
    }
    // Strictly-greater keeps the first class on ties (max_by_key would
    // keep the last).
    let mut best: Option<(Option<String>, usize)> = None;
    for (class, n) in counts {
        let keep = best.as_ref().is_some_and(|(_, m)| *m >= n);
        if !keep {
            best = Some((class, n));
        }
    }
    best.map(|(class, _)| class)
}

fn rank_orientation(orientation: Orientation) -> RankOrientation {
    match orientation {
        Orientation::Default => RankOrientation::Default,
        Orientation::Price => RankOrientation::Price,
        Orientation::Quality => RankOrientation::Quality,
    }
}

/// Order candidates best-first: the published ranking when it covers
/// (model, class), otherwise the same orientation formulas applied
/// inline to snapshot signals (no trends). Ranked entries that no
/// longer exist are skipped; live offerings missing from the ranking
/// append at the end, so a stale ranking never drops an offering.
fn ordered<'o>(
    ranking: Option<&RankingDocument>,
    model_id: &str,
    class: &Option<String>,
    orientation: Orientation,
    candidates: &[&'o Offering],
) -> Vec<&'o Offering> {
    if let Some(entries) = ranking
        .and_then(|r| r.models.get(model_id))
        .and_then(|classes| classes.iter().find(|c| &c.class == class))
        .map(|class_ranking| match orientation {
            Orientation::Default => &class_ranking.default,
            Orientation::Price => &class_ranking.price,
            Orientation::Quality => &class_ranking.quality,
        })
    {
        let mut order: Vec<&Offering> = Vec::with_capacity(candidates.len());
        let mut rest: Vec<&Offering> = candidates.to_vec();
        for entry in entries {
            if let Some(pos) = rest
                .iter()
                .position(|o| o.provider_name == entry.provider && o.tag == entry.tag)
            {
                order.push(rest.remove(pos));
            }
        }
        order.extend(rest);
        return order;
    }
    let mut sorted: Vec<&Offering> = candidates.to_vec();
    sorted.sort_by(|a, b| {
        compare_signals(
            &inline_signals(a),
            &inline_signals(b),
            rank_orientation(orientation),
        )
    });
    sorted
}

fn inline_signals(offering: &Offering) -> pennyrouter_catalog::Signals<'_> {
    pennyrouter_catalog::Signals {
        provider: &offering.provider_name,
        tag: offering.tag.as_deref(),
        cost: inline_cost(offering),
        tier: offering.quality_tier.as_deref(),
        score: offering.quality_score,
        error_rate: None,
        latency_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offering(provider: &str, price: Option<f64>, tier: Option<&str>) -> Offering {
        // Completion mirrors prompt so effective cost equals price.
        Offering {
            model_id: "test/model".to_string(),
            provider_name: provider.to_string(),
            tag: None,
            price_prompt: price,
            price_completion: price,
            price_cache_read: None,
            price_cache_write: None,
            promo_discount: None,
            precision_class: Some("fp16".to_string()),
            quality_tier: tier.map(str::to_string),
            quality_score: None,
        }
    }

    fn priced(model: &str, provider: &str, price: f64, tier: Option<&str>) -> Offering {
        Offering {
            model_id: model.to_string(),
            ..offering(provider, Some(price), tier)
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            models: HashMap::from([
                (
                    "meta/muse-spark".to_string(),
                    vec![
                        offering("Cheap", Some(1.0), Some("low")),
                        offering("Dear", Some(9.0), Some("high")),
                    ],
                ),
                (
                    "meta/muse-spark-open".to_string(),
                    vec![offering("Solo", Some(5.0), None)],
                ),
            ]),
            artifact_version: None,
        }
    }

    fn router() -> Router {
        Router {
            source: PathBuf::from("test"),
            snapshot: RwLock::new(snapshot()),
            ranking: RwLock::new(None),
            sessions: RwLock::new(HashMap::new()),
            seed: 0,
            throttled: RwLock::new(HashMap::new()),
        }
    }

    fn ranked_router() -> Router {
        use pennyrouter_catalog::{ClassRanking, RankEntry};
        let entry = |provider: &str| RankEntry {
            provider: provider.to_string(),
            tag: None,
        };
        // Price order deliberately disagrees with inline sort (Dear
        // first despite being dearer); default/quality agree with it.
        // A stale Ghost entry must be skipped, never served.
        let ranking = RankingDocument {
            created_at: "t".to_string(),
            catalog_version: None,
            models: [(
                "meta/muse-spark".to_string(),
                vec![ClassRanking {
                    class: Some("fp16".to_string()),
                    default: vec![entry("Dear"), entry("Ghost"), entry("Cheap")],
                    price: vec![entry("Dear"), entry("Cheap")],
                    quality: vec![entry("Dear"), entry("Cheap")],
                }],
            )]
            .into_iter()
            .collect(),
            free_offers: Default::default(),
        };
        let router = router();
        router.swap_ranking(ranking);
        router
    }

    #[test]
    fn longest_prefix_split_prefers_full_model_id() {
        let snap = snapshot();
        assert_eq!(
            split_model(&snap, "meta/muse-spark-open").unwrap(),
            ("meta/muse-spark-open".to_string(), vec![])
        );
        // "open" is a tag here only because the longer model id matches.
        let (model, tags) = split_model(&snap, "meta/muse-spark-open/price").unwrap();
        assert_eq!(model, "meta/muse-spark-open");
        assert_eq!(tags, vec!["price".to_string()]);
    }

    #[test]
    fn tags_split_and_unknown_models_pass_through() {
        let snap = snapshot();
        let (model, tags) = split_model(&snap, "meta/muse-spark/privacy/low_cost").unwrap();
        assert_eq!(model, "meta/muse-spark");
        assert_eq!(tags, vec!["privacy".to_string(), "low_cost".to_string()]);
        assert_eq!(split_model(&snap, "unknown/model"), None);
        assert_eq!(split_model(&snap, "meta/muse-spark/"), None);
    }

    #[test]
    fn first_orientation_tag_wins_and_unknowns_ignored() {
        assert_eq!(
            orientation_from(&["privacy".to_string(), "low_cost".to_string()]),
            Orientation::Price
        );
        assert_eq!(
            orientation_from(&["best".to_string(), "cheap".to_string()]),
            Orientation::Quality
        );
        assert_eq!(
            orientation_from(&["privacy".to_string()]),
            Orientation::Default
        );
        assert_eq!(orientation_from(&[]), Orientation::Default);
    }

    #[test]
    fn default_prefers_tier_then_price() {
        let r = router();
        let d = r.decide("meta/muse-spark", None).unwrap();
        assert_eq!(d.provider, "Dear"); // high tier beats cheap low tier
        assert_eq!(d.orientation, Orientation::Default);
        assert!(!d.sticky);
        let d = r.decide("meta/muse-spark/price", None).unwrap();
        assert_eq!(d.provider, "Cheap");
        assert_eq!(d.orientation, Orientation::Price);
    }

    #[test]
    fn published_ranking_overrides_inline_sort() {
        let r = ranked_router();
        // Inline price order would pick Cheap; the ranking prefers Dear.
        let d = r.decide("meta/muse-spark/price", None).unwrap();
        assert_eq!(d.provider, "Dear");
        // Stale Ghost entry skipped, never served.
        let d = r.decide("meta/muse-spark", None).unwrap();
        assert_eq!(d.provider, "Dear");
    }

    #[test]
    fn unranked_models_fall_back_to_inline_sort() {
        let r = ranked_router();
        // meta/muse-spark-open is absent from the ranking: inline sort.
        let d = r.decide("meta/muse-spark-open/price", None).unwrap();
        assert_eq!(d.provider, "Solo");
    }

    #[test]
    fn unranked_offerings_append_instead_of_dropping() {
        let r = ranked_router();
        // New offering since the rebuild: still routable, after ranked ones.
        r.swap_snapshot(Snapshot {
            models: HashMap::from([(
                "meta/muse-spark".to_string(),
                vec![
                    offering("Cheap", Some(1.0), Some("low")),
                    offering("Dear", Some(9.0), Some("high")),
                    offering("New", Some(0.5), Some("low")),
                ],
            )]),
            artifact_version: None,
        });
        let d = r.decide("meta/muse-spark/price", None).unwrap();
        assert_eq!(d.provider, "Dear"); // ranked first, New appended
    }

    #[test]
    fn free_model_routes_to_free_offerings_across_models() {
        let router = Router {
            source: PathBuf::from("test"),
            snapshot: RwLock::new(Snapshot {
                models: HashMap::from([
                    (
                        "m/one".to_string(),
                        vec![priced("m/one", "Paid", 5.0, Some("high"))],
                    ),
                    (
                        "m/two".to_string(),
                        vec![
                            priced("m/two", "Free", 0.0, Some("standard")),
                            priced("m/two", "Paid", 1.0, Some("high")),
                        ],
                    ),
                ]),
                artifact_version: None,
            }),
            ranking: RwLock::new(None),
            sessions: RwLock::new(HashMap::new()),
            seed: 0,
            throttled: RwLock::new(HashMap::new()),
        };
        let d = router.decide("openrouter/free", None).unwrap();
        assert_eq!(d.provider, "Free");
        assert_eq!(d.model_id, "m/two"); // rewritten to the backend id
        assert!(!d.sticky);
        // Orientation tags still apply.
        let d = router.decide("openrouter/free/quality", None).unwrap();
        assert_eq!(d.provider, "Free");
        assert_eq!(d.orientation, Orientation::Quality);
    }

    /// F11: a paid offering under a live published promo window routes
    /// free; an expired window never does; a missing fact never does.
    #[test]
    fn promo_free_eligibility_expires_by_wall_clock() {
        use pennyrouter_catalog::FreeOffer;
        let ranked = |valid_until_unix: Option<u64>| {
            let router = Router {
                source: PathBuf::from("test"),
                snapshot: RwLock::new(Snapshot {
                    models: HashMap::from([(
                        "m/one".to_string(),
                        vec![priced("m/one", "Promo", 5.0, Some("high"))],
                    )]),
                    artifact_version: None,
                }),
                ranking: RwLock::new(None),
                sessions: RwLock::new(HashMap::new()),
                seed: 0,
                throttled: RwLock::new(HashMap::new()),
            };
            router.swap_ranking(RankingDocument {
                created_at: "t".to_string(),
                catalog_version: None,
                models: Default::default(),
                free_offers: [(
                    "m/one".to_string(),
                    vec![FreeOffer {
                        provider: "Promo".to_string(),
                        tag: None,
                        valid_until_unix,
                    }],
                )]
                .into_iter()
                .collect(),
            });
            router
        };
        // Live window (far future): routes free to the backend id.
        let d = ranked(Some(u64::MAX))
            .decide("openrouter/free", None)
            .unwrap();
        assert_eq!(d.provider, "Promo");
        assert_eq!(d.model_id, "m/one");
        // Expired window (epoch): passes through untouched.
        assert_eq!(
            ranked(Some(0))
                .decide("openrouter/free", None)
                .map(|d| d.provider),
            None
        );
        // No published fact at all: passes through untouched.
        let router = Router {
            source: PathBuf::from("test"),
            snapshot: RwLock::new(Snapshot {
                models: HashMap::from([(
                    "m/one".to_string(),
                    vec![priced("m/one", "Promo", 5.0, Some("high"))],
                )]),
                artifact_version: None,
            }),
            ranking: RwLock::new(None),
            sessions: RwLock::new(HashMap::new()),
            seed: 0,
            throttled: RwLock::new(HashMap::new()),
        };
        assert_eq!(
            router.decide("openrouter/free", None).map(|d| d.provider),
            None
        );
    }

    #[test]
    fn free_model_passes_through_when_nothing_is_free() {
        let r = router();
        assert_eq!(r.decide("openrouter/free", None).map(|d| d.provider), None);
    }

    #[test]
    fn free_pin_reroutes_when_freeness_lapses() {
        let router = Router {
            source: PathBuf::from("test"),
            snapshot: RwLock::new(Snapshot {
                models: HashMap::from([(
                    "m/one".to_string(),
                    vec![priced("m/one", "Flash", 0.0, Some("standard"))],
                )]),
                artifact_version: None,
            }),
            ranking: RwLock::new(None),
            sessions: RwLock::new(HashMap::new()),
            seed: 0,
            throttled: RwLock::new(HashMap::new()),
        };
        let auth = Some("Bearer key");
        let d = router.decide("openrouter/free", auth).unwrap();
        assert_eq!(d.provider, "Flash");
        // Promo expired for Flash; Steadfast is free now.
        router.swap_snapshot(Snapshot {
            models: HashMap::from([(
                "m/one".to_string(),
                vec![
                    priced("m/one", "Flash", 4.0, Some("standard")),
                    priced("m/one", "Steadfast", 0.0, Some("standard")),
                ],
            )]),
            artifact_version: None,
        });
        let d = router.decide("openrouter/free", auth).unwrap();
        assert_eq!(d.provider, "Steadfast");
        assert!(!d.sticky);
    }

    #[test]
    fn sessions_pin_provider_across_snapshot_swaps() {
        let r = router();
        let auth = Some("Bearer key");
        assert_eq!(
            r.decide("meta/muse-spark/price", auth).unwrap().provider,
            "Cheap"
        );
        // Repriced world: Dear is now cheapest.
        r.swap_snapshot(Snapshot {
            models: HashMap::from([(
                "meta/muse-spark".to_string(),
                vec![
                    offering("Cheap", Some(10.0), Some("low")),
                    offering("Dear", Some(1.0), Some("high")),
                ],
            )]),
            artifact_version: None,
        });
        // Same session stays; a new session follows the new snapshot.
        let d = r.decide("meta/muse-spark/price", auth).unwrap();
        assert_eq!(d.provider, "Cheap");
        assert!(d.sticky);
        let d = r
            .decide("meta/muse-spark/price", Some("Bearer other"))
            .unwrap();
        assert_eq!(d.provider, "Dear");
        assert!(!d.sticky);
    }

    #[test]
    fn vanished_provider_repins_fresh() {
        let r = router();
        let auth = Some("Bearer key");
        assert_eq!(
            r.decide("meta/muse-spark/price", auth).unwrap().provider,
            "Cheap"
        );
        r.swap_snapshot(Snapshot {
            models: HashMap::from([(
                "meta/muse-spark".to_string(),
                vec![offering("Dear", Some(9.0), Some("high"))],
            )]),
            artifact_version: None,
        });
        let d = r.decide("meta/muse-spark", auth).unwrap();
        assert_eq!(d.provider, "Dear");
        assert!(!d.sticky);
    }

    /// F01: caller provider restrictions survive routing and constrain
    /// selection; unsatisfiable restrictions pass through untouched.
    fn routed_body(model: &str, provider: serde_json::Value) -> Vec<u8> {
        serde_json::json!({
            "model": model,
            "messages": [],
            "provider": provider,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn caller_provider_restrictions_survive_routing() {
        let r = router();
        let body = routed_body(
            "meta/muse-spark/price",
            serde_json::json!({
                "zdr": true,
                "data_collection": "deny",
                "only": ["Cheap"],
                "max_price": {"prompt": 1, "completion": 2},
            }),
        );
        let (rewritten, decision) = r
            .route_request(&axum::http::HeaderMap::new(), &body)
            .unwrap();
        assert_eq!(decision.provider, "Cheap");
        let out: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        let provider = out.get("provider").unwrap();
        assert_eq!(provider.get("zdr"), Some(&serde_json::json!(true)));
        assert_eq!(
            provider.get("data_collection"),
            Some(&serde_json::json!("deny"))
        );
        assert_eq!(
            provider.get("max_price"),
            Some(&serde_json::json!({"prompt": 1, "completion": 2}))
        );
        assert_eq!(provider.get("order"), Some(&serde_json::json!(["Cheap"])));
        assert_eq!(
            provider.get("allow_fallbacks"),
            Some(&serde_json::json!(false))
        );
    }

    #[test]
    fn unsatisfiable_only_passes_through_untouched() {
        let r = router();
        let body = routed_body(
            "meta/muse-spark/price",
            serde_json::json!({"only": ["SomebodyElse"], "zdr": true}),
        );
        assert!(r
            .route_request(&axum::http::HeaderMap::new(), &body)
            .is_none());
    }

    #[test]
    fn ignore_list_excludes_the_default_pick() {
        let r = router();
        // Default pick is Cheap; ignoring it must not silently route
        // there — Dear also serves, so routing picks Dear instead.
        let body = routed_body(
            "meta/muse-spark/price",
            serde_json::json!({"ignore": ["cheap"]}),
        );
        let (rewritten, decision) = r
            .route_request(&axum::http::HeaderMap::new(), &body)
            .unwrap();
        assert_eq!(decision.provider, "Dear");
        let out: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(
            out.pointer("/provider/order"),
            Some(&serde_json::json!(["Dear"]))
        );
    }

    #[test]
    fn changed_allowlist_invalidates_the_sticky_pin() {
        let r = router();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer [REDACTED]".parse().unwrap(),
        );
        // Price orientation pins Cheap (cheapest); default would pin Dear.
        let first = routed_body("meta/muse-spark/price", serde_json::json!({}));
        let (_, d) = r.route_request(&headers, &first).unwrap();
        assert_eq!(d.provider, "Cheap");
        assert!(!d.sticky);
        // Same session, still unrestricted: sticky.
        let (_, d) = r.route_request(&headers, &first).unwrap();
        assert!(d.sticky);
        // Same session, now restricted away from the pinned provider:
        // must re-route or pass through, never stay pinned.
        let narrowed = routed_body(
            "meta/muse-spark/price",
            serde_json::json!({"only": ["Dear"]}),
        );
        if let Some((_, d)) = r.route_request(&headers, &narrowed) {
            assert_eq!(d.provider, "Dear");
            assert!(!d.sticky);
        }
    }

    #[test]
    fn caller_quantizations_win_over_precision_class() {
        let r = router();
        let body = routed_body(
            "meta/muse-spark",
            serde_json::json!({"quantizations": ["fp8"]}),
        );
        let (rewritten, _) = r
            .route_request(&axum::http::HeaderMap::new(), &body)
            .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(
            out.pointer("/provider/quantizations"),
            Some(&serde_json::json!(["fp8"])),
            "caller constraint preserved, not overwritten by the fp16 class",
        );
    }

    #[test]
    fn known_precision_class_pins_quantization() {
        let r = router();
        let body = routed_body("meta/muse-spark", serde_json::json!({}));
        let (rewritten, _) = r
            .route_request(&axum::http::HeaderMap::new(), &body)
            .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(
            out.pointer("/provider/quantizations"),
            Some(&serde_json::json!(["fp16"])),
        );
    }

    /// A03: the inline sort the gateway falls back to must break
    /// same-provider ties by the real tag, exactly like the offline
    /// ranker — not by insertion order.
    #[test]
    fn inline_sort_breaks_same_provider_ties_by_tag() {
        use pennyrouter_catalog::{RankOrientation, Signals};
        fn signals(tag: Option<&str>) -> Signals<'_> {
            Signals {
                provider: "Solo",
                tag,
                cost: None,
                tier: None,
                score: None,
                error_rate: None,
                latency_ms: None,
            }
        }
        assert_eq!(
            compare_signals(
                &signals(Some("b-tag")),
                &signals(Some("a-tag")),
                RankOrientation::Default
            ),
            std::cmp::Ordering::Greater
        );
        // Untagged sorts before tagged (None < Some): stable either
        // way, and identical offline and inline.
        assert_eq!(
            compare_signals(
                &signals(None),
                &signals(Some("a-tag")),
                RankOrientation::Default
            ),
            std::cmp::Ordering::Less
        );
    }

    /// Iter 13: virtual ids parse strictly; anything else passes
    /// through for the catalog split to handle (or ignore).
    #[test]
    fn virtual_ids_parse_strictly() {
        assert_eq!(
            parse_virtual("openrouter/pair/low"),
            Some(VirtualModel {
                tier: VirtualTier::Low,
                privacy: false
            })
        );
        assert_eq!(
            parse_virtual("openrouter/pair/privacy/think"),
            Some(VirtualModel {
                tier: VirtualTier::Think,
                privacy: true
            })
        );
        assert_eq!(parse_virtual("azure/pair/free"), None);
        assert_eq!(parse_virtual("openrouter/pair/ultra"), None);
        assert_eq!(parse_virtual("openrouter/pair/privacy"), None);
        assert_eq!(parse_virtual("openrouter/pair/free/extra"), None);
        assert_eq!(parse_virtual("openrouter/free"), None);
        assert_eq!(parse_virtual("meta/muse-spark"), None);
        assert_eq!(VIRTUAL_MODELS.len(), 6);
        assert!(VIRTUAL_MODELS.iter().all(|id| parse_virtual(id).is_some()));
    }

    /// Dedicated snapshot for virtual-tier tests: four models with
    /// correct per-offering model ids (the shared `router()` fixture
    /// reuses one placeholder id across map keys).
    fn virtual_router() -> Router {
        Router {
            source: PathBuf::from("test"),
            snapshot: RwLock::new(Snapshot {
                models: HashMap::from([
                    (
                        "m/cheap".to_string(),
                        vec![priced("m/cheap", "Cheap", 1.0, Some("low"))],
                    ),
                    (
                        "m/mid".to_string(),
                        vec![priced("m/mid", "Mid", 3.0, Some("standard"))],
                    ),
                    (
                        "m/dear".to_string(),
                        vec![priced("m/dear", "Dear", 9.0, Some("high"))],
                    ),
                    (
                        "m/free".to_string(),
                        vec![priced("m/free", "Freebie", 0.0, Some("standard"))],
                    ),
                ]),
                artifact_version: None,
            }),
            ranking: RwLock::new(None),
            sessions: RwLock::new(HashMap::new()),
            seed: 0,
            throttled: RwLock::new(HashMap::new()),
        }
    }

    /// Iter 13: `low` is the cheapest non-free offering across models;
    /// `think` is the best-quality non-free offering; both skip
    /// throttled providers when alternatives exist.
    #[test]
    fn virtual_tiers_pick_across_models() {
        let r = virtual_router();
        let low = r.decide("openrouter/pair/low", None).unwrap();
        assert_eq!(low.provider, "Cheap");
        assert_eq!(low.model_id, "m/cheap");
        // Freebie (0.0) is excluded: tiers are disjoint by design.
        let think = r.decide("openrouter/pair/think", None).unwrap();
        assert_eq!(think.provider, "Dear");
        assert_eq!(think.model_id, "m/dear");
        // Free tier still routes the zero-cost offering.
        let free = r.decide("openrouter/pair/free", None).unwrap();
        assert_eq!(free.provider, "Freebie");
        // Unknown provider segments pass through untouched.
        assert_eq!(r.decide("azure/pair/free", None).map(|d| d.provider), None);
    }

    #[test]
    fn virtual_picks_skip_throttled_providers() {
        let r = virtual_router();
        r.note_throttled("m/cheap", "Cheap");
        let low = r.decide("openrouter/pair/low", None).unwrap();
        assert_eq!(low.provider, "Mid");
        // Everything throttled: availability wins, still serves.
        r.note_throttled("m/dear", "Dear");
        r.note_throttled("m/mid", "Mid");
        let low = r.decide("openrouter/pair/low", None).unwrap();
        assert_eq!(low.provider, "Cheap");
    }

    #[test]
    fn privacy_virtual_enforces_zero_retention_on_the_wire() {
        let r = virtual_router();
        let body = routed_body("openrouter/pair/privacy/low", serde_json::json!({}));
        let (rewritten, _) = r
            .route_request(&axum::http::HeaderMap::new(), &body)
            .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(out.pointer("/provider/zdr"), Some(&serde_json::json!(true)));
        assert_eq!(
            out.pointer("/provider/data_collection"),
            Some(&serde_json::json!("deny"))
        );
        // Non-privacy virtuals add no retention fields of their own.
        let body = routed_body("openrouter/pair/low", serde_json::json!({}));
        let (rewritten, _) = r
            .route_request(&axum::http::HeaderMap::new(), &body)
            .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(out.pointer("/provider/zdr"), None);
        // Caller-set fields always win.
        let body = routed_body(
            "openrouter/pair/privacy/low",
            serde_json::json!({"zdr": false}),
        );
        let (rewritten, _) = r
            .route_request(&axum::http::HeaderMap::new(), &body)
            .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(
            out.pointer("/provider/zdr"),
            Some(&serde_json::json!(false))
        );
    }

    #[test]
    fn majority_class_breaks_ties_by_load_order() {
        let a = offering("A", Some(1.0), None);
        let mut b = offering("B", Some(1.0), None);
        b.precision_class = Some("fp8".to_string());
        assert_eq!(
            majority_class(&[a.clone(), b.clone()]),
            Some(Some("fp16".to_string()))
        );
        assert_eq!(majority_class(&[b, a]), Some(Some("fp8".to_string())));
    }
}
