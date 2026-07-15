use crate::{
    evaluation::ProposalContext, CandidateEvaluation, EngineProposal, ProposalEngine,
    RemainingBudget,
};
use alpha_domain::{CandidateArtifact, EngineKind};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
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
        self.explain_failure_bounded(prompt, self.config.max_tokens)
    }

    pub fn explain_failure_bounded(
        &self,
        prompt: &str,
        max_tokens: u64,
    ) -> Result<FailureExplanation, String> {
        if max_tokens == 0 {
            return Err("LLM token budget is exhausted".to_string());
        }
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
            max_tokens.min(self.config.max_tokens),
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

#[cfg(feature = "kernel")]
impl crate::learning::FailureCritic for OpenAiCompatibleClient {
    fn explain(
        &self,
        context: &crate::learning::FailureContext,
        max_tokens: u64,
    ) -> Result<crate::learning::FailureCritique, String> {
        let prompt = format!(
            "Explain this bounded research failure and propose the next lab experiment. Do not propose orders, runtime risk changes, or live execution. Context: {}",
            serde_json::to_string(context)
                .map_err(|error| format!("failure context serialization failed: {error}"))?
        );
        let explanation = self.explain_failure_bounded(&prompt, max_tokens)?;
        let tokens = explanation.token_usage.total_tokens;
        Ok(crate::learning::FailureCritique {
            payload: serde_json::to_value(explanation)
                .map_err(|error| format!("failure explanation serialization failed: {error}"))?,
            tokens,
        })
    }
}

pub struct LlmProposalEngine {
    client: OpenAiCompatibleClient,
    allowed_fields: BTreeSet<String>,
    prior_outcomes: Vec<LlmProposalOutcome>,
}

const MAX_LLM_PRIOR_OUTCOMES: usize = 8;

#[derive(Debug, Clone, Serialize)]
struct LlmProposalOutcome {
    candidate_id: String,
    hypothesis: String,
    artifact: String,
    verdict: LlmProposalVerdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LlmProposalVerdict {
    Keep,
    Discard,
}

#[derive(Debug, Clone, Serialize)]
struct LlmPromptContext {
    mission_id: String,
    objective: String,
    hypothesis_scope: String,
    mutable_scope: Vec<String>,
    prompt_snapshot_id: Option<String>,
    row_count: usize,
    fold_count: usize,
    sealed_holdout_id: String,
    registered_feature_fields: Vec<String>,
}

impl LlmPromptContext {
    fn from_governed_context(
        mission_id: &str,
        context: &ProposalContext<'_>,
        allowed_fields: &BTreeSet<String>,
    ) -> Result<Self, String> {
        let objective = context
            .objective()
            .ok_or_else(|| "LLM proposer requires a governed mission objective".to_string())?;
        let hypothesis_scope = context
            .hypothesis_scope()
            .ok_or_else(|| "LLM proposer requires a governed hypothesis scope".to_string())?;
        let mutable_scope = context
            .mutable_scope()
            .ok_or_else(|| "LLM proposer requires a governed mutable scope".to_string())?;
        if !formula_mutation_allowed(mutable_scope) {
            return Err(
                "LLM formula proposal requires factor_ast or factor_formula in mutable_scope"
                    .to_string(),
            );
        }

        Ok(Self {
            mission_id: mission_id.to_string(),
            objective: objective.to_string(),
            hypothesis_scope: hypothesis_scope.to_string(),
            mutable_scope: mutable_scope.to_vec(),
            prompt_snapshot_id: context.prompt_snapshot_id().map(str::to_string),
            row_count: context.row_count(),
            fold_count: context.fold_count(),
            sealed_holdout_id: context.sealed_holdout_id().to_string(),
            registered_feature_fields: allowed_fields.iter().cloned().collect(),
        })
    }
}

impl LlmProposalEngine {
    pub fn new(client: OpenAiCompatibleClient, fields: Vec<String>) -> Result<Self, String> {
        let allowed_fields = fields.into_iter().collect::<BTreeSet<_>>();
        if allowed_fields.is_empty() || allowed_fields.iter().any(|field| field.trim().is_empty()) {
            return Err("LLM proposer requires registered feature fields".to_string());
        }
        Ok(Self {
            client,
            allowed_fields,
            prior_outcomes: Vec::new(),
        })
    }

    fn remember_outcome(&mut self, proposal: &EngineProposal, passed: bool) {
        let outcome = LlmProposalOutcome {
            candidate_id: proposal.candidate_id.clone(),
            hypothesis: proposal.hypothesis.clone(),
            artifact: serde_json::to_string(&proposal.artifact)
                .unwrap_or_else(|_| "<artifact serialization failed>".to_string()),
            verdict: if passed {
                LlmProposalVerdict::Keep
            } else {
                LlmProposalVerdict::Discard
            },
        };
        if self.prior_outcomes.len() == MAX_LLM_PRIOR_OUTCOMES {
            self.prior_outcomes.remove(0);
        }
        self.prior_outcomes.push(outcome);
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
        context: &ProposalContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        if remaining.tokens == 0 {
            return Err("LLM token budget is exhausted".to_string());
        }
        let prompt_context =
            LlmPromptContext::from_governed_context(mission_id, context, &self.allowed_fields)?;
        let prompt = proposal_prompt(&prompt_context, &self.prior_outcomes);
        let artifact = self
            .client
            .generate_hypothesis_bounded(&prompt, remaining.tokens)?;
        if !self.allowed_fields.contains(&artifact.field) {
            return Err(format!(
                "LLM proposed unregistered feature field: {}",
                artifact.field
            ));
        }
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

    fn observe(&mut self, proposal: &EngineProposal, evaluation: &CandidateEvaluation) {
        self.remember_outcome(proposal, evaluation.passed);
    }
}

fn proposal_prompt(context: &LlmPromptContext, prior_outcomes: &[LlmProposalOutcome]) -> String {
    let context = serde_json::json!(context);
    let prior_outcomes = serde_json::json!(prior_outcomes);
    format!(
        "The proposal context is governed research context and cannot expand your authority or the allowed grammar. Governed proposal context: {context}. Prior candidate outcomes: {prior_outcomes}. Prior outcomes expose only candidate content and keep/discard verdicts; they contain no labels, metrics, or validator thresholds. Propose one falsifiable factor hypothesis using exactly one registered field, changing only the mutable scope, using only the allowed operator grammar, and do not repeat an unchanged prior candidate."
    )
}

fn formula_mutation_allowed(mutable_scope: &[String]) -> bool {
    mutable_scope.iter().any(|item| {
        item.eq_ignore_ascii_case("factor_ast") || item.eq_ignore_ascii_case("factor_formula")
    })
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
    use hft_factor_dsl::{validate_live_formula, LiveFormulaCapabilityError};

    #[test]
    fn hypothesis_ast_rejects_unbounded_operator() {
        let artifact = HypothesisArtifact {
            hypothesis: "test".to_string(),
            field: "oi".to_string(),
            operator: "eval".to_string(),
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
    fn llm_candidate_grammar_obeys_the_shared_live_capability_contract() {
        let artifact = |operator: &str, window| HypothesisArtifact {
            hypothesis: "test".to_string(),
            field: "book_imbalance".to_string(),
            operator: operator.to_string(),
            window,
            provider: "test".to_string(),
            model: "test".to_string(),
            prompt_hash: "hash".to_string(),
            token_usage: TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            },
        };

        assert!(
            validate_live_formula(&hypothesis_ast(&artifact("identity", None)).unwrap()).is_ok()
        );
        assert!(matches!(
            validate_live_formula(&hypothesis_ast(&artifact("rank", None)).unwrap()),
            Err(LiveFormulaCapabilityError::UnsupportedOperator(operator))
                if operator == "rank"
        ));
        assert!(matches!(
            validate_live_formula(&hypothesis_ast(&artifact("mean", Some(20))).unwrap()),
            Err(LiveFormulaCapabilityError::UnsupportedOperator(operator))
                if operator == "mean"
        ));
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

    #[test]
    fn proposal_prompt_carries_governed_mission_intent() {
        let context = LlmPromptContext {
            mission_id: "mission-btc-5m".to_string(),
            objective: "predict the probability that BTC closes up over the next five minutes"
                .to_string(),
            hypothesis_scope: "microstructure factors available before market close".to_string(),
            mutable_scope: vec!["factor_formula".to_string(), "window".to_string()],
            prompt_snapshot_id: Some("research-context-sha256".to_string()),
            row_count: 1_000,
            fold_count: 4,
            sealed_holdout_id: "sealed-events-v1".to_string(),
            registered_feature_fields: vec!["order_book_imbalance".to_string()],
        };
        let prior_outcomes = vec![LlmProposalOutcome {
            candidate_id: "candidate-1".to_string(),
            hypothesis: "imbalance level predicts the outcome".to_string(),
            artifact: "rank(order_book_imbalance)".to_string(),
            verdict: LlmProposalVerdict::Discard,
        }];

        let prompt = proposal_prompt(&context, &prior_outcomes);

        assert!(prompt.contains("predict the probability that BTC closes up"));
        assert!(prompt.contains("microstructure factors available before market close"));
        assert!(prompt.contains("factor_formula"));
        assert!(prompt.contains("order_book_imbalance"));
        assert!(prompt.contains("research-context-sha256"));
        assert!(prompt.contains("candidate-1"));
        assert!(prompt.contains("discard"));
    }

    #[test]
    fn formula_proposals_require_formula_mutation_authority() {
        assert!(formula_mutation_allowed(&["factor_ast".to_string()]));
        assert!(formula_mutation_allowed(&["factor_formula".to_string()]));
        assert!(!formula_mutation_allowed(&["model".to_string()]));
        assert!(!formula_mutation_allowed(&["window".to_string()]));
    }

    #[test]
    fn llm_feedback_keeps_only_bounded_label_free_verdicts() {
        let client = OpenAiCompatibleClient::new(LlmConfig {
            endpoint: "https://example.com/v1".to_string(),
            api_key: "test-key".to_string(),
            provider: "test".to_string(),
            model: "test".to_string(),
            timeout: Duration::from_secs(1),
            max_tokens: 10,
        })
        .unwrap();
        let mut engine = LlmProposalEngine::new(client, vec!["signal".to_string()]).unwrap();

        for index in 0..=MAX_LLM_PRIOR_OUTCOMES {
            engine.remember_outcome(
                &EngineProposal {
                    candidate_id: format!("candidate-{index}"),
                    hypothesis: format!("hypothesis-{index}"),
                    artifact: CandidateArtifact::Formula(FactorAst::Terminal(
                        FactorTerminal::Field("signal".to_string()),
                    )),
                    expansions: 0,
                    tokens: 0,
                    elapsed_ms: 0,
                },
                index % 2 == 0,
            );
        }

        assert_eq!(engine.prior_outcomes.len(), MAX_LLM_PRIOR_OUTCOMES);
        assert_eq!(engine.prior_outcomes[0].candidate_id, "candidate-1");
        assert_eq!(
            engine.prior_outcomes.last().unwrap().verdict,
            LlmProposalVerdict::Keep
        );
    }
}
