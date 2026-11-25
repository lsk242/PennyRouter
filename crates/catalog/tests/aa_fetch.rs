//! Iter 42: AA free API fetch walks the paginated model list with
//! `x-api-key`, parses headline indexes + versions + cost/task +
//! pricing, and stores per slug. A std-only stub server stands in
//! for the API, so no test calls the network.

use pennyrouter_catalog::{
    fetch_aa_models, read_aa_intel_version, read_aa_models, store_aa_models, Catalog,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn page(records: &str, page: u32, total: u32, more: bool) -> String {
    format!(
        r#"{{"tier":"free","intelligence_index_version":"4.3","pagination":{{"page":{page},"page_size":200,"total_pages":{total},"has_more":{more}}},"data":[{records}]}}"#
    )
}

fn record(slug: &str, intel: &str) -> String {
    format!(
        r#"{{"id":"u-{slug}","name":"{slug}","slug":"{slug}","evaluations":{{"artificial_analysis_intelligence_index":{intel}}},"artificial_analysis_intelligence_index_cost":{{"cost_per_task":{{"total_cost":0.01}}}},"pricing":{{"input":1.0,"output":4.0}}}}"#
    )
}

/// Serve `pages` GETs: page bodies in order, then 500s. Returns the
/// base URL, the request lines seen, and the join handle.
fn stub_server(
    pages: Vec<String>,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_clone = seen.clone();
    let handle = thread::spawn(move || {
        for page in pages {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let header_len = loop {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "stub server: client closed mid-request");
                raw.extend_from_slice(&buf[..n]);
                if let Some(i) = raw
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|i| i + 4)
                {
                    break i;
                }
            };
            let head = String::from_utf8_lossy(&raw[..header_len]).to_string();
            seen_clone.lock().unwrap().push(head);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                page.len(),
                page
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (base_url, seen, handle)
}

#[test]
fn fetch_walks_pages_with_api_key() {
    let (base, seen, handle) = stub_server(vec![
        page(&record("alpha", "71.4"), 1, 2, true),
        page(&record("beta", "52"), 2, 2, false),
    ]);
    let aa = fetch_aa_models(&base, "test-key").unwrap();
    handle.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].contains("GET /api/v2/language/models/free?page=1"));
    assert!(seen[1].contains("GET /api/v2/language/models/free?page=2"));
    for head in seen.iter() {
        assert!(
            head.to_lowercase().contains("x-api-key: test-key"),
            "key travels as x-api-key, got: {head}"
        );
    }
    assert_eq!(aa.models.len(), 2);
    assert_eq!(aa.models[0].slug, "alpha");
    assert_eq!(aa.models[0].intelligence, Some(71));
    assert_eq!(aa.models[0].cost_per_task, Some(0.01));
    assert_eq!(aa.models[1].slug, "beta");
    assert_eq!(aa.intel_version.as_deref(), Some("4.3"));
    assert!(aa.as_of.is_some(), "citation timestamp present");
}

#[test]
fn fetch_stops_without_has_more() {
    let (base, seen, handle) = stub_server(vec![page(&record("solo", "60"), 1, 1, false)]);
    let aa = fetch_aa_models(&base, "k").unwrap();
    handle.join().unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(aa.models.len(), 1);
}

#[test]
fn store_read_round_trip_and_keyless_empty() {
    let dir = std::env::temp_dir().join(format!("aa-fetch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let catalog = Catalog::open(&dir.join("t.db")).unwrap();
    catalog.init().unwrap();
    // Keyless syncs store nothing: reads come back empty.
    assert!(read_aa_models(&catalog).is_empty());
    assert_eq!(read_aa_intel_version(&catalog), None);
    let (base, _, handle) = stub_server(vec![page(&record("alpha", "71.4"), 1, 1, false)]);
    let aa = fetch_aa_models(&base, "k").unwrap();
    handle.join().unwrap();
    store_aa_models(&catalog, &aa).unwrap();
    let back = read_aa_models(&catalog);
    assert_eq!(back.len(), 1);
    assert_eq!(back["alpha"].intelligence, Some(71));
    assert_eq!(back["alpha"].price_input_m, Some(1.0));
    assert_eq!(back["alpha"].price_output_m, Some(4.0));
    assert_eq!(read_aa_intel_version(&catalog).as_deref(), Some("4.3"));
    assert!(catalog.get_meta("aa_as_of").unwrap().is_some());
}
