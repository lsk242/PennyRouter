//! Iter 25: `POST /v1/decisions` routes decisions bodies over
//! decisions-capable offerings and forwards to the upstream alpha
//! decisions endpoint — model rewritten, nothing else injected.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use pennyrouter_catalog::{Catalog, EndpointIndex, EndpointRow, ModelRow};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const API_KEY: &str = "Bearer [REDACTED]";

/// Upstream stand-in for the alpha decisions endpoint: records the
/// path and body model per provider-order hit, 429s providers in
/// `throttle`, echoes a decisions-shaped 200 otherwise.
#[derive(Default)]
struct Seen {
    paths: Vec<String>,
    models: Vec<String>,
    bodies: Vec<serde_json::Value>,
    hits: HashMap<String, u64>,
}

fn mock_app(seen: Arc<Mutex<Seen>>, throttle: Vec<&'static str>) -> Router {
    let handler = move |body: Bytes| {
        let seen = seen.clone();
        let throttle = throttle.clone();
        async move {
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let model = json["model"].as_str().unwrap_or("?").to_string();
            let mut seen = seen.lock().unwrap();
            seen.paths.push("/alpha/decisions".to_string());
            seen.models.push(model.clone());
            seen.bodies.push(json);
            *seen.hits.entry(model.clone()).or_insert(0) += 1;
            if throttle.iter().any(|t| model.contains(t)) {
                let mut headers = axum::http::HeaderMap::new();
                headers.insert("content-type", "application/json".parse().unwrap());
                headers.insert("x-ratelimit-limit", "50".parse().unwrap());
                headers.insert("x-ratelimit-remaining", "0".parse().unwrap());
                headers.insert("x-ratelimit-reset", "1793491200000".parse().unwrap());
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    headers,
                    r#"{"error":{"message":"slow down"}}"#.to_string(),
                );
            }
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            (
                StatusCode::OK,
                headers,
                format!(r#"{{"id":"dec-1","model":"{model}","provider":"P","answers":{{}}}}"#),
            )
        }
    };
    // The mock only serves the alpha path: anything else 404s, so a
    // wrong forward path fails the test loudly.
    Router::new().route("/alpha/decisions", post(handler.clone()))
}

async fn spawn_mock(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
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

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "gateway-decisions-{test}-{}.db",
        std::process::id()
    ))
}

/// `m/dfree`: NoDec (never capable) + DecCo (capable when
/// `with_capable`); `m/dpaid`: PaidDec (capable paid) when
/// `with_paid`.
fn build_db(path: &std::path::Path, with_capable: bool, with_paid: bool) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    let mut specs: Vec<(&str, &str, f64, bool)> = vec![
        ("m/dfree", "NoDec", 0.0, false),
        ("m/dfree", "DecCo", 0.0, with_capable),
    ];
    if with_paid {
        specs.push(("m/dpaid", "PaidDec", 1.0, true));
    }
    for (model, provider, price, decisions) in specs {
        catalog
            .insert_model(&ModelRow {
                model_id: model.to_string(),
                name: model.to_string(),
                source: "test".to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
            })
            .unwrap_or(());
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
        let id = catalog
            .endpoints_for(model)
            .unwrap()
            .iter()
            .find(|e| e.provider_name == provider)
            .unwrap()
            .id;
        catalog
            .update_endpoint_index(
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
                    supports_decisions: Some(i64::from(decisions)),
                },
            )
            .unwrap();
    }
}

fn decisions_body(model: &str) -> String {
    format!(
        r#"{{"model":"{model}","questions":{{"is_bug":{{"type":"noul","criteria":{{"true":"x"}},"instructions":"y"}}}},"state":{{"ticket":"z"}}}}"#
    )
}

async fn post_decisions(gw: SocketAddr, model: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{gw}/v1/decisions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(decisions_body(model))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn decision_free_hits_alpha_path_with_rewritten_model() {
    let db = scratch_db("route");
    build_db(&db, true, false);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_decisions(gw, "openrouter/pair/decision/free").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "DecCo"
    );
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["answers"], serde_json::json!({}));

    let guard = seen.lock().unwrap();
    // One upstream hit, on the alpha path, backend model id only.
    assert_eq!(guard.paths, vec!["/alpha/decisions".to_string()]);
    assert_eq!(guard.models, vec!["m/dfree".to_string()]);
    // Model-only rewrite: no chat-injected fields leak in.
    assert!(guard.bodies[0].get("provider").is_none());
    assert!(guard.bodies[0].get("questions").is_some());
}

#[tokio::test]
async fn decision_free_starved_fails_closed() {
    let db = scratch_db("closed");
    build_db(&db, false, false);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_decisions(gw, "openrouter/pair/decision/free").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
    assert!(seen.lock().unwrap().paths.is_empty());
}

#[tokio::test]
async fn unknown_model_passes_through_to_alpha_path() {
    let db = scratch_db("passthrough");
    build_db(&db, true, false);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_decisions(gw, "someone/else").await;
    assert_eq!(res.status(), StatusCode::OK);
    let guard = seen.lock().unwrap();
    assert_eq!(guard.paths, vec!["/alpha/decisions".to_string()]);
    assert_eq!(guard.models, vec!["someone/else".to_string()]);
    assert!(res.headers().get("x-pennyrouter-provider").is_none());
}

#[tokio::test]
async fn throttled_free_falls_back_to_paid_decision() {
    let db = scratch_db("fallback");
    build_db(&db, true, true);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec!["m/dfree"])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_decisions(gw, "openrouter/pair/decision/free/fallback/decision").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "PaidDec"
    );
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models,
        vec!["m/dfree".to_string(), "m/dpaid".to_string()]
    );
}

#[tokio::test]
async fn chat_tier_fallback_fails_closed_when_needed() {
    let db = scratch_db("wrong-fallback");
    build_db(&db, true, false);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec!["m/dfree"])).await;
    let gw = spawn_gateway(mock, &db).await;

    // Free leg 429s, retry finds nothing else, and the `low` target
    // serves chat bodies — failing closed beats 400ing upstream.
    let res = post_decisions(gw, "openrouter/pair/decision/free/fallback/low").await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_body");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("decision"),
        "rejection names the shape-safe target: {}",
        body["error"]["message"]
    );
}
