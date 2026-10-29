//! catalog-process: fill a catalog DB's indexed columns in place.
//!
//! Deterministic extraction (Phase 2a), then model-judged signals from a
//! recorded-output file when `--judge` is passed (Phase 2b, no live model
//! calls) or from a live backend with `--judge-backend baseten|modal`
//! (Phase 4, keys from the environment). `--judges <file>` selects the
//! backend and its policy from a decision-model catalog instead of the
//! built-ins (Iter 15): recorded stays the default, live runs are
//! sampled and capped. Migrates older DBs on open.
//! NOTE: run before publish — processing a published artifact afterwards
//! would invalidate its manifest hash.

use pennyrouter_catalog::{
    build_judge, now_rfc3339, plan_live_calls, run_judging, run_judging_selected, run_processing,
    select_endpoints, spend_line, BasetenJudge, Catalog, JudgeCatalog, ModalJudge, RecordedJudge,
};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: catalog-process --db <catalog.db> [--judge <judgments.json> | --judge-backend <name>] [--judges <judges.json>] [--sample-rate <0..1>] [--max-calls <n>] [--max-spend-usd <x>]"
}

struct Args {
    db: PathBuf,
    judge: Option<PathBuf>,
    judge_backend: Option<String>,
    judges: Option<PathBuf>,
    sample_rate: Option<f64>,
    max_calls: Option<u64>,
    max_spend_usd: Option<f64>,
}

fn parse_number<T: std::str::FromStr>(key: &str, raw: &str) -> Result<T, String> {
    raw.parse()
        .map_err(|_| format!("{key} needs a numeric value, got {raw:?}"))
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut db = None;
    let mut judge = None;
    let mut judge_backend = None;
    let mut judges = None;
    let mut sample_rate = None;
    let mut max_calls = None;
    let mut max_spend_usd = None;
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
            "--judges" => judges = Some(PathBuf::from(value(inline)?)),
            "--sample-rate" => sample_rate = Some(parse_number(key, &value(inline)?)?),
            "--max-calls" => max_calls = Some(parse_number(key, &value(inline)?)?),
            "--max-spend-usd" => max_spend_usd = Some(parse_number(key, &value(inline)?)?),
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
        judges,
        sample_rate,
        max_calls,
        max_spend_usd,
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
    if args.judges.is_some() && args.judge_backend.is_none() {
        return Err(
            "--judges selects a --judge-backend entry; pass one or use --judge".to_string(),
        );
    }
    if args.judge.is_some() && args.judges.is_some() {
        return Err(
            "--judges selects a --judge-backend entry; --judge is already a recording".to_string(),
        );
    }
    // The decision-model catalog (Iter 15): which judge serves the
    // named backend and under which policy. Loaded before any work so
    // an unknown backend or bad policy fails with the DB untouched.
    let judges = args
        .judges
        .as_ref()
        .map(|path| JudgeCatalog::load(path))
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut policy = judges
        .as_ref()
        .map(|c| c.policy.clone())
        .unwrap_or_default();
    if let Some(rate) = args.sample_rate {
        policy.sample_rate = rate;
    }
    if let Some(calls) = args.max_calls {
        policy.max_calls = Some(calls);
    }
    if let Some(spend) = args.max_spend_usd {
        policy.max_spend_usd = Some(spend);
    }
    if args.judge_backend.is_some() {
        policy.validate().map_err(|e| e.to_string())?;
    }
    // Validate the live backend (and its keys) before doing any work.
    // Boxed: the backends are different types behind one `Judge` trait.
    let live_judge: Option<(String, Box<dyn pennyrouter_catalog::Judge>)> =
        match (&args.judge_backend, &judges) {
            (None, _) => None,
            (Some(_), Some(catalog)) => {
                let name = args.judge_backend.as_deref().unwrap_or("");
                let entry = catalog.resolve(name).map_err(|e| e.to_string())?;
                let judge = build_judge(entry).map_err(|e| e.to_string())?;
                Some((name.to_string(), judge))
            }
            (Some(backend), None) if backend == "baseten" => Some((
                backend.clone(),
                Box::new(BasetenJudge::from_env().map_err(|e| e.to_string())?),
            )),
            (Some(backend), None) if backend == "modal" => Some((
                backend.clone(),
                Box::new(ModalJudge::from_env().map_err(|e| e.to_string())?),
            )),
            (Some(backend), None) => {
                return Err(format!(
                    "unknown --judge-backend: {backend} (supported: baseten, modal)"
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
    if let Some((backend, judge)) = &live_judge {
        // Recorded judging is free and deterministic: full run, no
        // sampling, no caps. Live rebuilds are policy-governed:
        // select, check caps, then judge — caps fail closed before
        // the first write.
        let entry = judges
            .as_ref()
            .and_then(|catalog| catalog.resolve(backend).ok());
        let live = backend != "recorded";
        let selected = if live && entry.is_some() {
            let all = catalog.all_endpoints().map_err(|e| e.to_string())?;
            select_endpoints(&all, &policy)
        } else {
            catalog.all_endpoints().map_err(|e| e.to_string())?
        };
        let plan = match &entry {
            Some(entry) if live => {
                Some(plan_live_calls(selected.len(), entry, &policy).map_err(|e| e.to_string())?)
            }
            _ => None,
        };
        let summary = run_judging_selected(&catalog, judge.as_ref(), &selected, &stamp)
            .map_err(|e| e.to_string())?;
        catalog
            .set_meta("judge_backend", backend)
            .map_err(|e| e.to_string())?;
        catalog
            .set_meta("judge_calls", &summary.judged.to_string())
            .map_err(|e| e.to_string())?;
        println!(
            "judged {} endpoints ({} matched) in {}",
            summary.judged,
            summary.matched,
            args.db.display()
        );
        if let (Some(entry), Some(plan)) = (entry, plan) {
            let spend = plan
                .spend_usd
                .map(|s| format!("{s:.4}"))
                .unwrap_or_else(|| "unknown".to_string());
            catalog
                .set_meta("judge_spend_usd", &spend)
                .map_err(|e| e.to_string())?;
            println!("{}", spend_line(&plan, entry, &policy));
        }
    } else if let Some(path) = &args.judge {
        let judge = RecordedJudge::load(path).map_err(|e| e.to_string())?;
        let summary = run_judging(&catalog, &judge, &stamp).map_err(|e| e.to_string())?;
        catalog
            .set_meta("judge_backend", "recorded")
            .map_err(|e| e.to_string())?;
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
