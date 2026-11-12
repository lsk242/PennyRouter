//! Iter 10: catalog freshness — reload picks up DB changes without a
//! restart, and a corrupt DB keeps the last good snapshot serving.

use axum::http::HeaderMap;
use pennyrouter_catalog::{now_rfc3339, Catalog, EndpointIndex, EndpointRow, ModelIndex, ModelRow};
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
            scored_at: None,
            score_model: None,
            score_prompt: None,
            price_override_tokens: None,
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
                price_override_tokens: None,
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

/// Iter 31: the snapshot carries the AA coding index per model
/// (display + future filters); the UI projection exposes it.
#[test]
fn snapshot_carries_aa_coding_index() {
    let (_dir, db) = seed_db("aa-coding");
    let catalog = Catalog::open(&db).unwrap();
    catalog
        .update_model_index(
            "test/reload-a",
            &ModelIndex {
                aa_intelligence_index: Some(51),
                aa_coding_index: Some(71),
                aa_agentic_index: None,
                aa_cost_per_task: None,
            },
        )
        .unwrap();
    drop(catalog);
    let router = Router::load(&db).unwrap();
    let (_, coding, _) = router
        .ui_summary()
        .models
        .into_iter()
        .find(|(id, _, _)| id == "test/reload-a")
        .unwrap();
    assert_eq!(coding, Some(71));
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

/// B18/Q12-A: an over-age ranking is rejected — at explicit load and
/// on reload — so month-old ordering never goes live; last good keeps
/// serving instead.
#[test]
fn stale_ranking_is_rejected_and_keeps_last_good() {
    use pennyrouter_catalog::{publish, publish_ranking, RankingDocument};
    use std::collections::BTreeMap;

    // Hermetic staleness bound: ignore any ambient SYNC_INTERVAL.
    let ambient = std::env::var("SYNC_INTERVAL").ok();
    std::env::remove_var("SYNC_INTERVAL");

    let doc = |at: &str| RankingDocument {
        created_at: at.to_string(),
        // Distinct content per stamp: identical docs share filenames
        // (I09 idempotent versioning), which would overwrite instead
        // of coexisting.
        catalog_version: Some(format!("ranked-at-{at}")),
        models: BTreeMap::new(),
        free_offers: BTreeMap::new(),
    };
    let (_seed_dir, db) = seed_db("stale-ranking");
    let out = scratch("stale-ranking-out");
    publish(&db, &out, "test", 1, 1).unwrap();

    let fresh_stamp = now_rfc3339().unwrap();
    let (_, fresh_manifest, _) = publish_ranking(&doc(&fresh_stamp), &out).unwrap();
    let router = Router::load(&out).unwrap();
    assert_eq!(
        routes(&router, "test/reload-a").as_deref(),
        Some("provider-a")
    );
    // Explicit load of a stale manifest fails too.
    let (_, stale_manifest, _) = publish_ranking(&doc("2020-01-01T00:00:00Z"), &out).unwrap();
    let err = match Router::load_with_ranking(&db, &stale_manifest) {
        Ok(_) => panic!("stale ranking must fail explicit load"),
        Err(err) => err,
    };
    assert!(
        err.0.contains("too old"),
        "stale load must name staleness, got: {}",
        err.0
    );

    // Reload with only the stale ranking left: rejected, last good serves.
    std::fs::remove_file(&fresh_manifest).unwrap();
    // A newer snapshot is waiting too: the failed reload must swap
    // nothing at all (Q05 load-before-swap), not even the good part.
    add_model(&db, "test/reload-b", "provider-b", 0.002);
    publish(&db, &out, "test", 2, 2).unwrap();
    assert!(router.reload().is_err(), "stale reload must fail");
    assert_eq!(
        routes(&router, "test/reload-a").as_deref(),
        Some("provider-a"),
        "last good keeps serving"
    );
    assert_eq!(
        routes(&router, "test/reload-b"),
        None,
        "failed reload swaps nothing, not even the fresh snapshot"
    );

    if let Some(value) = ambient {
        std::env::set_var("SYNC_INTERVAL", value);
    }
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
async fn zero_interval_disables_reload_without_panic() {
    // C08: `0 disables` must not panic inside `tokio::time::interval`.
    // The returned handle completes without ever ticking.
    let dir = scratch("zero-interval");
    let router = Arc::new(Router::standby(&dir));
    let metrics = Arc::new(Metrics::new());
    let task = pennyrouter_gateway::spawn_reload_task(router, Duration::ZERO, metrics.clone());
    let done = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("zero-interval handle must complete");
    assert!(done.is_ok(), "zero-interval task must not panic");
    let rendered = metrics.render();
    let ticks: u64 = rendered
        .lines()
        .filter(|line| line.starts_with("pennyrouter_catalog_reload_total{"))
        .filter_map(|line| line.rsplit(' ').next())
        .filter_map(|value| value.parse::<u64>().ok())
        .sum();
    assert_eq!(ticks, 0, "zero interval must never tick:\n{rendered}");
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
