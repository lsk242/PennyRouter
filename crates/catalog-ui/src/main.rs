//! catalog-ui: read-only web browser over a catalog artifact directory.
//!
//! Shows run history (published manifests, newest first), the current
//! catalog DB (models, endpoints, rebuild/audit meta), the ranking
//! document, and — when `--log-file` points at pipeline output — the
//! execution log tail. Everything is read from disk per request;
//! nothing is cached and nothing is written, so the UI can never
//! disturb a running sync.
//!
//! Off unless explicitly enabled: `CATALOG_UI=1` is required, the
//! binary exits otherwise. No auth — bind localhost or firewall it.

use axum::{extract::State, response::Html, routing::get, Router};
use pennyrouter_catalog::{load_ranking_doc, verify, verify_ranking, Catalog};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

struct UiState {
    dir: PathBuf,
    log_file: Option<PathBuf>,
}

fn usage() -> &'static str {
    "usage: CATALOG_UI=1 catalog-ui --dir <artifact-dir> [--port <port> | PORT=<port>] [--log-file <path>]"
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn shell(title: &str, nav: &str, body: String) -> String {
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
        <title>{title} — PennyRouter catalog</title>\
        <style>body{{font-family:system-ui,sans-serif;margin:2em;max-width:72em}}\
        table{{border-collapse:collapse;margin:1em 0}}\
        th,td{{border:1px solid #999;padding:.3em .6em;text-align:left;font-size:.9em}}\
        th{{background:#eee}}pre{{background:#f4f4f4;padding:1em;overflow:auto}}\
        nav a{{margin-right:1em}}</style></head><body>\
        <h1>PennyRouter catalog</h1><nav>{nav}</nav>{body}</body></html>",
        title = esc(title),
    )
}

fn nav() -> &'static str {
    "<a href=\"/\">runs</a><a href=\"/models\">models</a>\
    <a href=\"/ranking\">ranking</a><a href=\"/meta\">meta</a><a href=\"/log\">log</a>"
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Manifest sidecars in the dir, newest first by `created_at`.
/// Unparseable files are skipped, never fatal.
fn manifests(dir: &Path, suffix: &str) -> Vec<(PathBuf, serde_json::Value)> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.ends_with(suffix) {
            continue;
        }
        if let Some(doc) = read_json(&path) {
            out.push((path, doc));
        }
    }
    out.sort_by(|a, b| {
        b.1.get("created_at")
            .and_then(|v| v.as_str())
            .cmp(&a.1.get("created_at").and_then(|v| v.as_str()))
    });
    out
}

fn at(doc: &serde_json::Value, key: &str) -> String {
    doc.get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        })
        .unwrap_or_else(|| "-".to_string())
}

/// Newest verifying catalog manifest's DB, opened read-only.
fn current_db(dir: &Path) -> Option<(String, Catalog)> {
    for (path, doc) in manifests(dir, ".manifest.json") {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("catalog-") {
            continue;
        }
        if verify(&path).unwrap_or(false) {
            let db_file = doc.get("db_file").and_then(|v| v.as_str())?;
            let version = doc
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let db = Catalog::open_read_only(&dir.join(db_file)).ok()?;
            return Some((version, db));
        }
    }
    None
}

async fn index(State(state): State<Arc<UiState>>) -> Html<String> {
    let mut body = format!(
        "<h2>Config</h2><ul><li>version <strong>{}</strong> (build <strong>{}</strong>)</li>\
        <li>artifact dir: {}</li><li>log file: {}</li></ul>",
        esc(env!("CARGO_PKG_VERSION")),
        esc(option_env!("GIT_HASH").unwrap_or("unknown")),
        esc(&state.dir.display().to_string()),
        state
            .log_file
            .as_ref()
            .map(|p| esc(&p.display().to_string()))
            .unwrap_or_else(|| "(none — pass --log-file)".to_string()),
    );
    body.push_str(
        "<h2>Runs (newest first)</h2>\
        <table><tr><th>artifact</th><th>version</th><th>created</th>\
        <th>models/endpoints</th><th>verifies</th></tr>",
    );
    for (path, doc) in manifests(&state.dir, ".manifest.json") {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let ok = if name.starts_with("catalog-") {
            verify(&path).unwrap_or(false)
        } else {
            verify_ranking(&path).unwrap_or(false)
        };
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{} / {}</td><td>{}</td></tr>",
            esc(name),
            esc(&at(&doc, "version")),
            esc(&at(&doc, "created_at")),
            esc(&at(&doc, "model_count")),
            esc(&at(&doc, "endpoint_count")),
            if ok { "yes" } else { "NO" },
        ));
    }
    body.push_str("</table>");
    Html(shell("runs", nav(), body))
}

async fn models(State(state): State<Arc<UiState>>) -> Html<String> {
    let Some((version, db)) = current_db(&state.dir) else {
        return Html(shell(
            "models",
            nav(),
            "<p>No verifying catalog found.</p>".to_string(),
        ));
    };
    let mut body = format!("<h2>Models (catalog {version})</h2>");
    match db.all_endpoints() {
        Err(e) => body.push_str(&format!("<p>DB error: {}</p>", esc(&e.to_string()))),
        Ok(endpoints) => {
            let mut by_model: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for ep in &endpoints {
                *by_model.entry(ep.model_id.as_str()).or_default() += 1;
            }
            body.push_str("<table><tr><th>model</th><th>endpoints</th></tr>");
            for (model, count) in &by_model {
                body.push_str(&format!(
                    "<tr><td><a href=\"/endpoints?model={}\">{}</a></td><td>{count}</td></tr>",
                    esc(model),
                    esc(model),
                ));
            }
            body.push_str("</table>");
        }
    }
    Html(shell("models", nav(), body))
}

async fn endpoints(
    State(state): State<Arc<UiState>>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Html<String> {
    let Some((version, db)) = current_db(&state.dir) else {
        return Html(shell(
            "endpoints",
            nav(),
            "<p>No verifying catalog found.</p>".to_string(),
        ));
    };
    let filter = query.get("model").cloned().unwrap_or_default();
    let mut body = format!("<h2>Endpoints (catalog {version})</h2>");
    match db.all_endpoints() {
        Err(e) => body.push_str(&format!("<p>DB error: {}</p>", esc(&e.to_string()))),
        Ok(endpoints) => {
            body.push_str(
                "<table><tr><th>model</th><th>provider</th><th>tag</th>\
                <th>prompt $/tok</th><th>completion $/tok</th><th>class</th>\
                <th>tier</th><th>score</th><th>judged by</th></tr>",
            );
            for ep in &endpoints {
                if !filter.is_empty() && ep.model_id != filter {
                    continue;
                }
                let tag = ep
                    .payload
                    .parse::<serde_json::Value>()
                    .ok()
                    .and_then(|p| p.get("tag").and_then(|t| t.as_str()).map(str::to_string));
                let judged = match (&ep.judge_model, &ep.judge_prompt) {
                    (Some(m), Some(p)) => format!("{m} / {p}"),
                    _ => "-".to_string(),
                };
                body.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
                    <td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    esc(&ep.model_id),
                    esc(&ep.provider_name),
                    esc(tag.as_deref().unwrap_or("-")),
                    ep.price_prompt
                        .map(|v| format!("{v:.7}"))
                        .unwrap_or_else(|| "-".to_string()),
                    ep.price_completion
                        .map(|v| format!("{v:.7}"))
                        .unwrap_or_else(|| "-".to_string()),
                    esc(ep.precision_class.as_deref().unwrap_or("-")),
                    esc(ep.quality_tier.as_deref().unwrap_or("-")),
                    ep.quality_score
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or_else(|| "-".to_string()),
                    esc(&judged),
                ));
            }
            body.push_str("</table>");
        }
    }
    Html(shell("endpoints", nav(), body))
}

async fn ranking(State(state): State<Arc<UiState>>) -> Html<String> {
    let mut found: Option<(String, PathBuf)> = None;
    for (path, doc) in manifests(&state.dir, ".manifest.json") {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("ranking-") || !verify_ranking(&path).unwrap_or(false) {
            continue;
        }
        let file = doc
            .get("ranking_file")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        found = Some((file.to_string(), state.dir.join(file)));
        break;
    }
    let mut body = String::from("<h2>Ranking</h2>");
    match found {
        None => body.push_str("<p>No verifying ranking found.</p>"),
        Some((file, path)) => match load_ranking_doc(&path) {
            Err(e) => body.push_str(&format!("<p>Ranking error: {}</p>", esc(&e.to_string()))),
            Ok(doc) => {
                body.push_str(&format!(
                    "<p>{} (created {})</p>",
                    esc(&file),
                    esc(&doc.created_at)
                ));
                for (model, classes) in &doc.models {
                    body.push_str(&format!("<h3>{}</h3>", esc(model)));
                    for class in classes {
                        let order: Vec<String> = class
                            .default
                            .iter()
                            .map(|e| match &e.tag {
                                Some(t) => format!("{} ({t})", e.provider),
                                None => e.provider.clone(),
                            })
                            .collect();
                        body.push_str(&format!(
                            "<p>{}: {}</p>",
                            esc(class.class.as_deref().unwrap_or("any")),
                            esc(&order.join(" > ")),
                        ));
                    }
                }
            }
        },
    }
    Html(shell("ranking", nav(), body))
}

async fn meta(State(state): State<Arc<UiState>>) -> Html<String> {
    let Some((version, db)) = current_db(&state.dir) else {
        return Html(shell(
            "meta",
            nav(),
            "<p>No verifying catalog found.</p>".to_string(),
        ));
    };
    let mut body = format!("<h2>Rebuild / audit meta (catalog {version})</h2>");
    match db.all_meta() {
        Err(e) => body.push_str(&format!("<p>DB error: {}</p>", esc(&e.to_string()))),
        Ok(rows) => {
            body.push_str("<table><tr><th>key</th><th>value</th></tr>");
            for (key, value) in &rows {
                body.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td></tr>",
                    esc(key),
                    esc(value)
                ));
            }
            body.push_str("</table>");
        }
    }
    Html(shell("meta", nav(), body))
}

async fn log(State(state): State<Arc<UiState>>) -> Html<String> {
    let mut body = String::from("<h2>Execution log</h2>");
    match &state.log_file {
        None => body.push_str("<p>No log file configured (pass --log-file).</p>"),
        Some(path) => match std::fs::read_to_string(path) {
            Err(e) => body.push_str(&format!("<p>Log error: {}</p>", esc(&e.to_string()))),
            Ok(text) => {
                let lines: Vec<&str> = text.lines().collect();
                let tail = &lines[lines.len().saturating_sub(200)..];
                body.push_str(&format!(
                    "<p>Last {} of {} lines: {}</p><pre>",
                    tail.len(),
                    lines.len(),
                    esc(&path.display().to_string())
                ));
                body.push_str(&esc(&tail.join("\n")));
                body.push_str("</pre>");
            }
        },
    }
    Html(shell("log", nav(), body))
}

fn run() -> Result<(), String> {
    let enabled = std::env::var("CATALOG_UI")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if !enabled {
        return Err("refusing to serve: set CATALOG_UI=1 to enable the catalog UI".to_string());
    }
    let argv: Vec<String> = std::env::args().collect();
    let mut dir: Option<PathBuf> = None;
    let mut port: u16 = match std::env::var("PORT") {
        Err(_) => 8080,
        Ok(raw) if raw.is_empty() => 8080,
        Ok(raw) => raw
            .parse()
            .map_err(|_| format!("PORT needs a numeric value, got {raw:?}"))?,
    };
    let mut log_file: Option<PathBuf> = None;
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
            "--dir" => dir = Some(PathBuf::from(value(inline)?)),
            "--port" => {
                port = value(inline)?
                    .parse()
                    .map_err(|_| "--port needs a numeric value".to_string())?
            }
            "--log-file" => log_file = Some(PathBuf::from(value(inline)?)),
            "--help" | "-h" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    let dir = dir.ok_or_else(|| "--dir is required".to_string())?;
    if std::fs::metadata(&dir).is_err() {
        return Err(format!("dir not found: {}", dir.display()));
    }
    let state = Arc::new(UiState { dir, log_file });
    let app = Router::new()
        .route("/", get(index))
        .route("/models", get(models))
        .route("/endpoints", get(endpoints))
        .route("/ranking", get(ranking))
        .route("/meta", get(meta))
        .route("/log", get(log))
        .with_state(state);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
            let addr = listener
                .local_addr()
                .map_err(|e| format!("local addr: {e}"))?;
            println!("catalog-ui on http://{addr}");
            axum::serve(listener, app)
                .await
                .map_err(|e| format!("serve: {e}"))
        })?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if e == "help" {
                println!("{usage}", usage = usage());
                ExitCode::SUCCESS
            } else {
                eprintln!("catalog-ui: {e}\n{usage}", usage = usage());
                ExitCode::FAILURE
            }
        }
    }
}
