//! Catalog DB: schema, store, sync, processing, judging, ranking,
//! pricing, and artifact publishing.
//!
//! The catalog sync is a separate process from the gateway; this crate owns
//! the DB format both sides share. See `docs/catalog-sync-architecture.md`.

mod artifact;
mod fetch;
mod judge;
mod judge_baseten;
mod judge_catalog;
mod judge_modal;
mod judge_openai;
mod price;
mod process;
mod rank;
mod schema;
mod store;
mod sync;

pub use artifact::{now_rfc3339, publish, sha256_hex, verify, Manifest};
pub use fetch::{fetch_openrouter, Fetched};
pub use judge::{
    endpoint_tag, run_judging, run_judging_selected, Judge, JudgeIdentity, JudgeSummary, NullJudge,
    RecordedJudge,
};
pub use judge_baseten::BasetenJudge;
pub use judge_catalog::{
    build_judge, plan_live_calls, select_endpoints, spend_line, JudgeCatalog, JudgeEntry,
    JudgePlan, JudgePolicy, KNOWN_BACKENDS,
};
pub use judge_modal::ModalJudge;
pub use judge_openai::PROMPT_VERSION;
pub use price::{
    balanced_profile, builtin_profiles, effective_cost, resolve_discount, Promos, WorkloadProfile,
};
pub use process::{run_processing, ProcessSummary};
pub use rank::{
    compare as compare_signals, load_ranking_doc, publish_ranking, rank_catalog, verify_ranking,
    Assist, ClassRanking, FreeOffer, RankEntry, RankOrientation, RankingDocument, RankingManifest,
    Signals, TrendStats, Trends,
};
pub use schema::{SCHEMA_VERSION, V1_DDL};
pub use store::{Catalog, EndpointIndex, EndpointJudgment, EndpointRow, ModelRow};
pub use sync::{load_endpoints_dir, run_fixture_sync, Summary};

use std::fmt;

/// Catalog error: sqlite, IO, JSON, time, schema, malformed input,
/// live-gather or live-judge failures (transport vs. backend behavior).
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
    Judge(String),
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
            Error::Judge(e) => write!(f, "judge: {e}"),
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
            Error::Judge(_) => None,
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
