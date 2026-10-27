//! catalog-process: fill a catalog DB's indexed columns in place.
//!
//! Deterministic extraction (Phase 2a), then model-judged signals from a
//! recorded-output file when `--judge` is passed (Phase 2b, no live model
//! calls). Migrates older DBs on open.
//! NOTE: run before publish — processing a published artifact afterwards
//! would invalidate its manifest hash.

use pennyrouter_catalog::{now_rfc3339, run_judging, run_processing, Catalog, RecordedJudge};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: catalog-process --db <catalog.db> [--judge <judgments.json>]"
}

struct Args {
    db: PathBuf,
    judge: Option<PathBuf>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut db = None;
    let mut judge = None;
    let mut it = argv.iter().skip(1);
    while let Some(arg) = it.next() {
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        let mut value = |inline: Option<String>| -> Result<String, String> {
            match inline {
                Some(v) => Ok(v),
                None => it
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{key} needs a value")),
            }
        };
        match key {
            "--db" => db = Some(PathBuf::from(value(inline)?)),
            "--judge" => judge = Some(PathBuf::from(value(inline)?)),
            "--help" | "-h" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(Args {
        db: db.ok_or_else(|| "--db is required".to_string())?,
        judge,
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
    if fs::metadata(&args.db).is_err() {
        return Err(format!("db not found: {}", args.db.display()));
    }

    let stamp = now_rfc3339().map_err(|e| e.to_string())?;
    let catalog = Catalog::open(&args.db).map_err(|e| e.to_string())?;
    catalog.init().map_err(|e| e.to_string())?;
    let summary = run_processing(&catalog, &stamp).map_err(|e| e.to_string())?;
    println!(
        "processed {} endpoints in {}",
        summary.processed,
        args.db.display()
    );
    if let Some(path) = args.judge {
        let judge = RecordedJudge::load(&path).map_err(|e| e.to_string())?;
        let summary = run_judging(&catalog, &judge, &stamp).map_err(|e| e.to_string())?;
        println!(
            "judged {} endpoints ({} matched) in {}",
            summary.judged,
            summary.matched,
            args.db.display()
        );
    }
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
