//! Minimal Messages API client over raw reqwest (there is no official Rust
//! SDK). Non-streaming: each request is exactly one journaled step.

use anyhow::{Context, Result, bail};
use serde_json::Value;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

pub struct Anthropic {
    http: reqwest::Client,
    api_key: String,
    messages_url: String,
}

impl Anthropic {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY is not set — required for `beater agent run`")?;
        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self {
            http: reqwest::Client::new(),
            api_key,
            messages_url: messages_url(&base_url),
        })
    }

    pub async fn create_message(&self, body: &Value) -> Result<Value> {
        let mut delay = std::time::Duration::from_secs(2);
        for attempt in 1..=3 {
            let resp = self
                .http
                .post(&self.messages_url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            // 429 / 500 / 529 are retryable per the API error reference
            if (status.as_u16() == 429 || status.as_u16() >= 500) && attempt < 3 {
                tracing::warn!("anthropic {status}, retrying in {delay:?}");
                tokio::time::sleep(delay).await;
                delay *= 4;
                continue;
            }
            if !status.is_success() {
                bail!("anthropic api error {status}: {text}");
            }
            return serde_json::from_str(&text).context("anthropic response was not JSON");
        }
        unreachable!("retry loop returns or bails")
    }
}

fn messages_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1/messages") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1/messages")
    }
}

#[cfg(test)]
mod tests {
    use super::messages_url;

    #[test]
    fn base_url_override_accepts_root_or_messages_endpoint() {
        assert_eq!(
            messages_url("http://127.0.0.1:8123"),
            "http://127.0.0.1:8123/v1/messages"
        );
        assert_eq!(
            messages_url("http://127.0.0.1:8123/v1/messages"),
            "http://127.0.0.1:8123/v1/messages"
        );
        assert_eq!(
            messages_url("http://127.0.0.1:8123/"),
            "http://127.0.0.1:8123/v1/messages"
        );
    }
}
