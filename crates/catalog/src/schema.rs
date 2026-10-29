//! Schema v3: fixed core columns + raw JSON payloads + indexed columns
//! (deterministic from Phase 2a, model-judged from Phase 2b).

/// Current schema version, recorded in `sync_meta` on init.
pub const SCHEMA_VERSION: u32 = 3;

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
    price_prompt      REAL,
    price_completion  REAL,
    price_cache_read  REAL,
    price_cache_write REAL,
    precision_class   TEXT,
    promo_discount    REAL,
    processed_at      TEXT,
    quality_tier      TEXT,
    quality_score     REAL,
    judged_at         TEXT,
    judge_model       TEXT,
    judge_prompt      TEXT,
    FOREIGN KEY (model_id) REFERENCES models(model_id)
);
CREATE INDEX IF NOT EXISTS idx_endpoints_model ON endpoints(model_id);
CREATE TABLE IF NOT EXISTS sync_meta (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
";

/// Indexed columns added after v1: (name, type) pairs for migration.
/// Migration checks each column's presence, so one list covers v1 and
/// v2 databases alike.
pub const ENDPOINT_INDEXED_COLUMNS: &[(&str, &str)] = &[
    ("price_prompt", "REAL"),
    ("price_completion", "REAL"),
    ("price_cache_read", "REAL"),
    ("price_cache_write", "REAL"),
    ("precision_class", "TEXT"),
    ("promo_discount", "REAL"),
    ("processed_at", "TEXT"),
    ("quality_tier", "TEXT"),
    ("quality_score", "REAL"),
    ("judged_at", "TEXT"),
    ("judge_model", "TEXT"),
    ("judge_prompt", "TEXT"),
];

/// Phase 1 layout, kept so the migration test can build a legacy DB.
pub const V1_DDL: &str = "
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
