//! Iter 33: the `minindex` suffix filter against a mock backend.
//!
//! Two catalog models with different AA coding indexes: the floor must
//! pick the eligible one, echo the floor, and fail closed on an
//! unusable or unsatisfiable suffix.

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use pennyrouter_catalog::{
    Catalog, EndpointIndex, EndpointRow, EndpointScore, ModelIndex, ModelRow,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

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

/// `acme/weak` (index 30, cheapest) and `acme/strong` (index 60).
fn build_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    for (model_id, name) in [("acme/weak", "Weak"), ("acme/strong", "Strong")] {
        catalog
            .insert_model(&ModelRow {
                model_id: model_id.to_string(),
                name: name.to_string(),
                source: "test".to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
            })
            .unwrap();
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: model_id.to_string(),
                provider_name: "Solo".to_string(),
                payload: "{}".to_string(),
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
                scored_at: None,
                score_model: None,
                score_prompt: None,
                price_override_tokens: None,
            })
            .unwrap();
    }
    let index = |price: f64| EndpointIndex {
        price_prompt: Some(price),
        price_completion: Some(price),
        price_cache_read: None,
        price_cache_write: None,
        precision_class: Some("fp16".to_string()),
        promo_discount: None,
        processed_at: "t".to_string(),
        supports_tools: None,
        supports_decisions: None,
        price_override_tokens: None,
    };
    let score = EndpointScore {
        quality_tier: Some("low".to_string()),
        quality_score: None,
        scored_at: "t".to_string(),
        score_model: None,
        score_prompt: None,
    };
    for (model_id, price) in [("acme/weak", 1.0), ("acme/strong", 5.0)] {
        let endpoint = &catalog.endpoints_for(model_id).unwrap()[0];
        catalog
            .update_endpoint_index(endpoint.id, &index(price))
            .unwrap();
        catalog.update_endpoint_score(endpoint.id, &score).unwrap();
    }
    for (model_id, coding) in [("acme/weak", 30), ("acme/strong", 60)] {
        catalog
            .update_model_index(
                model_id,
                &ModelIndex {
                    aa_intelligence_index: None,
                    aa_coding_index: Some(coding),
                    aa_agentic_index: None,
                    aa_cost_per_task: None,
                },
            )
            .unwrap();
    }
}

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-minindex-{test}-{}.db", std::process::id()))
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

async fn post_chat(addr: SocketAddr, model: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .header("authorization", "Bearer test")
        .header("content-type", "application/json")
        .body(format!(
            r#"{{"model":"{model}","messages":[{{"role":"user","content":"hi"}}],"stream":false}}"#
        ))
        .send()
        .await
        .unwrap()
}

async fn setup(test: &str) -> (SocketAddr, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let upstream = spawn_mock(seen.clone()).await;
    let db = scratch_db(test);
    build_db(&db);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).unwrap());
    (spawn_gateway(upstream, router).await, seen)
}

fn upstream_model(body: &[u8]) -> String {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap();
    json["model"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn minindex_picks_the_eligible_model_and_echoes_the_floor() {
    let (addr, seen) = setup("route").await;
    let response = post_chat(addr, "openrouter/pair/low/minindex/50").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-pennyrouter-minindex")
            .and_then(|v| v.to_str().ok()),
        Some("50")
    );
    let bodies = seen.lock().unwrap().bodies.clone();
    assert_eq!(upstream_model(&bodies[0]), "acme/strong");
}

#[tokio::test]
async fn minindex_invalid_suffix_is_a_400() {
    let (addr, _seen) = setup("invalid").await;
    let response = post_chat(addr, "openrouter/pair/low/minindex/abc").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_model");
}

#[tokio::test]
async fn minindex_unsatisfiable_floor_is_a_503() {
    let (addr, _seen) = setup("unsatisfiable").await;
    let response = post_chat(addr, "openrouter/pair/low/minindex/90").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
}

#[tokio::test]
async fn minindex_requests_are_counted() {
    let (addr, _seen) = setup("metrics").await;
    assert_eq!(
        post_chat(addr, "openrouter/pair/low/minindex/50")
            .await
            .status(),
        StatusCode::OK
    );
    let metrics = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains(
            r#"pennyrouter_minindex_requests_total{minindex="50",tier="low",result="ok"} 1"#
        ),
        "{metrics}"
    );
}
