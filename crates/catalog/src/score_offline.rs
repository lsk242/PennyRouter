//! Offline scorer (Iter 32, adaptive since Iter 41): deterministic
//! tiers from Artificial Analysis indexes — no model calls.
//!
//! Each index type (coding, intelligence) calibrates its own
//! cutoffs from the models people actually use: the tertiles of
//! that index over used, non-batch models. Index scales are never
//! compared with each other — a coding 60 and an intelligence 60
//! mean different things — so each type votes a tier on its own
//! scale and the model takes the best vote (ties prefer coding,
//! the historical signal). Models with no index get no signal and
//! stay untiered for the online scorers.
//!
//! Calibration needs `MIN_CALIBRATION_MODELS` used models per
//! type. Below that, coding falls back to the fixed Iter 32
//! cutoffs (fixtures and keyless syncs tier exactly as before)
//! while intelligence sits the run out — it never had fixed
//! cutoffs, and guessing them on an unknown scale would tier
//! noise. `run_processing` runs this automatically on every sync
//! and only fills untiered rows, so an online verdict already on
//! the row always wins.

use crate::score::{Score, Scorer, ScorerIdentity};
use crate::Error;
use serde_json::Value;
use std::collections::HashMap;

/// Ruleset behind offline tiers, carried in the audit trail.
pub const RULESET_VERSION: &str = "aa-adaptive-v1";

/// Minimum used models carrying an index type before that type
/// calibrates adaptive cutoffs.
pub const MIN_CALIBRATION_MODELS: usize = 5;

/// Coding fixed fallback (Iter 32 cutoffs): 70+ `high`, 40+
/// `standard`, else `low`. Only used when coding cannot
/// calibrate this run.
const CODING_FIXED_HIGH: i64 = 70;
const CODING_FIXED_STANDARD: i64 = 40;

/// Tier cutoffs on one index type's own scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cutoffs {
    pub high: i64,
    pub standard: i64,
}

/// Where one index type's cutoffs came from this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoffSource {
    Adaptive,
    Fixed,
    Skipped,
}

impl CutoffSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CutoffSource::Adaptive => "adaptive",
            CutoffSource::Fixed => "fixed",
            CutoffSource::Skipped => "skipped",
        }
    }
}

/// One index type's calibration result, for the `score_cutoffs`
/// audit meta. Skipped types carry no cutoffs.
#[derive(Debug, Clone, Copy)]
pub struct IndexCutoffs {
    pub cutoffs: Option<Cutoffs>,
    pub source: CutoffSource,
    /// Used non-batch models carrying the index this run.
    pub n: usize,
}

/// Deterministic scorer over per-model index maps plus trailing
/// usage. Provider and payload are ignored: the tier is a
/// property of the model, not the offering.
pub struct OfflineScorer {
    coding: HashMap<String, i64>,
    intelligence: HashMap<String, i64>,
    coding_cutoffs: IndexCutoffs,
    intel_cutoffs: IndexCutoffs,
}

impl OfflineScorer {
    pub fn new(
        coding: HashMap<String, i64>,
        intelligence: HashMap<String, i64>,
        popularity: HashMap<String, i64>,
    ) -> Self {
        let (coding_n, coding_adaptive) = calibrate(&coding, &popularity);
        let coding_cutoffs = match coding_adaptive {
            Some(cutoffs) => IndexCutoffs {
                cutoffs: Some(cutoffs),
                source: CutoffSource::Adaptive,
                n: coding_n,
            },
            None => IndexCutoffs {
                cutoffs: Some(Cutoffs {
                    high: CODING_FIXED_HIGH,
                    standard: CODING_FIXED_STANDARD,
                }),
                source: CutoffSource::Fixed,
                n: coding_n,
            },
        };
        let (intel_n, intel_adaptive) = calibrate(&intelligence, &popularity);
        let intel_cutoffs = IndexCutoffs {
            cutoffs: intel_adaptive,
            source: if intel_adaptive.is_some() {
                CutoffSource::Adaptive
            } else {
                CutoffSource::Skipped
            },
            n: intel_n,
        };
        Self {
            coding,
            intelligence,
            coding_cutoffs,
            intel_cutoffs,
        }
    }

    /// Coding calibration result, for the audit meta. Always
    /// carries cutoffs (adaptive or fixed).
    pub fn coding_cutoffs(&self) -> &IndexCutoffs {
        &self.coding_cutoffs
    }

    /// Intelligence calibration result, for the audit meta. Empty
    /// when the type sat this run out.
    pub fn intel_cutoffs(&self) -> &IndexCutoffs {
        &self.intel_cutoffs
    }

    /// Tier for one index value on its own type's scale.
    pub fn tier_for(value: i64, cutoffs: &Cutoffs) -> &'static str {
        if value >= cutoffs.high {
            "high"
        } else if value >= cutoffs.standard {
            "standard"
        } else {
            "low"
        }
    }

    /// Tier rank for the merge: the best vote wins.
    fn tier_rank(tier: &str) -> u8 {
        match tier {
            "high" => 3,
            "standard" => 2,
            _ => 1,
        }
    }
}

/// Values one index type calibrates from: used, non-batch models
/// carrying the index. `:batch` rows share their base model's
/// usage, so admitting them would double-vote it.
fn calibrate(
    index: &HashMap<String, i64>,
    popularity: &HashMap<String, i64>,
) -> (usize, Option<Cutoffs>) {
    let mut values: Vec<i64> = index
        .iter()
        .filter(|(id, _)| !id.ends_with(":batch") && popularity.contains_key(id.as_str()))
        .map(|(_, &value)| value)
        .collect();
    let n = values.len();
    if n < MIN_CALIBRATION_MODELS {
        return (n, None);
    }
    values.sort_unstable();
    (n, Some(tertiles(&values)))
}

/// Tertile cutoffs over sorted values: linear interpolation at
/// thirds, rounded to integers. Rounding never inverts the order
/// (round is monotone), so high >= standard always holds.
fn tertiles(sorted: &[i64]) -> Cutoffs {
    debug_assert!(sorted.len() >= MIN_CALIBRATION_MODELS);
    let at = |p: f64| {
        let rank = p * (sorted.len() - 1) as f64;
        let lo = rank.floor() as usize;
        let hi = rank.ceil() as usize;
        let value = sorted[lo] as f64 + (rank - lo as f64) * (sorted[hi] - sorted[lo]) as f64;
        value.round() as i64
    };
    Cutoffs {
        high: at(2.0 / 3.0),
        standard: at(1.0 / 3.0),
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
        // Coding always votes when the model carries it (adaptive
        // or fixed cutoffs); intelligence only votes on runs where
        // it calibrated.
        let coding_vote = self.coding.get(model_id).and_then(|&value| {
            self.coding_cutoffs
                .cutoffs
                .map(|cutoffs| (Self::tier_for(value, &cutoffs), value))
        });
        let intel_vote = self.intelligence.get(model_id).and_then(|&value| {
            self.intel_cutoffs
                .cutoffs
                .map(|cutoffs| (Self::tier_for(value, &cutoffs), value))
        });
        let (tier, value) = match (coding_vote, intel_vote) {
            (None, None) => {
                return Ok(Score {
                    quality_tier: None,
                    quality_score: None,
                });
            }
            (Some(vote), None) | (None, Some(vote)) => vote,
            // Best vote wins; ties prefer coding, the historical signal.
            (Some(coding), Some(intel)) => {
                if Self::tier_rank(intel.0) > Self::tier_rank(coding.0) {
                    intel
                } else {
                    coding
                }
            }
        };
        Ok(Score {
            quality_tier: Some(tier.to_string()),
            // Same clamp as the tier: the score stays within the
            // 0.0..=1.0 range the live scorers enforce.
            quality_score: Some(value.clamp(0, 100) as f64 / 100.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn maps(
        coding: &[(&str, i64)],
        intel: &[(&str, i64)],
        popular: &[&str],
    ) -> (
        HashMap<String, i64>,
        HashMap<String, i64>,
        HashMap<String, i64>,
    ) {
        let coding = coding
            .iter()
            .map(|(id, index)| (id.to_string(), *index))
            .collect();
        let intel = intel
            .iter()
            .map(|(id, index)| (id.to_string(), *index))
            .collect();
        let popularity = popular
            .iter()
            .enumerate()
            .map(|(i, id)| (id.to_string(), 1000 - i as i64))
            .collect();
        (coding, intel, popularity)
    }

    fn scorer(coding: &[(&str, i64)], intel: &[(&str, i64)], popular: &[&str]) -> OfflineScorer {
        let (coding, intel, popularity) = maps(coding, intel, popular);
        OfflineScorer::new(coding, intel, popularity)
    }

    /// Six used models calibrate both types; m/ghost has usage
    /// but no index. Coding and intel values disagree on purpose
    /// so the merge — not the raw numbers — decides.
    fn calibrated() -> OfflineScorer {
        scorer(
            &[
                ("m/a", 90),
                ("m/b", 40),
                ("m/c", 80),
                ("m/d", 15),
                ("m/e", 60),
                ("m/f", 25),
            ],
            &[
                ("m/a", 60),
                ("m/b", 50),
                ("m/c", 40),
                ("m/d", 30),
                ("m/e", 20),
                ("m/f", 10),
            ],
            &["m/a", "m/b", "m/c", "m/d", "m/e", "m/f", "m/ghost"],
        )
    }

    #[test]
    fn tertiles_split_the_calibration_set_into_thirds() {
        let scorer = calibrated();
        // Coding values sorted [15, 25, 40, 60, 80, 90]: thirds
        // at 35.0 -> 35 and 66.67 -> 67.
        assert_eq!(
            scorer.coding_cutoffs().cutoffs,
            Some(Cutoffs {
                high: 67,
                standard: 35
            })
        );
        assert_eq!(scorer.coding_cutoffs().source, CutoffSource::Adaptive);
        assert_eq!(scorer.coding_cutoffs().n, 6);
        // Intel values 10..=60 step 10: thirds at 26.67 -> 27
        // and 43.33 -> 43.
        assert_eq!(
            scorer.intel_cutoffs().cutoffs,
            Some(Cutoffs {
                high: 43,
                standard: 27
            })
        );
        assert_eq!(scorer.intel_cutoffs().source, CutoffSource::Adaptive);
        assert_eq!(scorer.intel_cutoffs().n, 6);
        let out = scorer.score("m/a", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("high"));
        // m/d: coding 15 is low on 67/35 while intel 30 is
        // standard on 43/27 — the better vote serves.
        let out = scorer.score("m/d", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("standard"));
        assert_eq!(out.quality_score, Some(0.3));
        let out = scorer.score("m/f", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("low"));
    }

    #[test]
    fn tiers_never_compare_scales_across_index_types() {
        // m/b: coding 40 is standard on 67/35 while intel 50 is
        // high on 43/27 — the merge takes the best vote, and the
        // raw 40-vs-50 gap never enters it.
        let scorer = calibrated();
        let out = scorer.score("m/b", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("high"));
        assert_eq!(out.quality_score, Some(0.5));
        // m/e: coding 60 is standard on 67/35 while intel 20 is
        // low on 43/27 — the coding vote wins from below.
        let out = scorer.score("m/e", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("standard"));
        assert_eq!(out.quality_score, Some(0.6));
    }

    #[test]
    fn merge_tie_prefers_the_coding_value() {
        let scorer = calibrated();
        // m/a is high on both scales: the coding value scores.
        let out = scorer.score("m/a", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("high"));
        assert_eq!(out.quality_score, Some(0.9));
        // m/f is low on both: same rule.
        let out = scorer.score("m/f", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("low"));
        assert_eq!(out.quality_score, Some(0.25));
    }

    #[test]
    fn unused_models_calibrate_nothing_but_still_score() {
        // m/idle carries intel but has no usage: excluded from the
        // set, yet tiered by the calibrated cutoffs.
        let (coding, intel, popularity) = maps(
            &[],
            &[
                ("m/a", 60),
                ("m/b", 50),
                ("m/c", 40),
                ("m/d", 30),
                ("m/e", 20),
                ("m/idle", 99),
            ],
            &["m/a", "m/b", "m/c", "m/d", "m/e"],
        );
        let scorer = OfflineScorer::new(coding, intel, popularity);
        assert_eq!(scorer.intel_cutoffs().n, 5);
        let out = scorer.score("m/idle", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("high"));
        assert_eq!(out.quality_score, Some(0.99));
    }

    #[test]
    fn batch_variants_never_calibrate_but_still_score() {
        // Five used non-batch models calibrate; the batch twin of
        // m/a shares its usage and must not double-vote it.
        let scorer = scorer(
            &[],
            &[
                ("m/a", 60),
                ("m/b", 50),
                ("m/c", 40),
                ("m/d", 30),
                ("m/e", 20),
                ("m/a:batch", 99),
            ],
            &["m/a", "m/b", "m/c", "m/d", "m/e", "m/a:batch"],
        );
        assert_eq!(scorer.intel_cutoffs().n, 5);
        assert_eq!(scorer.intel_cutoffs().source, CutoffSource::Adaptive);
        let out = scorer.score("m/a:batch", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("high"));
    }

    #[test]
    fn below_minimum_coding_falls_back_and_intel_sits_out() {
        let scorer = scorer(
            &[("m/hi", 85), ("m/lo", 10)],
            &[("m/hi", 65), ("m/lo", 5)],
            &["m/hi", "m/lo"],
        );
        assert_eq!(scorer.coding_cutoffs().source, CutoffSource::Fixed);
        assert_eq!(
            scorer.coding_cutoffs().cutoffs,
            Some(Cutoffs {
                high: 70,
                standard: 40
            })
        );
        assert_eq!(scorer.intel_cutoffs().source, CutoffSource::Skipped);
        assert_eq!(scorer.intel_cutoffs().cutoffs, None);
        // Fixed Iter 32 behavior, byte for byte.
        let out = scorer.score("m/hi", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("high"));
        assert_eq!(out.quality_score, Some(0.85));
        let out = scorer.score("m/lo", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("low"));
        assert_eq!(out.quality_score, Some(0.1));
    }

    #[test]
    fn fixtures_without_usage_tier_coding_exactly_as_before() {
        let scorer = scorer(&[("m/hi", 71), ("m/lo", 5)], &[], &[]);
        let high = scorer.score("m/hi", "p", &json!({})).unwrap();
        assert_eq!(high.quality_tier.as_deref(), Some("high"));
        assert_eq!(high.quality_score, Some(0.71));
        let low = scorer.score("m/lo", "other", &json!({"tag": "t"})).unwrap();
        assert_eq!(low.quality_tier.as_deref(), Some("low"));
        assert_eq!(low.quality_score, Some(0.05));
    }

    #[test]
    fn out_of_range_clamps_to_the_ends() {
        let scorer = scorer(&[("m/hi", 101), ("m/lo", -1)], &[], &[]);
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
    fn missing_index_scores_no_signal() {
        let scorer = calibrated();
        let out = scorer.score("m/ghost", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier, None);
        assert_eq!(out.quality_score, None);
    }

    #[test]
    fn identical_values_tier_everyone_high() {
        let scorer = scorer(
            &[],
            &[
                ("m/1", 30),
                ("m/2", 30),
                ("m/3", 30),
                ("m/4", 30),
                ("m/5", 30),
            ],
            &["m/1", "m/2", "m/3", "m/4", "m/5"],
        );
        assert_eq!(
            scorer.intel_cutoffs().cutoffs,
            Some(Cutoffs {
                high: 30,
                standard: 30
            })
        );
        let out = scorer.score("m/3", "p", &json!({})).unwrap();
        assert_eq!(out.quality_tier.as_deref(), Some("high"));
    }

    #[test]
    fn identity_names_the_adaptive_ruleset() {
        let identity = calibrated().identity();
        assert_eq!(identity.model, "offline");
        assert_eq!(identity.prompt_version, RULESET_VERSION);
    }
}
