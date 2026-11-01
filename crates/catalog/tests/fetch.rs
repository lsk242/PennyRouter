//! Iter 18: live OpenRouter fetch pulls the models list plus one
//! endpoints body per model. A std-only stub server stands in for
//! OpenRouter, so no test calls the network.

use pennyrouter_catalog::fetch_openrouter;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn models_body() -> String {
    r#"{"data":[
{"id":"test/free-model:free","name":"Free","pricing":{"prompt":"0","completion":"0"}},
{"id":"test/paid-model","name":"Paid","pricing":{"prompt":"0.001","completion":"0.002"}}
]}"#
    .to_string()
}

fn endpoints_body(provider: &str, prompt: &str) -> String {
    format!(
        r#"{{"data":{{"endpoints":[{{"provider_name":"{provider}","pricing":{{"prompt":"{prompt}","completion":"0"}},"quantization":"bf16"}}]}}}}"#
    )
}

/// Serve GETs by path: the models list, per-model endpoints, 404 for
/// the paid model's endpoints when `fail_paid` is set. Handles exactly
/// the expected request count, then returns the request lines seen.
fn stub_server(fail_paid: bool) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        // Models list + one endpoints fetch per model id.
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let header_len = loop {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "stub server: client closed mid-request");
                raw.extend_from_slice(&buf[..n]);
                if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4) {
                    break i;
                }
            };
            let head = String::from_utf8_lossy(&raw[..header_len]).to_string();
            let path = head
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_string();
            requests.push(path.clone());
            let (status, body) = if path == "/v1/models" {
                ("200 OK", models_body())
            } else if path == "/v1/models/test/free-model:free/endpoints" {
                ("200 OK", endpoints_body("FreeProvider", "0"))
            } else if path == "/v1/models/test/paid-model/endpoints" {
                if fail_paid {
                    ("404 Not Found", r#"{"error":"no such model"}"#.to_string())
                } else {
                    ("200 OK", endpoints_body("PaidProvider", "0.001"))
                }
            } else {
                (
                    "404 Not Found",
                    r#"{"error":"unexpected path"}"#.to_string(),
                )
            };
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
        requests
    });
    (base_url, handle)
}

#[test]
fn fetch_pulls_models_and_endpoints() {
    let (base_url, server) = stub_server(false);
    let fetched = fetch_openrouter(&base_url).unwrap();
    assert!(fetched.skipped.is_empty());

    let root: serde_json::Value = serde_json::from_slice(&fetched.models).unwrap();
    let ids: Vec<&str> = root["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["test/free-model:free", "test/paid-model"]);

    let names: Vec<&str> = fetched
        .endpoints
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(names, vec!["test/free-model:free", "test/paid-model"]);
    let free_body: serde_json::Value = serde_json::from_slice(&fetched.endpoints[0].1).unwrap();
    assert_eq!(
        free_body["data"]["endpoints"][0]["provider_name"],
        serde_json::json!("FreeProvider")
    );

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.contains(&"/v1/models".to_string()));
    // Raw model ids ride in the path, slashes and colons unescaped.
    assert!(requests.contains(&"/v1/models/test/free-model:free/endpoints".to_string()));
    assert!(requests.contains(&"/v1/models/test/paid-model/endpoints".to_string()));
}

#[test]
fn fetch_skips_failed_endpoints() {
    let (base_url, server) = stub_server(true);
    let fetched = fetch_openrouter(&base_url).unwrap();
    assert_eq!(fetched.skipped, vec!["test/paid-model".to_string()]);
    assert_eq!(fetched.endpoints.len(), 1);
    assert_eq!(fetched.endpoints[0].0, "test/free-model:free");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
}

/// Iter 24: caller-nominated ids absent from the list are fetched
/// individually and join both the models array (sans nested
/// endpoints) and the endpoints pairs. Listed ids are not
/// refetched; failed extras land in `skipped`.
fn stub_server_extra() -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        // List + two listed endpoints + two extra attempts.
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let header_len = loop {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "stub server: client closed mid-request");
                raw.extend_from_slice(&buf[..n]);
                if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4) {
                    break i;
                }
            };
            let head = String::from_utf8_lossy(&raw[..header_len]).to_string();
            let path = head
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_string();
            requests.push(path.clone());
            let (status, body) = if path == "/v1/models" {
                ("200 OK", models_body())
            } else if path == "/v1/models/test/free-model:free/endpoints" {
                ("200 OK", endpoints_body("FreeProvider", "0"))
            } else if path == "/v1/models/test/paid-model/endpoints" {
                ("200 OK", endpoints_body("PaidProvider", "0.001"))
            } else if path == "/v1/models/test/unlisted:free/endpoints" {
                (
                    "200 OK",
                    r#"{"data":{"id":"test/unlisted:free","name":"Unlisted","architecture":{"output_modalities":["decisions"]},"endpoints":[{"provider_name":"UnlistedProvider","pricing":{"prompt":"0","completion":"0"},"quantization":"unknown"}]}}"#.to_string(),
                )
            } else {
                ("404 Not Found", r#"{"error":"no such model"}"#.to_string())
            };
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
        requests
    });
    (base_url, handle)
}

#[test]
fn fetch_merges_unlisted_extra_models() {
    use pennyrouter_catalog::fetch_openrouter_extra;

    let (base_url, server) = stub_server_extra();
    let fetched = fetch_openrouter_extra(
        &base_url,
        &[
            "test/paid-model".to_string(),
            "test/unlisted:free".to_string(),
            "test/missing".to_string(),
        ],
    )
    .unwrap();

    // Failed extra lands in skipped; the listed extra was not refetched.
    assert_eq!(fetched.skipped, vec!["test/missing".to_string()]);
    let root: serde_json::Value = serde_json::from_slice(&fetched.models).unwrap();
    let ids: Vec<&str> = root["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            "test/free-model:free",
            "test/paid-model",
            "test/unlisted:free"
        ]
    );
    // The joined model object carries architecture but no nested endpoints.
    let extra = &root["data"][2];
    assert_eq!(
        extra["architecture"]["output_modalities"],
        serde_json::json!(["decisions"])
    );
    assert!(extra.get("endpoints").is_none());
    let names: Vec<&str> = fetched
        .endpoints
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "test/free-model:free",
            "test/paid-model",
            "test/unlisted:free"
        ]
    );

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|p| *p == "/v1/models/test/paid-model/endpoints")
            .count(),
        1,
        "listed ids are fetched once, never as extras",
    );
}
