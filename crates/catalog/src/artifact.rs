//! Versioned artifact publish: DB file + manifest (version, sha256, counts).

use crate::schema::SCHEMA_VERSION;
use crate::Error;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub sha256: String,
    pub db_file: String,
    pub schema_version: u32,
    pub source: String,
    pub model_count: u64,
    pub endpoint_count: u64,
    pub created_at: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn now_rfc3339() -> Result<String, Error> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

/// Atomically install `bytes` at `final_path` (Iter 11, F07): write a
/// unique temp file on the same filesystem, sync it, and rename over
/// the target — readers never see a truncated file. Identical existing
/// content is left untouched: published files are content-addressed
/// and immutable once written.
pub fn atomic_write(final_path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if fs::read(final_path)
        .map(|current| current == bytes)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos().max(0) as u128;
    let file_name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".to_string());
    let tmp_path =
        final_path.with_file_name(format!(".{file_name}.tmp-{}-{nanos}", std::process::id()));
    let mut tmp = fs::File::create(&tmp_path)?;
    use std::io::Write;
    tmp.write_all(bytes)?;
    tmp.sync_all()?;
    drop(tmp);
    fs::rename(&tmp_path, final_path)?;
    Ok(())
}

/// Publish `work_db` into `out_dir` as `catalog-{version}.db` plus a
/// `catalog-{version}.manifest.json` sidecar. The version is the DB
/// content-hash prefix, so re-publishing identical content is idempotent.
/// Data lands before the manifest (both atomic), so discovery never
/// sees a manifest pointing at a half-written DB.
pub fn publish(
    work_db: &Path,
    out_dir: &Path,
    source: &str,
    model_count: u64,
    endpoint_count: u64,
) -> Result<(PathBuf, PathBuf, Manifest), Error> {
    fs::create_dir_all(out_dir)?;
    let bytes = fs::read(work_db)?;
    let sha256 = sha256_hex(&bytes);
    let version = sha256[..16].to_string();
    let db_name = format!("catalog-{version}.db");
    let manifest_name = format!("catalog-{version}.manifest.json");
    let db_path = out_dir.join(&db_name);
    let manifest_path = out_dir.join(&manifest_name);
    atomic_write(&db_path, &bytes)?;
    let manifest = Manifest {
        version,
        sha256,
        db_file: db_name,
        schema_version: SCHEMA_VERSION,
        source: source.to_string(),
        model_count,
        endpoint_count,
        created_at: now_rfc3339()?,
    };
    atomic_write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )?;
    Ok((db_path, manifest_path, manifest))
}

/// Verify a published artifact: manifest parses and the DB file hash matches.
pub fn verify(manifest_path: &Path) -> Result<bool, Error> {
    let raw = fs::read(manifest_path)?;
    let manifest: Manifest = serde_json::from_slice(&raw)?;
    let Some(dir) = manifest_path.parent() else {
        return Ok(false);
    };
    let db_bytes = fs::read(dir.join(&manifest.db_file))?;
    Ok(sha256_hex(&db_bytes) == manifest.sha256)
}
