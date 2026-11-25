//! Iter 44: routing reads the published score/ordering only.
//! `low`/`think` serve the bracket ends from the stored v2
//! components; `lowest` stays full-list cheapest with `/minindex/`
//! filtering and activity tie-breaks; missing score data falls back
//! to the legacy ordering.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use pennyrouter_catalog::{
    Catalog, EndpointIndex, EndpointRank, EndpointRow, EndpointScore, ModelIndex, ModelRow,
};
use pennyrouter_gateway::routing::OrderMode;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const API_KEY: &str = "Bearer [REDACTED]";

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-score-{test}-{}.db", std::process::id()))
}

fn put_model(catalog: &Catalog, model: &str) {
    catalog
        .insert_model(&ModelRow {
            model_id: model.to_string(),
            name: model.to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
}

fn put_endpoint(catalog: &Catalog, model: &str, provider: &str, price: f64) -> i64 {
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
    let id = catalog.endpoints_for(model).unwrap().last().unwrap().id;
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
    id
}

fn put_rank(
    catalog: &Catalog,
    id: i64,
    score: f64,
    quality: f64,
    efficiency: f64,
    activity: f64,
    cost_task: f64,
) {
    catalog
        .update_endpoint_rank(
            id,
            &EndpointRank {
                rank_score: Some(score),
                rank_quality: Some(quality),
                rank_efficiency: Some(efficiency),
                rank_popularity: None,
                rank_activity: Some(activity),
                rank_cost_task: Some(cost_task),
                rank_basis: Some("coding".to_string()),
                rank_ruleset: Some("rank-v2".to_string()),
                ranked_at: Some("t".to_string()),
            },
        )
        .unwrap();
}

/// Bracket fixture: four scored members plus two scored outsiders
/// that would win the legacy ordering.
fn build_db_bracket(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    // (model, provider, price, quality, cost_task, score, legacy tier)
    for (model, provider, price, quality, cost_task, score, tier) in [
        ("m/pop-cheap", "Cheap", 2e-7, 60.0, 0.002, 30.0, "low"),
        ("m/pop-best", "Best", 1e-6, 90.0, 0.01, 50.0, "low"),
        ("m/pop-weak", "Weak", 1e-7, 30.0, 0.001, 10.0, "low"),
        ("m/pop-pricey", "Pricey", 1e-5, 95.0, 0.1, 60.0, "high"),
        ("m/obscure-cheap", "Sneaky", 5e-8, 70.0, 0.0005, 40.0, "low"),
        ("m/obscure-best", "Dark", 3e-7, 99.0, 0.003, 70.0, "low"),
    ] {
        put_model(&catalog, model);
        let id = put_endpoint(&catalog, model, provider, price);
        put_rank(&catalog, id, score, quality, 0.5, 0.9, cost_task);
        catalog
            .update_endpoint_score(
                id,
                &EndpointScore {
                    quality_tier: Some(tier.to_string()),
                    quality_score: Some(0.5),
                    scored_at: "t".to_string(),
                    score_model: None,
                    score_prompt: None,
                },
            )
            .unwrap();
    }
    catalog
        .set_meta(
            "bracket",
            r#"{"ruleset":"rank-v2","members":["m/pop-best","m/pop-cheap","m/pop-pricey","m/pop-weak"]}"#,
        )
        .unwrap();
}

/// Lowest fixture: a cheapest outsider, an equal-price tie pair with
/// split activity, and a pricey indexed model. Only `m/dear` is a
/// bracket member, so every `lowest` pick below proves full-list.
fn build_db_lowest(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    for (model, provider, price, coding, level) in [
        ("m/tie-a", "TieA", 1e-7, 50, Some(0.9)),
        ("m/tie-b", "TieB", 1e-7, 50, Some(0.1)),
        ("m/dear", "Dear", 5e-7, 90, Some(0.5)),
        ("m/cheapster", "Cheapster", 5e-8, 30, None),
    ] {
        put_model(&catalog, model);
        put_endpoint(&catalog, model, provider, price);
        catalog
            .update_model_index(
                model,
                &ModelIndex {
                    aa_intelligence_index: None,
                    aa_coding_index: Some(coding),
                    aa_agentic_index: None,
                    aa_cost_per_task: None,
                },
            )
            .unwrap();
        if let Some(level) = level {
            catalog
                .update_model_activity(model, Some(level), Some(1.0))
                .unwrap();
        }
    }
    catalog
        .set_meta("bracket", r#"{"ruleset":"rank-v2","members":["m/dear"]}"#)
        .unwrap();
}

/// Legacy fixture: tiers and prices but no ranks and no bracket.
fn build_db_legacy(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    for (model, provider, price, tier, score) in [
        ("m/cheap", "Cheap", 1e-7, "low", 0.1),
        ("m/good", "Good", 1e-6, "high", 0.9),
    ] {
        put_model(&catalog, model);
        let id = put_endpoint(&catalog, model, provider, price);
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

async fn spawn_gateway(upstream: SocketAddr, db: &std::path::Path, mode: OrderMode) -> SocketAddr {
    let router = Arc::new(
        pennyrouter_gateway::routing::Router::load(db)
            .expect("router loads")
            .with_ordering(mode),
    );
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
            seen.lock().unwrap().models.insert(
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

async fn post_chat(gw: SocketAddr, model: &str) -> reqwest::Response {
    post_body(gw, &format!(r#"{{"model":"{model}","messages":[]}}"#)).await
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
async fn low_picks_cheapest_above_floor_in_bracket() {
    let db = scratch_db("low");
    build_db_bracket(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, OrderMode::ScoreFirst).await;

    let res = post_chat(gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("Cheap").map(String::as_str),
        Some("m/pop-cheap")
    );
}

#[tokio::test]
async fn think_picks_best_under_cap() {
    let db = scratch_db("think");
    build_db_bracket(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, OrderMode::ScoreFirst).await;

    let res = post_chat(gw, "openrouter/pair/think").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Best");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("Best").map(String::as_str),
        Some("m/pop-best")
    );
}

#[tokio::test]
async fn gates_apply_on_top_of_score_order() {
    let db = scratch_db("gates");
    build_db_bracket(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, OrderMode::ScoreFirst).await;

    // A ceiling below every price fails closed with the budget code.
    let res = post_body(
        gw,
        r#"{"model":"openrouter/pair/low","messages":[],
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
    assert!(seen.lock().unwrap().models.is_empty());
}

#[tokio::test]
async fn lowest_stays_full_list_cheapest() {
    let db = scratch_db("lowest");
    build_db_lowest(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, OrderMode::ScoreFirst).await;

    // Cheapest overall wins even though it sits outside the bracket.
    let res = post_chat(gw, "m/tie-a/lowest").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheapster");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("Cheapster").map(String::as_str),
        Some("m/cheapster")
    );
}

#[tokio::test]
async fn lowest_minindex_filters_and_activity_breaks_ties() {
    let db = scratch_db("lowest-floor");
    build_db_lowest(&db);
    let mock = spawn_mock(mock_app(Arc::new(Mutex::new(Upstream::default())))).await;
    let gw = spawn_gateway(mock, &db, OrderMode::ScoreFirst).await;

    // Floor 40 drops Cheapster (30): the 1e-7 tie breaks toward the
    // higher activity level.
    let res = post_chat(gw, "m/tie-a/lowest/minindex/40").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "TieA");
    assert_eq!(
        res.headers().get("x-pennyrouter-minindex").unwrap(),
        "40",
        "floor echoed back"
    );
    // Floor 85 leaves Dear (90) alone.
    let res = post_chat(gw, "m/tie-a/lowest/minindex/85").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Dear");
    // Nothing indexed that high: fail closed.
    let res = post_chat(gw, "m/tie-a/lowest/minindex/100").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_eligible_offering");
    // Malformed floors fail closed like the virtual suffix.
    let res = post_chat(gw, "m/tie-a/lowest/minindex/abc").await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_model");
}

#[tokio::test]
async fn missing_score_falls_back_to_legacy() {
    let db = scratch_db("legacy");
    build_db_legacy(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, OrderMode::ScoreFirst).await;

    // No bracket, no ranks: legacy cheapest-tiered and best-quality.
    let res = post_chat(gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
    let res = post_chat(gw, "openrouter/pair/think").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Good");
}

#[test]
fn previews_match_score_routing() {
    let db = scratch_db("preview");
    build_db_bracket(&db);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    let low = router.preview("openrouter/pair/low", None, 5);
    assert!(low.rejection.is_none(), "{:?}", low.rejection);
    assert_eq!(low.fresh.as_ref().unwrap().provider, "Cheap");
    // Below-floor and out-of-bracket offerings never list.
    let models: Vec<&str> = low.candidates.iter().map(|c| c.model_id.as_str()).collect();
    assert_eq!(models, vec!["m/pop-cheap", "m/pop-best", "m/pop-pricey"]);
    let think = router.preview("openrouter/pair/think", None, 5);
    assert_eq!(think.fresh.as_ref().unwrap().provider, "Best");
    let models: Vec<&str> = think
        .candidates
        .iter()
        .map(|c| c.model_id.as_str())
        .collect();
    assert_eq!(models, vec!["m/pop-best", "m/pop-cheap", "m/pop-weak"]);

    let db = scratch_db("preview-lowest");
    build_db_lowest(&db);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    let lowest = router.preview("m/tie-a/lowest/minindex/40", None, 5);
    assert!(lowest.rejection.is_none(), "{:?}", lowest.rejection);
    assert_eq!(lowest.fresh.as_ref().unwrap().provider, "TieA");
    let bad = router.preview("m/tie-a/lowest/minindex/abc", None, 5);
    assert_eq!(bad.rejection.as_ref().unwrap().code, "invalid_model");
}

/// Iter 47: legacy mode ignores the bracket entirely — full-list
/// legacy ordering, pre-score behavior.
#[tokio::test]
async fn legacy_mode_routes_full_list_legacy() {
    let db = scratch_db("legacy-mode");
    build_db_bracket(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, OrderMode::Legacy).await;

    // Cheapest tiered overall (out-of-bracket Sneaky), not the
    // in-bracket cheapest.
    let res = post_chat(gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Sneaky");
    // Only high tier anywhere (Pricey), bracket ignored.
    let res = post_chat(gw, "openrouter/pair/think").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Pricey");

    // Previews mirror the mode: full-list legacy order, bracket
    // ignored, cheapest first.
    let router = pennyrouter_gateway::routing::Router::load(&db)
        .expect("router loads")
        .with_ordering(OrderMode::Legacy);
    let low = router.preview("openrouter/pair/low", None, 5);
    assert!(low.rejection.is_none(), "{:?}", low.rejection);
    assert_eq!(low.fresh.as_ref().unwrap().provider, "Sneaky");
    let models: Vec<&str> = low.candidates.iter().map(|c| c.model_id.as_str()).collect();
    assert_eq!(
        models,
        vec![
            "m/obscure-cheap",
            "m/pop-weak",
            "m/pop-cheap",
            "m/obscure-best",
            "m/pop-best",
        ]
    );
}

/// Iter 47: the ordering counter tells score-first apart from
/// legacy per pick — the A/B comparison.
#[tokio::test]
async fn ordering_metric_counts_picks_by_ordering() {
    let db = scratch_db("ordering-metric");
    build_db_bracket(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;

    let score_gw = spawn_gateway(mock, &db, OrderMode::ScoreFirst).await;
    let res = post_chat(score_gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
    let text: String = reqwest::Client::new()
        .get(format!("http://{score_gw}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        text.contains("pennyrouter_routing_order_total{ordering=\"score\",tier=\"low\"} 1"),
        "score pick counted"
    );

    let mock = spawn_mock(mock_app(Arc::new(Mutex::new(Upstream::default())))).await;
    let legacy_gw = spawn_gateway(mock, &db, OrderMode::Legacy).await;
    let res = post_chat(legacy_gw, "openrouter/pair/low").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Sneaky");
    let text: String = reqwest::Client::new()
        .get(format!("http://{legacy_gw}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        text.contains("pennyrouter_routing_order_total{ordering=\"legacy\",tier=\"low\"} 1"),
        "legacy pick counted"
    );
}
