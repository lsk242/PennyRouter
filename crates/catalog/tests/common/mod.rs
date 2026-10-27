//! Shared helpers for catalog integration tests.

use pennyrouter_catalog::{load_endpoints_dir, now_rfc3339, run_fixture_sync, Catalog};
use std::fs;
use std::path::PathBuf;

pub const MODELS: &[u8] = include_bytes!("../fixtures/models.json");

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

pub fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("catalog-test-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Sync the fixtures into a fresh DB; returns (scratch dir, db path).
pub fn synced_db(test: &str) -> (PathBuf, PathBuf) {
    let dir = scratch(test);
    let db = dir.join("work.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    let endpoints = load_endpoints_dir(&fixtures_dir().join("endpoints")).unwrap();
    assert_eq!(endpoints.len(), 3);
    let fetched_at = now_rfc3339().unwrap();
    let summary =
        run_fixture_sync(&catalog, MODELS, &endpoints, "openrouter", &fetched_at).unwrap();
    assert_eq!(summary.model_count, 3);
    assert_eq!(summary.endpoint_count, 4);
    drop(catalog);
    (dir, db)
}
