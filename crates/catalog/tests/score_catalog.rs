//! Iter 15: decision-model catalog + selection policy.
//!
//! The scorers file names which model scorers per backend and under
//! which policy. Each cataloged scorer reproduces its fixtures (live
//! backends against loopback stub servers, so no test touches the
//! network); unknown backends fail at load/resolve; call and spend
//! caps fail closed before any score is written; every score
//! carries its scorer model + prompt version.

mod common;

use common::{fixtures_dir, synced_db};
use pennyrouter_catalog::{
    build_score, plan_live_calls, run_processing, run_scoring, run_scoring_selected,
    select_endpoints, spend_line, BasetenScorer, Catalog, EndpointRow, ModalScorer,
    OpenRouterScorer, Scorer, ScorerCatalog, ScorerPolicy, PROMPT_VERSION,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

const STAMP: &str = "2026-10-29T00:00:00Z";

fn catalog() -> ScorerCatalog {
    ScorerCatalog::load(&fixtures_dir().join("scorers.json")).unwrap()
}

#[test]
fn shipped_catalog_loads_and_resolves_known_backends() {
    let catalog = catalog();
    assert_eq!(catalog.scorers.len(), 4);
    let baseten = catalog.resolve("baseten").unwrap();
    assert_eq!(baseten.model, BasetenScorer::MODEL);
    assert_eq!(baseten.prompt_version, PROMPT_VERSION);
    assert_eq!(baseten.auth_env, vec!["BASETEN_API_KEY".to_string()]);
    let modal = catalog.resolve("modal").unwrap();
    assert_eq!(modal.model, ModalScorer::MODEL);
    assert_eq!(
        modal.auth_env,
        vec!["MODAL_KEY_ID".to_string(), "MODAL_KEY_SECRET".to_string()]
    );
    let recorded = catalog.resolve("recorded").unwrap();
    assert!(recorded.recording.is_some());
    let openrouter = catalog.resolve("openrouter").unwrap();
    assert_eq!(openrouter.base_url, OpenRouterScorer::BASE_URL);
    assert_eq!(openrouter.prompt_version, PROMPT_VERSION);
    assert_eq!(openrouter.auth_env, ["RANKINGS_API_KEY".to_string()]);
}

#[test]
fn unknown_backend_rejected() {
    let catalog = catalog();
    assert!(catalog.resolve("watson").is_err());
    // Unknown backends fail the file, never a rebuild.
    let dir = common::scratch("scorers-unknown");
    let path = dir.join("scorers.json");
    std::fs::write(
        &path,
        r#"{"scorers": [{"backend": "watson", "model": "m"}]}"#,
    )
    .unwrap();
    assert!(ScorerCatalog::load(&path).is_err());
}

/// Canned `POST /chat/completions` response carrying one score.
fn chat_response(content: &str) -> String {
    let content = serde_json::to_string(content).unwrap();
    format!(
        r#"{{"id":"chatcmpl-test","object":"chat.completion","model":"test-model",
"choices":[{{"index":0,"message":{{"role":"assistant","content":{content}}},
"finish_reason":"stop"}}]}}"#
    )
}

/// Serve canned responses to sequential requests; returns (base URL,
/// request bodies seen).
fn stub_server(responses: Vec<String>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in &responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let header_len = loop {
                let n = stream.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
                if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let headers = String::from_utf8_lossy(&raw[..header_len]).to_string();
            let body_len: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    (name.trim().eq_ignore_ascii_case("content-length"))
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            while raw.len() < header_len + body_len {
                let n = stream.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                response.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            stream.flush().unwrap();
            requests.push(String::from_utf8_lossy(&raw).to_string());
        }
        requests
    });
    (base_url, handle)
}

#[test]
fn cataloged_recorded_score_reproduces_its_fixture() {
    let catalog = catalog();
    let entry = catalog.resolve("recorded").unwrap();
    // Recording paths resolve from the process working directory
    // (the crate dir under `cargo test`).
    let score = build_score(entry).unwrap();
    let identity = score.identity();
    assert_eq!(identity.model, "recorded");

    let (_dir, db) = synced_db("catalog-recorded");
    let db = Catalog::open(&db).unwrap();
    run_processing(&db, STAMP).unwrap();
    let summary = run_scoring(&db, score.as_ref(), STAMP).unwrap();
    // Same expectations as the per-backend recorded test.
    assert_eq!(summary.scored, 4);
    assert_eq!(summary.matched, 2);
    let eps = db.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps[1].quality_tier.as_deref(), Some("high"));
    assert_eq!(eps[1].quality_score, Some(0.91));
    // Audit trail: model + prompt on every scored row and in meta.
    assert_eq!(eps[1].score_model.as_deref(), Some("recorded"));
    assert_eq!(eps[1].score_prompt.as_deref(), Some("recorded"));
    assert_eq!(
        db.get_meta("score_model").unwrap().as_deref(),
        Some("recorded")
    );
}

#[test]
fn cataloged_baseten_score_reproduces_its_fixture() {
    let catalog = catalog();
    let entry = catalog.resolve("baseten").unwrap();
    let (base_url, server) = stub_server(vec![chat_response(
        r#"{"quality_tier": "high", "quality_score": 0.9}"#,
    )]);
    // The catalog's model is what the scorer sends.
    let score = BasetenScorer::new(&base_url, "test-key", &entry.model).unwrap();
    let out = score.score("m", "p", &json!({})).unwrap();
    assert_eq!(out.quality_tier.as_deref(), Some("high"));
    assert_eq!(out.quality_score, Some(0.9));
    let identity = score.identity();
    assert_eq!(identity.model, entry.model);
    assert_eq!(identity.prompt_version, PROMPT_VERSION);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["model"], json!(entry.model));
}

#[test]
fn cataloged_modal_score_reproduces_its_fixture() {
    let catalog = catalog();
    let entry = catalog.resolve("modal").unwrap();
    let (base_url, server) = stub_server(vec![chat_response(
        r#"{"quality_tier": "standard", "quality_score": 0.77}"#,
    )]);
    let score = ModalScorer::new(&base_url, "test-id", "test-secret", &entry.model).unwrap();
    let out = score.score("m", "p", &json!({})).unwrap();
    assert_eq!(out.quality_tier.as_deref(), Some("standard"));
    assert_eq!(out.quality_score, Some(0.77));
    let identity = score.identity();
    assert_eq!(identity.model, entry.model);
    assert_eq!(identity.prompt_version, PROMPT_VERSION);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["model"], json!(entry.model));
}

#[test]
fn cataloged_openrouter_score_reproduces_its_fixture() {
    let catalog = catalog();
    let entry = catalog.resolve("openrouter").unwrap();
    let (base_url, server) = stub_server(vec![chat_response(
        r#"{"quality_tier": "high", "quality_score": 0.85}"#,
    )]);
    let score = OpenRouterScorer::new(&base_url, "test-key", &entry.model).unwrap();
    let out = score.score("m", "p", &json!({})).unwrap();
    assert_eq!(out.quality_tier.as_deref(), Some("high"));
    assert_eq!(out.quality_score, Some(0.85));
    let identity = score.identity();
    assert_eq!(identity.model, entry.model);
    assert_eq!(identity.prompt_version, PROMPT_VERSION);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.contains("\r\nauthorization: Bearer test-key\r\n"));
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["model"], json!(entry.model));
}

#[test]
fn build_score_builds_openrouter_from_its_entry() {
    let catalog = catalog();
    let entry = catalog.resolve("openrouter").unwrap();
    std::env::set_var("RANKINGS_API_KEY", "probe-dummy");
    let score = build_score(entry).unwrap();
    std::env::remove_var("RANKINGS_API_KEY");
    let identity = score.identity();
    assert_eq!(identity.model, entry.model);
    assert_eq!(identity.prompt_version, PROMPT_VERSION);
}

#[test]
fn build_score_needs_secrets_before_any_call() {
    let catalog = catalog();
    let entry = catalog.resolve("baseten").unwrap();
    // No such variable set: construction fails without touching the
    // network (the stub URL is never even used).
    let mut entry = entry.clone();
    entry.auth_env = vec!["PENNYROUTER_TEST_MISSING_KEY".to_string()];
    std::env::remove_var("PENNYROUTER_TEST_MISSING_KEY");
    assert!(build_score(&entry).is_err());
}

#[test]
fn selection_scopes_and_samples_deterministically() {
    let (_dir, db) = synced_db("catalog-select");
    let db = Catalog::open(&db).unwrap();
    let all = db.all_endpoints().unwrap();
    assert_eq!(all.len(), 4);

    let full = ScorerPolicy::default();
    assert_eq!(select_endpoints(&all, &full).len(), 4);

    let scoped = ScorerPolicy {
        only_models: vec!["openai/gpt-6.1-terra-pro".to_string()],
        ..ScorerPolicy::default()
    };
    let subset = select_endpoints(&all, &scoped);
    assert_eq!(subset.len(), 3);
    assert!(subset
        .iter()
        .all(|ep| ep.model_id == "openai/gpt-6.1-terra-pro"));

    // Sampling is stable across rebuilds: same subset twice.
    let sampled = ScorerPolicy {
        sample_rate: 0.5,
        ..ScorerPolicy::default()
    };
    let first: Vec<i64> = select_endpoints(&all, &sampled)
        .iter()
        .map(|ep| ep.id)
        .collect();
    let second: Vec<i64> = select_endpoints(&all, &sampled)
        .iter()
        .map(|ep| ep.id)
        .collect();
    assert_eq!(first, second);

    let none = ScorerPolicy {
        sample_rate: 0.0,
        ..ScorerPolicy::default()
    };
    assert!(select_endpoints(&all, &none).is_empty());
}

#[test]
fn caps_fail_closed_before_any_write() {
    let catalog = catalog();
    let entry = catalog.resolve("baseten").unwrap();

    // Call cap breached.
    let capped = ScorerPolicy {
        max_calls: Some(1),
        ..ScorerPolicy::default()
    };
    assert!(plan_live_calls(4, entry, &capped).is_err());

    // Spend cap with known cost breached.
    let mut priced = entry.clone();
    priced.cost_per_call_usd = Some(0.01);
    let poor = ScorerPolicy {
        max_spend_usd: Some(0.001),
        ..ScorerPolicy::default()
    };
    assert!(plan_live_calls(4, &priced, &poor).is_err());

    // Spend cap with unknown cost fails rather than guessing.
    let blind = ScorerPolicy {
        max_spend_usd: Some(10.0),
        ..ScorerPolicy::default()
    };
    assert!(plan_live_calls(4, entry, &blind).is_err());

    // Fitting plan reports calls + spend for the log line.
    let rich = ScorerPolicy {
        max_calls: Some(10),
        max_spend_usd: Some(10.0),
        ..ScorerPolicy::default()
    };
    let plan = plan_live_calls(4, &priced, &rich).unwrap();
    assert_eq!(plan.calls, 4);
    assert!((plan.spend_usd.unwrap() - 0.04).abs() < 1e-12);
    let line = spend_line(&plan, &priced, &rich);
    assert!(line.contains("4 live calls"), "got: {line}");
    assert!(line.contains("0.0400"), "got: {line}");

    // End to end: a breached cap leaves the DB unscored.
    let (_dir, db) = synced_db("catalog-capped");
    let db = Catalog::open(&db).unwrap();
    run_processing(&db, STAMP).unwrap();
    let all = db.all_endpoints().unwrap();
    let selected = select_endpoints(&all, &capped);
    assert!(plan_live_calls(selected.len(), entry, &capped).is_err());
    for ep in db.all_endpoints().unwrap() {
        assert_eq!(ep.scored_at, None, "cap breach must not write");
    }
}

#[test]
fn live_run_stamps_audit_identity_per_row() {
    let (_dir, db) = synced_db("catalog-audit");
    let db = Catalog::open(&db).unwrap();
    run_processing(&db, STAMP).unwrap();
    let (base_url, server) = stub_server(vec![
        chat_response(r#"{"quality_tier": "high", "quality_score": 0.8}"#),
        chat_response(r#"{"quality_tier": "high", "quality_score": 0.8}"#),
        chat_response(r#"{"quality_tier": "high", "quality_score": 0.8}"#),
        chat_response(r#"{"quality_tier": "high", "quality_score": 0.8}"#),
    ]);
    let score = BasetenScorer::new(&base_url, "k", "audit-model").unwrap();
    let all = db.all_endpoints().unwrap();
    let selected = select_endpoints(&all, &ScorerPolicy::default());
    let summary = run_scoring_selected(&db, &score, &selected, STAMP).unwrap();
    assert_eq!(summary.scored, 4);
    assert_eq!(server.join().unwrap().len(), 4);
    // Every row carries the live model + prompt version.
    for ep in db.all_endpoints().unwrap() {
        assert_eq!(ep.score_model.as_deref(), Some("audit-model"));
        assert_eq!(ep.score_prompt.as_deref(), Some(PROMPT_VERSION));
    }
    assert_eq!(
        db.get_meta("score_model").unwrap().as_deref(),
        Some("audit-model")
    );
}

#[test]
fn sampling_distinguishes_endpoint_tags() {
    // B20: same (model, provider), different tags. Post-fix FNV
    // buckets are 0.3102 ("openai") vs 0.9060 ("openai/flex"), so
    // rate 0.5 keeps exactly one; pre-fix both hash 0.4552 and
    // select together.
    let row = |tag: &str| EndpointRow {
        id: 0,
        model_id: "m".to_string(),
        provider_name: "P".to_string(),
        payload: json!({"tag": tag}).to_string(),
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
    };
    let endpoints = vec![row("openai"), row("openai/flex")];
    let policy = ScorerPolicy {
        sample_rate: 0.5,
        ..ScorerPolicy::default()
    };
    let selected = select_endpoints(&endpoints, &policy);
    assert_eq!(selected.len(), 1);
    let tag: Value = serde_json::from_str(&selected[0].payload).unwrap();
    assert_eq!(tag["tag"], json!("openai"));
    // Deterministic: same input re-samples identically.
    assert_eq!(select_endpoints(&endpoints, &policy).len(), 1);
}

#[test]
fn build_score_normalizes_modal_root_url() {
    // B21: a catalog modal entry with a deployment-root base_url
    // (mirroring MODAL_SCORE_URL, no /v1 suffix) must still reach
    // /v1/chat/completions, like ModalScorer::from_env.
    let catalog = catalog();
    let mut entry = catalog.resolve("modal").unwrap().clone();
    let (stub_root, server) = stub_server(vec![chat_response(
        r#"{"quality_tier": "standard", "quality_score": 0.77}"#,
    )]);
    entry.base_url = stub_root;
    entry.auth_env = vec![
        "PENNYROUTER_TEST_MODAL_ID".to_string(),
        "PENNYROUTER_TEST_MODAL_SECRET".to_string(),
    ];
    std::env::set_var("PENNYROUTER_TEST_MODAL_ID", "id");
    std::env::set_var("PENNYROUTER_TEST_MODAL_SECRET", "secret");
    let score = build_score(&entry).unwrap();
    let out = score.score("m", "p", &json!({})).unwrap();
    std::env::remove_var("PENNYROUTER_TEST_MODAL_ID");
    std::env::remove_var("PENNYROUTER_TEST_MODAL_SECRET");
    assert_eq!(out.quality_tier.as_deref(), Some("standard"));
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("POST /v1/chat/completions "),
        "root-style base must gain /v1, got: {}",
        requests[0].lines().next().unwrap_or("")
    );
}
