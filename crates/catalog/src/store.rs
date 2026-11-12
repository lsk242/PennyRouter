//! SQLite store: open/init, inserts, and read queries.

use crate::schema::{
    DDL, ENDPOINT_INDEXED_COLUMNS, ENDPOINT_RENAMED_COLUMNS, LEGACY_SCORE_META_KEYS,
    MODEL_INDEXED_COLUMNS, SCHEMA_VERSION,
};
use crate::Error;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashMap;
use std::path::Path;

pub struct ModelRow {
    pub model_id: String,
    pub name: String,
    pub source: String,
    pub payload: String,
    pub fetched_at: String,
}

/// Model id + raw payload (see `all_models`).
pub struct ModelPayload {
    pub model_id: String,
    pub payload: String,
}

#[derive(Clone)]
pub struct EndpointRow {
    pub id: i64,
    pub model_id: String,
    pub provider_name: String,
    pub payload: String,
    pub fetched_at: String,
    pub price_prompt: Option<f64>,
    pub price_completion: Option<f64>,
    pub price_cache_read: Option<f64>,
    pub price_cache_write: Option<f64>,
    pub precision_class: Option<String>,
    pub promo_discount: Option<f64>,
    pub processed_at: Option<String>,
    pub supports_tools: Option<i64>,
    pub supports_decisions: Option<i64>,
    pub quality_tier: Option<String>,
    pub quality_score: Option<f64>,
    pub scored_at: Option<String>,
    pub score_model: Option<String>,
    pub score_prompt: Option<String>,
    pub price_override_tokens: Option<i64>,
}

/// Indexed values distilled from an endpoint payload by processing.
pub struct EndpointIndex {
    pub price_prompt: Option<f64>,
    pub price_completion: Option<f64>,
    pub price_cache_read: Option<f64>,
    pub price_cache_write: Option<f64>,
    pub precision_class: Option<String>,
    pub promo_discount: Option<f64>,
    pub processed_at: String,
    pub supports_tools: Option<i64>,
    pub supports_decisions: Option<i64>,
    pub price_override_tokens: Option<i64>,
}

/// Artificial Analysis indexes distilled from a model payload by
/// processing (Iter 31): `benchmarks.artificial_analysis.*`,
/// rounded to `0..=100`, or `None` when absent/non-numeric/out of
/// range.
pub struct ModelIndex {
    pub aa_intelligence_index: Option<i64>,
    pub aa_coding_index: Option<i64>,
    pub aa_agentic_index: Option<i64>,
    /// AA's own cost per task when the payload carries it (Iter 34);
    /// absent in today's payloads, reserved so it wins when it lands.
    pub aa_cost_per_task: Option<f64>,
}

/// Speed and availability signals distilled from an endpoint payload
/// (Iter 34). Every field is optional: a missing metric is unknown,
/// never zero.
#[derive(Clone, Default)]
pub struct EndpointSpeed {
    pub latency_ms: Option<f64>,
    pub throughput_tps: Option<f64>,
    pub uptime_pct: Option<f64>,
}

/// Value-score components for one endpoint (Iter 34). Stored rather
/// than recomputed so the UI can show the arithmetic and a later
/// gateway can read the score with one column.
#[derive(Clone, Default)]
pub struct EndpointRank {
    pub rank_score: Option<f64>,
    pub rank_quality: Option<f64>,
    pub rank_efficiency: Option<f64>,
    pub rank_popularity: Option<f64>,
    pub rank_basis: Option<String>,
    pub rank_ruleset: Option<String>,
    pub ranked_at: Option<String>,
}

/// Per-model signals the value score reads (Iter 34).
#[derive(Clone, Default)]
pub struct ModelSignals {
    pub aa_intelligence_index: Option<i64>,
    pub aa_coding_index: Option<i64>,
    pub aa_agentic_index: Option<i64>,
    pub aa_cost_per_task: Option<f64>,
    pub popularity_tokens: Option<i64>,
    pub popularity_share: Option<f64>,
}

/// Popularity facts for one model, from the rankings window (Iter 34).
pub struct ModelPopularity {
    pub popularity_tokens: Option<i64>,
    pub popularity_share: Option<f64>,
}

/// Model-scored signals distilled by scoring (Phase 2b), plus the
/// audit identity of the scorer that produced them (Iter 15: model +
/// prompt version with every score; `None` on rows scored before
/// the audit columns existed).
pub struct EndpointScore {
    pub quality_tier: Option<String>,
    pub quality_score: Option<f64>,
    pub scored_at: String,
    pub score_model: Option<String>,
    pub score_prompt: Option<String>,
}

const ENDPOINT_COLS: &str = "id, model_id, provider_name, payload, fetched_at,
    price_prompt, price_completion, price_cache_read, price_cache_write,
    precision_class, promo_discount, processed_at, supports_tools,
    supports_decisions, quality_tier, quality_score, scored_at, score_model, score_prompt,
    price_override_tokens";

fn endpoint_row(r: &Row) -> Result<EndpointRow, rusqlite::Error> {
    Ok(EndpointRow {
        id: r.get(0)?,
        model_id: r.get(1)?,
        provider_name: r.get(2)?,
        payload: r.get(3)?,
        fetched_at: r.get(4)?,
        price_prompt: r.get(5)?,
        price_completion: r.get(6)?,
        price_cache_read: r.get(7)?,
        price_cache_write: r.get(8)?,
        precision_class: r.get(9)?,
        promo_discount: r.get(10)?,
        processed_at: r.get(11)?,
        supports_tools: r.get(12)?,
        supports_decisions: r.get(13)?,
        quality_tier: r.get(14)?,
        quality_score: r.get(15)?,
        scored_at: r.get(16)?,
        score_model: r.get(17)?,
        score_prompt: r.get(18)?,
        price_override_tokens: r.get(19)?,
    })
}

pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        Ok(Self { conn })
    }

    /// Open an existing DB read-only: never creates, migrates, or locks
    /// for writing. The gateway serving path uses this against the
    /// shared artifact. Fails closed when the schema is missing,
    /// uninitialized, or not exactly the current version — a
    /// read-only handle cannot migrate, so anything older would
    /// break queries with confusing errors later.
    pub fn open_read_only(path: &Path) -> Result<Self, Error> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let catalog = Self { conn };
        catalog.check_schema()?;
        Ok(catalog)
    }

    /// Read-only schema gate: same tables and version `init` would
    /// produce, but without writing anything.
    fn check_schema(&self) -> Result<(), Error> {
        for table in ["models", "endpoints", "sync_meta"] {
            let present: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )?;
            if !present {
                return Err(Error::Schema(format!(
                    "db is missing table {table:?}: not a catalog database"
                )));
            }
        }
        let stored = self.get_meta("schema_version")?;
        let version: u32 = match stored {
            Some(value) => value.parse().map_err(|_| {
                Error::Schema(format!(
                    "unparseable schema_version in sync_meta: {value:?}"
                ))
            })?,
            None => {
                return Err(Error::Schema(
                    "db has no schema_version: not an initialized catalog".to_string(),
                ))
            }
        };
        if version != SCHEMA_VERSION {
            return Err(Error::Schema(format!(
                "db schema v{version} does not match this binary (v{SCHEMA_VERSION})"
            )));
        }
        Ok(())
    }

    /// Run `f` inside one SQLite transaction (BEGIN IMMEDIATE):
    /// commit on `Ok`, roll back on `Err`. Raw SQL rather than
    /// `rusqlite::Transaction` so callers keep `&self` (the handle
    /// never needs `&mut`). Callers must not nest: `f` may only run
    /// plain statements, never another `transaction` call.
    pub fn transaction<T>(&self, f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f() {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Create tables if missing, migrate older DBs, record the schema
    /// version. Idempotent. Required before any other call.
    pub fn init(&self) -> Result<(), Error> {
        self.conn.execute_batch(DDL)?;
        self.migrate()?;
        Ok(())
    }

    /// Bring an older DB to the current schema. Idempotent.
    fn migrate(&self) -> Result<(), Error> {
        let stored = self.get_meta("schema_version")?;
        let version: u32 = match stored {
            Some(v) => v.parse().map_err(|_| {
                Error::Schema(format!("unparseable schema_version in sync_meta: {v:?}"))
            })?,
            // Fresh DB: DDL is already current.
            None => SCHEMA_VERSION,
        };
        if version > SCHEMA_VERSION {
            return Err(Error::Schema(format!(
                "db schema v{version} is newer than this binary (v{SCHEMA_VERSION})"
            )));
        }
        // Table/column names are internal constants, never user input.
        // v7 renames first: a v6 database carries verdicts under the
        // old column names, and the ADD pass below must see the new
        // ones already in place.
        for (old, new) in ENDPOINT_RENAMED_COLUMNS {
            if self.has_column("endpoints", old)? && !self.has_column("endpoints", new)? {
                self.conn.execute_batch(&format!(
                    "ALTER TABLE endpoints RENAME COLUMN {old} TO {new}"
                ))?;
            }
        }
        for (name, ty) in ENDPOINT_INDEXED_COLUMNS {
            if !self.has_column("endpoints", name)? {
                self.conn
                    .execute_batch(&format!("ALTER TABLE endpoints ADD COLUMN {name} {ty}"))?;
            }
        }
        for (name, ty) in MODEL_INDEXED_COLUMNS {
            if !self.has_column("models", name)? {
                self.conn
                    .execute_batch(&format!("ALTER TABLE models ADD COLUMN {name} {ty}"))?;
            }
        }
        for key in LEGACY_SCORE_META_KEYS {
            self.conn
                .execute("DELETE FROM sync_meta WHERE key = ?1", params![key])?;
        }
        self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(())
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool, Error> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in names {
            if name? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Insert or replace: re-syncs overwrite, never duplicate.
    pub fn insert_model(&self, row: &ModelRow) -> Result<(), Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO models (model_id, name, source, payload, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                row.model_id,
                row.name,
                row.source,
                row.payload,
                row.fetched_at
            ],
        )?;
        Ok(())
    }

    /// Plain insert of the core columns (indexed columns stay NULL until
    /// processing): endpoints carry a surrogate key because one provider
    /// can serve a model several ways (e.g. `openai` vs `openai/flex` tags).
    /// Syncs replace per model via `delete_endpoints_for` first.
    pub fn insert_endpoint(&self, row: &EndpointRow) -> Result<(), Error> {
        self.conn.execute(
            "INSERT INTO endpoints (model_id, provider_name, payload, fetched_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![row.model_id, row.provider_name, row.payload, row.fetched_at],
        )?;
        Ok(())
    }

    /// Drop a model's endpoints so a re-sync mirrors the source exactly.
    pub fn delete_endpoints_for(&self, model_id: &str) -> Result<(), Error> {
        self.conn.execute(
            "DELETE FROM endpoints WHERE model_id = ?1",
            params![model_id],
        )?;
        Ok(())
    }

    /// Drop one model row. Callers delete its endpoints first: the
    /// foreign key has no cascade.
    pub fn delete_model(&self, model_id: &str) -> Result<(), Error> {
        self.conn
            .execute("DELETE FROM models WHERE model_id = ?1", params![model_id])?;
        Ok(())
    }

    /// Fill the AA index columns of one model, leaving the payload
    /// alone (Iter 31).
    pub fn update_model_index(&self, model_id: &str, index: &ModelIndex) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE models SET aa_intelligence_index = ?1, aa_coding_index = ?2,
             aa_agentic_index = ?3, aa_cost_per_task = ?4 WHERE model_id = ?5",
            params![
                index.aa_intelligence_index,
                index.aa_coding_index,
                index.aa_agentic_index,
                index.aa_cost_per_task,
                model_id
            ],
        )?;
        Ok(())
    }

    /// Coding index per model where known, for the gateway snapshot.
    pub fn aa_coding_map(&self) -> Result<HashMap<String, i64>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT model_id, aa_coding_index FROM models WHERE aa_coding_index IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let index: i64 = r.get(1)?;
            Ok((id, index))
        })?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(Error::from)
    }

    /// Every stored model id, for re-sync pruning.
    pub fn all_model_ids(&self) -> Result<Vec<String>, Error> {
        let mut stmt = self.conn.prepare("SELECT model_id FROM models")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    /// Every model id + raw payload, for the processing model loop
    /// (Iter 31). Indexed columns stay out: the loop only reads the
    /// payload and writes through `update_model_index`.
    pub fn all_models(&self) -> Result<Vec<ModelPayload>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT model_id, payload FROM models ORDER BY model_id")?;
        let rows = stmt.query_map([], |r| {
            Ok(ModelPayload {
                model_id: r.get(0)?,
                payload: r.get(1)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    /// Fill the indexed columns of one endpoint, leaving the payload alone.
    pub fn update_endpoint_index(&self, id: i64, index: &EndpointIndex) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE endpoints SET price_prompt = ?1, price_completion = ?2,
             price_cache_read = ?3, price_cache_write = ?4, precision_class = ?5,
             promo_discount = ?6, processed_at = ?7, supports_tools = ?8,
             supports_decisions = ?9, price_override_tokens = ?10 WHERE id = ?11",
            params![
                index.price_prompt,
                index.price_completion,
                index.price_cache_read,
                index.price_cache_write,
                index.precision_class,
                index.promo_discount,
                index.processed_at,
                index.supports_tools,
                index.supports_decisions,
                index.price_override_tokens,
                id
            ],
        )?;
        Ok(())
    }

    /// Fill the score columns of one endpoint, leaving the payload
    /// and the deterministic indexed columns alone.
    pub fn update_endpoint_score(&self, id: i64, score: &EndpointScore) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE endpoints SET quality_tier = ?1, quality_score = ?2,
             scored_at = ?3, score_model = ?4, score_prompt = ?5 WHERE id = ?6",
            params![
                score.quality_tier,
                score.quality_score,
                score.scored_at,
                score.score_model,
                score.score_prompt,
                id
            ],
        )?;
        Ok(())
    }

    /// Fill the speed/availability columns of one endpoint (Iter 34).
    pub fn update_endpoint_speed(&self, id: i64, speed: &EndpointSpeed) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE endpoints SET latency_ms = ?1, throughput_tps = ?2,
             uptime_pct = ?3 WHERE id = ?4",
            params![speed.latency_ms, speed.throughput_tps, speed.uptime_pct, id],
        )?;
        Ok(())
    }

    /// Fill the value-score columns of one endpoint (Iter 34).
    pub fn update_endpoint_rank(&self, id: i64, rank: &EndpointRank) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE endpoints SET rank_score = ?1, rank_quality = ?2,
             rank_efficiency = ?3, rank_popularity = ?4, rank_basis = ?5,
             rank_ruleset = ?6, ranked_at = ?7 WHERE id = ?8",
            params![
                rank.rank_score,
                rank.rank_quality,
                rank.rank_efficiency,
                rank.rank_popularity,
                rank.rank_basis,
                rank.rank_ruleset,
                rank.ranked_at,
                id
            ],
        )?;
        Ok(())
    }

    /// Fill the popularity columns of one model (Iter 34).
    pub fn update_model_popularity(
        &self,
        model_id: &str,
        popularity: &ModelPopularity,
    ) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE models SET popularity_tokens = ?1, popularity_share = ?2
             WHERE model_id = ?3",
            params![
                popularity.popularity_tokens,
                popularity.popularity_share,
                model_id
            ],
        )?;
        Ok(())
    }

    /// Speed/availability per endpoint id, for the ranking rebuild and
    /// the catalog UI (Iter 34).
    pub fn endpoint_speeds(&self) -> Result<HashMap<i64, EndpointSpeed>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, latency_ms, throughput_tps, uptime_pct FROM endpoints")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                EndpointSpeed {
                    latency_ms: r.get(1)?,
                    throughput_tps: r.get(2)?,
                    uptime_pct: r.get(3)?,
                },
            ))
        })?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(Error::from)
    }

    /// Value-score components per endpoint id (Iter 34).
    pub fn endpoint_ranks(&self) -> Result<HashMap<i64, EndpointRank>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, rank_score, rank_quality, rank_efficiency, rank_popularity,
             rank_basis, rank_ruleset, ranked_at FROM endpoints",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                EndpointRank {
                    rank_score: r.get(1)?,
                    rank_quality: r.get(2)?,
                    rank_efficiency: r.get(3)?,
                    rank_popularity: r.get(4)?,
                    rank_basis: r.get(5)?,
                    rank_ruleset: r.get(6)?,
                    ranked_at: r.get(7)?,
                },
            ))
        })?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(Error::from)
    }

    /// Per-model signals the value score and the catalog UI read
    /// (Iter 34): AA indexes, AA cost per task, popularity.
    pub fn model_signals(&self) -> Result<HashMap<String, ModelSignals>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT model_id, aa_intelligence_index, aa_coding_index, aa_agentic_index,
             aa_cost_per_task, popularity_tokens, popularity_share FROM models",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ModelSignals {
                    aa_intelligence_index: r.get(1)?,
                    aa_coding_index: r.get(2)?,
                    aa_agentic_index: r.get(3)?,
                    aa_cost_per_task: r.get(4)?,
                    popularity_tokens: r.get(5)?,
                    popularity_share: r.get(6)?,
                },
            ))
        })?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(Error::from)
    }

    pub fn get_model(&self, model_id: &str) -> Result<Option<ModelRow>, Error> {
        self.conn
            .query_row(
                "SELECT model_id, name, source, payload, fetched_at
                 FROM models WHERE model_id = ?1",
                params![model_id],
                |r| {
                    Ok(ModelRow {
                        model_id: r.get(0)?,
                        name: r.get(1)?,
                        source: r.get(2)?,
                        payload: r.get(3)?,
                        fetched_at: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }

    pub fn endpoints_for(&self, model_id: &str) -> Result<Vec<EndpointRow>, Error> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ENDPOINT_COLS} FROM endpoints WHERE model_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![model_id], endpoint_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    pub fn all_endpoints(&self) -> Result<Vec<EndpointRow>, Error> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ENDPOINT_COLS} FROM endpoints ORDER BY id"
        ))?;
        let rows = stmt.query_map([], endpoint_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    /// (model_count, endpoint_count).
    pub fn counts(&self) -> Result<(u64, u64), Error> {
        // rusqlite 0.41 dropped `FromSql for u64`; counts are
        // non-negative, so read i64 and widen.
        let models: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM models", [], |r| r.get(0))?;
        let endpoints: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM endpoints", [], |r| r.get(0))?;
        Ok((models as u64, endpoints as u64))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sync_meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Every sync_meta row, ordered by key (Iter 17: the catalog UI
    /// shows the full rebuild/audit trail, including keys added by
    /// later iterations).
    pub fn all_meta(&self) -> Result<Vec<(String, String)>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM sync_meta ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, Error> {
        self.conn
            .query_row(
                "SELECT value FROM sync_meta WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent_and_records_version() {
        let catalog = Catalog::open(Path::new(":memory:")).unwrap();
        catalog.init().unwrap();
        catalog.init().unwrap();
        assert_eq!(
            catalog.get_meta("schema_version").unwrap().as_deref(),
            Some("8")
        );
        assert_eq!(catalog.counts().unwrap(), (0, 0));
    }

    fn temp_db(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "pennyrouter-b13-{name}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn read_only_rejects_missing_schema() {
        let path = temp_db("missing");
        drop(Catalog::open(&path).unwrap()); // creates the file, no init
        let err = match Catalog::open_read_only(&path) {
            Ok(_) => panic!("expected schema rejection"),
            Err(e) => e,
        };
        assert!(matches!(err, Error::Schema(_)), "unexpected: {err:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_only_rejects_newer_schema() {
        let path = temp_db("newer");
        let catalog = Catalog::open(&path).unwrap();
        catalog.init().unwrap();
        catalog.set_meta("schema_version", "999").unwrap();
        drop(catalog);
        let err = match Catalog::open_read_only(&path) {
            Ok(_) => panic!("expected schema rejection"),
            Err(e) => e,
        };
        assert!(matches!(err, Error::Schema(_)), "unexpected: {err:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_only_accepts_current_schema() {
        let path = temp_db("current");
        let catalog = Catalog::open(&path).unwrap();
        catalog.init().unwrap();
        drop(catalog);
        let ro = Catalog::open_read_only(&path).unwrap();
        assert_eq!(ro.counts().unwrap(), (0, 0));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resync_overwrites_model() {
        let catalog = Catalog::open(Path::new(":memory:")).unwrap();
        catalog.init().unwrap();
        let row = |name: &str| ModelRow {
            model_id: "m".into(),
            name: name.into(),
            source: "openrouter".into(),
            payload: "{}".into(),
            fetched_at: "t".into(),
        };
        catalog.insert_model(&row("one")).unwrap();
        catalog.insert_model(&row("two")).unwrap();
        assert_eq!(catalog.counts().unwrap(), (1, 0));
        assert_eq!(catalog.get_model("m").unwrap().unwrap().name, "two");
    }
}
