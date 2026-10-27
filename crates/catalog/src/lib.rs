//! Catalog DB: schema, store, sync, processing, and artifact publishing.
//!
//! The catalog sync is a separate process from the gateway; this crate owns
//! the DB format both sides share. See `docs/catalog-sync-architecture.md`.

mod artifact;
mod process;
mod schema;
mod store;
mod sync;

pub use artifact::{now_rfc3339, publish, sha256_hex, verify, Manifest};
pub use process::{run_processing, ProcessSummary};
pub use schema::{SCHEMA_VERSION, V1_DDL};
pub use store::{Catalog, EndpointIndex, EndpointRow, ModelRow};
pub use sync::{load_endpoints_dir, run_fixture_sync, Summary};

use std::fmt;

/// Catalog error: sqlite, IO, JSON, time, schema, or malformed input.
#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
    TimeFormat(time::error::Format),
    Schema(String),
    InvalidFixture(String),
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
