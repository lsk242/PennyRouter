//! Iter 17: catalog-ui browser over a published artifact dir.
//!
//! Builds a real artifact dir with `publish`/`publish_ranking`, then
//! drives the actual binary: refusal without `CATALOG_UI=1`, and page
//! content per route with it set.

use pennyrouter_catalog::{
    publish, publish_ranking, Catalog, ClassRanking, EndpointJudgment, EndpointRow, ModelRow,
    RankEntry, RankingDocument,
};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn artifact_dir(test: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("catalog-ui-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let work = dir.join("work.db");
    let catalog = Catalog::open(&work).unwrap();
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
            judged_at: Some("t".to_string()),
            judge_model: Some("recorded".to_string()),
            judge_prompt: Some("recorded".to_string()),
        })
        .unwrap();
    let ep = catalog.endpoints_for("m/ui").unwrap();
    assert_eq!(ep.len(), 1);
    catalog
        .update_endpoint_judgment(
            ep[0].id,
            &EndpointJudgment {
                quality_tier: Some("high".to_string()),
                quality_score: Some(0.9),
                judged_at: "t".to_string(),
                judge_model: Some("recorded".to_string()),
                judge_prompt: Some("recorded".to_string()),
            },
        )
        .unwrap();
    catalog.set_meta("judge_backend", "recorded").unwrap();
    drop(catalog);
    let out = dir.join("out");
    let (_, _, manifest) = publish(&work, &out, "test", 1, 1).unwrap();
    let mut models = BTreeMap::new();
    models.insert(
        "m/ui".to_string(),
        vec![ClassRanking {
            class: Some("fp16".to_string()),
            default: vec![RankEntry {
                provider: "UiProvider".to_string(),
                tag: None,
            }],
            price: vec![],
            quality: vec![],
        }],
    );
    let doc = RankingDocument {
        created_at: "2026-10-29T00:00:00Z".to_string(),
        catalog_version: Some(manifest.version.clone()),
        models,
        free_offers: BTreeMap::new(),
    };
    publish_ranking(&doc, &out).unwrap();
    out
}

fn bin() -> std::path::PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_catalog-ui"))
}

#[test]
fn refuses_without_flag() {
    let dir = artifact_dir("refuse");
    let out = Command::new(bin())
        .arg("--dir")
        .arg(&dir)
        .env_remove("CATALOG_UI")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("CATALOG_UI=1"), "got: {stderr}");
}

struct Server {
    child: Child,
    addr: String,
}

fn spawn(dir: &std::path::Path, log: &std::path::Path) -> Server {
    let mut child = Command::new(bin())
        .arg("--dir")
        .arg(dir)
        .arg("--port")
        .arg("0")
        .arg("--log-file")
        .arg(log)
        .env("CATALOG_UI", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let first = lines
        .next()
        .expect("ui prints its address")
        .expect("readable");
    let addr = first
        .strip_prefix("catalog-ui on http://")
        .expect("address line")
        .to_string();
    Server { child, addr }
}

fn get(server: &Server, path: &str) -> String {
    let mut stream = TcpStream::connect(&server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n"
    )
    .unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).unwrap();
    body
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn port_env_selects_listen_port() {
    let out = artifact_dir("port-env");
    let mut child = Command::new(bin())
        .arg("--dir")
        .arg(&out)
        .env("CATALOG_UI", "1")
        .env("PORT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let first = lines.next().expect("address line").expect("readable");
    let addr = first
        .strip_prefix("catalog-ui on http://")
        .expect("address format")
        .to_string();
    let server = Server { child, addr };
    let index = get(&server, "/");
    assert!(index.contains("200 OK"), "got: {index}");
    assert!(index.contains("catalog-"));
}

#[test]
fn serves_runs_models_ranking_meta_and_log() {
    let out = artifact_dir("serve");
    let log = out.join("pipe.log");
    std::fs::write(&log, "line one\nline two\n").unwrap();
    let server = spawn(&out, &log);

    let index = get(&server, "/");
    assert!(index.contains("200 OK"), "got: {index}");
    assert!(index.contains("catalog-"), "run history lists the publish");
    assert!(index.contains("text/html"));
    assert!(index.contains("0.11.0"), "crate version shown");
    assert!(index.contains("build"), "build hash labeled");
    assert!(index.contains("artifact dir"), "config block present");

    let models = get(&server, "/models");
    assert!(models.contains("m/ui"));

    let endpoints = get(&server, "/endpoints?model=m/ui");
    assert!(endpoints.contains("UiProvider"));
    assert!(endpoints.contains("recorded / recorded"));

    let ranking = get(&server, "/ranking");
    assert!(ranking.contains("UiProvider"));

    let meta = get(&server, "/meta");
    assert!(meta.contains("judge_backend"));

    let log_page = get(&server, "/log");
    assert!(log_page.contains("line two"));
}
