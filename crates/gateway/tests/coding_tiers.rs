//! Iter 48: `openrouter/pair/coding` (+ `privacy/`) runs the
//! cheapest ordering scoped to the most-used (top-usage bracket)
//! models. No bracket data means no pool — it fails closed
//! instead of silently serving the full list. (Iter 51 dropped
//! the `coding/think` sibling; Iter 54 added `coding/task` —
//! cheapest cost-per-task over the same bracket, intel 28+.)

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use pennyrouter_catalog::{
    Catalog, EndpointIndex, EndpointRank, EndpointRow, EndpointScore, ModelIndex, ModelRow,
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const API_KEY: &str = "Bearer [REDACTED]";

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-coding-{test}-{}.db", std::process::id()))
}

fn build_db(path: &std::path::Path, bracket: bool) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    // (model, provider, price, tier, score, intel, cost_task)
    for (model, provider, price, tier, score, intel, cost_task) in [
        ("m/pop-cheap", "Cheap", 5e-7, "low", 0.1, 60, 0.01),
        ("m/pop-best", "Best", 1e-6, "high", 0.9, 90, 0.005),
        ("m/obscure", "Sneaky", 1e-7, "standard", 0.5, 95, 0.001),
        ("m/pop-weak", "Weak", 7e-7, "low", 0.2, 20, 0.0005),
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
        let endpoint = catalog.endpoints_for(model).unwrap()[0].id;
        catalog
            .update_endpoint_rank(
                endpoint,
                &EndpointRank {
                    rank_score: None,
                    rank_quality: None,
                    rank_efficiency: None,
                    rank_popularity: None,
                    rank_activity: None,
                    rank_cost_task: Some(cost_task),
                    rank_basis: None,
                    rank_ruleset: None,
                    ranked_at: None,
                },
            )
            .unwrap();
    }
    if bracket {
        catalog
            .set_meta(
                "bracket",
                r#"{"ruleset":"rank-v2","members":["m/pop-best","m/pop-cheap","m/pop-weak"]}"#,
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
    providers: HashMap<String, serde_json::Value>,
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
            seen.providers
                .insert(order.clone(), json["provider"].clone());
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
async fn coding_routes_cheapest_in_bracket() {
    let db = scratch_db("route");
    build_db(&db, true);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Sneaky is cheaper overall but sits outside the bracket.
    let res = post_chat(gw, "openrouter/pair/coding").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("Cheap").map(String::as_str),
        Some("m/pop-cheap")
    );
}

#[tokio::test]
async fn coding_without_bracket_fails_closed() {
    let db = scratch_db("empty");
    build_db(&db, false);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    for model in ["openrouter/pair/coding", "openrouter/pair/privacy/coding"] {
        let res = post_chat(gw, model).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{model}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["error"]["code"], "no_eligible_offering");
    }
    assert!(seen.lock().unwrap().models.is_empty());
}

#[tokio::test]
async fn coding_privacy_and_mintel_and_fallback_target() {
    let db = scratch_db("variants");
    build_db(&db, true);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Privacy variant: same pick, retention fields enforced.
    let res = post_chat(gw, "openrouter/pair/privacy/coding").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
    {
        let guard = seen.lock().unwrap();
        let provider = guard.providers.get("Cheap").cloned().unwrap();
        assert_eq!(provider["zdr"], serde_json::json!(true));
        assert_eq!(provider["data_collection"], serde_json::json!("deny"));
    }

    // Mintel floor 85 drops Cheap (60): Best (90) serves.
    let res = post_chat(gw, "openrouter/pair/coding/mintel/85").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Best");

    // Valid fallback target: no free offerings exist, so the free leg
    // fails straight into the coding pick.
    let res = post_chat(gw, "openrouter/pair/free/fallback/coding").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Cheap");
}

#[test]
fn coding_previews_list_bracket_only() {
    let db = scratch_db("preview");
    build_db(&db, true);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    let coding = router.preview("openrouter/pair/coding", None, 5);
    assert!(coding.rejection.is_none(), "{:?}", coding.rejection);
    assert_eq!(coding.fresh.as_ref().unwrap().provider, "Cheap");
    assert!(
        coding.candidates.iter().all(|c| c.model_id != "m/obscure"),
        "out-of-bracket never lists: {:?}",
        coding
            .candidates
            .iter()
            .map(|c| &c.model_id)
            .collect::<Vec<_>>()
    );
    // Iter 51: `coding/think` is dropped — unknown ids preview
    // passthrough, never a candidate list.
    let dropped = router.preview("openrouter/pair/coding/think", None, 5);
    assert!(dropped.fresh.is_none());
    assert!(dropped.candidates.is_empty());
    assert!(dropped.rejection.is_none());
    // No bracket data: the preview fails closed like routing.
    let db = scratch_db("preview-empty");
    build_db(&db, false);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    let empty = router.preview("openrouter/pair/coding", None, 5);
    assert_eq!(
        empty.rejection.as_ref().unwrap().code,
        "no_eligible_offering"
    );
}

#[tokio::test]
async fn menu_lists_coding_ids() {
    let state = pennyrouter_gateway::AppState::new("http://127.0.0.1:9".to_string());
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{gw}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    for id in ["openrouter/pair/coding", "openrouter/pair/privacy/coding"] {
        assert!(ids.contains(&id), "{id} listed");
    }
    for id in [
        "openrouter/pair/coding/think",
        "openrouter/pair/privacy/coding/think",
    ] {
        assert!(!ids.contains(&id), "{id} dropped in Iter 51");
    }
    for id in [
        "openrouter/pair/coding/task",
        "openrouter/pair/privacy/coding/task",
    ] {
        assert!(ids.contains(&id), "{id} listed");
    }
}

#[tokio::test]
async fn coding_task_routes_cheapest_task_cost_in_bracket() {
    let db = scratch_db("task-route");
    build_db(&db, true);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Best's task cost (0.005) beats Cheap's (0.01) even though Cheap
    // is cheaper per token; Sneaky (0.001) sits outside the bracket
    // and Weak (0.0005) below the intel floor.
    let res = post_chat(gw, "openrouter/pair/coding/task").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Best");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("Best").map(String::as_str),
        Some("m/pop-best")
    );
}

#[tokio::test]
async fn coding_task_without_bracket_fails_closed() {
    let db = scratch_db("task-empty");
    build_db(&db, false);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    for model in [
        "openrouter/pair/coding/task",
        "openrouter/pair/privacy/coding/task",
    ] {
        let res = post_chat(gw, model).await;
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{model}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["error"]["code"], "no_eligible_offering");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no coding pool data"),
            "{model}"
        );
    }
    assert!(seen.lock().unwrap().models.is_empty());
}

#[tokio::test]
async fn coding_task_privacy_mintel_and_fallback_target() {
    let db = scratch_db("task-variants");
    build_db(&db, true);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db).await;

    // Privacy variant routes the same pick.
    let res = post_chat(gw, "openrouter/pair/privacy/coding/task").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Best");

    // Mintel composes above the fixed floor: 80 drops Cheap (60).
    let res = post_chat(gw, "openrouter/pair/coding/task/mintel/80").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Best");
    assert_eq!(
        res.headers()["x-pennyrouter-mintel"].to_str().unwrap(),
        "80"
    );

    // Valid fallback target: no free offerings exist, so the free leg
    // fails straight into the coding/task pick.
    let res = post_chat(gw, "openrouter/pair/free/fallback/coding/task").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "Best");
}

#[test]
fn coding_task_preview_lists_cheapest_task_first() {
    let db = scratch_db("task-preview");
    build_db(&db, true);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    let preview = router.preview("openrouter/pair/coding/task", None, 5);
    assert!(preview.rejection.is_none(), "{:?}", preview.rejection);
    assert_eq!(preview.fresh.as_ref().unwrap().provider, "Best");
    let ids: Vec<&str> = preview
        .candidates
        .iter()
        .map(|c| c.model_id.as_str())
        .collect();
    assert_eq!(ids, vec!["m/pop-best", "m/pop-cheap"]);
}
