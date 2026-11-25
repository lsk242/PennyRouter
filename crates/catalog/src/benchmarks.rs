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
/// timestamp. `openrouter` carries the same body's OpenRouter-run
/// evals when present (Iter 42, no extra call).
#[derive(Debug)]
pub struct Benchmarks {
    pub intelligence: Vec<(String, i64)>,
    pub openrouter: Vec<(String, OpenRouterEvals)>,
    pub as_of: Option<String>,
}

/// Fetch the Artificial Analysis intelligence leaderboard. Any valid
/// OpenRouter key works; the caller shares `RANKINGS_API_KEY` with
/// the popularity fetch. Errors fail loudly — the caller decides
/// fail-soft (no key configured) vs warn-and-publish.
pub fn fetch_benchmarks(base_url: &str, api_key: &str) -> Result<Benchmarks, Error> {
    fetch_benchmarks_task(base_url, api_key, "intelligence")
}

/// Fetch one benchmarks task slice (Iter 42): `task_type` is
/// `intelligence` (the backfill) or `code` (Design Arena code
/// categories). Same key, same failure contract as
/// `fetch_benchmarks`.
pub fn fetch_benchmarks_task(
    base_url: &str,
    api_key: &str,
    task_type: &str,
) -> Result<Benchmarks, Error> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let body: Value = serde_json::from_slice(&get_bytes(
        &client,
        &format!("{base}/api/v1/benchmarks?source=artificial-analysis&task_type={task_type}"),
        api_key,
    )?)?;
    let as_of = body
        .get("meta")
        .and_then(|meta| meta.get("as_of"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(Benchmarks {
        intelligence: aggregate_task_index(&body, task_type),
        openrouter: aggregate_openrouter_evals(&body),
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
    aggregate_task_index(body, "intelligence")
}

/// One task slice's permaslug scores (Iter 42): the `{task}_index`
/// field on `artificial-analysis` rows (`intelligence_index`,
/// `coding_index`), same strict rules as `aggregate_intelligence`.
/// Unknown task types read nothing — a mispelled fetch parses empty
/// instead of guessing a field.
pub fn aggregate_task_index(body: &Value, task_type: &str) -> Vec<(String, i64)> {
    let field = match task_type {
        "intelligence" => "intelligence_index",
        "code" => "coding_index",
        _ => return Vec::new(),
    };
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
        let index = row.get(field).and_then(|v| match v {
            Value::Number(n) => n.as_f64(),
            _ => None,
        });
        match (slug, index) {
            (Some(slug), Some(index)) if index.is_finite() && (0.0..=100.0).contains(&index) => {
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

/// OpenRouter's own eval scores (Iter 42): `source == "openrouter"`
/// rows parsed from an already-fetched benchmarks body (no extra
/// call) for the assumed `gpqa` / `tau_bench` numeric fields,
/// `0..=100`. The field names are unverified against live payloads
/// (proposal open question 1): a shape mismatch parses empty and
/// the artifact simply carries no fallback — never a penalty.
pub fn aggregate_openrouter_evals(body: &Value) -> Vec<(String, OpenRouterEvals)> {
    let mut scores: HashMap<String, OpenRouterEvals> = HashMap::new();
    for row in body
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if row.get("source").and_then(Value::as_str) != Some("openrouter") {
            continue;
        }
        let Some(slug) = row.get("model_permaslug").and_then(Value::as_str) else {
            continue;
        };
        let field = |key: &str| {
            row.get(key)
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
                .map(|n| n.round() as i64)
        };
        let evals = OpenRouterEvals {
            gpqa: field("gpqa"),
            tau_bench: field("tau_bench"),
        };
        if evals.gpqa.is_none() && evals.tau_bench.is_none() {
            continue;
        }
        scores.entry(slug.to_string()).or_insert(evals);
    }
    let mut ranked: Vec<(String, OpenRouterEvals)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    ranked
}

/// One model's OpenRouter-run eval scores, when the benchmarks body
/// carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenRouterEvals {
    pub gpqa: Option<i64>,
    pub tau_bench: Option<i64>,
}

/// Persist the fetch for the artifact: slug -> intelligence object
/// plus the citation timestamp. Read back with
/// `read_benchmark_intel`.
pub fn store_benchmarks(catalog: &Catalog, benchmarks: &Benchmarks) -> Result<(), Error> {
    store_task_benchmarks(catalog, "intelligence", benchmarks)
}

/// Persist one task slice (Iter 42): `intelligence` keeps the
/// historical `benchmark_intel` keys; `code` takes `benchmark_code`
/// + `benchmark_code_as_of`.
///
/// Unknown tasks are a caller bug and fail loudly instead of writing
/// a mystery key.
pub fn store_task_benchmarks(
    catalog: &Catalog,
    task_type: &str,
    benchmarks: &Benchmarks,
) -> Result<(), Error> {
    let (scores_key, as_of_key) = match task_type {
        "intelligence" => ("benchmark_intel", "benchmark_as_of"),
        "code" => ("benchmark_code", "benchmark_code_as_of"),
        _ => {
            return Err(Error::Fetch(format!(
                "unknown benchmark task {task_type:?}"
            )))
        }
    };
    let scores: serde_json::Map<String, Value> = benchmarks
        .intelligence
        .iter()
        .map(|(slug, index)| (slug.clone(), serde_json::json!(index)))
        .collect();
    catalog.set_meta(scores_key, &Value::Object(scores).to_string())?;
    catalog.set_meta(as_of_key, benchmarks.as_of.as_deref().unwrap_or(""))?;
    Ok(())
}

/// Persist OpenRouter-run evals (Iter 42): slug -> `{gpqa,
/// tau_bench}` plus the citation of the body they were parsed from.
/// Read back with `read_openrouter_evals`.
pub fn store_openrouter_evals(
    catalog: &Catalog,
    evals: &[(String, OpenRouterEvals)],
    as_of: Option<&str>,
) -> Result<(), Error> {
    let scores: serde_json::Map<String, Value> = evals
        .iter()
        .map(|(slug, e)| {
            (
                slug.clone(),
                serde_json::json!({"gpqa": e.gpqa, "tau_bench": e.tau_bench}),
            )
        })
        .collect();
    catalog.set_meta("benchmark_openrouter", &Value::Object(scores).to_string())?;
    catalog.set_meta("benchmark_openrouter_as_of", as_of.unwrap_or(""))?;
    Ok(())
}

/// Backfill map as slug -> intelligence. Missing or unparseable (old
/// artifacts, keyless or failed syncs) reads as empty: no backfill,
/// no penalty.
pub fn read_benchmark_intel(catalog: &Catalog) -> HashMap<String, i64> {
    read_benchmark_task(catalog, "benchmark_intel")
}

/// Code-task backfill map (Iter 42): same tolerance as
/// `read_benchmark_intel`.
pub fn read_benchmark_code(catalog: &Catalog) -> HashMap<String, i64> {
    read_benchmark_task(catalog, "benchmark_code")
}

fn read_benchmark_task(catalog: &Catalog, key: &str) -> HashMap<String, i64> {
    catalog
        .get_meta(key)
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

/// Stored OpenRouter evals by slug. Missing or unparseable reads as
/// empty; malformed entries drop individually.
pub fn read_openrouter_evals(catalog: &Catalog) -> HashMap<String, OpenRouterEvals> {
    catalog
        .get_meta("benchmark_openrouter")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(slug, v)| {
                    let evals = OpenRouterEvals {
                        gpqa: v.get("gpqa").and_then(Value::as_i64),
                        tau_bench: v.get("tau_bench").and_then(Value::as_i64),
                    };
                    if evals.gpqa.is_none() && evals.tau_bench.is_none() {
                        return None;
                    }
                    Some((slug, evals))
                })
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
                openrouter: Vec::new(),
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

    #[test]
    fn task_index_reads_code_and_rejects_unknown_tasks() {
        let body = json!({"data": [
            {"source": "artificial-analysis", "model_permaslug": "a/x",
             "intelligence_index": 70, "coding_index": 64.6},
            {"source": "artificial-analysis", "model_permaslug": "b/y",
             "coding_index": 101},
            {"source": "openrouter", "model_permaslug": "c/z",
             "coding_index": 90},
        ]});
        assert_eq!(
            aggregate_task_index(&body, "code"),
            vec![("a/x".to_string(), 65)]
        );
        assert_eq!(
            aggregate_task_index(&body, "intelligence"),
            vec![("a/x".to_string(), 70)]
        );
        assert!(aggregate_task_index(&body, "agentic").is_empty());
    }

    #[test]
    fn openrouter_evals_parse_and_round_trip() {
        let body = json!({"data": [
            {"source": "openrouter", "model_permaslug": "a/x",
             "gpqa": 72.4, "tau_bench": 60},
            {"source": "openrouter", "model_permaslug": "b/y",
             "gpqa": 101},
            {"source": "artificial-analysis", "model_permaslug": "c/z",
             "gpqa": 99},
            {"source": "openrouter", "model_permaslug": "d/w"},
        ]});
        assert_eq!(
            aggregate_openrouter_evals(&body),
            vec![(
                "a/x".to_string(),
                OpenRouterEvals {
                    gpqa: Some(72),
                    tau_bench: Some(60)
                }
            )]
        );
        let dir = std::env::temp_dir().join(format!("benchmarks-42-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let catalog = Catalog::open(&dir.join("t.db")).unwrap();
        catalog.init().unwrap();
        assert!(read_benchmark_code(&catalog).is_empty());
        assert!(read_openrouter_evals(&catalog).is_empty());
        store_task_benchmarks(
            &catalog,
            "code",
            &Benchmarks {
                intelligence: vec![("a/x".to_string(), 65)],
                openrouter: Vec::new(),
                as_of: Some("t".to_string()),
            },
        )
        .unwrap();
        assert_eq!(
            read_benchmark_code(&catalog),
            HashMap::from([("a/x".to_string(), 65)])
        );
        assert!(store_task_benchmarks(
            &catalog,
            "agentic",
            &Benchmarks {
                intelligence: Vec::new(),
                openrouter: Vec::new(),
                as_of: None,
            },
        )
        .is_err());
        store_openrouter_evals(
            &catalog,
            &[(
                "a/x".to_string(),
                OpenRouterEvals {
                    gpqa: Some(72),
                    tau_bench: None,
                },
            )],
            Some("t"),
        )
        .unwrap();
        assert_eq!(
            read_openrouter_evals(&catalog).get("a/x"),
            Some(&OpenRouterEvals {
                gpqa: Some(72),
                tau_bench: None
            })
        );
    }
}
