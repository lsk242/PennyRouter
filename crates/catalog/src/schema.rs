//! Schema v1: fixed core columns + raw JSON payloads.

/// Current schema version, recorded in `sync_meta` on init.
pub const SCHEMA_VERSION: u32 = 1;

pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS models (
    model_id    TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    source      TEXT NOT NULL,
    payload     TEXT NOT NULL,
    fetched_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS endpoints (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    model_id       TEXT NOT NULL,
    provider_name  TEXT NOT NULL,
    payload        TEXT NOT NULL,
    fetched_at     TEXT NOT NULL,
    FOREIGN KEY (model_id) REFERENCES models(model_id)
);
CREATE INDEX IF NOT EXISTS idx_endpoints_model ON endpoints(model_id);
CREATE TABLE IF NOT EXISTS sync_meta (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
";
