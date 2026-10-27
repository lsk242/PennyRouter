//! Deterministic processing: payloads into fixed indexed columns.
//!
//! Pure-code extraction only (Phase 2a). Model-judged signals plug in
//! later (Phase 2b) behind the same per-endpoint update.

use crate::store::{Catalog, EndpointIndex};
use crate::Error;
use serde_json::Value;

pub struct ProcessSummary {
    pub processed: usize,
}

/// Extract indexed values for every endpoint. Idempotent: re-running
/// overwrites the same columns.
pub fn run_processing(catalog: &Catalog, processed_at: &str) -> Result<ProcessSummary, Error> {
    let endpoints = catalog.all_endpoints()?;
    let mut processed = 0;
    for ep in &endpoints {
        let payload: Value = serde_json::from_str(&ep.payload)?;
        catalog.update_endpoint_index(
            ep.id,
            &EndpointIndex {
                price_prompt: extract_price(&payload, "prompt"),
                price_completion: extract_price(&payload, "completion"),
                price_cache_read: extract_price(&payload, "input_cache_read"),
                price_cache_write: extract_price(&payload, "input_cache_write"),
                precision_class: Some(classify_precision(&payload)),
                promo_discount: extract_discount(&payload),
                processed_at: processed_at.to_string(),
            },
        )?;
        processed += 1;
    }
    catalog.set_meta("processed_at", processed_at)?;
    catalog.set_meta("processed_count", &processed.to_string())?;
    Ok(ProcessSummary { processed })
}

/// Per-token price as f64; accepts a string or a number, else None.
fn extract_price(payload: &Value, key: &str) -> Option<f64> {
    match payload.get("pricing")?.get(key)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn extract_discount(payload: &Value) -> Option<f64> {
    match payload.get("pricing")?.get("discount")? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Quantization label to precision class. Anything unrecognized (including
/// missing) is "unknown" — never silently treated as a known class.
fn classify_precision(payload: &Value) -> String {
    let q = payload
        .get("quantization")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    if q.contains("fp32") {
        "fp32"
    } else if q.contains("bf16") || q.contains("fp16") {
        "fp16"
    } else if q.contains("fp8") {
        "fp8"
    } else if q.contains("int8") {
        "int8"
    } else if q.contains("int4") || q.contains("nf4") || q.contains("fp4") {
        "int4"
    } else {
        "unknown"
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn price_accepts_string_number_or_nothing() {
        let payload = json!({"pricing": {
            "prompt": "0.0000005",
            "completion": 0.25,
            "input_cache_read": null,
            "input_cache_write": "garbage",
        }});
        assert_eq!(extract_price(&payload, "prompt"), Some(0.0000005));
        assert_eq!(extract_price(&payload, "completion"), Some(0.25));
        assert_eq!(extract_price(&payload, "input_cache_read"), None);
        assert_eq!(extract_price(&payload, "input_cache_write"), None);
        assert_eq!(extract_price(&payload, "missing"), None);
        assert_eq!(extract_price(&json!({}), "prompt"), None);
    }

    #[test]
    fn precision_maps_known_quantizations() {
        let class = |q: &str| classify_precision(&json!({"quantization": q}));
        assert_eq!(class("fp32"), "fp32");
        assert_eq!(class("fp16"), "fp16");
        assert_eq!(class("BF16"), "fp16");
        assert_eq!(class("fp8"), "fp8");
        assert_eq!(class("fp8_e4m3"), "fp8");
        assert_eq!(class("int8"), "int8");
        assert_eq!(class("nf4"), "int4");
    }

    #[test]
    fn precision_falls_back_to_unknown() {
        assert_eq!(
            classify_precision(&json!({"quantization": "unknown"})),
            "unknown"
        );
        assert_eq!(
            classify_precision(&json!({"quantization": "w4a8-exotic"})),
            "unknown"
        );
        assert_eq!(classify_precision(&json!({"quantization": ""})), "unknown");
        assert_eq!(classify_precision(&json!({})), "unknown");
    }
}
