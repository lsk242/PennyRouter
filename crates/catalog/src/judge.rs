//! AI-assisted processing: model-judged signals into indexed columns.
//!
//! Phase 2b plugs behind the same per-endpoint update hook Phase 2a
//! establishes. The recorded judge reads model outputs from a JSON file
//! (no live model calls); live judges (`BasetenJudge`, later Modal)
//! implement the same trait without touching the store.

use crate::store::{Catalog, EndpointJudgment};
use crate::Error;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Model-judged signals for one endpoint. `None` means "no signal".
pub struct Judgment {
    pub quality_tier: Option<String>,
    pub quality_score: Option<f64>,
}

/// Judge hook: signals for one endpoint, given its identity and raw
/// payload. Implementations must be deterministic for the same input.
pub trait Judge {
    fn judge(
        &self,
        model_id: &str,
        provider_name: &str,
        payload: &Value,
    ) -> Result<Judgment, Error>;
}

/// Judge that records "no signal" everywhere. Used when judging runs
/// without a recording so endpoints still get a `judged_at` stamp.
pub struct NullJudge;

impl Judge for NullJudge {
    fn judge(&self, _: &str, _: &str, _: &Value) -> Result<Judgment, Error> {
        Ok(Judgment {
            quality_tier: None,
            quality_score: None,
        })
    }
}

/// One recorded model output. `tag` narrows the match to one offering
/// of a (model, provider) pair; omitted it matches every offering.
struct Record {
    tag: Option<String>,
    quality_tier: Option<String>,
    quality_score: Option<f64>,
}

/// Judge backed by a recorded-output file. File shape:
///
/// ```json
/// {"judgments": [{"model_id": "m", "provider_name": "p", "tag": "t",
///   "quality_tier": "high", "quality_score": 0.9}]}
/// ```
///
/// `quality_tier` is one of `high`/`standard`/`low`. Records for unknown
/// endpoints are ignored; several records may match one endpoint and the
/// last one wins. Invalid files fail at load, never mid-run.
pub struct RecordedJudge {
    by_endpoint: HashMap<(String, String), Vec<Record>>,
}

impl RecordedJudge {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_str(&raw)
    }

    fn from_str(raw: &str) -> Result<Self, Error> {
        let doc: Value = serde_json::from_str(raw)
            .map_err(|e| Error::InvalidFixture(format!("judgments file is not valid JSON: {e}")))?;
        let list = doc
            .get("judgments")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidFixture("judgments file needs a \"judgments\" array".to_string())
            })?;
        let mut by_endpoint: HashMap<(String, String), Vec<Record>> = HashMap::new();
        for (i, item) in list.iter().enumerate() {
            let at = |what: &str| format!("judgments[{i}]: {what}");
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
            let tag = item.get("tag").and_then(Value::as_str).map(str::to_string);
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

impl Judge for RecordedJudge {
    fn judge(
        &self,
        model_id: &str,
        provider_name: &str,
        payload: &Value,
    ) -> Result<Judgment, Error> {
        let mut out = Judgment {
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

pub struct JudgeSummary {
    pub judged: usize,
    pub matched: usize,
}

/// Fill the judgment columns for every endpoint. Idempotent:
/// re-running overwrites the same columns. Touching only judgment
/// columns — deterministic values from Phase 2a are left alone.
pub fn run_judging(
    catalog: &Catalog,
    judge: &dyn Judge,
    judged_at: &str,
) -> Result<JudgeSummary, Error> {
    let endpoints = catalog.all_endpoints()?;
    let mut judged = 0;
    let mut matched = 0;
    for ep in &endpoints {
        let payload: Value = serde_json::from_str(&ep.payload)?;
        let judgment = judge.judge(&ep.model_id, &ep.provider_name, &payload)?;
        if judgment.quality_tier.is_some() || judgment.quality_score.is_some() {
            matched += 1;
        }
        catalog.update_endpoint_judgment(
            ep.id,
            &EndpointJudgment {
                quality_tier: judgment.quality_tier,
                quality_score: judgment.quality_score,
                judged_at: judged_at.to_string(),
            },
        )?;
        judged += 1;
    }
    catalog.set_meta("judged_at", judged_at)?;
    catalog.set_meta("judged_count", &judged.to_string())?;
    Ok(JudgeSummary { judged, matched })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recorded(raw: &str) -> RecordedJudge {
        RecordedJudge::from_str(raw).unwrap()
    }

    #[test]
    fn tag_narrows_the_match() {
        let judge = recorded(
            r#"{"judgments": [
                {"model_id": "m", "provider_name": "p",
                 "quality_tier": "low", "quality_score": 0.1},
                {"model_id": "m", "provider_name": "p", "tag": "fast",
                 "quality_tier": "high", "quality_score": 0.9}
            ]}"#,
        );
        let tagged = judge.judge("m", "p", &json!({"tag": "fast"})).unwrap();
        assert_eq!(tagged.quality_tier.as_deref(), Some("high"));
        assert_eq!(tagged.quality_score, Some(0.9));
        // Untagged record matches every offering.
        let other = judge.judge("m", "p", &json!({"tag": "slow"})).unwrap();
        assert_eq!(other.quality_tier.as_deref(), Some("low"));
        assert_eq!(other.quality_score, Some(0.1));
    }

    #[test]
    fn unknown_endpoints_get_no_signal() {
        let judge = recorded(r#"{"judgments": []}"#);
        let out = judge.judge("m", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier, None);
        assert_eq!(out.quality_score, None);
    }

    #[test]
    fn invalid_recordings_fail_at_load() {
        let bad = [
            r#"{"judgments": "nope"}"#,
            r#"{"judgments": [{"provider_name": "p"}]}"#,
            r#"{"judgments": [{"model_id": "m", "provider_name": ""}]}"#,
            r#"{"judgments": [{"model_id": "m", "provider_name": "p",
                "quality_tier": "ultra"}]}"#,
            r#"{"judgments": [{"model_id": "m", "provider_name": "p",
                "quality_score": 1.5}]}"#,
            r#"{"judgments": [{"model_id": "m", "provider_name": "p",
                "quality_score": "high"}]}"#,
        ];
        for raw in bad {
            assert!(
                RecordedJudge::from_str(raw).is_err(),
                "should reject: {raw}"
            );
        }
    }
}
