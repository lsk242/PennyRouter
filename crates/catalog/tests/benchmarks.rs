//! Intelligence backfill fetch: one authenticated call to the
//! benchmarks endpoint, parsed to permaslug scores. A std-only stub
//! server stands in for OpenRouter, so no test calls the network.

use pennyrouter_catalog::fetch_benchmarks;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn benchmarks_body() -> String {
    r#"{"data":[
{"source":"artificial-analysis","model_permaslug":"a/x","intelligence_index":71.4},
{"source":"artificial-analysis","model_permaslug":"b/y","intelligence_index":50},
{"source":"openrouter","model_permaslug":"c/z","intelligence_index":99}
],"meta":{"as_of":"2026-11-04T00:00:00.000Z"}}"#
        .to_string()
}

/// Serve exactly one GET, then return the request path and the
/// Authorization header seen.
fn stub_server() -> (String, thread::JoinHandle<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
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
        let auth = head
            .lines()
            .find_map(|line| line.strip_prefix("authorization: "))
            .unwrap_or("")
            .to_string();
        let body = benchmarks_body();
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).unwrap();
        stream.flush().unwrap();
        (path, auth)
    });
    (base_url, handle)
}

#[test]
fn fetch_pulls_intelligence_rows_with_auth() {
    let (base_url, server) = stub_server();
    let benchmarks = fetch_benchmarks(&base_url, "KEY").unwrap();
    assert_eq!(
        benchmarks.intelligence,
        vec![("a/x".to_string(), 71), ("b/y".to_string(), 50)]
    );
    assert_eq!(
        benchmarks.as_of.as_deref(),
        Some("2026-11-04T00:00:00.000Z")
    );
    let (path, auth) = server.join().unwrap();
    assert_eq!(
        path,
        "/api/v1/benchmarks?source=artificial-analysis&task_type=intelligence"
    );
    assert_eq!(auth, "Bearer KEY");
}

#[test]
fn fetch_fails_loud_on_http_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).unwrap();
        let body = r#"{"error":{"message":"slow down"}}"#;
        let reply = format!(
            "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).unwrap();
        stream.flush().unwrap();
    });
    let err = fetch_benchmarks(&base_url, "KEY").unwrap_err();
    assert!(err.to_string().contains("429"), "got: {err}");
    handle.join().unwrap();
}
