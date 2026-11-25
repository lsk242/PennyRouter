//! Iter 53: `openrouter/pair/think` (+ `privacy/`) serves the
//! highest intelligence index under the price caps ($0.20/M in,
//! $0.50/M out on list prices). Both caps and a known index are
//! hard gates — no floor, no bracket, no score path. Ties break
//! cheapest-first.

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

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-think-{test}-{}.db", std::process::id()))
}

fn build_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    // (model, provider, prompt $/token, completion $/token, intel)
    for (model, provider, prompt, completion, intel) in [
        ("m/smart", "Brain", 1.5e-7, 4e-7, Some(70)),
        ("m/smarter-over", "Over", 1e-7, 6e-7, Some(90)),
        ("m/smarter-pricey", "Pricey", 3e-7, 4e-7, Some(85)),
        ("m/mystery", "Myst", 1e-7, 4e-7, None),
        ("m/tie", "TieC", 1.8e-7, 4.5e-7, Some(70)),
        ("m/free", "Freebie", 0.0, 0.0, Some(95)),
        ("m/trap:batch", "Batchy", 1e-8, 1e-8, Some(99)),
    ] {
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
                    price_prompt: Some(prompt),
                    price_completion: Some(completion),
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
                    quality_tier: None,
                    quality_score: None,
                    scored_at: "t".to_string(),
                    score_model: None,
                    score_prompt: None,
                },
            )
            .unwrap();
        if let Some(intel) = intel {
            catalog
                .update_model_index(
                    model,
                    &ModelIndex {
                        aa_intelligence_index: Some(intel),
                        aa_coding_index: None,
                        aa_agentic_index: None,
                        aa_cost_per_task: None,
                    },
                )
                .unwrap();
        }
    }
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

#[derive(Default)]
struct Upstream {
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
            seen.models.insert(
                order.clone(),
                json["model"].as_str().unwrap_or("?").to_string(),
            );
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                format!(r#"{{"id":"ok","provider":"{order}"}}"#),
            )
        }
    };
    Router::new().route("/v1/chat/completions", post(handler))
}

async fn post_chat(gw: SocketAddr, model: &str) -> reqwest::Response {
    post_body(gw, &format!(r#"{{"model":"{model}","messages":[]}}"#)).await
}

async fn post_body(gw: SocketAddr, body: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
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
async fn think_routes_max_intel_under_caps() {
    let db = scratch_db("route");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Over (90) breaches the completion cap, Pricey (85) the prompt
    // cap, Myst has no index, Freebie is free, Batchy is :batch —
    // Brain (70) wins, TieC (70, pricier) loses the tie-break.
    let res = post_chat(gw, "openrouter/pair/think").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Brain");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("Brain").map(String::as_str),
        Some("m/smart")
    );
}

#[tokio::test]
async fn think_empty_pool_names_the_caps() {
    let db = scratch_db("empty");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Restricted to the over-cap model, nothing qualifies.
    let res = post_body(
        gw,
        r#"{"model":"openrouter/pair/think","messages":[],
            "provider":{"only":["Over"]}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("price caps"),
        "caps message, got: {}",
        body["error"]["message"]
    );
    assert!(seen.lock().unwrap().models.is_empty());
}

#[tokio::test]
async fn think_mintel_privacy_fallback_and_gates() {
    let db = scratch_db("variants");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Mintel floors the pool first: Brain/TieC (70) drop, and the
    // higher-indexed models sit over the caps — nothing serves.
    let res = post_chat(gw, "openrouter/pair/think/mintel/80").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Privacy variant routes the same pick.
    let res = post_chat(gw, "openrouter/pair/privacy/think").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Brain");

    // Think works as a fallback target: the mintel floor empties the
    // free leg (Freebie's index is 95 — above it, so floor at 96).
    let res = post_chat(gw, "openrouter/pair/free/mintel/96/fallback/think").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Brain");

    // Caller gates still apply: an impossible budget fails closed.
    let res = post_body(
        gw,
        r#"{"model":"openrouter/pair/think","messages":[],
            "provider":{"max_price":{"prompt":1e-9,"completion":1e-9}}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "budget_exceeded");
    // Tool-bearing requests starve: no offering is tool-capable.
    let res = post_body(
        gw,
        r#"{"model":"openrouter/pair/think","messages":[],
            "tools":[{"type":"function","function":{"name":"f"}}]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "tools_unsupported");
}

#[test]
fn think_preview_lists_best_intel_first() {
    let db = scratch_db("preview");
    build_db(&db);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");

    let preview = router.preview("openrouter/pair/think", None, 10);
    assert!(preview.rejection.is_none());
    let ids: Vec<&str> = preview
        .candidates
        .iter()
        .map(|c| c.model_id.as_str())
        .collect();
    assert_eq!(ids, vec!["m/smart", "m/tie"]);
    let fresh = preview.fresh.expect("fresh pick");
    assert_eq!(fresh.model_id, "m/smart");
    assert_eq!(fresh.provider, "Brain");
}
