//! Catalog DB: schema, store, sync, processing, scoring, ranking,
//! pricing, and artifact publishing.
//!
//! The catalog sync is a separate process from the gateway; this crate owns
//! the DB format both sides share. See `docs/catalog-sync-architecture.md`.

mod aa;
mod artifact;
mod benchmarks;
mod fetch;
mod popular;
mod price;
mod process;
mod rank;
mod rank_score;
mod schema;
mod score;
mod score_baseten;
mod score_catalog;
mod score_modal;
mod score_offline;
mod score_openai;
mod score_openrouter;
mod store;
mod sync;

pub use aa::{
    fetch_aa_models, read_aa_intel_version, read_aa_models, store_aa_models, AaModel, AaModels,
};
pub use artifact::{
    now_rfc3339, publish, read_fetch_health, record_fetch_health, sha256_hex, verify, Manifest,
    FETCH_SOURCES,
};
pub use benchmarks::{
    aggregate_intelligence, aggregate_openrouter_evals, aggregate_task_index, fetch_benchmarks,
    fetch_benchmarks_task, read_benchmark_code, read_benchmark_intel, read_openrouter_evals,
    store_benchmarks, store_openrouter_evals, store_task_benchmarks, Benchmarks, OpenRouterEvals,
};
pub use fetch::{fetch_openrouter, fetch_openrouter_extra, Fetched};
pub use popular::{
    aggregate_activity, aggregate_apps, aggregate_models, aggregate_models_all,
    aggregate_task_code_share, fetch_app_rankings, fetch_popularity, fetch_task_share,
    read_activity, read_app_list, read_popular_tokens, read_popularity, read_task_code_share,
    split_ranking_windows, store_app_list, store_popularity, store_task_share, AppList,
    ModelActivity, Popularity, TaskShare, POPULAR_APPS_TOP_N, POPULAR_MODELS_TOP_N,
    RANKINGS_SPAN_DAYS, RANKINGS_WINDOW_DAYS,
};
pub use price::{
    balanced_profile, builtin_profiles, effective_cost, fmt_price_per_m, resolve_discount, Promos,
    WorkloadProfile,
};
pub use process::{
    read_agent_top20, read_bracket_members, read_top_models, run_processing, ProcessSummary,
    TopModel,
};
pub use rank::{
    compare as compare_signals, load_ranking_doc, publish_ranking, rank_catalog, verify_ranking,
    Assist, ClassRanking, FreeOffer, RankEntry, RankOrientation, RankingDocument, RankingManifest,
    Signals, TrendStats, Trends,
};
pub use rank_score::{
    activity_factor, compare as compare_rank, cost_per_task, efficiency, precision_factor,
    RankInputs, RankKey, RankScore, ACTIVITY_UNKNOWN, BRACKET_MIN_SHARE, BRACKET_PERFORMANCE_FLOOR,
    BRACKET_TOP_N, COST_HALF_CREDIT, POP_WEIGHT, RULESET_VERSION as RANK_RULESET_VERSION,
    TASK_TOKENS, THINK_MAX_COST_TASK, TREND_BOOST_MAX,
};
pub use schema::{SCHEMA_VERSION, V1_DDL};
pub use score::{
    endpoint_tag, run_scoring, run_scoring_selected, NullScorer, RecordedScorer, Scorer,
    ScorerIdentity, ScorerSummary,
};
pub use score_baseten::BasetenScorer;
pub use score_catalog::{
    build_score, plan_live_calls, select_endpoints, spend_line, ScorerCatalog, ScorerEntry,
    ScorerPlan, ScorerPolicy, KNOWN_BACKENDS,
};
pub use score_modal::ModalScorer;
pub use score_offline::{OfflineScorer, RULESET_VERSION};
pub use score_openai::PROMPT_VERSION;
pub use score_openrouter::OpenRouterScorer;
pub use store::{
    Catalog, EndpointIndex, EndpointRank, EndpointRow, EndpointScore, EndpointSpeed, ModelIndex,
    ModelPayload, ModelPopularity, ModelRow, ModelSignals,
};
pub use sync::{load_endpoints_dir, run_fixture_sync, Summary};

use std::fmt;

/// Catalog error: sqlite, IO, JSON, time, schema, malformed input,
/// live-gather or live-score failures (transport vs. backend behavior).
#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
    TimeFormat(time::error::Format),
    Schema(String),
    InvalidFixture(String),
    Http(reqwest::Error),
    Fetch(String),
    Scorer(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "sqlite: {e}"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Json(e) => write!(f, "json: {e}"),
            Error::TimeFormat(e) => write!(f, "time format: {e}"),
            Error::Schema(e) => write!(f, "schema: {e}"),
            Error::InvalidFixture(e) => write!(f, "invalid fixture: {e}"),
            Error::Http(e) => write!(f, "http: {e}"),
            Error::Fetch(e) => write!(f, "fetch: {e}"),
            Error::Scorer(e) => write!(f, "score: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            Error::TimeFormat(_) => None,
            Error::Schema(_) => None,
            Error::InvalidFixture(_) => None,
            Error::Http(e) => Some(e),
            Error::Fetch(_) => None,
            Error::Scorer(_) => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

impl From<time::error::Format> for Error {
    fn from(e: time::error::Format) -> Self {
        Error::TimeFormat(e)
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

/// Shared brand mark for the gateway and catalog UIs: a copper penny
/// `P` on the arcade-theme background, one inline SVG so both
/// binaries serve the identical bytes with no asset files.
pub const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" fill="#0d0f1e"/><circle cx="32" cy="32" r="24" fill="#c87533"/><circle cx="32" cy="32" r="24" fill="none" stroke="#7a4a1f" stroke-width="4"/><circle cx="32" cy="32" r="16" fill="none" stroke="#7a4a1f" stroke-width="2"/><text x="32" y="43" font-family="monospace" font-size="30" font-weight="bold" text-anchor="middle" fill="#0d0f1e">P</text></svg>"##;
