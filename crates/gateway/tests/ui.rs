//! Iter 17: gateway debug UI (`GET /ui`, `GATEWAY_UI=1`).
//!
//! The flag is process-global, so these tests serialize on a lock
//! around app construction (the only point the flag is read).
//! Disabled means no `/ui` route at all: the path falls through to
//! the fallback proxy like any unknown path.

use pennyrouter_catalog::{Catalog, EndpointRow, ModelRow};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn lock_env() -> tokio::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

fn scratch_db(test: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("gateway-ui-{test}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let catalog = Catalog::open(&path).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "m/ui".to_string(),
            name: "m/ui".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "m/ui".to_string(),
            provider_name: "UiProvider".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(1.0),
            price_completion: Some(2.0),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: Some("fp16".to_string()),
            promo_discount: None,
            processed_at: Some("t".to_string()),
            quality_tier: Some("high".to_string()),
            quality_score: Some(0.9),
            judged_at: Some("t".to_string()),
            judge_model: Some("recorded".to_string()),
            judge_prompt: Some("recorded".to_string()),
        })
        .unwrap();
    drop(catalog);
    path
}

async fn spawn_mock() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = {
        let seen = seen.clone();
        axum::Router::new().route(
            "/{*path}",
            axum::routing::any(move |uri: axum::http::Uri, body: axum::body::Bytes| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(format!(
                        "{} {}",
                        uri.path(),
                        String::from_utf8_lossy(&body)
                    ));
                    (
                        axum::http::StatusCode::OK,
                        [("content-type", "application/json")],
                        r#"{"id":"ok"}"#.to_string(),
                    )
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, seen)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[tokio::test]
async fn ui_disabled_falls_through_to_fallback() {
    let _guard = lock_env().await;
    std::env::remove_var("GATEWAY_UI");
    let (mock, seen) = spawn_mock().await;
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}"));
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let res = client()
        .get(format!("http://{gw}/ui"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    // No /ui route: the fallback proxied it upstream.
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(seen.lock().unwrap()[0].starts_with("/ui "));
}

#[tokio::test]
async fn ui_enabled_shows_ranking_and_metrics() {
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let (mock, _) = spawn_mock().await;
    let db = scratch_db("ranking");
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).expect("router loads"));
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}")).with_router(router);
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::env::remove_var("GATEWAY_UI");

    let res = client()
        .get(format!("http://{gw}/ui"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    assert!(res.headers()["content-type"]
        .to_str()
        .unwrap()
        .contains("text/html"));
    let body = res.text().await.unwrap();
    assert!(body.contains("m/ui"), "model key listed");
    assert!(body.contains("UiProvider"), "offering listed");
    assert!(body.contains("openrouter/pair/low"), "menu listed");
    assert!(body.contains("Metrics"), "metrics section present");
    assert!(body.contains("pennyrouter_"), "metrics text embedded");
}

#[tokio::test]
async fn landing_shows_version_config_and_ui_link() {
    let _guard = lock_env().await;
    std::env::remove_var("GATEWAY_UI");
    let (mock, _) = spawn_mock().await;
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}"))
        .with_upstream("baseten", "http://127.0.0.1:9");
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let res = client().get(format!("http://{gw}/")).send().await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let body = res.text().await.unwrap();
    assert!(body.contains("0.11.0"), "crate version shown");
    assert!(body.contains("build"), "build hash labeled");
    assert!(body.contains("/ui"), "links the debug UI");
    assert!(body.contains("disabled"), "UI flag state shown");
    assert!(body.contains("baseten"), "pinned upstreams listed");
    assert!(body.contains("off (passthrough)"), "routing state shown");
}

#[tokio::test]
async fn ui_enabled_without_router_reports_off() {
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let (mock, _) = spawn_mock().await;
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}"));
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::env::remove_var("GATEWAY_UI");

    let body = client()
        .get(format!("http://{gw}/ui"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("Routing: <strong>off</strong>"));
}

#[tokio::test]
async fn ui_enabled_leaves_chat_path_intact() {
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let (mock, seen) = spawn_mock().await;
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}"));
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::env::remove_var("GATEWAY_UI");

    // The request path is untouched by UI registration.
    let res = client()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"model":"m","messages":[]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    assert_eq!(seen.lock().unwrap().len(), 1);
}
