//! Phase 4: the Modal live judge posts OpenAI-shaped chat requests to
//! the deployed endpoint, authenticated by the Modal proxy-token pair. A
//! std-only stub server stands in, so no test calls the network; the
//! recorded fixture pins the output shape.

mod common;

use common::{fixtures_dir, synced_db};
use pennyrouter_catalog::{run_judging, Judge, ModalJudge, RecordedJudge};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

const STAMP: &str = "2026-10-28T00:00:00Z";

/// Canned `POST /chat/completions` response carrying one judgment.
fn chat_response(content: &str) -> String {
    let content = serde_json::to_string(content).unwrap();
    format!(
        r#"{{"id":"chatcmpl-test","object":"chat.completion","model":"test-model",
"choices":[{{"index":0,"message":{{"role":"assistant","content":{content}}},
"finish_reason":"stop"}}],
"usage":{{"prompt_tokens":10,"completion_tokens":9,"total_tokens":19}}}}"#
    )
}

/// Read one HTTP request: headers, then exactly Content-Length body bytes.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let header_len = loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "stub server: client closed mid-request");
        raw.extend_from_slice(&buf[..n]);
        if let Some(i) = find_header_end(&raw) {
            break i;
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
        assert!(n > 0, "stub server: client closed mid-body");
        raw.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&raw).to_string()
}

fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Serve `responses` to sequential requests; returns (base URL, requests).
fn stub_server(responses: Vec<String>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for body in &responses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
        requests
    });
    (base_url, handle)
}

fn request_body(request: &str) -> Value {
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    serde_json::from_str(body).unwrap()
}

#[test]
fn live_round_trip_posts_chat_request_with_proxy_auth() {
    let (base_url, server) = stub_server(vec![chat_response(
        r#"{"quality_tier": "standard", "quality_score": 0.77}"#,
    )]);
    let judge = ModalJudge::new(&base_url, "test-id", "test-secret", "test-model").unwrap();
    let out = judge
        .judge("m", "p", &json!({"tag": "t", "price": 1}))
        .unwrap();
    assert_eq!(out.quality_tier.as_deref(), Some("standard"));
    assert_eq!(out.quality_score, Some(0.77));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.starts_with("POST /chat/completions HTTP/1.1\r\n"));
    assert!(request.contains("\r\nmodal-key: test-id\r\n"));
    assert!(request.contains("\r\nmodal-secret: test-secret\r\n"));
    let body: Value = request_body(request);
    assert_eq!(body["model"], json!("test-model"));
    assert_eq!(body["temperature"], json!(0));
    assert_eq!(body["response_format"], json!({"type": "json_object"}));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], json!("system"));
    let user = messages[1]["content"].as_str().unwrap();
    assert!(user.contains('m'), "user prompt names the model: {user}");
    assert!(user.contains('p'), "user prompt names the provider: {user}");
}

#[test]
fn invalid_model_output_is_an_error() {
    let bad = [
        "oops",
        r#"{"quality_tier": "ultra"}"#,
        r#"{"quality_score": 2.0}"#,
    ];
    let responses = bad.iter().map(|c| chat_response(c)).collect();
    let (base_url, server) = stub_server(responses);
    let judge = ModalJudge::new(&base_url, "test-id", "test-secret", "test-model").unwrap();
    for content in bad {
        assert!(
            judge.judge("m", "p", &json!({})).is_err(),
            "should reject model output: {content}"
        );
    }
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn modal_recorded_fixture_yields_expected_values() {
    let (_dir, db) = synced_db("modal-recorded");
    let catalog = pennyrouter_catalog::Catalog::open(&db).unwrap();
    let judge = RecordedJudge::load(&fixtures_dir().join("judge-modal.json")).unwrap();
    let summary = run_judging(&catalog, &judge, STAMP).unwrap();
    assert_eq!(summary.judged, 4);
    assert_eq!(summary.matched, 2);

    let eps = catalog.endpoints_for("openai/gpt-6.1-terra-pro").unwrap();
    assert_eq!(eps.len(), 3);
    // Only the "openai/fast" offering matches (ORDER BY id: flex, standard, fast).
    assert_eq!(eps[0].quality_tier, None);
    assert_eq!(eps[1].quality_tier, None);
    assert_eq!(eps[2].quality_tier.as_deref(), Some("standard"));
    assert_eq!(eps[2].quality_score, Some(0.77));
    assert_eq!(eps[2].judged_at.as_deref(), Some(STAMP));

    let tencent = &catalog.endpoints_for("tencent/hy-mt2-1.8b").unwrap()[0];
    assert_eq!(tencent.quality_tier.as_deref(), Some("high"));
    assert_eq!(tencent.quality_score, Some(0.88));
}
