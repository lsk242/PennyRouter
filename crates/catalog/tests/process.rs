//! Phase 2a: deterministic processing fills indexed columns from payloads.

mod common;

use common::{fixtures_dir, scratch, synced_db};
use pennyrouter_catalog::{publish, run_processing, verify, Catalog, EndpointRow, ModelRow};
use std::fs;

const PROCESSED_AT: &str = "2026-01-02T03:04:05Z";

fn processed_db(test: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (dir, db) = synced_db(test);
    let catalog = Catalog::open(&db).unwrap();
    let summary = run_processing(&catalog, PROCESSED_AT).unwrap();
    assert_eq!(summary.processed, 4);
    drop(catalog);
    (dir, db)
}

#[test]
fn processing_fills_expected_indexed_values() {
    let (_dir, db) = processed_db("values");
    let catalog = Catalog::open(&db).unwrap();

    // File order preserved (ORDER BY id).
    let eps = catalog.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps.len(), 3);
    assert_eq!(eps[0].price_prompt, Some(0.0000005));
    assert_eq!(eps[0].price_completion, Some(0.0000025));
    assert_eq!(eps[0].price_cache_read, Some(0.000000025));
    assert_eq!(eps[0].price_cache_write, Some(0.000000625));
    assert_eq!(eps[0].precision_class.as_deref(), Some("unknown"));
    assert_eq!(eps[0].promo_discount, Some(0.0));
    assert_eq!(eps[0].processed_at.as_deref(), Some(PROCESSED_AT));
    // Fixture endpoints list "tools" in supported_parameters.
    assert_eq!(eps[0].supports_tools, Some(1));
    // Second offering carries different prices.
    assert_eq!(eps[1].price_prompt, Some(0.000001));

    // fp8 quantization maps to the fp8 class; absent cache prices stay NULL.
    let tencent = &catalog.endpoints_for("tencent/hy-mt2-1.8b").unwrap()[0];
    assert_eq!(tencent.price_prompt, Some(0.000000044));
    assert_eq!(tencent.price_completion, Some(0.000000177));
    assert_eq!(tencent.price_cache_read, None);
    assert_eq!(tencent.price_cache_write, None);
    assert_eq!(tencent.precision_class.as_deref(), Some("fp8"));
    // Its supported_parameters omit "tools".
    assert_eq!(tencent.supports_tools, Some(0));

    // Raw payloads preserved: stored JSON still matches the fixture.
    let fixture: serde_json::Value = serde_json::from_slice(
        &fs::read(fixtures_dir().join("endpoints/openai__gpt-6.1-terra-pro.json")).unwrap(),
    )
    .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&eps[0].payload).unwrap();
    assert_eq!(stored, fixture["data"]["endpoints"][0]);
}

#[test]
fn out_of_range_discounts_extract_as_unknown() {
    // C03 probe: a bad/imported discount (>1, negative, NaN) must
    // extract as unknown, never store — storing 2.0 would clamp to
    // cost 0 and publish the endpoint as permanently free.
    let dir = scratch("bad-discount");
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "syn/disc".to_string(),
            name: "Disc".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    for (provider, discount) in [
        (
            "Over",
            r#"{"pricing":{"prompt":"0.001","completion":"0.002","discount":2.0}}"#,
        ),
        (
            "Under",
            r#"{"pricing":{"prompt":"0.001","completion":"0.002","discount":-0.5}}"#,
        ),
        (
            "Words",
            r#"{"pricing":{"prompt":"0.001","completion":"0.002","discount":"abc"}}"#,
        ),
    ] {
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: "syn/disc".to_string(),
                provider_name: provider.to_string(),
                payload: discount.to_string(),
                fetched_at: "t".to_string(),
                price_prompt: None,
                price_completion: None,
                price_cache_read: None,
                price_cache_write: None,
                precision_class: None,
                promo_discount: None,
                processed_at: None,
                supports_tools: None,
                supports_decisions: None,
                quality_tier: None,
                quality_score: None,
                scored_at: None,
                score_model: None,
                score_prompt: None,
                price_override_tokens: None,
            })
            .unwrap();
    }
    let summary = run_processing(&catalog, PROCESSED_AT).unwrap();
    assert_eq!(summary.processed, 3);
    for ep in catalog.endpoints_for("syn/disc").unwrap() {
        assert_eq!(
            ep.promo_discount, None,
            "{} stored {:?}",
            ep.provider_name, ep.promo_discount
        );
    }
}

#[test]
fn override_threshold_indexed_from_payload() {
    // Q13-C: the smallest min_prompt_tokens override lands in the
    // index so the gateway can skip the offering past it; endpoints
    // without overrides index None.
    let dir = scratch("override-threshold");
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "syn/ovr".to_string(),
            name: "Ovr".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    for (provider, payload) in [
        (
            "Tiered",
            r#"{"pricing":{"prompt":"0.001","completion":"0.002","overrides":[{"min_prompt_tokens":500000},{"min_prompt_tokens":272000}]}}"#,
        ),
        (
            "Flat",
            r#"{"pricing":{"prompt":"0.001","completion":"0.002"}}"#,
        ),
    ] {
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: "syn/ovr".to_string(),
                provider_name: provider.to_string(),
                payload: payload.to_string(),
                fetched_at: "t".to_string(),
                price_prompt: None,
                price_completion: None,
                price_cache_read: None,
                price_cache_write: None,
                precision_class: None,
                promo_discount: None,
                processed_at: None,
                supports_tools: None,
                supports_decisions: None,
                quality_tier: None,
                quality_score: None,
                scored_at: None,
                score_model: None,
                score_prompt: None,
                price_override_tokens: None,
            })
            .unwrap();
    }
    let summary = run_processing(&catalog, PROCESSED_AT).unwrap();
    assert_eq!(summary.processed, 2);
    let mut thresholds: Vec<(String, Option<i64>)> = catalog
        .endpoints_for("syn/ovr")
        .unwrap()
        .iter()
        .map(|ep| (ep.provider_name.clone(), ep.price_override_tokens))
        .collect();
    thresholds.sort();
    assert_eq!(
        thresholds,
        vec![
            ("Flat".to_string(), None),
            ("Tiered".to_string(), Some(272000)),
        ]
    );
}

#[test]
fn corrupt_model_payload_fails_loudly() {
    // B12 probe: a broken model lookup must error — never silently
    // mark every endpoint unknown — and the failed run writes nothing.
    let dir = scratch("bad-model");
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "syn/bad".to_string(),
            name: "Bad".to_string(),
            source: "test".to_string(),
            payload: "not json".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "syn/bad".to_string(),
            provider_name: "P".to_string(),
            payload: r#"{"pricing":{"prompt":"0.001","completion":"0.002"}}"#.to_string(),
            fetched_at: "t".to_string(),
            price_prompt: None,
            price_completion: None,
            price_cache_read: None,
            price_cache_write: None,
            precision_class: None,
            promo_discount: None,
            processed_at: None,
            supports_tools: None,
            supports_decisions: None,
            quality_tier: None,
            quality_score: None,
            scored_at: None,
            score_model: None,
            score_prompt: None,
            price_override_tokens: None,
        })
        .unwrap();
    assert!(run_processing(&catalog, PROCESSED_AT).is_err());
    let ep = &catalog.endpoints_for("syn/bad").unwrap()[0];
    assert_eq!(ep.processed_at, None);
    assert_eq!(ep.price_prompt, None);
}

#[test]
fn processing_is_idempotent() {
    let (_dir, db) = processed_db("idempotent");
    let catalog = Catalog::open(&db).unwrap();
    let summary = run_processing(&catalog, "2026-06-06T06:06:06Z").unwrap();
    assert_eq!(summary.processed, 4);
    assert_eq!(catalog.counts().unwrap(), (3, 4));
    let eps = catalog.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps[0].price_prompt, Some(0.0000005));
    assert_eq!(eps[0].processed_at.as_deref(), Some("2026-06-06T06:06:06Z"));
}

#[test]
fn migration_v1_to_current_preserves_data() {
    use rusqlite::{params, Connection};

    // Legacy v1 layout, as Phase 1 created it.
    let dir = common::scratch("migrate");
    let db = dir.join("v1.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(pennyrouter_catalog::V1_DDL).unwrap();
        conn.execute(
            "INSERT INTO models (model_id, name, source, payload, fetched_at)
             VALUES (?1, 'n', 'openrouter', '{}', 't')",
            params!["m/id"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO endpoints (model_id, provider_name, payload, fetched_at)
             VALUES (?1, 'p', '{}', 't')",
            params!["m/id"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_meta (key, value) VALUES ('schema_version', '1')",
            [],
        )
        .unwrap();
    }

    // Open migrates: indexed columns exist and start NULL, data intact.
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    let eps = catalog.endpoints_for("m/id").unwrap();
    assert_eq!(eps.len(), 1);
    assert_eq!(eps[0].price_prompt, None);
    assert_eq!(eps[0].precision_class, None);
    assert_eq!(eps[0].processed_at, None);
    assert_eq!(eps[0].quality_tier, None);
    assert_eq!(eps[0].quality_score, None);
    assert_eq!(eps[0].scored_at, None);
    assert_eq!(eps[0].score_model, None);
    assert_eq!(eps[0].score_prompt, None);
    assert_eq!(eps[0].supports_tools, None);
    assert_eq!(eps[0].supports_decisions, None);
    assert_eq!(eps[0].price_override_tokens, None);
    assert_eq!(
        catalog.get_meta("schema_version").unwrap().as_deref(),
        Some("8")
    );

    // Migrated models carry the new AA columns (NULL until reprocessed).
    let conn = rusqlite::Connection::open(&db).unwrap();
    let aa: (Option<i64>, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT aa_intelligence_index, aa_coding_index, aa_agentic_index
             FROM models WHERE model_id = 'm/id'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(aa, (None, None, None));

    // Processing works on the migrated DB.
    run_processing(&catalog, PROCESSED_AT).unwrap();
    let eps = catalog.endpoints_for("m/id").unwrap();
    assert_eq!(eps[0].price_prompt, None); // '{}' has no pricing
    assert_eq!(eps[0].precision_class.as_deref(), Some("unknown"));
    assert_eq!(eps[0].processed_at.as_deref(), Some(PROCESSED_AT));
    assert_eq!(eps[0].supports_tools, None); // '{}' has no supported_parameters
    assert_eq!(eps[0].supports_decisions, None); // '{}' model payload has no architecture
}

/// Iter 31: `run_processing` distills
/// `benchmarks.artificial_analysis.*` into the indexed `aa_*` model
/// columns; models without benchmarks stay NULL.
#[test]
fn processing_writes_aa_indexes_from_model_payloads() {
    let dir = common::scratch("aa-index");
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    let model = |id: &str, payload: &str| ModelRow {
        model_id: id.into(),
        name: id.into(),
        source: "openrouter".into(),
        payload: payload.into(),
        fetched_at: "t".into(),
    };
    catalog
        .insert_model(&model(
            "m/scored",
            r#"{"benchmarks":{"artificial_analysis":{
                "intelligence_index": 51, "coding_index": 71, "agentic_index": null}}}"#,
        ))
        .unwrap();
    catalog.insert_model(&model("m/bare", "{}")).unwrap();

    run_processing(&catalog, PROCESSED_AT).unwrap();

    let map = catalog.aa_coding_map().unwrap();
    assert_eq!(map.get("m/scored"), Some(&71));
    assert!(!map.contains_key("m/bare"));
    // Full triple incl. the null agentic leg, straight from the row.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let aa: (Option<i64>, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT aa_intelligence_index, aa_coding_index, aa_agentic_index
             FROM models WHERE model_id = 'm/scored'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(aa, (Some(51), Some(71), None));
}

#[test]
fn sync_process_publish_pipeline() {
    // The composition `--process` performs: published artifact verifies
    // and carries indexed values.
    let (dir, db) = synced_db("pipeline");
    let catalog = Catalog::open(&db).unwrap();
    run_processing(&catalog, PROCESSED_AT).unwrap();
    drop(catalog);
    let out = dir.join("out");
    let (db_path, manifest_path, _) = publish(&db, &out, "openrouter", 3, 4).unwrap();
    assert!(verify(&manifest_path).unwrap());
    let catalog = Catalog::open(&db_path).unwrap();
    catalog.init().unwrap();
    let eps = catalog.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps[0].price_prompt, Some(0.0000005));
    assert_eq!(eps[0].processed_at.as_deref(), Some(PROCESSED_AT));
}

/// Iter 24: decision-modality extraction reads the parent *model*
/// payload — `decisions` in `architecture.output_modalities` marks
/// the endpoint, other modalities clear it, missing data is unknown.
#[test]
fn processing_extracts_decision_support_from_model_payload() {
    use pennyrouter_catalog::{EndpointIndex, EndpointRow, ModelRow};

    let dir = common::scratch("decisions");
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    let model = |id: &str, payload: &str| ModelRow {
        model_id: id.into(),
        name: id.into(),
        source: "openrouter".into(),
        payload: payload.into(),
        fetched_at: "t".into(),
    };
    let endpoint = |id: &str| EndpointRow {
        id: 0,
        model_id: id.into(),
        provider_name: "p".into(),
        payload: "{}".into(),
        fetched_at: "t".into(),
        price_prompt: None,
        price_completion: None,
        price_cache_read: None,
        price_cache_write: None,
        precision_class: None,
        promo_discount: None,
        processed_at: None,
        supports_tools: None,
        supports_decisions: None,
        quality_tier: None,
        quality_score: None,
        scored_at: None,
        score_model: None,
        score_prompt: None,
        price_override_tokens: None,
    };
    catalog
        .insert_model(&model(
            "m/decider",
            r#"{"architecture":{"output_modalities":["text","decisions"]}}"#,
        ))
        .unwrap();
    catalog
        .insert_model(&model(
            "m/chatter",
            r#"{"architecture":{"output_modalities":["text"]}}"#,
        ))
        .unwrap();
    catalog.insert_model(&model("m/bare", "{}")).unwrap();
    for id in ["m/decider", "m/chatter", "m/bare"] {
        catalog.insert_endpoint(&endpoint(id)).unwrap();
    }
    // Fill NOT NULL price columns so nothing else interferes.
    for id in ["m/decider", "m/chatter", "m/bare"] {
        let row = catalog.endpoints_for(id).unwrap().remove(0);
        catalog
            .update_endpoint_index(
                row.id,
                &EndpointIndex {
                    price_prompt: Some(0.0),
                    price_completion: Some(0.0),
                    price_cache_read: None,
                    price_cache_write: None,
                    precision_class: Some("fp16".to_string()),
                    promo_discount: None,
                    processed_at: "t".to_string(),
                    supports_tools: None,
                    supports_decisions: None,
                    price_override_tokens: None,
                },
            )
            .unwrap();
    }

    run_processing(&catalog, PROCESSED_AT).unwrap();

    assert_eq!(
        catalog.endpoints_for("m/decider").unwrap()[0].supports_decisions,
        Some(1)
    );
    assert_eq!(
        catalog.endpoints_for("m/chatter").unwrap()[0].supports_decisions,
        Some(0)
    );
    assert_eq!(
        catalog.endpoints_for("m/bare").unwrap()[0].supports_decisions,
        None
    );
}

/// Iter 32: offline scoring fills tiers from the coding index —
/// high at 70+, standard at 40+, low below — with the normalized
/// index as the score. Models without an index stay untiered.
#[test]
fn processing_scores_offline_tiers_from_coding_index() {
    use pennyrouter_catalog::RULESET_VERSION;

    let dir = common::scratch("offline-tiers");
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    let model = |id: &str, payload: &str| ModelRow {
        model_id: id.into(),
        name: id.into(),
        source: "openrouter".into(),
        payload: payload.into(),
        fetched_at: "t".into(),
    };
    let endpoint = |id: &str| EndpointRow {
        id: 0,
        model_id: id.into(),
        provider_name: "p".into(),
        payload: "{}".into(),
        fetched_at: "t".into(),
        price_prompt: None,
        price_completion: None,
        price_cache_read: None,
        price_cache_write: None,
        precision_class: None,
        promo_discount: None,
        processed_at: None,
        supports_tools: None,
        supports_decisions: None,
        quality_tier: None,
        quality_score: None,
        scored_at: None,
        score_model: None,
        score_prompt: None,
        price_override_tokens: None,
    };
    let aa = |coding: i64| {
        format!(r#"{{"benchmarks":{{"artificial_analysis":{{"coding_index":{coding}}}}}}}"#)
    };
    for (id, coding) in [("m/hi", 85), ("m/std", 50), ("m/lo", 10)] {
        catalog.insert_model(&model(id, &aa(coding))).unwrap();
        catalog.insert_endpoint(&endpoint(id)).unwrap();
    }
    catalog.insert_model(&model("m/bare", "{}")).unwrap();
    catalog.insert_endpoint(&endpoint("m/bare")).unwrap();

    let summary = run_processing(&catalog, PROCESSED_AT).unwrap();
    assert_eq!(summary.processed, 4);
    assert_eq!(summary.scored, 3);

    let hi = &catalog.endpoints_for("m/hi").unwrap()[0];
    assert_eq!(hi.quality_tier.as_deref(), Some("high"));
    assert_eq!(hi.quality_score, Some(0.85));
    assert_eq!(hi.scored_at.as_deref(), Some(PROCESSED_AT));
    assert_eq!(hi.score_model.as_deref(), Some("offline"));
    assert_eq!(hi.score_prompt.as_deref(), Some(RULESET_VERSION));
    let std = &catalog.endpoints_for("m/std").unwrap()[0];
    assert_eq!(std.quality_tier.as_deref(), Some("standard"));
    assert_eq!(std.quality_score, Some(0.5));
    let lo = &catalog.endpoints_for("m/lo").unwrap()[0];
    assert_eq!(lo.quality_tier.as_deref(), Some("low"));
    assert_eq!(lo.quality_score, Some(0.1));
    let bare = &catalog.endpoints_for("m/bare").unwrap()[0];
    assert_eq!(bare.quality_tier, None);
    assert_eq!(bare.quality_score, None);
    assert_eq!(bare.scored_at, None);

    // Run-level audit meta mirrors the row stamps.
    assert_eq!(
        catalog.get_meta("scored_count").unwrap().as_deref(),
        Some("3")
    );
    assert_eq!(
        catalog.get_meta("score_model").unwrap().as_deref(),
        Some("offline")
    );
    assert_eq!(
        catalog.get_meta("score_prompt").unwrap().as_deref(),
        Some(RULESET_VERSION)
    );
}

/// Iter 32: an existing tier (e.g. an online verdict) always wins —
/// the offline pass only fills untiered rows.
#[test]
fn offline_scoring_never_overwrites_existing_tiers() {
    use pennyrouter_catalog::EndpointScore;

    let dir = common::scratch("offline-precedence");
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "m/coder".to_string(),
            name: "Coder".to_string(),
            source: "openrouter".to_string(),
            payload: r#"{"benchmarks":{"artificial_analysis":{"coding_index":90}}}"#.to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    for provider in ["p-online", "p-fresh"] {
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: "m/coder".to_string(),
                provider_name: provider.to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
                price_prompt: None,
                price_completion: None,
                price_cache_read: None,
                price_cache_write: None,
                precision_class: None,
                promo_discount: None,
                processed_at: None,
                supports_tools: None,
                supports_decisions: None,
                quality_tier: None,
                quality_score: None,
                scored_at: None,
                score_model: None,
                score_prompt: None,
                price_override_tokens: None,
            })
            .unwrap();
    }
    // An online verdict landed on one offering before processing.
    let online = catalog
        .endpoints_for("m/coder")
        .unwrap()
        .into_iter()
        .find(|ep| ep.provider_name == "p-online")
        .unwrap();
    catalog
        .update_endpoint_score(
            online.id,
            &EndpointScore {
                quality_tier: Some("low".to_string()),
                quality_score: Some(0.2),
                scored_at: "t0".to_string(),
                score_model: Some("llm-x".to_string()),
                score_prompt: Some("v9".to_string()),
            },
        )
        .unwrap();

    let summary = run_processing(&catalog, PROCESSED_AT).unwrap();
    assert_eq!(summary.scored, 1);

    let eps = catalog.endpoints_for("m/coder").unwrap();
    let online = eps
        .iter()
        .find(|ep| ep.provider_name == "p-online")
        .unwrap();
    assert_eq!(online.quality_tier.as_deref(), Some("low"));
    assert_eq!(online.quality_score, Some(0.2));
    assert_eq!(online.scored_at.as_deref(), Some("t0"));
    assert_eq!(online.score_model.as_deref(), Some("llm-x"));
    assert_eq!(online.score_prompt.as_deref(), Some("v9"));
    let fresh = eps.iter().find(|ep| ep.provider_name == "p-fresh").unwrap();
    assert_eq!(fresh.quality_tier.as_deref(), Some("high"));
    assert_eq!(fresh.quality_score, Some(0.9));
    assert_eq!(fresh.score_model.as_deref(), Some("offline"));
}

/// Iter 32: v6 databases migrate to v7 with verdicts intact — the
/// audit columns rename, legacy meta keys drop, version bumps.
#[test]
fn migration_v6_to_v7_renames_scoring_columns() {
    use rusqlite::{params, Connection};

    // v6 layout, as Iter 31 created it: indexed columns under their
    // v6 (judge-*) names plus a scored row and legacy meta.
    let dir = common::scratch("migrate-v6");
    let db = dir.join("v6.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(pennyrouter_catalog::V1_DDL).unwrap();
        for (name, ty) in [
            ("price_prompt", "REAL"),
            ("price_completion", "REAL"),
            ("price_cache_read", "REAL"),
            ("price_cache_write", "REAL"),
            ("precision_class", "TEXT"),
            ("promo_discount", "REAL"),
            ("processed_at", "TEXT"),
            ("supports_tools", "INTEGER"),
            ("supports_decisions", "INTEGER"),
            ("quality_tier", "TEXT"),
            ("quality_score", "REAL"),
            ("judged_at", "TEXT"),
            ("judge_model", "TEXT"),
            ("judge_prompt", "TEXT"),
            ("price_override_tokens", "INTEGER"),
        ] {
            conn.execute_batch(&format!("ALTER TABLE endpoints ADD COLUMN {name} {ty}"))
                .unwrap();
        }
        for (name, ty) in [
            ("aa_intelligence_index", "INTEGER"),
            ("aa_coding_index", "INTEGER"),
            ("aa_agentic_index", "INTEGER"),
        ] {
            conn.execute_batch(&format!("ALTER TABLE models ADD COLUMN {name} {ty}"))
                .unwrap();
        }
        conn.execute(
            "INSERT INTO models (model_id, name, source, payload, fetched_at)
             VALUES (?1, 'n', 'openrouter', '{}', 't')",
            params!["m/id"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO endpoints (model_id, provider_name, payload, fetched_at,
             quality_tier, quality_score, judged_at, judge_model, judge_prompt)
             VALUES (?1, 'p', '{}', 't', 'high', 0.9, 't0', 'm9', 'v1')",
            params!["m/id"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_meta (key, value) VALUES ('schema_version', '6'),
             ('judge_backend', 'baseten'), ('judged_count', '4')",
            [],
        )
        .unwrap();
    }

    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();

    // Verdicts carry over under the new names.
    let ep = &catalog.endpoints_for("m/id").unwrap()[0];
    assert_eq!(ep.quality_tier.as_deref(), Some("high"));
    assert_eq!(ep.quality_score, Some(0.9));
    assert_eq!(ep.scored_at.as_deref(), Some("t0"));
    assert_eq!(ep.score_model.as_deref(), Some("m9"));
    assert_eq!(ep.score_prompt.as_deref(), Some("v1"));

    // Old columns are gone, new ones present.
    let conn = Connection::open(&db).unwrap();
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(endpoints)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for old in ["judged_at", "judge_model", "judge_prompt"] {
        assert!(
            !cols.contains(&old.to_string()),
            "legacy column survived: {old}"
        );
    }
    for new in ["scored_at", "score_model", "score_prompt"] {
        assert!(
            cols.contains(&new.to_string()),
            "renamed column missing: {new}"
        );
    }

    // Legacy meta dropped, version bumped.
    assert_eq!(catalog.get_meta("judge_backend").unwrap(), None);
    assert_eq!(catalog.get_meta("judged_count").unwrap(), None);
    assert_eq!(
        catalog.get_meta("schema_version").unwrap().as_deref(),
        Some("8")
    );
}

/// Iter 34: processing writes the value score from the AA index, the
/// effective price, the precision class, and the popularity window.
#[test]
fn processing_writes_value_scores() {
    let dir = common::scratch("rank-score");
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();

    let model = |id: &str, payload: &str| ModelRow {
        model_id: id.into(),
        name: id.into(),
        source: "test".into(),
        payload: payload.into(),
        fetched_at: "t".into(),
    };
    catalog
        .insert_model(&model(
            "a/cheap",
            r#"{"id":"a/cheap","canonical_slug":"a/cheap-1",
                "benchmarks":{"artificial_analysis":{"coding_index":60,"intelligence_index":70}}}"#,
        ))
        .unwrap();
    catalog
        .insert_model(&model("b/none", r#"{"id":"b/none"}"#))
        .unwrap();

    let endpoint = |model_id: &str, provider: &str, payload: &str| EndpointRow {
        id: 0,
        model_id: model_id.into(),
        provider_name: provider.into(),
        payload: payload.into(),
        fetched_at: "t".into(),
        price_prompt: None,
        price_completion: None,
        price_cache_read: None,
        price_cache_write: None,
        precision_class: None,
        promo_discount: None,
        processed_at: None,
        supports_tools: None,
        supports_decisions: None,
        quality_tier: None,
        quality_score: None,
        scored_at: None,
        score_model: None,
        score_prompt: None,
        price_override_tokens: None,
    };
    // fp8, priced, with live speed metrics.
    catalog
        .insert_endpoint(&endpoint(
            "a/cheap",
            "P",
            r#"{"tag":"p/fp8","quantization":"fp8",
                "pricing":{"prompt":"0.000001","completion":"0.000005",
                           "input_cache_read":"0.00000005","input_cache_write":"0.00000125"},
                "latency_last_30m":250,"throughput_last_30m":120,"uptime_last_30m":99.5}"#,
        ))
        .unwrap();
    // Free: efficiency is 1, so the score is quality × popularity.
    catalog
        .insert_endpoint(&endpoint(
            "a/cheap",
            "Free",
            r#"{"tag":"p/free","quantization":"fp8",
                "pricing":{"prompt":"0","completion":"0"}}"#,
        ))
        .unwrap();
    // No price: no score, never a free pass.
    catalog
        .insert_endpoint(&endpoint("a/cheap", "Unpriced", r#"{"tag":"p/none"}"#))
        .unwrap();
    // No AA index: no score.
    catalog
        .insert_endpoint(&endpoint(
            "b/none",
            "P",
            r#"{"tag":"p/fp16","pricing":{"prompt":"0.000001","completion":"0.000001"}}"#,
        ))
        .unwrap();

    // Popularity window: a/cheap is the top model, b/none half of it.
    catalog
        .set_meta("popular_tokens", r#"{"a/cheap-1":100,"b/none":50}"#)
        .unwrap();

    let summary = run_processing(&catalog, PROCESSED_AT).unwrap();
    assert_eq!(summary.processed, 4);
    assert_eq!(summary.ranked, 2);

    let eps = catalog.endpoints_for("a/cheap").unwrap();
    let priced = &eps[0];
    // Speed/availability land in their own columns.
    let speeds = catalog.endpoint_speeds().unwrap();
    let speed = &speeds[&priced.id];
    assert_eq!(speed.latency_ms, Some(250.0));
    assert_eq!(speed.throughput_tps, Some(120.0));
    assert_eq!(speed.uptime_pct, Some(99.5));

    let ranks = catalog.endpoint_ranks().unwrap();
    let rank = &ranks[&priced.id];
    assert_eq!(rank.rank_basis.as_deref(), Some("coding"));
    assert_eq!(rank.rank_ruleset.as_deref(), Some("rank-v1"));
    assert_eq!(rank.ranked_at.as_deref(), Some(PROCESSED_AT));
    // quality = 60 × 0.97 (fp8)
    assert!((rank.rank_quality.unwrap() - 58.2).abs() < 1e-9);
    // cost/task = 0.5·1e-6 + 0.4·5e-6 + 0.05·5e-8 + 0.05·1.25e-6, ×10k
    let cost_task = 0.02565;
    let efficiency = 0.01 / (cost_task + 0.01);
    assert!((rank.rank_efficiency.unwrap() - efficiency).abs() < 1e-9);
    // Top model: share 1.0 → factor 1.0.
    assert!((rank.rank_popularity.unwrap() - 1.0).abs() < 1e-9);
    assert!((rank.rank_score.unwrap() - 58.2 * efficiency).abs() < 1e-6);

    // Free offering: full price credit.
    let free = &eps[1];
    let free_rank = &ranks[&free.id];
    assert_eq!(free_rank.rank_efficiency, Some(1.0));
    assert!((free_rank.rank_score.unwrap() - 58.2).abs() < 1e-9);

    // Unpriced offering: evaluated, but no score.
    let unpriced = &eps[2];
    let unpriced_rank = &ranks[&unpriced.id];
    assert_eq!(unpriced_rank.rank_score, None);
    assert_eq!(unpriced_rank.rank_ruleset.as_deref(), Some("rank-v1"));

    // No index: no score even though it is priced.
    let none = &catalog.endpoints_for("b/none").unwrap()[0];
    assert_eq!(ranks[&none.id].rank_score, None);

    // Popularity is stored per model, keyed by canonical_slug.
    let signals = catalog.model_signals().unwrap();
    assert_eq!(signals["a/cheap"].popularity_tokens, Some(100));
    assert_eq!(signals["a/cheap"].popularity_share, Some(1.0));
    assert_eq!(signals["b/none"].popularity_share, Some(0.5));
    assert_eq!(signals["a/cheap"].aa_coding_index, Some(60));

    // Meta records the pass.
    assert_eq!(
        catalog.get_meta("ranked_count").unwrap().as_deref(),
        Some("2")
    );
    assert_eq!(
        catalog.get_meta("rank_ruleset").unwrap().as_deref(),
        Some("rank-v1")
    );
}
