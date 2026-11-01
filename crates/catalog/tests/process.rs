//! Phase 2a: deterministic processing fills indexed columns from payloads.

mod common;

use common::{fixtures_dir, synced_db};
use pennyrouter_catalog::{publish, run_processing, verify, Catalog};
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
    assert_eq!(eps[0].judged_at, None);
    assert_eq!(eps[0].judge_model, None);
    assert_eq!(eps[0].judge_prompt, None);
    assert_eq!(eps[0].supports_tools, None);
    assert_eq!(eps[0].supports_decisions, None);
    assert_eq!(
        catalog.get_meta("schema_version").unwrap().as_deref(),
        Some("5")
    );

    // Processing works on the migrated DB.
    run_processing(&catalog, PROCESSED_AT).unwrap();
    let eps = catalog.endpoints_for("m/id").unwrap();
    assert_eq!(eps[0].price_prompt, None); // '{}' has no pricing
    assert_eq!(eps[0].precision_class.as_deref(), Some("unknown"));
    assert_eq!(eps[0].processed_at.as_deref(), Some(PROCESSED_AT));
    assert_eq!(eps[0].supports_tools, None); // '{}' has no supported_parameters
    assert_eq!(eps[0].supports_decisions, None); // '{}' model payload has no architecture
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
        judged_at: None,
        judge_model: None,
        judge_prompt: None,
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
