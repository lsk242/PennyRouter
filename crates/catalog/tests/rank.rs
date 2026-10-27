//! Iter 6: ranking rebuilds from catalog + trends + recorded AI assist.

mod common;

use common::{fixtures_dir, synced_db};
use pennyrouter_catalog::{
    load_ranking_doc, publish_ranking, rank_catalog, run_judging, run_processing, verify_ranking,
    Assist, Catalog, EndpointIndex, EndpointJudgment, EndpointRow, ModelRow, Promos, RecordedJudge,
    Trends,
};

const STAMP: &str = "2026-03-04T05:06:07Z";

/// Fixture DB with deterministic + judged columns filled.
fn ranked_inputs(test: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (dir, db) = synced_db(test);
    let catalog = Catalog::open(&db).unwrap();
    run_processing(&catalog, STAMP).unwrap();
    let judge = RecordedJudge::load(&fixtures_dir().join("judgments.json")).unwrap();
    run_judging(&catalog, &judge, STAMP).unwrap();
    drop(catalog);
    (dir, db)
}

fn tags(entries: &[pennyrouter_catalog::RankEntry]) -> Vec<&str> {
    entries
        .iter()
        .map(|e| e.tag.as_deref().unwrap_or(""))
        .collect()
}

#[test]
fn fixture_rebuild_yields_expected_stable_ordering() {
    let (_dir, db) = ranked_inputs("ordering");
    let catalog = Catalog::open(&db).unwrap();
    let trends = Trends::load(&fixtures_dir().join("trends.json")).unwrap();
    let assist = Assist::load(&fixtures_dir().join("rank-assist.json")).unwrap();
    let promos = Promos::load(&fixtures_dir().join("promos.json")).unwrap();
    let doc = rank_catalog(
        &catalog,
        &trends,
        &assist,
        &promos,
        STAMP,
        STAMP,
        Some("cat-v1".to_string()),
    )
    .unwrap();

    assert_eq!(doc.models.len(), 2);
    // All OpenAI offerings share precision "unknown" and one provider, so
    // signals alone order them: tiers high/standard/low, prices asc.
    let openai = &doc.models["openai/gpt-6.1-terra-pro"];
    assert_eq!(openai.len(), 1);
    assert_eq!(openai[0].class.as_deref(), Some("unknown"));
    assert_eq!(
        tags(&openai[0].default),
        ["openai", "openai/flex", "openai/fast"]
    );
    assert_eq!(
        tags(&openai[0].price),
        ["openai/flex", "openai", "openai/fast"]
    );
    assert_eq!(
        tags(&openai[0].quality),
        ["openai", "openai/flex", "openai/fast"]
    );

    // Single fp8 offering: every orientation lists it alone.
    let tencent = &doc.models["tencent/hy-mt2-1.8b"];
    assert_eq!(tencent.len(), 1);
    assert_eq!(tencent[0].class.as_deref(), Some("fp8"));
    assert_eq!(tags(&tencent[0].default), ["tencent/fp8"]);
    assert_eq!(tags(&tencent[0].price), ["tencent/fp8"]);
}

#[test]
fn rebuild_is_deterministic_and_publish_idempotent() {
    let (_dir, db) = ranked_inputs("deterministic");
    let catalog = Catalog::open(&db).unwrap();
    let trends = Trends::load(&fixtures_dir().join("trends.json")).unwrap();
    let doc1 = rank_catalog(
        &catalog,
        &trends,
        &Assist::empty(),
        &Promos::empty(),
        STAMP,
        STAMP,
        None,
    )
    .unwrap();
    let doc2 = rank_catalog(
        &catalog,
        &trends,
        &Assist::empty(),
        &Promos::empty(),
        STAMP,
        STAMP,
        None,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&doc1).unwrap(),
        serde_json::to_string(&doc2).unwrap()
    );

    let out = common::scratch("rank-publish");
    let (_, m1, manifest1) = publish_ranking(&doc1, &out).unwrap();
    let (_, m2, manifest2) = publish_ranking(&doc2, &out).unwrap();
    assert_eq!(manifest1.version, manifest2.version);
    assert!(verify_ranking(&m1).unwrap());
    assert_eq!(m1, m2);
    let loaded = load_ranking_doc(&out.join(&manifest1.ranking_file)).unwrap();
    assert_eq!(loaded, doc1);
}

/// Synthetic two-provider model; trends/assist drive the ordering.
fn tie_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "syn/tie".to_string(),
            name: "Tie".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    for provider in ["Keep", "Drop"] {
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: "syn/tie".to_string(),
                provider_name: provider.to_string(),
                payload: format!(r#"{{"tag": "{provider}-tag"}}"#),
                fetched_at: "t".to_string(),
                price_prompt: None,
                price_completion: None,
                price_cache_read: None,
                price_cache_write: None,
                precision_class: None,
                promo_discount: None,
                processed_at: None,
                quality_tier: None,
                quality_score: None,
                judged_at: None,
            })
            .unwrap();
    }
    let index = |price: f64| EndpointIndex {
        price_prompt: Some(price),
        price_completion: None,
        price_cache_read: None,
        price_cache_write: None,
        precision_class: None,
        promo_discount: None,
        processed_at: "t".to_string(),
    };
    let judgment = EndpointJudgment {
        quality_tier: Some("standard".to_string()),
        quality_score: None,
        judged_at: "t".to_string(),
    };
    for ep in catalog.endpoints_for("syn/tie").unwrap() {
        catalog.update_endpoint_index(ep.id, &index(1.0)).unwrap();
        catalog.update_endpoint_judgment(ep.id, &judgment).unwrap();
    }
}

#[test]
fn trends_break_signal_ties() {
    let dir = common::scratch("rank-trends");
    let db = dir.join("tie.db");
    tie_db(&db);
    let catalog = Catalog::open(&db).unwrap();

    // Same signals; Drop's 90% error rate sinks it on every orientation.
    // Built via file to exercise the loader aggregation.
    let trends_path = dir.join("trends.json");
    std::fs::write(
        &trends_path,
        r#"{"trends": [
            {"app": "a", "model_id": "syn/tie", "provider_name": "Keep",
             "requests": 100, "errors": 0, "avg_latency_ms": 50.0},
            {"app": "a", "model_id": "syn/tie", "provider_name": "Drop",
             "requests": 100, "errors": 90, "avg_latency_ms": 50.0}
        ]}"#,
    )
    .unwrap();
    let trends = Trends::load(&trends_path).unwrap();
    assert_eq!(
        trends.get("syn/tie", "Drop").unwrap().error_rate(),
        Some(0.9)
    );
    let doc = rank_catalog(
        &catalog,
        &trends,
        &Assist::empty(),
        &Promos::empty(),
        STAMP,
        STAMP,
        None,
    )
    .unwrap();
    let class = &doc.models["syn/tie"][0];
    assert_eq!(class.class, None);
    for orientation in [&class.default, &class.price, &class.quality] {
        let providers: Vec<&str> = orientation.iter().map(|e| e.provider.as_str()).collect();
        assert_eq!(providers, ["Keep", "Drop"]);
    }
}

#[test]
fn promo_active_leads_and_expired_falls_back() {
    let dir = common::scratch("rank-promos");
    let db = dir.join("promo.db");
    let _ = std::fs::remove_file(&db);
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "syn/promo".to_string(),
            name: "Promo".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    for (provider, price) in [("Full", 1.0), ("Promo", 4.0)] {
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: "syn/promo".to_string(),
                provider_name: provider.to_string(),
                payload: format!(r#"{{"tag": "{provider}"}}"#),
                fetched_at: "t".to_string(),
                price_prompt: None,
                price_completion: None,
                price_cache_read: None,
                price_cache_write: None,
                precision_class: None,
                promo_discount: None,
                processed_at: None,
                quality_tier: None,
                quality_score: None,
                judged_at: None,
            })
            .unwrap();
        let ep = catalog.endpoints_for("syn/promo").unwrap().pop().unwrap();
        catalog
            .update_endpoint_index(
                ep.id,
                &EndpointIndex {
                    price_prompt: Some(price),
                    price_completion: Some(price),
                    price_cache_read: None,
                    price_cache_write: None,
                    precision_class: None,
                    promo_discount: None,
                    processed_at: "t".to_string(),
                },
            )
            .unwrap();
    }
    let promos_path = dir.join("promos.json");
    std::fs::write(
        &promos_path,
        r#"{"promos": [
            {"model_id": "syn/promo", "provider_name": "Promo",
             "discount": 0.9, "starts_at": "2026-01-01T00:00:00Z",
             "ends_at": "2026-02-01T00:00:00Z"}
        ]}"#,
    )
    .unwrap();
    let promos = Promos::load(&promos_path).unwrap();
    let leaders = |now: &str| {
        let doc = rank_catalog(
            &catalog,
            &Trends::empty(),
            &Assist::empty(),
            &promos,
            now,
            STAMP,
            None,
        )
        .unwrap();
        doc.models["syn/promo"][0]
            .price
            .iter()
            .map(|e| e.provider.clone())
            .collect::<Vec<_>>()
    };
    // Active window: Promo at 10% of 4.0 beats Full at 1.0.
    assert_eq!(leaders("2026-01-15T00:00:00Z"), ["Promo", "Full"]);
    // Expired (and before start): Full leads again — no misrouting.
    assert_eq!(leaders("2026-03-01T00:00:00Z"), ["Full", "Promo"]);
    assert_eq!(leaders("2025-12-01T00:00:00Z"), ["Full", "Promo"]);
}

#[test]
fn assist_prefer_and_avoid_override_signals() {
    let dir = common::scratch("rank-assist");
    let db = dir.join("tie.db");
    tie_db(&db);
    // Reprieve Drop on price: without assist it would lead on price.
    let catalog = Catalog::open(&db).unwrap();
    let eps = catalog.endpoints_for("syn/tie").unwrap();
    let cheap = EndpointIndex {
        price_prompt: Some(0.5),
        price_completion: None,
        price_cache_read: None,
        price_cache_write: None,
        precision_class: None,
        promo_discount: None,
        processed_at: "t".to_string(),
    };
    catalog.update_endpoint_index(eps[1].id, &cheap).unwrap();

    let assist_path = dir.join("assist.json");
    std::fs::write(
        &assist_path,
        r#"{"adjustments": [
            {"model_id": "syn/tie", "provider_name": "Keep",
             "action": "prefer", "reason": "test prefer"},
            {"model_id": "syn/tie", "provider_name": "Drop",
             "action": "avoid", "reason": "test avoid"}
        ]}"#,
    )
    .unwrap();
    let assist = Assist::load(&assist_path).unwrap();
    let doc = rank_catalog(
        &catalog,
        &Trends::empty(),
        &assist,
        &Promos::empty(),
        STAMP,
        STAMP,
        None,
    )
    .unwrap();
    let class = &doc.models["syn/tie"][0];
    // Keep leads even the price ordering despite Drop being cheaper.
    for orientation in [&class.default, &class.price, &class.quality] {
        let providers: Vec<&str> = orientation.iter().map(|e| e.provider.as_str()).collect();
        assert_eq!(providers, ["Keep", "Drop"]);
    }
    // Without assist, price leads with Drop.
    let plain = rank_catalog(
        &catalog,
        &Trends::empty(),
        &Assist::empty(),
        &Promos::empty(),
        STAMP,
        STAMP,
        None,
    )
    .unwrap();
    let providers: Vec<&str> = plain.models["syn/tie"][0]
        .price
        .iter()
        .map(|e| e.provider.as_str())
        .collect();
    assert_eq!(providers, ["Drop", "Keep"]);
}
