//! Iter 15: decision-model catalog + selection policy.
//!
//! The judges file names which model judges per backend and under
//! which policy. Each cataloged judge reproduces its fixtures (live
//! backends against loopback stub servers, so no test touches the
//! network); unknown backends fail at load/resolve; call and spend
//! caps fail closed before any judgment is written; every judgment
//! carries its judge model + prompt version.

mod common;

use common::{fixtures_dir, synced_db};
use pennyrouter_catalog::{
    build_judge, plan_live_calls, run_judging, run_judging_selected, run_processing,
    select_endpoints, spend_line, BasetenJudge, Catalog, Judge, JudgeCatalog, JudgePolicy,
    ModalJudge, PROMPT_VERSION,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

const STAMP: &str = "2026-10-29T00:00:00Z";

fn catalog() -> JudgeCatalog {
    JudgeCatalog::load(&fixtures_dir().join("judges.json")).unwrap()
}

#[test]
fn shipped_catalog_loads_and_resolves_known_backends() {
    let catalog = catalog();
    assert_eq!(catalog.judges.len(), 3);
    let baseten = catalog.resolve("baseten").unwrap();
    assert_eq!(baseten.model, BasetenJudge::MODEL);
    assert_eq!(baseten.prompt_version, PROMPT_VERSION);
    assert_eq!(baseten.auth_env, vec!["BASETEN_API_KEY".to_string()]);
    let modal = catalog.resolve("modal").unwrap();
    assert_eq!(modal.model, ModalJudge::MODEL);
    assert_eq!(
        modal.auth_env,
        vec!["MODAL_KEY_ID".to_string(), "MODAL_KEY_SECRET".to_string()]
    );
    let recorded = catalog.resolve("recorded").unwrap();
    assert!(recorded.recording.is_some());
}

#[test]
fn unknown_backend_rejected() {
    let catalog = catalog();
    assert!(catalog.resolve("watson").is_err());
    // Unknown backends fail the file, never a rebuild.
    let dir = common::scratch("judges-unknown");
    let path = dir.join("judges.json");
    std::fs::write(
        &path,
        r#"{"judges": [{"backend": "watson", "model": "m"}]}"#,
    )
    .unwrap();
    assert!(JudgeCatalog::load(&path).is_err());
}

/// Canned `POST /chat/completions` response carrying one judgment.
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
fn cataloged_recorded_judge_reproduces_its_fixture() {
    let catalog = catalog();
    let entry = catalog.resolve("recorded").unwrap();
    // Recording paths resolve from the process working directory
    // (the crate dir under `cargo test`).
    let judge = build_judge(entry).unwrap();
    let identity = judge.identity();
    assert_eq!(identity.model, "recorded");

    let (_dir, db) = synced_db("catalog-recorded");
    let db = Catalog::open(&db).unwrap();
    run_processing(&db, STAMP).unwrap();
    let summary = run_judging(&db, judge.as_ref(), STAMP).unwrap();
    // Same expectations as the per-backend recorded test.
    assert_eq!(summary.judged, 4);
    assert_eq!(summary.matched, 2);
    let eps = db.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps[1].quality_tier.as_deref(), Some("high"));
    assert_eq!(eps[1].quality_score, Some(0.91));
    // Audit trail: model + prompt on every judged row and in meta.
    assert_eq!(eps[1].judge_model.as_deref(), Some("recorded"));
    assert_eq!(eps[1].judge_prompt.as_deref(), Some("recorded"));
    assert_eq!(
        db.get_meta("judge_model").unwrap().as_deref(),
        Some("recorded")
    );
}

#[test]
fn cataloged_baseten_judge_reproduces_its_fixture() {
    let catalog = catalog();
    let entry = catalog.resolve("baseten").unwrap();
    let (base_url, server) = stub_server(vec![chat_response(
        r#"{"quality_tier": "high", "quality_score": 0.9}"#,
    )]);
    // The catalog's model is what the judge sends.
    let judge = BasetenJudge::new(&base_url, "test-key", &entry.model).unwrap();
    let out = judge.judge("m", "p", &json!({})).unwrap();
    assert_eq!(out.quality_tier.as_deref(), Some("high"));
    assert_eq!(out.quality_score, Some(0.9));
    let identity = judge.identity();
    assert_eq!(identity.model, entry.model);
    assert_eq!(identity.prompt_version, PROMPT_VERSION);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["model"], json!(entry.model));
}

#[test]
fn cataloged_modal_judge_reproduces_its_fixture() {
    let catalog = catalog();
    let entry = catalog.resolve("modal").unwrap();
    let (base_url, server) = stub_server(vec![chat_response(
        r#"{"quality_tier": "standard", "quality_score": 0.77}"#,
    )]);
    let judge = ModalJudge::new(&base_url, "test-id", "test-secret", &entry.model).unwrap();
    let out = judge.judge("m", "p", &json!({})).unwrap();
    assert_eq!(out.quality_tier.as_deref(), Some("standard"));
    assert_eq!(out.quality_score, Some(0.77));
    let identity = judge.identity();
    assert_eq!(identity.model, entry.model);
    assert_eq!(identity.prompt_version, PROMPT_VERSION);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["model"], json!(entry.model));
}

#[test]
fn build_judge_needs_secrets_before_any_call() {
    let catalog = catalog();
    let entry = catalog.resolve("baseten").unwrap();
    // No such variable set: construction fails without touching the
    // network (the stub URL is never even used).
    let mut entry = entry.clone();
    entry.auth_env = vec!["PENNYROUTER_TEST_MISSING_KEY".to_string()];
    std::env::remove_var("PENNYROUTER_TEST_MISSING_KEY");
    assert!(build_judge(&entry).is_err());
}

#[test]
fn selection_scopes_and_samples_deterministically() {
    let (_dir, db) = synced_db("catalog-select");
    let db = Catalog::open(&db).unwrap();
    let all = db.all_endpoints().unwrap();
    assert_eq!(all.len(), 4);

    let full = JudgePolicy::default();
    assert_eq!(select_endpoints(&all, &full).len(), 4);

    let scoped = JudgePolicy {
        only_models: vec!["openai/gpt-6.1-terra-pro".to_string()],
        ..JudgePolicy::default()
    };
    let subset = select_endpoints(&all, &scoped);
    assert_eq!(subset.len(), 3);
    assert!(subset
        .iter()
        .all(|ep| ep.model_id == "openai/gpt-6.1-terra-pro"));

    // Sampling is stable across rebuilds: same subset twice.
    let sampled = JudgePolicy {
        sample_rate: 0.5,
        ..JudgePolicy::default()
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

    let none = JudgePolicy {
        sample_rate: 0.0,
        ..JudgePolicy::default()
    };
    assert!(select_endpoints(&all, &none).is_empty());
}

#[test]
fn caps_fail_closed_before_any_write() {
    let catalog = catalog();
    let entry = catalog.resolve("baseten").unwrap();

    // Call cap breached.
    let capped = JudgePolicy {
        max_calls: Some(1),
        ..JudgePolicy::default()
    };
    assert!(plan_live_calls(4, entry, &capped).is_err());

    // Spend cap with known cost breached.
    let mut priced = entry.clone();
    priced.cost_per_call_usd = Some(0.01);
    let poor = JudgePolicy {
        max_spend_usd: Some(0.001),
        ..JudgePolicy::default()
    };
    assert!(plan_live_calls(4, &priced, &poor).is_err());

    // Spend cap with unknown cost fails rather than guessing.
    let blind = JudgePolicy {
        max_spend_usd: Some(10.0),
        ..JudgePolicy::default()
    };
    assert!(plan_live_calls(4, entry, &blind).is_err());

    // Fitting plan reports calls + spend for the log line.
    let rich = JudgePolicy {
        max_calls: Some(10),
        max_spend_usd: Some(10.0),
        ..JudgePolicy::default()
    };
    let plan = plan_live_calls(4, &priced, &rich).unwrap();
    assert_eq!(plan.calls, 4);
    assert!((plan.spend_usd.unwrap() - 0.04).abs() < 1e-12);
    let line = spend_line(&plan, &priced, &rich);
    assert!(line.contains("4 live calls"), "got: {line}");
    assert!(line.contains("0.0400"), "got: {line}");

    // End to end: a breached cap leaves the DB unjudged.
    let (_dir, db) = synced_db("catalog-capped");
    let db = Catalog::open(&db).unwrap();
    run_processing(&db, STAMP).unwrap();
    let all = db.all_endpoints().unwrap();
    let selected = select_endpoints(&all, &capped);
    assert!(plan_live_calls(selected.len(), entry, &capped).is_err());
    for ep in db.all_endpoints().unwrap() {
        assert_eq!(ep.judged_at, None, "cap breach must not write");
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
    let judge = BasetenJudge::new(&base_url, "k", "audit-model").unwrap();
    let all = db.all_endpoints().unwrap();
    let selected = select_endpoints(&all, &JudgePolicy::default());
    let summary = run_judging_selected(&db, &judge, &selected, STAMP).unwrap();
    assert_eq!(summary.judged, 4);
    assert_eq!(server.join().unwrap().len(), 4);
    // Every row carries the live model + prompt version.
    for ep in db.all_endpoints().unwrap() {
        assert_eq!(ep.judge_model.as_deref(), Some("audit-model"));
        assert_eq!(ep.judge_prompt.as_deref(), Some(PROMPT_VERSION));
    }
    assert_eq!(
        db.get_meta("judge_model").unwrap().as_deref(),
        Some("audit-model")
    );
}
