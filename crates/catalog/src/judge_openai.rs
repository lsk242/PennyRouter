//! Shared core for OpenAI-compatible live judges (Baseten, Modal).
//!
//! Backends differ only in base URL, model, and auth headers; the prompt,
//! request shape, and reply validation are identical.

use crate::judge::{endpoint_tag, Judgment};
use crate::Error;
use serde_json::{json, Value};

pub const SYSTEM_PROMPT: &str = "You judge one LLM provider offering for a routing catalog. Reply with exactly one JSON object and no other text: {\"quality_tier\": \"high\", \"standard\" or \"low\", \"quality_score\": a number from 0.0 to 1.0}. Judge expected answer quality for general chat use.";

pub const PROMPT_VERSION: &str = "v1";

/// Chat completions body: temperature 0, JSON-object response, one system
/// message plus one user message naming the offering under judgment.
pub fn chat_body(model: &str, model_id: &str, provider_name: &str, payload: &Value) -> Value {
    let mut offering = format!("model: {model_id}\nprovider: {provider_name}");
    if let Some(tag) = endpoint_tag(payload) {
        offering.push_str(&format!("\ntag: {tag}"));
    }
    offering.push_str(&format!("\nendpoint: {payload}"));
    json!({
        "model": model,
        "temperature": 0,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": offering},
        ],
    })
}

/// Extract and validate the judgment from a chat completion reply.
/// `source` names the backend for error messages ("baseten", "modal").
pub fn judgment_from_reply(source: &str, reply: &Value) -> Result<Judgment, Error> {
    let content = reply
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Judge(format!("{source} reply has no choices[0].message.content")))?;
    parse_judgment(source, content)
}

/// Validate one model reply. Same value rules as the recorded backend:
/// tier is high/standard/low, score is 0.0..=1.0, missing means no signal.
pub fn parse_judgment(source: &str, content: &str) -> Result<Judgment, Error> {
    let value: Value = serde_json::from_str(content)
        .map_err(|e| Error::Judge(format!("{source} reply is not JSON: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| Error::Judge(format!("{source} reply is not a JSON object")))?;
    let quality_tier = match obj.get("quality_tier") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) if ["high", "standard", "low"].contains(&t.as_str()) => {
            Some(t.clone())
        }
        Some(other) => {
            return Err(Error::Judge(format!(
                "{source} \"quality_tier\" must be high/standard/low, got {other}"
            )));
        }
    };
    let quality_score = match obj.get("quality_score") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => {
            let score = n.as_f64().ok_or_else(|| {
                Error::Judge(format!("{source} \"quality_score\" is not a number"))
            })?;
            if !(0.0..=1.0).contains(&score) {
                return Err(Error::Judge(format!(
                    "{source} \"quality_score\" must be within 0.0..=1.0"
                )));
            }
            Some(score)
        }
        Some(other) => {
            return Err(Error::Judge(format!(
                "{source} \"quality_score\" must be a number, got {other}"
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
    fn chat_body_names_the_offering() {
        let body = chat_body("judge-model", "m", "p", &serde_json::json!({"tag": "t"}));
        assert_eq!(body["model"], json!("judge-model"));
        assert_eq!(body["temperature"], json!(0));
        assert_eq!(body["response_format"], json!({"type": "json_object"}));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        let user = messages[1]["content"].as_str().unwrap();
        assert!(user.contains('m') && user.contains('p') && user.contains('t'));
    }

    #[test]
    fn valid_replies_yield_signals() {
        let full =
            parse_judgment("test", r#"{"quality_tier": "high", "quality_score": 0.9}"#).unwrap();
        assert_eq!(full.quality_tier.as_deref(), Some("high"));
        assert_eq!(full.quality_score, Some(0.9));
        let tier_only = parse_judgment("test", r#"{"quality_tier": "low"}"#).unwrap();
        assert_eq!(tier_only.quality_tier.as_deref(), Some("low"));
        assert_eq!(tier_only.quality_score, None);
        let empty = parse_judgment("test", r#"{}"#).unwrap();
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
            assert!(
                parse_judgment("test", content).is_err(),
                "should reject: {content}"
            );
        }
    }

    #[test]
    fn missing_content_is_an_error() {
        let reply = json!({"choices": []});
        assert!(judgment_from_reply("test", &reply).is_err());
    }
}
