//! SQLite store: open/init, inserts, and read queries.

use crate::schema::{DDL, ENDPOINT_INDEXED_COLUMNS, SCHEMA_VERSION};
use crate::Error;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;

pub struct ModelRow {
    pub model_id: String,
    pub name: String,
    pub source: String,
    pub payload: String,
    pub fetched_at: String,
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
    pub judged_at: Option<String>,
    pub judge_model: Option<String>,
    pub judge_prompt: Option<String>,
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
}

/// Model-judged signals distilled by judging (Phase 2b), plus the
/// audit identity of the judge that produced them (Iter 15: model +
/// prompt version with every judgment; `None` on rows judged before
/// the audit columns existed).
pub struct EndpointJudgment {
    pub quality_tier: Option<String>,
    pub quality_score: Option<f64>,
    pub judged_at: String,
    pub judge_model: Option<String>,
    pub judge_prompt: Option<String>,
}

const ENDPOINT_COLS: &str = "id, model_id, provider_name, payload, fetched_at,
    price_prompt, price_completion, price_cache_read, price_cache_write,
    precision_class, promo_discount, processed_at, supports_tools,
    supports_decisions, quality_tier, quality_score, judged_at, judge_model, judge_prompt";

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
        judged_at: r.get(16)?,
        judge_model: r.get(17)?,
        judge_prompt: r.get(18)?,
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
    /// shared artifact.
    pub fn open_read_only(path: &Path) -> Result<Self, Error> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { conn })
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
        for (name, ty) in ENDPOINT_INDEXED_COLUMNS {
            if !self.has_column("endpoints", name)? {
                self.conn
                    .execute_batch(&format!("ALTER TABLE endpoints ADD COLUMN {name} {ty}"))?;
            }
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

    /// Fill the indexed columns of one endpoint, leaving the payload alone.
    pub fn update_endpoint_index(&self, id: i64, index: &EndpointIndex) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE endpoints SET price_prompt = ?1, price_completion = ?2,
             price_cache_read = ?3, price_cache_write = ?4, precision_class = ?5,
             promo_discount = ?6, processed_at = ?7, supports_tools = ?8,
             supports_decisions = ?9 WHERE id = ?10",
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
                id
            ],
        )?;
        Ok(())
    }

    /// Fill the judgment columns of one endpoint, leaving the payload
    /// and the deterministic indexed columns alone.
    pub fn update_endpoint_judgment(
        &self,
        id: i64,
        judgment: &EndpointJudgment,
    ) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE endpoints SET quality_tier = ?1, quality_score = ?2,
             judged_at = ?3, judge_model = ?4, judge_prompt = ?5 WHERE id = ?6",
            params![
                judgment.quality_tier,
                judgment.quality_score,
                judgment.judged_at,
                judgment.judge_model,
                judgment.judge_prompt,
                id
            ],
        )?;
        Ok(())
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
        let models: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM models", [], |r| r.get(0))?;
        let endpoints: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM endpoints", [], |r| r.get(0))?;
        Ok((models, endpoints))
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
            Some("5")
        );
        assert_eq!(catalog.counts().unwrap(), (0, 0));
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
