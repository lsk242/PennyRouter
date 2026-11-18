//! Offline scorer (Iter 32): deterministic tiers from the Artificial
//! Analysis coding index — no model calls.
//!
//! Every offering of a model shares the model's coding strength, so
//! every endpoint of an AA-indexed model scores the same tier:
//! coding 70+ is `high`, 40+ is `standard`, below that is `low`.
//! Models without a coding index get no signal and stay untiered for
//! the online scorers. The cutoffs are constants, not env knobs: a
//! tier must mean the same thing on every rebuild.
//!
//! `run_processing` runs this automatically on every sync (free and
//! deterministic) and only fills untiered rows, so an online verdict
//! already on the row always wins.

use crate::score::{Score, Scorer, ScorerIdentity};
use crate::Error;
use serde_json::Value;
use std::collections::HashMap;

/// Ruleset behind offline tiers, carried in the audit trail.
pub const RULESET_VERSION: &str = "aa-coding-v1";

/// Coding-index cutoffs: `high` at and above `HIGH_CUTOFF`,
/// `standard` at and above `STANDARD_CUTOFF`, `low` below that.
pub const HIGH_CUTOFF: i64 = 70;
pub const STANDARD_CUTOFF: i64 = 40;

/// Deterministic scorer over a coding-index map (see
/// `Catalog::aa_coding_map`). Provider and payload are ignored: the
/// tier is a property of the model, not the offering.
pub struct OfflineScorer {
    coding: HashMap<String, i64>,
}

impl OfflineScorer {
    pub fn new(coding: HashMap<String, i64>) -> Self {
        Self { coding }
    }

    /// Tier for a coding index. Inputs outside 0..=100 clamp to the
    /// ends instead of crashing: the store only holds that range, so
    /// anything else is caller error, never a reason to fail a sync.
    pub fn tier_for(coding: i64) -> &'static str {
        if coding >= HIGH_CUTOFF {
            "high"
        } else if coding >= STANDARD_CUTOFF {
            "standard"
        } else {
            "low"
        }
    }
}

impl Scorer for OfflineScorer {
    fn identity(&self) -> ScorerIdentity {
        ScorerIdentity {
            model: "offline".to_string(),
            prompt_version: RULESET_VERSION.to_string(),
        }
    }

    fn score(
        &self,
        model_id: &str,
        _provider_name: &str,
        _payload: &Value,
    ) -> Result<Score, Error> {
        match self.coding.get(model_id) {
            Some(&coding) => Ok(Score {
                quality_tier: Some(Self::tier_for(coding).to_string()),
                // Same clamp as the tier: the score stays within the
                // 0.0..=1.0 range the live scorers enforce.
                quality_score: Some(coding.clamp(0, 100) as f64 / 100.0),
            }),
            None => Ok(Score {
                quality_tier: None,
                quality_score: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scorer(pairs: &[(&str, i64)]) -> OfflineScorer {
        OfflineScorer::new(
            pairs
                .iter()
                .map(|(id, index)| (id.to_string(), *index))
                .collect(),
        )
    }

    #[test]
    fn cutoffs_map_to_tiers() {
        assert_eq!(OfflineScorer::tier_for(100), "high");
        assert_eq!(OfflineScorer::tier_for(70), "high");
        assert_eq!(OfflineScorer::tier_for(69), "standard");
        assert_eq!(OfflineScorer::tier_for(40), "standard");
        assert_eq!(OfflineScorer::tier_for(39), "low");
        assert_eq!(OfflineScorer::tier_for(0), "low");
    }

    #[test]
    fn out_of_range_clamps_to_the_ends() {
        assert_eq!(OfflineScorer::tier_for(101), "high");
        assert_eq!(OfflineScorer::tier_for(-1), "low");
        let scorer = scorer(&[("m/hi", 101), ("m/lo", -1)]);
        assert_eq!(
            scorer.score("m/hi", "p", &json!({})).unwrap().quality_score,
            Some(1.0)
        );
        assert_eq!(
            scorer.score("m/lo", "p", &json!({})).unwrap().quality_score,
            Some(0.0)
        );
    }

    #[test]
    fn indexed_models_score_tier_and_normalized_index() {
        let scorer = scorer(&[("m/high", 71), ("m/low", 5)]);
        let high = scorer.score("m/high", "p", &json!({})).unwrap();
        assert_eq!(high.quality_tier.as_deref(), Some("high"));
        assert_eq!(high.quality_score, Some(0.71));
        let low = scorer
            .score("m/low", "other", &json!({"tag": "t"}))
            .unwrap();
        assert_eq!(low.quality_tier.as_deref(), Some("low"));
        assert_eq!(low.quality_score, Some(0.05));
    }

    #[test]
    fn missing_index_scores_no_signal() {
        let scorer = scorer(&[("m/known", 80)]);
        let out = scorer.score("m/unknown", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier, None);
        assert_eq!(out.quality_score, None);
    }

    #[test]
    fn identity_names_the_offline_ruleset() {
        let identity = scorer(&[]).identity();
        assert_eq!(identity.model, "offline");
        assert_eq!(identity.prompt_version, RULESET_VERSION);
    }
}
