//! Popularity pins for the gateway menu: top OpenRouter models and
//! hot apps from the Data API (`rankings-daily`, `app-rankings`),
//! fetched at sync time and stored in `sync_meta` as JSON.
//!
//! `rankings-daily` rows aggregate to top `POPULAR_MODELS_TOP_N`
//! permaslugs by summed tokens over the trailing
//! `RANKINGS_WINDOW_DAYS` (the `other` aggregate row is skipped);
//! `app-rankings` contributes top `POPULAR_APPS_TOP_N` app names.
//! The API's `meta.as_of` is stored alongside (`popular_as_of`) for
//! the CC-BY citation the menu/docs carry. Gateway read path only
//! needs the slugs: the menu pins `openrouter/<slug>` ids (served
//! passthrough — no routing change), `/ui` shows both lists.
//!
//! No key means no fetch: the menu still serves virtual ids plus the
//! operator `MODAL_MODELS`/`BASETEN_MODELS` lists. Two calls per sync
//! (far inside the 30/min, 500/day limits); sync-time only, never
//! the request path.

use crate::{Catalog, Error};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Trailing window for "popular now" (API default would be 30d).
pub const RANKINGS_WINDOW_DAYS: i64 = 7;
/// Pinned `openrouter/<slug>` menu ids.
pub const POPULAR_MODELS_TOP_N: usize = 10;
/// Hot apps shown on `/ui` (the API publishes no per-app models).
pub const POPULAR_APPS_TOP_N: usize = 10;

/// Aggregated popularity: models by summed tokens, apps by volume.
/// `models` is the menu's top-N pin list; `all_models` is the whole
/// window, which the value score needs as a per-model factor
/// (Iter 34).
pub struct Popularity {
    pub models: Vec<(String, u64)>,
    pub all_models: Vec<(String, u64)>,
    pub apps: Vec<(String, u64, u64)>,
    pub as_of: Option<String>,
}

/// Fetch both datasets and aggregate. Any valid OpenRouter key works;
/// the same key as inference is fine. Errors fail the sync loudly —
/// the caller decides fail-soft (no key configured) vs fail-loud.
pub fn fetch_popularity(base_url: &str, api_key: &str) -> Result<Popularity, Error> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let (end, start) = rankings_window();
    let rankings = get_json(
        &client,
        &format!("{base}/api/v1/datasets/rankings-daily?start_date={start}&end_date={end}"),
        api_key,
    )?;
    let apps = get_json(
        &client,
        &format!("{base}/api/v1/datasets/app-rankings?sort=popular&limit={}&start_date={start}&end_date={end}", POPULAR_APPS_TOP_N),
        api_key,
    )?;
    let as_of = rankings
        .get("meta")
        .and_then(|meta| meta.get("as_of"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let all_models = aggregate_models_all(&rankings);
    Ok(Popularity {
        models: all_models
            .iter()
            .take(POPULAR_MODELS_TOP_N)
            .cloned()
            .collect(),
        all_models,
        apps: aggregate_apps(&apps, POPULAR_APPS_TOP_N),
        as_of,
    })
}

/// Most recent completed UTC day + trailing window, `YYYY-MM-DD`.
fn rankings_window() -> (String, String) {
    let end = time::OffsetDateTime::now_utc().date() - time::Duration::days(1);
    let start = end - time::Duration::days(RANKINGS_WINDOW_DAYS - 1);
    (ymd(end), ymd(start))
}

fn ymd(date: time::Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

fn get_json(client: &reqwest::blocking::Client, url: &str, api_key: &str) -> Result<Value, Error> {
    let body = client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()?
        .error_for_status()?;
    serde_json::from_slice(&body.bytes()?).map_err(Error::from)
}

/// Sum tokens per permaslug over the window, skipping the `other`
/// aggregate row; unparseable counts never guess (row dropped).
/// Truncated to `top_n` for the menu pin list.
pub fn aggregate_models(body: &Value, top_n: usize) -> Vec<(String, u64)> {
    let mut ranked = aggregate_models_all(body);
    ranked.truncate(top_n);
    ranked
}

/// The whole window, best first: the value score's popularity input
/// (Iter 34). Same parsing rules as `aggregate_models`.
pub fn aggregate_models_all(body: &Value) -> Vec<(String, u64)> {
    let mut totals: HashMap<String, u64> = HashMap::new();
    for row in body
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let slug = row.get("model_permaslug").and_then(Value::as_str);
        let tokens = row.get("total_tokens").and_then(|v| match v {
            Value::String(s) => s.parse().ok(),
            Value::Number(n) => n.as_u64(),
            _ => None,
        });
        match (slug, tokens) {
            (Some("other"), _) | (_, None) | (None, _) => {}
            (Some(slug), Some(tokens)) => {
                *totals.entry(slug.to_string()).or_default() += tokens;
            }
        }
    }
    let mut ranked: Vec<(String, u64)> = totals.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// App names by volume; unparseable rows dropped, never guessed.
pub fn aggregate_apps(body: &Value, top_n: usize) -> Vec<(String, u64, u64)> {
    let mut apps: Vec<(String, u64, u64)> = body
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| {
            let name = row.get("app_name")?.as_str()?.to_string();
            let tokens = match row.get("total_tokens")? {
                Value::String(s) => s.parse().ok()?,
                Value::Number(n) => n.as_u64()?,
                _ => return None,
            };
            let requests = match row.get("total_requests")? {
                Value::Number(n) => n.as_u64()?,
                Value::String(s) => s.parse().ok()?,
                _ => return None,
            };
            Some((name, tokens, requests))
        })
        .collect();
    apps.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    apps.truncate(top_n);
    apps
}

/// Persist the fetch for the artifact: slugs, apps, and the citation
/// timestamp. Read back with `read_popularity`.
pub fn store_popularity(catalog: &Catalog, popularity: &Popularity) -> Result<(), Error> {
    let models: Vec<serde_json::Value> = popularity
        .models
        .iter()
        .map(|(slug, tokens)| serde_json::json!({"slug": slug, "tokens": tokens}))
        .collect();
    let apps: Vec<serde_json::Value> = popularity
        .apps
        .iter()
        .map(|(name, tokens, requests)| {
            serde_json::json!({"name": name, "tokens": tokens, "requests": requests})
        })
        .collect();
    catalog.set_meta(
        "popular_models",
        &serde_json::Value::Array(models).to_string(),
    )?;
    catalog.set_meta("popular_apps", &serde_json::Value::Array(apps).to_string())?;
    catalog.set_meta("popular_as_of", popularity.as_of.as_deref().unwrap_or(""))?;
    // Full window as a slug -> tokens object: the value score reads it
    // per model (Iter 34). The menu keeps the top-N array above.
    let tokens: serde_json::Map<String, Value> = popularity
        .all_models
        .iter()
        .map(|(slug, tokens)| (slug.clone(), serde_json::json!(tokens)))
        .collect();
    catalog.set_meta("popular_tokens", &Value::Object(tokens).to_string())?;
    Ok(())
}

/// Full popularity window as slug -> tokens. Missing or unparseable
/// (old artifacts, keyless syncs) reads as empty: no factor, no
/// penalty.
pub fn read_popular_tokens(catalog: &Catalog) -> HashMap<String, u64> {
    catalog
        .get_meta("popular_tokens")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(slug, tokens)| Some((slug, tokens.as_u64()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// Read back: pinned slugs, hot app names, citation timestamp.
/// Missing keys (old artifacts, keyless syncs) yield empties.
pub fn read_popularity(catalog: &Catalog) -> (Vec<String>, Vec<String>, Option<String>) {
    let models = catalog
        .get_meta("popular_models")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m.get("slug")?.as_str().map(str::to_string))
        .collect();
    let apps = catalog
        .get_meta("popular_apps")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|a| a.get("name")?.as_str().map(str::to_string))
        .collect();
    let as_of = catalog
        .get_meta("popular_as_of")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty());
    (models, apps, as_of)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn models_aggregate_and_skip_other() {
        let body = json!({"data": [
            {"date": "2026-11-01", "model_permaslug": "a/x", "total_tokens": "100"},
            {"date": "2026-11-02", "model_permaslug": "a/x", "total_tokens": "50"},
            {"date": "2026-11-02", "model_permaslug": "b/y", "total_tokens": 200u64},
            {"date": "2026-11-02", "model_permaslug": "other", "total_tokens": "99999"},
            {"date": "2026-11-02", "model_permaslug": "c/z", "total_tokens": "abc"},
            {"date": "2026-11-02", "total_tokens": "10"},
        ]});
        assert_eq!(
            aggregate_models(&body, 10),
            vec![("b/y".to_string(), 200), ("a/x".to_string(), 150),]
        );
        assert_eq!(aggregate_models(&body, 1).len(), 1);
        assert!(aggregate_models(&json!({}), 10).is_empty());
    }

    #[test]
    fn apps_parse_and_truncate() {
        let body = json!({"data": [
            {"rank": 1, "app_name": "Cline", "total_tokens": "10", "total_requests": 5},
            {"rank": 2, "app_name": "Roo", "total_tokens": 7u64, "total_requests": 3},
            {"rank": 3, "app_name": "Bad", "total_tokens": "x", "total_requests": 1},
        ]});
        assert_eq!(
            aggregate_apps(&body, 10),
            vec![("Cline".to_string(), 10, 5), ("Roo".to_string(), 7, 3),]
        );
        assert_eq!(aggregate_apps(&body, 1).len(), 1);
    }

    #[test]
    fn window_is_seven_days_ending_yesterday() {
        let (end, start) = rankings_window();
        assert_eq!(end.len(), 10);
        assert_eq!(start.len(), 10);
        assert!(start < end);
    }

    #[test]
    fn store_read_round_trip_and_empty() {
        let dir = std::env::temp_dir().join(format!("popular-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let catalog = Catalog::open(&dir.join("t.db")).unwrap();
        catalog.init().unwrap();
        // Old artifacts and keyless syncs read as empty.
        assert_eq!(read_popularity(&catalog), (Vec::new(), Vec::new(), None));
        store_popularity(
            &catalog,
            &Popularity {
                models: vec![("a/x".to_string(), 5)],
                all_models: vec![("a/x".to_string(), 5)],
                apps: vec![("Cline".to_string(), 9, 2)],
                as_of: Some("2026-11-04T00:00:00.000Z".to_string()),
            },
        )
        .unwrap();
        assert_eq!(
            read_popularity(&catalog),
            (
                vec!["a/x".to_string()],
                vec!["Cline".to_string()],
                Some("2026-11-04T00:00:00.000Z".to_string())
            )
        );
    }
}
