//! Phase 4: live scoring through a Baseten-hosted model.
//!
//! OpenAI chat completions with a Bearer key; shared prompt, request
//! shape, and validation live in `score_openai`. The API key travels
//! only in the Authorization header and is never logged (no `Debug`
//! impl on purpose).

use crate::score::{Score, Scorer};
use crate::score_openai::{chat_body, score_from_reply};
use crate::Error;
use serde_json::Value;
use std::time::Duration;

/// Live score over the Baseten OpenAI-compatible chat API.
pub struct BasetenScorer {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl BasetenScorer {
    /// Shared Model APIs / Frontier Gateway base URL.
    pub const BASE_URL: &'static str = "https://inference.baseten.co/v1";
    /// Hackathon scorer model a federated key is scoped to.
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

    /// Production score: documented base URL + model, key from `BASETEN_API_KEY`.
    pub fn from_env() -> Result<Self, Error> {
        let api_key = std::env::var("BASETEN_API_KEY").map_err(|_| {
            Error::Scorer("BASETEN_API_KEY is not set (see .env.example)".to_string())
        })?;
        if api_key.is_empty() {
            return Err(Error::Scorer(
                "BASETEN_API_KEY is empty (see .env.example)".to_string(),
            ));
        }
        Self::new(Self::BASE_URL, &api_key, Self::MODEL)
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
        score_from_reply("baseten", &reply)
    }
}

impl Scorer for BasetenScorer {
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
