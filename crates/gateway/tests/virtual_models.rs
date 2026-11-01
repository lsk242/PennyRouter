//! Iter 13: virtual model catalog (`openrouter/pair/...`).
//!
//! The gateway serves a curated six-model menu, routes each tier to a
//! real backend offering, enforces privacy fields on the wire, retries
//! virtual requests once on another provider, and steers later picks
//! away from recently rate-limited providers.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use pennyrouter_catalog::{Catalog, EndpointIndex, EndpointJudgment, EndpointRow, ModelRow};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const API_KEY: &str = "Bearer [REDACTED]";

/// Models: m/cheap (Cheap 1.0/low), m/dear (Dear 9.0/high),
/// m/free (Freebie 0.0/standard). All fp16.
fn build_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    for (model, provider, price, tier) in [
        ("m/cheap", "Cheap", 1.0, "low"),
        ("m/dear", "Dear", 9.0, "high"),
        ("m/free", "Freebie", 0.0, "standard"),
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
                    promo_discount: None,
                    processed_at: "t".to_string(),
                    supports_tools: None,
                    supports_decisions: None,
                },
            )
            .unwrap();
        catalog
            .update_endpoint_judgment(
                id,
                &EndpointJudgment {
                    quality_tier: Some(tier.to_string()),
                    quality_score: None,
                    judged_at: "t".to_string(),
                    judge_model: None,
                    judge_prompt: None,
                },
            )
            .unwrap();
    }
}

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-virtual-{test}-{}.db", std::process::id()))
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

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn post_chat(gw: SocketAddr, model: &str) -> reqwest::Response {
    client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(format!(r#"{{"model":"{model}","messages":[]}}"#))
        .send()
        .await
        .unwrap()
}

/// Upstream: 200 with the chosen provider echoed, 429 for providers in
/// `throttle`, 503 for providers in `fail_once` (first hit only).
/// Records per-provider hits and last-seen provider objects.
#[derive(Default)]
struct Upstream {
    hits: HashMap<String, u64>,
    providers: HashMap<String, serde_json::Value>,
    models: HashMap<String, String>,
}

fn mock_app(
    seen: Arc<Mutex<Upstream>>,
    throttle: Vec<&'static str>,
    fail_once: Vec<&'static str>,
) -> Router {
    // Same upstream behavior on both routed endpoints (Iter 14).
    let handler = move |body: Bytes| {
        let seen = seen.clone();
        let throttle = throttle.clone();
        let fail_once = fail_once.clone();
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
            let hit = seen.hits[&order];
            if throttle.contains(&order.as_str()) {
                let mut headers = axum::http::HeaderMap::new();
                headers.insert("content-type", "application/json".parse().unwrap());
                headers.insert("x-ratelimit-limit", "50".parse().unwrap());
                headers.insert("x-ratelimit-remaining", "0".parse().unwrap());
                headers.insert("x-ratelimit-reset", "1793491200000".parse().unwrap());
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    headers,
                    r#"{"error":{"message":"slow down","type":"server_error","code":"rate_limited"}}"#.to_string(),
                );
            }
            if fail_once.contains(&order.as_str()) && hit == 1 {
                let mut headers = axum::http::HeaderMap::new();
                headers.insert("content-type", "application/json".parse().unwrap());
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    headers,
                    r#"{"error":{"message":"flaky","type":"server_error","code":"flaky"}}"#
                        .to_string(),
                );
            }
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            (
                StatusCode::OK,
                headers,
                format!(r#"{{"id":"ok","provider":"{order}"}}"#),
            )
        }
    };
    Router::new()
        .route("/v1/chat/completions", post(handler.clone()))
        .route("/v1/responses", post(handler))
}

#[tokio::test]
async fn models_menu_lists_exactly_the_ten_virtual_ids() {
    let db = scratch_db("menu");
    build_db(&db);
    let mock = spawn_mock(mock_app(
        Arc::new(Mutex::new(Upstream::default())),
        vec![],
        vec![],
    ))
    .await;
    let gw = spawn_gateway(mock, &db).await;

    let res = client()
        .get(format!("http://{gw}/v1/models"))
        .header("authorization", API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    let mut ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "openrouter/pair/decision",
            "openrouter/pair/decision/free",
            "openrouter/pair/free",
            "openrouter/pair/low",
            "openrouter/pair/privacy/decision",
            "openrouter/pair/privacy/decision/free",
            "openrouter/pair/privacy/free",
            "openrouter/pair/privacy/low",
            "openrouter/pair/privacy/think",
            "openrouter/pair/think",
        ]
    );
    assert!(body["data"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| { e["object"] == "model" && e["owned_by"] == "pennyrouter" }));
}

#[tokio::test]
async fn virtual_tiers_route_to_real_offerings() {
    let db = scratch_db("tiers");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    for (virtual_id, provider, model) in [
        ("openrouter/pair/low", "Cheap", "m/cheap"),
        ("openrouter/pair/think", "Dear", "m/dear"),
        ("openrouter/pair/free", "Freebie", "m/free"),
    ] {
        let res = post_chat(gw, virtual_id).await;
        assert_eq!(res.status(), StatusCode::OK, "for {virtual_id}");
        assert_eq!(
            res.headers().get("x-pennyrouter-provider").unwrap(),
            provider,
            "for {virtual_id}",
        );
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["provider"], provider, "upstream saw {virtual_id}");
        let guard = seen.lock().unwrap();
        assert_eq!(
            guard.models.get(provider).map(String::as_str),
            Some(model),
            "upstream model rewritten to the backend id for {virtual_id}",
        );
    }
    // Privacy variant routes the same pick with retention enforced.
    let res = post_chat(gw, "openrouter/pair/privacy/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    let guard = seen.lock().unwrap();
    let provider_obj = guard.providers.get("Cheap").cloned().unwrap();
    assert_eq!(provider_obj["zdr"], serde_json::json!(true));
    assert_eq!(provider_obj["data_collection"], serde_json::json!("deny"));
}

#[tokio::test]
async fn unknown_virtual_ids_pass_through_untouched() {
    let db = scratch_db("unknown-virtual");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "other/pair/free").await;
    assert_eq!(res.status(), StatusCode::OK);
    // No provider object on the wire: the mock defaults its echo to
    // "?", proving the body passed through unrouted and unrewritten.
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["provider"], "?");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("?").map(String::as_str),
        Some("other/pair/free")
    );
}

#[tokio::test]
async fn virtual_retry_lands_on_another_provider() {
    let db = scratch_db("retry");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    // Cheap is cheapest but flaky once: the retry lands on Dear (the
    // only other priced, non-free pick in this DB).
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec!["Cheap"])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("x-pennyrouter-provider").unwrap(), "Dear");
    let guard = seen.lock().unwrap();
    assert_eq!(guard.hits.get("Cheap"), Some(&1));
    assert_eq!(guard.hits.get("Dear"), Some(&1));
    let total: u64 = guard.hits.values().sum();
    assert_eq!(total, 2, "exactly one retry, hits: {:?}", guard.hits);
}

#[tokio::test]
async fn throttled_providers_are_avoided_and_counted() {
    let db = scratch_db("throttle");
    build_db(&db);
    // m/mid needed as the fallback target.
    {
        let catalog = Catalog::open(&db).unwrap();
        catalog
            .insert_model(&ModelRow {
                model_id: "m/mid".to_string(),
                name: "m/mid".to_string(),
                source: "test".to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
            })
            .unwrap();
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: "m/mid".to_string(),
                provider_name: "Mid".to_string(),
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
        let id = catalog.endpoints_for("m/mid").unwrap()[0].id;
        catalog
            .update_endpoint_index(
                id,
                &EndpointIndex {
                    price_prompt: Some(3.0),
                    price_completion: Some(3.0),
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
        // Judged `standard`: the `low`-tier quality floor (Iter 14)
        // needs a judged tier, and this test is about throttling.
        catalog
            .update_endpoint_judgment(
                id,
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
    let seen = Arc::new(Mutex::new(Upstream::default()));
    // Cheap always 429s: first request retries onto Mid and records it.
    let mock = spawn_mock(mock_app(seen.clone(), vec!["Cheap"], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("x-pennyrouter-provider").unwrap(), "Mid");

    let metrics = client()
        .get(format!("http://{gw}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("pennyrouter_upstream_throttled_total 1"),
        "429 recorded, got:\n{metrics}",
    );

    // Next request skips Cheap outright: no new Cheap hit.
    let res = post_chat(gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    let guard = seen.lock().unwrap();
    assert_eq!(guard.hits.get("Cheap"), Some(&1), "no second Cheap hit");
    assert_eq!(guard.hits.get("Mid"), Some(&2));
}

// Iter 14: product gaps (quality floor, fail-closed budgets,
// affinity, Responses routing).

async fn post_body(gw: SocketAddr, path: &str, body: &str) -> reqwest::Response {
    client()
        .post(format!("http://{gw}{path}"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn over_budget_virtual_fails_closed() {
    let db = scratch_db("budget");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    // Cheapest priced offering is 1.0/1.0: a 0.5 cap fits nothing.
    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/low","messages":[],"provider":{"max_price":{"prompt":0.5,"completion":0.5}}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "budget_exceeded");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    // Fail-closed means the upstream never saw the request.
    assert!(seen.lock().unwrap().hits.is_empty());
}

#[tokio::test]
async fn think_floor_unmet_without_high_tier() {
    let db = scratch_db("floor");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    // `think` needs a `high` tier; restricting to Cheap (`low`) leaves
    // the floor unmet instead of serving below the promise.
    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/think","messages":[],"provider":{"only":["Cheap"]}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "quality_floor_unmet");
    assert!(seen.lock().unwrap().hits.is_empty());
}

#[tokio::test]
async fn ignored_cheapest_never_wins_on_price() {
    let db = scratch_db("ineligible");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/low","messages":[],"provider":{"ignore":["Cheap"]}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("x-pennyrouter-provider").unwrap(), "Dear");
    let guard = seen.lock().unwrap();
    assert_eq!(guard.models.get("Dear").map(String::as_str), Some("m/dear"));
    assert!(!guard.hits.contains_key("Cheap"));
}

#[tokio::test]
async fn affinity_sticks_then_policy_change_reroutes() {
    let db = scratch_db("affinity");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    // Same session twice: the pin sticks to Cheap.
    for _ in 0..2 {
        let res = post_chat(gw, "openrouter/pair/low").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get("x-pennyrouter-provider").unwrap(),
            "Cheap"
        );
    }
    // A new restriction invalidates the pin: Dear serves instead.
    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/low","messages":[],"provider":{"ignore":["Cheap"]}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("x-pennyrouter-provider").unwrap(), "Dear");
    // A new budget cap overrides affinity too: nothing fits 0.5.
    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/low","messages":[],"provider":{"max_price":{"prompt":0.5,"completion":0.5}}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "budget_exceeded");
}

#[tokio::test]
async fn responses_routes_virtual_ids_like_chat() {
    let db = scratch_db("responses");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_body(
        gw,
        "/v1/responses",
        r#"{"model":"openrouter/pair/low","input":"hi"}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "Cheap"
    );
    {
        let guard = seen.lock().unwrap();
        assert_eq!(
            guard.models.get("Cheap").map(String::as_str),
            Some("m/cheap"),
            "Responses model rewritten to the backend id",
        );
    }

    // Rejections apply on Responses too.
    let res = post_body(
        gw,
        "/v1/responses",
        r#"{"model":"openrouter/pair/think","input":"hi","provider":{"only":["Cheap"]}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "quality_floor_unmet");
}

#[tokio::test]
async fn free_tier_fails_over_on_throttle() {
    let db = scratch_db("free-failover");
    build_db(&db);
    // Second free offering so the throttled one can yield.
    {
        let catalog = Catalog::open(&db).unwrap();
        catalog
            .insert_model(&ModelRow {
                model_id: "m/free2".to_string(),
                name: "m/free2".to_string(),
                source: "test".to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
            })
            .unwrap();
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: "m/free2".to_string(),
                provider_name: "Free2".to_string(),
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
        let id = catalog.endpoints_for("m/free2").unwrap()[0].id;
        catalog
            .update_endpoint_index(
                id,
                &EndpointIndex {
                    price_prompt: Some(0.0),
                    price_completion: Some(0.0),
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
    let seen = Arc::new(Mutex::new(Upstream::default()));
    // Freebie always 429s: the retry lands on Free2.
    let mock = spawn_mock(mock_app(seen.clone(), vec!["Freebie"], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "openrouter/pair/free").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "Free2"
    );
    // Live eligibility: the next request skips Freebie outright.
    let res = post_chat(gw, "openrouter/pair/free").await;
    assert_eq!(res.status(), StatusCode::OK);
    let guard = seen.lock().unwrap();
    assert_eq!(guard.hits.get("Freebie"), Some(&1), "no second Freebie hit");
    assert_eq!(guard.hits.get("Free2"), Some(&2));
}

/// Iter 21: `free/fallback/low` serves paid when the free leg 429s —
/// one free attempt, no usable retry, then exactly one fallback call.
#[tokio::test]
async fn free_fallback_serves_paid_after_free_429() {
    let db = scratch_db("free-fallback");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec!["Freebie"], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "openrouter/pair/free/fallback/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-pennyrouter-provider").unwrap(),
        "Cheap"
    );
    let guard = seen.lock().unwrap();
    assert_eq!(guard.hits.get("Freebie"), Some(&1));
    assert_eq!(guard.hits.get("Cheap"), Some(&1));
    assert_eq!(guard.models.get("Cheap").unwrap(), "m/cheap");
}

/// Iter 21: upstream quota headers reach the client on 429s instead
/// of being stripped.
#[tokio::test]
async fn upstream_ratelimit_headers_reach_client_on_429() {
    let db = scratch_db("ratelimit-headers");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec!["Cheap", "Dear"], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_chat(gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res.headers().get("x-ratelimit-remaining").unwrap(), "0");
    assert_eq!(res.headers().get("x-ratelimit-limit").unwrap(), "50");
    assert_eq!(
        res.headers().get("x-ratelimit-reset").unwrap(),
        "1793491200000"
    );
}

/// Iter 22: a tool-bearing request skips the cheaper tool-incapable
/// offering and pins the capable one, instead of failing upstream
/// with "No endpoints found that support tool use".
#[tokio::test]
async fn tool_request_routes_to_capable_offering() {
    let db = scratch_db("tools-route");
    build_tools_db(&db, true);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/low","messages":[],"tools":[{"type":"function","function":{"name":"patch"}}]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let guard = seen.lock().unwrap();
    assert_eq!(guard.hits.get("ToolCo"), Some(&1));
    assert_eq!(guard.hits.get("NoToolCo"), None);
}

/// Iter 22: a tool-bearing request with no capable offering fails
/// closed with a message naming the fix, instead of pinning a
/// provider the upstream then 404s.
#[tokio::test]
async fn tool_request_without_capable_offering_fails_closed() {
    let db = scratch_db("tools-closed");
    build_tools_db(&db, false);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/low","messages":[],"tools":[{"type":"function","function":{"name":"patch"}}]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "tools_unsupported");
    assert!(
        body["error"]["message"].as_str().unwrap().contains("tool"),
        "rejection names the tool problem: {}",
        body["error"]["message"]
    );
    assert!(seen.lock().unwrap().hits.is_empty());
}

/// Two paid `low`-tier offerings on one model: NoToolCo is cheaper
/// but tool-incapable; ToolCo costs more and supports tools.
/// `with_capable` toggles whether ToolCo is actually capable.
fn build_tools_db(path: &std::path::Path, with_capable: bool) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "m/tool".to_string(),
            name: "m/tool".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    for (provider, price, supports) in [("NoToolCo", 1.0, false), ("ToolCo", 2.0, with_capable)] {
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: "m/tool".to_string(),
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
            .endpoints_for("m/tool")
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
                    supports_tools: Some(i64::from(supports)),
                    supports_decisions: None,
                },
            )
            .unwrap();
        catalog
            .update_endpoint_judgment(
                id,
                &EndpointJudgment {
                    quality_tier: Some("low".to_string()),
                    quality_score: None,
                    judged_at: "t".to_string(),
                    judge_model: None,
                    judge_prompt: None,
                },
            )
            .unwrap();
    }
}

/// Iter 24: `decision/free` serves the decisions-capable free
/// offering, skipping the cheaper-or-equal incapable one.
#[tokio::test]
async fn decision_free_routes_to_capable_offering() {
    let db = scratch_db("decision-route");
    build_decision_db(&db, true, false);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/decision/free","messages":[]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let guard = seen.lock().unwrap();
    assert_eq!(guard.hits.get("DecCo"), Some(&1));
    assert_eq!(guard.hits.get("NoDec"), None);
}

/// Iter 24: `decision/free/fallback/low` serves the paid low tier
/// when no free offering is decisions-capable.
#[tokio::test]
async fn decision_free_falls_back_to_low() {
    let db = scratch_db("decision-fallback");
    build_decision_db(&db, false, true);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/decision/free/fallback/low","messages":[]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let guard = seen.lock().unwrap();
    assert_eq!(guard.hits.get("Cheap"), Some(&1));
    assert_eq!(guard.hits.get("NoDec"), None);
}

/// Iter 24: `decision/free` with no capable offering and no fallback
/// fails closed (503 availability, like the other tiers) without
/// touching the upstream.
#[tokio::test]
async fn decision_free_starved_fails_closed() {
    let db = scratch_db("decision-closed");
    build_decision_db(&db, false, false);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone(), vec![], vec![])).await;
    let gw = spawn_gateway(mock, &db).await;

    let res = post_body(
        gw,
        "/v1/chat/completions",
        r#"{"model":"openrouter/pair/decision/free","messages":[]}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
    assert!(seen.lock().unwrap().hits.is_empty());
}

/// Decision fixtures: `m/dfree` holds two zero-cost offerings —
/// NoDec (never capable) and DecCo (capable when `with_capable`) —
/// and `m/dpaid` optionally holds a judged paid offering for the
/// fallback leg.
fn build_decision_db(path: &std::path::Path, with_capable: bool, with_paid: bool) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    let mut specs: Vec<(&str, &str, f64, bool, Option<&str>)> = vec![
        ("m/dfree", "NoDec", 0.0, false, None),
        ("m/dfree", "DecCo", 0.0, with_capable, None),
    ];
    if with_paid {
        specs.push(("m/dpaid", "Cheap", 1.0, false, Some("low")));
    }
    for (model, provider, price, decisions, tier) in specs {
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
        if let Some(tier) = tier {
            catalog
                .update_endpoint_judgment(
                    id,
                    &EndpointJudgment {
                        quality_tier: Some(tier.to_string()),
                        quality_score: None,
                        judged_at: "t".to_string(),
                        judge_model: None,
                        judge_prompt: None,
                    },
                )
                .unwrap();
        }
    }
}
