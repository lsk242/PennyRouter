//! Deterministic value score (Iter 34): maximum intelligence for the
//! lowest price, with no AI and no external ranking service.
//!
//! `rank_score = quality × efficiency × popularity`, all bounded:
//! quality is the Artificial Analysis index (coding first) times a
//! precision factor, efficiency is a saturating curve anchored at the
//! live median cost per task, and popularity is a ≤10% factor. Speed
//! and availability never enter the score — they break ties.
//!
//! Every constant is declared here, not read from the environment: a
//! score must mean the same thing on every rebuild. `RULESET_VERSION`
//! travels with each row so a formula change is visible in the
//! artifact.

use crate::rank::{asc_nulls_last, desc_nulls_last};
use std::cmp::Ordering;

/// Ruleset behind the score, carried in `rank_ruleset`.
pub const RULESET_VERSION: &str = "rank-v1";

/// Tokens in one canonical task: the balanced workload profile's mix
/// (5k input / 4k output / 0.5k cache read / 0.5k cache write).
pub const TASK_TOKENS: f64 = 10_000.0;

/// Cost per task at which price efficiency is exactly 0.5. The live
/// catalog's median model costs $1.00/M tokens ≈ $0.01/task, so the
/// curve is centred on the real distribution.
pub const COST_HALF_CREDIT: f64 = 0.01;

/// Maximum popularity contribution: a top model gains at most 10%.
pub const POP_WEIGHT: f64 = 0.10;

/// Quality discount for serving a model below its benchmarked
/// precision. AA benchmarks the reference model; a quantized endpoint
/// is not that model. Unknown precision takes a mild, uniform
/// discount — it never reorders two unknowns.
pub fn precision_factor(class: Option<&str>) -> f64 {
    match class {
        Some("fp32") | Some("fp16") => 1.0,
        Some("fp8") => 0.97,
        Some("int8") => 0.95,
        Some("int4") => 0.88,
        _ => 0.95,
    }
}

/// Cost of one canonical task from a per-token price. Negative or
/// non-finite prices are unpriceable, never free.
pub fn cost_per_task(cost_per_token: Option<f64>) -> Option<f64> {
    let cost = cost_per_token?;
    (cost.is_finite() && cost >= 0.0).then_some(cost * TASK_TOKENS)
}

/// Price efficiency in `0..=1`: 1 when free, 0.5 at
/// `COST_HALF_CREDIT`, approaching 0 as cost grows. A missing cost is
/// `None` — the offering gets no score at all, never a free pass.
pub fn efficiency(cost_per_task: Option<f64>) -> Option<f64> {
    let cost = cost_per_task?;
    if !cost.is_finite() || cost < 0.0 {
        return None;
    }
    Some(COST_HALF_CREDIT / (cost + COST_HALF_CREDIT))
}

/// Popularity factor in `0.909..=1.0`, normalized so a top-quality
/// free model still scores exactly 100. Absent popularity is a zero
/// share: no bonus, and the same factor as a model nobody uses.
pub fn popularity_factor(share: Option<f64>) -> f64 {
    let share = share.unwrap_or(0.0).clamp(0.0, 1.0);
    (1.0 + POP_WEIGHT * share) / (1.0 + POP_WEIGHT)
}

/// Inputs for one offering. `decision` switches the basis: behaviour
/// models are not benchmarked for coding, so they read the agentic
/// index and skip the precision discount.
pub struct RankInputs<'a> {
    pub coding: Option<i64>,
    pub intelligence: Option<i64>,
    pub agentic: Option<i64>,
    pub precision_class: Option<&'a str>,
    pub cost_per_token: Option<f64>,
    pub popularity_share: Option<f64>,
    pub decision: bool,
}

/// One computed score plus the components it was built from, so the
/// UI can show the arithmetic without re-deriving it.
#[derive(Debug, Clone, PartialEq)]
pub struct RankScore {
    pub score: f64,
    pub quality: f64,
    pub efficiency: f64,
    pub popularity: f64,
    pub basis: &'static str,
}

/// Score one offering. `None` when the model has no usable index or
/// the offering has no price: an unmeasured model must never sort
/// below a measured bad one.
pub fn score(inputs: &RankInputs) -> Option<RankScore> {
    let (basis, index) = if inputs.decision {
        match (inputs.agentic, inputs.intelligence) {
            (Some(value), _) => ("agentic", value),
            (None, Some(value)) => ("intel", value),
            _ => return None,
        }
    } else {
        match (inputs.coding, inputs.intelligence, inputs.agentic) {
            (Some(value), _, _) => ("coding", value),
            (None, Some(value), _) => ("intel", value),
            (None, None, Some(value)) => ("agentic", value),
            _ => return None,
        }
    };
    let factor = if inputs.decision {
        1.0
    } else {
        precision_factor(inputs.precision_class)
    };
    let quality = index.clamp(0, 100) as f64 * factor;
    let efficiency = efficiency(cost_per_task(inputs.cost_per_token))?;
    let popularity = popularity_factor(inputs.popularity_share);
    Some(RankScore {
        score: quality * efficiency * popularity,
        quality,
        efficiency,
        popularity,
        basis,
    })
}

/// Ordering key for one offering: the score plus the tie-breakers.
pub struct RankKey<'a> {
    pub score: Option<f64>,
    pub throughput_tps: Option<f64>,
    pub latency_ms: Option<f64>,
    pub uptime_pct: Option<f64>,
    pub provider: &'a str,
    pub tag: Option<&'a str>,
}

/// Scores compare at three decimals: below that the difference is
/// float noise, not a ranking decision.
fn score_key(value: Option<f64>) -> Option<f64> {
    value.map(|v| (v * 1000.0).round() / 1000.0)
}

/// Best first: score, then speed (throughput, latency), then
/// availability, then provider/tag so rebuilds are byte-stable.
/// Unknowns sort last at every step — no evidence is not evidence of
/// perfection.
pub fn compare(a: &RankKey, b: &RankKey) -> Ordering {
    desc_nulls_last(score_key(a.score), score_key(b.score))
        .then_with(|| desc_nulls_last(a.throughput_tps, b.throughput_tps))
        .then_with(|| asc_nulls_last(a.latency_ms, b.latency_ms))
        .then_with(|| desc_nulls_last(a.uptime_pct, b.uptime_pct))
        .then_with(|| a.provider.cmp(b.provider))
        .then_with(|| a.tag.cmp(&b.tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> RankInputs<'static> {
        RankInputs {
            coding: Some(60),
            intelligence: Some(70),
            agentic: Some(80),
            precision_class: Some("fp8"),
            cost_per_token: Some(1.2825e-6),
            popularity_share: Some(0.42),
            decision: false,
        }
    }

    #[test]
    fn precision_factors_are_declared_per_class() {
        assert_eq!(precision_factor(Some("fp32")), 1.0);
        assert_eq!(precision_factor(Some("fp16")), 1.0);
        assert_eq!(precision_factor(Some("fp8")), 0.97);
        assert_eq!(precision_factor(Some("int8")), 0.95);
        assert_eq!(precision_factor(Some("int4")), 0.88);
        // Unknown and absent are the same mild, uniform discount.
        assert_eq!(precision_factor(Some("unknown")), 0.95);
        assert_eq!(precision_factor(None), 0.95);
    }

    #[test]
    fn efficiency_curve_is_anchored_at_half_credit() {
        assert_eq!(efficiency(Some(0.0)), Some(1.0));
        assert_eq!(efficiency(Some(COST_HALF_CREDIT)), Some(0.5));
        let far = efficiency(Some(100.0 * COST_HALF_CREDIT)).unwrap();
        assert!((far - 0.0099).abs() < 1e-4, "got {far}");
        // Missing or invalid cost is no score, never free.
        assert_eq!(efficiency(None), None);
        assert_eq!(efficiency(Some(-1.0)), None);
        assert_eq!(efficiency(Some(f64::NAN)), None);
    }

    #[test]
    fn cost_per_task_scales_by_the_canonical_task() {
        assert_eq!(cost_per_task(Some(1e-6)), Some(0.01));
        assert_eq!(cost_per_task(Some(0.0)), Some(0.0));
        assert_eq!(cost_per_task(None), None);
        assert_eq!(cost_per_task(Some(-1e-6)), None);
    }

    #[test]
    fn popularity_is_bounded_and_absent_is_neutral() {
        // Absent and zero share are the same: no bonus.
        assert_eq!(popularity_factor(None), popularity_factor(Some(0.0)));
        assert!((popularity_factor(None) - 1.0 / 1.1).abs() < 1e-12);
        // Normalized by 1 + POP_WEIGHT so the top model still scores 100.
        assert!((popularity_factor(Some(1.0)) - 1.0).abs() < 1e-12);
        let mid = popularity_factor(Some(0.5));
        assert!(mid > 0.95 && mid < 1.0, "got {mid}");
        // Out-of-range shares clamp instead of inflating the score.
        assert_eq!(popularity_factor(Some(5.0)), 1.0);
        assert_eq!(popularity_factor(Some(-5.0)), popularity_factor(Some(0.0)));
    }

    #[test]
    fn worked_example_matches_the_proposal() {
        let out = score(&inputs()).unwrap();
        assert_eq!(out.basis, "coding");
        assert!((out.quality - 58.2).abs() < 1e-9);
        assert!((out.efficiency - 0.4381).abs() < 1e-3);
        assert!((out.popularity - 0.9473).abs() < 1e-3);
        assert!((out.score - 24.1).abs() < 0.1, "got {}", out.score);
    }

    #[test]
    fn basis_chain_is_coding_then_intelligence_then_agentic() {
        let mut input = inputs();
        assert_eq!(score(&input).unwrap().basis, "coding");
        input.coding = None;
        assert_eq!(score(&input).unwrap().basis, "intel");
        input.intelligence = None;
        assert_eq!(score(&input).unwrap().basis, "agentic");
        input.agentic = None;
        assert_eq!(score(&input), None);
    }

    #[test]
    fn decision_models_read_agentic_and_skip_the_precision_discount() {
        let mut input = inputs();
        input.decision = true;
        input.precision_class = Some("int4");
        let out = score(&input).unwrap();
        assert_eq!(out.basis, "agentic");
        // 80 × 1.0, not 80 × 0.88.
        assert!((out.quality - 80.0).abs() < 1e-9);
        input.agentic = None;
        assert_eq!(score(&input).unwrap().basis, "intel");
        input.intelligence = None;
        assert_eq!(score(&input), None);
    }

    #[test]
    fn missing_price_or_index_means_no_score() {
        let mut input = inputs();
        input.cost_per_token = None;
        assert_eq!(score(&input), None);
        let mut input = inputs();
        input.coding = None;
        input.intelligence = None;
        input.agentic = None;
        assert_eq!(score(&input), None);
    }

    #[test]
    fn free_offering_scores_on_quality_and_popularity_only() {
        let mut input = inputs();
        input.cost_per_token = Some(0.0);
        let out = score(&input).unwrap();
        assert_eq!(out.efficiency, 1.0);
        assert!((out.score - 58.2 * out.popularity).abs() < 1e-9);
    }

    #[test]
    fn out_of_range_indexes_clamp_instead_of_overflowing() {
        let mut input = inputs();
        input.precision_class = Some("fp16");
        input.coding = Some(999);
        assert!((score(&input).unwrap().quality - 100.0).abs() < 1e-9);
        input.coding = Some(-5);
        assert_eq!(score(&input).unwrap().quality, 0.0);
    }

    fn key(score: Option<f64>, throughput: Option<f64>, latency: Option<f64>) -> RankKey<'static> {
        RankKey {
            score,
            throughput_tps: throughput,
            latency_ms: latency,
            uptime_pct: Some(100.0),
            provider: "p",
            tag: None,
        }
    }

    #[test]
    fn ties_break_on_speed_then_availability() {
        // Higher score wins.
        assert_eq!(
            compare(&key(Some(50.0), None, None), &key(Some(40.0), None, None)),
            Ordering::Less
        );
        // Equal score: faster throughput wins.
        assert_eq!(
            compare(
                &key(Some(50.0), Some(90.0), None),
                &key(Some(50.0), Some(10.0), None)
            ),
            Ordering::Less
        );
        // Equal throughput: lower latency wins.
        assert_eq!(
            compare(
                &key(Some(50.0), Some(90.0), Some(100.0)),
                &key(Some(50.0), Some(90.0), Some(900.0))
            ),
            Ordering::Less
        );
        // Unknown speed sorts after known speed.
        assert_eq!(
            compare(
                &key(Some(50.0), None, None),
                &key(Some(50.0), Some(1.0), None)
            ),
            Ordering::Greater
        );
        // Unscored rows sort last.
        assert_eq!(
            compare(&key(None, Some(999.0), None), &key(Some(1.0), None, None)),
            Ordering::Greater
        );
    }

    #[test]
    fn scores_compare_at_three_decimals() {
        // Float noise below 0.001 is not a ranking decision.
        assert_eq!(
            compare(
                &key(Some(50.0001), None, None),
                &key(Some(50.0002), None, None)
            ),
            Ordering::Equal
        );
    }

    #[test]
    fn identity_breaks_remaining_ties() {
        let a = RankKey {
            score: Some(10.0),
            throughput_tps: None,
            latency_ms: None,
            uptime_pct: None,
            provider: "alpha",
            tag: None,
        };
        let b = RankKey {
            provider: "beta",
            ..a
        };
        assert_eq!(compare(&a, &b), Ordering::Less);
    }
}
