//! Deterministic value score (Iter 34, v2 since Iter 43): maximum
//! intelligence for the lowest price, with no AI and no external
//! ranking service.
//!
//! `rank_score = quality × efficiency × activity`, all bounded:
//! quality is the Artificial Analysis index (coding first) times a
//! precision factor, efficiency is a saturating curve anchored at the
//! live median cost per task, and activity is the recent usage share
//! times a bounded trend multiplier. Speed and availability never
//! enter the score — they break ties.
//!
//! v2 changes since `rank-v1`: usage reads measured activity
//! (level × trend) instead of the popularity-window share; the
//! intelligence index counts only on the current test version
//! (stale or unversioned intel is NULL once a version is known);
//! and AA's own cost/task wins over the catalog blend wherever the
//! fetch carries it.
//!
//! Every constant is declared here, not read from the environment: a
//! score must mean the same thing on every rebuild. `RULESET_VERSION`
//! travels with each row so a formula change is visible in the
//! artifact.

use crate::rank::{asc_nulls_last, desc_nulls_last};
use std::cmp::Ordering;

/// Ruleset behind the score, carried in `rank_ruleset`.
pub const RULESET_VERSION: &str = "rank-v2";

/// Tokens in one canonical task: the balanced workload profile's mix
/// (5k input / 4k output / 0.5k cache read / 0.5k cache write).
pub const TASK_TOKENS: f64 = 10_000.0;

/// Cost per task at which price efficiency is exactly 0.5. The live
/// catalog's median model costs $1.00/M tokens ≈ $0.01/task, so the
/// curve is centred on the real distribution.
pub const COST_HALF_CREDIT: f64 = 0.01;

/// Maximum popularity contribution under `rank-v1` (kept for the
/// legacy formula display; v2 reads activity instead).
pub const POP_WEIGHT: f64 = 0.10;

/// Trend boost bound: activity moves at most ±10% on trend — usage
/// orders models, but a spike never doubles a score.
pub const TREND_BOOST_MAX: f64 = 0.10;

/// Activity factor without measured activity: the v1-missing
/// neutral (`1/1.1`). Unknown usage neither boosts nor zeroes the
/// score; the `no-activity` flag carries the gap.
pub const ACTIVITY_UNKNOWN: f64 = 1.0 / 1.1;

/// Top-usage bracket (Iter 43): models ranked by activity level
/// enter at this rank (1-based) or better, with an absolute
/// recent-token share at or above `BRACKET_MIN_SHARE`, and at
/// least one v2-scored offering.
pub const BRACKET_TOP_N: usize = 25;
pub const BRACKET_MIN_SHARE: f64 = 0.005;

/// Performance floor for the bracket's low end: quality (index ×
/// precision factor) at or above this, matching the standard-tier
/// cutoff, so `low` never anchors on a weak model.
pub const BRACKET_PERFORMANCE_FLOOR: f64 = 40.0;

/// Max spend for the bracket's think end, $/task.
pub const THINK_MAX_COST_TASK: f64 = 0.05;

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

/// Usage factor in `0..=1`: the recent level times a bounded
/// trend multiplier, normalized by the max multiplier so a top
/// rising model with full quality and efficiency still scores
/// exactly 100. Absent activity is the v1-missing neutral
/// (`ACTIVITY_UNKNOWN`).
pub fn activity_factor(level: Option<f64>, trend: Option<f64>) -> f64 {
    let Some(level) = level.filter(|l| l.is_finite() && *l >= 0.0) else {
        return ACTIVITY_UNKNOWN;
    };
    let trend = trend.filter(|t| t.is_finite() && *t >= 0.0).unwrap_or(1.0);
    let boost = (trend - 1.0).clamp(-TREND_BOOST_MAX, TREND_BOOST_MAX);
    level.clamp(0.0, 1.0) * (1.0 + boost) / (1.0 + TREND_BOOST_MAX)
}

/// Inputs for one offering. `decision` switches the basis: behaviour
/// models are not benchmarked for coding, so they read the agentic
/// index and skip the precision discount. `intel_version` is the
/// test version behind `intelligence` (`None` = unversioned);
/// `current_version` is the sync's current version (`None` =
/// unknown, the gate stays open).
pub struct RankInputs<'a> {
    pub coding: Option<i64>,
    pub intelligence: Option<i64>,
    pub agentic: Option<i64>,
    pub intel_version: Option<&'a str>,
    pub current_version: Option<&'a str>,
    pub precision_class: Option<&'a str>,
    pub cost_per_token: Option<f64>,
    pub aa_cost_per_task: Option<f64>,
    pub activity_level: Option<f64>,
    pub activity_trend: Option<f64>,
    pub decision: bool,
}

/// One computed score plus the components it was built from, so the
/// UI can show the arithmetic without re-deriving it. `cost_task` is
/// the exact task cost the efficiency used (AA's number when the
/// fetch carried it, else the catalog blend).
#[derive(Debug, Clone, PartialEq)]
pub struct RankScore {
    pub score: f64,
    pub quality: f64,
    pub efficiency: f64,
    pub activity: f64,
    pub cost_task: f64,
    pub basis: &'static str,
}

/// Score one offering. `None` when the model has no usable index or
/// the offering has no price: an unmeasured model must never sort
/// below a measured bad one. Intelligence counts only on the
/// current test version once a version is known — stale or
/// unversioned intel falls through to coding/agentic or to no
/// score, never in as itself.
pub fn score(inputs: &RankInputs) -> Option<RankScore> {
    let gated_intel = match (inputs.intelligence, inputs.current_version) {
        // Version unknown: the gate stays open (keyless or
        // failed-fetch syncs score like v1).
        (index, None) => index,
        // Version known: only the current version counts.
        (Some(index), Some(current)) if inputs.intel_version == Some(current) => Some(index),
        _ => None,
    };
    let (basis, index) = if inputs.decision {
        match (inputs.agentic, gated_intel) {
            (Some(value), _) => ("agentic", value),
            (None, Some(value)) => ("intel", value),
            _ => return None,
        }
    } else {
        match (inputs.coding, gated_intel, inputs.agentic) {
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
    let cost_task = inputs
        .aa_cost_per_task
        .filter(|c| c.is_finite() && *c >= 0.0)
        .or_else(|| cost_per_task(inputs.cost_per_token))?;
    let efficiency = efficiency(Some(cost_task))?;
    let activity = activity_factor(inputs.activity_level, inputs.activity_trend);
    Some(RankScore {
        score: quality * efficiency * activity,
        quality,
        efficiency,
        activity,
        cost_task,
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
            intel_version: Some("5"),
            current_version: Some("5"),
            precision_class: Some("fp8"),
            cost_per_token: Some(1.2825e-6),
            aa_cost_per_task: None,
            activity_level: Some(1.0),
            activity_trend: Some(1.0),
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
    fn activity_is_bounded_and_absent_is_neutral() {
        // Absent activity is the v1-missing neutral, not zero.
        assert_eq!(activity_factor(None, None), ACTIVITY_UNKNOWN);
        assert!((ACTIVITY_UNKNOWN - 1.0 / 1.1).abs() < 1e-12);
        // Top flat model sits at 1/1.1; only a maxed boost
        // reaches exactly 1.0, so 100 stays the ceiling.
        assert!((activity_factor(Some(1.0), Some(1.0)) - 1.0 / 1.1).abs() < 1e-12);
        assert_eq!(activity_factor(Some(1.0), Some(2.0)), 1.0);
        // Rising beats flat beats falling at equal level.
        let rising = activity_factor(Some(0.5), Some(3.0));
        let flat = activity_factor(Some(0.5), Some(1.0));
        let falling = activity_factor(Some(0.5), Some(0.25));
        assert!(rising > flat && flat > falling);
        // The boost clamps at ±10%: a 10x spike ties a mild rise.
        assert_eq!(
            activity_factor(Some(0.5), Some(10.0)),
            activity_factor(Some(0.5), Some(1.5))
        );
        // Garbage levels read as missing; garbage trends read flat.
        assert_eq!(activity_factor(Some(f64::NAN), Some(3.0)), ACTIVITY_UNKNOWN);
        assert_eq!(
            activity_factor(Some(0.5), Some(f64::NAN)),
            activity_factor(Some(0.5), Some(1.0))
        );
        assert_eq!(
            activity_factor(Some(5.0), Some(1.0)),
            activity_factor(Some(1.0), Some(1.0))
        );
    }

    #[test]
    fn worked_example_matches_the_proposal() {
        let out = score(&inputs()).unwrap();
        assert_eq!(out.basis, "coding");
        assert!((out.quality - 58.2).abs() < 1e-9);
        assert!((out.efficiency - 0.4381).abs() < 1e-3);
        assert!((out.activity - 1.0 / 1.1).abs() < 1e-9);
        assert!((out.cost_task - 0.012825).abs() < 1e-9);
        assert!((out.score - 23.2).abs() < 0.1, "got {}", out.score);
    }

    #[test]
    fn version_gate_current_in_stale_out_missing_null() {
        let mut input = inputs();
        input.coding = None;
        input.agentic = None;
        // Current version: intel counts.
        assert_eq!(score(&input).unwrap().basis, "intel");
        // Stale version: excluded, and nothing else to fall back to.
        input.intel_version = Some("4");
        assert_eq!(score(&input), None);
        // Unversioned intel under a known current: excluded too.
        input.intel_version = None;
        assert_eq!(score(&input), None);
        // Version unknown (keyless): the gate stays open, legacy
        // behavior — unversioned intel counts.
        input.current_version = None;
        assert_eq!(score(&input).unwrap().basis, "intel");
    }

    #[test]
    fn aa_cost_per_task_wins_when_present() {
        let mut input = inputs();
        input.aa_cost_per_task = Some(0.005);
        let out = score(&input).unwrap();
        assert_eq!(out.cost_task, 0.005);
        assert!((out.efficiency - 0.01 / 0.015).abs() < 1e-9);
        // Invalid AA costs fall back to the catalog blend.
        input.aa_cost_per_task = Some(-1.0);
        assert!((score(&input).unwrap().cost_task - 0.012825).abs() < 1e-9);
        input.aa_cost_per_task = Some(f64::NAN);
        assert!((score(&input).unwrap().cost_task - 0.012825).abs() < 1e-9);
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
    fn free_offering_scores_on_quality_and_activity_only() {
        let mut input = inputs();
        input.cost_per_token = Some(0.0);
        let out = score(&input).unwrap();
        assert_eq!(out.efficiency, 1.0);
        assert!((out.score - 58.2 * out.activity).abs() < 1e-9);
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
