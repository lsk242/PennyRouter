//! Iter 45: long-tail min-value fallback. Models outside the
//! top-usage bracket route cheapest-first with no tier floor —
//! unknown is cheapest-first, never quality-first. Bracket members
//! never take the long-tail path, and the fallback only exists when
//! bracket data exists (no data means pure legacy).

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use pennyrouter_catalog::{
    Catalog, ClassRanking, EndpointIndex, EndpointRow, EndpointScore, ModelRow, RankEntry,
    RankingDocument,
};
use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const API_KEY: &str = "Bearer [REDACTED]";

fn scratch_db(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("gateway-longtail-{test}-{}.db", std::process::id()))
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

fn put_offering(catalog: &Catalog, model: &str, provider: &str, price: f64, tier: Option<&str>) {
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
    if let Some(tier) = tier {
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
}

/// In-bracket standard model plus an untiered rare pair. No ranks
/// anywhere, so the legacy path (with long-tail) decides.
fn build_db_tail(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    put_model(&catalog, "m/popular");
    put_offering(&catalog, "m/popular", "Pop", 5e-7, Some("standard"));
    put_model(&catalog, "m/rare");
    put_offering(&catalog, "m/rare", "RareCheap", 1e-7, None);
    put_offering(&catalog, "m/rare", "RareDear", 9e-7, Some("high"));
    catalog
        .set_meta(
            "bracket",
            r#"{"ruleset":"rank-v2","members":["m/popular"]}"#,
        )
        .unwrap();
}

/// In-bracket high-tier model pricier than an out-of-bracket high
/// model: priority beats price.
fn build_db_priority(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let catalog = Catalog::open(path).unwrap();
    catalog.init().unwrap();
    put_model(&catalog, "m/star");
    put_offering(&catalog, "m/star", "StarA", 5e-6, Some("high"));
    put_offering(&catalog, "m/star", "StarB", 6e-6, Some("high"));
    put_model(&catalog, "m/rare");
    put_offering(&catalog, "m/rare", "RareCheap", 1e-7, Some("high"));
    catalog
        .set_meta("bracket", r#"{"ruleset":"rank-v2","members":["m/star"]}"#)
        .unwrap();
}

/// Ranking that orders the expensive provider first, for both the
/// rare and the star models.
fn ranking_dear_first() -> RankingDocument {
    let class = |first: &str, second: &str| ClassRanking {
        class: Some("fp16".to_string()),
        default: vec![
            RankEntry {
                provider: first.to_string(),
                tag: None,
            },
            RankEntry {
                provider: second.to_string(),
                tag: None,
            },
        ],
        price: vec![],
        quality: vec![],
        score: vec![],
    };
    let mut models = BTreeMap::new();
    models.insert("m/rare".to_string(), vec![class("RareDear", "RareCheap")]);
    models.insert("m/star".to_string(), vec![class("StarB", "StarA")]);
    RankingDocument {
        created_at: "2026-11-24T00:00:00Z".to_string(),
        catalog_version: None,
        models,
        free_offers: BTreeMap::new(),
    }
}

async fn spawn_gateway(
    upstream: SocketAddr,
    db: &std::path::Path,
    ranking: Option<RankingDocument>,
) -> SocketAddr {
    let router = Arc::new(pennyrouter_gateway::routing::Router::load(db).expect("router loads"));
    if let Some(doc) = ranking {
        router.swap_ranking(doc);
    }
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
async fn think_falls_back_to_cheapest_out_of_bracket() {
    let db = scratch_db("think-tail");
    build_db_tail(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, None).await;

    // In-bracket Pop is standard (below the high floor): the untiered
    // RareCheap serves — no floor on the long tail.
    let res = post_chat(gw, "openrouter/pair/think").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "RareCheap");
    let guard = seen.lock().unwrap();
    assert_eq!(
        guard.models.get("RareCheap").map(String::as_str),
        Some("m/rare")
    );
}

#[tokio::test]
async fn bracket_members_never_take_long_tail() {
    let db = scratch_db("priority");
    build_db_priority(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, None).await;

    // RareCheap is cheaper AND high-tier, but out-of-bracket: the
    // in-bracket StarA wins on legacy quality order.
    let res = post_chat(gw, "openrouter/pair/think").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "StarA");
}

#[tokio::test]
async fn pinned_rare_model_skips_ranking_for_cheapest() {
    let db = scratch_db("pinned-tail");
    build_db_tail(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, Some(ranking_dear_first())).await;

    // The ranking orders RareDear first, but m/rare sits outside the
    // bracket: cheapest wins.
    let res = post_chat(gw, "openrouter/pair/model/m/rare").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "RareCheap");
}

#[tokio::test]
async fn pinned_bracket_member_keeps_ranking() {
    let db = scratch_db("pinned-star");
    build_db_priority(&db);
    let seen = Arc::new(Mutex::new(Upstream::default()));
    let mock = spawn_mock(mock_app(seen.clone())).await;
    let gw = spawn_gateway(mock, &db, Some(ranking_dear_first())).await;

    // m/star is bracketed: the published ranking (StarB first) wins
    // over inline cheapest.
    let res = post_chat(gw, "openrouter/pair/model/m/star").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(provider_of(&res), "StarB");
}

#[tokio::test]
async fn exhausted_tail_clarifies_the_rejection() {
    let db = scratch_db("clarify");
    build_db_tail(&db);
    let mock = spawn_mock(mock_app(Arc::new(Mutex::new(Upstream::default())))).await;
    let gw = spawn_gateway(mock, &db, None).await;

    // A ceiling below every price starves the long tail too: the
    // rejection keeps its code and names the fallback.
    let res = post_body(
        gw,
        r#"{"model":"openrouter/pair/think","messages":[],
            "provider":{"max_price":{"prompt":1e-9,"completion":1e-9}}}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "quality_floor_unmet");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside the top-usage bracket, falling back to cheapest"),
        "clarified: {}",
        body["error"]["message"]
    );
}

#[test]
fn previews_list_bracket_first_then_cheapest_tail() {
    let db = scratch_db("preview");
    build_db_tail(&db);
    let router = pennyrouter_gateway::routing::Router::load(&db).expect("router loads");
    // In-bracket Pop fails the high floor, so the list is pure tail,
    // cheapest first — untiered rows included.
    let think = router.preview("openrouter/pair/think", None, 5);
    assert!(think.rejection.is_none(), "{:?}", think.rejection);
    assert_eq!(think.fresh.as_ref().unwrap().provider, "RareCheap");
    let models: Vec<&str> = think
        .candidates
        .iter()
        .map(|c| c.provider.as_str())
        .collect();
    assert_eq!(models, vec!["RareCheap", "RareDear"]);
    // Pinned preview skips the ranking for out-of-bracket models.
    router.swap_ranking(ranking_dear_first());
    let pinned = router.preview("openrouter/pair/model/m/rare", None, 5);
    assert_eq!(pinned.fresh.as_ref().unwrap().provider, "RareCheap");
    let models: Vec<&str> = pinned
        .candidates
        .iter()
        .map(|c| c.provider.as_str())
        .collect();
    assert_eq!(models, vec!["RareCheap", "RareDear"]);
}
