//! catalog-rank: rebuild the provider ranking from a catalog DB.
//!
//! Offline periodic job (Iter 6). Reads the DB read-only plus optional
//! trends and recorded AI adjustments, and publishes a versioned
//! ranking artifact the gateway serves with a single lookup.

use pennyrouter_catalog::{
    now_rfc3339, publish_ranking, rank_catalog, Assist, Catalog, Promos, Trends,
};
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage: catalog-rank --db <catalog.db> --out <dir> [--trends <trends.json>] [--assist <assist.json>] [--promos <promos.json>]"
}

struct Args {
    db: PathBuf,
    out: PathBuf,
    trends: Option<PathBuf>,
    assist: Option<PathBuf>,
    promos: Option<PathBuf>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut db = None;
    let mut out = None;
    let mut trends = None;
    let mut assist = None;
    let mut promos = None;
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
            "--db" => db = Some(PathBuf::from(value("--db")?)),
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--trends" => trends = Some(PathBuf::from(value("--trends")?)),
            "--assist" => assist = Some(PathBuf::from(value("--assist")?)),
            "--promos" => promos = Some(PathBuf::from(value("--promos")?)),
            "--help" | "-h" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(Args {
        db: db.ok_or_else(|| "--db is required".to_string())?,
        out: out.ok_or_else(|| "--out is required".to_string())?,
        trends,
        assist,
        promos,
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

    let catalog = Catalog::open_read_only(&args.db).map_err(|e| e.to_string())?;
    let trends = match &args.trends {
        Some(path) => Trends::load(path).map_err(|e| e.to_string())?,
        None => Trends::empty(),
    };
    let assist = match &args.assist {
        Some(path) => Assist::load(path).map_err(|e| e.to_string())?,
        None => Assist::empty(),
    };
    let promos = match &args.promos {
        Some(path) => Promos::load(path).map_err(|e| e.to_string())?,
        None => Promos::empty(),
    };
    let created_at = now_rfc3339().map_err(|e| e.to_string())?;
    // Best-effort lineage: filename stem of the source DB.
    let catalog_version = args
        .db
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string);
    let doc = rank_catalog(
        &catalog,
        &trends,
        &assist,
        &promos,
        &created_at,
        &created_at,
        catalog_version,
    )
    .map_err(|e| e.to_string())?;
    let (_file, manifest_path, manifest) =
        publish_ranking(&doc, &args.out).map_err(|e| e.to_string())?;
    println!(
        "ranked {} models -> {} (version {})",
        manifest.model_count,
        manifest_path.display(),
        manifest.version
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("catalog-rank: {e}");
            ExitCode::FAILURE
        }
    }
}
