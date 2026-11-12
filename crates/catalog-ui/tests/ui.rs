//! Iter 17: catalog-ui browser over a published artifact dir.
//!
//! Builds a real artifact dir with `publish`/`publish_ranking`, then
//! drives the actual binary: refusal without `CATALOG_UI=1`, and page
//! content per route with it set.

use pennyrouter_catalog::{
    publish, publish_ranking, Catalog, ClassRanking, EndpointIndex, EndpointRank, EndpointRow,
    EndpointScore, EndpointSpeed, ModelIndex, ModelRow, RankEntry, RankingDocument,
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
            scored_at: Some("t".to_string()),
            score_model: Some("recorded".to_string()),
            score_prompt: Some("recorded".to_string()),
            price_override_tokens: None,
        })
        .unwrap();
    catalog
        .insert_model(&ModelRow {
            model_id: "a&b/c".to_string(),
            name: "a&b/c".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "a&b/c".to_string(),
            provider_name: "AmpProvider".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(4.4e-8),
            price_completion: Some(1.25e-7),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: Some("fp16".to_string()),
            promo_discount: None,
            processed_at: Some("t".to_string()),
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
    let amp = catalog.endpoints_for("a&b/c").unwrap();
    assert_eq!(amp.len(), 1);
    catalog
        .update_endpoint_index(
            amp[0].id,
            &EndpointIndex {
                price_prompt: Some(4.4e-8),
                price_completion: Some(1.25e-7),
                price_cache_read: None,
                price_cache_write: None,
                precision_class: Some("unknown".to_string()),
                promo_discount: None,
                processed_at: "t".to_string(),
                supports_tools: None,
                supports_decisions: None,
                price_override_tokens: None,
            },
        )
        .unwrap();
    let ep = catalog.endpoints_for("m/ui").unwrap();
    assert_eq!(ep.len(), 1);
    catalog
        .update_endpoint_score(
            ep[0].id,
            &EndpointScore {
                quality_tier: Some("high".to_string()),
                quality_score: Some(0.9),
                scored_at: "t".to_string(),
                score_model: Some("recorded".to_string()),
                score_prompt: Some("recorded".to_string()),
            },
        )
        .unwrap();
    catalog.set_meta("score_backend", "recorded").unwrap();

    // Iter 34: a free, scored offering so the free tab and the score
    // breakdown have something real to render.
    catalog
        .insert_model(&ModelRow {
            model_id: "free/model".to_string(),
            name: "free/model".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .update_model_index(
            "free/model",
            &ModelIndex {
                aa_intelligence_index: Some(70),
                aa_coding_index: Some(80),
                aa_agentic_index: None,
                aa_cost_per_task: None,
            },
        )
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "free/model".to_string(),
            provider_name: "FreeProvider".to_string(),
            payload: r#"{"tag":"free/fp16"}"#.to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(0.0),
            price_completion: Some(0.0),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: Some("fp16".to_string()),
            promo_discount: None,
            processed_at: Some("t".to_string()),
            supports_tools: None,
            supports_decisions: None,
            quality_tier: Some("high".to_string()),
            quality_score: Some(0.9),
            scored_at: Some("t".to_string()),
            score_model: Some("recorded".to_string()),
            score_prompt: Some("recorded".to_string()),
            price_override_tokens: None,
        })
        .unwrap();
    let free = &catalog.endpoints_for("free/model").unwrap()[0];
    catalog
        .update_endpoint_index(
            free.id,
            &EndpointIndex {
                price_prompt: Some(0.0),
                price_completion: Some(0.0),
                price_cache_read: None,
                price_cache_write: None,
                precision_class: Some("fp16".to_string()),
                promo_discount: None,
                processed_at: "t".to_string(),
                supports_tools: None,
                supports_decisions: None,
                price_override_tokens: None,
            },
        )
        .unwrap();
    catalog
        .update_endpoint_speed(
            free.id,
            &EndpointSpeed {
                latency_ms: Some(300.0),
                throughput_tps: Some(120.0),
                uptime_pct: Some(99.5),
            },
        )
        .unwrap();
    catalog
        .update_endpoint_rank(
            free.id,
            &EndpointRank {
                rank_score: Some(80.0),
                rank_quality: Some(80.0),
                rank_efficiency: Some(1.0),
                rank_popularity: Some(1.0),
                rank_basis: Some("coding".to_string()),
                rank_ruleset: Some("rank-v1".to_string()),
                ranked_at: Some("t".to_string()),
            },
        )
        .unwrap();
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
            score: vec![],
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
fn bind_flag_selects_listen_address() {
    let out = artifact_dir("bind");
    let mut child = Command::new(bin())
        .arg("--dir")
        .arg(&out)
        .arg("--bind")
        .arg("127.0.0.1")
        .arg("--port")
        .arg("0")
        .env("CATALOG_UI", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let first = lines.next().expect("address line").expect("readable");
    assert!(
        first.starts_with("catalog-ui on http://127.0.0.1:"),
        "binds the requested address, got: {first}"
    );
    let addr = first
        .strip_prefix("catalog-ui on http://")
        .expect("address format")
        .to_string();
    let server = Server { child, addr };
    let index = get(&server, "/");
    assert!(index.contains("200 OK"), "got: {index}");
}

#[test]
fn invalid_bind_rejected() {
    let dir = artifact_dir("bad-bind");
    let out = Command::new(bin())
        .arg("--dir")
        .arg(&dir)
        .arg("--bind")
        .arg("not-an-ip")
        .env("CATALOG_UI", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--bind needs an IP"), "got: {stderr}");
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
    assert!(meta.contains("score_backend"));

    let log_page = get(&server, "/log");
    assert!(log_page.contains("line two"));
}

#[test]
fn model_link_round_trips_special_chars_and_prices_render() {
    let out = artifact_dir("encode");
    let log = out.join("pipe.log");
    let server = spawn(&out, &log);

    let models = get(&server, "/models");
    assert!(
        models.contains("/endpoints?model=a%26b%2Fc"),
        "id is percent-encoded in the href, got: {models}"
    );
    let filtered = get(&server, "/endpoints?model=a%26b%2Fc");
    assert!(
        filtered.contains("AmpProvider"),
        "decoded filter matches the row, got: {filtered}"
    );
    assert!(
        !filtered.contains("UiProvider"),
        "filter hides other models, got: {filtered}"
    );
    assert!(
        filtered.contains("$/M"),
        "prices labeled per million, got: {filtered}"
    );
    assert!(
        !filtered.contains("$/tok"),
        "no per-token prices shown, got: {filtered}"
    );
    assert!(
        filtered.contains("0.044"),
        "tiny price renders non-zero $/M, got: {filtered}"
    );
    assert!(
        !filtered.contains("0.0000"),
        "tiny price never looks free, got: {filtered}"
    );
    assert!(
        !filtered.contains("0.0440"),
        "no trailing zero decimals, got: {filtered}"
    );
    assert!(
        filtered.contains("0.125"),
        "normal prices keep needed precision, got: {filtered}"
    );
    assert!(
        !filtered.contains("0.1250"),
        "no trailing zero decimals, got: {filtered}"
    );
    assert!(
        !filtered.contains(">unknown<"),
        "stored class unknown renders as -, got: {filtered}"
    );
}

/// Iter 34: the home page is an overview (coverage, gaps, the formula,
/// a worked example) and the models page splits free from tiers.
#[test]
fn overview_explains_the_score_and_models_tabs_split_free_from_tiers() {
    let out = artifact_dir("score-ui");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    let index = get(&server, "/");
    assert!(index.contains("200 OK"), "got: {index}");
    assert!(
        index.contains("How the rank score is calculated"),
        "formula section present"
    );
    assert!(
        index.contains("rank_score = quality × efficiency × popularity"),
        "formula spelled out"
    );
    assert!(index.contains("Worked example"), "worked example present");
    assert!(
        index.contains("models with no AA index"),
        "coverage counts present"
    );
    assert!(index.contains("Data quality"), "gap warnings present");
    assert!(
        index.contains("coding → intelligence → agentic"),
        "basis chain documented"
    );

    let free = get(&server, "/models?tab=free");
    assert!(free.contains("free/model"), "free offering listed");
    assert!(
        !free.contains("UiProvider"),
        "paid offerings stay out of the free tab, got: {free}"
    );
    assert!(free.contains("80.0"), "score rendered");
    assert!(free.contains("calc"), "per-row breakdown available");
    assert!(
        free.contains("efficiency = 0.01 / (0.000000 + 0.01) = 1.0000"),
        "free efficiency shown as full credit, got: {free}"
    );
    assert!(free.contains("120 tok/s"), "speed shown as a tie-breaker");

    let tiers = get(&server, "/models?tab=tiers");
    assert!(tiers.contains("UiProvider"), "paid offering listed");
    assert!(
        !tiers.contains("free/model"),
        "free offerings stay out of the tiers tab, got: {tiers}"
    );
    assert!(
        tiers.contains("no score: no price"),
        "unpriced offering explains why it has no score"
    );
}
