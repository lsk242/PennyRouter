//! Catalog DB: schema, store, sync, processing, scoring, ranking,
//! pricing, and artifact publishing.
//!
//! The catalog sync is a separate process from the gateway; this crate owns
//! the DB format both sides share. See `docs/catalog-sync-architecture.md`.

mod artifact;
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

pub use artifact::{now_rfc3339, publish, sha256_hex, verify, Manifest};
pub use fetch::{fetch_openrouter, fetch_openrouter_extra, Fetched};
pub use popular::{
    aggregate_apps, aggregate_models, aggregate_models_all, fetch_popularity, read_popular_tokens,
    read_popularity, store_popularity, Popularity, POPULAR_APPS_TOP_N, POPULAR_MODELS_TOP_N,
    RANKINGS_WINDOW_DAYS,
};
pub use price::{
    balanced_profile, builtin_profiles, effective_cost, fmt_price_per_m, resolve_discount, Promos,
    WorkloadProfile,
};
pub use process::{run_processing, ProcessSummary};
pub use rank::{
    compare as compare_signals, load_ranking_doc, publish_ranking, rank_catalog, verify_ranking,
    Assist, ClassRanking, FreeOffer, RankEntry, RankOrientation, RankingDocument, RankingManifest,
    Signals, TrendStats, Trends,
};
pub use rank_score::{
    compare as compare_rank, cost_per_task, efficiency, popularity_factor, precision_factor,
    RankInputs, RankKey, RankScore, COST_HALF_CREDIT, POP_WEIGHT,
    RULESET_VERSION as RANK_RULESET_VERSION, TASK_TOKENS,
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
