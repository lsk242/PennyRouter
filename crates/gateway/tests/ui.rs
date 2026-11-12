//! Iter 17: gateway debug UI (`GET /ui`, `GATEWAY_UI=1`).
//!
//! The flag is process-global, so these tests serialize on a lock
//! around app construction (the only point the flag is read).
//! Disabled means no `/ui` route at all: the path falls through to
//! the fallback proxy like any unknown path.

use pennyrouter_catalog::{
    Catalog, EndpointIndex, EndpointRow, EndpointScore, ModelIndex, ModelRow,
};
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
            supports_tools: None,
            supports_decisions: None,
            quality_tier: Some("high".to_string()),
            quality_score: Some(0.9),
            scored_at: Some("t".to_string()),
            score_model: Some("recorded".to_string()),
            score_prompt: Some("recorded".to_string()),
            price_override_tokens: None,
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
    // Price columns are labeled per million tokens, never per token.
    assert!(body.contains("$/M"), "prices labeled $/M");
    assert!(!body.contains("$/tok"), "no per-token prices shown");
    // Tabbed shell: one tab bar, five panels, every placeholder filled.
    assert!(body.contains("role=\"tablist\""), "tab bar present");
    for panel in ["overview", "models", "inspector", "classifier", "metrics"] {
        assert!(
            body.contains(&format!("id=\"tab-{panel}\"")),
            "{panel} panel present"
        );
    }
    assert!(!body.contains("{{"), "no unrendered placeholders");
}

/// Popularity section: pins, hot apps, and the citation timestamp
/// render when the artifact carries them.
#[tokio::test]
async fn ui_shows_popular_section() {
    use pennyrouter_catalog::{Catalog, EndpointRow, ModelRow};
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let (mock, _) = spawn_mock().await;
    let db = scratch_db("popular");
    let _ = std::fs::remove_file(&db);
    let catalog = Catalog::open(&db).unwrap();
    catalog.init().unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "m/pop".to_string(),
            name: "m/pop".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "m/pop".to_string(),
            provider_name: "PopProvider".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(1.0),
            price_completion: Some(2.0),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: None,
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
    catalog
        .set_meta("popular_models", r#"[{"slug":"x/y","tokens":9}]"#)
        .unwrap();
    catalog
        .set_meta(
            "popular_apps",
            r#"[{"name":"Cline","tokens":9,"requests":2}]"#,
        )
        .unwrap();
    catalog.set_meta("popular_as_of", "2026-11-04").unwrap();
    drop(catalog);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).expect("router loads"));
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}")).with_router(router);
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
    assert!(body.contains("Popular"), "popular section present");
    assert!(body.contains("openrouter/x/y"), "pin listed");
    assert!(body.contains("Cline"), "hot app listed");
    assert!(body.contains("2026-11-04"), "citation timestamp shown");
}

#[tokio::test]
async fn ui_flag_truthy_variants() {
    // I10(a): `true`/`yes` (any case) register /ui like `1`; anything
    // else falls through to the fallback proxy.
    let _guard = lock_env().await;
    for value in ["true", "TRUE", "yes", "YES"] {
        std::env::set_var("GATEWAY_UI", value);
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
        assert!(
            res.headers()["content-type"]
                .to_str()
                .unwrap()
                .contains("text/html"),
            "GATEWAY_UI={value} registers /ui"
        );
        assert!(
            seen.lock().unwrap().is_empty(),
            "GATEWAY_UI={value} serves locally, not proxied"
        );
    }
    for value in ["0", "false", "no"] {
        std::env::set_var("GATEWAY_UI", value);
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
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "GATEWAY_UI={value} falls through to fallback"
        );
    }
    std::env::remove_var("GATEWAY_UI");
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
    // Static template: every placeholder rendered, endpoint index intact.
    assert!(!body.contains("{{"), "no unrendered placeholders");
    assert!(body.contains("Endpoints"), "endpoint index present");
    assert!(body.contains("/v1/chat/completions"), "chat path listed");
    assert!(body.contains("/metrics"), "metrics path listed");
    assert!(
        body.contains("requires GATEWAY_UI=1"),
        "UI endpoint carries the flag note while disabled"
    );
}

#[tokio::test]
async fn landing_enabled_links_ui_and_marks_status() {
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let (mock, _) = spawn_mock().await;
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}"));
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // The flag stays set through the request: the landing page reads it
    // per request, like route registration reads it at startup.
    let body = client()
        .get(format!("http://{gw}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    std::env::remove_var("GATEWAY_UI");
    assert!(body.contains("(enabled)"), "UI flag state shown");
    assert!(
        body.contains("<a href=\"/ui\">debug UI</a>"),
        "status links the debug UI"
    );
    assert!(!body.contains("{{"), "no unrendered placeholders");
    assert!(!body.contains("requires GATEWAY_UI=1"), "flag note lifted");
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

// Iter 29: routing inspector preview. Rich fixture: distinct prices so
// low (cheapest) and think (best quality) disagree, plus free and
// decisions-capable offerings for the fixed combos.
fn preview_db(test: &str, cheap_price: f64) -> std::path::PathBuf {
    preview_db_inner(test, cheap_price, "m/code")
}

fn preview_db_inner(test: &str, cheap_price: f64, _tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "gateway-preview-{test}-{cheap_price}-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let catalog = Catalog::open(&path).unwrap();
    catalog.init().unwrap();
    for model_id in ["m/code", "m/free", "m/score"] {
        catalog
            .insert_model(&ModelRow {
                model_id: model_id.to_string(),
                name: model_id.to_string(),
                source: "test".to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
            })
            .unwrap();
    }
    // `insert_endpoint` stores core columns only; indexed + score
    // columns land via the update calls (same pattern as the virtual
    // model fixtures).
    let put = |catalog: &Catalog,
               model_id: &str,
               provider: &str,
               prompt: f64,
               completion: f64,
               tier: &str,
               score: f64,
               tools: Option<i64>,
               decisions: Option<i64>| {
        catalog
            .insert_endpoint(&EndpointRow {
                id: 0,
                model_id: model_id.to_string(),
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
                scored_at: None,
                score_model: None,
                score_prompt: None,
                price_override_tokens: None,
            })
            .unwrap();
        let row_id = catalog.endpoints_for(model_id).unwrap().last().unwrap().id;
        catalog
            .update_endpoint_index(
                row_id,
                &EndpointIndex {
                    price_prompt: Some(prompt),
                    price_completion: Some(completion),
                    price_cache_read: Some(prompt / 2.0),
                    price_cache_write: Some(prompt / 2.0),
                    precision_class: Some("fp16".to_string()),
                    promo_discount: Some(0.0),
                    processed_at: "t".to_string(),
                    supports_tools: tools,
                    supports_decisions: decisions,
                    price_override_tokens: None,
                },
            )
            .unwrap();
        catalog
            .update_endpoint_score(
                row_id,
                &EndpointScore {
                    quality_tier: Some(tier.to_string()),
                    quality_score: Some(score),
                    scored_at: "t".to_string(),
                    score_model: Some("recorded".to_string()),
                    score_prompt: Some("recorded".to_string()),
                },
            )
            .unwrap();
    };
    put(
        &catalog,
        "m/code",
        "Cheap",
        cheap_price,
        cheap_price,
        "high",
        0.9,
        Some(1),
        Some(0),
    );
    put(
        &catalog,
        "m/code",
        "Mid",
        2.0,
        2.0,
        "standard",
        0.5,
        Some(1),
        Some(0),
    );
    put(
        &catalog,
        "m/code",
        "Exp",
        3.0,
        3.0,
        "low",
        0.1,
        Some(0),
        Some(0),
    );
    put(
        &catalog,
        "m/free",
        "FreeP",
        0.0,
        0.0,
        "high",
        0.8,
        Some(1),
        Some(0),
    );
    // Paid decoy on the free model.
    put(
        &catalog,
        "m/free",
        "PaidP",
        5.0,
        5.0,
        "standard",
        0.4,
        Some(1),
        Some(0),
    );
    put(
        &catalog,
        "m/score",
        "ScoreP",
        1.5,
        1.5,
        "high",
        0.95,
        Some(1),
        Some(1),
    );
    drop(catalog);
    path
}

/// Display unit is $/M tokens: the priced preview_db fixture (1.0,
/// 2.0, 3.0 $/tok) renders ×1e6 in the /ui models table; stored class
/// "unknown" renders as `-`, like missing.
#[tokio::test]
async fn ui_prices_render_per_million_tokens() {
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let db = preview_db("money", 1.0);
    {
        let catalog = Catalog::open(&db).unwrap();
        let eps = catalog.endpoints_for("m/code").unwrap();
        let cheap = eps.iter().find(|e| e.provider_name == "Cheap").unwrap();
        catalog
            .update_endpoint_index(
                cheap.id,
                &EndpointIndex {
                    price_prompt: cheap.price_prompt,
                    price_completion: cheap.price_completion,
                    price_cache_read: cheap.price_cache_read,
                    price_cache_write: cheap.price_cache_write,
                    precision_class: Some("unknown".to_string()),
                    promo_discount: cheap.promo_discount,
                    processed_at: cheap
                        .processed_at
                        .clone()
                        .unwrap_or_else(|| "t".to_string()),
                    supports_tools: cheap.supports_tools,
                    supports_decisions: cheap.supports_decisions,
                    price_override_tokens: cheap.price_override_tokens,
                },
            )
            .unwrap();
        catalog
            .update_model_index(
                "m/code",
                &ModelIndex {
                    aa_intelligence_index: None,
                    aa_coding_index: Some(71),
                    aa_agentic_index: None,
                    aa_cost_per_task: None,
                },
            )
            .unwrap();
    }
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).expect("router loads"));
    let state =
        pennyrouter_gateway::AppState::new("http://127.0.0.1:9".to_string()).with_router(router);
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
    for (raw, shown) in [
        ("1.0", "1000000.00"),
        ("2.0", "2000000.00"),
        ("3.0", "3000000.00"),
    ] {
        assert!(
            body.contains(shown),
            "fixture {raw} $/tok renders as {shown} $/M"
        );
        let padded = format!("{shown}00");
        assert!(
            !body.contains(padded.as_str()),
            "fixture {raw} $/tok has no trailing zero decimals"
        );
    }
    assert!(body.contains("Cheap"), "unknown-class row still listed");
    assert!(
        !body.contains(">unknown<"),
        "stored class unknown renders as -, not unknown"
    );
    assert!(
        body.contains("coding 71"),
        "AA coding index shows on the model header"
    );
}

#[test]
fn preview_low_top_matches_decide_and_shows_columns() {
    let db = preview_db("unit-low", 1.0);
    let router = pennyrouter_gateway::routing::Router::load(&db).unwrap();
    let preview = router.preview("openrouter/pair/low", None, 3);
    assert!(preview.rejection.is_none(), "{:?}", preview.rejection);
    assert_eq!(preview.candidates.len(), 3);
    // Cheapest paid wins the fresh pick.
    let fresh = preview.fresh.as_ref().expect("fresh pick");
    assert_eq!(fresh.provider, "Cheap");
    // Anonymous preview: sticky mirrors fresh, no hit.
    assert!(!preview.sticky_hit);
    // Stored columns are all present on every row.
    for c in &preview.candidates {
        assert!(!c.provider.is_empty());
        assert!(!c.model_id.is_empty());
        assert!(c.price_prompt.is_some());
        assert!(c.price_completion.is_some());
        assert!(c.price_cache_read.is_some());
        assert!(c.price_cache_write.is_some());
        assert!(c.effective_cost.is_some());
        assert_eq!(c.precision_class.as_deref(), Some("fp16"));
        assert!(c.quality_tier.is_some());
        assert!(c.quality_score.is_some());
        assert!(c.supports_tools.is_some());
        assert!(c.supports_decisions.is_some());
        assert!(c.rank_index.is_some());
    }
    // Top-1 matches the real anonymous decision.
    let decision = router.decide("openrouter/pair/low", None).expect("decides");
    assert_eq!(decision.provider, fresh.provider);
    assert_eq!(decision.model_id, fresh.model_id);
}

#[test]
fn preview_think_disagrees_with_low_and_free_picks_free() {
    let db = preview_db("unit-tiers", 1.0);
    let router = pennyrouter_gateway::routing::Router::load(&db).unwrap();
    let low = router.preview("openrouter/pair/low", None, 3);
    let think = router.preview("openrouter/pair/think", None, 3);
    assert_eq!(low.fresh.as_ref().unwrap().provider, "Cheap");
    // Best quality (high 0.95) beats cheapest here.
    assert_eq!(think.fresh.as_ref().unwrap().provider, "ScoreP");
    let free = router.preview("openrouter/pair/free", None, 3);
    assert_eq!(free.fresh.as_ref().unwrap().provider, "FreeP");
    assert!(free.candidates.iter().all(|c| c.free_eligible));
    let decision = router.preview("openrouter/pair/decision", None, 3);
    assert_eq!(decision.fresh.as_ref().unwrap().provider, "ScoreP");
}

#[test]
fn preview_sticky_vs_fresh_survives_snapshot_swap_and_matches_decide() {
    let db1 = preview_db("unit-sticky-1", 1.0);
    let router = pennyrouter_gateway::routing::Router::load(&db1).unwrap();
    let auth = "Bearer test-key-1";
    let first = router
        .decide("openrouter/pair/low", Some(auth))
        .expect("first decides");
    assert_eq!(first.provider, "Cheap");
    assert!(!first.sticky);
    // Make Cheap expensive: fresh moves to ScoreP, the pin stays Cheap.
    let db2 = preview_db("unit-sticky-2", 10.0);
    let snap2 = pennyrouter_gateway::routing::Router::load_snapshot(&db2).unwrap();
    router.swap_snapshot(snap2);
    let preview = router.preview("openrouter/pair/low", Some(auth), 3);
    let fresh = preview.fresh.as_ref().unwrap();
    let sticky = preview.sticky.as_ref().unwrap();
    assert_eq!(fresh.provider, "ScoreP");
    assert_eq!(sticky.provider, "Cheap");
    assert!(preview.sticky_hit);
    assert!(sticky.sticky);
    assert!(!fresh.sticky);
    assert!(preview.session_key_hash.is_some());
    assert_eq!(preview.session_namespace, "openrouter/pair/low".to_string());
    // The real path agrees with the sticky preview.
    let second = router
        .decide("openrouter/pair/low", Some(auth))
        .expect("second decides");
    assert!(second.sticky);
    assert_eq!(second.provider, "Cheap");
}

#[test]
fn preview_never_pins_and_reports_hash() {
    let db = preview_db("unit-nopin", 1.0);
    let router = pennyrouter_gateway::routing::Router::load(&db).unwrap();
    let auth = "Bearer fresh-key-9";
    let before = router.preview("openrouter/pair/low", Some(auth), 3);
    assert!(!before.sticky_hit);
    let expected_hash = router.session_hash_for(auth, "openrouter/pair/low");
    assert_eq!(before.session_key_hash, Some(expected_hash));
    // A preview must not create the pin: the next real decision is fresh.
    let decision = router
        .decide("openrouter/pair/low", Some(auth))
        .expect("decides");
    assert!(!decision.sticky);
}

#[test]
fn preview_throttled_pin_falls_through_like_routing() {
    let db = preview_db("unit-throttle", 1.0);
    let router = pennyrouter_gateway::routing::Router::load(&db).unwrap();
    let auth = "Bearer throttle-key";
    let first = router
        .decide("openrouter/pair/low", Some(auth))
        .expect("pins Cheap");
    assert_eq!(first.provider, "Cheap");
    // Cool Cheap down: fresh moves on and the sticky pin must not win.
    router.note_throttled(
        &first.model_id,
        &first.provider,
        std::time::Instant::now() + std::time::Duration::from_secs(600),
    );
    let preview = router.preview("openrouter/pair/low", Some(auth), 5);
    let fresh = preview.fresh.as_ref().unwrap();
    assert_ne!(fresh.provider, "Cheap");
    assert!(!preview.sticky_hit);
    assert!(preview
        .candidates
        .iter()
        .find(|c| c.provider == "Cheap")
        .is_some_and(|c| c.throttled));
}

#[tokio::test]
async fn preview_endpoint_serves_json_with_key_hash() {
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let (mock, _) = spawn_mock().await;
    let db = preview_db("http-json", 1.0);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).expect("router loads"));
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}")).with_router(router);
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::env::remove_var("GATEWAY_UI");

    let body: serde_json::Value = client()
        .get(format!(
            "http://{gw}/ui/preview?model=openrouter/pair/low&top=3"
        ))
        .header("Authorization", "Bearer abc")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["model"], "openrouter/pair/low");
    assert_eq!(body["candidates"].as_array().unwrap().len(), 3);
    assert!(body["fresh"]["provider"].is_string());
    assert!(body["session_key_hash_hex"].is_string());
    let first = &body["candidates"][0];
    for key in [
        "provider",
        "model_id",
        "price_prompt",
        "price_completion",
        "price_cache_read",
        "price_cache_write",
        "effective_cost",
        "precision_class",
        "quality_tier",
        "quality_score",
        "supports_tools",
        "supports_decisions",
        "free_eligible",
        "availability",
        "rank_index",
        "sticky_match",
    ] {
        assert!(first.get(key).is_some(), "missing {key}");
    }
}

#[tokio::test]
async fn preview_endpoint_needs_model_and_router() {
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let (mock, _) = spawn_mock().await;
    let db = preview_db("http-err", 1.0);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).expect("router loads"));
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}")).with_router(router);
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let missing = client()
        .get(format!("http://{gw}/ui/preview"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), axum::http::StatusCode::BAD_REQUEST);

    // Router-less preview fails closed with 503, not passthrough.
    let bare = pennyrouter_gateway::AppState::new(format!("http://{mock}"));
    let bare_app = pennyrouter_gateway::app_with_state(bare);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw2 = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, bare_app).await.unwrap() });
    std::env::remove_var("GATEWAY_UI");
    let no_router = client()
        .get(format!("http://{gw2}/ui/preview?model=openrouter/pair/low"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        no_router.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn preview_disabled_falls_through_to_fallback() {
    let _guard = lock_env().await;
    std::env::remove_var("GATEWAY_UI");
    let (mock, seen) = spawn_mock().await;
    let db = preview_db("http-off", 1.0);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).expect("router loads"));
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}")).with_router(router);
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let res = client()
        .get(format!("http://{gw}/ui/preview?model=openrouter/pair/low"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(seen.lock().unwrap()[0].starts_with("/ui/preview "));
}

#[tokio::test]
async fn ui_page_contains_inspector() {
    let _guard = lock_env().await;
    std::env::set_var("GATEWAY_UI", "1");
    let (mock, _) = spawn_mock().await;
    let db = preview_db("http-page", 1.0);
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(&db).expect("router loads"));
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}")).with_router(router);
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
    assert!(body.contains("Routing inspector"), "inspector section");
    assert!(body.contains("inspector-key"), "key input");
    assert!(body.contains("Fixed combinations"), "fixed combos");
    assert!(body.contains("inspector-model"), "custom box");
    assert!(body.contains("openrouter/pair/low"), "fixed id listed");
    assert!(body.contains("session hash"), "hash explained");
    assert!(
        body.contains("Plain-request view"),
        "plain-request caveat shown"
    );
}
