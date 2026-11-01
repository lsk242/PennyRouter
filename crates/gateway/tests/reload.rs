//! Iter 10: catalog freshness — reload picks up DB changes without a
//! restart, and a corrupt DB keeps the last good snapshot serving.

use axum::http::HeaderMap;
use pennyrouter_catalog::{now_rfc3339, Catalog, EndpointIndex, EndpointRow, ModelRow};
use pennyrouter_gateway::{routing::Router, Metrics};
use std::{path::PathBuf, sync::Arc, time::Duration};

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gateway-reload-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn add_model(db: &std::path::Path, model_id: &str, provider: &str, prompt_price: f64) {
    let catalog = Catalog::open(db).unwrap();
    let fetched_at = now_rfc3339().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: model_id.to_string(),
            name: model_id.to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: fetched_at.clone(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: model_id.to_string(),
            provider_name: provider.to_string(),
            payload: "{}".to_string(),
            fetched_at,
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
        })
        .unwrap();
    let id = catalog.endpoints_for(model_id).unwrap()[0].id;
    catalog
        .update_endpoint_index(
            id,
            &EndpointIndex {
                price_prompt: Some(prompt_price),
                price_completion: Some(prompt_price * 2.0),
                price_cache_read: Some(0.0),
                price_cache_write: Some(0.0),
                precision_class: Some("fp16".to_string()),
                promo_discount: None,
                processed_at: now_rfc3339().unwrap(),
                supports_tools: None,
                supports_decisions: None,
            },
        )
        .unwrap();
}

fn seed_db(test: &str) -> (PathBuf, PathBuf) {
    let dir = scratch(test);
    let db = dir.join("catalog.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    drop(catalog);
    add_model(&db, "test/reload-a", "provider-a", 0.001);
    (dir, db)
}

fn routes(router: &Router, model: &str) -> Option<String> {
    let body = serde_json::json!({"model": model, "messages": []}).to_string();
    router
        .route_request(&HeaderMap::new(), body.as_bytes())
        .map(|(_, decision)| decision.provider)
}

#[test]
fn reload_picks_up_new_models_without_restart() {
    let (_dir, db) = seed_db("new-models");
    let router = Router::load(&db).unwrap();
    assert_eq!(
        routes(&router, "test/reload-a").as_deref(),
        Some("provider-a")
    );
    assert_eq!(routes(&router, "test/reload-b"), None);

    add_model(&db, "test/reload-b", "provider-b", 0.002);
    router.reload().unwrap();

    assert_eq!(
        routes(&router, "test/reload-a").as_deref(),
        Some("provider-a")
    );
    assert_eq!(
        routes(&router, "test/reload-b").as_deref(),
        Some("provider-b")
    );
}

/// F07: a newer-but-invalid manifest (bad hash) and a truncated
/// manifest file never poison discovery — the newest *verifying*
/// artifact loads.
#[test]
fn newest_verifying_manifest_wins_over_invalid_newer() {
    use pennyrouter_catalog::publish;

    let (dir, db) = seed_db("newest-valid");
    let (_db_path, _manifest_path, manifest) = publish(&db, &dir, "test", 1, 1).unwrap();

    // Newer created_at, but the hash does not match: skipped.
    let bad = serde_json::json!({
        "version": "ffffffffffffffff",
        "sha256": "00",
        "db_file": "catalog-ffffffffffffffff.db",
        "schema_version": manifest.schema_version,
        "source": "test",
        "model_count": 1,
        "endpoint_count": 1,
        "created_at": "2999-01-01T00:00:00Z",
    });
    std::fs::write(
        dir.join("catalog-ffffffffffffffff.manifest.json"),
        serde_json::to_string_pretty(&bad).unwrap(),
    )
    .unwrap();
    // Truncated file: unparseable, sorts without a timestamp, skipped.
    std::fs::write(
        dir.join("catalog-eeeeeeeeeeeeeeee.manifest.json"),
        "{truncated",
    )
    .unwrap();

    let router = Router::load(&dir).unwrap();
    assert_eq!(
        routes(&router, "test/reload-a").as_deref(),
        Some("provider-a"),
        "newest verifying artifact serves despite invalid newer files",
    );
}

#[test]
fn corrupt_db_keeps_last_good_snapshot() {
    let (_dir, db) = seed_db("last-good");
    let router = Router::load(&db).unwrap();
    assert_eq!(
        routes(&router, "test/reload-a").as_deref(),
        Some("provider-a")
    );

    let catalog = Catalog::open(&db).unwrap();
    catalog.set_meta("schema_version", "999").unwrap();
    drop(catalog);

    assert!(router.reload().is_err(), "bad schema must fail the reload");
    assert_eq!(
        routes(&router, "test/reload-a").as_deref(),
        Some("provider-a"),
        "last good snapshot keeps serving",
    );
}

#[tokio::test]
async fn background_task_publishes_new_snapshot() {
    let (_dir, db) = seed_db("background");
    let router = Arc::new(Router::load(&db).unwrap());
    let metrics = Arc::new(Metrics::new());
    let task = pennyrouter_gateway::spawn_reload_task(
        router.clone(),
        Duration::from_millis(50),
        metrics.clone(),
    );

    add_model(&db, "test/reload-live", "provider-live", 0.003);
    let mut seen = None;
    for _ in 0..60 {
        seen = routes(&router, "test/reload-live");
        if seen.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    task.abort();

    assert_eq!(
        seen.as_deref(),
        Some("provider-live"),
        "background reload published the new snapshot",
    );
    let rendered = metrics.render();
    let ok_count: u64 = rendered
        .lines()
        .find(|line| line.starts_with("pennyrouter_catalog_reload_total{result=\"ok\"}"))
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    assert!(
        ok_count >= 1,
        "at least one reload counted, got:\n{rendered}",
    );
}

/// Iter 23: a gateway booted against a missing catalog recovers
/// without a restart once the artifact lands — the standby router
/// passes through until the background reload heals it.
#[tokio::test]
async fn standby_router_heals_on_reload_without_restart() {
    use pennyrouter_catalog::publish;

    let dir = scratch("standby-heals");
    let router = Arc::new(Router::standby(&dir));
    assert!(!router.is_ready(), "empty snapshot is not ready");
    assert_eq!(
        routes(&router, "test/reload-a"),
        None,
        "standby passes through like a disabled router",
    );

    let metrics = Arc::new(Metrics::new());
    let task = pennyrouter_gateway::spawn_reload_task(
        router.clone(),
        Duration::from_millis(50),
        metrics.clone(),
    );

    // The catalog lands later (slow sync at boot): publish a manifest
    // the way catalog-sync does.
    let db = dir.join("catalog.db");
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    drop(catalog);
    add_model(&db, "test/reload-a", "provider-a", 0.001);
    publish(&db, &dir, "test", 1, 1).unwrap();

    let mut seen = None;
    for _ in 0..60 {
        seen = routes(&router, "test/reload-a");
        if seen.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    task.abort();

    assert_eq!(
        seen.as_deref(),
        Some("provider-a"),
        "background reload healed the standby router",
    );
    assert!(router.is_ready(), "healed router reports ready");
}
