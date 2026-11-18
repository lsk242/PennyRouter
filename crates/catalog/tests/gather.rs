//! Phase 1: fixture sync populates the DB and publishes a versioned artifact.
//!
//! Fixtures are trimmed real OpenRouter API shapes (`GET /models` plus one
//! `GET /models/{id}/endpoints` body per model) and contain no secrets.

mod common;

use common::synced_db;
use pennyrouter_catalog::{
    load_endpoints_dir, now_rfc3339, publish, run_fixture_sync, verify, Catalog,
};

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
fn manifest_counts_come_from_the_db_not_the_caller() {
    // B09 probe: caller-guessed counts must not reach the manifest —
    // inflated guesses would otherwise verify and display as truth.
    let (dir, db) = synced_db("artifact-counts");
    let out = dir.join("out");
    let (_, _, manifest) = publish(&db, &out, "openrouter", 99, 99).unwrap();
    assert_eq!((manifest.model_count, manifest.endpoint_count), (3, 4));
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
fn failed_resync_keeps_prior_endpoints() {
    // C05 probe: a re-sync that fails mid-model must leave the prior
    // rows intact (per-model transaction), not delete-then-abort.
    let (_dir, db) = synced_db("resync-atomic");
    let catalog = Catalog::open(&db).unwrap();
    assert_eq!(
        catalog
            .endpoints_for("openai/gpt-6.1-terra-pro")
            .unwrap()
            .len(),
        3
    );
    let mut endpoints =
        pennyrouter_catalog::load_endpoints_dir(&common::fixtures_dir().join("endpoints")).unwrap();
    let poisoned = endpoints
        .iter()
        .position(|(id, _)| id == "openai/gpt-6.1-terra-pro")
        .unwrap();
    endpoints[poisoned].1 = br#"{"data":{"endpoints":[{"pricing":{"prompt":"1"}}]}}"#.to_vec();
    let fetched_at = pennyrouter_catalog::now_rfc3339().unwrap();
    let err = run_fixture_sync(
        &catalog,
        common::MODELS,
        &endpoints,
        "openrouter",
        &fetched_at,
    );
    assert!(err.is_err());
    assert_eq!(
        catalog
            .endpoints_for("openai/gpt-6.1-terra-pro")
            .unwrap()
            .len(),
        3,
        "failed re-sync must not delete prior rows"
    );
    assert_eq!(catalog.counts().unwrap(), (3, 4));
}

#[test]
fn resync_prunes_upstream_removed_models() {
    // B11 probe: a model dropped upstream must vanish from the DB
    // (model row + endpoints), not linger with stale rankings.
    let (_dir, db) = synced_db("resync-prune");
    let catalog = Catalog::open(&db).unwrap();
    assert_eq!(catalog.counts().unwrap(), (3, 4));
    let mut root: serde_json::Value = serde_json::from_slice(common::MODELS).unwrap();
    let data = root.get_mut("data").unwrap().as_array_mut().unwrap();
    let before = data.len();
    data.retain(|m| m.get("id").and_then(serde_json::Value::as_str) != Some("tencent/hy-mt2-1.8b"));
    assert_eq!(data.len(), before - 1);
    let models = serde_json::to_vec(&root).unwrap();
    let mut endpoints = load_endpoints_dir(&common::fixtures_dir().join("endpoints")).unwrap();
    endpoints.retain(|(id, _)| id != "tencent/hy-mt2-1.8b");
    let fetched_at = now_rfc3339().unwrap();
    run_fixture_sync(&catalog, &models, &endpoints, "openrouter", &fetched_at).unwrap();
    assert!(catalog.get_model("tencent/hy-mt2-1.8b").unwrap().is_none());
    assert!(catalog
        .endpoints_for("tencent/hy-mt2-1.8b")
        .unwrap()
        .is_empty());
    assert_eq!(catalog.counts().unwrap(), (2, 3));
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

fn b15_models(ids: &[&str]) -> Vec<u8> {
    let data: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| serde_json::json!({"id": id, "name": id}))
        .collect();
    serde_json::to_vec(&serde_json::json!({"data": data})).unwrap()
}

fn b15_endpoints(id: Option<&str>) -> Vec<u8> {
    let mut data = serde_json::json!({"endpoints": [{"provider_name": "P"}]});
    if let Some(id) = id {
        data["id"] = serde_json::json!(id);
    }
    serde_json::to_vec(&serde_json::json!({"data": data})).unwrap()
}

#[test]
fn endpoints_file_under_payload_id_not_filename() {
    let dir = common::scratch("b15-wins");
    let catalog = Catalog::open(&dir.join("work.db")).unwrap();
    catalog.init().unwrap();
    // The filename mapping is lossy: "a____b.json" derives "a//b",
    // but the payload carries the true id "a__b".
    let models = b15_models(&["a__b"]);
    let endpoints = vec![("a//b".to_string(), b15_endpoints(Some("a__b")))];
    let summary = run_fixture_sync(&catalog, &models, &endpoints, "openrouter", "t").unwrap();
    assert_eq!(summary.endpoint_count, 1);
    assert!(summary.skipped.is_empty());
    assert_eq!(catalog.endpoints_for("a__b").unwrap().len(), 1);
    assert!(catalog.endpoints_for("a//b").unwrap().is_empty());
}

#[test]
fn endpoints_for_unlisted_model_are_skipped() {
    let dir = common::scratch("b15-skip");
    let catalog = Catalog::open(&dir.join("work.db")).unwrap();
    catalog.init().unwrap();
    let models = b15_models(&["listed/m"]);
    let endpoints = vec![
        ("ghost/m".to_string(), b15_endpoints(Some("ghost/m"))),
        // No payload id: filename fallback still files listed models.
        ("listed/m".to_string(), b15_endpoints(None)),
    ];
    let summary = run_fixture_sync(&catalog, &models, &endpoints, "openrouter", "t").unwrap();
    assert_eq!(summary.skipped, vec!["ghost/m".to_string()]);
    assert_eq!(summary.endpoint_count, 1);
    assert_eq!(catalog.counts().unwrap(), (1, 1));
}
