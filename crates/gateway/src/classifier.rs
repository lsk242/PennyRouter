//! Live per-request classifier (Iter 26): prompt construction, input
//! extraction/truncation, and verdict validation for the
//! `openrouter/pair/smart[/max]` virtual ids.
//!
//! The classifier is one short non-streaming OpenAI-chat call whose only
//! contract is `{"tier": ...}` inside the caller-visible `tier_set`.
//! Everything else (`confidence`, `reasoning`) is diagnostic. A verdict
//! that is not exactly in-domain fails closed to the configured
//! fallback tier — never to a guess, never to a 500.

/// Pinned system prompt. Tests pin this text: changing it changes what
/// the decision model optimizes for, so edits here are deliberate.
pub const SYSTEM_PROMPT: &str = "You are a routing classifier. Given the full chat history, pick the cheapest tier that can answer well. Reply exactly one JSON object with no other text.\n\nsmart modes: {\"tier\": \"free\"|\"lowest\"|\"low\"}\nsmart/max modes: {\"tier\": \"free\"|\"lowest\"|\"low\"|\"think\"}\n\ntiers:\n  free   - trivial chat, greetings, reformatting, facts answerable by small models.\n  lowest - general help where lowest paid cost wins (no judged floor).\n  low    - cheapest judged offering (known quality floor).\n  think  - needs frontier quality/deep reasoning (best judged signals).\n\nConsider: prompt length, multi-step reasoning, factual risk, need for recency, ambiguity.\nOptional: {\"confidence\": 0.0..1.0, \"reasoning\": \"one short sentence\"} - never shown to user, logged.";

/// Classifier output domain. `smart` is always the 3-tier set;
/// `smart/max` always adds `think`. There is no env alias that widens
/// `smart`: the id on the wire is the whole contract.
pub fn tier_set(smart_max: bool) -> Vec<&'static str> {
    if smart_max {
        vec!["free", "lowest", "low", "think"]
    } else {
        vec!["free", "lowest", "low"]
    }
}

/// Extract the classifiable input from a routed request body. Chat
/// bodies contribute their `messages` array; Responses bodies their
/// `input` (string, array, or object). Missing, mistyped, or empty
/// inputs yield `None` (`classifier_no_input` → fallback): there is
/// nothing to judge.
pub fn extract_input(body: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(messages) = body.get("messages") {
        if messages
            .as_array()
            .is_some_and(|messages| !messages.is_empty())
        {
            return Some(messages.clone());
        }
    }
    match body.get("input") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) if s.is_empty() => None,
        Some(serde_json::Value::Array(items)) if items.is_empty() => None,
        Some(input) => Some(input.clone()),
    }
}

/// Bound the classifier input to `cap_chars` characters of serialized
/// JSON. Message arrays drop oldest-first (recency wins — the latest
/// turn usually decides difficulty); a single oversize element, string,
/// or object degrades to a head-truncated stub so the call still
/// carries the gist instead of failing. Small inputs pass through
/// untouched.
pub fn truncate_input(input: serde_json::Value, cap_chars: usize) -> serde_json::Value {
    fn chars(value: &serde_json::Value) -> usize {
        serde_json::to_string(value)
            .map(|s| s.chars().count())
            .unwrap_or(0)
    }
    fn head_truncated(raw: &str, cap_chars: usize) -> serde_json::Value {
        serde_json::Value::String(raw.chars().take(cap_chars).collect())
    }
    match input {
        serde_json::Value::Array(mut items) => {
            while items.len() > 1 && chars(&serde_json::Value::Array(items.clone())) > cap_chars {
                items.remove(0);
            }
            if items.len() == 1 && chars(&items[0]) > cap_chars {
                let raw = serde_json::to_string(&items[0]).unwrap_or_default();
                return serde_json::json!([{"role": "user", "content": head_truncated(&raw, cap_chars)}]);
            }
            serde_json::Value::Array(items)
        }
        serde_json::Value::String(s) if s.chars().count() > cap_chars => {
            serde_json::Value::String(s.chars().take(cap_chars).collect())
        }
        other => {
            if chars(&other) > cap_chars {
                let raw = serde_json::to_string(&other).unwrap_or_default();
                serde_json::json!({"truncated": head_truncated(&raw, cap_chars)})
            } else {
                other
            }
        }
    }
}

/// Build the classifier request body: fixed model, system + one user
/// turn carrying the tier gate and the (truncated) input, temperature
/// zero, `json_object` only, never streamed. Privacy ids attach the
/// same zero-retention fields the completion leg gets — the classifier
/// sees the same prompt the completion serves.
pub fn build_request(
    model: &str,
    set: &[&str],
    input: &serde_json::Value,
    max_tokens: u32,
    privacy: bool,
) -> serde_json::Value {
    let user = serde_json::json!({
        "tier_set": set,
        "prompt": input,
    });
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": serde_json::to_string(&user).unwrap_or_default()},
        ],
        "temperature": 0,
        "max_tokens": max_tokens,
        "response_format": {"type": "json_object"},
    });
    if privacy {
        body["provider"] = serde_json::json!({"zdr": true, "data_collection": "deny"});
    }
    body
}

/// A validated classifier verdict: the tier to serve plus diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub tier: String,
    pub confidence: Option<f64>,
    pub reasoning: Option<String>,
}

/// Why a classifier reply was rejected (all map to
/// `classifier_invalid` → fallback, counted in metrics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerdictError {
    NotJson,
    NotObject,
    MissingTier,
    UnknownTier(String),
    UnknownKeys,
    BadField(&'static str),
}

/// Validate one classifier message `content` string against the tier
/// gate. Only `tier`/`confidence`/`reasoning` keys are known — anything
/// else fails closed, so a chatty model can never smuggle an
/// out-of-domain pick through extra fields.
pub fn parse_verdict(content: &str, set: &[&str]) -> Result<Verdict, VerdictError> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|_| VerdictError::NotJson)?;
    let object = value.as_object().ok_or(VerdictError::NotObject)?;
    for key in object.keys() {
        if !matches!(key.as_str(), "tier" | "confidence" | "reasoning") {
            return Err(VerdictError::UnknownKeys);
        }
    }
    let tier = object
        .get("tier")
        .and_then(serde_json::Value::as_str)
        .ok_or(VerdictError::MissingTier)?;
    if !set.contains(&tier) {
        return Err(VerdictError::UnknownTier(tier.to_string()));
    }
    // Strict JSON numbers are always finite, so a present numeric
    // confidence always carries a value; anything else shaped is a
    // malformed reply.
    let confidence = match object.get("confidence") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(_) => return Err(VerdictError::BadField("confidence")),
    };
    let reasoning = match object.get("reasoning") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(VerdictError::BadField("reasoning")),
    };
    Ok(Verdict {
        tier: tier.to_string(),
        confidence,
        reasoning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_sets_are_fixed() {
        assert_eq!(tier_set(false), vec!["free", "lowest", "low"]);
        assert_eq!(tier_set(true), vec!["free", "lowest", "low", "think"]);
    }

    #[test]
    fn valid_verdicts_parse() {
        let full = parse_verdict(
            r#"{"tier":"low","confidence":0.86,"reasoning":"multi-step synthesis needed"}"#,
            &tier_set(false),
        )
        .unwrap();
        assert_eq!(full.tier, "low");
        assert_eq!(full.confidence, Some(0.86));
        assert!(full.reasoning.unwrap().contains("multi-step"));
        let bare = parse_verdict(r#"{"tier":"free"}"#, &tier_set(false)).unwrap();
        assert_eq!(bare.confidence, None);
        assert_eq!(bare.reasoning, None);
    }

    #[test]
    fn out_of_domain_tier_rejected() {
        // `think` is valid for smart/max only.
        assert_eq!(
            parse_verdict(r#"{"tier":"think"}"#, &tier_set(false)),
            Err(VerdictError::UnknownTier("think".to_string()))
        );
        assert!(parse_verdict(r#"{"tier":"think"}"#, &tier_set(true)).is_ok());
        assert_eq!(
            parse_verdict(r#"{"tier":"frontier"}"#, &tier_set(true)),
            Err(VerdictError::UnknownTier("frontier".to_string()))
        );
    }

    #[test]
    fn malformed_replies_rejected() {
        assert_eq!(
            parse_verdict("not json", &tier_set(false)),
            Err(VerdictError::NotJson)
        );
        assert_eq!(
            parse_verdict("[\"low\"]", &tier_set(false)),
            Err(VerdictError::NotObject)
        );
        assert_eq!(
            parse_verdict(r#"{"confidence":0.5}"#, &tier_set(false)),
            Err(VerdictError::MissingTier)
        );
        assert_eq!(
            parse_verdict(r#"{"tier":3}"#, &tier_set(false)),
            Err(VerdictError::MissingTier)
        );
        assert_eq!(
            parse_verdict(r#"{"tier":"low","explanation":"x"}"#, &tier_set(false)),
            Err(VerdictError::UnknownKeys)
        );
        assert_eq!(
            parse_verdict(r#"{"tier":"low","confidence":"high"}"#, &tier_set(false)),
            Err(VerdictError::BadField("confidence"))
        );
        assert_eq!(
            parse_verdict(r#"{"tier":"low","reasoning":42}"#, &tier_set(false)),
            Err(VerdictError::BadField("reasoning"))
        );
    }

    #[test]
    fn input_extraction_covers_both_endpoints() {
        let chat =
            serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        assert!(extract_input(&chat).is_some());
        let empty_chat = serde_json::json!({"model": "m", "messages": []});
        assert_eq!(extract_input(&empty_chat), None);
        let responses = serde_json::json!({"model": "m", "input": "do the thing"});
        assert_eq!(
            extract_input(&responses),
            Some(serde_json::Value::String("do the thing".to_string()))
        );
        let bare = serde_json::json!({"model": "m"});
        assert_eq!(extract_input(&bare), None);
    }

    #[test]
    fn truncation_drops_oldest_first() {
        let input = serde_json::json!([
            {"role": "user", "content": "oldest"},
            {"role": "user", "content": "newest"},
        ]);
        let full = serde_json::to_string(&input).unwrap().chars().count();
        let cut = truncate_input(input.clone(), full);
        assert_eq!(cut, input);
        let tiny = truncate_input(input, 40);
        let raw = serde_json::to_string(&tiny).unwrap();
        assert!(raw.contains("newest"));
        assert!(!raw.contains("oldest"));
    }

    #[test]
    fn request_shape_is_pinned() {
        let body = build_request("clf", &tier_set(true), &serde_json::json!("hi"), 64, true);
        assert_eq!(body["model"], serde_json::json!("clf"));
        assert_eq!(body["temperature"], serde_json::json!(0));
        assert_eq!(body["max_tokens"], serde_json::json!(64));
        assert_eq!(
            body["response_format"],
            serde_json::json!({"type": "json_object"})
        );
        assert_eq!(
            body["provider"],
            serde_json::json!({"zdr": true, "data_collection": "deny"})
        );
        let user = body["messages"][1]["content"].as_str().unwrap();
        let envelope: serde_json::Value = serde_json::from_str(user).unwrap();
        assert_eq!(
            envelope["tier_set"],
            serde_json::json!(["free", "lowest", "low", "think"])
        );
        let plain = build_request("clf", &tier_set(false), &serde_json::json!("hi"), 64, false);
        assert!(plain.get("provider").is_none());
    }
}
