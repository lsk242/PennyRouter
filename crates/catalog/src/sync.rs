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
}

/// Load one endpoints-route body per model from a directory.
///
/// File `<model id with '/' replaced by '__'>.json` holds that model's
/// `GET /models/{id}/endpoints` response. Returns `(model_id, body)` pairs
/// in filename order.
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
        model_count += 1;
    }

    let mut endpoint_count = 0;
    for (model_id, body) in endpoints {
        let root: Value = serde_json::from_slice(body)?;
        let eps = root
            .pointer("/data/endpoints")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidFixture(format!(
                    "endpoints for {model_id}: missing `data.endpoints`"
                ))
            })?;
        catalog.delete_endpoints_for(model_id)?;
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
                promo_discount: None,
                processed_at: None,
                model_id: model_id.clone(),
                provider_name: provider.to_string(),
                payload: serde_json::to_string(e)?,
                fetched_at: fetched_at.to_string(),
            })?;
            endpoint_count += 1;
        }
    }

    catalog.set_meta("source", source)?;
    catalog.set_meta("completed_at", fetched_at)?;
    catalog.set_meta("model_count", &model_count.to_string())?;
    catalog.set_meta("endpoint_count", &endpoint_count.to_string())?;

    Ok(Summary {
        model_count,
        endpoint_count,
    })
}
