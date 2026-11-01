//! Iter 5: provider routing against a mock backend.
//!
//! The gateway loads a catalog DB built programmatically (no fixtures),
//! routes recognized models to a forced provider, and passes everything
//! else through untouched.

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use pennyrouter_catalog::{Catalog, EndpointIndex, EndpointJudgment, EndpointRow, ModelRow};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const MODEL: &str = "acme/widget-1";
const CHAT_COMPLETION: &[u8] = include_bytes!("fixtures/chat_completion.json");

#[derive(Default)]
struct Seen {
    bodies: Vec<Vec<u8>>,
}

async fn spawn_mock(seen: Arc<Mutex<Seen>>) -> SocketAddr {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |_headers: HeaderMap, req_body: Bytes| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().bodies.push(req_body.to_vec());
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    CHAT_COMPLETION.to_vec(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Test catalog: Alpha (standard, priced by caller), Beta (high, priced
/// by caller), Gamma (high, cheapest, but fp8 — never a silent
/// substitute for the default class).
fn build_db(path: &std::path::Path, alpha_price: f64, beta_price: f64) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: MODEL.to_string(),
            name: "Widget 1".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    let endpoint = |provider: &str, tag: &str| EndpointRow {
        id: 0,
        model_id: MODEL.to_string(),
        provider_name: provider.to_string(),
        payload: format!(r#"{{"tag": "{tag}"}}"#),
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
    };
    for row in [
        endpoint("Alpha", "alpha"),
        endpoint("Beta", "beta"),
        endpoint("Gamma", "gamma"),
    ] {
        catalog.insert_endpoint(&row).unwrap();
    }
    let index = |price: f64, precision: Option<&str>| EndpointIndex {
        price_prompt: Some(price),
        price_completion: Some(price),
        price_cache_read: None,
        price_cache_write: None,
        precision_class: precision.map(str::to_string),
        promo_discount: None,
        processed_at: "t".to_string(),
        supports_tools: None,
        supports_decisions: None,
    };
    let judgment = |tier: &str| EndpointJudgment {
        quality_tier: Some(tier.to_string()),
        quality_score: None,
        judged_at: "t".to_string(),
        judge_model: None,
        judge_prompt: None,
    };
    let eps = catalog.endpoints_for(MODEL).unwrap();
    assert_eq!(eps.len(), 3);
    catalog
        .update_endpoint_index(eps[0].id, &index(alpha_price, None))
        .unwrap();
    catalog
        .update_endpoint_judgment(eps[0].id, &judgment("standard"))
        .unwrap();
    catalog
        .update_endpoint_index(eps[1].id, &index(beta_price, None))
        .unwrap();
    catalog
        .update_endpoint_judgment(eps[1].id, &judgment("high"))
        .unwrap();
    catalog
        .update_endpoint_index(eps[2].id, &index(0.1, Some("fp8")))
        .unwrap();
    catalog
        .update_endpoint_judgment(eps[2].id, &judgment("high"))
        .unwrap();
}

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-routing-{test}-{}.db", std::process::id()))
}

async fn spawn_gateway(
    upstream: SocketAddr,
    router: Arc<pennyrouter_gateway::routing::Router>,
) -> SocketAddr {
    let state =
        pennyrouter_gateway::AppState::new(format!("http://{upstream}")).with_router(router);
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn post_chat(addr: SocketAddr, auth: &str, model: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .header("authorization", auth)
        .header("content-type", "application/json")
        .body(format!(
            r#"{{"model":"{model}","messages":[{{"role":"user","content":"hi"}}],"stream":false}}"#
        ))
        .send()
        .await
        .unwrap()
}

fn router_for(
    test: &str,
    alpha_price: f64,
    beta_price: f64,
) -> Arc<pennyrouter_gateway::routing::Router> {
    let db = scratch_db(test);
    build_db(&db, alpha_price, beta_price);
    Arc::new(pennyrouter_gateway::routing::Router::load(&db).unwrap())
}

fn upstream_model(body: &[u8]) -> (String, serde_json::Value) {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap();
    (
        json["model"].as_str().unwrap().to_string(),
        json["provider"].clone(),
    )
}

#[tokio::test]
async fn price_tag_forces_cheapest_provider() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(mock, router_for("price", 1.0, 5.0)).await;

    let res = post_chat(gw, "Bearer K1", &format!("{MODEL}/price")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "Alpha"
    );

    let seen = seen.lock().unwrap();
    assert_eq!(seen.bodies.len(), 1);
    let (model, provider) = upstream_model(&seen.bodies[0]);
    assert_eq!(model, MODEL); // suffix stripped before forwarding
    assert_eq!(provider["order"], serde_json::json!(["Alpha"]));
    assert_eq!(provider["allow_fallbacks"], serde_json::json!(false));
}

#[tokio::test]
async fn default_route_needs_no_tags_and_ignores_unknown_ones() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(mock, router_for("default", 1.0, 5.0)).await;

    // No tags: best tier (Beta/high) beats cheapest (Alpha/standard);
    // Gamma is cheaper still but a different precision class.
    let res = post_chat(gw, "Bearer K1", MODEL).await;
    assert_eq!(res.headers().get("x-pennyrouter-provider").unwrap(), "Beta");
    // Unknown tags are ignored, not an error.
    let res = post_chat(gw, "Bearer K2", &format!("{MODEL}/nonsense")).await;
    assert_eq!(res.headers().get("x-pennyrouter-provider").unwrap(), "Beta");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.bodies.len(), 2);
    for body in &seen.bodies {
        let (model, provider) = upstream_model(body);
        assert_eq!(model, MODEL);
        assert_eq!(provider["order"], serde_json::json!(["Beta"]));
    }
}

#[tokio::test]
async fn unknown_model_passes_through_untouched() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(mock, router_for("unknown", 1.0, 5.0)).await;

    let body =
        r#"{"model":"unknown/model","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", "Bearer K1")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("x-pennyrouter-provider").is_none());

    let seen = seen.lock().unwrap();
    assert_eq!(seen.bodies.len(), 1);
    assert_eq!(seen.bodies[0], body.as_bytes());
}

fn build_free_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    for (model, provider, price) in [("acme/paid", "Paid", 2.0), ("acme/freebie", "Zero", 0.0)] {
        catalog
            .insert_model(&ModelRow {
                model_id: model.to_string(),
                name: "g".to_string(),
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
                payload: format!(r#"{{"tag": "{provider}"}}"#),
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
        let ep = catalog.endpoints_for(model).unwrap().pop().unwrap();
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
                    supports_tools: None,
                    supports_decisions: None,
                },
            )
            .unwrap();
        catalog
            .update_endpoint_judgment(
                ep.id,
                &EndpointJudgment {
                    quality_tier: Some("standard".to_string()),
                    quality_score: None,
                    judged_at: "t".to_string(),
                    judge_model: None,
                    judge_prompt: None,
                },
            )
            .unwrap();
    }
}

#[tokio::test]
async fn free_model_routes_to_free_offering_and_rewrites_model() {
    let db = scratch_db("free");
    build_free_db(&db);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(mock, router).await;

    let res = post_chat(gw, "Bearer K1", "openrouter/free").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("x-pennyrouter-provider").unwrap(), "Zero");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.bodies.len(), 1);
    let (model, provider) = upstream_model(&seen.bodies[0]);
    assert_eq!(model, "acme/freebie"); // meta-model rewritten to backend id
    assert_eq!(provider["order"], serde_json::json!(["Zero"]));
}

#[tokio::test]
async fn free_model_passes_through_when_nothing_is_free() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let gw = spawn_gateway(mock, router_for("free-none", 1.0, 5.0)).await;

    let body =
        r#"{"model":"openrouter/free","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", "Bearer K1")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("x-pennyrouter-provider").is_none());

    let seen = seen.lock().unwrap();
    assert_eq!(seen.bodies.len(), 1);
    assert_eq!(seen.bodies[0], body.as_bytes());
}

#[tokio::test]
async fn session_stays_pinned_across_snapshot_swap() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(seen.clone()).await;
    let router = router_for("sticky", 1.0, 5.0);
    let gw = spawn_gateway(mock, router.clone()).await;

    let res = post_chat(gw, "Bearer K1", &format!("{MODEL}/price")).await;
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "Alpha"
    );

    // Reprice the world: Beta is now cheapest. Same session stays on
    // Alpha; a new session follows the fresh snapshot to Beta.
    let db2 = scratch_db("sticky-2");
    build_db(&db2, 10.0, 1.0);
    router.swap_snapshot(pennyrouter_gateway::routing::Router::load_snapshot(&db2).unwrap());

    let res = post_chat(gw, "Bearer K1", &format!("{MODEL}/price")).await;
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "Alpha"
    );
    let res = post_chat(gw, "Bearer K2", &format!("{MODEL}/price")).await;
    assert_eq!(res.headers().get("x-pennyrouter-provider").unwrap(), "Beta");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.bodies.len(), 3);
    let (_, p1) = upstream_model(&seen.bodies[1]);
    let (_, p2) = upstream_model(&seen.bodies[2]);
    assert_eq!(p1["order"], serde_json::json!(["Alpha"]));
    assert_eq!(p2["order"], serde_json::json!(["Beta"]));
}
