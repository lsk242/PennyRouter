//! Batch tier: `openrouter/pair/batch` routes to the cheapest
//! `:batch`-variant offering (async batch API pricing), ignoring
//! cheaper interactive offerings. Same chat path as the other tiers —
//! only the candidate filter differs.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use pennyrouter_catalog::{
    Catalog, EndpointIndex, EndpointRow, EndpointScore, ModelIndex, ModelRow,
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const API_KEY: &str = "Bearer [REDACTED]";

/// m/cheap (Cheap 1.0) is cheaper than any batch row but interactive,
/// so the batch tier must skip it; m/cheap:batch (CheapB 2.0) is the
/// batch pick; m/dear:batch (DearB 9.0) the expensive alternative.
fn build_db(path: &std::path::Path) {
    build_db_with(path, true);
}

fn build_db_without_batch(path: &std::path::Path) {
    build_db_with(path, false);
}

fn build_db_with(path: &std::path::Path, batch: bool) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    let mut rows = vec![
        ("m/cheap", "Cheap", 1.0, "low"),
        ("m/free", "Freebie", 0.0, "standard"),
    ];
    if batch {
        rows.push(("m/dear:batch", "DearB", 9.0, "high"));
        rows.push(("m/cheap:batch", "CheapB", 2.0, "low"));
    }
    for (model, provider, price, tier) in rows {
        catalog
            .insert_model(&ModelRow {
                model_id: model.to_string(),
                name: model.to_string(),
                source: "test".to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
            })
            .unwrap();
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: model.to_string(),
                provider_name: provider.to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
                price_prompt: None,
                price_completion: None,
                price_cache_read: None,
                price_cache_write: None,
                precision_class: None,
                quantization: None,
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
        let id = catalog.endpoints_for(model).unwrap()[0].id;
        catalog
            .update_endpoint_index(
                id,
                &EndpointIndex {
                    price_prompt: Some(price),
                    price_completion: Some(price),
                    price_cache_read: None,
                    price_cache_write: None,
                    precision_class: Some("fp16".to_string()),
                    quantization: None,
                    promo_discount: None,
                    processed_at: "t".to_string(),
                    supports_tools: None,
                    supports_decisions: None,
                    price_override_tokens: None,
                },
            )
            .unwrap();
        catalog
            .update_endpoint_score(
                id,
                &EndpointScore {
                    quality_tier: Some(tier.to_string()),
                    quality_score: None,
                    scored_at: "t".to_string(),
                    score_model: None,
                    score_prompt: None,
                },
            )
            .unwrap();
    }
}

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-batch-{test}-{}.db", std::process::id()))
}

async fn spawn_gateway(upstream: SocketAddr, db: &std::path::Path) -> SocketAddr {
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(db).expect("router loads"));
    let state =
        pennyrouter_gateway::AppState::new(format!("http://{upstream}")).with_router(router);
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn spawn_mock(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn post_chat(gw: SocketAddr, model: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(format!(r#"{{"model":"{model}","messages":[]}}"#))
        .send()
        .await
        .unwrap()
}

#[derive(Default)]
struct Upstream {
    hits: HashMap<String, u64>,
    providers: HashMap<String, serde_json::Value>,
    models: HashMap<String, String>,
}

fn mock_app(seen: Arc<Mutex<Upstream>>) -> Router {
    let handler = move |body: Bytes| {
        let seen = seen.clone();
        async move {
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let order = json["provider"]["order"][0]
                .as_str()
                .unwrap_or("?")
                .to_string();
            let mut seen = seen.lock().unwrap();
            *seen.hits.entry(order.clone()).or_insert(0) += 1;
            seen.providers
                .insert(order.clone(), json["provider"].clone());
            seen.models.insert(
                order.clone(),
                json["model"].as_str().unwrap_or("?").to_string(),
            );
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            (
                StatusCode::OK,
                headers,
                format!(r#"{{"id":"ok","provider":"{order}"}}"#),
            )
        }
    };
    Router::new().route("/v1/chat/completions", post(handler))
}

#[tokio::test]
async fn batch_tier_routes_to_cheapest_batch_offering() {
    let db = scratch_db("route");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "openrouter/pair/batch").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "CheapB"
    );
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("CheapB").map(String::as_str),
        Some("m/cheap:batch"),
        "upstream model rewritten to the batch backend id"
    );
    assert!(
        !guard.hits.contains_key("Cheap"),
        "cheaper interactive offering never serves batch: {:?}",
        guard.hits
    );
    assert!(
        !guard.hits.contains_key("Freebie"),
        "free offering never serves the paid-only batch tier: {:?}",
        guard.hits
    );
}

#[tokio::test]
async fn batch_privacy_variant_routes_same_pick_with_retention() {
    let db = scratch_db("privacy");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "openrouter/pair/privacy/batch").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "CheapB"
    );
    let guard = seen.lock().unwrap();
    let provider_obj = guard.providers.get("CheapB").cloned().unwrap();
    assert_eq!(provider_obj["zdr"], serde_json::json!(true));
    assert_eq!(provider_obj["data_collection"], serde_json::json!("deny"));
}

#[tokio::test]
async fn batch_without_batch_offerings_fails_closed() {
    let db = scratch_db("empty");
    build_db_without_batch(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "openrouter/pair/batch").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
    assert!(seen.lock().unwrap().hits.is_empty());
}

#[test]
fn batch_preview_lists_batch_candidates_only() {
    let db = scratch_db("preview");
    build_db(&db);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    let preview = router.preview("openrouter/pair/batch", None, 5);
    assert!(preview.rejection.is_none());
    let fresh = preview.fresh.expect("fresh pick");
    assert_eq!(fresh.provider, "CheapB");
    assert_eq!(fresh.model_id, "m/cheap:batch");
    assert_eq!(preview.candidates.len(), 2);
    assert!(
        preview
            .candidates
            .iter()
            .all(|c| c.model_id.ends_with(":batch")),
        "only batch variants listed: {:?}",
        preview
            .candidates
            .iter()
            .map(|c| &c.model_id)
            .collect::<Vec<_>>()
    );
    // Cheapest first.
    assert_eq!(preview.candidates[0].provider, "CheapB");
    assert_eq!(preview.candidates[1].provider, "DearB");
}

#[tokio::test]
async fn batch_minindex_floor_scopes_to_indexed_batch_models() {
    let db = scratch_db("minindex");
    build_db(&db);
    {
        let catalog = Catalog::open(&db).unwrap();
        for (model, index) in [("m/cheap:batch", 30), ("m/dear:batch", 60)] {
            catalog
                .update_model_index(
                    model,
                    &ModelIndex {
                        aa_intelligence_index: None,
                        aa_coding_index: Some(index),
                        aa_agentic_index: None,
                        aa_cost_per_task: None,
                    },
                )
                .unwrap();
        }
    }
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // The floor admits only the indexed-60 batch model.
    let res = post_chat(gw, "openrouter/pair/batch/minindex/50").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "DearB"
    );
    assert_eq!(
        res.headers()
            .get("x-pennyrouter-minindex")
            .and_then(|v| v.to_str().ok()),
        Some("50"),
        "floor echoed back"
    );

    // Nothing indexed that high: fail closed.
    let res = post_chat(gw, "openrouter/pair/batch/minindex/90").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
}

/// Trap fixture: the batch variants would win every interactive leg
/// if they were eligible — TrapB is the cheapest paid offering with
/// the best quality signals and decisions support, FreeB the best
/// free one — so each interactive pick below proves the exclusion.
fn build_db_sneaky(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    // (model, provider, price, tier, score, decisions)
    let rows = [
        ("m/cheap", "Cheap", 5.0, "low", 0.1, None),
        ("m/good", "Good", 6.0, "high", 0.9, None),
        ("m/free", "Freebie", 0.0, "standard", 0.4, None),
        ("m/thinker", "Thinker", 7.0, "high", 0.5, Some(1)),
        ("m/trap:batch", "TrapB", 1.0, "high", 0.99, Some(1)),
        ("m/freeloader:batch", "FreeB", 0.0, "high", 0.99, Some(1)),
    ];
    for (model, provider, price, tier, score, decisions) in rows {
        catalog
            .insert_model(&ModelRow {
                model_id: model.to_string(),
                name: model.to_string(),
                source: "test".to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
            })
            .unwrap();
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: model.to_string(),
                provider_name: provider.to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
                price_prompt: None,
                price_completion: None,
                price_cache_read: None,
                price_cache_write: None,
                precision_class: None,
                quantization: None,
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
        let id = catalog.endpoints_for(model).unwrap()[0].id;
        catalog
            .update_endpoint_index(
                id,
                &EndpointIndex {
                    price_prompt: Some(price),
                    price_completion: Some(price),
                    price_cache_read: None,
                    price_cache_write: None,
                    precision_class: Some("fp16".to_string()),
                    quantization: None,
                    promo_discount: None,
                    processed_at: "t".to_string(),
                    supports_tools: None,
                    supports_decisions: decisions,
                    price_override_tokens: None,
                },
            )
            .unwrap();
        catalog
            .update_endpoint_score(
                id,
                &EndpointScore {
                    quality_tier: Some(tier.to_string()),
                    quality_score: Some(score),
                    scored_at: "t".to_string(),
                    score_model: None,
                    score_prompt: None,
                },
            )
            .unwrap();
    }
}

fn provider_of(res: &reqwest::Response) -> String {
    res.headers()
        .get("x-pennyrouter-provider")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn interactive_tiers_never_route_to_batch_variants() {
    let db = scratch_db("sneaky-route");
    build_db_sneaky(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Each trap would win its leg if batch variants were eligible.
    for (model, provider) in [
        ("openrouter/pair/low", "Cheap"),
        ("openrouter/pair/think", "Good"),
        ("openrouter/pair/free", "Freebie"),
        ("openrouter/pair/decision", "Thinker"),
        ("m/cheap/lowest", "Cheap"),
    ] {
        let res = post_chat(gw, model).await;
        assert_eq!(res.status(), StatusCode::OK, "{model} routes");
        assert_eq!(provider_of(&res), provider, "{model} skips the batch trap");
    }
    // No decisions-capable interactive free offering exists (FreeB is
    // batch): fail closed instead of serving it.
    let res = post_chat(gw, "openrouter/pair/decision/free").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
    // The batch tier itself still serves the trap.
    let res = post_chat(gw, "openrouter/pair/batch").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "TrapB");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.hits.get("TrapB"),
        Some(&1),
        "TrapB serves the batch leg only: {:?}",
        guard.hits
    );
    assert!(
        !guard.hits.contains_key("FreeB"),
        "free batch variant never serves: {:?}",
        guard.hits
    );
    assert!(
        guard
            .models
            .values()
            .all(|model| !model.ends_with(":batch") || *model == "m/trap:batch"),
        "upstream rewrites stay interactive except the batch leg: {:?}",
        guard.models
    );
}

#[test]
fn interactive_previews_never_list_batch_variants() {
    let db = scratch_db("sneaky-preview");
    build_db_sneaky(&db);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    for model in [
        "openrouter/pair/low",
        "openrouter/pair/think",
        "openrouter/pair/free",
        "openrouter/pair/decision",
        "openrouter/pair/decision/free",
        "m/cheap/lowest",
    ] {
        let preview = router.preview(model, None, 10);
        assert!(
            preview
                .candidates
                .iter()
                .all(|c| !c.model_id.ends_with(":batch")),
            "{model} lists no batch variants: {:?}",
            preview
                .candidates
                .iter()
                .map(|c| &c.model_id)
                .collect::<Vec<_>>()
        );
    }
    let low = router.preview("openrouter/pair/low", None, 5);
    assert_eq!(low.fresh.as_ref().unwrap().provider, "Cheap");
    let think = router.preview("openrouter/pair/think", None, 5);
    assert_eq!(think.fresh.as_ref().unwrap().provider, "Good");
}
