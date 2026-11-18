//! B17/Q11-A: the sync cycle fails (non-zero exit, no publish)
//! instead of publishing a gutted catalog: zero models synced, or
//! more than half the endpoint inputs skipped.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_catalog-sync"))
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn catalog_dbs(out: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(out)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("catalog-") && n.ends_with(".db"))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ghost_endpoints(id: &str) -> String {
    serde_json::json!({"data": {"id": id, "endpoints": []}}).to_string()
}

fn run_sync(models: &str, endpoints: &[(&str, String)], out: &Path) -> std::process::Output {
    let dir = out.join("in");
    write(&dir.join("models.json"), models);
    std::fs::create_dir_all(dir.join("endpoints")).unwrap();
    for (name, body) in endpoints {
        write(&dir.join("endpoints").join(name), body);
    }
    Command::new(bin())
        .arg("--models")
        .arg(dir.join("models.json"))
        .arg("--endpoints-dir")
        .arg(dir.join("endpoints"))
        .arg("--out")
        .arg(out)
        .output()
        .unwrap()
}

fn assert_refused(out: &Path, output: &std::process::Output) {
    assert!(!output.status.success(), "gutted sync must fail the cycle");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refusing to publish"),
        "expected a refusal, got:\n{stderr}"
    );
    assert!(
        out.join("catalog-old.db").exists(),
        "last-good artifact keeps serving"
    );
    assert!(
        catalog_dbs(out)
            .iter()
            .all(|p| p.file_name().unwrap() == "catalog-old.db"),
        "no new catalog published"
    );
    assert!(
        !out.join(".catalog-work.db").exists(),
        "refused cycle leaves no work DB behind"
    );
}

#[test]
fn zero_models_refuses_to_publish() {
    let out = common::scratch("b17-zero");
    write(&out.join("catalog-old.db"), "old");
    let output = run_sync(r#"{"data": []}"#, &[], &out);
    assert_refused(&out, &output);
}

#[test]
fn majority_skipped_refuses_to_publish() {
    let out = common::scratch("b17-skipped");
    write(&out.join("catalog-old.db"), "old");
    // 1 listed model, 3 endpoint inputs, 2 for unlisted models.
    let models = r#"{"data": [{"id": "a/m", "name": "a/m"}]}"#;
    let endpoints = [
        ("a__m.json", ghost_endpoints("a/m")),
        ("ghost__1.json", ghost_endpoints("ghost/1")),
        ("ghost__2.json", ghost_endpoints("ghost/2")),
    ];
    let refs: Vec<(&str, String)> = endpoints.to_vec();
    let output = run_sync(models, &refs, &out);
    assert_refused(&out, &output);
}

#[test]
fn normal_fixture_still_publishes() {
    let out = common::scratch("b17-ok");
    let output = Command::new(bin())
        .arg("--models")
        .arg(common::fixtures_dir().join("models.json"))
        .arg("--endpoints-dir")
        .arg(common::fixtures_dir().join("endpoints"))
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "normal fixture must publish:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(catalog_dbs(&out).len(), 1, "one catalog published");
}
