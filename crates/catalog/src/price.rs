//! Price model v1 (Iter 7): workload profiles, effective cost, and
//! time-boxed promo windows — no classifier.
//!
//! Input, output, and cache tokens price differently and the mix varies
//! per request. Without a classifier, cost collapses onto a small set of
//! workload profiles; the default (balanced) profile prices the common
//! case. Promos and free windows are provider-scoped facts with expiry,
//! evaluated at rebuild time.

use crate::Error;
use std::collections::HashMap;

/// Token-mix shares for one workload shape. Shares sum to 1.0.
#[derive(Debug, Clone)]
pub struct WorkloadProfile {
    pub name: &'static str,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// v1 profile set. Balanced is the default: it prices the common case
/// and every cost comparison the ranker and router make.
pub fn builtin_profiles() -> Vec<WorkloadProfile> {
    vec![
        WorkloadProfile {
            name: "balanced",
            input: 0.5,
            output: 0.4,
            cache_read: 0.05,
            cache_write: 0.05,
        },
        WorkloadProfile {
            name: "cache_heavy",
            input: 0.2,
            output: 0.1,
            cache_read: 0.6,
            cache_write: 0.1,
        },
        WorkloadProfile {
            name: "output_heavy",
            input: 0.2,
            output: 0.7,
            cache_read: 0.05,
            cache_write: 0.05,
        },
    ]
}

pub fn balanced_profile() -> WorkloadProfile {
    builtin_profiles().into_iter().next().unwrap()
}

/// Blended per-token cost under a profile after a fractional discount.
/// Missing prompt/completion prices are unpriceable (None); missing
/// cache prices fall back to the prompt price (cache billed as input
/// where no cache price exists). Discounts clamp to [0, 1].
pub fn effective_cost(
    price_prompt: Option<f64>,
    price_completion: Option<f64>,
    price_cache_read: Option<f64>,
    price_cache_write: Option<f64>,
    profile: &WorkloadProfile,
    discount: f64,
) -> Option<f64> {
    let prompt = price_prompt?;
    let completion = price_completion?;
    let cache_read = price_cache_read.unwrap_or(prompt);
    let cache_write = price_cache_write.unwrap_or(prompt);
    let total = profile.input * prompt
        + profile.output * completion
        + profile.cache_read * cache_read
        + profile.cache_write * cache_write;
    Some(total * (1.0 - discount.clamp(0.0, 1.0)))
}

/// One time-boxed promo window. `tag` narrows the match to one offering;
/// omitted it matches every offering of the (model, provider) pair.
#[derive(Debug, Clone)]
struct Promo {
    tag: Option<String>,
    discount: f64,
    starts_at: String,
    ends_at: String,
}

/// Provider-scoped promo/free facts with expiry. File shape:
/// `{"promos": [{"model_id": "m", "provider_name": "p", "tag": "t",
/// "discount": 0.5, "starts_at": "2026-01-01T00:00:00Z", "ends_at":
/// "2026-02-01T00:00:00Z"}]}`. `discount` is the fraction off (1.0 =
/// free). Unknown endpoints are ignored; several active windows match
/// last-wins. Timestamps are RFC 3339 UTC.
#[derive(Debug, Clone, Default)]
pub struct Promos {
    by_endpoint: HashMap<(String, String), Vec<Promo>>,
}

impl Promos {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load(path: &std::path::Path) -> Result<Self, Error> {
        Self::from_str(&std::fs::read_to_string(path)?)
    }

    fn from_str(raw: &str) -> Result<Self, Error> {
        let doc: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| Error::InvalidFixture(format!("promos file is not valid JSON: {e}")))?;
        let list = doc
            .get("promos")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                Error::InvalidFixture("promos file needs a \"promos\" array".to_string())
            })?;
        let mut by_endpoint: HashMap<(String, String), Vec<Promo>> = HashMap::new();
        for (i, item) in list.iter().enumerate() {
            let at = |what: &str| format!("promos[{i}]: {what}");
            let need_str = |key: &str| {
                item.get(key)
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        Error::InvalidFixture(at(&format!("needs a non-empty \"{key}\"")))
                    })
            };
            let model_id = need_str("model_id")?;
            let provider_name = need_str("provider_name")?;
            // A wrong-type tag is rejected, never widened to an
            // unscoped match-all (Iter 11, A05).
            let tag = match item.get("tag") {
                None => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| Error::InvalidFixture(at("\"tag\" must be a string")))?
                        .to_string(),
                ),
            };
            let discount = item
                .get("discount")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| Error::InvalidFixture(at("\"discount\" must be a number")))?;
            if !(0.0..=1.0).contains(&discount) {
                return Err(Error::InvalidFixture(at(
                    "\"discount\" must be within 0.0..=1.0",
                )));
            }
            let starts_at = need_str("starts_at")?;
            let ends_at = need_str("ends_at")?;
            let start = parse_time(starts_at)
                .ok_or_else(|| Error::InvalidFixture(at("\"starts_at\" is not RFC 3339")))?;
            let end = parse_time(ends_at)
                .ok_or_else(|| Error::InvalidFixture(at("\"ends_at\" is not RFC 3339")))?;
            if end <= start {
                return Err(Error::InvalidFixture(at(
                    "\"ends_at\" must be after \"starts_at\"",
                )));
            }
            by_endpoint
                .entry((model_id.to_string(), provider_name.to_string()))
                .or_default()
                .push(Promo {
                    tag,
                    discount,
                    starts_at: starts_at.to_string(),
                    ends_at: ends_at.to_string(),
                });
        }
        Ok(Self { by_endpoint })
    }

    /// End (`ends_at`) of the last active 100%-off window covering the
    /// offering at `now` (Iter 11, F11). `None` means no promo makes it
    /// free right now — permanent catalog zeros are not promo facts.
    pub fn free_until(
        &self,
        model_id: &str,
        provider_name: &str,
        tag: Option<&str>,
        now: &str,
    ) -> Option<String> {
        let now = parse_time(now)?;
        let mut free_until = None;
        if let Some(promos) = self
            .by_endpoint
            .get(&(model_id.to_string(), provider_name.to_string()))
        {
            for promo in promos {
                if promo.tag.as_deref().is_some_and(|t| Some(t) != tag) {
                    continue;
                }
                let (Some(start), Some(end)) =
                    (parse_time(&promo.starts_at), parse_time(&promo.ends_at))
                else {
                    continue;
                };
                if start <= now && now < end && promo.discount == 1.0 {
                    free_until = Some(promo.ends_at.clone());
                }
            }
        }
        free_until
    }

    /// Active discount for one offering at `now` (RFC 3339), if any promo
    /// window covers it. Windows are [starts_at, ends_at).
    pub fn active_discount(
        &self,
        model_id: &str,
        provider_name: &str,
        tag: Option<&str>,
        now: &str,
    ) -> Option<f64> {
        let now = parse_time(now)?;
        let mut active = None;
        if let Some(promos) = self
            .by_endpoint
            .get(&(model_id.to_string(), provider_name.to_string()))
        {
            for promo in promos {
                if promo.tag.as_deref().is_some_and(|t| Some(t) != tag) {
                    continue;
                }
                let (Some(start), Some(end)) =
                    (parse_time(&promo.starts_at), parse_time(&promo.ends_at))
                else {
                    continue;
                };
                if start <= now && now < end {
                    active = Some(promo.discount);
                }
            }
        }
        active
    }
}

/// Effective discount: an active promo window wins; otherwise the
/// catalog's discount column (a stale fact without expiry); else zero.
pub fn resolve_discount(
    promos: &Promos,
    model_id: &str,
    provider_name: &str,
    tag: Option<&str>,
    column: Option<f64>,
    now: &str,
) -> f64 {
    promos
        .active_discount(model_id, provider_name, tag, now)
        .or(column)
        .unwrap_or(0.0)
        .clamp(0.0, 1.0)
}

fn parse_time(raw: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_sum_to_one() {
        for profile in builtin_profiles() {
            let total = profile.input + profile.output + profile.cache_read + profile.cache_write;
            assert!((total - 1.0).abs() < 1e-12, "{}", profile.name);
        }
        assert_eq!(balanced_profile().name, "balanced");
    }

    #[test]
    fn cost_blends_parts_and_discount() {
        let balanced = balanced_profile();
        // 0.5*1 + 0.4*3 + 0.05*0.1 + 0.05*0.2 = 1.715, half off.
        assert_eq!(
            effective_cost(Some(1.0), Some(3.0), Some(0.1), Some(0.2), &balanced, 0.5),
            Some(0.8575)
        );
        // Missing cache prices bill as input: 0.5*2 + 0.4*2 + 0.1*2 = 2.
        assert_eq!(
            effective_cost(Some(2.0), Some(2.0), None, None, &balanced, 0.0),
            Some(2.0)
        );
        // Missing prompt/completion is unpriceable; discounts clamp.
        assert_eq!(
            effective_cost(None, Some(1.0), None, None, &balanced, 0.0),
            None
        );
        assert_eq!(
            effective_cost(Some(1.0), Some(1.0), None, None, &balanced, 9.0),
            Some(0.0)
        );
    }

    #[test]
    fn cache_heavy_and_balanced_disagree() {
        let profiles = builtin_profiles();
        let balanced = &profiles[0];
        let cache_heavy = &profiles[1];
        // Cache-cheap vs cache-blind at the same prompt/completion price.
        let cheap_cache =
            effective_cost(Some(1.0), Some(1.0), Some(0.01), Some(0.01), balanced, 0.0);
        let blind_cache = effective_cost(Some(1.0), Some(1.0), None, None, balanced, 0.0);
        assert!(cheap_cache < blind_cache);
        // The gap widens under the cache-heavy mix.
        let gap_balanced = blind_cache.unwrap() - cheap_cache.unwrap();
        let cheap_cache = effective_cost(
            Some(1.0),
            Some(1.0),
            Some(0.01),
            Some(0.01),
            cache_heavy,
            0.0,
        );
        let blind_cache = effective_cost(Some(1.0), Some(1.0), None, None, cache_heavy, 0.0);
        assert!(blind_cache.unwrap() - cheap_cache.unwrap() > gap_balanced);
    }

    /// A05: a wrong-type tag fails the file instead of widening to a
    /// match-all window.
    #[test]
    fn wrong_type_tag_rejected() {
        let raw = r#"{"promos": [
            {"model_id": "m", "provider_name": "p", "tag": 42,
             "discount": 1.0, "starts_at": "2026-01-01T00:00:00Z",
             "ends_at": "2026-02-01T00:00:00Z"}
        ]}"#;
        assert!(Promos::from_str(raw).is_err());
        let raw = r#"{"promos": [
            {"model_id": "m", "provider_name": "p", "tag": ["t"],
             "discount": 1.0, "starts_at": "2026-01-01T00:00:00Z",
             "ends_at": "2026-02-01T00:00:00Z"}
        ]}"#;
        assert!(Promos::from_str(raw).is_err());
    }

    #[test]
    fn promo_windows_match_and_expire() {
        let promos = Promos::from_str(
            r#"{"promos": [
                {"model_id": "m", "provider_name": "p", "tag": "t",
                 "discount": 0.5, "starts_at": "2026-01-01T00:00:00Z",
                 "ends_at": "2026-02-01T00:00:00Z"},
                {"model_id": "m", "provider_name": "p",
                 "discount": 1.0, "starts_at": "2026-03-01T00:00:00Z",
                 "ends_at": "2026-04-01T00:00:00Z"}
            ]}"#,
        )
        .unwrap();
        // Tagged window matches its tag only, while active.
        assert_eq!(
            promos.active_discount("m", "p", Some("t"), "2026-01-15T00:00:00Z"),
            Some(0.5)
        );
        assert_eq!(
            promos.active_discount("m", "p", Some("other"), "2026-01-15T00:00:00Z"),
            None
        );
        // Untagged window matches every tag; expiry is exclusive.
        assert_eq!(
            promos.active_discount("m", "p", Some("t"), "2026-03-15T00:00:00Z"),
            Some(1.0)
        );
        assert_eq!(
            promos.active_discount("m", "p", None, "2026-04-01T00:00:00Z"),
            None
        );
        assert_eq!(
            promos.active_discount("m", "p", None, "2025-12-01T00:00:00Z"),
            None
        );
        // Column fallback when no window is active.
        assert_eq!(
            resolve_discount(&promos, "m", "p", None, Some(0.25), "2025-12-01T00:00:00Z"),
            0.25
        );
        assert_eq!(
            resolve_discount(&promos, "m", "p", None, None, "2025-12-01T00:00:00Z"),
            0.0
        );
    }

    #[test]
    fn invalid_promos_fail_at_load() {
        for raw in [
            r#"{"promos": {}}"#,
            r#"{"promos": [{"model_id": "m", "provider_name": "p",
                "discount": 1.5, "starts_at": "2026-01-01T00:00:00Z",
                "ends_at": "2026-02-01T00:00:00Z"}]}"#,
            r#"{"promos": [{"model_id": "m", "provider_name": "p",
                "discount": 0.5, "starts_at": "2026-02-01T00:00:00Z",
                "ends_at": "2026-01-01T00:00:00Z"}]}"#,
            r#"{"promos": [{"model_id": "m", "provider_name": "p",
                "discount": 0.5, "starts_at": "yesterday",
                "ends_at": "2026-01-01T00:00:00Z"}]}"#,
        ] {
            assert!(Promos::from_str(raw).is_err(), "should reject: {raw}");
        }
    }
}
