//! Iter 62: sessions carry the calling application, and the
//! "why selected" rule moves into an expandable row under the model
//! line. The label comes from request headers — `X-Title`, else
//! referer (host+path), else user-agent — stamped onto live pins
//! after routing, refreshed on every request.

use axum::http::HeaderMap;
use pennyrouter_catalog::{Catalog, EndpointRow, ModelRow};
use pennyrouter_gateway::routing::Router;
use std::sync::Arc;

const API_KEY: &str = "Bearer [REDACTED]";

fn scratch_db(test: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("gateway-sess-{test}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let catalog = Catalog::open(&path).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "m/app".to_string(),
            name: "m/app".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "m/app".to_string(),
            provider_name: "AppP".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(1e-7),
            price_completion: Some(2e-7),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: Some("fp16".to_string()),
            quantization: None,
            promo_discount: None,
            processed_at: Some("t".to_string()),
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
    drop(catalog);
    path
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    map.insert("authorization", API_KEY.parse().unwrap());
    for (k, v) in pairs {
        map.insert(
            k.parse::<axum::http::HeaderName>().unwrap(),
            v.parse().unwrap(),
        );
    }
    map
}

fn route(router: &Router, headers: &HeaderMap) {
    let body = br#"{"model":"m/app","messages":[]}"#;
    assert!(
        router.route_request(headers, body).is_some(),
        "request routes and pins"
    );
}

fn session_app(router: &Router) -> Option<String> {
    let sessions = router.ui_summary().sessions;
    assert_eq!(sessions.len(), 1, "one live pin");
    assert!(
        !sessions[0].reason.is_empty(),
        "selection rule still recorded"
    );
    sessions[0].app.clone()
}

#[test]
fn session_app_uses_x_title_first() {
    let db = scratch_db("title");
    let router = Router::load(&db).unwrap();
    route(
        &router,
        &headers(&[
            ("x-title", "My Agent"),
            ("referer", "https://example.com/a"),
            ("user-agent", "curl/8"),
        ]),
    );
    assert_eq!(session_app(&router).as_deref(), Some("My Agent"));
}

#[test]
fn session_app_falls_back_to_referer_then_user_agent() {
    let db = scratch_db("referer");
    let router = Router::load(&db).unwrap();
    // Referer trims the scheme and query, keeps host+path.
    route(
        &router,
        &headers(&[
            ("referer", "https://example.com/a/b?x=1"),
            ("user-agent", "curl/8"),
        ]),
    );
    assert_eq!(session_app(&router).as_deref(), Some("example.com/a/b"));

    let db = scratch_db("ua");
    let router = Router::load(&db).unwrap();
    route(&router, &headers(&[("user-agent", "cursor/1.2")]));
    assert_eq!(session_app(&router).as_deref(), Some("cursor/1.2"));
}

#[test]
fn session_app_empty_without_client_headers() {
    let db = scratch_db("none");
    let router = Router::load(&db).unwrap();
    route(&router, &headers(&[]));
    assert_eq!(session_app(&router), None);
}

#[test]
fn session_app_refreshes_on_every_request() {
    let db = scratch_db("refresh");
    let router = Router::load(&db).unwrap();
    route(&router, &headers(&[("x-title", "First")]));
    assert_eq!(session_app(&router).as_deref(), Some("First"));
    route(&router, &headers(&[("x-title", "Second")]));
    assert_eq!(session_app(&router).as_deref(), Some("Second"));
}

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The sessions tab shows an Application column, no Why-selected
/// column — the rule hides in a row under the model line, toggled
/// by clicking the model name.
#[tokio::test]
async fn ui_sessions_expandable_reason_and_application() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("GATEWAY_UI", "1");
    let db = scratch_db("ui");
    let router = Arc::new(Router::load(&db).expect("router loads"));
    let state = pennyrouter_gateway::AppState::new("http://127.0.0.1:1".to_string())
        .with_router(router.clone());
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::env::remove_var("GATEWAY_UI");

    // Route directly so no upstream mock is needed; the pin the UI
    // renders is the same one routing stored.
    route(&router, &headers(&[("x-title", "Ui Probe")]));

    let body = reqwest::Client::new()
        .get(format!("http://{gw}/ui"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("<th>Application</th>"), "app column");
    assert!(
        !body.contains("<th>Why selected</th>"),
        "no why-selected column"
    );
    assert!(body.contains("Ui Probe"), "app label rendered");
    assert!(body.contains("sess-0"), "expandable row id");
    assert!(body.contains("Why selected:"), "rule in detail row");
    assert!(
        body.contains("onclick=\"document.getElementById('sess-0')"),
        "model name toggles the detail row"
    );
}
