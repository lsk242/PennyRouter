//! catalog-ui: read-only web browser over a catalog artifact directory.
//!
//! Shows run history (published manifests, newest first), the current
//! catalog DB (models, endpoints, rebuild/audit meta), and — when
//! `--log-file` points at pipeline output — the execution log tail.
//! Everything is read from disk per request;
//! nothing is cached, and the only write is the `.resync-requested`
//! trigger file behind the force re-sync button, which the sync
//! loop picks up on its next poll — the UI never touches the DB.
//!
//! Off unless explicitly enabled: `CATALOG_UI=1` is required, the
//! binary exits otherwise. No auth — binds localhost unless `--bind`
//! says otherwise; firewall any wider bind.

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
    Router,
};
use pennyrouter_catalog::{
    balanced_profile, compare_rank, effective_cost, precision_factor, verify, verify_ranking,
    Catalog, EndpointRank, EndpointRow, EndpointSpeed, ModelSignals, RankKey, TopModel,
    COST_HALF_CREDIT, POP_WEIGHT, RANK_RULESET_VERSION, SCHEMA_VERSION, TASK_TOKENS,
    TREND_BOOST_MAX,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

struct UiState {
    dir: PathBuf,
    log_file: Option<PathBuf>,
}

fn usage() -> &'static str {
    "usage: CATALOG_UI=1 catalog-ui --dir <artifact-dir> [--port <port> | PORT=<port>] [--bind <addr>] [--log-file <path>] (bind default 127.0.0.1)"
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Percent-encode a query-string value (RFC 3986 unreserved set):
/// model ids contain `/`, and may contain `& + % #`, all of which
/// would corrupt the `model=` filter if emitted raw. Tiny local
/// encoder — no new dependency. Output is HTML-safe as-is.
fn url_encode(text: &str) -> String {
    const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        if UNRESERVED.contains(byte) {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Price rendering in $/M tokens (×1e6): two decimals by default,
/// extra decimals only when needed — shared with the gateway /ui.
fn fmt_price(value: f64) -> String {
    pennyrouter_catalog::fmt_price_per_m(value)
}

fn shell(title: &str, nav: &str, body: String) -> String {
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
        <link rel=\"icon\" href=\"/favicon.ico\" type=\"image/svg+xml\">\
        <title>{title} — PennyRouter catalog</title>\
        <style>:root{{color-scheme:dark;--bg:#0d0f1e;--panel:#151a33;--panel-2:#1c2342;\
        --ink:#e9ecff;--muted:#a3acc9;--line:#2c3560;--cyan:#00e5ff;--magenta:#ff2e88;\
        --yellow:#ffd23f;--green:#39ff6a}}\
        *{{box-sizing:border-box;border-radius:0!important}}\
        body{{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;\
        background:var(--bg);color:var(--ink);margin:0;padding:0 1.5em 3em;line-height:1.55}}\
        body::after{{content:\"\";position:fixed;inset:0;z-index:9999;pointer-events:none;\
        background:repeating-linear-gradient(0deg,rgba(0,0,0,.14) 0 1px,transparent 1px 3px)}}\
        a{{color:var(--cyan)}}a:hover{{color:var(--yellow)}}\
        :focus-visible{{outline:2px solid var(--yellow);outline-offset:2px}}\
        .wrap{{width:100%;margin:0}}\
        header.top{{background:var(--panel);border-bottom:3px solid var(--magenta);\
        box-shadow:0 3px 0 #000;margin:0 -1.5em 1.5em;padding:1.2em 1.5em}}\
        h1{{font-size:1.4em;margin:0 0 .6em;letter-spacing:.06em;color:var(--yellow);\
        text-shadow:3px 3px 0 #000}}h1 span{{color:var(--cyan)}}\
        h2{{font-size:1.05em;letter-spacing:.08em;color:var(--cyan);text-shadow:2px 2px 0 #000}}\
        h3{{font-size:.95em;letter-spacing:.06em;color:var(--yellow);text-shadow:2px 2px 0 #000}}\
        nav{{display:flex;gap:.5em;flex-wrap:wrap}}\
        nav a{{font-size:.85em;font-weight:700;letter-spacing:.08em;text-transform:uppercase;\
        text-decoration:none;color:var(--muted);background:var(--bg);border:2px solid var(--line);\
        box-shadow:3px 3px 0 #000;padding:.5em .9em}}\
        nav a:hover{{color:var(--ink);border-color:var(--cyan)}}\
        table{{border-collapse:collapse;width:100%;margin:1em 0;background:var(--panel);\
        border:3px solid var(--line);box-shadow:6px 6px 0 #000}}\
        th,td{{border:1px solid var(--line);padding:.4em .7em;text-align:left;font-size:.9em}}\
        th{{background:var(--panel-2);color:var(--yellow);letter-spacing:.08em;\
        border-bottom:3px solid var(--magenta)}}\
        tbody tr:nth-child(even) td{{background:rgba(255,255,255,.025)}}\
        pre{{background:#0a0d1c;border:3px solid var(--line);box-shadow:6px 6px 0 #000;\
        padding:1em;overflow:auto}}p{{color:var(--ink)}}\
        .muted{{color:var(--muted)}}\
        nav.tabs{{margin:1em 0}}nav.tabs a.active{{color:var(--bg);background:var(--cyan);\
        border-color:var(--cyan)}}\
        .calc{{font-size:.8em;color:var(--muted);white-space:pre-wrap;padding:.4em 0}}\
        .calc-row{{display:none}}.calc-row:target{{display:table-row}}\
        .ranknum{{font-weight:700}}\
        input[type=search]{{background:var(--bg);color:var(--ink);border:2px solid var(--line);\
        padding:.4em .6em;font-family:inherit}}\
        button{{background:var(--panel-2);color:var(--ink);border:2px solid var(--line);\
        padding:.4em .8em;font-family:inherit;cursor:pointer}}\
        @media (prefers-reduced-motion:reduce){{body::after{{display:none}}}}\
        </style></head><body><div class=\"wrap\">\
        <header class=\"top\"><h1>PennyRouter <span>catalog</span></h1><nav>{nav}</nav></header>\
        {body}</div></body></html>",
        title = esc(title),
    )
}

fn nav() -> &'static str {
    "<a href=\"/\">overview</a><a href=\"/models\">models</a>\
    <a href=\"/meta\">meta</a><a href=\"/log\">log</a>"
}

/// Re-sync trigger (Iter 35): `POST /resync` writes this dotfile into
/// the artifact dir, and the sync loop (`docker/catalog-entrypoint.sh`)
/// polls for it between scheduled cycles. A dotfile, so it never
/// matches the `catalog-*.manifest.json` / `ranking-*.manifest.json`
/// scans on either side.
const TRIGGER_FILE: &str = ".resync-requested";

fn trigger_path(dir: &Path) -> PathBuf {
    dir.join(TRIGGER_FILE)
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
struct Current {
    version: String,
    created_at: String,
    db: Catalog,
}

fn current_db(dir: &Path) -> Option<Current> {
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
            let created_at = doc
                .get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let db = Catalog::open_read_only(&dir.join(db_file)).ok()?;
            return Some(Current {
                version,
                created_at,
                db,
            });
        }
    }
    None
}

/// One offering joined with the model signals, speed, and score the
/// value-score views read (Iter 34).
struct Row<'a> {
    ep: &'a EndpointRow,
    signals: Option<&'a ModelSignals>,
    rank: Option<&'a EndpointRank>,
    speed: Option<&'a EndpointSpeed>,
    tag: Option<String>,
    cost: Option<f64>,
    cost_task: Option<f64>,
}

impl<'a> Row<'a> {
    fn new(
        ep: &'a EndpointRow,
        signals: Option<&'a ModelSignals>,
        rank: Option<&'a EndpointRank>,
        speed: Option<&'a EndpointSpeed>,
    ) -> Self {
        let tag = serde_json::from_str::<serde_json::Value>(&ep.payload)
            .ok()
            .and_then(|payload| {
                payload
                    .get("tag")
                    .and_then(|tag| tag.as_str())
                    .map(str::to_string)
            });
        let cost = effective_cost(
            ep.price_prompt,
            ep.price_completion,
            ep.price_cache_read,
            ep.price_cache_write,
            &balanced_profile(),
            ep.promo_discount.unwrap_or(0.0),
        );
        Self {
            ep,
            signals,
            rank,
            speed,
            tag,
            cost,
            cost_task: cost.map(|cost| cost * TASK_TOKENS),
        }
    }

    fn decision(&self) -> bool {
        self.ep.supports_decisions == Some(1)
    }

    /// Batch variant (OpenRouter `:batch` suffix): async batch API
    /// pricing, a category of its own — never mixed into the
    /// interactive free/tiers tabs.
    fn batch(&self) -> bool {
        self.ep.model_id.ends_with(":batch")
    }

    fn score(&self) -> Option<f64> {
        self.rank.and_then(|rank| rank.rank_score)
    }

    fn key(&self) -> RankKey<'_> {
        RankKey {
            score: self.score(),
            throughput_tps: self.speed.and_then(|speed| speed.throughput_tps),
            latency_ms: self.speed.and_then(|speed| speed.latency_ms),
            uptime_pct: self.speed.and_then(|speed| speed.uptime_pct),
            provider: &self.ep.provider_name,
            tag: self.tag.as_deref(),
        }
    }

    /// The AA index the score actually used, per `rank_basis`.
    fn index(&self) -> Option<i64> {
        let signals = self.signals?;
        match self.rank.and_then(|rank| rank.rank_basis.as_deref()) {
            Some("coding") => signals.aa_coding_index,
            Some("intel") => signals.aa_intelligence_index,
            Some("agentic") => signals.aa_agentic_index,
            _ => None,
        }
    }

    fn factor(&self) -> f64 {
        if self.decision() {
            1.0
        } else {
            precision_factor(self.ep.precision_class.as_deref())
        }
    }
}

fn pct(part: usize, total: usize) -> String {
    if total == 0 {
        "0%".to_string()
    } else {
        format!("{:.0}%", part as f64 * 100.0 / total as f64)
    }
}

fn num(value: Option<f64>, decimals: usize) -> String {
    match value {
        Some(value) => format!("{value:.decimals$}"),
        None => "-".to_string(),
    }
}

fn score_cell(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("<strong>{value:.1}</strong>"),
        None => "<span class=\"muted\">-</span>".to_string(),
    }
}

/// The full arithmetic behind one row, from the stored components.
/// `rank-v1` rows show the popularity line; `rank-v2` rows (those
/// carrying `rank_activity`) show the stored cost/task and the
/// activity line instead — each ruleset renders its own formula.
fn breakdown(row: &Row) -> String {
    let Some(rank) = row.rank else {
        return "not scored".to_string();
    };
    let Some(score) = rank.rank_score else {
        let why = if row.cost.is_none() {
            "no price"
        } else if row.index().is_none() {
            "no AA index"
        } else {
            "not processed"
        };
        return format!("no score: {why}");
    };
    if rank.rank_activity.is_some() {
        return breakdown_v2(row, rank, score);
    }
    let quality = rank.rank_quality.unwrap_or(0.0);
    let efficiency = rank.rank_efficiency.unwrap_or(0.0);
    let popularity = rank.rank_popularity.unwrap_or(0.0);
    let share = row
        .signals
        .and_then(|signals| signals.popularity_share)
        .unwrap_or(0.0);
    let cost_task = row.cost_task.unwrap_or(0.0);
    let mut lines = vec![
        format!(
            "quality    = {} ({}) × {:.2} ({}) = {:.2}",
            row.index().unwrap_or(0),
            rank.rank_basis.as_deref().unwrap_or("-"),
            row.factor(),
            row.ep.precision_class.as_deref().unwrap_or("unknown"),
            quality
        ),
        format!(
            "cost/task  = {} $/M × {} tokens = ${:.6}",
            row.cost.map(fmt_price).unwrap_or_else(|| "-".to_string()),
            TASK_TOKENS as u64,
            cost_task
        ),
        format!(
            "efficiency = {:.2} / ({:.6} + {:.2}) = {:.4}",
            COST_HALF_CREDIT, cost_task, COST_HALF_CREDIT, efficiency
        ),
        format!(
            "popularity = (1 + {:.2} × {:.3}) / {:.2} = {:.4}",
            POP_WEIGHT,
            share,
            1.0 + POP_WEIGHT,
            popularity
        ),
        format!(
            "rank_score = {:.2} × {:.4} × {:.4} = {:.1}",
            quality, efficiency, popularity, score
        ),
        format!(
            "tie-breaks = {} tok/s · {} ms · {}% uptime",
            num(row.speed.and_then(|speed| speed.throughput_tps), 0),
            num(row.speed.and_then(|speed| speed.latency_ms), 0),
            num(row.speed.and_then(|speed| speed.uptime_pct), 1)
        ),
    ];
    if let Some(ruleset) = rank.rank_ruleset.as_deref() {
        lines.push(format!("ruleset    = {ruleset}"));
    }
    lines.join("\n")
}

/// v2 arithmetic: the stored cost/task (AA's number when the fetch
/// carried it, else the catalog blend) and the activity factor.
fn breakdown_v2(row: &Row, rank: &EndpointRank, score: f64) -> String {
    let quality = rank.rank_quality.unwrap_or(0.0);
    let efficiency = rank.rank_efficiency.unwrap_or(0.0);
    let activity = rank.rank_activity.unwrap_or(0.0);
    let cost_task = rank.rank_cost_task.unwrap_or(0.0);
    let activity_line = match (
        row.signals.and_then(|signals| signals.activity_level),
        row.signals.and_then(|signals| signals.activity_trend),
    ) {
        (Some(level), Some(trend)) => {
            let boost = (trend - 1.0).clamp(-TREND_BOOST_MAX, TREND_BOOST_MAX);
            format!(
                "activity   = {:.4} × (1 + {:+.4}) / {:.2} = {:.4}",
                level,
                boost,
                1.0 + TREND_BOOST_MAX,
                activity
            )
        }
        _ => format!("activity   = {activity:.4} (neutral, unmeasured)"),
    };
    let mut lines = vec![
        format!(
            "quality    = {} ({}) × {:.2} ({}) = {:.2}",
            row.index().unwrap_or(0),
            rank.rank_basis.as_deref().unwrap_or("-"),
            row.factor(),
            row.ep.precision_class.as_deref().unwrap_or("unknown"),
            quality
        ),
        format!("cost/task  = ${cost_task:.6} (stored)"),
        format!(
            "efficiency = {:.2} / ({:.6} + {:.2}) = {:.4}",
            COST_HALF_CREDIT, cost_task, COST_HALF_CREDIT, efficiency
        ),
        activity_line,
        format!(
            "rank_score = {:.2} × {:.4} × {:.4} = {:.1}",
            quality, efficiency, activity, score
        ),
        format!(
            "tie-breaks = {} tok/s · {} ms · {}% uptime",
            num(row.speed.and_then(|speed| speed.throughput_tps), 0),
            num(row.speed.and_then(|speed| speed.latency_ms), 0),
            num(row.speed.and_then(|speed| speed.uptime_pct), 1)
        ),
    ];
    if let Some(ruleset) = rank.rank_ruleset.as_deref() {
        lines.push(format!("ruleset    = {ruleset}"));
    }
    lines.join("\n")
}

/// The breakdown as a full-width row under its model row, revealed
/// when the rank number links to it (`:target` shows it, no JS).
fn breakdown_row(position: usize, row: &Row, columns: usize) -> String {
    format!(
        "<tr id=\"calc-{position}\" class=\"calc-row\"><td colspan=\"{columns}\">\
        <div class=\"calc\">calc — {} / {}<br>{}</div>\
        <a href=\"#\" class=\"muted\">close</a></td></tr>",
        esc(&row.ep.model_id),
        esc(&row.ep.provider_name),
        esc(&breakdown(row)).replace('\n', "<br>")
    )
}

/// The score formula and its constants, rendered from the crate so the
/// explainer can never drift from the code. v2 is current; v1 stays
/// documented for old artifacts.
fn formula_html() -> String {
    format!(
        "<h2>How the rank score is calculated</h2>\
        <p><code>rank_score = quality × efficiency × activity</code> — \
        maximum intelligence for the lowest price, 0..100.</p>\
        <table><tr><th>term</th><th>formula</th><th>range</th></tr>\
        <tr><td>quality</td><td>AA index × precision factor</td><td>0..100</td></tr>\
        <tr><td>cost/task</td><td>AA cost/task, else effective $/M × {task} tokens</td><td>$</td></tr>\
        <tr><td>efficiency</td><td>{half} / (cost/task + {half})</td><td>0..1</td></tr>\
        <tr><td>activity</td><td>level × (1 + trend boost ±{boost}) / {norm}</td><td>0..1</td></tr>\
        </table>\
        <p>Basis chain: <code>coding → intelligence → agentic → none</code> \
        (decision models read <code>agentic</code> and skip the precision \
        factor). Intelligence counts only on the current test version — \
        stale or unversioned intel is NULL once a version is known. \
        No index or no price means <strong>no score</strong>, never zero.</p>\
        <p>Precision factors: fp32/fp16 1.00 · fp8 0.97 · int8 0.95 · \
        int4/fp4 0.88 · unknown 0.95.</p>\
        <p>Ties break on throughput → latency → uptime → provider/tag. \
        Ruleset <code>{ruleset}</code>.</p>\
        <p class=\"muted\">Legacy <code>rank-v1</code>: quality × efficiency × \
        popularity, with popularity = (1 + {pop} × share) / (1 + {pop}).</p>",
        task = TASK_TOKENS as u64,
        half = COST_HALF_CREDIT,
        boost = TREND_BOOST_MAX,
        norm = 1.0 + TREND_BOOST_MAX,
        pop = POP_WEIGHT,
        ruleset = RANK_RULESET_VERSION,
    )
}

/// Bracket counts (Iter 46): in-bracket members, stale-excluded,
/// and popular-unscored models from the TOP rollup.
fn bracket_html(db: &Catalog) -> String {
    let entries = pennyrouter_catalog::read_top_models(db);
    if entries.is_empty() {
        return "<h2>Top-usage bracket</h2>\
            <p>No TOP data in this artifact (sync without scoring).</p>"
            .to_string();
    }
    let members = entries.values().filter(|e| e.bracket).count();
    let stale = entries
        .values()
        .filter(|e| !e.bracket && e.flags.iter().any(|f| f == "stale-version"))
        .count();
    let popular = entries
        .values()
        .filter(|e| e.flags.iter().any(|f| f == "popular-but-unscored"))
        .count();
    format!(
        "<h2>Top-usage bracket</h2>\
        <table><tr><th>in bracket</th><th>stale-excluded</th><th>popular-unscored</th></tr>\
        <tr><td>{members}</td><td>{stale}</td><td>{popular}</td></tr></table>"
    )
}

/// Fetch health (Iter 46): per-source citation, version, and last-ok.
/// Missing data reads as never fetched, never guessed.
fn health_html(db: &Catalog) -> String {
    let health = pennyrouter_catalog::read_fetch_health(db);
    let meta = |key: &str| db.get_meta(key).ok().flatten().filter(|v| !v.is_empty());
    let aa_ok = health.get("aa").map(String::as_str).unwrap_or("never");
    let or_ok = ["popularity", "benchmarks", "apps", "task"]
        .iter()
        .filter_map(|group| health.get(*group))
        .max()
        .map(String::as_str)
        .unwrap_or("never");
    format!(
        "<h2>Fetch health</h2>\
        <table><tr><th>source</th><th>as of</th><th>version</th><th>last ok</th></tr>\
        <tr><td>Artificial Analysis</td><td>{}</td><td>{}</td><td>{}</td></tr>\
        <tr><td>OpenRouter Data API</td><td>{}</td><td>-</td><td>{}</td></tr></table>",
        meta("aa_as_of")
            .map(|v| esc(&v))
            .unwrap_or_else(|| "never".to_string()),
        meta("aa_intel_version")
            .map(|v| esc(&v))
            .unwrap_or_else(|| "-".to_string()),
        esc(aa_ok),
        meta("popular_as_of")
            .map(|v| esc(&v))
            .unwrap_or_else(|| "never".to_string()),
        esc(or_ok),
    )
}

async fn index(
    State(state): State<Arc<UiState>>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Html<String> {
    let Some(current) = current_db(&state.dir) else {
        return Html(shell(
            "overview",
            nav(),
            "<p>No verifying catalog found.</p>".to_string(),
        ));
    };
    let db = &current.db;
    let signals = db.model_signals().unwrap_or_default();
    let endpoints = db.all_endpoints().unwrap_or_default();
    let ranks = db.endpoint_ranks().unwrap_or_default();
    let speeds = db.endpoint_speeds().unwrap_or_default();

    let models_total = signals.len();
    let with_coding = signals
        .values()
        .filter(|s| s.aa_coding_index.is_some())
        .count();
    let with_intel = signals
        .values()
        .filter(|s| s.aa_intelligence_index.is_some())
        .count();
    let with_agentic = signals
        .values()
        .filter(|s| s.aa_agentic_index.is_some())
        .count();
    let with_any = signals
        .values()
        .filter(|s| {
            s.aa_coding_index.is_some()
                || s.aa_intelligence_index.is_some()
                || s.aa_agentic_index.is_some()
        })
        .count();
    let no_index = models_total - with_any;
    let with_popularity = signals
        .values()
        .filter(|s| s.popularity_share.is_some())
        .count();

    let mut priced = 0usize;
    let mut unpriced = 0usize;
    let mut free = 0usize;
    let mut scored = 0usize;
    let mut decision = 0usize;
    let mut classes: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut tiers: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut rows: Vec<Row> = Vec::new();
    for ep in &endpoints {
        let row = Row::new(
            ep,
            signals.get(&ep.model_id),
            ranks.get(&ep.id),
            speeds.get(&ep.id),
        );
        if row.cost.is_some() {
            priced += 1;
        } else {
            unpriced += 1;
        }
        if row.cost == Some(0.0) {
            free += 1;
        }
        if row.score().is_some() {
            scored += 1;
        }
        if row.decision() {
            decision += 1;
        }
        *classes
            .entry(
                ep.precision_class
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            )
            .or_default() += 1;
        *tiers
            .entry(
                ep.quality_tier
                    .clone()
                    .unwrap_or_else(|| "untiered".to_string()),
            )
            .or_default() += 1;
        rows.push(row);
    }
    let total = endpoints.len();

    let mut body = format!(
        "<h2>Artifact</h2><ul>\
        <li>catalog-ui <strong>{ui_version}</strong> (build {build})</li>\
        <li>catalog <strong>{version}</strong> (schema v{schema})</li>\
        <li>created {created}</li>\
        <li>source {source}</li>\
        <li>artifact dir {dir}</li>\
        <li>log file {log}</li></ul>",
        ui_version = env!("CARGO_PKG_VERSION"),
        build = option_env!("GIT_HASH").unwrap_or("unknown"),
        version = esc(&current.version),
        schema = SCHEMA_VERSION,
        created = esc(&current.created_at),
        source = esc(&db
            .get_meta("source")
            .ok()
            .flatten()
            .unwrap_or_else(|| "-".to_string())),
        dir = esc(&state.dir.display().to_string()),
        log = state
            .log_file
            .as_ref()
            .map(|path| esc(&path.display().to_string()))
            .unwrap_or_else(|| "(none — pass --log-file)".to_string()),
    );

    let confirmed = query.get("resync").is_some_and(|v| v == "requested");
    if confirmed {
        body.push_str(
            "<p><strong>Re-sync requested</strong> — the sync loop picks it up \
            within ~10 seconds; the new run appears below when the cycle finishes.</p>",
        );
    } else if trigger_path(&state.dir).is_file() {
        body.push_str(
            "<p><strong>Re-sync pending</strong> — a request is waiting for the \
            sync loop; the new run appears below when the cycle finishes.</p>",
        );
    }

    body.push_str(&format!(
        "<h2>Coverage</h2><table>\
        <tr><th>metric</th><th>count</th><th>share</th></tr>\
        <tr><td>models</td><td>{models_total}</td><td>100%</td></tr>\
        <tr><td>models with any AA index</td><td>{with_any}</td><td>{any_pct}</td></tr>\
        <tr><td>&nbsp;&nbsp;coding index</td><td>{with_coding}</td><td>{coding_pct}</td></tr>\
        <tr><td>&nbsp;&nbsp;intelligence index</td><td>{with_intel}</td><td>{intel_pct}</td></tr>\
        <tr><td>&nbsp;&nbsp;agentic index</td><td>{with_agentic}</td><td>{agentic_pct}</td></tr>\
        <tr><td>models with no AA index</td><td>{no_index}</td><td>{none_pct}</td></tr>\
        <tr><td>models with popularity</td><td>{with_popularity}</td><td>{pop_pct}</td></tr>\
        <tr><td>offerings</td><td>{total}</td><td>100%</td></tr>\
        <tr><td>&nbsp;&nbsp;priced</td><td>{priced}</td><td>{priced_pct}</td></tr>\
        <tr><td>&nbsp;&nbsp;unpriced</td><td>{unpriced}</td><td>{unpriced_pct}</td></tr>\
        <tr><td>&nbsp;&nbsp;free</td><td>{free}</td><td>{free_pct}</td></tr>\
        <tr><td>&nbsp;&nbsp;scored</td><td>{scored}</td><td>{scored_pct}</td></tr>\
        <tr><td>&nbsp;&nbsp;decision models</td><td>{decision}</td><td>{decision_pct}</td></tr>\
        </table>",
        any_pct = pct(with_any, models_total),
        coding_pct = pct(with_coding, models_total),
        intel_pct = pct(with_intel, models_total),
        agentic_pct = pct(with_agentic, models_total),
        none_pct = pct(no_index, models_total),
        pop_pct = pct(with_popularity, models_total),
        priced_pct = pct(priced, total),
        unpriced_pct = pct(unpriced, total),
        free_pct = pct(free, total),
        scored_pct = pct(scored, total),
        decision_pct = pct(decision, total),
    ));

    body.push_str(
        "<p>Gateway filter: the intelligence index powers the gateway's \
        <code>/mintel/&lt;n&gt;</code> suffix — e.g. \
        <code>openrouter/pair/low/mintel/20</code> keeps only models with intelligence \
        at or above 20 (models with no index never pass).</p>",
    );

    body.push_str("<h2>Precision classes</h2><table><tr><th>class</th><th>offerings</th></tr>");
    for (class, count) in &classes {
        body.push_str(&format!("<tr><td>{}</td><td>{count}</td></tr>", esc(class)));
    }
    body.push_str("</table>");

    body.push_str("<h2>Tiers</h2><table><tr><th>tier</th><th>offerings</th></tr>");
    for (tier, count) in &tiers {
        body.push_str(&format!("<tr><td>{}</td><td>{count}</td></tr>", esc(tier)));
    }
    body.push_str("</table>");

    let mut warnings = Vec::new();
    if no_index > 0 {
        warnings.push(format!(
            "{no_index} models have no AA index ({}) — they cannot be scored",
            pct(no_index, models_total)
        ));
    }
    if unpriced > 0 {
        warnings.push(format!(
            "{unpriced} offerings have no price ({}) — no score",
            pct(unpriced, total)
        ));
    }
    if scored < total {
        warnings.push(format!(
            "{} offerings have no score ({})",
            total - scored,
            pct(total - scored, total)
        ));
    }
    if with_popularity == 0 {
        warnings.push(
            "popularity missing (no RANKINGS_API_KEY at sync) — the factor is neutral".to_string(),
        );
    }
    body.push_str("<h2>Data quality</h2>");
    if warnings.is_empty() {
        body.push_str(
            "<p>No gaps: every model is indexed and every offering is priced and scored.</p>",
        );
    } else {
        body.push_str("<ul>");
        for warning in &warnings {
            body.push_str(&format!("<li>{}</li>", esc(warning)));
        }
        body.push_str("</ul>");
    }

    body.push_str(&bracket_html(db));
    body.push_str(&health_html(db));
    body.push_str(&formula_html());

    rows.sort_by(|a, b| compare_rank(&a.key(), &b.key()));
    match rows.iter().find(|row| row.score().is_some()) {
        Some(example) => body.push_str(&format!(
            "<h3>Worked example — {} / {}</h3><pre>{}</pre>",
            esc(&example.ep.model_id),
            esc(&example.ep.provider_name),
            esc(&breakdown(example))
        )),
        None => body.push_str("<p>No scored offering to show an example from.</p>"),
    }

    body.push_str(
        "<h2>Re-sync</h2>\
        <form method=\"post\" action=\"/resync\">\
        <button type=\"submit\">Force re-sync</button></form>\
        <p class=\"muted\">Runs a full sync + process + rank cycle now instead of \
        waiting for the next scheduled run; the gateway picks the new artifact \
        up within about a minute.</p>",
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
    Html(shell("overview", nav(), body))
}

/// Finite values only: corrupt stored floats display as `-`/zero,
/// never NaN or inf.
fn fin(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

/// TOP tab (Iter 46): model-level rollup — bracket members
/// score-desc, then flagged outsiders slug-asc. `?q=` filters model
/// ids, `?min=` keeps scores at/above a threshold (unparseable
/// means no floor, unscored rows fail a set floor).
fn top_body(db: &Catalog, version: &str, tabs: &str, search: &str, min: Option<&String>) -> String {
    let entries = pennyrouter_catalog::read_top_models(db);
    let floor: Option<f64> = min.and_then(|m| m.parse().ok());
    let mut body = format!(
        "<h2>Models (catalog {})</h2>{}\
        <form method=\"get\" action=\"/models\"><input type=\"hidden\" name=\"tab\" value=\"top\">\
        <input type=\"search\" name=\"q\" value=\"{}\" placeholder=\"filter model id\"> \
        <input type=\"text\" name=\"min\" value=\"{}\" placeholder=\"min score\" size=\"8\"> \
        <button type=\"submit\">filter</button></form>",
        esc(version),
        tabs,
        esc(search),
        esc(min.map(String::as_str).unwrap_or("")),
    );
    if entries.is_empty() {
        body.push_str("<p>No TOP data in this artifact (sync without scoring).</p>");
        return body;
    }
    let mut bracket: Vec<(&String, &TopModel)> = Vec::new();
    let mut flagged: Vec<(&String, &TopModel)> = Vec::new();
    for (id, entry) in entries.iter() {
        if !search.is_empty() && !id.to_lowercase().contains(search) {
            continue;
        }
        if let Some(floor) = floor {
            if entry.score.is_none_or(|s| s < floor) {
                continue;
            }
        }
        if entry.bracket {
            bracket.push((id, entry));
        } else if !entry.flags.is_empty() {
            flagged.push((id, entry));
        }
    }
    bracket.sort_by(|a, b| {
        b.1.score
            .partial_cmp(&a.1.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    flagged.sort_by(|a, b| a.0.cmp(b.0));
    let rows: Vec<(&String, &TopModel)> = bracket.into_iter().chain(flagged).collect();
    if rows.is_empty() {
        body.push_str("<p>No models match this tab.</p>");
        return body;
    }
    body.push_str(
        "<table><tr><th>#</th><th>model</th><th>score</th><th>quality</th>\
        <th>efficiency</th><th>activity</th><th>cost/task</th><th>basis</th>\
        <th>level</th><th>trend</th><th>bracket</th><th>flags</th></tr>",
    );
    const COLUMNS: usize = 12;
    for (position, (id, entry)) in rows.iter().enumerate() {
        let flags = if entry.flags.is_empty() {
            "<span class=\"muted\">-</span>".to_string()
        } else {
            esc(&entry.flags.join(", "))
        };
        body.push_str(&format!(
            "<tr><td><a class=\"ranknum\" href=\"#calc-{}\" title=\"show calculation\">{}</a></td>\
            <td><a href=\"/endpoints?model={}\">{}</a></td>\
            <td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
            <td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            position + 1,
            position + 1,
            url_encode(id),
            esc(id),
            score_cell(fin(entry.score)),
            num(fin(entry.quality), 2),
            num(fin(entry.efficiency), 3),
            num(fin(entry.activity), 3),
            fin(entry.cost_task)
                .map(|cost| format!("${cost:.6}"))
                .unwrap_or_else(|| "-".to_string()),
            esc(entry.basis.as_deref().unwrap_or("-")),
            num(fin(entry.level), 4),
            num(fin(entry.trend), 3),
            if entry.bracket {
                "yes"
            } else {
                "<span class=\"muted\">no</span>"
            },
            flags,
        ));
        body.push_str(&format!(
            "<tr id=\"calc-{}\" class=\"calc-row\"><td colspan=\"{}\">\
            <div class=\"calc\">calc — {}<br>{}</div>\
            <a href=\"#\" class=\"muted\">close</a></td></tr>",
            position + 1,
            COLUMNS,
            esc(id),
            esc(&top_breakdown(entry)).replace('\n', "<br>")
        ));
    }
    body.push_str("</table>");
    body
}

/// Model-level arithmetic from one TOP entry: the stored components
/// plus bracket membership and flags. Unscored rows explain via
/// their flags.
fn top_breakdown(entry: &TopModel) -> String {
    let flags = if entry.flags.is_empty() {
        "none".to_string()
    } else {
        entry.flags.join(", ")
    };
    let bracket = if entry.bracket { "yes" } else { "no" };
    let Some(score) = fin(entry.score) else {
        return format!("no score\nflags      = {flags}\nbracket    = {bracket}");
    };
    let quality = fin(entry.quality).unwrap_or(0.0);
    let efficiency = fin(entry.efficiency).unwrap_or(0.0);
    let activity = fin(entry.activity).unwrap_or(0.0);
    let cost_task = fin(entry.cost_task).unwrap_or(0.0);
    let activity_line = match (fin(entry.level), fin(entry.trend)) {
        (Some(level), Some(trend)) => {
            let boost = (trend - 1.0).clamp(-TREND_BOOST_MAX, TREND_BOOST_MAX);
            format!(
                "activity   = {:.4} × (1 + {:+.4}) / {:.2} = {:.4}",
                level,
                boost,
                1.0 + TREND_BOOST_MAX,
                activity
            )
        }
        _ => format!("activity   = {activity:.4} (neutral, unmeasured)"),
    };
    [
        format!(
            "quality    = {:.2} ({})",
            quality,
            entry.basis.as_deref().unwrap_or("-")
        ),
        format!("cost/task  = ${cost_task:.6} (stored)"),
        format!(
            "efficiency = {:.2} / ({:.6} + {:.2}) = {:.4}",
            COST_HALF_CREDIT, cost_task, COST_HALF_CREDIT, efficiency
        ),
        activity_line,
        format!(
            "rank_score = {:.2} × {:.4} × {:.4} = {:.1}",
            quality, efficiency, activity, score
        ),
        format!("bracket    = {bracket}"),
        format!("flags      = {flags}"),
    ]
    .join("\n")
}

async fn models(
    State(state): State<Arc<UiState>>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Html<String> {
    let Some(current) = current_db(&state.dir) else {
        return Html(shell(
            "models",
            nav(),
            "<p>No verifying catalog found.</p>".to_string(),
        ));
    };
    let db = &current.db;
    let signals = db.model_signals().unwrap_or_default();
    let endpoints = db.all_endpoints().unwrap_or_default();
    let ranks = db.endpoint_ranks().unwrap_or_default();
    let speeds = db.endpoint_speeds().unwrap_or_default();

    // Tiers is the default view: it is where most offerings live, and
    // the free tab is one click away.
    let tab = query.get("tab").map(String::as_str).unwrap_or("tiers");
    let tab = match tab {
        "free" | "batch" | "decision" | "top" => tab,
        _ => "tiers",
    };
    let search = query.get("q").cloned().unwrap_or_default().to_lowercase();

    let mut rows: Vec<Row> = endpoints
        .iter()
        .map(|ep| {
            Row::new(
                ep,
                signals.get(&ep.model_id),
                ranks.get(&ep.id),
                speeds.get(&ep.id),
            )
        })
        .collect();
    let batch_count = rows.iter().filter(|row| row.batch()).count();
    let free_count = rows
        .iter()
        .filter(|row| !row.batch() && !row.decision() && row.cost == Some(0.0))
        .count();
    let tier_count = rows
        .iter()
        .filter(|row| !row.batch() && !row.decision() && row.cost != Some(0.0))
        .count();
    let decision_count = rows
        .iter()
        .filter(|row| !row.batch() && row.decision())
        .count();

    rows.retain(|row| match tab {
        "batch" => row.batch(),
        "decision" => !row.batch() && row.decision(),
        "tiers" => !row.batch() && !row.decision() && row.cost != Some(0.0),
        _ => !row.batch() && !row.decision() && row.cost == Some(0.0),
    });
    if !search.is_empty() {
        rows.retain(|row| row.ep.model_id.to_lowercase().contains(&search));
    }
    rows.sort_by(|a, b| compare_rank(&a.key(), &b.key()));

    let tab_link = |name: &str, label: &str, count: usize| {
        let class = if name == tab { " class=\"active\"" } else { "" };
        format!("<a href=\"/models?tab={name}\"{class}>{label} ({count})</a>")
    };
    let top_entries = pennyrouter_catalog::read_top_models(db);
    let top_count = top_entries
        .iter()
        .filter(|(_, entry)| entry.bracket || !entry.flags.is_empty())
        .count();
    let tabs = format!(
        "<nav class=\"tabs\">{}{}{}{}{}</nav>",
        tab_link("free", "free", free_count),
        tab_link("tiers", "tiers", tier_count),
        tab_link("batch", "batch", batch_count),
        tab_link("decision", "decision", decision_count),
        tab_link("top", "top", top_count),
    );
    if tab == "top" {
        return Html(shell(
            "models",
            nav(),
            top_body(db, &current.version, &tabs, &search, query.get("min")),
        ));
    }
    let mut body = format!(
        "<h2>Models (catalog {})</h2>{}\
        <form method=\"get\" action=\"/models\"><input type=\"hidden\" name=\"tab\" value=\"{}\">\
        <input type=\"search\" name=\"q\" value=\"{}\" placeholder=\"filter model id\"> \
        <button type=\"submit\">filter</button></form>",
        esc(&current.version),
        tabs,
        esc(tab),
        esc(&search),
    );

    if rows.is_empty() {
        body.push_str("<p>No offerings in this tab.</p>");
        return Html(shell("models", nav(), body));
    }

    body.push_str(
        "<table><tr><th>#</th><th>model</th><th>provider</th><th>tag</th>\
        <th>precision</th><th>index</th><th>factor</th><th>quality</th>\
        <th>in $/M</th><th>out $/M</th><th>cost/task</th><th>efficiency</th>\
        <th>popularity</th><th>score</th><th>tok/s</th><th>latency</th>\
        <th>uptime</th></tr>",
    );
    // Data columns above; the breakdown row spans all of them.
    const COLUMNS: usize = 17;
    for (position, row) in rows.iter().enumerate() {
        let index = match row.index() {
            Some(value) => format!(
                "{value} <span class=\"muted\">{}</span>",
                esc(row
                    .rank
                    .and_then(|rank| rank.rank_basis.as_deref())
                    .unwrap_or("-"))
            ),
            None => "<span class=\"muted\">-</span>".to_string(),
        };
        body.push_str(&format!(
            "<tr><td><a class=\"ranknum\" href=\"#calc-{}\" title=\"show calculation\">{}</a></td>\
            <td><a href=\"/endpoints?model={}\">{}</a></td>\
            <td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.2}</td>\
            <td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
            <td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            position + 1,
            position + 1,
            url_encode(&row.ep.model_id),
            esc(&row.ep.model_id),
            esc(&row.ep.provider_name),
            esc(row.tag.as_deref().unwrap_or("-")),
            esc(row.ep.precision_class.as_deref().unwrap_or("-")),
            index,
            row.factor(),
            num(row.rank.and_then(|rank| rank.rank_quality), 2),
            row.ep
                .price_prompt
                .map(fmt_price)
                .unwrap_or_else(|| "-".to_string()),
            row.ep
                .price_completion
                .map(fmt_price)
                .unwrap_or_else(|| "-".to_string()),
            row.cost_task
                .map(|cost| format!("${cost:.5}"))
                .unwrap_or_else(|| "-".to_string()),
            num(row.rank.and_then(|rank| rank.rank_efficiency), 3),
            num(row.rank.and_then(|rank| rank.rank_popularity), 3),
            score_cell(row.score()),
            num(row.speed.and_then(|speed| speed.throughput_tps), 0),
            num(row.speed.and_then(|speed| speed.latency_ms), 0),
            num(row.speed.and_then(|speed| speed.uptime_pct), 1),
        ));
        body.push_str(&breakdown_row(position + 1, row, COLUMNS));
    }
    body.push_str("</table>");
    Html(shell("models", nav(), body))
}

async fn endpoints(
    State(state): State<Arc<UiState>>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Html<String> {
    let Some(current) = current_db(&state.dir) else {
        return Html(shell(
            "endpoints",
            nav(),
            "<p>No verifying catalog found.</p>".to_string(),
        ));
    };
    let filter = query.get("model").cloned().unwrap_or_default();
    let mut body = format!("<h2>Endpoints (catalog {})</h2>", esc(&current.version));
    match current.db.all_endpoints() {
        Err(e) => body.push_str(&format!("<p>DB error: {}</p>", esc(&e.to_string()))),
        Ok(endpoints) => {
            body.push_str(
                "<table><tr><th>model</th><th>provider</th><th>tag</th>\
                <th>prompt $/M</th><th>completion $/M</th><th>class</th>\
                <th>tier</th><th>score</th><th>scored by</th></tr>",
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
                let scored = match (&ep.score_model, &ep.score_prompt) {
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
                        .map(fmt_price)
                        .unwrap_or_else(|| "-".to_string()),
                    ep.price_completion
                        .map(fmt_price)
                        .unwrap_or_else(|| "-".to_string()),
                    ep.precision_class
                        .as_deref()
                        .filter(|c| *c != "unknown")
                        .map(esc)
                        .unwrap_or_else(|| "-".to_string()),
                    esc(ep.quality_tier.as_deref().unwrap_or("-")),
                    ep.quality_score
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or_else(|| "-".to_string()),
                    esc(&scored),
                ));
            }
            body.push_str("</table>");
        }
    }
    Html(shell("endpoints", nav(), body))
}

async fn meta(State(state): State<Arc<UiState>>) -> Html<String> {
    let Some(current) = current_db(&state.dir) else {
        return Html(shell(
            "meta",
            nav(),
            "<p>No verifying catalog found.</p>".to_string(),
        ));
    };
    let mut body = format!(
        "<h2>Rebuild / audit meta (catalog {})</h2>",
        esc(&current.version)
    );
    match current.db.all_meta() {
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

/// Force a re-sync (Iter 35): drop the trigger file and bounce back
/// to the overview banner. The sync loop stays the single writer —
/// the UI only asks. An unwritable artifact dir (pre-Iter-35 `:ro`
/// compose mounts) answers 500 naming the fix.
async fn resync(
    State(state): State<Arc<UiState>>,
) -> Result<Redirect, (axum::http::StatusCode, Html<String>)> {
    if let Err(e) = std::fs::write(trigger_path(&state.dir), "requested\n") {
        return Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Html(shell(
                "resync",
                nav(),
                format!(
                    "<p>Could not request a re-sync: {}</p>\
                    <p class=\"muted\">The artifact dir is not writable — redeploy \
                    so the catalog-ui service mounts it read-write.</p>\
                    <p><a href=\"/\">back to overview</a></p>",
                    esc(&e.to_string())
                ),
            )),
        ));
    }
    Ok(Redirect::to("/?resync=requested"))
}

/// Brand mark (shared penny SVG with the gateway): served locally
/// so browser icon fetches 200 instead of 404ing.
async fn favicon() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "image/svg+xml")],
        pennyrouter_catalog::FAVICON_SVG,
    )
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
    let mut bind: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
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
            "--bind" => {
                let raw = value(inline)?;
                bind = raw
                    .parse()
                    .map_err(|_| format!("--bind needs an IP address, got {raw:?}"))?
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
        .route("/favicon.ico", get(favicon))
        .route("/models", get(models))
        .route("/endpoints", get(endpoints))
        .route("/meta", get(meta))
        .route("/log", get(log))
        .route("/resync", post(resync))
        .with_state(state);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind((bind, port))
                .await
                .map_err(|e| format!("bind {bind}:{port}: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_render_per_million_tokens() {
        assert_eq!(fmt_price(0.0), "0");
        assert_eq!(fmt_price(1e-6), "1.00");
        assert_eq!(fmt_price(2.5e-6), "2.50");
        assert_eq!(fmt_price(1.0), "1000000.00");
        // Extra decimals only when needed for low values.
        assert_eq!(fmt_price(1.25e-7), "0.125");
        assert_eq!(fmt_price(4.4e-8), "0.044");
        // Floor: fixed down to $0.0001/M, then scientific.
        assert_eq!(fmt_price(2e-10), "0.0002");
        assert_eq!(fmt_price(1e-11), "1.00e-5");
    }
}
