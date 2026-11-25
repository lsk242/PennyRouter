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
            quantization: None,
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
            quantization: None,
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
                quantization: None,
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
            quantization: None,
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
                quantization: None,
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
                rank_cost_task: None,
            },
        )
        .unwrap();

    // A measured offering: activity + stored task cost set.
    catalog
        .insert_model(&ModelRow {
            model_id: "v2/model".to_string(),
            name: "v2/model".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .update_model_index(
            "v2/model",
            &ModelIndex {
                aa_intelligence_index: None,
                aa_coding_index: Some(80),
                aa_agentic_index: None,
                aa_cost_per_task: None,
            },
        )
        .unwrap();
    catalog
        .update_model_activity("v2/model", Some(0.99), Some(1.0))
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "v2/model".to_string(),
            provider_name: "V2Provider".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(1.0),
            price_completion: Some(2.0),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: Some("fp16".to_string()),
            quantization: None,
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
    let v2 = &catalog.endpoints_for("v2/model").unwrap()[0];
    catalog
        .update_endpoint_index(
            v2.id,
            &EndpointIndex {
                price_prompt: Some(1.0),
                price_completion: Some(2.0),
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
    catalog
        .update_endpoint_speed(
            v2.id,
            &EndpointSpeed {
                latency_ms: Some(300.0),
                throughput_tps: Some(120.0),
                uptime_pct: Some(99.5),
            },
        )
        .unwrap();
    catalog
        .update_endpoint_rank(
            v2.id,
            &EndpointRank {
                rank_cost_task: Some(0.01),
            },
        )
        .unwrap();

    // A priced batch-variant offering: without the batch split it
    // would land in the tiers tab.
    catalog
        .insert_model(&ModelRow {
            model_id: "paid/model:batch".to_string(),
            name: "paid/model:batch".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "paid/model:batch".to_string(),
            provider_name: "BatchProvider".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(1.0),
            price_completion: Some(2.0),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: Some("fp16".to_string()),
            quantization: None,
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
    // A measured non-text offering: hidden from every tab.
    catalog
        .insert_model(&ModelRow {
            model_id: "m/audio".to_string(),
            name: "m/audio".to_string(),
            source: "test".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
        })
        .unwrap();
    catalog
        .insert_endpoint(&EndpointRow {
            id: 0,
            model_id: "m/audio".to_string(),
            provider_name: "AudioProvider".to_string(),
            payload: "{}".to_string(),
            fetched_at: "t".to_string(),
            price_prompt: Some(1e-9),
            price_completion: Some(1e-9),
            price_cache_read: None,
            price_cache_write: None,
            precision_class: Some("fp16".to_string()),
            quantization: None,
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
    catalog
        .update_model_modalities("m/audio", Some(&["text".to_string(), "audio".to_string()]))
        .unwrap();
    // Usage pools + fetch health: two bracket members, three
    // top-20 members.
    catalog
        .set_meta(
            "bracket",
            r#"{"ruleset":"rank-v2","members":["top/alpha","top/beta"]}"#,
        )
        .unwrap();
    catalog
        .set_meta(
            "agent_top20",
            r#"{"ruleset":"rank-v2","top_n":20,"members":["a","b","c"]}"#,
        )
        .unwrap();
    catalog
        .set_meta("aa_as_of", "2026-11-24T10:00:00Z")
        .unwrap();
    catalog.set_meta("aa_intel_version", "5").unwrap();
    catalog
        .set_meta("popular_as_of", "2026-11-24T09:00:00Z")
        .unwrap();
    catalog
        .set_meta(
            "fetch_health",
            r#"{"aa":"2026-11-24T10:00:00Z","popularity":"2026-11-24T09:30:00Z"}"#,
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

fn post(server: &Server, path: &str) -> String {
    let mut stream = TcpStream::connect(&server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nhost: x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
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
fn serves_runs_models_meta_and_log() {
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
    assert!(
        ranking.contains("404"),
        "ranking page removed, got: {ranking}"
    );

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
fn overview_shows_pools_and_models_tabs_split_free_from_tiers() {
    let out = artifact_dir("pools-ui");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    let index = get(&server, "/");
    assert!(index.contains("200 OK"), "got: {index}");
    assert!(index.contains("Usage pools"), "pools section present");
    assert!(
        index.contains("models with no AA index"),
        "coverage counts present"
    );
    assert!(index.contains("Data quality"), "gap warnings present");
    assert!(
        index.contains("openrouter/pair/low/mintel/20"),
        "mintel example tied to the intelligence index, got: {index}"
    );
    assert!(
        !index.contains("rank_score"),
        "no score formula anywhere, got: {index}"
    );

    let free = get(&server, "/models?tab=free");
    assert!(free.contains("free/model"), "free offering listed");
    assert!(
        !free.contains("UiProvider"),
        "paid offerings stay out of the free tab, got: {free}"
    );
    assert!(free.contains("<th>usage</th>"), "usage column present");
    assert!(
        !free.contains("calc-"),
        "no breakdown rows anymore, got: {free}"
    );

    let tiers = get(&server, "/models?tab=tiers");
    assert!(tiers.contains("UiProvider"), "paid offering listed");
    assert!(
        !tiers.contains("free/model"),
        "free offerings stay out of the tiers tab, got: {tiers}"
    );
    // Usage-first ordering: the measured v2 row sorts above the
    // unmeasured ones, which fall back to cheapest-first.
    let v2 = tiers.find("v2/model").unwrap();
    let amp = tiers.find("AmpProvider").unwrap();
    let ui = tiers.find("UiProvider").unwrap();
    assert!(v2 < amp && amp < ui, "usage then price, got: {tiers}");
    assert!(tiers.contains("0.990"), "usage level rendered");
    assert!(
        tiers.contains("$0.01000"),
        "stored cost/task rendered, got: {tiers}"
    );
}

/// Iter 35: the overview carries a force re-sync button; pressing it
/// drops a trigger file the sync loop picks up on its next poll.
#[test]
fn resync_button_requests_a_sync_cycle() {
    let out = artifact_dir("resync");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    let index = get(&server, "/");
    assert!(index.contains("200 OK"), "got: {index}");
    assert!(
        index.contains("action=\"/resync\""),
        "overview carries the re-sync form, got: {index}"
    );
    assert!(
        index.contains("Force re-sync"),
        "button is labeled, got: {index}"
    );
    assert!(
        !index.contains("Re-sync pending"),
        "no pending banner before any request, got: {index}"
    );

    let posted = post(&server, "/resync");
    assert!(posted.contains("303"), "redirects back, got: {posted}");
    assert!(
        posted
            .to_lowercase()
            .contains("location: /?resync=requested"),
        "redirects to the overview banner, got: {posted}"
    );
    assert!(
        out.join(".resync-requested").is_file(),
        "trigger file lands in the artifact dir"
    );

    let confirmed = get(&server, "/?resync=requested");
    assert!(
        confirmed.contains("Re-sync requested"),
        "confirmation banner shown, got: {confirmed}"
    );
    let overview = get(&server, "/");
    assert!(
        overview.contains("Re-sync pending"),
        "pending banner while the trigger waits, got: {overview}"
    );
}

/// Batch variants (`:batch` suffix) live in their own tab, out of the
/// interactive free/tiers views; the rank number opens the
/// calculation in a row under the model instead of a side column.
#[test]
fn batch_tab_lists_batch_models() {
    let out = artifact_dir("batch-tab");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    let batch = get(&server, "/models?tab=batch");
    assert!(batch.contains("200 OK"), "got: {batch}");
    assert!(
        batch.contains("paid/model:batch"),
        "batch offering listed, got: {batch}"
    );
    assert!(
        !batch.contains("UiProvider"),
        "interactive offerings stay out of the batch tab, got: {batch}"
    );

    let tiers = get(&server, "/models?tab=tiers");
    assert!(
        !tiers.contains(":batch"),
        "batch variants stay out of the tiers tab, got: {tiers}"
    );
    let free = get(&server, "/models?tab=free");
    assert!(
        !free.contains(":batch"),
        "batch variants stay out of the free tab, got: {free}"
    );

    assert!(
        !batch.contains("calc-"),
        "no breakdown rows anymore, got: {batch}"
    );
    assert!(
        batch.contains("<th>usage</th>"),
        "usage column present, got: {batch}"
    );
    assert!(
        batch.contains(".wrap{width:100%;margin:0}"),
        "layout uses the full screen width, got: {batch}"
    );
}

/// Iter 56: measured rows render their usage level and stored
/// task cost; unmeasured cells stay `-`.
#[test]
fn measured_rows_render_usage_and_task_cost() {
    let out = artifact_dir("usage-cols");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    let tiers = get(&server, "/models?tab=tiers");
    assert!(tiers.contains("200 OK"), "got: {tiers}");
    assert!(tiers.contains("v2/model"), "v2 row listed");
    assert!(tiers.contains("0.990"), "usage level rendered");
    assert!(
        tiers.contains("$0.01000"),
        "stored task cost rendered, got: {tiers}"
    );
    assert!(
        tiers.contains("https://openrouter.ai/v2/model"),
        "model page link present, got: {tiers}"
    );
    assert!(
        !tiers.contains("AudioProvider"),
        "measured non-text stays out of the browser, got: {tiers}"
    );
    assert!(
        !tiers.contains("rank_score"),
        "no score arithmetic, got: {tiers}"
    );
}
/// Iter 56: overview shows usage-pool counts and per-source fetch
/// health from the artifact metas.
#[test]
fn overview_shows_pool_counts_and_fetch_health() {
    let out = artifact_dir("pools-overview");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    let index = get(&server, "/");
    assert!(index.contains("Usage pools"), "pools section");
    assert!(
        index.contains("<td>2</td><td>3</td><td>2026-11-24T09:00:00Z</td>"),
        "2 bracket, 3 top-20, citation"
    );
    assert!(index.contains("Fetch health"), "health section");
    assert!(index.contains("Artificial Analysis"), "AA row");
    assert!(index.contains("2026-11-24T10:00:00Z"), "AA citation");
    assert!(index.contains("OpenRouter Data API"), "OR row");
    assert!(index.contains("2026-11-24T09:00:00Z"), "OR citation");
    assert!(index.contains("2026-11-24T09:30:00Z"), "OR last ok");
}

/// Bare published artifact: no scores, no rollup, no health.
fn bare_artifact_dir(test: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("catalog-ui-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let work = dir.join("work.db");
    let catalog = Catalog::open(&work).unwrap();
    catalog.init().unwrap();
    drop(catalog);
    let out = dir.join("out");
    publish(&work, &out, "test", 1, 1).unwrap();
    out
}

/// Iter 56: unknown tabs fall back to tiers; a bare artifact
/// shows empty pools and never-fetched health.
#[test]
fn unknown_tab_falls_back_to_tiers() {
    let out = artifact_dir("tab-fallback");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    for tab in ["top", "nope"] {
        let body = get(&server, &format!("/models?tab={tab}"));
        assert!(body.contains("200 OK"), "got: {body}");
        assert!(
            body.contains("UiProvider"),
            "{tab} falls back to tiers, got: {body}"
        );
    }

    let out = bare_artifact_dir("pools-empty");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    let index = get(&server, "/");
    assert!(index.contains("Usage pools"), "pools section");
    assert!(
        index.contains("<td>0</td><td>0</td><td>-</td>"),
        "empty pools read zero"
    );
    assert!(index.contains("never"), "health reads never fetched");
    assert!(!index.contains("NaN"), "no NaN");
}

#[test]
fn favicon_serves_the_shared_penny_svg() {
    let out = artifact_dir("favicon");
    let log = out.join("pipe.log");
    std::fs::write(&log, "").unwrap();
    let server = spawn(&out, &log);

    let icon = get(&server, "/favicon.ico");
    assert!(icon.contains("200 OK"), "got: {icon}");
    assert!(
        icon.contains("image/svg+xml"),
        "svg content type, got: {icon}"
    );
    assert!(
        icon.contains(pennyrouter_catalog::FAVICON_SVG),
        "identical bytes to the gateway icon, got: {icon}"
    );

    let index = get(&server, "/");
    assert!(
        index.contains(r#"<link rel="icon" href="/favicon.ico""#),
        "overview links the icon, got: {index}"
    );
}
