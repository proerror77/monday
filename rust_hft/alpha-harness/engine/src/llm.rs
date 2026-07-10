use crate::{evaluation::EngineContext, EngineProposal, ProposalEngine, RemainingBudget};
use alpha_domain::{CandidateArtifact, EngineKind};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub struct LlmConfig {
    pub endpoint: String,
    pub api_key: String,
    pub provider: String,
    pub model: String,
    pub timeout: Duration,
    pub max_tokens: u64,
}

impl LlmConfig {
    pub fn from_env() -> Result<Self, String> {
        let endpoint = std::env::var("ALPHA_LLM_ENDPOINT")
            .map_err(|_| "ALPHA_LLM_ENDPOINT is required".to_string())?;
        let api_key = std::env::var("ALPHA_LLM_API_KEY")
            .map_err(|_| "ALPHA_LLM_API_KEY is required".to_string())?;
        let model = std::env::var("ALPHA_LLM_MODEL")
            .map_err(|_| "ALPHA_LLM_MODEL is required".to_string())?;
        let provider =
            std::env::var("ALPHA_LLM_PROVIDER").unwrap_or_else(|_| "openai-compatible".to_string());
        let config = Self {
            endpoint,
            api_key,
            provider,
            model,
            timeout: Duration::from_secs(30),
            max_tokens: 1_000,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if self.endpoint.trim().is_empty()
            || self.api_key.trim().is_empty()
            || self.provider.trim().is_empty()
            || self.model.trim().is_empty()
            || self.max_tokens == 0
        {
            return Err("LLM configuration contains an empty required value".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HypothesisArtifact {
    pub hypothesis: String,
    pub field: String,
    pub operator: String,
    pub window: Option<u64>,
    pub provider: String,
    pub model: String,
    pub prompt_hash: String,
    pub token_usage: TokenUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureExplanation {
    pub failure_class: String,
    pub explanation: String,
    pub next_experiment: String,
    pub provider: String,
    pub model: String,
    pub prompt_hash: String,
    pub token_usage: TokenUsage,
}

#[derive(Debug, Deserialize)]
struct RawHypothesis {
    hypothesis: String,
    field: String,
    operator: String,
    window: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawFailureExplanation {
    failure_class: String,
    explanation: String,
    next_experiment: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Message {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

pub struct OpenAiCompatibleClient {
    config: LlmConfig,
    client: reqwest::blocking::Client,
}

impl OpenAiCompatibleClient {
    pub fn new(config: LlmConfig) -> Result<Self, String> {
        config.validate()?;
        let client = reqwest::blocking::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|error| format!("failed to build LLM HTTP client: {error}"))?;
        Ok(Self { config, client })
    }

    pub fn generate_hypothesis(&self, prompt: &str) -> Result<HypothesisArtifact, String> {
        self.generate_hypothesis_bounded(prompt, self.config.max_tokens)
    }

    pub fn generate_hypothesis_bounded(
        &self,
        prompt: &str,
        max_tokens: u64,
    ) -> Result<HypothesisArtifact, String> {
        if max_tokens == 0 {
            return Err("LLM token budget is exhausted".to_string());
        }
        let (content, usage, prompt_hash) = self.request_json(
            prompt,
            "hypothesis_artifact",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["hypothesis", "field", "operator", "window"],
                "properties": {
                    "hypothesis": {"type": "string"},
                    "field": {"type": "string"},
                    "operator": {"type": "string", "enum": ["identity", "rank", "delta", "mean", "zscore"]},
                    "window": {"type": ["integer", "null"], "minimum": 1, "maximum": 10000}
                }
            }),
            max_tokens.min(self.config.max_tokens),
        )?;
        let raw: RawHypothesis = serde_json::from_str(&content)
            .map_err(|error| format!("LLM hypothesis JSON is invalid: {error}"))?;
        validate_text(&raw.hypothesis, "hypothesis")?;
        validate_text(&raw.field, "field")?;
        validate_operator(&raw.operator, raw.window)?;
        Ok(HypothesisArtifact {
            hypothesis: raw.hypothesis,
            field: raw.field,
            operator: raw.operator,
            window: raw.window,
            provider: self.config.provider.clone(),
            model: self.config.model.clone(),
            prompt_hash,
            token_usage: usage,
        })
    }

    pub fn explain_failure(&self, prompt: &str) -> Result<FailureExplanation, String> {
        let (content, usage, prompt_hash) = self.request_json(
            prompt,
            "failure_explanation",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["failure_class", "explanation", "next_experiment"],
                "properties": {
                    "failure_class": {"type": "string"},
                    "explanation": {"type": "string"},
                    "next_experiment": {"type": "string"}
                }
            }),
            self.config.max_tokens,
        )?;
        let raw: RawFailureExplanation = serde_json::from_str(&content)
            .map_err(|error| format!("LLM failure JSON is invalid: {error}"))?;
        validate_text(&raw.failure_class, "failure_class")?;
        validate_text(&raw.explanation, "explanation")?;
        validate_text(&raw.next_experiment, "next_experiment")?;
        Ok(FailureExplanation {
            failure_class: raw.failure_class,
            explanation: raw.explanation,
            next_experiment: raw.next_experiment,
            provider: self.config.provider.clone(),
            model: self.config.model.clone(),
            prompt_hash,
            token_usage: usage,
        })
    }

    fn request_json(
        &self,
        prompt: &str,
        schema_name: &str,
        schema: serde_json::Value,
        max_tokens: u64,
    ) -> Result<(String, TokenUsage, String), String> {
        validate_text(prompt, "prompt")?;
        let prompt_hash = hex::encode(Sha256::digest(prompt.as_bytes()));
        let url = format!(
            "{}/chat/completions",
            self.config.endpoint.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.config.api_key)
            .json(&serde_json::json!({
                "model": self.config.model,
                "messages": [
                    {"role": "system", "content": "Return only JSON matching the supplied schema. Do not emit code or trading instructions."},
                    {"role": "user", "content": prompt}
                ],
                "max_tokens": max_tokens,
                "response_format": {
                    "type": "json_schema",
                    "json_schema": {
                        "name": schema_name,
                        "strict": true,
                        "schema": schema
                    }
                }
            }))
            .send()
            .map_err(|error| format!("LLM request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("LLM request returned HTTP {}", status.as_u16()));
        }
        let response: ChatResponse = response
            .json()
            .map_err(|error| format!("LLM response JSON is invalid: {error}"))?;
        let content = response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| "LLM response contains no choice".to_string())?;
        let usage = response
            .usage
            .ok_or_else(|| "LLM response contains no real token usage".to_string())?;
        Ok((
            content,
            TokenUsage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
            },
            prompt_hash,
        ))
    }
}

pub struct LlmProposalEngine {
    client: OpenAiCompatibleClient,
}

impl LlmProposalEngine {
    pub fn new(client: OpenAiCompatibleClient) -> Self {
        Self { client }
    }
}

impl ProposalEngine for LlmProposalEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::LlmProposer
    }

    fn propose(
        &mut self,
        mission_id: &str,
        iteration_index: usize,
        context: &EngineContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        if remaining.tokens == 0 {
            return Err("LLM token budget is exhausted".to_string());
        }
        let prompt = format!(
            "Mission: {mission_id}. Available research rows: {}. Walk-forward folds: {}. Sealed holdout id: {}. Propose one testable factor hypothesis using only a registered field and the allowed operator grammar.",
            context.rows().len(),
            context.folds().len(),
            context.sealed_holdout_id(),
        );
        let artifact = self
            .client
            .generate_hypothesis_bounded(&prompt, remaining.tokens)?;
        if artifact.token_usage.total_tokens > remaining.tokens {
            return Err("LLM response exceeded remaining token budget".to_string());
        }
        let ast = hypothesis_ast(&artifact)?;
        Ok(EngineProposal {
            candidate_id: format!("{mission_id}-llm-{iteration_index}"),
            hypothesis: artifact.hypothesis.clone(),
            artifact: CandidateArtifact::Formula(ast),
            expansions: 0,
            tokens: artifact.token_usage.total_tokens,
            elapsed_ms: 0,
        })
    }
}

fn hypothesis_ast(artifact: &HypothesisArtifact) -> Result<FactorAst, String> {
    let field = FactorAst::Terminal(FactorTerminal::Field(artifact.field.clone()));
    let window = || {
        artifact
            .window
            .map(|value| FactorAst::Terminal(FactorTerminal::Constant(value.to_string())))
            .ok_or_else(|| format!("operator {} requires a window", artifact.operator))
    };
    match artifact.operator.as_str() {
        "identity" => Ok(field),
        "rank" => FactorAst::call(FactorOperator::Rank, vec![field]),
        "delta" => FactorAst::call(FactorOperator::Delta, vec![field, window()?]),
        "mean" => FactorAst::call(FactorOperator::Mean, vec![field, window()?]),
        "zscore" => FactorAst::call(FactorOperator::ZScore, vec![field, window()?]),
        other => return Err(format!("LLM operator is not allowed: {other}")),
    }
    .map_err(|error| error.to_string())
}

fn validate_operator(operator: &str, window: Option<u64>) -> Result<(), String> {
    match operator {
        "identity" | "rank" if window.is_none() => Ok(()),
        "delta" | "mean" | "zscore" if window.is_some_and(|value| value > 0) => Ok(()),
        "identity" | "rank" | "delta" | "mean" | "zscore" => {
            Err("LLM operator/window combination is invalid".to_string())
        }
        _ => Err("LLM operator is outside the allowed grammar".to_string()),
    }
}

fn validate_text(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{name} cannot be empty"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hypothesis_ast_rejects_unbounded_operator() {
        let artifact = HypothesisArtifact {
            hypothesis: "test".to_string(),
            field: "oi".to_string(),
            operator: "python".to_string(),
            window: None,
            provider: "test".to_string(),
            model: "test".to_string(),
            prompt_hash: "hash".to_string(),
            token_usage: TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            },
        };
        assert!(hypothesis_ast(&artifact).is_err());
    }

    #[test]
    fn config_rejects_missing_secret() {
        let config = LlmConfig {
            endpoint: "https://example.com/v1".to_string(),
            api_key: String::new(),
            provider: "test".to_string(),
            model: "test".to_string(),
            timeout: Duration::from_secs(1),
            max_tokens: 10,
        };
        assert!(OpenAiCompatibleClient::new(config).is_err());
    }
}
