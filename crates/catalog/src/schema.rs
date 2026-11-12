//! Schema v8: fixed core columns + raw JSON payloads + indexed columns
//! (deterministic from Phase 2a, model-scored from Phase 2b, tool
//! support from Iter 22, decision-modality support from Iter 24,
//! Artificial Analysis indexes from Iter 31, scoring-audit rename
//! from Iter 32, speed/availability + value score from Iter 34).

/// Current schema version, recorded in `sync_meta` on init.
pub const SCHEMA_VERSION: u32 = 8;

pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS models (
    model_id    TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    source      TEXT NOT NULL,
    payload     TEXT NOT NULL,
    fetched_at  TEXT NOT NULL,
    aa_intelligence_index INTEGER,
    aa_coding_index       INTEGER,
    aa_agentic_index      INTEGER,
    aa_cost_per_task      REAL,
    popularity_tokens     INTEGER,
    popularity_share      REAL
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
    supports_tools    INTEGER,
    supports_decisions INTEGER,
    quality_tier      TEXT,
    quality_score     REAL,
    scored_at         TEXT,
    score_model       TEXT,
    score_prompt      TEXT,
    price_override_tokens INTEGER,
    latency_ms        REAL,
    throughput_tps    REAL,
    uptime_pct        REAL,
    rank_score        REAL,
    rank_quality      REAL,
    rank_efficiency   REAL,
    rank_popularity   REAL,
    rank_basis        TEXT,
    rank_ruleset      TEXT,
    ranked_at         TEXT,
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
    ("supports_tools", "INTEGER"),
    ("supports_decisions", "INTEGER"),
    ("quality_tier", "TEXT"),
    ("quality_score", "REAL"),
    ("scored_at", "TEXT"),
    ("score_model", "TEXT"),
    ("score_prompt", "TEXT"),
    ("price_override_tokens", "INTEGER"),
    ("latency_ms", "REAL"),
    ("throughput_tps", "REAL"),
    ("uptime_pct", "REAL"),
    ("rank_score", "REAL"),
    ("rank_quality", "REAL"),
    ("rank_efficiency", "REAL"),
    ("rank_popularity", "REAL"),
    ("rank_basis", "TEXT"),
    ("rank_ruleset", "TEXT"),
    ("ranked_at", "TEXT"),
];

/// Indexed columns on `models` (Iter 31) — mirrors the endpoints
/// pattern. Migration checks each column's presence, so one list
/// covers v1..v5 databases alike.
pub const MODEL_INDEXED_COLUMNS: &[(&str, &str)] = &[
    ("aa_intelligence_index", "INTEGER"),
    ("aa_coding_index", "INTEGER"),
    ("aa_agentic_index", "INTEGER"),
    ("aa_cost_per_task", "REAL"),
    ("popularity_tokens", "INTEGER"),
    ("popularity_share", "REAL"),
];

/// Endpoint columns renamed in v7 (Iter 32: judging → scoring), as
/// (old, new) pairs. Migration renames when the old column exists
/// so v6 verdicts carry over; databases old enough to have neither
/// take the plain ADD path from `ENDPOINT_INDEXED_COLUMNS`.
pub const ENDPOINT_RENAMED_COLUMNS: &[(&str, &str)] = &[
    ("judged_at", "scored_at"),
    ("judge_model", "score_model"),
    ("judge_prompt", "score_prompt"),
];

/// Scoring meta keys renamed in v7 alongside the columns above.
/// Migration deletes the legacy keys; current runs rewrite them
/// under the new names.
pub const LEGACY_SCORE_META_KEYS: &[&str] = &[
    "judged_at",
    "judged_count",
    "judge_model",
    "judge_prompt",
    "judge_backend",
    "judge_calls",
    "judge_spend_usd",
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
