//! Phase 4: live judging through a Modal-hosted model.
//!
//! The deployment speaks the OpenAI chat API and authenticates by the
//! Modal proxy-token pair in `Modal-Key`/`Modal-Secret` headers; shared
//! prompt, request shape, and validation live in `judge_openai`. The key
//! pair travels only in headers and is never logged (no `Debug` impl on
//! purpose).

use crate::judge::{Judge, Judgment};
use crate::judge_openai::{chat_body, judgment_from_reply};
use crate::Error;
use serde_json::Value;
use std::time::Duration;

/// Live judge over a Modal-hosted OpenAI-compatible chat endpoint.
pub struct ModalJudge {
    client: reqwest::blocking::Client,
    base_url: String,
    key_id: String,
    key_secret: String,
    model: String,
}

impl ModalJudge {
    /// Judge model the deployment serves (`GET /v1/models`).
    pub const MODEL: &'static str = "meta-models/Muse-Spark-Open-816B-A42B";

    pub fn new(base_url: &str, key_id: &str, key_secret: &str, model: &str) -> Result<Self, Error> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            key_id: key_id.to_string(),
            key_secret: key_secret.to_string(),
            model: model.to_string(),
        })
    }

    /// Production judge: endpoint + proxy-token pair from the environment.
    /// The deployment URL is the server root; the chat API lives under
    /// `/v1`, so it is appended unless already present.
    pub fn from_env() -> Result<Self, Error> {
        let deployment_url = judge_env("MODAL_JUDGE_URL")?;
        let key_id = judge_env("MODAL_KEY_ID")?;
        let key_secret = judge_env("MODAL_KEY_SECRET")?;
        Self::new(
            &api_root(&deployment_url),
            &key_id,
            &key_secret,
            Self::MODEL,
        )
    }

    fn chat(
        &self,
        model_id: &str,
        provider_name: &str,
        payload: &Value,
    ) -> Result<Judgment, Error> {
        let body = chat_body(&self.model, model_id, provider_name, payload);
        let url = format!("{}/chat/completions", self.base_url);
        let reply: Value = self
            .client
            .post(&url)
            .header("Modal-Key", &self.key_id)
            .header("Modal-Secret", &self.key_secret)
            .json(&body)
            .send()?
            .error_for_status()?
            .json()?;
        judgment_from_reply("modal", &reply)
    }
}

impl Judge for ModalJudge {
    fn judge(
        &self,
        model_id: &str,
        provider_name: &str,
        payload: &Value,
    ) -> Result<Judgment, Error> {
        self.chat(model_id, provider_name, payload)
    }
}

/// One required judge setting: set and non-empty, else a keyed error.
fn judge_env(name: &str) -> Result<String, Error> {
    let value = std::env::var(name)
        .map_err(|_| Error::Judge(format!("{name} is not set (see .env.example)")))?;
    if value.is_empty() {
        return Err(Error::Judge(format!("{name} is empty (see .env.example)")));
    }
    Ok(value)
}

/// Deployment URL (server root) → chat API root: trimmed, `/v1` appended
/// unless already present.
fn api_root(deployment_url: &str) -> String {
    let trimmed = deployment_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_root_appends_v1_once() {
        assert_eq!(
            api_root("https://x.modal.direct"),
            "https://x.modal.direct/v1"
        );
        assert_eq!(
            api_root("https://x.modal.direct/"),
            "https://x.modal.direct/v1"
        );
        assert_eq!(
            api_root("https://x.modal.direct/v1"),
            "https://x.modal.direct/v1"
        );
    }
}
