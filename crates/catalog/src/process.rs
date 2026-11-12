//! Deterministic processing: payloads into fixed indexed columns.
//!
//! Pure-code extraction only (Phase 2a). Model-scored signals plug in
//! later (Phase 2b) behind the same per-endpoint update.

use crate::rank_score::{self, RankInputs, RULESET_VERSION};
use crate::score_offline::OfflineScorer;
use crate::store::{
    Catalog, EndpointIndex, EndpointRank, EndpointScore, EndpointSpeed, ModelIndex,
    ModelPopularity, ModelSignals,
};
use crate::Error;
use serde_json::Value;
use std::collections::hash_map::Entry;
use std::collections::HashMap;

pub struct ProcessSummary {
    pub processed: usize,
    pub scored: usize,
    pub ranked: usize,
}

/// One endpoint's inputs to the value score, collected while its
/// indexed columns are written (Iter 34). The rows read before the
/// update still hold NULLs, so the score is computed from these
/// values, never re-read.
struct PendingRank {
    id: i64,
    model_id: String,
    precision_class: Option<String>,
    cost_per_token: Option<f64>,
    decision: bool,
}

/// Extract indexed values for every endpoint, plus per-model
/// Artificial Analysis indexes (Iter 31) and offline tiers (Iter
/// 32). Idempotent: re-running overwrites the same columns.
/// Model-level signals (decision modality, Iter 24) resolve through
/// a per-model payload cache so one lookup serves all of a model's
/// endpoints.
pub fn run_processing(catalog: &Catalog, processed_at: &str) -> Result<ProcessSummary, Error> {
    let endpoints = catalog.all_endpoints()?;
    // One transaction for the whole run (plus its meta): a mid-run
    // failure rolls every update back instead of half-processing.
    catalog.transaction(|| {
        // Popularity window (Iter 34): the score's per-model factor.
        // Absent (keyless sync, old artifact) means no factor, never a
        // penalty.
        let popular = crate::popular::read_popular_tokens(catalog);
        let max_tokens = popular.values().copied().max().unwrap_or(0);
        let mut signals: HashMap<String, ModelSignals> = HashMap::new();
        for model in catalog.all_models()? {
            let payload: Value = serde_json::from_str(&model.payload)?;
            let index = ModelIndex {
                aa_intelligence_index: extract_aa_index(&payload, "intelligence_index"),
                aa_coding_index: extract_aa_index(&payload, "coding_index"),
                aa_agentic_index: extract_aa_index(&payload, "agentic_index"),
                aa_cost_per_task: extract_aa_cost_per_task(&payload),
            };
            catalog.update_model_index(&model.model_id, &index)?;
            // The rankings dataset keys on the permaslug, which the
            // model payload carries as `canonical_slug`; the plain id
            // is the fallback for payloads without one.
            let slug = payload
                .get("canonical_slug")
                .and_then(Value::as_str)
                .unwrap_or(&model.model_id);
            let tokens = popular
                .get(slug)
                .or_else(|| popular.get(&model.model_id))
                .copied();
            let share = tokens.map(|tokens| {
                if max_tokens == 0 {
                    0.0
                } else {
                    tokens as f64 / max_tokens as f64
                }
            });
            catalog.update_model_popularity(
                &model.model_id,
                &ModelPopularity {
                    popularity_tokens: tokens.map(|tokens| tokens as i64),
                    popularity_share: share,
                },
            )?;
            signals.insert(
                model.model_id.clone(),
                ModelSignals {
                    aa_intelligence_index: index.aa_intelligence_index,
                    aa_coding_index: index.aa_coding_index,
                    aa_agentic_index: index.aa_agentic_index,
                    aa_cost_per_task: index.aa_cost_per_task,
                    popularity_tokens: tokens.map(|tokens| tokens as i64),
                    popularity_share: share,
                },
            );
        }
        let mut models: HashMap<String, Option<Value>> = HashMap::new();
        let mut pending: Vec<PendingRank> = Vec::new();
        let mut processed = 0;
        for ep in &endpoints {
            let payload: Value = serde_json::from_str(&ep.payload)?;
            // A broken model lookup fails the run like a broken
            // endpoint payload: silently caching `None` would mark
            // every endpoint unknown with no signal.
            let model_payload = match models.entry(ep.model_id.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let row = catalog.get_model(&ep.model_id)?;
                    let parsed = row
                        .map(|model| serde_json::from_str(&model.payload))
                        .transpose()?;
                    entry.insert(parsed)
                }
            };
            let index = EndpointIndex {
                price_prompt: extract_price(&payload, "prompt"),
                price_completion: extract_price(&payload, "completion"),
                price_cache_read: extract_price(&payload, "input_cache_read"),
                price_cache_write: extract_price(&payload, "input_cache_write"),
                precision_class: Some(classify_precision(&payload)),
                promo_discount: extract_discount(&payload),
                price_override_tokens: extract_override_tokens(&payload),
                processed_at: processed_at.to_string(),
                supports_tools: extract_tool_support(&payload),
                supports_decisions: extract_decision_support(model_payload.as_ref()),
            };
            catalog.update_endpoint_index(ep.id, &index)?;
            catalog.update_endpoint_speed(
                ep.id,
                &EndpointSpeed {
                    latency_ms: extract_metric(&payload, "latency_last_30m"),
                    throughput_tps: extract_metric(&payload, "throughput_last_30m"),
                    uptime_pct: extract_uptime(&payload),
                },
            )?;
            pending.push(PendingRank {
                id: ep.id,
                model_id: ep.model_id.clone(),
                precision_class: index.precision_class.clone(),
                // Static discount only: promo windows are evaluated at
                // ranking rebuild time, not here.
                cost_per_token: crate::price::effective_cost(
                    index.price_prompt,
                    index.price_completion,
                    index.price_cache_read,
                    index.price_cache_write,
                    &crate::price::balanced_profile(),
                    index.promo_discount.unwrap_or(0.0),
                ),
                decision: index.supports_decisions == Some(1),
            });
            processed += 1;
        }
        catalog.set_meta("processed_at", processed_at)?;
        catalog.set_meta("processed_count", &processed.to_string())?;
        let scored = run_offline_scoring(catalog, &endpoints, processed_at)?;
        let ranked = run_rank_scoring(catalog, &pending, &signals, processed_at)?;
        Ok(ProcessSummary {
            processed,
            scored,
            ranked,
        })
    })
}

/// Value-score pass (Iter 34): deterministic, no model calls. Runs
/// after the indexes and offline tiers so it reads the same facts the
/// UI shows. Rows without an index or a price get no score — never a
/// zero, which would sort them below measured bad models.
fn run_rank_scoring(
    catalog: &Catalog,
    pending: &[PendingRank],
    signals: &HashMap<String, ModelSignals>,
    processed_at: &str,
) -> Result<usize, Error> {
    let mut ranked = 0;
    for item in pending {
        let model = signals.get(&item.model_id);
        let out = rank_score::score(&RankInputs {
            coding: model.and_then(|m| m.aa_coding_index),
            intelligence: model.and_then(|m| m.aa_intelligence_index),
            agentic: model.and_then(|m| m.aa_agentic_index),
            precision_class: item.precision_class.as_deref(),
            cost_per_token: item.cost_per_token,
            popularity_share: model.and_then(|m| m.popularity_share),
            decision: item.decision,
        });
        if out.is_some() {
            ranked += 1;
        }
        catalog.update_endpoint_rank(
            item.id,
            &EndpointRank {
                rank_score: out.as_ref().map(|o| o.score),
                rank_quality: out.as_ref().map(|o| o.quality),
                rank_efficiency: out.as_ref().map(|o| o.efficiency),
                rank_popularity: out.as_ref().map(|o| o.popularity),
                rank_basis: out.as_ref().map(|o| o.basis.to_string()),
                rank_ruleset: Some(RULESET_VERSION.to_string()),
                ranked_at: Some(processed_at.to_string()),
            },
        )?;
    }
    catalog.set_meta("ranked_at", processed_at)?;
    catalog.set_meta("ranked_count", &ranked.to_string())?;
    catalog.set_meta("rank_ruleset", RULESET_VERSION)?;
    Ok(ranked)
}

/// Offline scoring pass (Iter 32): deterministic tiers from the AA
/// coding index just distilled above — free, no model calls. Only
/// untiered rows are filled: a tier already on the row (e.g. an
/// online verdict, when `catalog-process` re-runs processing over a
/// scored DB) always wins. Models without a coding index stay
/// untiered for the online scorers.
fn run_offline_scoring(
    catalog: &Catalog,
    endpoints: &[crate::store::EndpointRow],
    processed_at: &str,
) -> Result<usize, Error> {
    use crate::score::Scorer;
    let scorer = OfflineScorer::new(catalog.aa_coding_map()?);
    let identity = scorer.identity();
    let mut scored = 0;
    for ep in endpoints {
        if ep.quality_tier.is_some() {
            continue;
        }
        let payload: Value = serde_json::from_str(&ep.payload)?;
        let out = scorer.score(&ep.model_id, &ep.provider_name, &payload)?;
        let Some(tier) = out.quality_tier else {
            continue;
        };
        catalog.update_endpoint_score(
            ep.id,
            &EndpointScore {
                quality_tier: Some(tier),
                quality_score: out.quality_score,
                scored_at: processed_at.to_string(),
                score_model: Some(identity.model.clone()),
                score_prompt: Some(identity.prompt_version.clone()),
            },
        )?;
        scored += 1;
    }
    catalog.set_meta("scored_at", processed_at)?;
    catalog.set_meta("scored_count", &scored.to_string())?;
    catalog.set_meta("score_model", &identity.model)?;
    catalog.set_meta("score_prompt", &identity.prompt_version)?;
    Ok(scored)
}

/// One Artificial Analysis index from a model payload (Iter 31):
/// `benchmarks.artificial_analysis.<key>`, a number `0..=100`
/// (live values mix integers and floats, e.g. `64.6`). Rounded to
/// the nearest integer; missing, null, non-numeric, or out-of-range
/// reads as `None` (unknown) — garbage like `999` never sorts first.
fn extract_aa_index(model_payload: &Value, key: &str) -> Option<i64> {
    let n = model_payload
        .get("benchmarks")?
        .get("artificial_analysis")?
        .get(key)?;
    let n = match n {
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    if !n.is_finite() || !(0.0..=100.0).contains(&n) {
        return None;
    }
    Some(n.round() as i64)
}

/// AA's own cost per task (Iter 34), when the payload carries it.
/// Today's payloads do not; the column is reserved so AA's number
/// wins the moment it appears. Non-finite or negative is unknown.
fn extract_aa_cost_per_task(model_payload: &Value) -> Option<f64> {
    let value = model_payload
        .get("benchmarks")?
        .get("artificial_analysis")?
        .get("cost_per_task")?;
    let cost = match value {
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (cost.is_finite() && cost >= 0.0).then_some(cost)
}

/// One live metric from an endpoint payload (Iter 34): a finite,
/// non-negative number, else None. Missing is unknown, never zero.
fn extract_metric(payload: &Value, key: &str) -> Option<f64> {
    let value = match payload.get(key)? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.parse().ok()?,
        _ => return None,
    };
    (value.is_finite() && value >= 0.0).then_some(value)
}

/// Uptime percentage (Iter 34): `0..=100`, else None.
fn extract_uptime(payload: &Value) -> Option<f64> {
    extract_metric(payload, "uptime_last_30m").filter(|value| *value <= 100.0)
}

/// Per-token price as f64; accepts a string or a number, else None.
/// Negative or non-finite values are rejected (Iter 11, A02): an
/// operator-imported "-1" must never sort ahead of valid prices.
fn extract_price(payload: &Value, key: &str) -> Option<f64> {
    let price = match payload.get("pricing")?.get(key)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }?;
    (price.is_finite() && price >= 0.0).then_some(price)
}

/// Tool support per endpoint (Iter 22): OpenRouter lists the request
/// parameters each endpoint accepts in `supported_parameters`. A
/// `"tools"` entry means function calling works there; the flag
/// varies per provider on the same model, so it is indexed per
/// endpoint. Missing or non-array field is `None` (unknown), never
/// assumed capable or incapable.
fn extract_tool_support(payload: &Value) -> Option<i64> {
    let params = payload.get("supported_parameters")?.as_array()?;
    Some(i64::from(
        params.iter().any(|p| p.as_str() == Some("tools")),
    ))
}

/// Decision-modality support per endpoint (Iter 24): modality lives
/// on the parent *model* object (`architecture.output_modalities`),
/// so the caller passes the model's payload. A `"decisions"` entry
/// marks behavior-scoring models (e.g. Respan Span); routing matches
/// the `decision` tier on it. Missing model, architecture, or array
/// is `None` (unknown), never assumed.
fn extract_decision_support(model: Option<&Value>) -> Option<i64> {
    let modalities = model?
        .get("architecture")?
        .get("output_modalities")?
        .as_array()?;
    Some(i64::from(
        modalities.iter().any(|m| m.as_str() == Some("decisions")),
    ))
}

fn extract_discount(payload: &Value) -> Option<f64> {
    let discount = match payload.get("pricing")?.get("discount")? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }?;
    // Same 0.0..=1.0 range the promo windows enforce: an out-of-range
    // value (e.g. an imported 2.0) extracts as unknown instead of
    // clamping to cost 0 and publishing as permanently free.
    (discount.is_finite() && (0.0..=1.0).contains(&discount)).then_some(discount)
}

/// First long-context price tier (Q13-C): the smallest
/// `pricing.overrides[].min_prompt_tokens`, if any. Past this many
/// input tokens the endpoint bills its override price instead of the
/// base price, so the gateway skips the offering beyond it rather
/// than silently billing ~2×. Missing/empty/garbage overrides mean
/// no tier (`None`); a zero threshold means the base price never
/// applies, which the gateway treats as never fitting.
fn extract_override_tokens(payload: &Value) -> Option<i64> {
    payload
        .get("pricing")?
        .get("overrides")?
        .as_array()?
        .iter()
        .filter_map(|entry| entry.get("min_prompt_tokens"))
        .filter_map(|value| match value {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .filter(|tokens| *tokens >= 0)
        .min()
}

/// Quantization label to precision class. Anything unrecognized (including
/// missing) is "unknown" — never silently treated as a known class.
fn classify_precision(payload: &Value) -> String {
    let q = payload
        .get("quantization")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    if q.contains("fp32") {
        "fp32"
    } else if q.contains("bf16") || q.contains("fp16") {
        "fp16"
    } else if q.contains("fp8") {
        "fp8"
    } else if q.contains("int8") {
        "int8"
    } else if q.contains("int4") || q.contains("nf4") || q.contains("fp4") {
        "int4"
    } else {
        "unknown"
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn price_accepts_string_number_or_nothing() {
        let payload = json!({"pricing": {
            "prompt": "0.0000005",
            "completion": 0.25,
            "input_cache_read": null,
            "input_cache_write": "garbage",
        }});
        assert_eq!(extract_price(&payload, "prompt"), Some(0.0000005));
        assert_eq!(extract_price(&payload, "completion"), Some(0.25));
        assert_eq!(extract_price(&payload, "input_cache_read"), None);
        assert_eq!(extract_price(&payload, "input_cache_write"), None);
        assert_eq!(extract_price(&payload, "missing"), None);
        assert_eq!(extract_price(&json!({}), "prompt"), None);
    }

    /// A02: negative or non-finite prices are unpriceable, never
    /// cheapest-first.
    #[test]
    fn negative_and_nonfinite_prices_rejected() {
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": -1}}), "prompt"),
            None
        );
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": "-1"}}), "prompt"),
            None
        );
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": "NaN"}}), "prompt"),
            None
        );
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": "inf"}}), "prompt"),
            None
        );
        assert_eq!(
            extract_discount(&json!({"pricing": {"discount": -0.5}})),
            None
        );
        // Valid zero still prices (free is real).
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": 0}}), "prompt"),
            Some(0.0)
        );
    }

    /// Q13-C: the long-context tier threshold indexes the smallest
    /// `min_prompt_tokens` across overrides; missing/empty/garbage
    /// means no tier.
    #[test]
    fn override_threshold_indexes_minimum() {
        assert_eq!(
            extract_override_tokens(&json!({"pricing": {"overrides": [
                {"min_prompt_tokens": 500000},
                {"min_prompt_tokens": 272000},
            ]}})),
            Some(272000)
        );
        assert_eq!(
            extract_override_tokens(&json!({"pricing": {"overrides": []}})),
            None
        );
        assert_eq!(extract_override_tokens(&json!({"pricing": {}})), None);
        assert_eq!(extract_override_tokens(&json!({})), None);
        assert_eq!(
            extract_override_tokens(
                &json!({"pricing": {"overrides": [{"min_prompt_tokens": "abc"}]}})
            ),
            None
        );
    }

    #[test]
    fn precision_maps_known_quantizations() {
        let class = |q: &str| classify_precision(&json!({"quantization": q}));
        assert_eq!(class("fp32"), "fp32");
        assert_eq!(class("fp16"), "fp16");
        assert_eq!(class("BF16"), "fp16");
        assert_eq!(class("fp8"), "fp8");
        assert_eq!(class("fp8_e4m3"), "fp8");
        assert_eq!(class("int8"), "int8");
        assert_eq!(class("nf4"), "int4");
    }

    #[test]
    fn aa_index_reads_benchmarks_when_valid() {
        let payload = |aa: Value| json!({"benchmarks": {"artificial_analysis": aa}});
        assert_eq!(
            extract_aa_index(&payload(json!({"coding_index": 71})), "coding_index"),
            Some(71)
        );
        assert_eq!(
            extract_aa_index(
                &payload(json!({"intelligence_index": 0})),
                "intelligence_index"
            ),
            Some(0)
        );
        assert_eq!(
            extract_aa_index(&payload(json!({"agentic_index": 100})), "agentic_index"),
            Some(100)
        );
        // Live values mix integers and floats; floats round.
        assert_eq!(
            extract_aa_index(&payload(json!({"coding_index": 30.6})), "coding_index"),
            Some(31)
        );
        assert_eq!(
            extract_aa_index(&payload(json!({"coding_index": 51.5})), "coding_index"),
            Some(52)
        );
    }

    #[test]
    fn aa_index_falls_back_to_none() {
        let payload = |aa: Value| json!({"benchmarks": {"artificial_analysis": aa}});
        // Missing benchmarks / section / key.
        assert_eq!(extract_aa_index(&json!({}), "coding_index"), None);
        assert_eq!(
            extract_aa_index(&json!({"benchmarks": {}}), "coding_index"),
            None
        );
        assert_eq!(extract_aa_index(&payload(json!({})), "coding_index"), None);
        // Null, non-numeric, out of range.
        assert_eq!(
            extract_aa_index(&payload(json!({"coding_index": null})), "coding_index"),
            None
        );
        assert_eq!(
            extract_aa_index(&payload(json!({"coding_index": "71"})), "coding_index"),
            None
        );
        assert_eq!(
            extract_aa_index(&payload(json!({"coding_index": -1})), "coding_index"),
            None
        );
        assert_eq!(
            extract_aa_index(&payload(json!({"coding_index": 999})), "coding_index"),
            None
        );
        // Rounds out of range, not into it.
        assert_eq!(
            extract_aa_index(&payload(json!({"coding_index": 100.6})), "coding_index"),
            None
        );
    }

    #[test]
    fn precision_falls_back_to_unknown() {
        assert_eq!(
            classify_precision(&json!({"quantization": "unknown"})),
            "unknown"
        );
        assert_eq!(
            classify_precision(&json!({"quantization": "w4a8-exotic"})),
            "unknown"
        );
        assert_eq!(classify_precision(&json!({"quantization": ""})), "unknown");
        assert_eq!(classify_precision(&json!({})), "unknown");
    }
}
