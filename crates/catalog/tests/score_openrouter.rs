//! Iter 30: the OpenRouter live scorer posts OpenAI-shaped chat requests
//! and reads scores back. A std-only stub server stands in for
//! OpenRouter, so no test calls the network. The scorer model comes
//! from `OPENROUTER_SCORE_MODEL` (fail-closed, nothing pinned) and
//! the key is the shared `RANKINGS_API_KEY`.

use pennyrouter_catalog::{OpenRouterScorer, Scorer};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// Canned `POST /chat/completions` response carrying one score.
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
fn live_round_trip_posts_chat_request_and_reads_score() {
    let (base_url, server) = stub_server(vec![chat_response(
        r#"{"quality_tier": "high", "quality_score": 0.9}"#,
    )]);
    let score = OpenRouterScorer::new(&base_url, "test-key", "test-model").unwrap();
    let out = score
        .score("m", "p", &json!({"tag": "t", "price": 1}))
        .unwrap();
    assert_eq!(out.quality_tier.as_deref(), Some("high"));
    assert_eq!(out.quality_score, Some(0.9));
    let identity = score.identity();
    assert_eq!(identity.model, "test-model");
    assert_eq!(identity.prompt_version, pennyrouter_catalog::PROMPT_VERSION);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.starts_with("POST /chat/completions HTTP/1.1\r\n"));
    assert!(request.contains("\r\nauthorization: Bearer test-key\r\n"));
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
    let score = OpenRouterScorer::new(&base_url, "test-key", "test-model").unwrap();
    for content in bad {
        // (No `unwrap_err`: `Score` has no `Debug`, like the scorer.)
        match score.score("m", "p", &json!({})) {
            Ok(_) => panic!("should reject model output: {content}"),
            Err(err) => assert!(
                err.to_string().contains("openrouter"),
                "error names the backend: {err}"
            ),
        }
    }
    assert_eq!(server.join().unwrap().len(), 3);
}

/// Save/restore guard: the fail-closed tests below mutate real
/// variable names, which must not leak into other tests or clobber
/// ambient developer values.
struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn take(names: &[&str]) -> Self {
        let saved = names
            .iter()
            .map(|name| (name.to_string(), std::env::var(name).ok()))
            .collect();
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// from_env failure text. (No `unwrap_err`: the scorer has no
/// `Debug` impl on purpose, so the key can never leak into one.)
fn from_env_err() -> String {
    OpenRouterScorer::from_env()
        .err()
        .expect("from_env must fail here")
        .to_string()
}

#[test]
fn from_env_fails_closed_without_model_or_key() {
    let _guard = EnvGuard::take(&["OPENROUTER_SCORE_MODEL", "RANKINGS_API_KEY"]);
    // Model missing or empty fails before the key is even read.
    std::env::remove_var("OPENROUTER_SCORE_MODEL");
    std::env::set_var("RANKINGS_API_KEY", "key");
    let err = from_env_err();
    assert!(
        err.contains("OPENROUTER_SCORE_MODEL"),
        "model miss names the variable: {err}"
    );
    std::env::set_var("OPENROUTER_SCORE_MODEL", "");
    let err = from_env_err();
    assert!(
        err.contains("OPENROUTER_SCORE_MODEL"),
        "empty model names the variable: {err}"
    );
    // Model set, key missing or empty fails on the key.
    std::env::set_var("OPENROUTER_SCORE_MODEL", "some/model");
    std::env::remove_var("RANKINGS_API_KEY");
    let err = from_env_err();
    assert!(
        err.contains("RANKINGS_API_KEY"),
        "key miss names the variable: {err}"
    );
    std::env::set_var("RANKINGS_API_KEY", "");
    let err = from_env_err();
    assert!(
        err.contains("RANKINGS_API_KEY"),
        "empty key names the variable: {err}"
    );
    // Both set builds, with the env model as its identity.
    std::env::set_var("RANKINGS_API_KEY", "key");
    let score = OpenRouterScorer::from_env().unwrap();
    assert_eq!(score.identity().model, "some/model");
}

#[test]
fn production_base_url_targets_openrouter_chat() {
    assert_eq!(OpenRouterScorer::BASE_URL, "https://openrouter.ai/api/v1");
}
