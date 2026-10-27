//! catalog-process: fill a catalog DB's indexed columns in place.
//!
//! Deterministic extraction only (Phase 2a). Migrates older DBs on open.
//! NOTE: run before publish — processing a published artifact afterwards
//! would invalidate its manifest hash.

use pennyrouter_catalog::{now_rfc3339, run_processing, Catalog};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: catalog-process --db <catalog.db>"
}

fn parse_args(argv: &[String]) -> Result<PathBuf, String> {
    let mut db = None;
    let mut it = argv.iter().skip(1);
    while let Some(arg) = it.next() {
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        match key {
            "--db" => {
                let val = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .cloned()
                        .ok_or_else(|| "--db needs a value".to_string())?,
                };
                db = Some(PathBuf::from(val));
            }
            "--help" | "-h" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    db.ok_or_else(|| "--db is required".to_string())
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().collect();
    let db = parse_args(&argv).map_err(|e| {
        if e == "help" {
            usage().to_string()
        } else {
            format!("{e}\n{}", usage())
        }
    })?;
    if fs::metadata(&db).is_err() {
        return Err(format!("db not found: {}", db.display()));
    }

    let processed_at = now_rfc3339().map_err(|e| e.to_string())?;
    let catalog = Catalog::open(&db).map_err(|e| e.to_string())?;
    catalog.init().map_err(|e| e.to_string())?;
    let summary = run_processing(&catalog, &processed_at).map_err(|e| e.to_string())?;
    println!(
        "processed {} endpoints in {}",
        summary.processed,
        db.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("catalog-process: {e}");
            ExitCode::FAILURE
        }
    }
}
