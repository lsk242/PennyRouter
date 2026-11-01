//! Iter 26: live per-request classifier (`openrouter/pair/smart[/max]`).
//!
//! A mock classifier upstream plus a mock completion upstream prove the
//! contract: classified tiers serve, failures fall back to the one
//! configured tier, auth/config failures answer directly, and the
//! classifier never bypasses budget, tool, or floor policy.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use pennyrouter_catalog::{Catalog, EndpointIndex, EndpointJudgment, EndpointRow, ModelRow};
use pennyrouter_gateway::{ClassifierConfig, Metrics};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

const API_KEY: &str = "Bearer test-key";
const CLASSIFIER_MODEL: &str = "test-classifier";

/// Catalog: m/cheap (Cheap 1.0/low), m/dear (Dear 9.0/high), m/free
/// (Freebie 0.0/standard), m/raw (Raw 0.5/unjudged). All fp16, none
/// tool-capable — so tool-bearing requests starve by construction.
fn build_db(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    for (model, provider, price, tier) in [
        ("m/cheap", "Cheap", 1.0, Some("low")),
        ("m/dear", "Dear", 9.0, Some("high")),
        ("m/free", "Freebie", 0.0, Some("standard")),
        ("m/raw", "Raw", 0.5, None),
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
    drop(catalog);
}

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-smart-{test}-{}.db", std::process::id()))
}

/// One classifier call as the mock saw it.
struct SeenCall {
    auth: Option<String>,
    body: serde_json::Value,
}

struct ClassifierMock {
    status: u16,
    body: String,
    delay_ms: u64,
    seen: Mutex<Vec<SeenCall>>,
}

async fn classifier_handler(
    State(mock): State<Arc<ClassifierMock>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    {
        let mut seen = mock.seen.lock().unwrap();
        seen.push(SeenCall {
            auth: headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            body: serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        });
    }
    if mock.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(mock.delay_ms)).await;
    }
    (
        StatusCode::from_u16(mock.status).unwrap(),
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        mock.body.clone(),
    )
        .into_response()
}

/// Completion upstream: records the forced provider + rewritten model,
/// optionally fails one provider once (retry test).
#[derive(Default)]
struct Upstream {
    hits: HashMap<String, u64>,
    providers: HashMap<String, serde_json::Value>,
    models: HashMap<String, String>,
}

fn mock_upstream(seen: Arc<Mutex<Upstream>>, fail_once: Vec<&'static str>) -> Router {
    let handler = move |body: Bytes| {
        let seen = seen.clone();
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
            if fail_once.contains(&order.as_str()) && hit == 1 {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    r#"{"error":{"message":"flaky","type":"server_error","code":"flaky"}}"#
                        .to_string(),
                );
            }
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                format!(r#"{{"id":"ok","provider":"{order}"}}"#),
            )
        }
    };
    Router::new()
        .route("/v1/chat/completions", post(handler.clone()))
        .route("/v1/responses", post(handler))
}

async fn spawn_mock(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn classifier_config(base: &str) -> ClassifierConfig {
    ClassifierConfig {
        model: CLASSIFIER_MODEL.to_string(),
        base_url: base.to_string(),
        api_key: None,
        timeout: Duration::from_millis(2000),
        fallback_tier: "low".to_string(),
        max_tokens: 64,
        max_input_chars: 32_000,
    }
}

async fn spawn_gateway(
    upstream: SocketAddr,
    db: &std::path::Path,
    classifier: Option<ClassifierConfig>,
) -> (SocketAddr, Arc<Metrics>) {
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(db).expect("router loads"));
    let mut state =
        pennyrouter_gateway::AppState::new(format!("http://{upstream}")).with_router(router);
    if let Some(config) = classifier {
        state = state.with_classifier(config);
    }
    let metrics = state.metrics_handle();
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, metrics)
}

fn verdict_mock(content: &str) -> Arc<ClassifierMock> {
    Arc::new(ClassifierMock {
        status: 200,
        body: serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string(),
        delay_ms: 0,
        seen: Mutex::new(Vec::new()),
    })
}

async fn setup(
    test: &str,
    classifier: Arc<ClassifierMock>,
    fail_once: Vec<&'static str>,
) -> (SocketAddr, Arc<Mutex<Upstream>>, Arc<Metrics>) {
    let db = scratch_db(test);
    build_db(&db);
    let clf = spawn_mock(
        Router::new()
            .route("/chat/completions", post(classifier_handler))
            .with_state(classifier),
    )
    .await;
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_upstream(seen.clone(), fail_once)).await;
    let (gw, metrics) =
        spawn_gateway(mock, &db, Some(classifier_config(&format!("http://{clf}")))).await;
    (gw, seen, metrics)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn post_chat(gw: SocketAddr, body: serde_json::Value) -> reqwest::Response {
    client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(serde_json::to_string(&body).unwrap())
        .send()
        .await
        .unwrap()
}

fn chat_body(model: &str) -> serde_json::Value {
    serde_json::json!({"model": model, "messages": [{"role": "user", "content": "hello"}]})
}

#[tokio::test]
async fn smart_serves_the_classified_tier() {
    let mock = verdict_mock(r#"{"tier":"low","confidence":0.86}"#);
    let (gw, seen, metrics) = setup("classified", mock, vec![]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::OK);
    let headers = res.headers().clone();
    assert_eq!(headers["x-pennyrouter-smart-tier"], "low");
    assert_eq!(headers["x-pennyrouter-smart-classifier"], CLASSIFIER_MODEL);
    assert_eq!(headers["x-pennyrouter-classifier-confidence"], "0.86");
    assert!(headers.contains_key("x-pennyrouter-classifier-latency-ms"));
    assert!(!headers.contains_key("x-pennyrouter-classifier-fallback"));
    assert_eq!(headers["x-pennyrouter-provider"], "Cheap");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.models["Cheap"], "m/cheap");
    let rendered = metrics.render();
    assert!(rendered.contains("pennyrouter_classifier_total{result=\"ok\",tier=\"low\"} 1"));
}

#[tokio::test]
async fn smart_serves_classified_free() {
    let mock = verdict_mock(r#"{"tier":"free"}"#);
    let (gw, seen, _) = setup("classified-free", mock, vec![]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-pennyrouter-smart-tier"], "free");
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Freebie");
    assert_eq!(seen.lock().unwrap().models["Freebie"], "m/free");
}

#[tokio::test]
async fn smart_max_serves_think() {
    let mock = verdict_mock(r#"{"tier":"think"}"#);
    let (gw, seen, _) = setup("max-think", mock, vec![]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart/max")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-pennyrouter-smart-tier"], "think");
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Dear");
    assert_eq!(seen.lock().unwrap().models["Dear"], "m/dear");
}

#[tokio::test]
async fn smart_rejects_out_of_domain_tier_and_falls_back() {
    // `think` is outside the plain-smart tier set: invalid verdict,
    // fallback to `low`, marked.
    let mock = verdict_mock(r#"{"tier":"think"}"#);
    let (gw, _, metrics) = setup("wrong-tier", mock, vec![]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-pennyrouter-smart-tier"], "low");
    assert_eq!(res.headers()["x-pennyrouter-classifier-fallback"], "true");
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Cheap");
    let rendered = metrics.render();
    assert!(rendered.contains("pennyrouter_classifier_total{result=\"invalid\",tier=\"none\"} 1"));
    assert!(rendered.contains("pennyrouter_smart_fallback_total{tier=\"low\"} 1"));
}

#[tokio::test]
async fn smart_lowest_has_no_quality_floor() {
    // Cheapest paid is the unjudged Raw pick: `lowest` takes it while
    // `low` (judged floor) would take Cheap.
    let mock = verdict_mock(r#"{"tier":"lowest"}"#);
    let (gw, _, _) = setup("lowest", mock, vec![]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-pennyrouter-smart-tier"], "lowest");
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Raw");
}

#[tokio::test]
async fn smart_without_classifier_fails_closed() {
    let db = scratch_db("unconfigured");
    build_db(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_upstream(seen, vec![])).await;
    let (gw, metrics) = spawn_gateway(mock, &db, None).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "classifier_not_configured");
    // Plain ids still route with no classifier configured.
    let res = post_chat(gw, chat_body("openrouter/pair/low")).await;
    assert_eq!(res.status(), StatusCode::OK);
    let rendered = metrics.render();
    assert!(rendered
        .contains("pennyrouter_classifier_total{result=\"not_configured\",tier=\"none\"} 1"));
}

#[tokio::test]
async fn smart_timeout_falls_back() {
    let mock = Arc::new(ClassifierMock {
        status: 200,
        body: serde_json::json!({"choices": [{"message": {"content": r#"{"tier":"think"}"#}}]})
            .to_string(),
        delay_ms: 5000,
        seen: Mutex::new(Vec::new()),
    });
    let db = scratch_db("timeout");
    build_db(&db);
    let clf = spawn_mock(
        Router::new()
            .route("/chat/completions", post(classifier_handler))
            .with_state(mock),
    )
    .await;
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let upstream = spawn_mock(mock_upstream(seen.clone(), vec![])).await;
    let mut config = classifier_config(&format!("http://{clf}"));
    config.timeout = Duration::from_millis(100);
    let (gw, metrics) = spawn_gateway(upstream, &db, Some(config)).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart/max")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-pennyrouter-smart-tier"], "low");
    assert_eq!(res.headers()["x-pennyrouter-classifier-fallback"], "true");
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Cheap");
    let rendered = metrics.render();
    assert!(rendered.contains("pennyrouter_classifier_total{result=\"timeout\",tier=\"none\"} 1"));
}

#[tokio::test]
async fn smart_invalid_reply_falls_back() {
    let mock = Arc::new(ClassifierMock {
        status: 200,
        body: "this is not json".to_string(),
        delay_ms: 0,
        seen: Mutex::new(Vec::new()),
    });
    let (gw, _, _) = setup("invalid", mock, vec![]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-pennyrouter-classifier-fallback"], "true");
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Cheap");
}

#[tokio::test]
async fn smart_auth_failure_answers_502() {
    let mock = Arc::new(ClassifierMock {
        status: 401,
        body: r#"{"error":{"message":"bad key"}}"#.to_string(),
        delay_ms: 0,
        seen: Mutex::new(Vec::new()),
    });
    let (gw, _, metrics) = setup("auth", mock, vec![]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "classifier_auth_error");
    let rendered = metrics.render();
    assert!(
        rendered.contains("pennyrouter_classifier_total{result=\"auth_error\",tier=\"none\"} 1")
    );
}

#[tokio::test]
async fn smart_classifier_does_not_bypass_tool_policy() {
    // Classified `free`, but no endpoint supports tools: the final leg
    // still fails closed instead of serving.
    let mock = verdict_mock(r#"{"tier":"free"}"#);
    let (gw, _, _) = setup("tools", mock, vec![]).await;

    let res = post_chat(
        gw,
        serde_json::json!({
            "model": "openrouter/pair/smart",
            "messages": [{"role": "user", "content": "patch this"}],
            "tools": [{"type": "function", "function": {"name": "patch"}}],
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let headers = res.headers().clone();
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "tools_unsupported");
    // The classifier headers ride the rejection: the decision is
    // visible, not silently re-picked.
    assert_eq!(headers["x-pennyrouter-smart-tier"], "free");
}

#[tokio::test]
async fn smart_classifier_does_not_bypass_budget() {
    let mock = verdict_mock(r#"{"tier":"low"}"#);
    let (gw, _, _) = setup("budget", mock, vec![]).await;

    let res = post_chat(
        gw,
        serde_json::json!({
            "model": "openrouter/pair/smart",
            "messages": [{"role": "user", "content": "hi"}],
            "provider": {"max_price": {"prompt": 0.001, "completion": 0.001}},
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let headers = res.headers().clone();
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "budget_exceeded");
    assert_eq!(headers["x-pennyrouter-smart-tier"], "low");
    assert!(!headers.contains_key("x-pennyrouter-classifier-fallback"));
}

#[tokio::test]
async fn smart_privacy_marks_both_legs() {
    let mock = verdict_mock(r#"{"tier":"low"}"#);
    let db = scratch_db("privacy");
    build_db(&db);
    let clf = spawn_mock(
        Router::new()
            .route("/chat/completions", post(classifier_handler))
            .with_state(mock.clone()),
    )
    .await;
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let upstream = spawn_mock(mock_upstream(seen.clone(), vec![])).await;
    let (gw, _) = spawn_gateway(
        upstream,
        &db,
        Some(classifier_config(&format!("http://{clf}"))),
    )
    .await;

    let res = post_chat(gw, chat_body("openrouter/pair/privacy/smart")).await;
    assert_eq!(res.status(), StatusCode::OK);
    // Classifier leg: zero-retention fields set, caller key preserved.
    let seen_clf = mock.seen.lock().unwrap();
    assert_eq!(seen_clf.len(), 1);
    assert_eq!(seen_clf[0].auth.as_deref(), Some(API_KEY));
    assert_eq!(seen_clf[0].body["provider"]["zdr"], serde_json::json!(true));
    assert_eq!(
        seen_clf[0].body["provider"]["data_collection"],
        serde_json::json!("deny")
    );
    // Completion leg: same privacy fields on the forced provider object.
    let seen_up = seen.lock().unwrap();
    assert_eq!(seen_up.providers["Cheap"]["zdr"], serde_json::json!(true));
    assert_eq!(
        seen_up.providers["Cheap"]["data_collection"],
        serde_json::json!("deny")
    );
}

#[tokio::test]
async fn smart_retry_stays_inside_the_classified_tier() {
    let mock = verdict_mock(r#"{"tier":"low"}"#);
    let (gw, _, _) = setup("retry", mock, vec!["Cheap"]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::OK);
    // Cheap 503'd once: the retry stays in `low` (Dear), never
    // re-classifies into another tier.
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Dear");
    assert_eq!(res.headers()["x-pennyrouter-smart-tier"], "low");
    assert!(!res
        .headers()
        .contains_key("x-pennyrouter-classifier-fallback"));
}

#[tokio::test]
async fn smart_serves_the_responses_endpoint() {
    let mock = verdict_mock(r#"{"tier":"low"}"#);
    let (gw, seen, _) = setup("responses", mock.clone(), vec![]).await;

    let res = client()
        .post(format!("http://{gw}/v1/responses"))
        .header("authorization", API_KEY)
        .header("content-type", "application/json")
        .body(r#"{"model":"openrouter/pair/smart","input":"do the thing"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-pennyrouter-smart-tier"], "low");
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Cheap");
    assert_eq!(seen.lock().unwrap().models["Cheap"], "m/cheap");
    // The classifier saw the responses `input`, not chat messages.
    let seen_clf = mock.seen.lock().unwrap();
    let user = seen_clf[0].body["messages"][1]["content"].as_str().unwrap();
    let envelope: serde_json::Value = serde_json::from_str(user).unwrap();
    assert_eq!(envelope["prompt"], serde_json::json!("do the thing"));
}

#[tokio::test]
async fn smart_fallback_suffix_is_rejected() {
    let mock = verdict_mock(r#"{"tier":"low"}"#);
    let (gw, _, _) = setup("smart-fallback", mock, vec![]).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart/fallback/low")).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_model");
}

#[tokio::test]
async fn smart_fallback_tier_is_configurable() {
    let mock = Arc::new(ClassifierMock {
        status: 500,
        body: "boom".to_string(),
        delay_ms: 0,
        seen: Mutex::new(Vec::new()),
    });
    let db = scratch_db("fallback-lowest");
    build_db(&db);
    let clf = spawn_mock(
        Router::new()
            .route("/chat/completions", post(classifier_handler))
            .with_state(mock),
    )
    .await;
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let upstream = spawn_mock(mock_upstream(seen, vec![])).await;
    let mut config = classifier_config(&format!("http://{clf}"));
    config.fallback_tier = "lowest".to_string();
    let (gw, metrics) = spawn_gateway(upstream, &db, Some(config)).await;

    let res = post_chat(gw, chat_body("openrouter/pair/smart")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-pennyrouter-smart-tier"], "lowest");
    assert_eq!(res.headers()["x-pennyrouter-classifier-fallback"], "true");
    assert_eq!(res.headers()["x-pennyrouter-provider"], "Raw");
    let rendered = metrics.render();
    assert!(rendered.contains("pennyrouter_smart_fallback_total{tier=\"lowest\"} 1"));
}
