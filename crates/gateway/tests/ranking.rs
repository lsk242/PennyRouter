//! Iter 6: the gateway serves published rankings, falling back to
//! inline sort for unranked models.

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use pennyrouter_catalog::{
    publish_ranking, rank_catalog, Assist, Catalog, EndpointIndex, EndpointJudgment, EndpointRow,
    ModelRow, Promos, Trends,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const RANKED: &str = "acme/gadget-1";
const UNRANKED: &str = "acme/gadget-2";
const CHAT_COMPLETION: &[u8] = include_bytes!("fixtures/chat_completion.json");

async fn spawn_mock(seen: Arc<Mutex<Vec<Vec<u8>>>>) -> SocketAddr {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |req_body: Bytes| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(req_body.to_vec());
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    CHAT_COMPLETION.to_vec(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn insert_offering(catalog: &Catalog, model: &str, provider: &str, price: f64, tier: &str) {
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: model.to_string(),
            provider_name: provider.to_string(),
            payload: format!(r#"{{"tag": "{provider}"}}"#),
            fetched_at: "t".to_string(),
            price_prompt: None,
            price_completion: None,
            price_cache_read: None,
            price_cache_write: None,
            precision_class: None,
            promo_discount: None,
            processed_at: None,
            supports_tools: None,
            quality_tier: None,
            quality_score: None,
            judged_at: None,
            judge_model: None,
            judge_prompt: None,
        })
        .unwrap();
    let ep = catalog.endpoints_for(model).unwrap().pop().unwrap();
    catalog
        .update_endpoint_index(
            ep.id,
            &EndpointIndex {
                price_prompt: Some(price),
                price_completion: Some(price),
                price_cache_read: None,
                price_cache_write: None,
                precision_class: None,
                promo_discount: None,
                processed_at: "t".to_string(),
                supports_tools: None,
            },
        )
        .unwrap();
    catalog
        .update_endpoint_judgment(
            ep.id,
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

fn build_db(path: &std::path::Path, models: &[&str]) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    for model in models {
        catalog
            .insert_model(&ModelRow {
                model_id: model.to_string(),
                name: "g".to_string(),
                source: "test".to_string(),
                payload: "{}".to_string(),
                fetched_at: "t".to_string(),
            })
            .unwrap();
        insert_offering(&catalog, model, "Prime", 5.0, "high");
        insert_offering(&catalog, model, "Econ", 1.0, "low");
    }
}

fn rank_dir(test: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("gateway-ranking-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Ranking covers RANKED only; the snapshot covers both models.
    let rank_db = dir.join("rank.db");
    build_db(&rank_db, &[RANKED]);
    let catalog = Catalog::open(&rank_db).unwrap();
    let assist_path = dir.join("assist.json");
    std::fs::write(
        &assist_path,
        format!(
            r#"{{"adjustments": [{{"model_id": "{RANKED}", "provider_name": "Prime",
                "action": "prefer", "reason": "test"}}]}}"#
        ),
    )
    .unwrap();
    let doc = rank_catalog(
        &catalog,
        &Trends::empty(),
        &Assist::load(&assist_path).unwrap(),
        &Promos::empty(),
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00Z",
        None,
    )
    .unwrap();
    publish_ranking(&doc, &dir).unwrap();
    let snap_db = dir.join("snap.db");
    build_db(&snap_db, &[RANKED, UNRANKED]);
    (dir, snap_db)
}

async fn provider_for(
    gw: SocketAddr,
    seen: &Arc<Mutex<Vec<Vec<u8>>>>,
    model: &str,
) -> (String, String) {
    let res = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .header("authorization", "Bearer K")
        .header("content-type", "application/json")
        .body(format!(
            r#"{{"model":"{model}","messages":[{{"role":"user","content":"hi"}}],"stream":false}}"#
        ))
        .send()
        .await
        .unwrap();
    let header = res
        .headers()
        .get("x-pennyrouter-provider")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let bodies = seen.lock().unwrap();
    let json: serde_json::Value = serde_json::from_slice(bodies.last().unwrap()).unwrap();
    (
        header,
        json["provider"]["order"][0].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn ranked_model_serves_ranking_and_unranked_falls_back() {
    let (dir, snap_db) = rank_dir("serve");
    let router =
        Arc::new(pennyrouter_gateway::routing::Router::load_with_ranking(&snap_db, &dir).unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mock = spawn_mock(seen.clone()).await;
    let state = pennyrouter_gateway::AppState::new(format!("http://{mock}")).with_router(router);
    let app = pennyrouter_gateway::app_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Inline price sort would pick Econ (1.0 < 5.0); the ranking (assist
    // prefer Prime) wins for the ranked model...
    let (header, forced) = provider_for(gw, &seen, &format!("{RANKED}/price")).await;
    assert_eq!(header, "Prime");
    assert_eq!(forced, "Prime");
    // ...while the unranked model falls back to inline sort.
    let (header, forced) = provider_for(gw, &seen, &format!("{UNRANKED}/price")).await;
    assert_eq!(header, "Econ");
    assert_eq!(forced, "Econ");
}

#[test]
fn tampered_ranking_is_rejected() {
    let (dir, snap_db) = rank_dir("tamper");
    let ranking_file = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ranking-")
                && p.extension().is_some_and(|e| e == "json")
                && !p
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with(".manifest.json")
        })
        .unwrap();
    std::fs::write(&ranking_file, r#"{"models": {}}"#).unwrap();
    assert!(pennyrouter_gateway::routing::Router::load_with_ranking(&snap_db, &dir).is_err());
}
