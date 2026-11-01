//! catalog-sync: gather provider/model data into a versioned catalog DB.
//!
//! Fixture-driven by default: `--models` points at a `GET /models`
//! response body and `--endpoints-dir` holds one `GET
//! /models/{id}/endpoints` body per model, named `<id with '/' as
//! '__'>.json`. With `--fetch-openrouter` the same bytes are pulled
//! live from OpenRouter's public endpoints (no auth, no keys) instead.
//!
//! `--process` fills the indexed columns on the work DB before publish,
//! so the artifact ships with processing applied.

use pennyrouter_catalog::{
    fetch_openrouter_extra, load_endpoints_dir, now_rfc3339, publish, run_fixture_sync,
    run_processing, Catalog,
};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: catalog-sync (--models <models.json> --endpoints-dir <dir> | --fetch-openrouter) --out <dir> [--process]"
}

struct Args {
    models: Option<PathBuf>,
    endpoints_dir: Option<PathBuf>,
    fetch_openrouter: bool,
    out: PathBuf,
    process: bool,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut models = None;
    let mut endpoints_dir = None;
    let mut fetch_openrouter_flag = false;
    let mut out = None;
    let mut process = false;
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
            "--fetch-openrouter" => {
                if inline.is_some() {
                    return Err("--fetch-openrouter takes no value".to_string());
                }
                fetch_openrouter_flag = true;
            }
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--process" => {
                if inline.is_some() {
                    return Err("--process takes no value".to_string());
                }
                process = true;
            }
            "--help" | "-h" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    if fetch_openrouter_flag && (models.is_some() || endpoints_dir.is_some()) {
        return Err("--fetch-openrouter replaces --models/--endpoints-dir".to_string());
    }
    if !fetch_openrouter_flag {
        if models.is_none() {
            return Err("--models is required (or --fetch-openrouter)".to_string());
        }
        if endpoints_dir.is_none() {
            return Err("--endpoints-dir is required (or --fetch-openrouter)".to_string());
        }
    }
    Ok(Args {
        models,
        endpoints_dir,
        fetch_openrouter: fetch_openrouter_flag,
        out: out.ok_or_else(|| "--out is required".to_string())?,
        process,
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

    // Whatever produces the bytes, the fixture boundary loads them:
    // live fetch returns the same shapes the fixture files hold.
    // EXTRA_MODEL_IDS (comma-separated, Iter 24) nominates ids the
    // models list omits (e.g. non-text-output scoring models) for an
    // individual endpoints fetch each.
    let (models_json, endpoints) = if args.fetch_openrouter {
        let base = std::env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api".to_string());
        let extra_ids: Vec<String> = std::env::var("EXTRA_MODEL_IDS")
            .map(|raw| {
                raw.split(',')
                    .map(|id| id.trim().to_string())
                    .filter(|id| !id.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let fetched = fetch_openrouter_extra(&base, &extra_ids).map_err(|e| e.to_string())?;
        if !fetched.skipped.is_empty() {
            println!("skipped {} models without endpoints", fetched.skipped.len());
        }
        (fetched.models, fetched.endpoints)
    } else {
        let models_path = args.models.as_ref().expect("validated above");
        let endpoints_path = args.endpoints_dir.as_ref().expect("validated above");
        let models_json =
            fs::read(models_path).map_err(|e| format!("read {}: {e}", models_path.display()))?;
        let endpoints = load_endpoints_dir(endpoints_path).map_err(|e| e.to_string())?;
        (models_json, endpoints)
    };
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
    if args.process {
        run_processing(&catalog, &fetched_at).map_err(|e| e.to_string())?;
    }
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
        "synced {} models, {} endpoints{} -> {} (version {})",
        summary.model_count,
        summary.endpoint_count,
        if args.process { " (processed)" } else { "" },
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
