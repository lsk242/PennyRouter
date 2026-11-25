//! Deterministic processing: payloads into fixed indexed columns.
//!
//! Pure-code extraction only (Phase 2a). Model-scored signals plug in
//! later (Phase 2b) behind the same per-endpoint update.

use crate::benchmarks::{read_benchmark_code, read_benchmark_intel};
use crate::popular::{read_activity, ModelActivity, AGENT_TOP_N};
use crate::rank_score::{
    self, RankInputs, BRACKET_MIN_SHARE, BRACKET_PERFORMANCE_FLOOR, BRACKET_TOP_N, RULESET_VERSION,
    THINK_MAX_COST_TASK,
};
use crate::score_offline::OfflineScorer;
use crate::store::{
    Catalog, EndpointIndex, EndpointRank, EndpointScore, EndpointSpeed, ModelIndex,
    ModelPopularity, ModelSignals,
};
use crate::{AaModel, Error};
use serde_json::{json, Value};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};

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
    provider_name: String,
    tag: Option<String>,
    precision_class: Option<String>,
    cost_per_token: Option<f64>,
    aa_cost_per_task: Option<f64>,
    decision: bool,
}

/// Per-model facts the bracket/flag pass needs (Iter 43), captured
/// while the model loop still holds the raw inputs: the joined slug,
/// the pre-gate intel story, and whether activity measured the model.
#[derive(Default)]
struct ModelFacts {
    slug: String,
    intel_present: bool,
    intel_version: Option<String>,
    activity_present: bool,
    aa_cost: Option<f64>,
}

/// One v2-scored offering (Iter 43): the identity plus the
/// components the bracket ends and the TOP rollup read. Only scored
/// offerings land here.
struct ScoredOffering {
    model_id: String,
    provider_name: String,
    tag: Option<String>,
    cost_task: f64,
    performance: f64,
    efficiency: f64,
    activity: f64,
    basis: &'static str,
    score: f64,
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
        // Intelligence backfill from the benchmarks endpoint: fills
        // models whose embedded payload carries no score.
        let backfill = read_benchmark_intel(catalog);
        let backfill_code = read_benchmark_code(catalog);
        // Iter 43 inputs: AA direct records + the current version,
        // and per-slug activity. All absent-tolerant: keyless syncs
        // score like v1 (gate open, neutral activity).
        let aa_models = crate::read_aa_models(catalog);
        let aa_by_normalized = normalized_aa_index(&aa_models);
        let current_version = crate::read_aa_intel_version(catalog);
        let activity = read_activity(catalog);
        let mut signals: HashMap<String, ModelSignals> = HashMap::new();
        let mut facts: HashMap<String, ModelFacts> = HashMap::new();
        for model in catalog.all_models()? {
            let payload: Value = serde_json::from_str(&model.payload)?;
            // Both auxiliary datasets key on the permaslug, which the
            // model payload carries as `canonical_slug`; the plain id
            // is the fallback for payloads without one.
            let slug = payload
                .get("canonical_slug")
                .and_then(Value::as_str)
                .unwrap_or(&model.model_id);
            let aa = join_aa(&aa_models, &aa_by_normalized, slug, &model.model_id);
            // Intel precedence: AA direct (versioned current), then
            // embedded (with its version when the payload carries
            // one), then the backfill (unversioned). The gate below
            // NULLs whatever isn't provably current.
            let merged = aa
                .and_then(|aa| {
                    aa.intelligence
                        .map(|index| (index, current_version.clone()))
                })
                .or_else(|| {
                    extract_aa_index(&payload, "intelligence_index")
                        .map(|index| (index, extract_aa_version(&payload)))
                })
                .or_else(|| {
                    backfill
                        .get(slug)
                        .or_else(|| backfill.get(&model.model_id))
                        .copied()
                        .map(|index| (index, None))
                });
            let intel = merged.as_ref().map(|(index, _)| *index);
            let intel_version = merged.and_then(|(_, version)| version);
            // Coding/agentic carry no versions: AA direct, then
            // embedded, then the code backfill for coding.
            let coding = aa
                .and_then(|aa| aa.coding)
                .or_else(|| extract_aa_index(&payload, "coding_index"))
                .or_else(|| {
                    backfill_code
                        .get(slug)
                        .or_else(|| backfill_code.get(&model.model_id))
                        .copied()
                });
            let agentic = aa
                .and_then(|aa| aa.agentic)
                .or_else(|| extract_aa_index(&payload, "agentic_index"));
            // Version gate: stale or unversioned intel is NULL once a
            // current version is known — the columns always hold
            // counted values, so mintel, tiers, and the UI respect
            // the gate for free. Unknown current keeps the legacy
            // behavior (everything counts).
            let gated_intel = match (&intel, &current_version) {
                (_, None) => intel,
                (Some(index), Some(current)) if intel_version.as_ref() == Some(current) => {
                    Some(*index)
                }
                _ => None,
            };
            // The stored version travels with the stored index: the
            // current version when gated in, NULL otherwise.
            let stored_version = match (&gated_intel, &current_version) {
                (Some(_), Some(current)) => Some(current.clone()),
                (Some(_), None) => intel_version.clone(),
                _ => None,
            };
            let index = ModelIndex {
                aa_intelligence_index: gated_intel,
                aa_coding_index: coding,
                aa_agentic_index: agentic,
                aa_cost_per_task: aa
                    .and_then(|aa| aa.cost_per_task)
                    .or_else(|| extract_aa_cost_per_task(&payload)),
            };
            catalog.update_model_index(&model.model_id, &index)?;
            catalog.update_model_intel_version(&model.model_id, stored_version.as_deref())?;
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
            let model_activity = activity
                .get(slug)
                .or_else(|| activity.get(&model.model_id))
                .copied();
            catalog.update_model_activity(
                &model.model_id,
                model_activity.map(|a| a.level),
                model_activity.map(|a| a.trend),
            )?;
            signals.insert(
                model.model_id.clone(),
                ModelSignals {
                    aa_intelligence_index: index.aa_intelligence_index,
                    aa_coding_index: index.aa_coding_index,
                    aa_agentic_index: index.aa_agentic_index,
                    aa_cost_per_task: index.aa_cost_per_task,
                    aa_intel_version: stored_version.clone(),
                    activity_level: model_activity.map(|a| a.level),
                    activity_trend: model_activity.map(|a| a.trend),
                    popularity_tokens: tokens.map(|tokens| tokens as i64),
                    popularity_share: share,
                },
            );
            facts.insert(
                model.model_id.clone(),
                ModelFacts {
                    slug: slug.to_string(),
                    intel_present: intel.is_some(),
                    intel_version,
                    activity_present: model_activity.is_some(),
                    aa_cost: aa.and_then(|aa| aa.cost_per_task),
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
                quantization: extract_quantization(&payload),
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
                provider_name: ep.provider_name.clone(),
                tag: crate::endpoint_tag(&payload).map(str::to_string),
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
                // AA's own task cost wins over the catalog blend
                // wherever the direct fetch carried it (v2): the
                // model loop already joined it, so the cost and the
                // indexes always come from the same record.
                aa_cost_per_task: facts.get(&ep.model_id).and_then(|f| f.aa_cost),
                decision: index.supports_decisions == Some(1),
            });
            processed += 1;
        }
        catalog.set_meta("processed_at", processed_at)?;
        catalog.set_meta("processed_count", &processed.to_string())?;
        let scored = run_offline_scoring(catalog, &endpoints, processed_at)?;
        let (ranked, scored_offers) = run_rank_scoring(
            catalog,
            &pending,
            &signals,
            current_version.as_deref(),
            processed_at,
        )?;
        run_brackets(
            catalog,
            &pending,
            &scored_offers,
            &signals,
            &facts,
            &activity,
            &popular,
            current_version.as_deref(),
        )?;
        Ok(ProcessSummary {
            processed,
            scored,
            ranked,
        })
    })
}

/// Value-score pass (Iter 34, v2 since Iter 43): deterministic, no
/// model calls. Runs after the indexes and offline tiers so it reads
/// the same facts the UI shows. Rows without an index or a price get
/// no score — never a zero, which would sort them below measured bad
/// models. v2 rows carry activity + cost/task and leave popularity
/// NULL; returns the scored offerings for the bracket pass.
fn run_rank_scoring(
    catalog: &Catalog,
    pending: &[PendingRank],
    signals: &HashMap<String, ModelSignals>,
    current_version: Option<&str>,
    processed_at: &str,
) -> Result<(usize, Vec<ScoredOffering>), Error> {
    let mut ranked = 0;
    let mut scored = Vec::new();
    for item in pending {
        let model = signals.get(&item.model_id);
        let out = rank_score::score(&RankInputs {
            coding: model.and_then(|m| m.aa_coding_index),
            intelligence: model.and_then(|m| m.aa_intelligence_index),
            agentic: model.and_then(|m| m.aa_agentic_index),
            intel_version: model.and_then(|m| m.aa_intel_version.as_deref()),
            current_version,
            precision_class: item.precision_class.as_deref(),
            cost_per_token: item.cost_per_token,
            aa_cost_per_task: item.aa_cost_per_task,
            activity_level: model.and_then(|m| m.activity_level),
            activity_trend: model.and_then(|m| m.activity_trend),
            decision: item.decision,
        });
        if let Some(out) = &out {
            ranked += 1;
            scored.push(ScoredOffering {
                model_id: item.model_id.clone(),
                provider_name: item.provider_name.clone(),
                tag: item.tag.clone(),
                cost_task: out.cost_task,
                performance: out.quality,
                efficiency: out.efficiency,
                activity: out.activity,
                basis: out.basis,
                score: out.score,
            });
        }
        catalog.update_endpoint_rank(
            item.id,
            &EndpointRank {
                rank_score: out.as_ref().map(|o| o.score),
                rank_quality: out.as_ref().map(|o| o.quality),
                rank_efficiency: out.as_ref().map(|o| o.efficiency),
                rank_popularity: None,
                rank_activity: out.as_ref().map(|o| o.activity),
                rank_cost_task: out.as_ref().map(|o| o.cost_task),
                rank_basis: out.as_ref().map(|o| o.basis.to_string()),
                rank_ruleset: Some(RULESET_VERSION.to_string()),
                ranked_at: Some(processed_at.to_string()),
            },
        )?;
    }
    catalog.set_meta("ranked_at", processed_at)?;
    catalog.set_meta("ranked_count", &ranked.to_string())?;
    catalog.set_meta("rank_ruleset", RULESET_VERSION)?;
    Ok((ranked, scored))
}

/// Top-usage bracket + flags + TOP rollup (Iter 43): runs on the
/// scored offerings. Members rank top-N by activity level with an
/// absolute recent-token share at or above the minimum, and at
/// least one v2-scored offering; the low end is the cheapest
/// member offering at/above the performance floor, the think end
/// the best member performance under the spend cap. Writes the
/// `bracket` summary and the per-model `top_models` rollup (generic
/// models only — `:batch` variants are not routable picks). Empty
/// bracket (no activity or no scores) is a valid outcome: the
/// gateway falls back to legacy ordering.
#[allow(clippy::too_many_arguments)]
fn run_brackets(
    catalog: &Catalog,
    pending: &[PendingRank],
    scored: &[ScoredOffering],
    signals: &HashMap<String, ModelSignals>,
    facts: &HashMap<String, ModelFacts>,
    activity: &HashMap<String, ModelActivity>,
    popular: &HashMap<String, u64>,
    current_version: Option<&str>,
) -> Result<(), Error> {
    // Best scored offering per model: max score, provider-stable.
    let mut best: HashMap<&str, &ScoredOffering> = HashMap::new();
    for offer in scored {
        best.entry(offer.model_id.as_str())
            .and_modify(|cur| {
                if offer.score.total_cmp(&cur.score) == std::cmp::Ordering::Greater
                    || (offer.score == cur.score
                        && (offer.provider_name.as_str(), offer.tag.as_deref())
                            < (cur.provider_name.as_str(), cur.tag.as_deref()))
                {
                    *cur = offer;
                }
            })
            .or_insert(offer);
    }
    // Activity rank by level, best first; absolute shares from the
    // recent-window token sums.
    let mut by_level: Vec<(&String, f64)> =
        activity.iter().map(|(slug, a)| (slug, a.level)).collect();
    by_level.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let top_slugs: HashSet<&str> = by_level
        .into_iter()
        .take(BRACKET_TOP_N)
        .map(|(slug, _)| slug.as_str())
        .collect();
    let total: u64 = popular.values().copied().sum();
    let mut members: Vec<String> = facts
        .iter()
        .filter(|(id, f)| {
            best.contains_key(id.as_str())
                && (top_slugs.contains(f.slug.as_str()) || top_slugs.contains(id.as_str()))
                && popular
                    .get(&f.slug)
                    .or_else(|| popular.get(*id))
                    .is_some_and(|tokens| {
                        total > 0 && *tokens as f64 / total as f64 >= BRACKET_MIN_SHARE
                    })
        })
        .map(|(id, _)| id.clone())
        .collect();
    members.sort();
    let in_bracket: HashSet<&str> = members.iter().map(String::as_str).collect();
    let low_end = scored
        .iter()
        .filter(|o| {
            in_bracket.contains(o.model_id.as_str()) && o.performance >= BRACKET_PERFORMANCE_FLOOR
        })
        .min_by(|a, b| {
            a.cost_task
                .total_cmp(&b.cost_task)
                .then_with(|| a.model_id.cmp(&b.model_id))
                .then_with(|| a.provider_name.cmp(&b.provider_name))
        });
    // max_by takes the LAST maximum on ties, so the stability
    // tie-breaks run reversed: smallest model/provider wins.
    let think_end = scored
        .iter()
        .filter(|o| in_bracket.contains(o.model_id.as_str()) && o.cost_task <= THINK_MAX_COST_TASK)
        .max_by(|a, b| {
            a.performance
                .total_cmp(&b.performance)
                .then_with(|| a.score.total_cmp(&b.score))
                .then_with(|| b.model_id.cmp(&a.model_id))
                .then_with(|| b.provider_name.cmp(&a.provider_name))
        });
    let mut priceable: HashSet<&str> = HashSet::new();
    for item in pending {
        if item.cost_per_token.is_some()
            || item
                .aa_cost_per_task
                .is_some_and(|c| c.is_finite() && c >= 0.0)
        {
            priceable.insert(item.model_id.as_str());
        }
    }
    let end_json = |end: Option<&&ScoredOffering>| match end {
        Some(offer) => json!({
            "model": offer.model_id,
            "provider": offer.provider_name,
            "tag": offer.tag,
            "cost_task": offer.cost_task,
            "performance": offer.performance,
            "score": offer.score,
        }),
        None => Value::Null,
    };
    catalog.set_meta(
        "bracket",
        &json!({
            "ruleset": RULESET_VERSION,
            "top_n": BRACKET_TOP_N,
            "min_share": BRACKET_MIN_SHARE,
            "floor": BRACKET_PERFORMANCE_FLOOR,
            "think_cap": THINK_MAX_COST_TASK,
            "members": members,
            "low_end": end_json(low_end.as_ref()),
            "think_end": end_json(think_end.as_ref()),
        })
        .to_string(),
    )?;
    // Agent pool (Iter 49): top slugs by recent-window tokens,
    // `:free` variants excluded — the `agent` tier is paid-only.
    // Resolved to catalog model ids like the bracket (slug or id);
    // `:batch`/`:free` ids are not routable picks. Ranked best
    // first; empty without popularity data (the tier fails closed).
    let mut by_tokens: Vec<(&String, u64)> = popular
        .iter()
        .map(|(slug, tokens)| (slug, *tokens))
        .collect();
    by_tokens.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let agent_rank: HashMap<&str, usize> = by_tokens
        .into_iter()
        .filter(|(slug, _)| !slug.ends_with(":free"))
        .take(AGENT_TOP_N)
        .map(|(slug, _)| slug.as_str())
        .enumerate()
        .map(|(rank, slug)| (slug, rank))
        .collect();
    let mut agent: Vec<(&String, usize)> = facts
        .iter()
        .filter(|(id, _)| !id.ends_with(":batch") && !id.ends_with(":free"))
        .filter_map(|(id, f)| {
            agent_rank
                .get(f.slug.as_str())
                .or_else(|| agent_rank.get(id.as_str()))
                .map(|rank| (id, *rank))
        })
        .collect();
    agent.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
    catalog.set_meta(
        "agent_top20",
        &json!({
            "ruleset": RULESET_VERSION,
            "top_n": AGENT_TOP_N,
            "members": agent.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        })
        .to_string(),
    )?;
    let mut ids: Vec<&String> = facts.keys().collect();
    ids.sort();
    let mut top = serde_json::Map::new();
    for id in ids {
        if id.ends_with(":batch") {
            continue;
        }
        // facts and signals cover every model row; a missing entry
        // can only mean a caller bug, and debug builds say so.
        let f = &facts[id.as_str()];
        let is_top = top_slugs.contains(f.slug.as_str()) || top_slugs.contains(id.as_str());
        let scored_here = best.contains_key(id.as_str());
        let flags = model_flags(
            f,
            scored_here,
            priceable.contains(id.as_str()),
            !activity.is_empty(),
            is_top,
            current_version,
        );
        let entry = match best.get(id.as_str()) {
            Some(offer) => json!({
                "score": offer.score,
                "quality": offer.performance,
                "efficiency": offer.efficiency,
                "activity": offer.activity,
                "cost_task": offer.cost_task,
                "basis": offer.basis,
                "level": signals.get(id.as_str()).and_then(|s| s.activity_level),
                "trend": signals.get(id.as_str()).and_then(|s| s.activity_trend),
                "bracket": in_bracket.contains(id.as_str()),
                "flags": flags,
            }),
            None => json!({
                "score": Value::Null,
                "quality": Value::Null,
                "efficiency": Value::Null,
                "activity": Value::Null,
                "cost_task": Value::Null,
                "basis": Value::Null,
                "level": signals.get(id.as_str()).and_then(|s| s.activity_level),
                "trend": signals.get(id.as_str()).and_then(|s| s.activity_trend),
                "bracket": false,
                "flags": flags,
            }),
        };
        top.insert(id.clone(), entry);
    }
    catalog.set_meta("top_models", &Value::Object(top).to_string())?;
    Ok(())
}

/// Per-model data-quality flags (Iter 43), fixed order: `no-price`
/// (nothing priceable to score), `no-activity` (unmeasured while
/// others were), `stale-version` (intel version known-old),
/// `no-current-score` (intel exists but versionless while a current
/// version is known), `popular-but-unscored` (top usage, no score).
/// Version/activity flags fire only when the reference data exists —
/// keyless syncs flag nothing for missing aux datasets.
fn model_flags(
    facts: &ModelFacts,
    scored: bool,
    priceable: bool,
    activity_known: bool,
    is_top: bool,
    current: Option<&str>,
) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if !priceable {
        flags.push("no-price");
    }
    if activity_known && !facts.activity_present {
        flags.push("no-activity");
    }
    if let Some(current) = current {
        match &facts.intel_version {
            Some(version) if version != current => flags.push("stale-version"),
            None if facts.intel_present => flags.push("no-current-score"),
            _ => {}
        }
    }
    if is_top && !scored {
        flags.push("popular-but-unscored");
    }
    flags
}

/// Join an AA record to a catalog model: exact slug first, then the
/// normalized comparison (lowercase, `_`/`.` to `-`, trailing
/// 8-digit date stripped) against the canonical slug, the model id,
/// and the id's last segment. No fuzzy name matching — an unmatched
/// model simply has no AA data.
fn join_aa<'a>(
    aa: &'a HashMap<String, AaModel>,
    by_normalized: &BTreeMap<String, &'a AaModel>,
    slug: &str,
    model_id: &str,
) -> Option<&'a AaModel> {
    if let Some(record) = aa.get(slug).or_else(|| aa.get(model_id)) {
        return Some(record);
    }
    let last = model_id.rsplit('/').next().unwrap_or(model_id);
    [slug, model_id, last]
        .into_iter()
        .filter_map(|candidate| by_normalized.get(&normalize_slug(candidate)))
        .copied()
        .next()
}

/// Normalized AA slug index, first-wins over sorted slugs so ties
/// resolve deterministically.
fn normalized_aa_index(aa: &HashMap<String, AaModel>) -> BTreeMap<String, &AaModel> {
    let mut slugs: Vec<&String> = aa.keys().collect();
    slugs.sort();
    let mut index = BTreeMap::new();
    for slug in slugs {
        index
            .entry(normalize_slug(slug))
            .or_insert_with(|| &aa[slug]);
    }
    index
}

/// One TOP-rollup entry (Iter 43): a generic model's best score
/// plus the components, activity, bracket flag, and data-quality
/// flags behind it. Read by the TOP tab and the gateway metrics.
#[derive(Debug, Clone, Default)]
pub struct TopModel {
    pub score: Option<f64>,
    pub quality: Option<f64>,
    pub efficiency: Option<f64>,
    pub activity: Option<f64>,
    pub cost_task: Option<f64>,
    pub basis: Option<String>,
    pub level: Option<f64>,
    pub trend: Option<f64>,
    pub bracket: bool,
    pub flags: Vec<String>,
}

/// Model-level TOP rollup (Iter 46): every generic model with its
/// best score and flags. Missing or unparseable (old artifacts,
/// keyless syncs) reads as empty; malformed entries drop
/// individually.
pub fn read_top_models(catalog: &Catalog) -> HashMap<String, TopModel> {
    catalog
        .get_meta("top_models")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(model, v)| {
                    if !v.is_object() {
                        return None;
                    }
                    let num = |key: &str| v.get(key).and_then(Value::as_f64);
                    Some((
                        model,
                        TopModel {
                            score: num("score"),
                            quality: num("quality"),
                            efficiency: num("efficiency"),
                            activity: num("activity"),
                            cost_task: num("cost_task"),
                            basis: v.get("basis").and_then(Value::as_str).map(str::to_string),
                            level: num("level"),
                            trend: num("trend"),
                            bracket: v.get("bracket").and_then(Value::as_bool).unwrap_or(false),
                            flags: v
                                .get("flags")
                                .and_then(Value::as_array)
                                .map(|flags| {
                                    flags
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(str::to_string)
                                        .collect()
                                })
                                .unwrap_or_default(),
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// In-bracket model ids for the gateway (Iter 44): the `bracket`
/// summary's member list. Missing or unparseable (old artifacts,
/// keyless syncs, empty brackets) reads as empty — the gateway then
/// routes legacy.
pub fn read_bracket_members(catalog: &Catalog) -> HashSet<String> {
    catalog
        .get_meta("bracket")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get("members")?.as_array().cloned())
        .map(|members| {
            members
                .into_iter()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Ranked agent-pool model ids for the gateway (Iter 49): the
/// `agent_top20` summary's member list, best first. Missing or
/// unparseable (old artifacts, keyless syncs) reads as empty — the
/// tier fails closed.
pub fn read_agent_top20(catalog: &Catalog) -> Vec<String> {
    catalog
        .get_meta("agent_top20")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get("members")?.as_array().cloned())
        .map(|members| {
            members
                .into_iter()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Slug normalization for the AA join: lowercase, `_`/`.` to `-`,
/// one trailing 8-digit date run stripped. Byte-safe: the stripped
/// run is ASCII digits, so truncation never splits a char.
fn normalize_slug(slug: &str) -> String {
    let mut normalized: String = slug
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '_' | '.' => '-',
            c => c,
        })
        .collect();
    let bytes = normalized.as_bytes();
    if bytes.len() > 8 && bytes[bytes.len() - 8..].iter().all(|b| b.is_ascii_digit()) {
        normalized.truncate(bytes.len() - 8);
        while normalized.ends_with('-') {
            normalized.pop();
        }
    }
    normalized
}

/// Offline scoring pass (Iter 32, adaptive since Iter 41):
/// deterministic tiers from the AA indexes just distilled above —
/// free, no model calls. Each type calibrates its own cutoffs
/// from used models (see `OfflineScorer`); only untiered rows are
/// filled, so a tier already on the row (e.g. an online verdict,
/// when `catalog-process` re-runs processing over a scored DB)
/// always wins. Models with no index stay untiered for the online
/// scorers.
fn run_offline_scoring(
    catalog: &Catalog,
    endpoints: &[crate::store::EndpointRow],
    processed_at: &str,
) -> Result<usize, Error> {
    use crate::score::Scorer;
    let scorer = OfflineScorer::new(
        catalog.aa_coding_map()?,
        catalog.aa_intelligence_map()?,
        catalog.model_popularity_map()?,
    );
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
    catalog.set_meta("score_cutoffs", &cutoff_meta(&scorer))?;
    Ok(scored)
}

/// `score_cutoffs` audit meta (Iter 41): per index type, the
/// cutoffs the offline pass tiered with, the calibration size,
/// and where the cutoffs came from. Skipped types carry no
/// cutoffs — their models stayed untiered this run.
fn cutoff_meta(scorer: &OfflineScorer) -> String {
    fn one(rule: &crate::score_offline::IndexCutoffs) -> Value {
        let mut fields = serde_json::Map::new();
        if let Some(cutoffs) = rule.cutoffs {
            fields.insert("high".to_string(), Value::from(cutoffs.high));
            fields.insert("standard".to_string(), Value::from(cutoffs.standard));
        }
        fields.insert("n".to_string(), Value::from(rule.n as u64));
        fields.insert("source".to_string(), Value::from(rule.source.as_str()));
        Value::Object(fields)
    }
    let mut meta = serde_json::Map::new();
    meta.insert("coding".to_string(), one(scorer.coding_cutoffs()));
    meta.insert("intelligence".to_string(), one(scorer.intel_cutoffs()));
    Value::Object(meta).to_string()
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

/// Embedded intelligence-index version (Iter 43), when the payload
/// carries one beside the index: string as-is, number stringified.
/// Most payloads carry none — those indexes are unversioned.
fn extract_aa_version(model_payload: &Value) -> Option<String> {
    match model_payload
        .get("benchmarks")?
        .get("artificial_analysis")?
        .get("intelligence_index_version")?
    {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
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
/// Exact `quantization` string from the payload, byte-verbatim
/// (Iter 36): what OpenRouter reported is what the gateway sends
/// back on the wire pin. Missing, non-string, or empty means
/// `None` — the gateway then pins nothing (fail-open, as with the
/// old `unknown` class).
fn extract_quantization(payload: &Value) -> Option<String> {
    match payload.get("quantization").and_then(Value::as_str) {
        Some(q) if !q.is_empty() => Some(q.to_string()),
        _ => None,
    }
}

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
    fn quantization_extracts_verbatim() {
        let quant = |payload: Value| extract_quantization(&payload);
        assert_eq!(
            quant(json!({"quantization": "fp4"})),
            Some("fp4".to_string())
        );
        assert_eq!(
            quant(json!({"quantization": "mxfp4"})),
            Some("mxfp4".to_string())
        );
        assert_eq!(
            quant(json!({"quantization": "int4"})),
            Some("int4".to_string())
        );
        assert_eq!(
            quant(json!({"quantization": "unknown"})),
            Some("unknown".to_string())
        );
        assert_eq!(quant(json!({})), None);
        assert_eq!(quant(json!({"quantization": ""})), None);
        assert_eq!(quant(json!({"quantization": 4})), None);
        assert_eq!(quant(json!({"quantization": null})), None);
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

    #[test]
    fn slug_normalization_strips_dates_not_versions() {
        assert_eq!(
            normalize_slug("claude-opus-5.5-20260921"),
            "claude-opus-5-5"
        );
        assert_eq!(normalize_slug("gpt_6.1-terra"), "gpt-6-1-terra");
        assert_eq!(normalize_slug("llama-3"), "llama-3");
        assert_eq!(normalize_slug("m"), "m");
    }

    #[test]
    fn aa_join_prefers_exact_then_normalized() {
        use crate::AaModel;
        let mk = |slug: &str| AaModel {
            slug: slug.to_string(),
            ..Default::default()
        };
        let aa: HashMap<String, AaModel> = HashMap::from([
            ("exact".to_string(), mk("exact")),
            ("glm-5-flash".to_string(), mk("glm-5-flash")),
        ]);
        let index = normalized_aa_index(&aa);
        // Exact slug wins without normalization.
        assert_eq!(
            join_aa(&aa, &index, "exact", "other/id").unwrap().slug,
            "exact"
        );
        // Dated OpenRouter id meets the bare AA slug.
        assert_eq!(
            join_aa(
                &aa,
                &index,
                "z-ai/glm-5.3-flash-20260826",
                "z-ai/glm-5.3-flash-20260826"
            )
            .map(|m| m.slug.as_str()),
            None,
        );
        assert_eq!(
            join_aa(
                &aa,
                &index,
                "glm-5-flash-20260826",
                "x/glm-5-flash-20260826"
            )
            .unwrap()
            .slug,
            "glm-5-flash"
        );
        // No fuzzy name matching.
        assert!(join_aa(&aa, &index, "unrelated-model", "u/unrelated-model").is_none());
    }

    #[test]
    fn embedded_version_reads_string_or_number() {
        let payload = |version: serde_json::Value| {
            json!({"benchmarks": {"artificial_analysis": {
                "intelligence_index": 70,
                "intelligence_index_version": version,
            }}})
        };
        assert_eq!(
            extract_aa_version(&payload(json!("5"))).as_deref(),
            Some("5")
        );
        assert_eq!(
            extract_aa_version(&payload(json!(4.3))).as_deref(),
            Some("4.3")
        );
        assert_eq!(extract_aa_version(&payload(json!(""))), None);
        assert_eq!(extract_aa_version(&json!({})), None);
    }
}
