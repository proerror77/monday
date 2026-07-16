//! One-shot OpenAI-compatible proposal client for governed prediction research.
//!
//! This is intentionally only a transport boundary. The prediction loop owns
//! call accounting, retries, proposal semantics, and resume behavior.

use std::fmt;
use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::prediction_loop::{prediction_proposal_json_schema, ProposalCallOutput, ProposalClient};

pub const MONDAY_PREDICTION_LLM_BASE_URL_ENV: &str = "MONDAY_PREDICTION_LLM_BASE_URL";
pub const MONDAY_PREDICTION_LLM_MODEL_ENV: &str = "MONDAY_PREDICTION_LLM_MODEL";
pub const MONDAY_PREDICTION_LLM_API_KEY_ENV: &str = "MONDAY_PREDICTION_LLM_API_KEY";
pub const MONDAY_PREDICTION_LLM_PROVIDER_ENV: &str = "MONDAY_PREDICTION_LLM_PROVIDER";
pub const DEFAULT_PROPOSAL_RESPONSE_MAX_BYTES: usize = 256 * 1024;

pub struct ProposalClientConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub provider: String,
    pub timeout: Duration,
    pub max_response_bytes: usize,
}

impl fmt::Debug for ProposalClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProposalClientConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("provider", &self.provider)
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl ProposalClientConfig {
    pub fn from_env(timeout: Duration) -> Result<Self> {
        let base_url = required_env(MONDAY_PREDICTION_LLM_BASE_URL_ENV)?;
        let model = required_env(MONDAY_PREDICTION_LLM_MODEL_ENV)?;
        let api_key = std::env::var(MONDAY_PREDICTION_LLM_API_KEY_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let provider = std::env::var(MONDAY_PREDICTION_LLM_PROVIDER_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "openai-compatible".to_string());
        Ok(Self {
            base_url,
            model,
            api_key,
            provider,
            timeout,
            max_response_bytes: DEFAULT_PROPOSAL_RESPONSE_MAX_BYTES,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalProviderMetadata {
    pub provider: String,
    pub requested_model: String,
    pub response_model: Option<String>,
    pub response_id: Option<String>,
    pub finish_reason: Option<String>,
    pub system_fingerprint: Option<String>,
    pub usage: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalCallResponse {
    pub raw_proposal_json: String,
    pub provider_metadata: ProposalProviderMetadata,
}

pub struct OpenAiCompatibleProposalClient {
    client: Client,
    endpoint: Url,
    model: String,
    api_key: Option<String>,
    provider: String,
    timeout: Duration,
    max_response_bytes: usize,
}

impl OpenAiCompatibleProposalClient {
    pub fn from_env(timeout: Duration) -> Result<Self> {
        Self::new(ProposalClientConfig::from_env(timeout)?)
    }

    pub fn new(config: ProposalClientConfig) -> Result<Self> {
        if config.model.trim().is_empty() {
            bail!("proposal model must not be empty");
        }
        if config.provider.trim().is_empty() {
            bail!("proposal provider must not be empty");
        }
        if config.timeout.is_zero() {
            bail!("proposal timeout must be positive");
        }
        if config.max_response_bytes == 0 {
            bail!("proposal max_response_bytes must be positive");
        }
        let endpoint = chat_completions_endpoint(&config.base_url)?;
        let mut client_builder = Client::builder()
            .timeout(config.timeout)
            .redirect(Policy::none());
        if is_loopback_host(&endpoint) {
            client_builder = client_builder.no_proxy();
        }
        let client = client_builder
            .build()
            .context("build OpenAI-compatible proposal HTTP client")?;
        Ok(Self {
            client,
            endpoint,
            model: config.model,
            api_key: config.api_key,
            provider: config.provider,
            timeout: config.timeout,
            max_response_bytes: config.max_response_bytes,
        })
    }

    /// Makes exactly one HTTP request. Callers own all retry and call-ledger policy.
    pub fn propose(&self, prompt: &str) -> Result<ProposalCallResponse> {
        self.propose_with_timeout(prompt, self.timeout)
    }

    fn propose_with_timeout(
        &self,
        prompt: &str,
        timeout: Duration,
    ) -> Result<ProposalCallResponse> {
        if prompt.trim().is_empty() {
            bail!("proposal prompt must not be empty");
        }
        if timeout.is_zero() {
            bail!("proposal timeout must be positive");
        }
        let request = chat_completion_request(&self.model, prompt);
        let mut builder = self
            .client
            .post(self.endpoint.clone())
            .timeout(timeout)
            .json(&request);
        if let Some(api_key) = self.api_key.as_deref() {
            builder = builder.bearer_auth(api_key);
        }
        let response = builder
            .send()
            .context("OpenAI-compatible proposal request failed")?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            bail!(
                "proposal response exceeds {} bytes",
                self.max_response_bytes
            );
        }
        let body = read_bounded(response, self.max_response_bytes)
            .context("read OpenAI-compatible proposal response")?;
        if !status.is_success() {
            bail!("proposal provider returned HTTP {status}");
        }
        parse_chat_completion_response(&body, &self.model, &self.provider)
    }
}

impl ProposalClient for OpenAiCompatibleProposalClient {
    fn propose(
        &mut self,
        prompt: &str,
        timeout: Duration,
    ) -> std::result::Result<ProposalCallOutput, String> {
        let response = self
            .propose_with_timeout(prompt, timeout)
            .map_err(|error| format!("{error:#}"))?;
        Ok(ProposalCallOutput {
            raw_response: response.raw_proposal_json,
            provider: response.provider_metadata.provider,
            model: response
                .provider_metadata
                .response_model
                .unwrap_or(response.provider_metadata.requested_model),
            usage: response.provider_metadata.usage.unwrap_or(Value::Null),
        })
    }
}

fn chat_completion_request(model: &str, prompt: &str) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "n": 1,
        "stream": false,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "prediction_probability_blends",
                "strict": true,
                "schema": prediction_proposal_json_schema()
            }
        }
    })
}

fn chat_completions_endpoint(base_url: &str) -> Result<Url> {
    let mut url = Url::parse(base_url.trim()).context("invalid proposal base URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("proposal base URL must not contain credentials, query, or fragment");
    }
    if url.host_str().is_none() {
        bail!("proposal base URL must contain a host");
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&url) => {}
        "http" => bail!("plaintext proposal HTTP is allowed only for a loopback Grok Builder"),
        scheme => bail!("unsupported proposal URL scheme {scheme}"),
    }
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/chat/completions") {
        path.to_string()
    } else {
        format!("{path}/chat/completions")
    };
    url.set_path(&path);
    Ok(url)
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host_str() {
        Some(host) if host.eq_ignore_ascii_case("localhost") => true,
        Some(host) => host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

fn required_env(name: &str) -> Result<String> {
    let value = std::env::var(name).with_context(|| format!("{name} is required"))?;
    if value.trim().is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(value.trim().to_string())
}

fn read_bounded(reader: impl Read, max_bytes: usize) -> Result<Vec<u8>> {
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut body = Vec::with_capacity(max_bytes.min(8192));
    reader.take(limit).read_to_end(&mut body)?;
    if body.len() > max_bytes {
        bail!("proposal response exceeds {max_bytes} bytes");
    }
    Ok(body)
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    system_fingerprint: Option<String>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: ChatContent,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

#[derive(Deserialize)]
struct ChatContentPart {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

fn parse_chat_completion_response(
    body: &[u8],
    requested_model: &str,
    provider: &str,
) -> Result<ProposalCallResponse> {
    let response: ChatCompletionResponse = serde_json::from_slice(body)
        .context("proposal response is not valid chat-completions JSON")?;
    if response.choices.len() != 1 {
        bail!("proposal response must contain exactly one choice");
    }
    let choice = response.choices.into_iter().next().expect("length checked");
    if choice.finish_reason.as_deref() != Some("stop") {
        bail!(
            "proposal response did not finish normally: {}",
            choice.finish_reason.as_deref().unwrap_or("<missing>")
        );
    }
    let raw_proposal_json = match choice.message.content {
        ChatContent::Text(text) => text,
        ChatContent::Parts(parts) => {
            if parts.is_empty()
                || parts
                    .iter()
                    .any(|part| !matches!(part.kind.as_str(), "text" | "output_text"))
            {
                bail!("proposal response content parts must contain only text");
            }
            parts.into_iter().map(|part| part.text).collect()
        }
    };
    if raw_proposal_json.trim().is_empty() {
        bail!("proposal response content must not be empty");
    }
    Ok(ProposalCallResponse {
        raw_proposal_json,
        provider_metadata: ProposalProviderMetadata {
            provider: provider.to_string(),
            requested_model: requested_model.to_string(),
            response_model: response.model,
            response_id: response.id,
            finish_reason: choice.finish_reason,
            system_fingerprint: response.system_fingerprint,
            usage: response.usage,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const PROPOSAL: &str = r#"{"probability_blends":[{"name":"binance_flow","hypothesis":"Binance flow improves held-out Brier score.","market_midpoint_weight":1.0,"chainlink_digital_weight":1.0,"distance_lob_vol_weight":1.0,"event_surface_weight":0.0,"existing_model_weight":0.0}]}"#;

    #[test]
    fn request_reuses_the_public_strict_schema() {
        let request = chat_completion_request("grok-local", "propose");
        assert_eq!(
            request["response_format"]["json_schema"]["schema"],
            prediction_proposal_json_schema()
        );
        assert!(
            request["response_format"]["json_schema"]["schema"]["properties"]
                .get("mutations")
                .is_none()
        );
    }

    #[test]
    fn local_http_and_https_endpoints_are_normalized() {
        assert_eq!(
            chat_completions_endpoint("http://127.0.0.1:11434/v1")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1/chat/completions")
                .unwrap()
                .as_str(),
            "https://api.x.ai/v1/chat/completions"
        );
        assert!(chat_completions_endpoint("http://example.com/v1").is_err());
        assert!(chat_completions_endpoint("https://user:secret@example.com/v1").is_err());
        assert!(chat_completions_endpoint("http://[::1]:11434/v1").is_ok());
    }

    #[test]
    fn parses_standard_chat_completion_and_metadata() {
        let body = serde_json::to_vec(&json!({
            "id": "chatcmpl-local-1",
            "model": "grok-local-q4",
            "system_fingerprint": "builder-7",
            "choices": [{
                "message": {"role": "assistant", "content": PROPOSAL},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        }))
        .unwrap();
        let parsed = parse_chat_completion_response(&body, "grok-local", "grok-builder").unwrap();
        assert_eq!(parsed.raw_proposal_json, PROPOSAL);
        assert_eq!(parsed.provider_metadata.provider, "grok-builder");
        assert_eq!(
            parsed.provider_metadata.response_model.as_deref(),
            Some("grok-local-q4")
        );
        assert_eq!(
            parsed.provider_metadata.finish_reason.as_deref(),
            Some("stop")
        );
    }

    #[test]
    fn preserves_nonempty_content_for_durable_loop_validation() {
        let body = serde_json::to_vec(&json!({
            "choices": [{
                "message": {"content": "not-json"},
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
        let parsed = parse_chat_completion_response(&body, "model", "provider").unwrap();
        assert_eq!(parsed.raw_proposal_json, "not-json");
    }

    #[test]
    fn rejects_oversized_or_invalid_envelopes() {
        assert!(read_bounded(Cursor::new(vec![0_u8; 5]), 4).is_err());
        let body = serde_json::to_vec(&json!({
            "choices": []
        }))
        .unwrap();
        assert!(parse_chat_completion_response(&body, "model", "provider").is_err());

        let truncated = serde_json::to_vec(&json!({
            "choices": [{
                "message": {"content": PROPOSAL},
                "finish_reason": "length"
            }]
        }))
        .unwrap();
        assert!(parse_chat_completion_response(&truncated, "model", "provider").is_err());

        let empty = serde_json::to_vec(&json!({
            "choices": [{
                "message": {"content": "  "},
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
        assert!(parse_chat_completion_response(&empty, "model", "provider").is_err());
    }
}
