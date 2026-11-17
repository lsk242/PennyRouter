//! Intelligence-index backfill from the OpenRouter benchmarks API.
//!
//! `GET /v1/models` embeds `benchmarks.artificial_analysis` per model,
//! but coverage is partial: many servable models carry no
//! `intelligence_index` at all. The dedicated `GET /api/v1/benchmarks`
//! endpoint (`source=artificial-analysis&task_type=intelligence`) is
//! the fuller dataset, keyed by `model_permaslug` — one call per sync
//! cycle (far inside the 30/min, 500/day limits), sync-time only,
//! never the request path.
//!
//! Fetched rows backfill *missing* embedded values only: a model whose
//! payload already carries an intelligence index keeps it, so this
//! step can only add scores, never move existing ones. Join key is the
//! model's `canonical_slug` with the plain id as fallback — the same
//! rule as the popularity window. Stored in `sync_meta` as JSON, like
//! popularity; keyless syncs and old artifacts read as empty (no
//! backfill, no penalty). Attribute Artificial Analysis when
//! republishing the scores.

use crate::{Catalog, Error};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Backfill rows: permaslug intelligence scores plus the citation
/// timestamp.
#[derive(Debug)]
pub struct Benchmarks {
    pub intelligence: Vec<(String, i64)>,
    pub as_of: Option<String>,
}

/// Fetch the Artificial Analysis intelligence leaderboard. Any valid
/// OpenRouter key works; the caller shares `RANKINGS_API_KEY` with
/// the popularity fetch. Errors fail loudly — the caller decides
/// fail-soft (no key configured) vs warn-and-publish.
pub fn fetch_benchmarks(base_url: &str, api_key: &str) -> Result<Benchmarks, Error> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let body: Value = serde_json::from_slice(&get_bytes(
        &client,
        &format!("{base}/api/v1/benchmarks?source=artificial-analysis&task_type=intelligence"),
        api_key,
    )?)?;
    let as_of = body
        .get("meta")
        .and_then(|meta| meta.get("as_of"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(Benchmarks {
        intelligence: aggregate_intelligence(&body),
        as_of,
    })
}

fn get_bytes(
    client: &reqwest::blocking::Client,
    url: &str,
    api_key: &str,
) -> Result<Vec<u8>, Error> {
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Fetch(format!("GET {url}: HTTP {status}")));
    }
    Ok(response.bytes()?.to_vec())
}

/// Permaslug intelligence scores, best first. Only
/// `artificial-analysis` rows count (a mixed response must not leak
/// another source's numbers in); values follow the same rules as the
/// embedded extractor — finite numbers in `0..=100`, rounded, out of
/// range or unparseable dropped, never guessed. First row wins on
/// duplicate slugs.
pub fn aggregate_intelligence(body: &Value) -> Vec<(String, i64)> {
    let mut scores: HashMap<String, i64> = HashMap::new();
    for row in body
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if row.get("source").and_then(Value::as_str) != Some("artificial-analysis") {
            continue;
        }
        let slug = row.get("model_permaslug").and_then(Value::as_str);
        let index = row.get("intelligence_index").and_then(|v| match v {
            Value::Number(n) => n.as_f64(),
            _ => None,
        });
        match (slug, index) {
            (Some(slug), Some(index))
                if index.is_finite() && (0.0..=100.0).contains(&index) =>
            {
                scores
                    .entry(slug.to_string())
                    .or_insert_with(|| index.round() as i64);
            }
            _ => {}
        }
    }
    let mut ranked: Vec<(String, i64)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// Persist the fetch for the artifact: slug -> intelligence object
/// plus the citation timestamp. Read back with
/// `read_benchmark_intel`.
pub fn store_benchmarks(catalog: &Catalog, benchmarks: &Benchmarks) -> Result<(), Error> {
    let intel: serde_json::Map<String, Value> = benchmarks
        .intelligence
        .iter()
        .map(|(slug, index)| (slug.clone(), serde_json::json!(index)))
        .collect();
    catalog.set_meta("benchmark_intel", &Value::Object(intel).to_string())?;
    catalog.set_meta(
        "benchmark_as_of",
        benchmarks.as_of.as_deref().unwrap_or(""),
    )?;
    Ok(())
}

/// Backfill map as slug -> intelligence. Missing or unparseable (old
/// artifacts, keyless or failed syncs) reads as empty: no backfill,
/// no penalty.
pub fn read_benchmark_intel(catalog: &Catalog) -> HashMap<String, i64> {
    catalog
        .get_meta("benchmark_intel")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(slug, index)| Some((slug, index.as_i64()?)))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn intelligence_parses_rounds_and_skips_foreign_rows() {
        let body = json!({"data": [
            {"source": "artificial-analysis", "model_permaslug": "a/x",
             "intelligence_index": 71.4},
            {"source": "artificial-analysis", "model_permaslug": "b/y",
             "intelligence_index": 51.5},
            {"source": "artificial-analysis", "model_permaslug": "dup",
             "intelligence_index": 90},
            {"source": "artificial-analysis", "model_permaslug": "dup",
             "intelligence_index": 10},
            {"source": "openrouter", "model_permaslug": "c/z",
             "intelligence_index": 99},
            {"source": "artificial-analysis", "model_permaslug": "nulls",
             "intelligence_index": null},
            {"source": "artificial-analysis", "model_permaslug": "str",
             "intelligence_index": "71"},
            {"source": "artificial-analysis", "model_permaslug": "high",
             "intelligence_index": 101},
            {"source": "artificial-analysis", "model_permaslug": "neg",
             "intelligence_index": -1},
            {"source": "artificial-analysis", "intelligence_index": 50},
        ]});
        assert_eq!(
            aggregate_intelligence(&body),
            vec![
                ("dup".to_string(), 90),
                ("a/x".to_string(), 71),
                ("b/y".to_string(), 52),
            ]
        );
        assert!(aggregate_intelligence(&json!({})).is_empty());
    }

    #[test]
    fn store_read_round_trip_and_empty() {
        let dir = std::env::temp_dir().join(format!("benchmarks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let catalog = Catalog::open(&dir.join("t.db")).unwrap();
        catalog.init().unwrap();
        // Old artifacts and keyless syncs read as empty.
        assert!(read_benchmark_intel(&catalog).is_empty());
        store_benchmarks(
            &catalog,
            &Benchmarks {
                intelligence: vec![("a/x".to_string(), 71)],
                as_of: Some("2026-11-04T00:00:00.000Z".to_string()),
            },
        )
        .unwrap();
        assert_eq!(
            read_benchmark_intel(&catalog),
            HashMap::from([("a/x".to_string(), 71)])
        );
        assert_eq!(
            catalog.get_meta("benchmark_as_of").unwrap(),
            Some("2026-11-04T00:00:00.000Z".to_string())
        );
    }
}
