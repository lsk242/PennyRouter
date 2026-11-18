//! catalog-process: fill a catalog DB's indexed columns in place.
//!
//! Deterministic extraction (Phase 2a), then model-scored signals from a
//! recorded-output file when `--score` is passed (Phase 2b, no live model
//! calls) or from a live backend with `--score-backend
//! baseten|modal|openrouter` (Phase 4, keys from the environment).
//! `--scorers <file>` selects the
//! backend and its policy from a decision-model catalog instead of the
//! built-ins (Iter 15): recorded stays the default, live runs are
//! sampled and capped. Migrates older DBs on open.
//! NOTE: run before publish — processing a published artifact afterwards
//! would invalidate its manifest hash.

use pennyrouter_catalog::{
    build_score, now_rfc3339, plan_live_calls, run_processing, run_scoring, run_scoring_selected,
    select_endpoints, spend_line, BasetenScorer, Catalog, ModalScorer, OpenRouterScorer,
    RecordedScorer, ScorerCatalog, ScorerEntry,
};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: catalog-process --db <catalog.db> [--score <scores.json> | --score-backend <name>] [--scorers <scorers.json>] [--sample-rate <0..1>] [--max-calls <n>] [--max-spend-usd <x>]"
}

struct Args {
    db: PathBuf,
    score: Option<PathBuf>,
    score_backend: Option<String>,
    scorers: Option<PathBuf>,
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
    let mut score = None;
    let mut score_backend = None;
    let mut scorers = None;
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
            "--score" => score = Some(PathBuf::from(value(inline)?)),
            "--score-backend" => score_backend = Some(value(inline)?),
            "--scorers" => scorers = Some(PathBuf::from(value(inline)?)),
            "--sample-rate" => sample_rate = Some(parse_number(key, &value(inline)?)?),
            "--max-calls" => max_calls = Some(parse_number(key, &value(inline)?)?),
            "--max-spend-usd" => max_spend_usd = Some(parse_number(key, &value(inline)?)?),
            "--help" | "-h" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    if score.is_some() && score_backend.is_some() {
        return Err("use one of --score or --score-backend".to_string());
    }
    Ok(Args {
        db: db.ok_or_else(|| "--db is required".to_string())?,
        score,
        score_backend,
        scorers,
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
    if args.scorers.is_some() && args.score_backend.is_none() {
        return Err(
            "--scorers selects a --score-backend entry; pass one or use --score".to_string(),
        );
    }
    if args.score.is_some() && args.scorers.is_some() {
        return Err(
            "--scorers selects a --score-backend entry; --score is already a recording".to_string(),
        );
    }
    // The decision-model catalog (Iter 15): which scorer serves the
    // named backend and under which policy. Loaded before any work so
    // an unknown backend or bad policy fails with the DB untouched.
    let scorers = args
        .scorers
        .as_ref()
        .map(|path| ScorerCatalog::load(path))
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut policy = scorers
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
    if args.score_backend.is_some() {
        policy.validate().map_err(|e| e.to_string())?;
    }
    // Validate the live backend (and its keys) before doing any work.
    // Boxed: the backends are different types behind one `Scorer` trait.
    let live_score: Option<(String, Box<dyn pennyrouter_catalog::Scorer>)> =
        match (&args.score_backend, &scorers) {
            (None, _) => None,
            (Some(_), Some(catalog)) => {
                let name = args.score_backend.as_deref().unwrap_or("");
                let entry = catalog.resolve(name).map_err(|e| e.to_string())?;
                let score = build_score(entry).map_err(|e| e.to_string())?;
                Some((name.to_string(), score))
            }
            (Some(backend), None) if backend == "baseten" => Some((
                backend.clone(),
                Box::new(BasetenScorer::from_env().map_err(|e| e.to_string())?),
            )),
            (Some(backend), None) if backend == "modal" => Some((
                backend.clone(),
                Box::new(ModalScorer::from_env().map_err(|e| e.to_string())?),
            )),
            (Some(backend), None) if backend == "openrouter" => Some((
                backend.clone(),
                Box::new(OpenRouterScorer::from_env().map_err(|e| e.to_string())?),
            )),
            (Some(backend), None) => {
                return Err(format!(
                    "unknown --score-backend: {backend} (supported: baseten, modal, openrouter)"
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
    println!(
        "scored {} endpoints (offline) in {}",
        summary.scored,
        args.db.display()
    );
    if let Some((backend, score)) = &live_score {
        // Recorded scoring is free and deterministic: full run, no
        // sampling, no caps. Live rebuilds are policy-governed:
        // select, check caps, then score — caps fail closed before
        // the first write.
        let entry = scorers
            .as_ref()
            .and_then(|catalog| catalog.resolve(backend).ok());
        let live = backend != "recorded";
        // B19: sampling and caps apply to every live run, not just
        // cataloged ones — bare `--score-backend` runs used to ignore
        // the flags and score everything. Without a catalog the
        // per-call cost is unknown, so a spend cap fails closed (the
        // documented `plan_live_calls` semantics) while sampling and
        // max_calls still bind.
        let uncosted_entry = ScorerEntry {
            backend: backend.clone(),
            model: String::new(),
            base_url: String::new(),
            auth_env: Vec::new(),
            prompt_version: String::new(),
            cost_per_call_usd: None,
            recording: None,
        };
        let plan_entry: Option<&ScorerEntry> = match entry {
            Some(entry) if live => Some(entry),
            None if live => Some(&uncosted_entry),
            _ => None,
        };
        let selected = if live {
            let all = catalog.all_endpoints().map_err(|e| e.to_string())?;
            select_endpoints(&all, &policy)
        } else {
            catalog.all_endpoints().map_err(|e| e.to_string())?
        };
        let plan = match plan_entry {
            Some(entry) => {
                Some(plan_live_calls(selected.len(), entry, &policy).map_err(|e| e.to_string())?)
            }
            None => None,
        };
        let summary = run_scoring_selected(&catalog, score.as_ref(), &selected, &stamp)
            .map_err(|e| e.to_string())?;
        catalog
            .set_meta("score_backend", backend)
            .map_err(|e| e.to_string())?;
        catalog
            .set_meta("score_calls", &summary.scored.to_string())
            .map_err(|e| e.to_string())?;
        println!(
            "scored {} endpoints ({} matched) in {}",
            summary.scored,
            summary.matched,
            args.db.display()
        );
        if let (Some(entry), Some(plan)) = (plan_entry, plan) {
            let spend = plan
                .spend_usd
                .map(|s| format!("{s:.4}"))
                .unwrap_or_else(|| "unknown".to_string());
            catalog
                .set_meta("score_spend_usd", &spend)
                .map_err(|e| e.to_string())?;
            println!("{}", spend_line(&plan, entry, &policy));
        }
    } else if let Some(path) = &args.score {
        let score = RecordedScorer::load(path).map_err(|e| e.to_string())?;
        let summary = run_scoring(&catalog, &score, &stamp).map_err(|e| e.to_string())?;
        catalog
            .set_meta("score_backend", "recorded")
            .map_err(|e| e.to_string())?;
        println!(
            "scored {} endpoints ({} matched) in {}",
            summary.scored,
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
