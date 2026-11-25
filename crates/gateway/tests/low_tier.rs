//! Iter 52: `openrouter/pair/low` (+ `privacy/`) is the cheapest
//! paid offering across the full pool — the retired `lowest` meaning
//! — minus providers the availability gate removes (known uptime
//! below 90%, known-zero throughput; unknown telemetry passes). No
//! quality floor, no bracket, no score path. The `lowest` tag and
//! fallback target fail closed naming the retirement.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use pennyrouter_catalog::{
    Catalog, EndpointIndex, EndpointRow, EndpointScore, EndpointSpeed, ModelIndex, ModelRow,
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const API_KEY: &str = "Bearer [REDACTED]";

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-low-{test}-{}.db", std::process::id()))
}

fn build_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    // (model, provider, price, uptime, throughput, intel)
    for (model, provider, price, uptime, throughput, intel) in [
        ("m/cheap-flaky", "Flaky", 1e-8, Some(45.0), None, None),
        ("m/stalled", "Stalled", 2e-8, None, Some(0.0), None),
        ("m/cheap", "Cheap", 5e-8, None, None, Some(20)),
        ("m/ok", "Ok", 1e-7, Some(99.5), Some(40.0), Some(50)),
        ("m/free", "Freebie", 0.0, None, None, Some(10)),
        ("m/trap:batch", "Batchy", 1e-9, None, None, None),
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
        // No quality tier anywhere: `low` has no floor to check.
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
        catalog
            .update_endpoint_speed(
                id,
                &EndpointSpeed {
                    latency_ms: None,
                    throughput_tps: throughput,
                    uptime_pct: uptime,
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

#[tokio::test]
async fn low_routes_cheapest_paid_skipping_flaky() {
    let db = scratch_db("route");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Flaky (uptime 45) and Stalled (0 tok/s) are cheaper but gated
    // out; Cheap (unknown telemetry, no tier at all) wins with no
    // floor to check. Free and :batch traps never serve.
    let res = post_chat(gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("Cheap").map(String::as_str),
        Some("m/cheap")
    );
}

#[tokio::test]
async fn low_mintel_privacy_and_fallback_target() {
    let db = scratch_db("variants");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Mintel floors the pool first: Cheap (20) drops, Ok (50) serves.
    let res = post_chat(gw, "openrouter/pair/low/mintel/40").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Ok");
    assert_eq!(
        res.headers()["x-pennyrouter-mintel"].to_str().unwrap(),
        "40"
    );

    // Privacy variant routes the same pick.
    let res = post_chat(gw, "openrouter/pair/privacy/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");

    // Low works as a fallback target: the mintel floor empties the
    // free leg (Freebie's index is 10), so the unfiltered low leg
    // serves.
    let res = post_chat(gw, "openrouter/pair/free/mintel/95/fallback/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
}

#[tokio::test]
async fn low_budget_and_tools_gates() {
    let db = scratch_db("gates");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // An impossible budget fails closed.
    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(
            r#"{"model":"openrouter/pair/low","messages":[],
                "provider":{"max_price":{"prompt":1e-9,"completion":1e-9}}}"#,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "budget_exceeded");
    // Tool-bearing requests starve: no offering is tool-capable.
    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(
            r#"{"model":"openrouter/pair/low","messages":[],
                "tools":[{"type":"function","function":{"name":"f"}}]}"#,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "tools_unsupported");
    assert!(seen.lock().unwrap().models.is_empty());
}

#[tokio::test]
async fn retired_lowest_tag_fails_closed() {
    let db = scratch_db("retired");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    for model in ["m/cheap/lowest", "m/cheap/lowest/mintel/40"] {
        let res = post_chat(gw, model).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{model}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["error"]["code"], "invalid_model");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("retired"),
            "retired notice, got: {}",
            body["error"]["message"]
        );
    }
    // The retired fallback target names the retirement too (the
    // mintel floor empties the free leg first).
    let res = post_chat(gw, "openrouter/pair/free/mintel/95/fallback/lowest").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("retired"),
        "retired notice, got: {}",
        body["error"]["message"]
    );
    assert!(seen.lock().unwrap().models.is_empty());
}

#[test]
fn low_preview_lists_cheapest_first_skipping_flaky() {
    let db = scratch_db("preview");
    build_db(&db);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");

    let preview = router.preview("openrouter/pair/low", None, 10);
    assert!(preview.rejection.is_none());
    let ids: Vec<&str> = preview
        .candidates
        .iter()
        .map(|c| c.model_id.as_str())
        .collect();
    assert_eq!(ids, vec!["m/cheap", "m/ok"]);
    let fresh = preview.fresh.expect("fresh pick");
    assert_eq!(fresh.model_id, "m/cheap");

    // The retired tag previews invalid, matching the request path.
    let retired = router.preview("m/cheap/lowest", None, 10);
    assert!(retired.fresh.is_none());
    assert!(retired.candidates.is_empty());
    let rejection = retired.rejection.expect("rejection");
    assert_eq!(rejection.code, "invalid_model");
    assert!(rejection.message.contains("retired"));
}
