//! Iter 30: live scoring through an OpenRouter-hosted model.
//!
//! OpenAI chat completions with a Bearer [REDACTED]; shared prompt, request
//! shape, and validation live in `score_openai`. The scorer model is
//! operator-configured (`OPENROUTER_SCORE_MODEL`, fail-closed when
//! unset — OpenRouter models come and go, so nothing is pinned), and
//! the key is the shared `RANKINGS_API_KEY` (Data API pins + score
//! leg, offline batch only, never the gateway). The key travels only
//! in the Authorization header and is never logged (no `Debug` impl
//! on purpose).

use crate::score::{Score, Scorer};
use crate::score_openai::{chat_body, score_from_reply};
use crate::Error;
use serde_json::Value;
use std::time::Duration;

/// Live scorer over the OpenRouter OpenAI-compatible chat API.
pub struct OpenRouterScorer {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenRouterScorer {
    /// OpenRouter chat completions root (`POST /chat/completions`).
    pub const BASE_URL: &'static str = "https://openrouter.ai/api/v1";

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

    /// Production score: documented base URL, model from
    /// `OPENROUTER_SCORE_MODEL`, key from the shared
    /// `RANKINGS_API_KEY`. Both fail closed when unset or empty.
    pub fn from_env() -> Result<Self, Error> {
        let model = score_env("OPENROUTER_SCORE_MODEL")?;
        let api_key = score_env("RANKINGS_API_KEY")?;
        Self::new(Self::BASE_URL, &api_key, &model)
    }

    fn chat(&self, model_id: &str, provider_name: &str, payload: &Value) -> Result<Score, Error> {
        let body = chat_body(&self.model, model_id, provider_name, payload);
        let url = format!("{}/chat/completions", self.base_url);
        let reply: Value = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()?
            .error_for_status()?
            .json()?;
        score_from_reply("openrouter", &reply)
    }
}

impl Scorer for OpenRouterScorer {
    fn identity(&self) -> crate::score::ScorerIdentity {
        crate::score::ScorerIdentity {
            model: self.model.clone(),
            prompt_version: crate::score_openai::PROMPT_VERSION.to_string(),
        }
    }

    fn score(&self, model_id: &str, provider_name: &str, payload: &Value) -> Result<Score, Error> {
        self.chat(model_id, provider_name, payload)
    }
}

/// One required score setting: set and non-empty, else a keyed error.
fn score_env(name: &str) -> Result<String, Error> {
    let value = std::env::var(name)
        .map_err(|_| Error::Scorer(format!("{name} is not set (see .env.example)")))?;
    if value.is_empty() {
        return Err(Error::Scorer(format!("{name} is empty (see .env.example)")));
    }
    Ok(value)
}
