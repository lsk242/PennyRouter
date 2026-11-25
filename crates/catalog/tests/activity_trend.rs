//! Iter 42: the rankings window grows to recent + priors in one
//! call; `aggregate_activity` returns per-slug level + trend, the
//! pins keep trailing-week meaning over the recent slice, and the
//! artifact round-trips the new meta keys. Fixture bodies only — no
//! test calls the network.

use pennyrouter_catalog::{
    aggregate_activity, aggregate_models_all, read_activity, split_ranking_windows,
    store_popularity, Catalog, Popularity,
};
use serde_json::{json, Value};

fn dated(date: &str, slug: &str, tokens: u64) -> Value {
    json!({"date": date, "model_permaslug": slug, "total_tokens": tokens})
}

/// Ten present days: Oct 28-31 prior, Nov 1-7 recent.
fn wide_body() -> Value {
    let mut rows = Vec::new();
    for day in 28..=31u32 {
        rows.push(dated(&format!("2026-10-{day:02}"), "steady", 100));
        rows.push(dated(&format!("2026-10-{day:02}"), "fader", 400));
    }
    for day in 1..=7u32 {
        rows.push(dated(&format!("2026-11-{day:02}"), "steady", 100));
        rows.push(dated(&format!("2026-11-{day:02}"), "riser", 300));
        rows.push(dated(&format!("2026-11-{day:02}"), "fader", 100));
    }
    rows.push(dated("2026-11-07", "other", 99999));
    rows.push(json!({"date": "2026-11-07", "model_permaslug": "junk", "total_tokens": "abc"}));
    json!({"data": rows})
}

#[test]
fn activity_computes_level_and_trend() {
    let activity = aggregate_activity(&wide_body());
    let get = |slug: &str| {
        activity
            .iter()
            .find(|(s, _)| s == slug)
            .map(|(_, a)| *a)
            .unwrap()
    };
    // Recent totals: riser 2100, steady 700, fader 700. Top is riser.
    assert_eq!(get("riser").level, 1.0);
    assert!((get("steady").level - 700.0 / 2100.0).abs() < 1e-9);
    // Trends: riser is new (no baseline, neutral), steady flat,
    // fader fell from 400/day to 100/day.
    assert_eq!(get("riser").trend, 1.0);
    assert_eq!(get("steady").trend, 1.0);
    assert!((get("fader").trend - 0.25).abs() < 1e-9);
    assert_eq!(activity[0].0, "riser");
    assert!(!activity.iter().any(|(s, _)| s == "other" || s == "junk"));
}

#[test]
fn single_window_trends_neutral() {
    let body = json!({"data": [dated("2026-11-01", "solo", 42)]});
    let activity = aggregate_activity(&body);
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].1.level, 1.0);
    assert_eq!(activity[0].1.trend, 1.0);
}

#[test]
fn pins_keep_trailing_week_meaning_on_the_wide_span() {
    let body = wide_body();
    // The pins slice the recent window back out of the wide span:
    // a prior-only slug never pins, however loud its past.
    let (recent, prior) = split_ranking_windows(&body);
    // 3 slugs x 7 days + the `other` and junk rows (dated, so they
    // land recent and drop at sum time).
    assert_eq!(recent.len(), 21 + 2);
    assert_eq!(prior.len(), 8);
    let pins = aggregate_models_all(&json!({"data": recent}));
    let slugs: Vec<&str> = pins.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(slugs, vec!["riser", "fader", "steady"]);
    // Full-span sums would rank fader first (2300 vs riser 2100):
    // slicing is what keeps the pins current.
    let full = aggregate_models_all(&body);
    assert_eq!(full[0].0, "fader");
}

#[test]
fn activity_store_read_round_trip() {
    let dir = std::env::temp_dir().join(format!("activity-trend-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let catalog = Catalog::open(&dir.join("t.db")).unwrap();
    catalog.init().unwrap();
    // Keyless syncs store nothing: reads come back empty.
    assert!(read_activity(&catalog).is_empty());
    store_popularity(
        &catalog,
        &Popularity {
            models: vec![("riser".to_string(), 2100)],
            all_models: vec![("riser".to_string(), 2100)],
            apps: Vec::new(),
            activity: aggregate_activity(&wide_body()),
            as_of: Some("2026-11-07T00:00:00Z".to_string()),
        },
    )
    .unwrap();
    let back = read_activity(&catalog);
    assert_eq!(back.len(), 3);
    assert_eq!(back["riser"].level, 1.0);
    assert!((back["fader"].trend - 0.25).abs() < 1e-9);
    // Every stored source carries its citation.
    assert_eq!(
        catalog.get_meta("popular_as_of").unwrap().as_deref(),
        Some("2026-11-07T00:00:00Z")
    );
}
