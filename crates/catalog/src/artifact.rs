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
    // digest 0.11 output no longer formats as hex directly.
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
    let result = (|| {
        let mut tmp = fs::File::create(&tmp_path)?;
        use std::io::Write;
        tmp.write_all(bytes)?;
        tmp.sync_all()?;
        drop(tmp);
        fs::rename(&tmp_path, final_path)?;
        // Crash durability: the rename itself must reach the disk —
        // fsync the parent dir (the temp lives next to the target,
        // so both share it). A bare filename means the cwd.
        let parent = final_path.parent().filter(|p| !p.as_os_str().is_empty());
        fs::File::open(parent.unwrap_or(Path::new(".")))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        // Explicit cleanup beats a Drop guard here: one call site,
        // no new type, no temp leaks on any error path.
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// Publish `work_db` into `out_dir` as `catalog-{version}.db` plus a
/// `catalog-{version}.manifest.json` sidecar. The version is the DB
/// content-hash prefix, so re-publishing identical content is idempotent.
/// Data lands before the manifest (both atomic), so discovery never
/// sees a manifest pointing at a half-written DB. Counts are read back
/// from the DB itself: caller guesses (which counted duplicates) would
/// otherwise verify and display as truth.
pub fn publish(
    work_db: &Path,
    out_dir: &Path,
    source: &str,
    _model_count: u64,
    _endpoint_count: u64,
) -> Result<(PathBuf, PathBuf, Manifest), Error> {
    fs::create_dir_all(out_dir)?;
    let bytes = fs::read(work_db)?;
    let sha256 = sha256_hex(&bytes);
    let (model_count, endpoint_count) = crate::store::Catalog::open_read_only(work_db)?.counts()?;
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

#[cfg(test)]
mod tests {
    use super::{atomic_write, sha256_hex};

    /// Known-answer pin for the manual hex encoding (sha2 0.11).
    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string()
        );
    }

    /// I08: a failed install removes its temp file instead of leaking
    /// `.name.tmp-<pid>-<nanos>` files into the artifact dir.
    #[test]
    fn failed_install_leaves_no_temp_behind() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("pennyrouter-i08-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("final.dat");
        std::fs::create_dir(&target).unwrap(); // rename onto a dir fails
        let _ = atomic_write(&target, b"bytes").unwrap_err();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .filter(|name| name.to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "leaked temps: {leftovers:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
