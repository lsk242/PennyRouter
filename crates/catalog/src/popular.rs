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
//! Iter 42 grows the rankings fetch to the recent window plus three
//! priors in one wider call: the pins keep their trailing-week
//! meaning over the recent slice, while `aggregate_activity` adds
//! per-slug level (recent share) + trend (recent vs prior mean) for
//! the score. Trending and coding app lists plus the task Code
//! share ride separate fetches with their own warn-and-continue
//! blocks, so one failing dataset never kills the pins.
//!
//! ```rust
//! // Fetch policy: at most 1 request/hour to the OpenRouter Data API
//! // per endpoint group per sync host (self-imposed; upstream allows
//! // 30/min per key, 500/day per account). Syncs run daily by
//! // default, so a few small queries per sync stays far inside both.
//! // Sync-time only, never the request path.
//! ```
//!
//! No key means no fetch: the menu still serves virtual ids plus the
//! operator `MODAL_MODELS`/`BASETEN_MODELS` lists.

use crate::{Catalog, Error};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Trailing window for "popular now" (API default would be 30d).
pub const RANKINGS_WINDOW_DAYS: i64 = 7;
/// Fetched span: the recent window plus three priors, one call.
/// The pins slice the recent window back out; activity reads both.
pub const RANKINGS_SPAN_DAYS: i64 = 28;
/// Pinned `openrouter/<slug>` menu ids.
pub const POPULAR_MODELS_TOP_N: usize = 10;
/// Hot apps shown on `/ui` (the API publishes no per-app models).
pub const POPULAR_APPS_TOP_N: usize = 10;

/// Aggregated popularity: models by summed tokens, apps by volume.
/// `models` is the menu's top-N pin list; `all_models` is the whole
/// window, which the value score needs as a per-model factor
/// (Iter 34); `activity` is the per-slug level + trend the v2 score
/// reads (Iter 42).
pub struct Popularity {
    pub models: Vec<(String, u64)>,
    pub all_models: Vec<(String, u64)>,
    pub apps: Vec<(String, u64, u64)>,
    pub activity: Vec<(String, ModelActivity)>,
    pub as_of: Option<String>,
}

/// Per-slug usage (Iter 42): `level` is the recent-window share of
/// the top slug (`0..=1`, top is `1.0`); `trend` is recent daily
/// tokens over prior daily tokens (`1.0` = flat, `>1` rising,
/// `<1` falling; `1.0` neutral when no prior window exists).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelActivity {
    pub level: f64,
    pub trend: f64,
}

/// Fetch both datasets and aggregate. Any valid OpenRouter key works;
/// the same key as inference is fine. Errors fail the sync loudly —
/// the caller decides fail-soft (no key configured) vs fail-loud.
pub fn fetch_popularity(base_url: &str, api_key: &str) -> Result<Popularity, Error> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let (end, start) = rankings_span();
    let rankings = get_json(
        &client,
        &format!("{base}/api/v1/datasets/rankings-daily?start_date={start}&end_date={end}"),
        api_key,
    )?;
    // App lists stay trailing-week display data on the narrow span.
    let (week_end, week_start) = rankings_window();
    let apps = get_json(
        &client,
        &format!("{base}/api/v1/datasets/app-rankings?sort=popular&limit={}&start_date={week_start}&end_date={week_end}", POPULAR_APPS_TOP_N),
        api_key,
    )?;
    let as_of = rankings
        .get("meta")
        .and_then(|meta| meta.get("as_of"))
        .and_then(Value::as_str)
        .map(str::to_string);
    // Pins keep trailing-week meaning over the recent slice of the
    // wider span; activity reads both slices.
    let (recent, _) = split_ranking_windows(&rankings);
    let all_models = rank_totals(&recent);
    Ok(Popularity {
        models: all_models
            .iter()
            .take(POPULAR_MODELS_TOP_N)
            .cloned()
            .collect(),
        all_models,
        apps: aggregate_apps(&apps, POPULAR_APPS_TOP_N),
        activity: aggregate_activity(&rankings),
        as_of,
    })
}

/// App rankings beyond the popular list (Iter 42): `sort` is
/// `popular` or `trending`, `category` narrows to e.g. `coding`.
/// Separate fetch with its own warn-and-continue block, so a bad
/// sort or category never kills the pins.
pub fn fetch_app_rankings(
    base_url: &str,
    api_key: &str,
    sort: &str,
    category: Option<&str>,
) -> Result<AppList, Error> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let (end, start) = rankings_window();
    let mut url = format!(
        "{base}/api/v1/datasets/app-rankings?sort={sort}&limit={}&start_date={start}&end_date={end}",
        POPULAR_APPS_TOP_N
    );
    if let Some(category) = category {
        url.push_str(&format!("&category={category}"));
    }
    let body = get_json(&client, &url, api_key)?;
    Ok(AppList {
        apps: aggregate_apps(&body, POPULAR_APPS_TOP_N),
        as_of: body
            .get("meta")
            .and_then(|meta| meta.get("as_of"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// One app-rankings fetch: names by volume plus the citation.
pub struct AppList {
    pub apps: Vec<(String, u64, u64)>,
    pub as_of: Option<String>,
}

/// Task traffic share for the Code macro-category (Iter 42): sampled
/// relative shares per task. The endpoint shape is assumed from the
/// datasets family (`task-rankings` with dated rows); a 404 or a
/// shape mismatch warns and publishes without it — auxiliary data
/// must never fail a sync.
pub fn fetch_task_share(base_url: &str, api_key: &str) -> Result<TaskShare, Error> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let (end, start) = rankings_window();
    let body = get_json(
        &client,
        &format!("{base}/api/v1/datasets/task-rankings?start_date={start}&end_date={end}"),
        api_key,
    )?;
    Ok(TaskShare {
        code_share: aggregate_task_code_share(&body),
        as_of: body
            .get("meta")
            .and_then(|meta| meta.get("as_of"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Task share fetch: the Code fraction plus the citation. `None`
/// means the response carried no usable task rows.
pub struct TaskShare {
    pub code_share: Option<f64>,
    pub as_of: Option<String>,
}

/// Most recent completed UTC day + trailing window, `YYYY-MM-DD`.
fn rankings_window() -> (String, String) {
    let end = time::OffsetDateTime::now_utc().date() - time::Duration::days(1);
    let start = end - time::Duration::days(RANKINGS_WINDOW_DAYS - 1);
    (ymd(end), ymd(start))
}

/// Most recent completed UTC day + fetched span (recent + priors),
/// `YYYY-MM-DD`.
fn rankings_span() -> (String, String) {
    let end = time::OffsetDateTime::now_utc().date() - time::Duration::days(1);
    let start = end - time::Duration::days(RANKINGS_SPAN_DAYS - 1);
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
    rank_totals(&data_rows(body).collect::<Vec<_>>())
}

/// Split `rankings-daily` rows into the recent window (the seven
/// newest present dates) and the prior windows (everything older).
/// Bounds derive from the data, not the clock, so late syncs and
/// fixtures split the same way. Dateless rows drop — a row that
/// cannot be placed in either window is never guessed into one.
pub fn split_ranking_windows(body: &Value) -> (Vec<&Value>, Vec<&Value>) {
    let mut dates: Vec<&str> = data_rows(body).filter_map(row_date).collect();
    dates.sort_unstable();
    dates.dedup();
    let cutoff = dates.len().saturating_sub(RANKINGS_WINDOW_DAYS as usize);
    let recent: std::collections::HashSet<&str> = dates[cutoff..].iter().copied().collect();
    let mut recent_rows = Vec::new();
    let mut prior_rows = Vec::new();
    for row in data_rows(body) {
        match row_date(row) {
            Some(date) if recent.contains(date) => recent_rows.push(row),
            Some(_) => prior_rows.push(row),
            None => {}
        }
    }
    (recent_rows, prior_rows)
}

/// Per-slug activity (Iter 42): level is the recent-window share of
/// the top slug, trend is recent daily tokens over prior daily
/// tokens (per-day rates, so a ragged prior span never skews the
/// ratio). The `other` row, dateless rows, and unparseable counts
/// drop like everywhere else; no prior window means neutral `1.0`.
/// Best level first, slug-stable on ties.
pub fn aggregate_activity(body: &Value) -> Vec<(String, ModelActivity)> {
    let (recent, prior) = split_ranking_windows(body);
    let sum = |rows: &[&Value]| {
        let mut totals: HashMap<String, u64> = HashMap::new();
        for row in rows {
            if let Some((slug, tokens)) = row_tokens(row) {
                *totals.entry(slug.to_string()).or_default() += tokens;
            }
        }
        totals
    };
    let recent_totals = sum(&recent);
    let prior_totals = sum(&prior);
    let days = |rows: &[&Value]| {
        rows.iter()
            .filter_map(|row| row_date(row))
            .collect::<std::collections::HashSet<_>>()
            .len()
            .max(1) as f64
    };
    let recent_days = days(&recent);
    let prior_days = days(&prior);
    let max_recent = recent_totals.values().copied().max().unwrap_or(0);
    let mut slugs: Vec<&String> = recent_totals
        .keys()
        .chain(prior_totals.keys())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    slugs.sort();
    let mut out: Vec<(String, ModelActivity)> = slugs
        .into_iter()
        .map(|slug| {
            let recent = recent_totals.get(slug).copied().unwrap_or(0) as f64;
            let prior = prior_totals.get(slug).copied().unwrap_or(0);
            let level = if max_recent == 0 {
                0.0
            } else {
                recent / max_recent as f64
            };
            // No prior tokens means no baseline: neutral, never a
            // divide-by-zero spike. A brand-new model reads flat
            // until a second window exists to compare against.
            let trend = if prior == 0 {
                1.0
            } else {
                (recent / recent_days) / (prior as f64 / prior_days)
            };
            (
                slug.clone(),
                ModelActivity {
                    level,
                    trend: trend.clamp(0.0, 99.0),
                },
            )
        })
        .collect();
    out.sort_by(|a, b| {
        b.1.level
            .partial_cmp(&a.1.level)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// Code macro-category share of task traffic (Iter 42): rows name a
/// task (`task`, `category`, or `name`) with `total_tokens` counts;
/// the share is Code tokens over all tokens. Labels match
/// case-insensitively on `code`/`coding`. No usable rows means
/// `None` — a missing dataset is unknown, never zero share.
pub fn aggregate_task_code_share(body: &Value) -> Option<f64> {
    let mut code = 0u64;
    let mut total = 0u64;
    let mut any = false;
    for row in data_rows(body) {
        let label = row
            .get("task")
            .or_else(|| row.get("category"))
            .or_else(|| row.get("name"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_lowercase());
        let tokens = row.get("total_tokens").and_then(|v| match v {
            Value::String(s) => s.parse().ok(),
            Value::Number(n) => n.as_u64(),
            _ => None,
        });
        if let (Some(label), Some(tokens)) = (label, tokens) {
            any = true;
            total += tokens;
            if label == "code" || label == "coding" {
                code += tokens;
            }
        }
    }
    if !any || total == 0 {
        return None;
    }
    Some(code as f64 / total as f64)
}

fn data_rows(body: &Value) -> impl Iterator<Item = &Value> {
    body.get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
}

fn row_date(row: &Value) -> Option<&str> {
    row.get("date").and_then(Value::as_str)
}

/// One row's (slug, tokens): the `other` aggregate, missing slugs,
/// and unparseable counts are `None` (row dropped, never guessed).
fn row_tokens(row: &Value) -> Option<(&str, u64)> {
    let slug = row.get("model_permaslug").and_then(Value::as_str)?;
    if slug == "other" {
        return None;
    }
    let tokens = row.get("total_tokens").and_then(|v| match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    })?;
    Some((slug, tokens))
}

/// Sum one row slice per slug, best first, slug-stable on ties: the
/// shared core behind the full-window, recent-slice, and activity
/// sums.
fn rank_totals(rows: &[&Value]) -> Vec<(String, u64)> {
    let mut totals: HashMap<String, u64> = HashMap::new();
    for row in rows {
        if let Some((slug, tokens)) = row_tokens(row) {
            *totals.entry(slug.to_string()).or_default() += tokens;
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
    // Per-slug activity as level + trend (Iter 42): the v2 score
    // reads it per model. Same slug keys as `popular_tokens`.
    let activity: serde_json::Map<String, Value> = popularity
        .activity
        .iter()
        .map(|(slug, a)| {
            (
                slug.clone(),
                serde_json::json!({"level": a.level, "trend": a.trend}),
            )
        })
        .collect();
    catalog.set_meta("activity_tokens", &Value::Object(activity).to_string())?;
    Ok(())
}

/// Persist one auxiliary app list under its own meta keys (Iter 42).
/// `key` is `trending` or `coding`; shape matches `popular_apps`.
pub fn store_app_list(catalog: &Catalog, key: &str, list: &AppList) -> Result<(), Error> {
    let apps: Vec<serde_json::Value> = list
        .apps
        .iter()
        .map(|(name, tokens, requests)| {
            serde_json::json!({"name": name, "tokens": tokens, "requests": requests})
        })
        .collect();
    catalog.set_meta(
        &format!("{key}_apps"),
        &serde_json::Value::Array(apps).to_string(),
    )?;
    catalog.set_meta(&format!("{key}_as_of"), list.as_of.as_deref().unwrap_or(""))?;
    Ok(())
}

/// Persist the task Code share (Iter 42): one fraction plus the
/// citation. `None` stores nothing — unknown stays unknown.
pub fn store_task_share(catalog: &Catalog, share: &TaskShare) -> Result<(), Error> {
    if let Some(code) = share.code_share {
        catalog.set_meta("task_code_share", &code.to_string())?;
        catalog.set_meta("task_code_as_of", share.as_of.as_deref().unwrap_or(""))?;
    }
    Ok(())
}

/// Stored activity by slug. Missing or unparseable (old artifacts,
/// keyless syncs) reads as empty; malformed entries drop
/// individually.
pub fn read_activity(catalog: &Catalog) -> HashMap<String, ModelActivity> {
    catalog
        .get_meta("activity_tokens")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(slug, v)| {
                    let level = v.get("level")?.as_f64()?;
                    let trend = v.get("trend")?.as_f64()?;
                    if !level.is_finite() || !trend.is_finite() || level < 0.0 || trend < 0.0 {
                        return None;
                    }
                    Some((slug, ModelActivity { level, trend }))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Stored auxiliary app names by list key (`trending`, `coding`).
/// Missing keys (old artifacts, keyless or failed syncs) yield
/// empties.
pub fn read_app_list(catalog: &Catalog, key: &str) -> Vec<String> {
    catalog
        .get_meta(&format!("{key}_apps"))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|a| a.get("name")?.as_str().map(str::to_string))
        .collect()
}

/// Stored task Code share. Missing or unparseable reads as `None`.
pub fn read_task_code_share(catalog: &Catalog) -> Option<f64> {
    catalog
        .get_meta("task_code_share")
        .ok()
        .flatten()
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|share| share.is_finite() && (0.0..=1.0).contains(share))
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
        let (span_end, span_start) = rankings_span();
        assert_eq!(span_end, end);
        assert!(span_start < start);
    }

    fn dated(date: &str, slug: &str, tokens: u64) -> serde_json::Value {
        json!({"date": date, "model_permaslug": slug, "total_tokens": tokens})
    }

    #[test]
    fn windows_split_on_present_dates() {
        let body = json!({"data": [
            dated("2026-11-01", "old", 10),
            dated("2026-11-10", "new", 10),
            dated("2026-11-11", "new", 10),
            {"model_permaslug": "dateless", "total_tokens": 999u64},
        ]});
        let (recent, prior) = split_ranking_windows(&body);
        // Only three present dates: all are "recent", nothing prior,
        // and the dateless row drops from both.
        assert_eq!(recent.len(), 3);
        assert!(prior.is_empty());
        let mut many = Vec::new();
        for day in 1..=10u32 {
            many.push(dated(&format!("2026-11-{day:02}"), "m", 1));
        }
        let body = json!({"data": many});
        let (recent, prior) = split_ranking_windows(&body);
        assert_eq!(recent.len(), 7);
        assert_eq!(prior.len(), 3);
    }

    #[test]
    fn activity_levels_and_trends() {
        let mut rows = Vec::new();
        // Rising: 10/day prior, 30/day recent.
        for day in 1..=3u32 {
            rows.push(dated(&format!("2026-10-{day:02}"), "rising", 10));
        }
        for day in 1..=7u32 {
            rows.push(dated(&format!("2026-11-{day:02}"), "rising", 30));
        }
        // Flat: 10/day throughout.
        for day in 1..=3u32 {
            rows.push(dated(&format!("2026-10-{day:02}"), "flat", 10));
        }
        for day in 1..=7u32 {
            rows.push(dated(&format!("2026-11-{day:02}"), "flat", 10));
        }
        // Falling: 30/day prior, 10/day recent.
        for day in 1..=3u32 {
            rows.push(dated(&format!("2026-10-{day:02}"), "falling", 30));
        }
        for day in 1..=7u32 {
            rows.push(dated(&format!("2026-11-{day:02}"), "falling", 10));
        }
        // New: recent only, no baseline.
        for day in 1..=7u32 {
            rows.push(dated(&format!("2026-11-{day:02}"), "new", 5));
        }
        rows.push(dated("2026-11-01", "other", 99999));
        let activity = aggregate_activity(&json!({"data": rows}));
        let get = |slug: &str| {
            activity
                .iter()
                .find(|(s, _)| s == slug)
                .map(|(_, a)| *a)
                .unwrap()
        };
        assert_eq!(get("rising").trend, 3.0);
        assert_eq!(get("flat").trend, 1.0);
        assert!((get("falling").trend - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(get("new").trend, 1.0);
        // Level is the recent share of the top slug (rising: 210).
        assert_eq!(get("rising").level, 1.0);
        assert!((get("flat").level - 70.0 / 210.0).abs() < 1e-9);
        assert_eq!(activity[0].0, "rising");
        assert!(!activity.iter().any(|(s, _)| s == "other"));
        // One window only: every trend neutral.
        let solo = json!({"data": [dated("2026-11-01", "solo", 42)]});
        assert_eq!(aggregate_activity(&solo)[0].1.trend, 1.0);
        assert!(aggregate_activity(&json!({})).is_empty());
    }

    #[test]
    fn task_code_share_counts_code_tokens() {
        let body = json!({"data": [
            {"task": "Code", "total_tokens": 30u64},
            {"category": "coding", "total_tokens": "10"},
            {"name": "Chat", "total_tokens": 60u64},
            {"task": "Code", "total_tokens": "junk"},
            {"total_tokens": 5u64},
        ]});
        assert_eq!(aggregate_task_code_share(&body), Some(0.4));
        assert_eq!(aggregate_task_code_share(&json!({})), None);
        assert_eq!(
            aggregate_task_code_share(&json!({"data": [{"task": "Code"}]})),
            None
        );
    }

    #[test]
    fn activity_app_list_and_task_share_round_trip() {
        let dir = std::env::temp_dir().join(format!("popular-42-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let catalog = Catalog::open(&dir.join("t.db")).unwrap();
        catalog.init().unwrap();
        assert!(read_activity(&catalog).is_empty());
        assert!(read_app_list(&catalog, "trending").is_empty());
        assert_eq!(read_task_code_share(&catalog), None);
        store_popularity(
            &catalog,
            &Popularity {
                models: Vec::new(),
                all_models: Vec::new(),
                apps: Vec::new(),
                activity: vec![(
                    "a/x".to_string(),
                    ModelActivity {
                        level: 0.5,
                        trend: 2.0,
                    },
                )],
                as_of: None,
            },
        )
        .unwrap();
        assert_eq!(
            read_activity(&catalog).get("a/x"),
            Some(&ModelActivity {
                level: 0.5,
                trend: 2.0
            })
        );
        store_app_list(
            &catalog,
            "trending",
            &AppList {
                apps: vec![("Hot".to_string(), 9, 2)],
                as_of: Some("t".to_string()),
            },
        )
        .unwrap();
        assert_eq!(read_app_list(&catalog, "trending"), vec!["Hot".to_string()]);
        store_task_share(
            &catalog,
            &TaskShare {
                code_share: Some(0.25),
                as_of: Some("t".to_string()),
            },
        )
        .unwrap();
        assert_eq!(read_task_code_share(&catalog), Some(0.25));
        // Malformed entries drop; out-of-range shares reject.
        catalog
            .set_meta(
                "activity_tokens",
                r#"{"ok": {"level": 1, "trend": 1}, "bad": 7}"#,
            )
            .unwrap();
        assert_eq!(read_activity(&catalog).len(), 1);
        catalog.set_meta("task_code_share", "7").unwrap();
        assert_eq!(read_task_code_share(&catalog), None);
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
                activity: vec![(
                    "a/x".to_string(),
                    ModelActivity {
                        level: 1.0,
                        trend: 1.5,
                    },
                )],
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
