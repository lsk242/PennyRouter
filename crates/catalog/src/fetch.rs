//! Live gathering from OpenRouter (Iter 18): public `GET /models`
//! plus one per-model `GET /models/{id}/endpoints`, returned
//! byte-for-byte in the fixture shape so the existing
//! `run_fixture_sync` boundary loads them unchanged. Gathering reads
//! only public endpoints — no auth, no keys.

use crate::Error;
use serde_json::Value;
use std::time::Duration;

/// Fetched bytes in fixture shape: the models-list body plus one
/// endpoints body per model that had one. `skipped` names models
/// whose endpoints fetch failed; the caller publishes without them.
pub struct Fetched {
    pub models: Vec<u8>,
    pub endpoints: Vec<(String, Vec<u8>)>,
    pub skipped: Vec<String>,
}

/// Pull the full model list, then each model's endpoints. A failed
/// models-list fetch aborts (no publish; the last good artifact keeps
/// serving). A failed endpoints fetch skips just that model.
pub fn fetch_openrouter(base_url: &str) -> Result<Fetched, Error> {
    fetch_openrouter_extra(base_url, &[])
}

/// List fetch plus caller-nominated extras (Iter 24): the models
/// list omits some servable models (e.g. scoring models with
/// non-text output modalities), so `extra_ids` names ids to fetch
/// even when absent from the list. Already-listed ids are not
/// refetched; failed extras land in `skipped` like the rest. The
/// endpoints body carries the model object, which joins the models
/// array (sans its nested endpoints) so sync stores it whole.
pub fn fetch_openrouter_extra(base_url: &str, extra_ids: &[String]) -> Result<Fetched, Error> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let models = get_bytes(&client, &format!("{base}/v1/models"))?;
    let mut root: Value = serde_json::from_slice(&models)?;
    let ids: Vec<String> = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Fetch("GET /v1/models: missing `data` array".to_string()))?
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut endpoints = Vec::new();
    let mut skipped = Vec::new();
    for id in &ids {
        match get_bytes(&client, &format!("{base}/v1/models/{id}/endpoints")) {
            Ok(body) => endpoints.push((id.clone(), body)),
            Err(_) => skipped.push(id.clone()),
        }
    }
    for id in extra_ids {
        if ids.iter().any(|listed| listed == id) {
            continue;
        }
        match get_bytes(&client, &format!("{base}/v1/models/{id}/endpoints")) {
            Ok(body) => {
                // The model row must exist before its endpoints (foreign
                // key): keep the pair only when the body carries both.
                let mut stored = false;
                if let Ok(extra) = serde_json::from_slice::<Value>(&body) {
                    if let Some(object) = extra.get("data") {
                        let mut model = object.clone();
                        if let Some(map) = model.as_object_mut() {
                            map.remove("endpoints");
                        }
                        if let Some(list) = root.get_mut("data").and_then(Value::as_array_mut) {
                            list.push(model);
                            stored = true;
                        }
                    }
                }
                if stored {
                    endpoints.push((id.clone(), body));
                } else {
                    skipped.push(id.clone());
                }
            }
            Err(_) => skipped.push(id.clone()),
        }
    }
    let models =
        serde_json::to_vec(&root).map_err(|e| Error::Fetch(format!("re-serialize models: {e}")))?;
    Ok(Fetched {
        models,
        endpoints,
        skipped,
    })
}

fn get_bytes(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>, Error> {
    let response = client.get(url).send()?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Fetch(format!("GET {url}: HTTP {status}")));
    }
    Ok(response.bytes()?.to_vec())
}
