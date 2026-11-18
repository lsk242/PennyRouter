//! Phase 2b: recorded model outputs fill score columns, no live calls.

mod common;

use common::{fixtures_dir, synced_db};
use pennyrouter_catalog::{run_processing, run_scoring, Catalog, NullScorer, RecordedScorer};
use std::fs;

const STAMP: &str = "2026-02-03T04:05:06Z";

fn scored_db(test: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (dir, db) = synced_db(test);
    let catalog = Catalog::open(&db).unwrap();
    run_processing(&catalog, STAMP).unwrap();
    let score = RecordedScorer::load(&fixtures_dir().join("scores.json")).unwrap();
    let summary = run_scoring(&catalog, &score, STAMP).unwrap();
    assert_eq!(summary.scored, 4);
    assert_eq!(summary.matched, 3);
    drop(catalog);
    (dir, db)
}

#[test]
fn recorded_outputs_yield_expected_indexed_values() {
    let (_dir, db) = scored_db("values");
    let catalog = Catalog::open(&db).unwrap();

    // File order preserved (ORDER BY id): flex, standard, fast.
    let eps = catalog.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps.len(), 3);
    assert_eq!(eps[0].quality_tier.as_deref(), Some("standard"));
    assert_eq!(eps[0].quality_score, Some(0.72));
    assert_eq!(eps[0].scored_at.as_deref(), Some(STAMP));
    assert_eq!(eps[1].quality_tier.as_deref(), Some("high"));
    assert_eq!(eps[1].quality_score, Some(0.93));
    assert_eq!(eps[2].quality_tier.as_deref(), Some("low"));
    assert_eq!(eps[2].quality_score, Some(0.41));

    // No recording for tencent: stamp set, signals stay NULL.
    let tencent = &catalog.endpoints_for("tencent/hy-mt2-1.8b").unwrap()[0];
    assert_eq!(tencent.quality_tier, None);
    assert_eq!(tencent.quality_score, None);
    assert_eq!(tencent.scored_at.as_deref(), Some(STAMP));

    // Deterministic columns untouched by scoring.
    assert_eq!(eps[0].price_prompt, Some(0.0000005));
    assert_eq!(eps[0].processed_at.as_deref(), Some(STAMP));

    // Raw payloads preserved.
    let fixture: serde_json::Value = serde_json::from_slice(
        &fs::read(fixtures_dir().join("endpoints/openai__gpt-6.1-terra-pro.json")).unwrap(),
    )
    .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&eps[1].payload).unwrap();
    assert_eq!(stored, fixture["data"]["endpoints"][1]);

    // Unknown-model record ignored, not an error (matched == 3 above).
    assert_eq!(
        catalog.get_meta("scored_count").unwrap().as_deref(),
        Some("4")
    );
}

#[test]
fn scoring_is_idempotent_and_null_score_clears_signals() {
    let (_dir, db) = scored_db("idempotent");
    let catalog = Catalog::open(&db).unwrap();
    let summary = run_scoring(&catalog, &NullScorer, "2026-07-07T07:07:07Z").unwrap();
    assert_eq!(summary.scored, 4);
    assert_eq!(summary.matched, 0);
    let eps = catalog.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps[0].quality_tier, None);
    assert_eq!(eps[0].quality_score, None);
    assert_eq!(eps[0].scored_at.as_deref(), Some("2026-07-07T07:07:07Z"));
    // Deterministic values still intact.
    assert_eq!(eps[0].price_prompt, Some(0.0000005));
}

#[test]
fn invalid_recordings_fail_at_load() {
    let dir = common::scratch("score-invalid");
    let bad = [
        ("not-json", "nope"),
        ("no-array", r#"{"scores": {}}"#),
        (
            "bad-tier",
            r#"{"scores": [{"model_id": "m", "provider_name": "p",
                "quality_tier": "ultra"}]}"#,
        ),
        (
            "bad-score",
            r#"{"scores": [{"model_id": "m", "provider_name": "p",
                "quality_score": 2.0}]}"#,
        ),
    ];
    for (name, raw) in bad {
        let path = dir.join(format!("{name}.json"));
        fs::write(&path, raw).unwrap();
        assert!(RecordedScorer::load(&path).is_err(), "should reject {name}");
    }
}
