//! Phase 4: live judging through a Baseten-hosted model.
//!
//! Same `Judge` contract as the recorded backend: one judgment per
//! endpoint into `quality_tier`/`quality_score`. Requests are plain
//! OpenAI chat completions with `temperature: 0` and a JSON-object
//! response format. The API key travels only in the Authorization
//! header and is never logged (no `Debug` impl on purpose).

use crate::judge::{endpoint_tag, Judge, Judgment};
use crate::Error;
use serde_json::{json, Value};
use std::time::Duration;

const SYSTEM_PROMPT: &str = "You judge one LLM provider offering for a routing catalog. Reply with exactly one JSON object and no other text: {\"quality_tier\": \"high\", \"standard\" or \"low\", \"quality_score\": a number from 0.0 to 1.0}. Judge expected answer quality for general chat use.";

/// Live judge over the Baseten OpenAI-compatible chat API.
pub struct BasetenJudge {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl BasetenJudge {
    /// Shared Model APIs / Frontier Gateway base URL.
    pub const BASE_URL: &'static str = "https://inference.baseten.co/v1";
    /// Hackathon judge model a federated key is scoped to.
    pub const MODEL: &'static str = "meta-models/muse-spark-open-1026";

    pub fn new(base_url: &str, api_key: &str, model: &str) -> Result<Self, Error> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        })
    }

    /// Production judge: documented base URL + model, key from `BASETEN_API_KEY`.
    pub fn from_env() -> Result<Self, Error> {
        let api_key = std::env::var("BASETEN_API_KEY").map_err(|_| {
            Error::Judge("BASETEN_API_KEY is not set (see .env.example)".to_string())
        })?;
        if api_key.is_empty() {
            return Err(Error::Judge(
                "BASETEN_API_KEY is empty (see .env.example)".to_string(),
            ));
        }
        Self::new(Self::BASE_URL, &api_key, Self::MODEL)
    }

    fn chat(
        &self,
        model_id: &str,
        provider_name: &str,
        payload: &Value,
    ) -> Result<Judgment, Error> {
        let mut offering = format!("model: {model_id}\nprovider: {provider_name}");
        if let Some(tag) = endpoint_tag(payload) {
            offering.push_str(&format!("\ntag: {tag}"));
        }
        offering.push_str(&format!("\nendpoint: {payload}"));
        let body = json!({
            "model": self.model,
            "temperature": 0,
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": offering},
            ],
        });
        let url = format!("{}/chat/completions", self.base_url);
        let reply: Value = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()?
            .error_for_status()?
            .json()?;
        let content = reply
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::Judge("baseten reply has no choices[0].message.content".to_string())
            })?;
        parse_judgment(content)
    }
}

impl Judge for BasetenJudge {
    fn judge(
        &self,
        model_id: &str,
        provider_name: &str,
        payload: &Value,
    ) -> Result<Judgment, Error> {
        self.chat(model_id, provider_name, payload)
    }
}

/// Validate one model reply. Same value rules as the recorded backend:
/// tier is high/standard/low, score is 0.0..=1.0, missing means no signal.
fn parse_judgment(content: &str) -> Result<Judgment, Error> {
    let value: Value = serde_json::from_str(content)
        .map_err(|e| Error::Judge(format!("baseten reply is not JSON: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| Error::Judge("baseten reply is not a JSON object".to_string()))?;
    let quality_tier = match obj.get("quality_tier") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) if ["high", "standard", "low"].contains(&t.as_str()) => {
            Some(t.clone())
        }
        Some(other) => {
            return Err(Error::Judge(format!(
                "baseten \"quality_tier\" must be high/standard/low, got {other}"
            )));
        }
    };
    let quality_score = match obj.get("quality_score") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => {
            let score = n.as_f64().ok_or_else(|| {
                Error::Judge("baseten \"quality_score\" is not a number".to_string())
            })?;
            if !(0.0..=1.0).contains(&score) {
                return Err(Error::Judge(
                    "baseten \"quality_score\" must be within 0.0..=1.0".to_string(),
                ));
            }
            Some(score)
        }
        Some(other) => {
            return Err(Error::Judge(format!(
                "baseten \"quality_score\" must be a number, got {other}"
            )));
        }
    };
    Ok(Judgment {
        quality_tier,
        quality_score,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_replies_yield_signals() {
        let full = parse_judgment(r#"{"quality_tier": "high", "quality_score": 0.9}"#).unwrap();
        assert_eq!(full.quality_tier.as_deref(), Some("high"));
        assert_eq!(full.quality_score, Some(0.9));
        let tier_only = parse_judgment(r#"{"quality_tier": "low"}"#).unwrap();
        assert_eq!(tier_only.quality_tier.as_deref(), Some("low"));
        assert_eq!(tier_only.quality_score, None);
        let empty = parse_judgment(r#"{}"#).unwrap();
        assert_eq!(empty.quality_tier, None);
        assert_eq!(empty.quality_score, None);
    }

    #[test]
    fn invalid_replies_are_errors() {
        let bad = [
            "oops",
            "[1, 2]",
            r#"{"quality_tier": "ultra"}"#,
            r#"{"quality_tier": 7}"#,
            r#"{"quality_score": 1.5}"#,
            r#"{"quality_score": "high"}"#,
        ];
        for content in bad {
            assert!(parse_judgment(content).is_err(), "should reject: {content}");
        }
    }
}
