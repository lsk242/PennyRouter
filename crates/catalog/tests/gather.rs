//! Phase 1: fixture sync populates the DB and publishes a versioned artifact.
//!
//! Fixtures are trimmed real OpenRouter API shapes (`GET /models` plus one
//! `GET /models/{id}/endpoints` body per model) and contain no secrets.

mod common;

use common::synced_db;
use pennyrouter_catalog::{publish, run_fixture_sync, verify, Catalog};

#[test]
fn fixture_sync_populates_expected_rows() {
    let (_dir, db) = synced_db("rows");
    let catalog = Catalog::open(&db).unwrap();
    assert_eq!(catalog.counts().unwrap(), (3, 4));

    // Raw payload preserved: stored model JSON still carries pricing.
    let row = catalog
        .get_model("openai/gpt-6.1-terra-pro")
        .unwrap()
        .unwrap();
    assert_eq!(row.source, "openrouter");
    assert!(!row.fetched_at.is_empty());
    let payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap();
    assert_eq!(payload.get("id").unwrap(), "openai/gpt-6.1-terra-pro");
    assert!(payload.get("pricing").is_some());

    // Provider endpoints queryable per model.
    let eps = catalog.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps.len(), 3);
    assert!(eps.iter().any(|e| e.provider_name == "OpenAI"));

    // Model with no endpoints is still stored; unknown id is None.
    assert!(catalog
        .get_model("~openai/gpt-mini-latest")
        .unwrap()
        .is_some());
    assert!(catalog
        .endpoints_for("~openai/gpt-mini-latest")
        .unwrap()
        .is_empty());
    assert!(catalog.get_model("nope/not-a-model").unwrap().is_none());
}

#[test]
fn artifact_publish_verifies() {
    let (dir, db) = synced_db("artifact");
    let out = dir.join("out");
    let (db_path, manifest_path, manifest) = publish(&db, &out, "openrouter", 3, 4).unwrap();
    assert!(db_path.exists() && manifest_path.exists());
    assert_eq!(manifest.version.len(), 16);
    assert_eq!(
        manifest.db_file,
        db_path.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(manifest.schema_version, pennyrouter_catalog::SCHEMA_VERSION);
    assert!(verify(&manifest_path).unwrap());

    // Idempotent: same content republishes the same version.
    let (db2, _, manifest2) = publish(&db, &out, "openrouter", 3, 4).unwrap();
    assert_eq!(db2, db_path);
    assert_eq!(manifest2.version, manifest.version);
}

#[test]
fn resync_mirrors_source_without_duplicates() {
    let (_dir, db) = synced_db("resync");
    let catalog = Catalog::open(&db).unwrap();
    let endpoints =
        pennyrouter_catalog::load_endpoints_dir(&common::fixtures_dir().join("endpoints")).unwrap();
    let fetched_at = pennyrouter_catalog::now_rfc3339().unwrap();
    run_fixture_sync(
        &catalog,
        common::MODELS,
        &endpoints,
        "openrouter",
        &fetched_at,
    )
    .unwrap();
    assert_eq!(catalog.counts().unwrap(), (3, 4));
}

#[test]
fn sync_rejects_malformed_input() {
    let dir = common::scratch("malformed");
    let catalog = Catalog::open(&dir.join("work.db")).unwrap();
    catalog.init().unwrap();
    assert!(run_fixture_sync(&catalog, b"not json", &[], "openrouter", "t").is_err());
    assert!(run_fixture_sync(
        &catalog,
        br#"{"data": [{"name": "no-id"}]}"#,
        &[],
        "openrouter",
        "t"
    )
    .is_err());
    // Empty is valid: zero rows, still completes.
    let summary = run_fixture_sync(&catalog, br#"{"data": []}"#, &[], "openrouter", "t").unwrap();
    assert_eq!(summary.model_count, 0);
    assert_eq!(catalog.counts().unwrap(), (0, 0));
}
