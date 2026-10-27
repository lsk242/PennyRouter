//! Ranking rebuild (Iter 6): catalog signals + usage trends + recorded
//! AI adjustments into a versioned ranking artifact.
//!
//! The ranker is a periodic offline job. It never runs per request: the
//! gateway serves the published ordering with a single lookup and keeps
//! the last good artifact when a rebuild fails. Trends and AI
//! adjustments arrive as files (fixtures until live collection lands),
//! so rebuilds are byte-deterministic for the same inputs.

use crate::store::Catalog;
use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Ranking orientation: the default plus the tag-selectable orderings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankOrientation {
    Default,
    Quality,
    Price,
}

/// Comparable signals for one offering. Trend fields are `None` when no
/// usage data covers the (model, provider) pair.
pub struct Signals<'a> {
    pub provider: &'a str,
    pub tag: Option<&'a str>,
    pub price: Option<f64>,
    pub tier: Option<&'a str>,
    pub score: Option<f64>,
    pub error_rate: Option<f64>,
    pub latency_ms: Option<f64>,
}

fn tier_rank(tier: Option<&str>) -> u8 {
    match tier {
        Some("high") => 3,
        Some("standard") => 2,
        Some("low") => 1,
        _ => 0,
    }
}

fn asc_nulls_last(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Orientation ordering over signals. Unknown error rates count as zero
/// (no evidence of failures); unknown latencies sort after known ones.
/// Provider and tag break all remaining ties, so rebuilds are stable.
pub fn compare(a: &Signals, b: &Signals, orientation: RankOrientation) -> std::cmp::Ordering {
    let price = || asc_nulls_last(a.price, b.price);
    let errors = || {
        asc_nulls_last(
            Some(a.error_rate.unwrap_or(0.0)),
            Some(b.error_rate.unwrap_or(0.0)),
        )
    };
    let latency = || asc_nulls_last(a.latency_ms, b.latency_ms);
    let tier = || tier_rank(b.tier).cmp(&tier_rank(a.tier));
    let score = || asc_nulls_last(b.score, a.score);
    let identity = || a.provider.cmp(b.provider).then_with(|| a.tag.cmp(&b.tag));
    match orientation {
        RankOrientation::Default => tier()
            .then_with(errors)
            .then_with(price)
            .then_with(latency)
            .then_with(identity),
        RankOrientation::Price => price()
            .then_with(errors)
            .then_with(tier)
            .then_with(latency)
            .then_with(identity),
        RankOrientation::Quality => tier()
            .then_with(score)
            .then_with(errors)
            .then_with(price)
            .then_with(identity),
    }
}

/// Aggregated usage for one (model, provider) pair across apps.
#[derive(Debug, Clone, Default)]
pub struct TrendStats {
    pub requests: u64,
    pub errors: u64,
    pub latency_ms_sum: f64,
    pub latency_samples: u64,
}

impl TrendStats {
    pub fn error_rate(&self) -> Option<f64> {
        (self.requests > 0).then(|| self.errors as f64 / self.requests as f64)
    }

    pub fn latency_ms(&self) -> Option<f64> {
        (self.latency_samples > 0).then(|| self.latency_ms_sum / self.latency_samples as f64)
    }
}

/// Per-app usage trends, aggregated per (model, provider).
/// File shape: `{"trends": [{"app": "key-id", "model_id": "m",
/// "provider_name": "p", "requests": 10, "errors": 1,
/// "avg_latency_ms": 120.5}]}`. `avg_latency_ms` may be null.
#[derive(Debug, Clone, Default)]
pub struct Trends {
    by_endpoint: HashMap<(String, String), TrendStats>,
}

impl Trends {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        Self::from_str(&std::fs::read_to_string(path)?)
    }

    fn from_str(raw: &str) -> Result<Self, Error> {
        let doc: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| Error::InvalidFixture(format!("trends file is not valid JSON: {e}")))?;
        let list = doc
            .get("trends")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                Error::InvalidFixture("trends file needs a \"trends\" array".to_string())
            })?;
        let mut by_endpoint: HashMap<(String, String), TrendStats> = HashMap::new();
        for (i, item) in list.iter().enumerate() {
            let at = |what: &str| format!("trends[{i}]: {what}");
            let need_str = |key: &str| {
                item.get(key)
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        Error::InvalidFixture(at(&format!("needs a non-empty \"{key}\"")))
                    })
            };
            let model_id = need_str("model_id")?;
            let provider_name = need_str("provider_name")?;
            let requests = item
                .get("requests")
                .and_then(serde_json::Value::as_u64)
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    Error::InvalidFixture(at("\"requests\" must be a positive integer"))
                })?;
            let errors = item
                .get("errors")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    Error::InvalidFixture(at("\"errors\" must be a non-negative integer"))
                })?;
            if errors > requests {
                return Err(Error::InvalidFixture(at("\"errors\" exceeds \"requests\"")));
            }
            let latency = match item.get("avg_latency_ms") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::Number(n)) => {
                    let ms = n.as_f64().ok_or_else(|| {
                        Error::InvalidFixture(at("\"avg_latency_ms\" is not a number"))
                    })?;
                    if ms < 0.0 {
                        return Err(Error::InvalidFixture(at("\"avg_latency_ms\" must be >= 0")));
                    }
                    Some(ms)
                }
                Some(other) => {
                    return Err(Error::InvalidFixture(at(&format!(
                        "\"avg_latency_ms\" must be a number, got {other}"
                    ))));
                }
            };
            let stats = by_endpoint
                .entry((model_id.to_string(), provider_name.to_string()))
                .or_default();
            stats.requests += requests;
            stats.errors += errors;
            if let Some(ms) = latency {
                stats.latency_ms_sum += ms * requests as f64;
                stats.latency_samples += requests;
            }
        }
        Ok(Self { by_endpoint })
    }

    pub fn get(&self, model_id: &str, provider_name: &str) -> Option<&TrendStats> {
        self.by_endpoint
            .get(&(model_id.to_string(), provider_name.to_string()))
    }
}

/// Recorded AI adjustments: preferential ordering overrides.
/// File shape: `{"adjustments": [{"model_id": "m", "provider_name":
/// "p", "action": "prefer"|"avoid", "reason": "..."}]}`. Unknown
/// endpoints are ignored; the last record wins per (model, provider).
#[derive(Debug, Clone, Default)]
pub struct Assist {
    action: HashMap<(String, String), bool>,
}

impl Assist {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        Self::from_str(&std::fs::read_to_string(path)?)
    }

    fn from_str(raw: &str) -> Result<Self, Error> {
        let doc: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| Error::InvalidFixture(format!("assist file is not valid JSON: {e}")))?;
        let list = doc
            .get("adjustments")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                Error::InvalidFixture("assist file needs an \"adjustments\" array".to_string())
            })?;
        let mut action: HashMap<(String, String), bool> = HashMap::new();
        for (i, item) in list.iter().enumerate() {
            let at = |what: &str| format!("adjustments[{i}]: {what}");
            let need_str = |key: &str| {
                item.get(key)
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        Error::InvalidFixture(at(&format!("needs a non-empty \"{key}\"")))
                    })
            };
            let model_id = need_str("model_id")?;
            let provider_name = need_str("provider_name")?;
            let _reason = need_str("reason")?;
            let prefer = match item.get("action").and_then(serde_json::Value::as_str) {
                Some("prefer") => true,
                Some("avoid") => false,
                other => {
                    return Err(Error::InvalidFixture(at(&format!(
                        "\"action\" must be prefer/avoid, got {other:?}"
                    ))));
                }
            };
            action.insert((model_id.to_string(), provider_name.to_string()), prefer);
        }
        Ok(Self { action })
    }

    /// Some(true) = prefer (front), Some(false) = avoid (back), None = neutral.
    pub fn get(&self, model_id: &str, provider_name: &str) -> Option<bool> {
        self.action
            .get(&(model_id.to_string(), provider_name.to_string()))
            .copied()
    }
}

/// One ranked offering: provider plus offering tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankEntry {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// Per-precision-class orderings for one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassRanking {
    pub class: Option<String>,
    pub default: Vec<RankEntry>,
    pub price: Vec<RankEntry>,
    pub quality: Vec<RankEntry>,
}

/// Ranking artifact content. Models sort by key (BTreeMap) so rebuilds
/// from the same inputs are byte-identical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankingDocument {
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_version: Option<String>,
    pub models: BTreeMap<String, Vec<ClassRanking>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RankingManifest {
    pub version: String,
    pub sha256: String,
    pub ranking_file: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_version: Option<String>,
    pub model_count: u64,
}

/// Rebuild the ranking from a catalog DB plus trends and AI adjustments.
/// Deterministic: same inputs always produce the same document.
pub fn rank_catalog(
    catalog: &Catalog,
    trends: &Trends,
    assist: &Assist,
    created_at: &str,
    catalog_version: Option<String>,
) -> Result<RankingDocument, Error> {
    let mut by_model: BTreeMap<String, Vec<ClassRanking>> = BTreeMap::new();
    // Models in first-seen order for stable class grouping; the map
    // sorts keys on serialization.
    let mut seen_models: Vec<String> = Vec::new();
    let mut endpoints_by_model: HashMap<String, Vec<usize>> = HashMap::new();
    let endpoints = catalog.all_endpoints()?;
    for (i, ep) in endpoints.iter().enumerate() {
        if !endpoints_by_model.contains_key(&ep.model_id) {
            seen_models.push(ep.model_id.clone());
        }
        endpoints_by_model
            .entry(ep.model_id.clone())
            .or_default()
            .push(i);
    }
    for model_id in &seen_models {
        let Some(indices) = endpoints_by_model.get(model_id) else {
            continue;
        };
        // Precision classes in first-seen order.
        let mut classes: Vec<Option<String>> = Vec::new();
        for i in indices {
            let class = endpoints[*i].precision_class.clone();
            if !classes.contains(&class) {
                classes.push(class);
            }
        }
        let mut rankings = Vec::new();
        for class in classes {
            let members: Vec<usize> = indices
                .iter()
                .copied()
                .filter(|i| endpoints[*i].precision_class == class)
                .collect();
            let order = |orientation| {
                let mut sorted = members.clone();
                sorted.sort_by(|a, b| {
                    compare(
                        &signals(&endpoints[*a], trends),
                        &signals(&endpoints[*b], trends),
                        orientation,
                    )
                });
                apply_assist(model_id, &sorted, &endpoints, assist)
                    .into_iter()
                    .map(|i| RankEntry {
                        provider: endpoints[i].provider_name.clone(),
                        tag: endpoint_tag_of(&endpoints[i]),
                    })
                    .collect()
            };
            rankings.push(ClassRanking {
                class,
                default: order(RankOrientation::Default),
                price: order(RankOrientation::Price),
                quality: order(RankOrientation::Quality),
            });
        }
        by_model.insert(model_id.clone(), rankings);
    }
    Ok(RankingDocument {
        created_at: created_at.to_string(),
        catalog_version,
        models: by_model,
    })
}

fn signals<'a>(ep: &'a crate::store::EndpointRow, trends: &'a Trends) -> Signals<'a> {
    let trend = trends.get(&ep.model_id, &ep.provider_name);
    Signals {
        provider: &ep.provider_name,
        tag: None, // tag identity comes from the payload at entry build
        price: ep.price_prompt,
        tier: ep.quality_tier.as_deref(),
        score: ep.quality_score,
        error_rate: trend.and_then(TrendStats::error_rate),
        latency_ms: trend.and_then(TrendStats::latency_ms),
    }
}

fn endpoint_tag_of(ep: &crate::store::EndpointRow) -> Option<String> {
    serde_json::from_str(&ep.payload)
        .ok()
        .and_then(|v: serde_json::Value| crate::judge::endpoint_tag(&v).map(str::to_string))
}

/// Stable partition: preferred providers first, avoided last, relative
/// order preserved within each band.
fn apply_assist(
    model_id: &str,
    sorted: &[usize],
    endpoints: &[crate::store::EndpointRow],
    assist: &Assist,
) -> Vec<usize> {
    let mut prefer = Vec::new();
    let mut neutral = Vec::new();
    let mut avoid = Vec::new();
    for i in sorted {
        match assist.get(model_id, &endpoints[*i].provider_name) {
            Some(true) => prefer.push(*i),
            Some(false) => avoid.push(*i),
            None => neutral.push(*i),
        }
    }
    prefer.extend(neutral);
    prefer.extend(avoid);
    prefer
}

/// Publish a ranking document as `ranking-<version>.json` plus a
/// `ranking-<version>.manifest.json` sidecar. The version is the content
/// hash prefix, so identical rebuilds are idempotent.
pub fn publish_ranking(
    doc: &RankingDocument,
    out_dir: &Path,
) -> Result<(PathBuf, PathBuf, RankingManifest), Error> {
    std::fs::create_dir_all(out_dir)?;
    let bytes = serde_json::to_string(doc)?.into_bytes();
    let sha256 = crate::artifact::sha256_hex(&bytes);
    let version = sha256[..16].to_string();
    let file_name = format!("ranking-{version}.json");
    let manifest_name = format!("ranking-{version}.manifest.json");
    let file_path = out_dir.join(&file_name);
    let manifest_path = out_dir.join(&manifest_name);
    std::fs::write(&file_path, &bytes)?;
    let manifest = RankingManifest {
        version,
        sha256,
        ranking_file: file_name,
        created_at: doc.created_at.clone(),
        catalog_version: doc.catalog_version.clone(),
        model_count: doc.models.len() as u64,
    };
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
    Ok((file_path, manifest_path, manifest))
}

/// Verify a published ranking: manifest parses and the file hash matches.
pub fn verify_ranking(manifest_path: &Path) -> Result<bool, Error> {
    let raw = std::fs::read(manifest_path)?;
    let manifest: RankingManifest = serde_json::from_slice(&raw)?;
    let Some(dir) = manifest_path.parent() else {
        return Ok(false);
    };
    let bytes = std::fs::read(dir.join(&manifest.ranking_file))?;
    Ok(crate::artifact::sha256_hex(&bytes) == manifest.sha256)
}

/// Load a published ranking document.
pub fn load_ranking_doc(path: &Path) -> Result<RankingDocument, Error> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig<'a>(provider: &'a str, price: Option<f64>, tier: Option<&'a str>) -> Signals<'a> {
        Signals {
            provider,
            tag: None,
            price,
            tier,
            score: None,
            error_rate: None,
            latency_ms: None,
        }
    }

    #[test]
    fn orientations_order_signals() {
        use std::cmp::Ordering::*;
        use RankOrientation::*;
        let cheap_low = sig("c", Some(1.0), Some("low"));
        let dear_high = sig("d", Some(9.0), Some("high"));
        assert_eq!(compare(&cheap_low, &dear_high, Default), Greater);
        assert_eq!(compare(&cheap_low, &dear_high, Price), Less);
        assert_eq!(compare(&cheap_low, &dear_high, Quality), Greater);
    }

    #[test]
    fn unknown_trends_are_neutral_and_ties_break_by_identity() {
        use RankOrientation::*;
        let flaky = Signals {
            error_rate: Some(0.5),
            ..sig("b", Some(1.0), Some("high"))
        };
        let fresh = sig("a", Some(1.0), Some("high"));
        assert_eq!(compare(&fresh, &flaky, Default), std::cmp::Ordering::Less);
        let twin_b = sig("b", Some(1.0), Some("high"));
        assert_eq!(compare(&fresh, &twin_b, Price), std::cmp::Ordering::Less);
    }

    #[test]
    fn invalid_trends_and_assist_fail_at_load() {
        assert!(Trends::from_str(r#"{"trends": []}"#).is_ok());
        for raw in [
            r#"{"trends": {}}"#,
            r#"{"trends": [{"model_id": "m", "provider_name": "p",
                "requests": 0, "errors": 0}]}"#,
            r#"{"trends": [{"model_id": "m", "provider_name": "p",
                "requests": 2, "errors": 3}]}"#,
            r#"{"trends": [{"model_id": "", "provider_name": "p",
                "requests": 1, "errors": 0}]}"#,
        ] {
            assert!(Trends::from_str(raw).is_err(), "should reject: {raw}");
        }
        assert!(Assist::from_str(r#"{"adjustments": []}"#).is_ok());
        for raw in [
            r#"{"adjustments": [{"model_id": "m", "provider_name": "p",
                "action": "boost", "reason": "r"}]}"#,
            r#"{"adjustments": [{"model_id": "m", "provider_name": "p",
                "action": "prefer", "reason": ""}]}"#,
        ] {
            assert!(Assist::from_str(raw).is_err(), "should reject: {raw}");
        }
    }
}
