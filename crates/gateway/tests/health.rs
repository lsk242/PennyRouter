use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use pennyrouter_catalog::{Catalog, EndpointRow, ModelRow};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn health_returns_ok() {
    let res = pennyrouter_gateway::app()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    assert_eq!(body.as_ref(), br#"{"status":"ok"}"#);
}

/// Q07-A: no snapshot loaded → 503, so clients (and orchestrators)
/// stop discovering the failure at chat time.
#[tokio::test]
async fn ready_is_503_without_router() {
    let state = pennyrouter_gateway::AppState::new("http://127.0.0.1:1".to_string());
    let res = pennyrouter_gateway::app_with_state(state)
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    assert_eq!(body.as_ref(), br#"{"status":"not_ready"}"#);
}

/// Q07-A: a loaded snapshot → 200. `/health` stays liveness-only.
#[tokio::test]
async fn ready_is_200_with_snapshot() {
    let path = std::env::temp_dir().join(format!("gateway-ready-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let catalog = Catalog::open(&path).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "m/ready".to_string(),
            name: "m/ready".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "m/ready".to_string(),
            provider_name: "ReadyProvider".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(1.0),
            price_completion: Some(2.0),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: None,
            quantization: None,
            promo_discount: None,
            processed_at: Some("t".to_string()),
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
    drop(catalog);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&path).expect("router loads"));
    assert!(router.is_ready());
    let state =
        pennyrouter_gateway::AppState::new("http://127.0.0.1:1".to_string()).with_router(router);
    let res = pennyrouter_gateway::app_with_state(state)
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    assert_eq!(body.as_ref(), br#"{"status":"ready"}"#);
}
