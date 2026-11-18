//! B19: live-score policy flags apply with or without `--scorers`.
//!
//! Runs the real `catalog-process` binary: a bare `--score-backend
//! baseten` must honor `--sample-rate`/`--max-calls` instead of
//! running a full uncapped live rebuild.

mod common;

use common::synced_db;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_catalog-process"))
}

#[test]
fn sample_rate_zero_scorers_nothing_live() {
    let (_dir, db) = synced_db("b19-sample");
    std::env::set_var("BASETEN_API_KEY", "probe-dummy");
    let out = Command::new(bin())
        .arg("--db")
        .arg(&db)
        .arg("--score-backend")
        .arg("baseten")
        .arg("--sample-rate")
        .arg("0")
        .output()
        .unwrap();
    std::env::remove_var("BASETEN_API_KEY");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "zero-sample run must succeed without network.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("scored 0 endpoints"),
        "expected a scored-0 line, got:\n{stdout}"
    );
}

#[test]
fn openrouter_backend_dispatches_without_network() {
    // Iter 30: bare `--score-backend openrouter` reads its model from
    // OPENROUTER_SCORE_MODEL and its key from the shared
    // RANKINGS_API_KEY. Zero-sample run proves dispatch + from_env
    // wiring without touching OpenRouter.
    let (_dir, db) = synced_db("openrouter-cli");
    std::env::set_var("OPENROUTER_SCORE_MODEL", "probe/model");
    std::env::set_var("RANKINGS_API_KEY", "probe-dummy");
    let out = Command::new(bin())
        .arg("--db")
        .arg(&db)
        .arg("--score-backend")
        .arg("openrouter")
        .arg("--sample-rate")
        .arg("0")
        .output()
        .unwrap();
    std::env::remove_var("OPENROUTER_SCORE_MODEL");
    std::env::remove_var("RANKINGS_API_KEY");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "zero-sample run must succeed without network.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("scored 0 endpoints"),
        "expected a scored-0 line, got:\n{stdout}"
    );
}

#[test]
fn max_calls_cap_fails_closed_live() {
    let (_dir, db) = synced_db("b19-caps");
    std::env::set_var("BASETEN_API_KEY", "probe-dummy");
    let out = Command::new(bin())
        .arg("--db")
        .arg(&db)
        .arg("--score-backend")
        .arg("baseten")
        .arg("--max-calls")
        .arg("1")
        .output()
        .unwrap();
    std::env::remove_var("BASETEN_API_KEY");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "cap breach must fail.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("over the max_calls cap"),
        "expected a cap-breach error, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn processing_only_run_reports_offline_scoring() {
    // Iter 32: a bare run (no --score flags) still reports the
    // automatic offline pass. Fixtures carry no AA indexes, so the
    // count is 0 — the line proves the pass ran.
    let (_dir, db) = synced_db("offline-line");
    let out = Command::new(bin()).arg("--db").arg(&db).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "processing-only run must succeed.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("scored 0 endpoints (offline)"),
        "expected an offline-scored line, got:\n{stdout}"
    );
}
