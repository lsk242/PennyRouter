//! catalog-process: fill a catalog DB's indexed columns in place.
//!
//! Deterministic extraction (Phase 2a), then model-judged signals from a
//! recorded-output file when `--judge` is passed (Phase 2b, no live model
//! calls) or from a live backend with `--judge-backend baseten` (Phase 4,
//! key from `BASETEN_API_KEY`). Migrates older DBs on open.
//! NOTE: run before publish — processing a published artifact afterwards
//! would invalidate its manifest hash.

use pennyrouter_catalog::{
    now_rfc3339, run_judging, run_processing, BasetenJudge, Catalog, RecordedJudge,
};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: catalog-process --db <catalog.db> [--judge <judgments.json> | --judge-backend baseten]"
}

struct Args {
    db: PathBuf,
    judge: Option<PathBuf>,
    judge_backend: Option<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut db = None;
    let mut judge = None;
    let mut judge_backend = None;
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
            "--judge-backend" => judge_backend = Some(value(inline)?),
            "--help" | "-h" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    if judge.is_some() && judge_backend.is_some() {
        return Err("use one of --judge or --judge-backend".to_string());
    }
    Ok(Args {
        db: db.ok_or_else(|| "--db is required".to_string())?,
        judge,
        judge_backend,
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
    // Validate the live backend (and its key) before doing any work.
    let live_judge = match &args.judge_backend {
        None => None,
        Some(backend) if backend == "baseten" => {
            Some(BasetenJudge::from_env().map_err(|e| e.to_string())?)
        }
        Some(backend) => {
            return Err(format!(
                "unknown --judge-backend: {backend} (supported: baseten)"
            ));
        }
    };

    let stamp = now_rfc3339().map_err(|e| e.to_string())?;
    let catalog = Catalog::open(&args.db).map_err(|e| e.to_string())?;
    catalog.init().map_err(|e| e.to_string())?;
    let summary = run_processing(&catalog, &stamp).map_err(|e| e.to_string())?;
    println!(
        "processed {} endpoints in {}",
        summary.processed,
        args.db.display()
    );
    if let Some(judge) = &live_judge {
        let summary = run_judging(&catalog, judge, &stamp).map_err(|e| e.to_string())?;
        println!(
            "judged {} endpoints ({} matched) in {}",
            summary.judged,
            summary.matched,
            args.db.display()
        );
    } else if let Some(path) = &args.judge {
        let judge = RecordedJudge::load(path).map_err(|e| e.to_string())?;
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
