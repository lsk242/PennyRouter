//! AI-assisted processing: model-scored signals into indexed columns.
//!
//! Phase 2b plugs behind the same per-endpoint update hook Phase 2a
//! establishes. The recorded scorer reads model outputs from a JSON file
//! (no live model calls); live scorers (`BasetenScorer`, `ModalScorer`,
//! `OpenRouterScorer`) implement the same trait without touching the store.

use crate::store::{Catalog, EndpointScore};
use crate::Error;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Model-scored signals for one endpoint. `None` means "no signal".
pub struct Score {
    pub quality_tier: Option<String>,
    pub quality_score: Option<f64>,
}

/// Audit identity of a scorer (Iter 15): the model that produced the
/// signals and the prompt version it answered. Stored with every
/// score and in the rebuild meta, so a tier can always be traced
/// back to the exact scorer + prompt behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScorerIdentity {
    pub model: String,
    pub prompt_version: String,
}

/// Scorer hook: signals for one endpoint, given its identity and raw
/// payload. Implementations must be deterministic for the same input.
pub trait Scorer {
    fn score(&self, model_id: &str, provider_name: &str, payload: &Value) -> Result<Score, Error>;

    /// Who to blame in the audit trail. The default (`unknown`) keeps
    /// third-party impls compiling; every in-tree scorer overrides it.
    fn identity(&self) -> ScorerIdentity {
        ScorerIdentity {
            model: "unknown".to_string(),
            prompt_version: "unknown".to_string(),
        }
    }
}

/// Scorer that records "no signal" everywhere. Used when scoring runs
/// without a recording so endpoints still get a `scored_at` stamp.
pub struct NullScorer;

impl Scorer for NullScorer {
    fn score(&self, _: &str, _: &str, _: &Value) -> Result<Score, Error> {
        Ok(Score {
            quality_tier: None,
            quality_score: None,
        })
    }

    fn identity(&self) -> ScorerIdentity {
        ScorerIdentity {
            model: "none".to_string(),
            prompt_version: "none".to_string(),
        }
    }
}

/// One recorded model output. `tag` narrows the match to one offering
/// of a (model, provider) pair; omitted it matches every offering.
struct Record {
    tag: Option<String>,
    quality_tier: Option<String>,
    quality_score: Option<f64>,
}

/// Scorer backed by a recorded-output file. File shape:
///
/// ```json
/// {"scores": [{"model_id": "m", "provider_name": "p", "tag": "t",
///   "quality_tier": "high", "quality_score": 0.9}]}
/// ```
///
/// `quality_tier` is one of `high`/`standard`/`low`. Records for unknown
/// endpoints are ignored; several records may match one endpoint and the
/// last one wins. Invalid files fail at load, never mid-run.
pub struct RecordedScorer {
    by_endpoint: HashMap<(String, String), Vec<Record>>,
}

impl RecordedScorer {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_str(&raw)
    }

    fn from_str(raw: &str) -> Result<Self, Error> {
        let doc: Value = serde_json::from_str(raw)
            .map_err(|e| Error::InvalidFixture(format!("scores file is not valid JSON: {e}")))?;
        let list = doc.get("scores").and_then(Value::as_array).ok_or_else(|| {
            Error::InvalidFixture("scores file needs a \"scores\" array".to_string())
        })?;
        let mut by_endpoint: HashMap<(String, String), Vec<Record>> = HashMap::new();
        for (i, item) in list.iter().enumerate() {
            let at = |what: &str| format!("scores[{i}]: {what}");
            let model_id = item
                .get("model_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::InvalidFixture(at("needs a non-empty \"model_id\"")))?;
            let provider_name = item
                .get("provider_name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::InvalidFixture(at("needs a non-empty \"provider_name\"")))?;
            // A wrong-type tag is rejected, never widened to an
            // unscoped match-all (Iter 11, A05).
            let tag = match item.get("tag") {
                None => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| Error::InvalidFixture(at("\"tag\" must be a string")))?
                        .to_string(),
                ),
            };
            let quality_tier = match item.get("quality_tier") {
                None | Some(Value::Null) => None,
                Some(Value::String(t)) if ["high", "standard", "low"].contains(&t.as_str()) => {
                    Some(t.clone())
                }
                Some(other) => {
                    return Err(Error::InvalidFixture(at(&format!(
                        "\"quality_tier\" must be high/standard/low, got {other}"
                    ))));
                }
            };
            let quality_score = match item.get("quality_score") {
                None | Some(Value::Null) => None,
                Some(Value::Number(n)) => {
                    let score = n.as_f64().ok_or_else(|| {
                        Error::InvalidFixture(at("\"quality_score\" is not a number"))
                    })?;
                    if !(0.0..=1.0).contains(&score) {
                        return Err(Error::InvalidFixture(at(
                            "\"quality_score\" must be within 0.0..=1.0",
                        )));
                    }
                    Some(score)
                }
                Some(other) => {
                    return Err(Error::InvalidFixture(at(&format!(
                        "\"quality_score\" must be a number, got {other}"
                    ))));
                }
            };
            by_endpoint
                .entry((model_id.to_string(), provider_name.to_string()))
                .or_default()
                .push(Record {
                    tag,
                    quality_tier,
                    quality_score,
                });
        }
        Ok(Self { by_endpoint })
    }
}

/// The offering tag inside an endpoint payload, if present.
pub fn endpoint_tag(payload: &Value) -> Option<&str> {
    payload.get("tag").and_then(Value::as_str)
}

impl Scorer for RecordedScorer {
    /// Recordings carry signals, not provenance: the audit trail says
    /// `recorded`, honestly marking scores whose original model and
    /// prompt are no longer known.
    fn identity(&self) -> ScorerIdentity {
        ScorerIdentity {
            model: "recorded".to_string(),
            prompt_version: "recorded".to_string(),
        }
    }

    fn score(&self, model_id: &str, provider_name: &str, payload: &Value) -> Result<Score, Error> {
        let mut out = Score {
            quality_tier: None,
            quality_score: None,
        };
        let key = (model_id.to_string(), provider_name.to_string());
        if let Some(records) = self.by_endpoint.get(&key) {
            let tag = endpoint_tag(payload);
            for record in records {
                if record.tag.as_deref().is_some_and(|t| Some(t) != tag) {
                    continue;
                }
                if record.quality_tier.is_some() {
                    out.quality_tier.clone_from(&record.quality_tier);
                }
                if record.quality_score.is_some() {
                    out.quality_score = record.quality_score;
                }
            }
        }
        Ok(out)
    }
}

pub struct ScorerSummary {
    pub scored: usize,
    pub matched: usize,
}

/// Fill the score columns for every endpoint. Idempotent:
/// re-running overwrites the same columns. Touching only score
/// columns — deterministic values from Phase 2a are left alone.
pub fn run_scoring(
    catalog: &Catalog,
    score: &dyn Scorer,
    scored_at: &str,
) -> Result<ScorerSummary, Error> {
    let endpoints = catalog.all_endpoints()?;
    run_scoring_selected(catalog, score, &endpoints, scored_at)
}

/// Fill the score columns for exactly `endpoints` (Iter 15: the
/// policy-selected subset). Every written row carries the score's
/// audit identity; the rebuild meta records it too.
pub fn run_scoring_selected(
    catalog: &Catalog,
    score: &dyn Scorer,
    endpoints: &[crate::store::EndpointRow],
    scored_at: &str,
) -> Result<ScorerSummary, Error> {
    let identity = score.identity();
    // One transaction for the whole run (plus its meta): a mid-run
    // failure rolls every score back instead of half-scoring. The
    // pipeline scores a private work DB, so the lock never blocks
    // gateway readers (they see only published artifacts).
    catalog.transaction(|| {
        let mut scored = 0;
        let mut matched = 0;
        for ep in endpoints {
            let payload: Value = serde_json::from_str(&ep.payload)?;
            let score = score.score(&ep.model_id, &ep.provider_name, &payload)?;
            if score.quality_tier.is_some() || score.quality_score.is_some() {
                matched += 1;
            }
            catalog.update_endpoint_score(
                ep.id,
                &EndpointScore {
                    quality_tier: score.quality_tier,
                    quality_score: score.quality_score,
                    scored_at: scored_at.to_string(),
                    score_model: Some(identity.model.clone()),
                    score_prompt: Some(identity.prompt_version.clone()),
                },
            )?;
            scored += 1;
        }
        catalog.set_meta("scored_at", scored_at)?;
        catalog.set_meta("scored_count", &scored.to_string())?;
        catalog.set_meta("score_model", &identity.model)?;
        catalog.set_meta("score_prompt", &identity.prompt_version)?;
        Ok(ScorerSummary { scored, matched })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recorded(raw: &str) -> RecordedScorer {
        RecordedScorer::from_str(raw).unwrap()
    }

    #[test]
    fn tag_narrows_the_match() {
        let score = recorded(
            r#"{"scores": [
                {"model_id": "m", "provider_name": "p",
                 "quality_tier": "low", "quality_score": 0.1},
                {"model_id": "m", "provider_name": "p", "tag": "fast",
                 "quality_tier": "high", "quality_score": 0.9}
            ]}"#,
        );
        let tagged = score.score("m", "p", &json!({"tag": "fast"})).unwrap();
        assert_eq!(tagged.quality_tier.as_deref(), Some("high"));
        assert_eq!(tagged.quality_score, Some(0.9));
        // Untagged record matches every offering.
        let other = score.score("m", "p", &json!({"tag": "slow"})).unwrap();
        assert_eq!(other.quality_tier.as_deref(), Some("low"));
        assert_eq!(other.quality_score, Some(0.1));
    }

    #[test]
    fn unknown_endpoints_get_no_signal() {
        let score = recorded(r#"{"scores": []}"#);
        let out = score.score("m", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier, None);
        assert_eq!(out.quality_score, None);
    }

    /// A05: a wrong-type tag fails the file instead of widening to a
    /// match-all record.
    #[test]
    fn wrong_type_tag_rejected() {
        let raw = r#"{"scores": [
            {"model_id": "m", "provider_name": "p", "tag": 42,
             "quality_tier": "high", "quality_score": 0.9}
        ]}"#;
        assert!(RecordedScorer::from_str(raw).is_err());
    }

    #[test]
    fn invalid_recordings_fail_at_load() {
        let bad = [
            r#"{"scores": "nope"}"#,
            r#"{"scores": [{"provider_name": "p"}]}"#,
            r#"{"scores": [{"model_id": "m", "provider_name": ""}]}"#,
            r#"{"scores": [{"model_id": "m", "provider_name": "p",
                "quality_tier": "ultra"}]}"#,
            r#"{"scores": [{"model_id": "m", "provider_name": "p",
                "quality_score": 1.5}]}"#,
            r#"{"scores": [{"model_id": "m", "provider_name": "p",
                "quality_score": "high"}]}"#,
        ];
        for raw in bad {
            assert!(
                RecordedScorer::from_str(raw).is_err(),
                "should reject: {raw}"
            );
        }
    }
}
