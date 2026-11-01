//! Iter 28: pinned-model routes `openrouter/pair/model/<id>`.

use pennyrouter_catalog::{Catalog, EndpointIndex, EndpointRow, ModelRow};
use std::sync::Arc;

const PINNED: &str = "deepseek/deepseek-v4.1-flash";
const OTHER: &str = "m/other";

fn build_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let c = Catalog::open(path).unwrap();
    c.init().unwrap();
    for model in [PINNED, OTHER] {
        c.insert_model(&ModelRow {
            model_id: model.to_string(),
            name: model.to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    }
    // Pinned model: two providers, cheap vs dear.
    for (provider, price) in [("Cheap", 1.0), ("Dear", 9.0)] {
        c.insert_endpoint(&EndpointRow {
            id: 0,
            model_id: PINNED.to_string(),
            provider_name: provider.to_string(),
            payload: r#"{"tag":null}"#.to_string(),
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
            judged_at: None,
            judge_model: None,
            judge_prompt: None,
        })
        .unwrap();
        let id = c
            .endpoints_for(PINNED)
            .unwrap()
            .iter()
            .find(|e| e.provider_name == provider)
            .unwrap()
            .id;
        c.update_endpoint_index(
            id,
            &EndpointIndex {
                price_prompt: Some(price),
                price_completion: Some(price),
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
    // Other model: one provider, so fallback has something to pick.
    c.insert_endpoint(&EndpointRow {
        id: 0,
        model_id: OTHER.to_string(),
        provider_name: "FallbackProv".to_string(),
        payload: r#"{"tag":null}"#.to_string(),
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
        judged_at: None,
        judge_model: None,
        judge_prompt: None,
    })
    .unwrap();
    let id = c.endpoints_for(OTHER).unwrap()[0].id;
    c.update_endpoint_index(
        id,
        &EndpointIndex {
            price_prompt: Some(2.0),
            price_completion: Some(2.0),
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

fn scratch(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-pinned-{test}-{}.db", std::process::id()))
}

#[tokio::test]
async fn pinned_routes_to_cheapest_provider_for_model() {
    let db = scratch("route");
    build_db(&db);
    let body =
        serde_json::json!({"model": format!("openrouter/pair/model/{PINNED}"), "messages": []});
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).unwrap());
    let headers = axum::http::HeaderMap::new();
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let (rewritten, decision) = router
        .route_request_or_reject(&headers, &body_bytes)
        .unwrap()
        .unwrap();
    assert_eq!(decision.model_id, PINNED);
    assert_eq!(decision.provider, "Cheap");
    let v: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
    assert_eq!(v["model"], PINNED);
    assert_eq!(v["provider"]["order"], serde_json::json!(["Cheap"]));
}

#[tokio::test]
async fn pinned_unknown_model_fails_closed() {
    let db = scratch("unknown");
    build_db(&db);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).unwrap());
    let headers = axum::http::HeaderMap::new();
    let body = serde_json::json!({"model": "openrouter/pair/model/nope/missing", "messages": []});
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let err = router
        .route_request_or_reject(&headers, &body_bytes)
        .unwrap_err();
    assert_eq!(err.code, "no_eligible_offering");
}

#[tokio::test]
async fn pinned_privacy_enforces_zdr() {
    let db = scratch("privacy");
    build_db(&db);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).unwrap());
    let headers = axum::http::HeaderMap::new();
    let body = serde_json::json!({"model": format!("openrouter/pair/privacy/model/{PINNED}"), "messages": []});
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let (rewritten, _) = router
        .route_request_or_reject(&headers, &body_bytes)
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
    assert_eq!(v["provider"]["zdr"], true);
    assert_eq!(v["provider"]["data_collection"], "deny");
}

#[tokio::test]
async fn pinned_respects_tool_and_budget_filters() {
    let db = scratch("filters");
    build_db(&db);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).unwrap());
    let headers = axum::http::HeaderMap::new();
    // Tool-bearing with no tool-capable endpoints on this model -> 400.
    let body = serde_json::json!({
        "model": format!("openrouter/pair/model/{PINNED}"),
        "messages": [],
        "tools": [{"type": "function", "function": {"name": "patch"}}],
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let err = router
        .route_request_or_reject(&headers, &body_bytes)
        .unwrap_err();
    assert_eq!(err.code, "tools_unsupported");
    // Budget below cheapest -> budget_exceeded.
    let body = serde_json::json!({
        "model": format!("openrouter/pair/model/{PINNED}"),
        "messages": [],
        "provider": {"max_price": {"prompt": 0.0000001, "completion": 0.0000001}},
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let err = router
        .route_request_or_reject(&headers, &body_bytes)
        .unwrap_err();
    assert_eq!(err.code, "budget_exceeded");
}
