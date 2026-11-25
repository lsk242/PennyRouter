//! Artificial Analysis Data API, free tier (Iter 42): headline
//! indexes + versions + cost/task, sync-time only.
//!
//! `GET {base}/api/v2/language/models/free` with `x-api-key`
//! (`AA_API_KEY`). Envelope shape (per the public API reference and
//! observed snapshots):
//! `{tier, intelligence_index_version, pagination: {page, page_size,
//! total_pages, has_more}, data: [...]}`. Each record carries `id`,
//! `name`, `slug`, `release_date`, `model_creator`, `evaluations`
//! with `artificial_analysis_{intelligence,coding,agentic}_index`
//! (nullable), `artificial_analysis_intelligence_index_cost` with
//! `cost_per_task.total_cost`, `pricing` (input/output per 1M tokens
//! as published), and `performance` medians (not stored).
//!
//! Pagination is followed while `has_more` (bounded by
//! `AA_MAX_PAGES`); syncs run daily by default, so a few pages per
//! sync stays far inside both the upstream key limits and the
//! self-imposed rate.
//!
//! ```rust
//! // Fetch policy: at most 1 request/hour to the AA Data API per sync
//! // host (self-imposed; the free tier allows far more). Sync-time
//! // only, never the request path.
//! ```
//!
//! Stored per model in `sync_meta` (`aa_models` + `aa_as_of` +
//! `aa_intel_version`): the AA slug maps to its indexes, the
//! envelope version, cost/task, and pricing. Joining AA slugs to
//! catalog models lands in Iter 43 (processing); this step only
//! fetches, parses, and stores. Keyless syncs store nothing; old
//! artifacts read as empty — no data, no penalty.

use crate::{Catalog, Error};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Free-tier models path, joined onto the AA API base URL.
pub const AA_FREE_PATH: &str = "/api/v2/language/models/free";
/// Pagination bound: 200 records per page upstream, so this covers
/// far more models than any catalog holds.
pub const AA_MAX_PAGES: u32 = 10;

/// One AA free-shape record: the fields Iter 43 scores from.
#[derive(Debug, Clone, Default)]
pub struct AaModel {
    pub slug: String,
    pub name: Option<String>,
    pub intelligence: Option<i64>,
    pub coding: Option<i64>,
    pub agentic: Option<i64>,
    pub cost_per_task: Option<f64>,
    pub price_input_m: Option<f64>,
    pub price_output_m: Option<f64>,
}

/// A full fetch: every page's records plus the envelope version and
/// the fetch timestamp (the envelope carries no `as_of`, so the
/// citation is when we pulled it).
#[derive(Debug, Default)]
pub struct AaModels {
    pub models: Vec<AaModel>,
    pub intel_version: Option<String>,
    pub as_of: Option<String>,
}

/// Fetch every page of the free model list. Any AA data key works.
/// Errors fail loudly — the caller decides fail-soft (no key
/// configured) vs warn-and-publish.
pub fn fetch_aa_models(base_url: &str, api_key: &str) -> Result<AaModels, Error> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut models: HashMap<String, AaModel> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut intel_version: Option<String> = None;
    let mut page = 1u32;
    loop {
        let body: Value = serde_json::from_slice(&get_bytes(
            &client,
            &format!("{base}{AA_FREE_PATH}?page={page}"),
            api_key,
        )?)?;
        let (records, version, more) = aggregate_page(&body, page);
        if intel_version.is_none() {
            intel_version = version;
        }
        // First page wins on duplicate slugs, like the benchmark
        // backfill: a re-listed model keeps its first reading.
        for record in records {
            if !models.contains_key(&record.slug) {
                order.push(record.slug.clone());
                models.insert(record.slug.clone(), record);
            }
        }
        if !more || page >= AA_MAX_PAGES {
            break;
        }
        page += 1;
    }
    order.sort();
    Ok(AaModels {
        models: order
            .iter()
            .filter_map(|slug| models.remove(slug))
            .collect(),
        intel_version,
        as_of: crate::now_rfc3339().ok(),
    })
}

fn get_bytes(
    client: &reqwest::blocking::Client,
    url: &str,
    api_key: &str,
) -> Result<Vec<u8>, Error> {
    let response = client.get(url).header("x-api-key", api_key).send()?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Fetch(format!("GET {url}: HTTP {status}")));
    }
    Ok(response.bytes()?.to_vec())
}

/// Parse one page: records plus the envelope version and whether
/// another page follows. Stops at `total_pages` even when `has_more`
/// lies, so a confused pager can't spin to the cap.
fn aggregate_page(body: &Value, page: u32) -> (Vec<AaModel>, Option<String>, bool) {
    let version = body
        .get("intelligence_index_version")
        .and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
    let total_pages = body
        .get("pagination")
        .and_then(|p| p.get("total_pages"))
        .and_then(Value::as_u64);
    let more = body
        .get("pagination")
        .and_then(|p| p.get("has_more"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && total_pages.is_none_or(|total| u64::from(page) < total);
    let records = body
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(parse_record)
        .collect();
    (records, version, more)
}

/// One record's scored fields. The slug is required (no join key, no
/// record); every value follows the embedded-extractor rules —
/// indexes are finite numbers `0..=100` rounded, costs and prices
/// finite and non-negative — and anything else drops the field,
/// never the record.
fn parse_record(record: &Value) -> Option<AaModel> {
    let slug = record.get("slug")?.as_str()?;
    if slug.is_empty() {
        return None;
    }
    let evaluations = record.get("evaluations");
    let index = |key: &str| {
        evaluations
            .and_then(|e| e.get(key))
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
            .map(|n| n.round() as i64)
    };
    let money = |value: Option<&Value>| {
        value
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
    };
    Some(AaModel {
        slug: slug.to_string(),
        name: record
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        intelligence: index("artificial_analysis_intelligence_index"),
        coding: index("artificial_analysis_coding_index"),
        agentic: index("artificial_analysis_agentic_index"),
        cost_per_task: money(
            record
                .get("artificial_analysis_intelligence_index_cost")
                .and_then(|c| c.get("cost_per_task"))
                .and_then(|c| c.get("total_cost")),
        ),
        price_input_m: money(record.get("pricing").and_then(|p| p.get("input"))),
        price_output_m: money(record.get("pricing").and_then(|p| p.get("output"))),
    })
}

/// Persist the fetch for the artifact: slug -> record object plus
/// the envelope version and the fetch timestamp. Read back with
/// `read_aa_models` / `read_aa_intel_version`.
pub fn store_aa_models(catalog: &Catalog, aa: &AaModels) -> Result<(), Error> {
    let models: serde_json::Map<String, Value> = aa
        .models
        .iter()
        .map(|m| {
            (
                m.slug.clone(),
                serde_json::json!({
                    "name": m.name,
                    "aa_intel_index": m.intelligence,
                    "aa_coding_index": m.coding,
                    "aa_agentic_index": m.agentic,
                    "aa_intel_version": aa.intel_version,
                    "aa_cost_per_task": m.cost_per_task,
                    "aa_pricing": {"input": m.price_input_m, "output": m.price_output_m},
                }),
            )
        })
        .collect();
    catalog.set_meta("aa_models", &Value::Object(models).to_string())?;
    catalog.set_meta("aa_as_of", aa.as_of.as_deref().unwrap_or(""))?;
    catalog.set_meta(
        "aa_intel_version",
        aa.intel_version.as_deref().unwrap_or(""),
    )?;
    Ok(())
}

/// Stored AA records by slug. Missing or unparseable (old
/// artifacts, keyless syncs) reads as empty: no data, no penalty.
/// Malformed entries drop individually — one bad record never wipes
/// the map.
pub fn read_aa_models(catalog: &Catalog) -> HashMap<String, AaModel> {
    catalog
        .get_meta("aa_models")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(slug, v)| {
                    if slug.is_empty() || !v.is_object() {
                        return None;
                    }
                    let pricing = v.get("aa_pricing");
                    Some((
                        slug.clone(),
                        AaModel {
                            slug,
                            name: v.get("name").and_then(Value::as_str).map(str::to_string),
                            intelligence: v.get("aa_intel_index").and_then(Value::as_i64),
                            coding: v.get("aa_coding_index").and_then(Value::as_i64),
                            agentic: v.get("aa_agentic_index").and_then(Value::as_i64),
                            cost_per_task: v
                                .get("aa_cost_per_task")
                                .and_then(Value::as_f64)
                                .filter(|n| n.is_finite() && *n >= 0.0),
                            price_input_m: pricing
                                .and_then(|p| p.get("input"))
                                .and_then(Value::as_f64)
                                .filter(|n| n.is_finite() && *n >= 0.0),
                            price_output_m: pricing
                                .and_then(|p| p.get("output"))
                                .and_then(Value::as_f64)
                                .filter(|n| n.is_finite() && *n >= 0.0),
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Envelope intelligence-index version of the stored fetch: the
/// version gate (Iter 43) compares per-model versions against this.
/// Empty when nothing was fetched.
pub fn read_aa_intel_version(catalog: &Catalog) -> Option<String> {
    catalog
        .get_meta("aa_intel_version")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page() -> Value {
        json!({
            "tier": "free",
            "intelligence_index_version": "4.3",
            "pagination": {"page": 1, "page_size": 200, "total_pages": 2, "has_more": true},
            "data": [
                {"id": "u1", "name": "Alpha", "slug": "alpha",
                 "evaluations": {
                     "artificial_analysis_intelligence_index": 71.4,
                     "artificial_analysis_coding_index": 64.6,
                     "artificial_analysis_agentic_index": null},
                 "artificial_analysis_intelligence_index_cost": {
                     "cost_per_task": {"total_cost": 0.0123}},
                 "pricing": {"input": 1.5, "output": 6.0}},
                {"id": "u2", "name": "Beta", "slug": "beta",
                 "evaluations": {
                     "artificial_analysis_intelligence_index": 101,
                     "artificial_analysis_coding_index": -1},
                 "pricing": {"input": "garbage", "output": -2}},
                {"id": "u3", "name": "NoSlug",
                 "evaluations": {"artificial_analysis_intelligence_index": 50}},
            ]
        })
    }

    #[test]
    fn page_parses_records_version_and_more() {
        let (records, version, more) = aggregate_page(&page(), 1);
        assert_eq!(version.as_deref(), Some("4.3"));
        assert!(more);
        assert_eq!(records.len(), 2);
        let alpha = &records[0];
        assert_eq!(alpha.slug, "alpha");
        assert_eq!(alpha.name.as_deref(), Some("Alpha"));
        assert_eq!(alpha.intelligence, Some(71));
        assert_eq!(alpha.coding, Some(65));
        assert_eq!(alpha.agentic, None);
        assert_eq!(alpha.cost_per_task, Some(0.0123));
        assert_eq!(alpha.price_input_m, Some(1.5));
        assert_eq!(alpha.price_output_m, Some(6.0));
        // Out-of-range indexes and bad prices drop the field, and the
        // slugless row drops entirely.
        let beta = &records[1];
        assert_eq!(beta.slug, "beta");
        assert_eq!(beta.intelligence, None);
        assert_eq!(beta.coding, None);
        assert_eq!(beta.price_input_m, None);
        assert_eq!(beta.price_output_m, None);
        // Numeric versions stringify; the last page stops even when
        // has_more lies.
        let numbered = json!({"intelligence_index_version": 5,
            "pagination": {"page": 2, "total_pages": 2, "has_more": true},
            "data": []});
        assert_eq!(aggregate_page(&numbered, 2).1.as_deref(), Some("5"));
        assert!(!aggregate_page(&numbered, 2).2);
        assert!(aggregate_page(&json!({}), 1).0.is_empty());
    }

    #[test]
    fn store_read_round_trip_and_empty() {
        let dir = std::env::temp_dir().join(format!("aa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let catalog = Catalog::open(&dir.join("t.db")).unwrap();
        catalog.init().unwrap();
        // Old artifacts and keyless syncs read as empty.
        assert!(read_aa_models(&catalog).is_empty());
        assert_eq!(read_aa_intel_version(&catalog), None);
        store_aa_models(
            &catalog,
            &AaModels {
                models: vec![AaModel {
                    slug: "alpha".to_string(),
                    name: Some("Alpha".to_string()),
                    intelligence: Some(71),
                    coding: None,
                    agentic: None,
                    cost_per_task: Some(0.0123),
                    price_input_m: Some(1.5),
                    price_output_m: None,
                }],
                intel_version: Some("4.3".to_string()),
                as_of: Some("2026-11-20T00:00:00Z".to_string()),
            },
        )
        .unwrap();
        let back = read_aa_models(&catalog);
        assert_eq!(back.len(), 1);
        let alpha = &back["alpha"];
        assert_eq!(alpha.intelligence, Some(71));
        assert_eq!(alpha.cost_per_task, Some(0.0123));
        assert_eq!(alpha.price_input_m, Some(1.5));
        assert_eq!(alpha.price_output_m, None);
        assert_eq!(read_aa_intel_version(&catalog).as_deref(), Some("4.3"));
        assert_eq!(
            catalog.get_meta("aa_as_of").unwrap().as_deref(),
            Some("2026-11-20T00:00:00Z")
        );
        // One bad entry never wipes the map.
        catalog
            .set_meta("aa_models", r#"{"ok": {"aa_intel_index": 50}, "bad": 7}"#)
            .unwrap();
        assert_eq!(read_aa_models(&catalog).len(), 1);
    }
}
