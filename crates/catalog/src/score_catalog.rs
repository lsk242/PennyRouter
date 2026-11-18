//! Decision-model catalog + selection policy (Iter 15).
//!
//! Which model scorers, and how it is chosen, is config — not tribal
//! knowledge. A scorers file lists one entry per backend (recorded
//! plus live), with the scorer model, base URL, the *names* of the
//! environment variables holding its secrets (never values), the
//! prompt version it is expected to answer, and the optional known
//! per-call cost. A policy section governs selection: sampling rate,
//! per-endpoint scope, and per-rebuild call/spend caps.
//!
//! ```json
//! {"scorers": [
//!    {"backend": "recorded", "recording": "scores.json"},
//!    {"backend": "baseten", "model": "meta-models/muse-spark-open-1026",
//!     "base_url": "https://inference.baseten.co/v1",
//!     "auth_env": ["BASETEN_API_KEY"], "prompt_version": "v1"}
//!  ],
//!  "policy": {"sample_rate": 0.1, "max_calls": 200}}
//! ```
//!
//! Recorded scoring is the default (free, deterministic); live scorers
//! run only when explicitly selected, sampled, and within caps. Caps
//! are checked *before* any score is written, so a breached cap
//! fails closed with the DB untouched. Scorers never run on the
//! request path — this is all offline batch.

use crate::score::RecordedScorer;
use crate::store::EndpointRow;
use crate::{BasetenScorer, Error, ModalScorer, OpenRouterScorer};
use serde_json::Value;
use std::path::Path;

/// Backends the gateway knows how to construct. Anything else in a
/// scorers file is rejected at load, never mid-rebuild.
pub const KNOWN_BACKENDS: &[&str] = &["recorded", "baseten", "modal", "openrouter"];

/// One cataloged scorer: how to build it and what it costs.
#[derive(Debug, Clone)]
pub struct ScorerEntry {
    pub backend: String,
    pub model: String,
    pub base_url: String,
    pub auth_env: Vec<String>,
    pub prompt_version: String,
    pub cost_per_call_usd: Option<f64>,
    pub recording: Option<String>,
}

/// Selection policy: which endpoints a live rebuild scorers and how
/// much it may spend. Recorded runs ignore sampling and caps (free
/// and deterministic); live runs enforce all of it.
#[derive(Debug, Clone)]
pub struct ScorerPolicy {
    /// Fraction of in-scope endpoints to score, 0.0..=1.0.
    /// Deterministic per endpoint (FNV hash), so rebuilds re-score
    /// the same subset as the catalog grows.
    pub sample_rate: f64,
    /// Empty means every model/provider is in scope (exact match).
    pub only_models: Vec<String>,
    pub only_providers: Vec<String>,
    /// Hard cap on live calls per rebuild.
    pub max_calls: Option<u64>,
    /// Hard cap on live spend per rebuild. Needs a known per-call
    /// cost, else planning fails closed.
    pub max_spend_usd: Option<f64>,
}

impl Default for ScorerPolicy {
    fn default() -> Self {
        Self {
            sample_rate: 1.0,
            only_models: Vec::new(),
            only_providers: Vec::new(),
            max_calls: None,
            max_spend_usd: None,
        }
    }
}

impl ScorerPolicy {
    pub fn validate(&self) -> Result<(), Error> {
        if !(0.0..=1.0).contains(&self.sample_rate) {
            return Err(Error::InvalidFixture(format!(
                "\"sample_rate\" must be within 0.0..=1.0, got {}",
                self.sample_rate
            )));
        }
        if self.max_calls.is_some_and(|n| n == 0) {
            return Err(Error::InvalidFixture(
                "\"max_calls\" must be at least 1".to_string(),
            ));
        }
        if self
            .max_spend_usd
            .is_some_and(|s| !s.is_finite() || s <= 0.0)
        {
            return Err(Error::InvalidFixture(
                "\"max_spend_usd\" must be a positive number".to_string(),
            ));
        }
        Ok(())
    }

    fn in_scope(&self, endpoint: &EndpointRow) -> bool {
        (self.only_models.is_empty() || self.only_models.contains(&endpoint.model_id))
            && (self.only_providers.is_empty()
                || self.only_providers.contains(&endpoint.provider_name))
    }
}

/// The loaded scorers file.
#[derive(Debug, Clone)]
pub struct ScorerCatalog {
    pub scorers: Vec<ScorerEntry>,
    pub policy: ScorerPolicy,
}

impl ScorerCatalog {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_str(&raw)
    }

    fn from_str(raw: &str) -> Result<Self, Error> {
        let doc: Value = serde_json::from_str(raw)
            .map_err(|e| Error::InvalidFixture(format!("scorers file is not valid JSON: {e}")))?;
        let list = doc
            .get("scorers")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidFixture("scorers file needs a \"scorers\" array".to_string())
            })?;
        if list.is_empty() {
            return Err(Error::InvalidFixture(
                "scorers file needs at least one scorer".to_string(),
            ));
        }
        let mut scorers = Vec::with_capacity(list.len());
        for (i, item) in list.iter().enumerate() {
            scorers.push(entry_from(item, i)?);
        }
        let policy = match doc.get("policy") {
            None | Some(Value::Null) => ScorerPolicy::default(),
            Some(policy) if policy.is_object() => policy_from(policy)?,
            Some(other) => {
                return Err(Error::InvalidFixture(format!(
                    "\"policy\" must be an object, got {other}"
                )));
            }
        };
        policy.validate()?;
        Ok(Self { scorers, policy })
    }

    /// Resolve a `--score-backend` name to its entry. Unknown names
    /// fail here, never after half a rebuild.
    pub fn resolve(&self, backend: &str) -> Result<&ScorerEntry, Error> {
        self.scorers
            .iter()
            .find(|entry| entry.backend == backend)
            .ok_or_else(|| {
                Error::Scorer(format!(
                    "unknown score backend: {backend} (cataloged: {})",
                    self.scorers
                        .iter()
                        .map(|entry| entry.backend.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }
}

fn entry_from(item: &Value, i: usize) -> Result<ScorerEntry, Error> {
    let at = |what: &str| format!("scorers[{i}]: {what}");
    let backend = item
        .get("backend")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::InvalidFixture(at("needs a non-empty \"backend\"")))?;
    if !KNOWN_BACKENDS.contains(&backend) {
        return Err(Error::InvalidFixture(at(&format!(
            "unknown backend {backend:?} (known: {})",
            KNOWN_BACKENDS.join(", ")
        ))));
    }
    let live = backend != "recorded";
    let non_empty = |key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let model = match non_empty("model") {
        Some(model) => model,
        None if live => return Err(Error::InvalidFixture(at("live entries need a \"model\""))),
        None => "recorded".to_string(),
    };
    let base_url = match non_empty("base_url") {
        Some(url) => {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(Error::InvalidFixture(at(
                    "\"base_url\" must start with http:// or https://",
                )));
            }
            url
        }
        None if live => {
            return Err(Error::InvalidFixture(at(
                "live entries need a \"base_url\"",
            )))
        }
        None => String::new(),
    };
    let auth_env = match item.get("auth_env") {
        None => Vec::new(),
        Some(list) => list
            .as_array()
            .ok_or_else(|| Error::InvalidFixture(at("\"auth_env\" must be an array of names")))?
            .iter()
            .map(|name| {
                name.as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        Error::InvalidFixture(at("\"auth_env\" names must be non-empty strings"))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    if live && auth_env.is_empty() {
        return Err(Error::InvalidFixture(at(
            "live entries need \"auth_env\" naming the secret variables",
        )));
    }
    let prompt_version = match non_empty("prompt_version") {
        Some(version) => {
            if live && version != crate::score_openai::PROMPT_VERSION {
                return Err(Error::InvalidFixture(at(&format!(
                    "prompt_version {version:?} does not match the scoring prompt ({}) — update one of them",
                    crate::score_openai::PROMPT_VERSION
                ))));
            }
            version
        }
        None if live => {
            return Err(Error::InvalidFixture(at(
                "live entries need a \"prompt_version\"",
            )))
        }
        None => "recorded".to_string(),
    };
    let cost_per_call_usd = match item.get("cost_per_call_usd") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => {
            let cost = n.as_f64().ok_or_else(|| {
                Error::InvalidFixture(at("\"cost_per_call_usd\" is not a number"))
            })?;
            if !cost.is_finite() || cost < 0.0 {
                return Err(Error::InvalidFixture(at(
                    "\"cost_per_call_usd\" must be finite and >= 0",
                )));
            }
            Some(cost)
        }
        Some(other) => {
            return Err(Error::InvalidFixture(at(&format!(
                "\"cost_per_call_usd\" must be a number, got {other}"
            ))));
        }
    };
    let recording = match non_empty("recording") {
        Some(path) => Some(path),
        None if live => None,
        None => {
            return Err(Error::InvalidFixture(at(
                "recorded entries need a \"recording\" scores file",
            )));
        }
    };
    Ok(ScorerEntry {
        backend: backend.to_string(),
        model,
        base_url,
        auth_env,
        prompt_version,
        cost_per_call_usd,
        recording,
    })
}

fn policy_from(policy: &Value) -> Result<ScorerPolicy, Error> {
    let strings = |key: &str| match policy.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(list)) => list
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        Error::InvalidFixture(format!(
                            "\"policy.{key}\" entries must be non-empty strings"
                        ))
                    })
            })
            .collect(),
        Some(other) => Err(Error::InvalidFixture(format!(
            "\"policy.{key}\" must be an array, got {other}"
        ))),
    };
    let sample_rate = match policy.get("sample_rate") {
        None | Some(Value::Null) => 1.0,
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| {
            Error::InvalidFixture("\"policy.sample_rate\" is not a number".to_string())
        })?,
        Some(other) => {
            return Err(Error::InvalidFixture(format!(
                "\"policy.sample_rate\" must be a number, got {other}"
            )));
        }
    };
    let integer = |key: &str| match policy.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| {
                Error::InvalidFixture(format!("\"policy.{key}\" must be a non-negative integer"))
            })
            .map(Some),
        Some(other) => Err(Error::InvalidFixture(format!(
            "\"policy.{key}\" must be a non-negative integer, got {other}"
        ))),
    };
    let number = |key: &str| match policy.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_f64()
            .ok_or_else(|| Error::InvalidFixture(format!("\"policy.{key}\" is not a number")))
            .map(Some),
        Some(other) => Err(Error::InvalidFixture(format!(
            "\"policy.{key}\" must be a number, got {other}"
        ))),
    };
    let out = ScorerPolicy {
        sample_rate,
        only_models: strings("only_models")?,
        only_providers: strings("only_providers")?,
        max_calls: integer("max_calls")?,
        max_spend_usd: number("max_spend_usd")?,
    };
    out.validate()?;
    Ok(out)
}

/// Deterministic FNV-1a membership: the same endpoint samples the
/// same way on every rebuild, independent of catalog order or size.
/// Keyed by model + provider + tag: same-provider tag variants are
/// distinct endpoints and must sample independently (the DB `id`
/// would do it too, but re-sync reassigns ids every cycle).
fn sampled(endpoint: &EndpointRow, rate: f64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    // Unparseable or untagged payloads share the untagged bucket.
    let tag = serde_json::from_str::<Value>(&endpoint.payload)
        .ok()
        .and_then(|payload| crate::score::endpoint_tag(&payload).map(str::to_string))
        .unwrap_or_default();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in endpoint
        .model_id
        .bytes()
        .chain([0])
        .chain(endpoint.provider_name.bytes())
        .chain([0])
        .chain(tag.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % 10_000) as f64 / 10_000.0 < rate
}

/// Policy-selected endpoints for a rebuild, in catalog order.
pub fn select_endpoints(endpoints: &[EndpointRow], policy: &ScorerPolicy) -> Vec<EndpointRow> {
    endpoints
        .iter()
        .filter(|ep| policy.in_scope(ep) && sampled(ep, policy.sample_rate))
        .cloned()
        .collect()
}

/// Costed call plan for a live rebuild.
#[derive(Debug, Clone, PartialEq)]
pub struct ScorerPlan {
    pub calls: usize,
    /// `None` when the per-call cost is unknown.
    pub spend_usd: Option<f64>,
}

/// Check call/spend caps *before* any score is written (Iter 15):
/// a breached cap fails closed with the DB untouched. Unknown
/// per-call cost fails a spend cap rather than guessing it.
pub fn plan_live_calls(
    selected: usize,
    entry: &ScorerEntry,
    policy: &ScorerPolicy,
) -> Result<ScorerPlan, Error> {
    if policy.max_calls.is_some_and(|cap| selected as u64 > cap) {
        return Err(Error::Scorer(format!(
            "live scoring needs {selected} calls, over the max_calls cap ({})",
            policy.max_calls.unwrap_or(0)
        )));
    }
    let spend_usd = entry.cost_per_call_usd.map(|cost| cost * selected as f64);
    if let Some(cap) = policy.max_spend_usd {
        match spend_usd {
            None => {
                return Err(Error::Scorer(
                    "cannot enforce max_spend_usd with unknown cost_per_call_usd".to_string(),
                ));
            }
            Some(spend) if spend > cap => {
                return Err(Error::Scorer(format!(
                    "live scoring would spend ${spend:.4}, over the max_spend_usd cap (${cap:.4})"
                )));
            }
            Some(_) => {}
        }
    }
    Ok(ScorerPlan {
        calls: selected,
        spend_usd,
    })
}

/// Read the secret variables an entry names (Iter 15): values travel
/// from the environment straight into the scorer constructor — never
/// through config, logs, or errors (only variable *names* appear).
fn read_auth(entry: &ScorerEntry, want: usize) -> Result<Vec<String>, Error> {
    if entry.auth_env.len() != want {
        return Err(Error::InvalidFixture(format!(
            "backend {:?} needs {want} auth variable(s), catalog lists {}",
            entry.backend,
            entry.auth_env.len()
        )));
    }
    entry
        .auth_env
        .iter()
        .map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::Scorer(format!("{name} is not set (see .env.example)")))
        })
        .collect()
}

/// Construct the cataloged scorer (Iter 15). Live backends read keys
/// from the environment; the recorded backend reads its file.
/// Construction never touches the network, so unknown backends and
/// missing secrets fail before any score is written.
pub fn build_score(entry: &ScorerEntry) -> Result<Box<dyn crate::score::Scorer>, Error> {
    match entry.backend.as_str() {
        "recorded" => {
            let path = entry.recording.as_deref().ok_or_else(|| {
                Error::InvalidFixture(
                    "recorded entries need a \"recording\" scores file".to_string(),
                )
            })?;
            Ok(Box::new(RecordedScorer::load(std::path::Path::new(path))?))
        }
        "baseten" => {
            let vars = read_auth(entry, 1)?;
            Ok(Box::new(BasetenScorer::new(
                &entry.base_url,
                &vars[0],
                &entry.model,
            )?))
        }
        "modal" => {
            let vars = read_auth(entry, 2)?;
            // B21: accept deployment-root URLs like from_env (a
            // /v1-suffixed entry passes through unchanged).
            let base_url = crate::score_modal::api_root(&entry.base_url);
            Ok(Box::new(ModalScorer::new(
                &base_url,
                &vars[0],
                &vars[1],
                &entry.model,
            )?))
        }
        "openrouter" => {
            let vars = read_auth(entry, 1)?;
            Ok(Box::new(OpenRouterScorer::new(
                &entry.base_url,
                &vars[0],
                &entry.model,
            )?))
        }
        backend => Err(Error::Scorer(format!("unknown score backend: {backend}"))),
    }
}

/// One log line per live rebuild: calls, unit cost, total, caps.
pub fn spend_line(plan: &ScorerPlan, entry: &ScorerEntry, policy: &ScorerPolicy) -> String {
    let unit = entry
        .cost_per_call_usd
        .map(|c| format!("${c:.6}"))
        .unwrap_or_else(|| "unknown".to_string());
    let total = plan
        .spend_usd
        .map(|s| format!("${s:.4}"))
        .unwrap_or_else(|| "unknown".to_string());
    let caps = match (policy.max_calls, policy.max_spend_usd) {
        (None, None) => "no caps".to_string(),
        (calls, spend) => format!(
            "caps calls={} spend={}",
            calls
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string()),
            spend
                .map(|s| format!("${s:.4}"))
                .unwrap_or_else(|| "-".to_string())
        ),
    };
    format!(
        "score spend: {} live calls x {unit} = {total} ({caps})",
        plan.calls
    )
}
