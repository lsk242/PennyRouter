//! catalog-sync: gather provider/model data into a versioned catalog DB.
//!
//! Phase 1 is fixture-driven: `--models` points at a `GET /models`
//! response body and `--endpoints-dir` holds one `GET
//! /models/{id}/endpoints` body per model, named `<id with '/' as
//! '__'>.json`. Live fetching plugs in later.

use pennyrouter_catalog::{load_endpoints_dir, now_rfc3339, publish, run_fixture_sync, Catalog};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: catalog-sync --models <models.json> --endpoints-dir <dir> --out <dir>"
}

struct Args {
    models: PathBuf,
    endpoints_dir: PathBuf,
    out: PathBuf,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut models = None;
    let mut endpoints_dir = None;
    let mut out = None;
    let mut it = argv.iter().skip(1);
    while let Some(arg) = it.next() {
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        let mut value = |flag: &str| -> Result<String, String> {
            if let Some(v) = &inline {
                Ok(v.clone())
            } else {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            }
        };
        match key {
            "--models" => models = Some(PathBuf::from(value("--models")?)),
            "--endpoints-dir" => endpoints_dir = Some(PathBuf::from(value("--endpoints-dir")?)),
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--help" | "-h" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(Args {
        models: models.ok_or_else(|| "--models is required".to_string())?,
        endpoints_dir: endpoints_dir.ok_or_else(|| "--endpoints-dir is required".to_string())?,
        out: out.ok_or_else(|| "--out is required".to_string())?,
    })
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().collect();
    let args = parse_args(&argv).map_err(|e| {
        if e == "help" {
            usage().to_string()
        } else {
            format!("{e}\n{}", usage())
        }
    })?;

    let models_json =
        fs::read(&args.models).map_err(|e| format!("read {}: {e}", args.models.display()))?;
    let endpoints = load_endpoints_dir(&args.endpoints_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&args.out).map_err(|e| format!("create {}: {e}", args.out.display()))?;

    let work_db = args.out.join(".catalog-work.db");
    let _ = fs::remove_file(&work_db);
    let fetched_at = now_rfc3339().map_err(|e| e.to_string())?;
    let catalog = Catalog::open(&work_db).map_err(|e| e.to_string())?;
    catalog.init().map_err(|e| e.to_string())?;
    let summary = run_fixture_sync(
        &catalog,
        &models_json,
        &endpoints,
        "openrouter",
        &fetched_at,
    )
    .map_err(|e| e.to_string())?;
    drop(catalog);
    let (_db, manifest_path, manifest) = publish(
        &work_db,
        &args.out,
        "openrouter",
        summary.model_count as u64,
        summary.endpoint_count as u64,
    )
    .map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&work_db);

    println!(
        "synced {} models, {} endpoints -> {} (version {})",
        summary.model_count,
        summary.endpoint_count,
        manifest_path.display(),
        manifest.version
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("catalog-sync: {e}");
            ExitCode::FAILURE
        }
    }
}
