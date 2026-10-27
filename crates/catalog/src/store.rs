//! SQLite store: open/init, inserts, and read queries.

use crate::schema::{DDL, SCHEMA_VERSION};
use crate::Error;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub struct ModelRow {
    pub model_id: String,
    pub name: String,
    pub source: String,
    pub payload: String,
    pub fetched_at: String,
}

pub struct EndpointRow {
    pub id: i64,
    pub model_id: String,
    pub provider_name: String,
    pub payload: String,
    pub fetched_at: String,
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

    /// Create tables if missing; record the schema version. Idempotent.
    pub fn init(&self) -> Result<(), Error> {
        self.conn.execute_batch(DDL)?;
        self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(())
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

    /// Plain insert: endpoints carry a surrogate key because one provider
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
        let mut stmt = self.conn.prepare(
            "SELECT id, model_id, provider_name, payload, fetched_at
             FROM endpoints WHERE model_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![model_id], |r| {
            Ok(EndpointRow {
                id: r.get(0)?,
                model_id: r.get(1)?,
                provider_name: r.get(2)?,
                payload: r.get(3)?,
                fetched_at: r.get(4)?,
            })
        })?;
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
            Some("1")
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
