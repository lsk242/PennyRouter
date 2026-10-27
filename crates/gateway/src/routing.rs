//! Provider routing (Iter 5): model suffix filters, a good default, and
//! sticky providers per session.
//!
//! The router loads a catalog DB artifact into an in-memory snapshot at
//! startup; the request path is one HashMap lookup plus a tiny sort over
//! that model's offerings — no DB or network on the realtime path.
//!
//! Selection rules (see `docs/routing.md`):
//! - Split the `model` field by longest-prefix match against known model
//!   ids; the rest are tags. Unknown models pass through untouched.
//! - Orientation tags pick price (`price`, `low_cost`, `cost`, `cheap`)
//!   or quality (`quality`, `best`); the first recognized orientation
//!   wins and every other tag is ignored.
//! - The default compares within the model's most common precision class
//!   (ties keep load order) and prefers the best quality tier, then the
//!   cheapest prompt price. Precision is never substituted silently.
//! - A session is (authorization, model). The first decision pins the
//!   provider (and precision class); later requests in the session keep
//!   that provider, with orientation tags refining only which of its
//!   offerings serves. Anonymous requests route fresh every time.
//! - Any lookup problem falls back to plain passthrough: routing never
//!   fails a request.

use pennyrouter_catalog::{endpoint_tag, Catalog, EndpointRow, SCHEMA_VERSION};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

/// One provider offering of a model, projected from the catalog snapshot.
#[derive(Debug, Clone)]
pub struct Offering {
    pub provider_name: String,
    pub tag: Option<String>,
    pub price_prompt: Option<f64>,
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

/// Header carrying the chosen provider on routed responses.
pub const PROVIDER_HEADER: &str = "x-pennyrouter-provider";

pub struct Router {
    source: PathBuf,
    snapshot: RwLock<Snapshot>,
    sessions: Mutex<HashMap<(String, String), Pin>>,
}

impl Router {
    /// Load a router from a catalog artifact: a directory (newest verified
    /// manifest wins), a `*.manifest.json` file, or a `.db` file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, RouteError> {
        let path = path.as_ref();
        let snapshot = Self::load_snapshot(path)?;
        Ok(Self {
            source: path.to_path_buf(),
            snapshot: RwLock::new(snapshot),
            sessions: Mutex::new(HashMap::new()),
        })
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
    /// On failure the last good snapshot keeps serving.
    pub fn reload(&self) -> Result<(), RouteError> {
        let snapshot = Self::load_snapshot(&self.source)?;
        self.swap_snapshot(snapshot);
        Ok(())
    }

    /// Atomically replace the snapshot (ranking rebuild input, Iter 6).
    /// Sessions stay pinned; pins whose provider vanished re-route.
    pub fn swap_snapshot(&self, snapshot: Snapshot) {
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }

    /// Route one request. `model_field` is the raw `model` value,
    /// `authorization` the raw Authorization header if present. Returns
    /// `None` when the request should pass through untouched.
    pub fn decide(&self, model_field: &str, authorization: Option<&str>) -> Option<Decision> {
        let snapshot = self.snapshot.read().unwrap_or_else(|e| e.into_inner());
        let (model_id, tags) = split_model(&snapshot, model_field)?;
        let offerings = snapshot.models.get(&model_id)?;
        if offerings.is_empty() {
            return None;
        }
        let orientation = orientation_from(&tags);
        let key = authorization.map(|auth| (auth.to_string(), model_id.clone()));

        // Sticky path: keep the pinned provider when it still serves.
        if let Some(key) = &key {
            if let Some(pin) = self
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(key)
            {
                let mine: Vec<&Offering> = offerings
                    .iter()
                    .filter(|o| {
                        o.provider_name == pin.provider && o.precision_class == pin.precision_class
                    })
                    .collect();
                if !mine.is_empty() {
                    let pick =
                        pick(&mine, orientation).or_else(|| pick(&mine, Orientation::Default))?;
                    return Some(Decision {
                        model_id,
                        provider: pick.provider_name.clone(),
                        tag: pick.tag.clone(),
                        precision_class: pick.precision_class.clone(),
                        orientation,
                        sticky: true,
                    });
                }
                // Pin unusable (provider/class gone): fall through and
                // re-route fresh, re-pinning below.
            }
        }

        // Fresh path: most common precision class, then oriented pick.
        let class = majority_class(offerings)?;
        let candidates: Vec<&Offering> = offerings
            .iter()
            .filter(|o| o.precision_class == class)
            .collect();
        let pick = pick(&candidates, orientation)?;
        if let Some(key) = key {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if sessions.len() < MAX_SESSIONS {
                sessions.insert(
                    key,
                    Pin {
                        provider: pick.provider_name.clone(),
                        precision_class: pick.precision_class.clone(),
                    },
                );
            }
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

    /// Route a buffered chat-completions body: returns the rewritten body
    /// (stripped model + forced provider) with its decision, or `None`
    /// to forward the body untouched.
    pub fn route_request(
        &self,
        headers: &axum::http::HeaderMap,
        body: &[u8],
    ) -> Option<(Vec<u8>, Decision)> {
        let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;
        let model_field = json.get("model")?.as_str()?.to_string();
        let auth = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        let decision = self.decide(&model_field, auth)?;
        json["model"] = serde_json::Value::String(decision.model_id.clone());
        json["provider"] = serde_json::json!({
            "order": [decision.provider.clone()],
            "allow_fallbacks": false,
        });
        let rewritten = serde_json::to_vec(&json).ok()?;
        Some((rewritten, decision))
    }
}

/// Resolve an artifact path to (db file, version). Directories verify the
/// newest manifest; manifest files are verified; `.db` files open as-is.
fn resolve_artifact(path: &Path) -> Result<(PathBuf, Option<String>), RouteError> {
    if path.is_dir() {
        let mut manifests: Vec<(String, PathBuf)> = Vec::new();
        let entries = std::fs::read_dir(path)
            .map_err(|e| RouteError(format!("read {}: {e}", path.display())))?;
        for entry in entries {
            let entry = entry.map_err(|e| RouteError(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("catalog-") && name.ends_with(".manifest.json") {
                let raw =
                    std::fs::read_to_string(entry.path()).map_err(|e| RouteError(e.to_string()))?;
                let manifest: pennyrouter_catalog::Manifest = serde_json::from_str(&raw)
                    .map_err(|e| RouteError(format!("parse {}: {e}", entry.path().display())))?;
                manifests.push((manifest.created_at.clone(), entry.path()));
            }
        }
        manifests.sort();
        let manifest_path = manifests
            .pop()
            .map(|(_, p)| p)
            .ok_or_else(|| RouteError(format!("no catalog manifest in {}", path.display())))?;
        return resolve_manifest(&manifest_path);
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => resolve_manifest(path),
        _ => Ok((path.to_path_buf(), None)),
    }
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
        provider_name: ep.provider_name.clone(),
        tag,
        price_prompt: ep.price_prompt,
        precision_class: ep.precision_class.clone(),
        quality_tier: ep.quality_tier.clone(),
        quality_score: ep.quality_score,
    }
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

fn tier_rank(tier: Option<&str>) -> u8 {
    match tier {
        Some("high") => 3,
        Some("standard") => 2,
        Some("low") => 1,
        _ => 0,
    }
}

/// Best offering first: price sorts by prompt price, quality by tier
/// then score, default by tier then price. Missing values sort last.
fn pick<'a>(candidates: &[&'a Offering], orientation: Orientation) -> Option<&'a Offering> {
    let mut sorted: Vec<&Offering> = candidates.to_vec();
    sorted.sort_by(|a, b| {
        let price_ord = |a: &Offering, b: &Offering| match (a.price_prompt, b.price_prompt) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        let quality_ord = |a: &Offering, b: &Offering| {
            tier_rank(b.quality_tier.as_deref())
                .cmp(&tier_rank(a.quality_tier.as_deref()))
                .then_with(|| match (b.quality_score, a.quality_score) {
                    (Some(x), Some(y)) => x.total_cmp(&y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
        };
        match orientation {
            Orientation::Price => price_ord(a, b),
            Orientation::Quality => quality_ord(a, b),
            Orientation::Default => quality_ord(a, b).then_with(|| price_ord(a, b)),
        }
    });
    sorted.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offering(provider: &str, price: Option<f64>, tier: Option<&str>) -> Offering {
        Offering {
            provider_name: provider.to_string(),
            tag: None,
            price_prompt: price,
            precision_class: Some("fp16".to_string()),
            quality_tier: tier.map(str::to_string),
            quality_score: None,
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
            sessions: Mutex::new(HashMap::new()),
        }
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
