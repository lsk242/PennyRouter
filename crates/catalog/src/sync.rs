//! Fixture sync: OpenRouter-shaped JSON bytes into the store.
//!
//! Live fetching plugs in later behind the same function boundary:
//! whatever produces the bytes, `run_fixture_sync` loads them.

use crate::store::{Catalog, EndpointRow, ModelRow};
use crate::Error;
use serde_json::Value;
use std::fs;
use std::path::Path;

pub struct Summary {
    pub model_count: usize,
    pub endpoint_count: usize,
    /// Endpoint files that matched no listed model (same skipped-shape
    /// as the fetch path): recorded, not fatal.
    pub skipped: Vec<String>,
}

/// Load one endpoints-route body per model from a directory.
///
/// File `<model id with '/' replaced by '__'>.json` holds that model's
/// `GET /models/{id}/endpoints` response. Returns `(model_id, body)` pairs
/// in filename order. The derived id is lossy for ids containing `__`
/// and is only a fallback: sync prefers the payload's own id.
pub fn load_endpoints_dir(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let mut names: Vec<_> = fs::read_dir(dir)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        out.push((stem.replace("__", "/"), fs::read(dir.join(&name))?));
    }
    Ok(out)
}

/// Parse the `GET /models` body plus per-model endpoints bodies into the store.
pub fn run_fixture_sync(
    catalog: &Catalog,
    models_json: &[u8],
    endpoints: &[(String, Vec<u8>)],
    source: &str,
    fetched_at: &str,
) -> Result<Summary, Error> {
    let root: Value = serde_json::from_slice(models_json)?;
    let models = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidFixture("models: missing `data` array".into()))?;

    let mut model_count = 0;
    let mut input_ids = std::collections::HashSet::new();
    for m in models {
        let model_id = m
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidFixture("models: entry without `id`".into()))?;
        let name = m
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        catalog.insert_model(&ModelRow {
            model_id: model_id.to_string(),
            name,
            source: source.to_string(),
            payload: serde_json::to_string(m)?,
            fetched_at: fetched_at.to_string(),
        })?;
        input_ids.insert(model_id.to_string());
        model_count += 1;
    }

    let mut endpoint_count = 0;
    let mut skipped = Vec::new();
    for (model_id, body) in endpoints {
        let root: Value = serde_json::from_slice(body)?;
        // The payload's own id wins: the filename mapping (`__` for
        // `/`) is lossy for ids containing `__`. The filename is
        // only a fallback for bodies that carry no id.
        let effective = root
            .pointer("/data/id")
            .and_then(Value::as_str)
            .unwrap_or(model_id)
            .to_string();
        // An endpoint file for an unlisted model is skipped, never
        // fatal: filing it would trip the endpoints→models foreign
        // key and abort the whole sync. The skipped filename is
        // recorded like the fetch path's `skipped`.
        if !input_ids.contains(effective.as_str()) {
            skipped.push(model_id.clone());
            continue;
        }
        let model_id = &effective;
        let eps = root
            .pointer("/data/endpoints")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidFixture(format!(
                    "endpoints for {model_id}: missing `data.endpoints`"
                ))
            })?;
        // One transaction per model: a mid-model failure rolls the
        // delete back instead of leaving the model endpoint-less.
        let inserted = catalog.transaction(|| {
            catalog.delete_endpoints_for(model_id)?;
            let mut inserted = 0;
            for e in eps {
                let provider = e
                    .get("provider_name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Error::InvalidFixture(format!(
                            "endpoints for {model_id}: entry without `provider_name`"
                        ))
                    })?;
                catalog.insert_endpoint(&EndpointRow {
                    id: 0,
                    price_prompt: None,
                    price_completion: None,
                    price_cache_read: None,
                    price_cache_write: None,
                    precision_class: None,
                    quantization: None,
                    promo_discount: None,
                    processed_at: None,
                    supports_tools: None,
                    supports_decisions: None,
                    quality_tier: None,
                    quality_score: None,
                    scored_at: None,
                    score_model: None,
                    score_prompt: None,
                    price_override_tokens: None,
                    model_id: model_id.clone(),
                    provider_name: provider.to_string(),
                    payload: serde_json::to_string(e)?,
                    fetched_at: fetched_at.to_string(),
                })?;
                inserted += 1;
            }
            Ok(inserted)
        })?;
        endpoint_count += inserted;
    }

    // Prune models the source no longer lists: endpoints first (no
    // cascade), then the model row — one transaction so a failure
    // never half-prunes. Fresh DBs prune nothing.
    let missing: Vec<String> = catalog
        .all_model_ids()?
        .into_iter()
        .filter(|id| !input_ids.contains(id))
        .collect();
    if !missing.is_empty() {
        catalog.transaction(|| {
            for id in &missing {
                catalog.delete_endpoints_for(id)?;
                catalog.delete_model(id)?;
            }
            Ok(())
        })?;
    }

    catalog.set_meta("source", source)?;
    catalog.set_meta("completed_at", fetched_at)?;
    catalog.set_meta("model_count", &model_count.to_string())?;
    catalog.set_meta("endpoint_count", &endpoint_count.to_string())?;

    Ok(Summary {
        model_count,
        endpoint_count,
        skipped,
    })
}
