//! Iter 49: `openrouter/pair/agent` (+ `privacy/`) serves the
//! cheapest paid offering across the usage top-20 pool (`:free`
//! variants excluded), no quality floor. No pool data means no
//! picks — it fails closed instead of silently serving the full
//! list.

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
    std::env::temp_dir().join(format!("gateway-agent-{test}-{}.db", std::process::id()))
}

fn build_db(path: &std::path::Path, pool: bool) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    // (model, provider, price, tier, score, intel)
    for (model, provider, price, tier, score, intel) in [
        ("m/pop-cheap", "Cheap", 5e-7, "low", 0.1, 60),
        ("m/pop-best", "Best", 1e-6, "high", 0.9, 90),
        ("m/obscure", "Sneaky", 1e-7, "standard", 0.5, 95),
        ("m/pop-free", "Freebie", 0.0, "low", 0.1, 60),
        ("m/pop-batch:batch", "Batchy", 1e-8, "standard", 0.5, 70),
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
                    quality_score: Some(score),
                    scored_at: "t".to_string(),
                    score_model: None,
                    score_prompt: None,
                },
            )
            .unwrap();
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
    if pool {
        catalog
            .set_meta(
                "agent_top20",
                r#"{"ruleset":"rank-v2","top_n":20,
                    "members":["m/pop-cheap","m/pop-best","m/pop-free","m/pop-batch:batch"]}"#,
            )
            .unwrap();
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
    reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(format!(r#"{{"model":"{model}","messages":[]}}"#))
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

#[test]
fn agent_ids_parse() {
    use pennyrouter_gateway::routing::{parse_virtual, tier_label, VirtualTier};
    let plain = parse_virtual("openrouter/pair/agent").expect("agent parses");
    assert_eq!(plain.tier, VirtualTier::Agent);
    assert!(!plain.privacy);
    let privacy = parse_virtual("openrouter/pair/privacy/agent").expect("privacy agent parses");
    assert_eq!(privacy.tier, VirtualTier::Agent);
    assert!(privacy.privacy);
    assert_eq!(tier_label("openrouter/pair/agent"), "agent");
    // No fallback form on the tier itself (fallback targets resolve
    // through decide_tier instead).
    assert!(parse_virtual("openrouter/pair/agent/fallback/low").is_none());
}

#[tokio::test]
async fn agent_routes_cheapest_in_pool() {
    let db = scratch_db("route");
    build_db(&db, true);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Sneaky is cheaper overall but sits outside the pool; Freebie
    // (free) and Batchy (:batch) sit inside it but are excluded.
    let res = post_chat(gw, "openrouter/pair/agent").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("Cheap").map(String::as_str),
        Some("m/pop-cheap")
    );
}

#[tokio::test]
async fn agent_without_pool_fails_closed() {
    let db = scratch_db("empty");
    build_db(&db, false);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    for model in ["openrouter/pair/agent", "openrouter/pair/privacy/agent"] {
        let res = post_chat(gw, model).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{model}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["error"]["code"], "no_eligible_offering");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no agent pool data"),
            "{model}"
        );
    }
    assert!(seen.lock().unwrap().models.is_empty());
}

#[tokio::test]
async fn agent_privacy_mintel_and_fallback_target() {
    let db = scratch_db("variants");
    build_db(&db, true);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Privacy variant routes the same pick.
    let res = post_chat(gw, "openrouter/pair/privacy/agent").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");

    // mintel floors the pool first: Cheap (60) drops, Best (90) wins.
    let res = post_chat(gw, "openrouter/pair/agent/mintel/80").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Best");
    assert_eq!(
        res.headers()["x-pennyrouter-mintel"].to_str().unwrap(),
        "80"
    );

    // Agent works as a fallback target: the mintel floor empties
    // the free leg (Freebie's index is 60), so the unfiltered agent
    // leg serves.
    let res = post_chat(gw, "openrouter/pair/free/mintel/95/fallback/agent").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
}

#[tokio::test]
async fn agent_preview_lists_pool_cheapest_first() {
    let db = scratch_db("preview");
    build_db(&db, true);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");

    let preview = router.preview("openrouter/pair/agent", None, 10);
    assert!(preview.rejection.is_none());
    let ids: Vec<&str> = preview
        .candidates
        .iter()
        .map(|c| c.model_id.as_str())
        .collect();
    assert_eq!(ids, vec!["m/pop-cheap", "m/pop-best"]);
    let fresh = preview.fresh.expect("fresh pick");
    assert_eq!(fresh.model_id, "m/pop-cheap");
    assert_eq!(fresh.provider, "Cheap");

    // Without pool data the preview names the missing pool.
    let db = scratch_db("preview-empty");
    build_db(&db, false);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    let preview = router.preview("openrouter/pair/agent", None, 10);
    let rejection = preview.rejection.expect("rejection");
    assert_eq!(rejection.code, "no_eligible_offering");
    assert!(rejection.message.contains("no agent pool data"));
}
