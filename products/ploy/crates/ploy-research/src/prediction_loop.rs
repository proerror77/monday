//! Rust-owned, bounded research loop for one BTC or SOL prediction-market mission.
//!
//! This module owns mission validation, proposal budgets, resume state, and
//! deterministic evaluator feedback.  It has no execution or live-trading
//! authority.  Continuous-contract research remains in Monday's separate
//! `alpha-harness` workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fd::OwnedFd;
use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::prediction_loop_fs::{
    atomic_write_json, canonical_json_bytes, next_attempt_dir, read_json, sha256_hex,
    verify_artifact, write_content_addressed_json, write_content_addressed_text, ArtifactRef,
    OutputLock, OutputRoot,
};
use crate::{
    load_research_snapshot, normalized_underlying_symbol, recompute_prediction_research_outcome,
    validate_governed_chainlink_5m_settlement_evidence, validate_prediction_research_prior,
    LlmPriorSpec, LlmProbabilityBlendSpec, PredictionResearchFeedback,
    PredictionResearchGatePolicy, ResearchSnapshot, ResearchSnapshotManifest,
};

pub const PREDICTION_MISSION_SCHEMA_VERSION: &str = "prediction_research_mission.v1";
pub const PREDICTION_LOOP_TARGET: &str = "full_depth_settlement_executable_pnl";
pub const PREDICTION_EVENT_WINDOW_SECS: i64 = 300;
pub const PREDICTION_LOOP_STATE_SCHEMA_VERSION: &str = "ploy_prediction_research_loop.v4";
const PREDICTION_FEEDBACK_SCHEMA_VERSION: &str = "prediction_research_feedback.v3";
const SEARCH_PASS_EVIDENCE_SCHEMA_VERSION: &str = "prediction_research_search_pass.v1";
const SEARCH_PASS_EVIDENCE_PREFIX: &str = "prediction-research-search-pass";
const PROBABILITY_WEIGHT_EPSILON: f64 = 1e-9;
const MAX_GOVERNED_CANDIDATES: usize = 64;
const MAX_GOVERNED_LLM_CALLS: usize = 16;
const MAX_GOVERNED_SECONDS: u64 = 86_400;
pub const REQUIRED_BINANCE_DATA_REQUIREMENTS: [&str; 3] =
    ["binance_price", "binance_agg_trades", "binance_lob"];
pub const REQUIRED_BINANCE_SOURCE_SURFACES: [&str; 3] = [
    "binance_price_ticks",
    "binance_agg_trade_ticks",
    "binance_lob_ticks",
];
pub const REQUIRED_CHAINLINK_SOURCE_SURFACE: &str = "chainlink_reference_ticks";
pub const REQUIRED_POLYMARKET_SOURCE_SURFACE: &str = "clob_orderbook_snapshots";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionSearchBudget {
    pub max_candidates: usize,
    pub max_llm_calls: usize,
    pub max_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionResearchMission {
    pub schema_version: String,
    pub mission_id: String,
    pub lane: String,
    pub objective: String,
    pub hypothesis_scope: String,
    pub mutable_scope: Vec<String>,
    pub data_snapshot_id: String,
    pub target: String,
    pub symbols: Vec<String>,
    pub horizon: String,
    pub prompt_snapshot_id: String,
    pub search_policy_snapshot_id: String,
    pub search_budget: PredictionSearchBudget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedProbabilityBlend {
    pub name: String,
    pub hypothesis: String,
    pub market_midpoint_weight: f64,
    pub chainlink_digital_weight: f64,
    pub distance_lob_vol_weight: f64,
    pub event_surface_weight: f64,
    pub existing_model_weight: f64,
}

impl From<ProposedProbabilityBlend> for LlmProbabilityBlendSpec {
    fn from(value: ProposedProbabilityBlend) -> Self {
        Self {
            name: value.name,
            hypothesis: value.hypothesis,
            market_midpoint_weight: value.market_midpoint_weight,
            chainlink_digital_weight: value.chainlink_digital_weight,
            distance_lob_vol_weight: value.distance_lob_vol_weight,
            event_surface_weight: value.event_surface_weight,
            existing_model_weight: value.existing_model_weight,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionProposal {
    pub probability_blends: Vec<ProposedProbabilityBlend>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbabilityComponentDescription {
    pub name: &'static str,
    pub inputs: &'static str,
    pub theory: &'static str,
    pub authority: &'static str,
}

pub fn registered_probability_components() -> [ProbabilityComponentDescription; 5] {
    [
        ProbabilityComponentDescription {
            name: "market_midpoint",
            inputs: "Polymarket executable bid and ask midpoint",
            theory: "prediction-market consensus prior before independent evidence",
            authority: "market prior only; Polymarket CLOB owns executable price and depth",
        },
        ProbabilityComponentDescription {
            name: "chainlink_digital",
            inputs: "fresh Chainlink current price, arrival-timestamped pre-open Chainlink reference, remaining time, and horizon volatility",
            theory: "cash-or-nothing endpoint probability that the Chainlink settlement price finishes above the opening Chainlink reference",
            authority: "official settlement-source probability; Chainlink alone defines the opening reference and settlement truth",
        },
        ProbabilityComponentDescription {
            name: "distance_lob_vol",
            inputs: "Binance CEX spot distance to the contract price-to-beat (with Chainlink reconstruction fallback), Binance L2 imbalance and depth, Binance agg-trade flow, and realized volatility",
            theory: "independent Binance microstructure-conditioned endpoint probability proxy",
            authority: "predictive context only; never a settlement oracle or execution venue",
        },
        ProbabilityComponentDescription {
            name: "event_surface",
            inputs: "train-only asset, time-to-expiry, and distance buckets",
            theory: "empirical endpoint frequency surface estimated without test labels",
            authority: "research prior only",
        },
        ProbabilityComponentDescription {
            name: "existing_model",
            inputs: "Binance CEX spot, contract price-to-beat (with Chainlink reconstruction fallback), remaining time, and horizon volatility",
            theory: "Binance log-moneyness scaled by horizon volatility endpoint proxy",
            authority: "predictive context only; never a settlement oracle or execution venue",
        },
    ]
}

/// One source of truth for both the prompt and provider-side structured output.
pub fn prediction_proposal_json_schema() -> serde_json::Value {
    let non_negative_number = || {
        serde_json::json!({
            "type": "number",
            "minimum": 0
        })
    };
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["probability_blends"],
        "properties": {
            "probability_blends": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "name",
                        "hypothesis",
                        "market_midpoint_weight",
                        "chainlink_digital_weight",
                        "distance_lob_vol_weight",
                        "event_surface_weight",
                        "existing_model_weight"
                    ],
                    "properties": {
                        "name": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 80,
                            "pattern": "^[A-Za-z0-9_-]+$"
                        },
                        "hypothesis": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 500
                        },
                        "market_midpoint_weight": non_negative_number(),
                        "chainlink_digital_weight": non_negative_number(),
                        "distance_lob_vol_weight": non_negative_number(),
                        "event_surface_weight": non_negative_number(),
                        "existing_model_weight": non_negative_number()
                    }
                }
            }
        }
    })
}

pub fn build_prediction_prompt(
    mission: &PredictionResearchMission,
    remaining_candidates: usize,
    prior_outcomes: &[serde_json::Value],
) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "task": format!(
            "Propose up to {remaining_candidates} typed probability blends for this governed prediction-market mission. Change only probability_blend_weights. Every blend must state one falsifiable hypothesis and use finite non-negative weights with a positive total. Do not change labels, gates, costs, settlement rules, or execution settings."
        ),
        "target": mission.target,
        "mission": mission,
        "registered_probability_components": registered_probability_components(),
        "prior_candidate_outcomes": prior_outcomes,
        "response_json_schema": prediction_proposal_json_schema()
    }))
    .expect("prediction prompt is serializable")
}

fn prediction_policy_sources() -> [(&'static str, &'static [u8]); 19] {
    [
        ("Cargo.lock", include_bytes!("../../../Cargo.lock")),
        ("Cargo.toml", include_bytes!("../../../Cargo.toml")),
        (
            "crates/ploy-research/Cargo.toml",
            include_bytes!("../Cargo.toml"),
        ),
        (
            "crates/ploy-research/src/autofactor.rs",
            include_bytes!("autofactor.rs"),
        ),
        (
            "crates/ploy-research/src/factors.rs",
            include_bytes!("factors.rs"),
        ),
        (
            "crates/ploy-research/src/factors_v2.rs",
            include_bytes!("factors_v2.rs"),
        ),
        ("crates/ploy-research/src/lib.rs", include_bytes!("lib.rs")),
        (
            "crates/ploy-research/src/model/supervised/burn_binary.rs",
            include_bytes!("model/supervised/burn_binary.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_loop.rs",
            include_bytes!("prediction_loop.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_loop_fs.rs",
            include_bytes!("prediction_loop_fs.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_llm.rs",
            include_bytes!("prediction_llm.rs"),
        ),
        (
            "crates/ploy-research/src/research_snapshot.rs",
            include_bytes!("research_snapshot.rs"),
        ),
        (
            "crates/ploy-research/examples/factor_walk_forward_v2.rs",
            include_bytes!("../examples/factor_walk_forward_v2.rs"),
        ),
        (
            "crates/ploy-research/examples/prediction_research_loop.rs",
            include_bytes!("../examples/prediction_research_loop.rs"),
        ),
        (
            "crates/ploy-feed-loaders/src/database.rs",
            include_bytes!("../../ploy-feed-loaders/src/database.rs"),
        ),
        (
            "crates/ploy-market-contracts/src/lib.rs",
            include_bytes!("../../ploy-market-contracts/src/lib.rs"),
        ),
        (
            "crates/ploy-market-contracts/src/events.rs",
            include_bytes!("../../ploy-market-contracts/src/events.rs"),
        ),
        (
            "crates/ploy-market-contracts/src/fees.rs",
            include_bytes!("../../ploy-market-contracts/src/fees.rs"),
        ),
        (
            "crates/ploy-market-data/src/feeds.rs",
            include_bytes!("../../ploy-market-data/src/feeds.rs"),
        ),
    ]
}

fn prediction_policy_snapshot_id(sources: &[(&str, &[u8])]) -> String {
    let mut digest = Sha256::new();
    for (path, body) in sources {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(*body);
        digest.update([0]);
    }
    format!("sha256:{:x}", digest.finalize())
}

pub fn current_prediction_policy_snapshot_id() -> String {
    prediction_policy_snapshot_id(&prediction_policy_sources())
}

pub fn research_brief_snapshot_id(mission: &PredictionResearchMission) -> String {
    let mut brief = BTreeMap::new();
    brief.insert("hypothesis_scope", mission.hypothesis_scope.as_str());
    brief.insert("objective", mission.objective.as_str());
    let body = serde_json::to_vec(&brief).expect("research brief is serializable");
    format!("sha256:{:x}", Sha256::digest(body))
}

pub fn validate_prediction_mission(
    mission: &PredictionResearchMission,
    current_policy_snapshot_id: &str,
) -> Result<(), String> {
    if mission.schema_version != PREDICTION_MISSION_SCHEMA_VERSION {
        return Err(format!(
            "mission.schema_version must be {PREDICTION_MISSION_SCHEMA_VERSION}"
        ));
    }
    if mission.lane != "prediction_market" {
        return Err("mission.lane must be prediction_market".to_string());
    }
    require_non_empty(&mission.mission_id, "mission.mission_id")?;
    require_non_empty(&mission.objective, "mission.objective")?;
    require_non_empty(&mission.hypothesis_scope, "mission.hypothesis_scope")?;
    if mission.target != PREDICTION_LOOP_TARGET {
        return Err(format!("mission.target must be {PREDICTION_LOOP_TARGET}"));
    }
    if mission.horizon != "5m" {
        return Err("mission.horizon must be 5m".to_string());
    }
    if mission.symbols.len() != 1 || !matches!(mission.symbols[0].as_str(), "BTC" | "SOL") {
        return Err(
            "mission.symbols must contain exactly one isolated BTC or SOL symbol".to_string(),
        );
    }
    if mission.mutable_scope != ["probability_blend_weights"] {
        return Err("prediction mission may mutate only probability_blend_weights".to_string());
    }
    for (value, field) in [
        (&mission.data_snapshot_id, "mission.data_snapshot_id"),
        (&mission.prompt_snapshot_id, "mission.prompt_snapshot_id"),
        (
            &mission.search_policy_snapshot_id,
            "mission.search_policy_snapshot_id",
        ),
    ] {
        validate_sha256_id(value, field)?;
    }
    let expected_brief = research_brief_snapshot_id(mission);
    if mission.prompt_snapshot_id != expected_brief {
        return Err(format!(
            "mission.prompt_snapshot_id does not content-address objective and hypothesis_scope; expected {expected_brief}"
        ));
    }
    if mission.search_policy_snapshot_id != current_policy_snapshot_id {
        return Err(format!(
            "mission.search_policy_snapshot_id does not match the current Rust proposer/evaluator policy; expected {current_policy_snapshot_id}"
        ));
    }
    if mission.search_budget.max_candidates == 0
        || mission.search_budget.max_llm_calls == 0
        || mission.search_budget.max_seconds == 0
    {
        return Err("mission.search_budget values must be positive".to_string());
    }
    if mission.search_budget.max_candidates > MAX_GOVERNED_CANDIDATES
        || mission.search_budget.max_llm_calls > MAX_GOVERNED_LLM_CALLS
        || mission.search_budget.max_seconds > MAX_GOVERNED_SECONDS
    {
        return Err(format!(
            "mission.search_budget exceeds governed maxima: candidates<={MAX_GOVERNED_CANDIDATES}, calls<={MAX_GOVERNED_LLM_CALLS}, seconds<={MAX_GOVERNED_SECONDS}"
        ));
    }
    Ok(())
}

pub fn validate_prediction_proposal(
    proposal: PredictionProposal,
    remaining_candidates: usize,
) -> Result<Vec<LlmProbabilityBlendSpec>, String> {
    if proposal.probability_blends.is_empty() {
        return Err("proposal must contain at least one probability blend".to_string());
    }
    if proposal.probability_blends.len() > remaining_candidates {
        return Err(format!(
            "proposal exceeds remaining candidate budget: {} > {remaining_candidates}",
            proposal.probability_blends.len()
        ));
    }
    let mut names = BTreeSet::new();
    let mut blends = Vec::with_capacity(proposal.probability_blends.len());
    for blend in proposal.probability_blends {
        if blend.name.is_empty()
            || blend.name.len() > 80
            || !blend.name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err(
                "probability blend name must be a safe 1..80 character identifier".to_string(),
            );
        }
        if !names.insert(blend.name.clone()) {
            return Err("probability blend names must be unique".to_string());
        }
        let hypothesis = blend.hypothesis.trim().to_string();
        if hypothesis.is_empty() || hypothesis.chars().count() > 500 {
            return Err("probability blend hypothesis must contain 1..500 characters".to_string());
        }
        let weights = [
            blend.market_midpoint_weight,
            blend.chainlink_digital_weight,
            blend.distance_lob_vol_weight,
            blend.event_surface_weight,
            blend.existing_model_weight,
        ];
        if weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight < 0.0)
        {
            return Err("probability blend weights must be finite and non-negative".to_string());
        }
        let total = weights.iter().sum::<f64>();
        if !total.is_finite() || total <= PROBABILITY_WEIGHT_EPSILON {
            return Err("probability blend weights must have a positive finite total".to_string());
        }
        let mut blend = blend;
        blend.hypothesis = hypothesis;
        blends.push(blend.into());
    }
    Ok(blends)
}

fn require_non_empty(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value {
        Err(format!("{field} must be a trimmed non-empty string"))
    } else {
        Ok(())
    }
}

fn validate_sha256_id(value: &str, field: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(format!("{field} must use sha256:<64 lowercase hex>"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{field} must use sha256:<64 lowercase hex>"));
    }
    Ok(())
}

/// Enforce that a settlement mission contains both the Chainlink authority
/// surface and the Binance CEX context consumed by the Rust components.
///
/// Binance is an independent predictor/repricing clock only.  This contract
/// does not grant it settlement-oracle or execution authority.
pub fn validate_prediction_snapshot_sources(
    manifest: &ResearchSnapshotManifest,
    observations: &[crate::FactorObservation],
) -> Result<(), String> {
    validate_governed_chainlink_5m_settlement_evidence(manifest, observations)
        .map_err(|error| format!("prediction snapshot oracle evidence is invalid: {error:#}"))?;
    if manifest.data_audit_status.as_deref() != Some("ok") {
        return Err(
            "prediction snapshot data_audit_status must be ok before Binance factors run"
                .to_string(),
        );
    }
    let requirements = manifest
        .data_requirements
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if !requirements.contains("all") {
        let missing = REQUIRED_BINANCE_DATA_REQUIREMENTS
            .iter()
            .copied()
            .filter(|required| !requirements.contains(required))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "prediction snapshot data audit omitted required Binance sources: {}",
                missing.join(",")
            ));
        }
    }
    for required in REQUIRED_BINANCE_SOURCE_SURFACES {
        let surface = manifest
            .source_surfaces
            .iter()
            .find(|surface| surface.name == required)
            .ok_or_else(|| {
                format!("prediction snapshot is missing required Binance surface {required}")
            })?;
        if surface.gate_category != "required_for_prediction"
            || !surface.snapshot_sampled
            || surface.row_count.unwrap_or_default() == 0
        {
            return Err(format!(
                "required Binance surface {required} must be sampled, prediction-gated, and non-empty"
            ));
        }
    }
    let chainlink = manifest
        .source_surfaces
        .iter()
        .find(|surface| surface.name == REQUIRED_CHAINLINK_SOURCE_SURFACE)
        .ok_or_else(|| {
            format!(
                "prediction snapshot is missing settlement-authority surface {REQUIRED_CHAINLINK_SOURCE_SURFACE}"
            )
        })?;
    if chainlink.gate_category != "required_for_prediction"
        || !chainlink.snapshot_sampled
        || chainlink.row_count.unwrap_or_default() == 0
    {
        return Err(format!(
            "settlement-authority surface {REQUIRED_CHAINLINK_SOURCE_SURFACE} must be sampled, prediction-gated, and non-empty"
        ));
    }
    let polymarket = manifest
        .source_surfaces
        .iter()
        .find(|surface| surface.name == REQUIRED_POLYMARKET_SOURCE_SURFACE)
        .ok_or_else(|| {
            format!(
                "prediction snapshot is missing executable CLOB surface {REQUIRED_POLYMARKET_SOURCE_SURFACE}"
            )
        })?;
    if polymarket.gate_category != "required_for_execution"
        || !polymarket.snapshot_sampled
        || polymarket.row_count.unwrap_or_default() == 0
    {
        return Err(format!(
            "executable CLOB surface {REQUIRED_POLYMARKET_SOURCE_SURFACE} must be sampled, execution-gated, and non-empty"
        ));
    }
    if manifest.row_counts.observations == 0 || manifest.row_counts.pm_book_snapshots == 0 {
        return Err(
            "prediction snapshot must contain factor observations and Polymarket book snapshots"
                .to_string(),
        );
    }
    let blocking_quality_flags = manifest
        .quality_flags
        .iter()
        .filter(|flag| {
            matches!(
                flag.as_str(),
                "no_factor_observations"
                    | "no_chainlink_reference_ticks"
                    | "no_binance_price_ticks"
                    | "no_binance_agg_trade_ticks"
                    | "no_binance_lob_snapshots"
                    | "no_pm_book_snapshots"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if !blocking_quality_flags.is_empty() {
        return Err(format!(
            "prediction snapshot has blocking quality flags: {}",
            blocking_quality_flags.join(",")
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalCallOutput {
    /// The assistant's JSON object, before it is trusted or deserialized.
    pub raw_response: String,
    pub provider: String,
    pub model: String,
    pub usage: serde_json::Value,
}

pub trait ProposalClient {
    /// Perform exactly one provider call. Retries belong to the durable loop ledger.
    fn propose(&mut self, prompt: &str, timeout: Duration) -> Result<ProposalCallOutput, String>;
}

fn validate_proposal_call_output(output: &ProposalCallOutput) -> Result<(), String> {
    if output.raw_response.trim().is_empty()
        || output.provider.trim().is_empty()
        || output.model.trim().is_empty()
    {
        return Err("LLM response evidence requires response, provider, and model".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PredictionEvaluationRequest {
    pub mission: PredictionResearchMission,
    pub snapshot_dir: PathBuf,
    /// A private, single-evaluation staging directory. Evaluators may write
    /// scratch output here, but the LoopRun imports only the typed return value
    /// into its descriptor-anchored output root. This path is deleted after
    /// `evaluate` returns and is never durable evidence.
    pub artifact_dir: PathBuf,
    pub prior: Option<LlmPriorSpec>,
    /// A read-only staged copy of the governed content-addressed prior. This is
    /// deliberately outside the LoopRun output path so replacing that path
    /// cannot redirect the evaluator to a different prior.
    pub prior_artifact_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PredictionEvaluationOutput {
    pub success: bool,
    pub feedback: Option<PredictionResearchFeedback>,
    pub stdout: String,
    pub stderr: String,
    pub failure_reason: Option<String>,
}

pub trait PredictionEvaluator {
    fn evaluate(
        &mut self,
        request: &PredictionEvaluationRequest,
        timeout: Duration,
    ) -> PredictionEvaluationOutput;
}

/// Ephemeral evaluator filesystem capability.
///
/// External evaluators cannot consume `OutputRoot`'s in-process directory FD,
/// so each call receives an independently-created private staging directory.
/// The held descriptors let the LoopRun reject a staging path replacement
/// before importing any returned evidence.
#[derive(Debug)]
struct EvaluatorStaging {
    path: PathBuf,
    root_fd: OwnedFd,
    artifact_dir: PathBuf,
    artifact_fd: OwnedFd,
}

#[derive(Debug)]
struct OutputPathIdentity {
    path: PathBuf,
    fd: OwnedFd,
}

impl OutputPathIdentity {
    fn capture(output_root: &OutputRoot) -> Result<Self, String> {
        let path = output_root.path(Path::new(""))?;
        let fd = rustix::fs::open(
            &path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| {
            format!(
                "open prediction output path identity without following symlinks {}: {error}",
                path.display()
            )
        })?;
        Ok(Self { path, fd })
    }

    fn validate_after_external_evaluation(&self) -> Result<(), String> {
        validate_directory_path_identity(&self.path, &self.fd, "prediction output root path")
            .map_err(|reason| {
                format!(
                    "prediction output root path changed during external evaluation; evaluator output was discarded and the original directory must be restored before resume: {reason}"
                )
            })
    }
}

impl EvaluatorStaging {
    fn create() -> Result<Self, String> {
        let temporary_root = std::env::temp_dir();
        for _ in 0..128 {
            let candidate = temporary_root.join(format!(
                "ploy-prediction-evaluator-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "create private evaluator staging directory {}: {error}",
                        candidate.display()
                    ));
                }
            }

            let path = fs::canonicalize(&candidate).map_err(|error| {
                format!(
                    "canonicalize evaluator staging directory {}: {error}",
                    candidate.display()
                )
            })?;
            let root_fd = rustix::fs::open(
                &path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| {
                format!(
                    "open private evaluator staging directory without following symlinks {}: {error}",
                    path.display()
                )
            })?;
            rustix::fs::mkdirat(&root_fd, "artifacts", Mode::RWXU).map_err(|error| {
                format!(
                    "create evaluator artifact staging directory under {}: {error}",
                    path.display()
                )
            })?;
            let artifact_fd = rustix::fs::openat(
                &root_fd,
                "artifacts",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| {
                format!(
                    "open evaluator artifact staging directory without following symlinks under {}: {error}",
                    path.display()
                )
            })?;
            rustix::fs::fsync(&root_fd)
                .map_err(|error| format!("sync evaluator staging directory: {error}"))?;
            let artifact_dir = path.join("artifacts");
            return Ok(Self {
                path,
                root_fd,
                artifact_dir,
                artifact_fd,
            });
        }
        Err("exhausted unique evaluator staging directory names".to_string())
    }

    fn materialize_prior(
        &self,
        prior: &LlmPriorSpec,
        artifact: &ArtifactRef,
        verified_path: &Path,
    ) -> Result<PathBuf, String> {
        let body = canonical_json_bytes(prior)?;
        if sha256_hex(&body) != artifact.sha256 {
            return Err("staged evaluator prior differs from governed prior evidence".to_string());
        }
        let referenced_file_name = Path::new(&artifact.path)
            .file_name()
            .ok_or_else(|| "governed evaluator prior has no filename".to_string())?;
        let verified_file_name = verified_path
            .file_name()
            .ok_or_else(|| "verified evaluator prior has no filename".to_string())?;
        if referenced_file_name != verified_file_name {
            return Err(
                "verified evaluator prior filename differs from its evidence ref".to_string(),
            );
        }
        let file_name = verified_file_name
            .to_str()
            .ok_or_else(|| "governed evaluator prior has no UTF-8 filename".to_string())?;
        let expected_suffix = format!("-{}.json", artifact.sha256);
        if !file_name.starts_with("prediction-prior-") || !file_name.ends_with(&expected_suffix) {
            return Err("governed evaluator prior filename is not content-addressed".to_string());
        }

        rustix::fs::mkdirat(&self.root_fd, "inputs", Mode::RWXU).map_err(|error| {
            format!(
                "create evaluator input staging directory under {}: {error}",
                self.path.display()
            )
        })?;
        let input_fd = rustix::fs::openat(
            &self.root_fd,
            "inputs",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| format!("open evaluator input staging directory: {error}"))?;
        let prior_fd = rustix::fs::openat(
            &input_fd,
            file_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| format!("create staged evaluator prior {file_name}: {error}"))?;
        let mut prior_file = File::from(prior_fd);
        prior_file
            .write_all(&body)
            .map_err(|error| format!("write staged evaluator prior {file_name}: {error}"))?;
        rustix::fs::fchmod(&prior_file, Mode::RUSR).map_err(|error| {
            format!("make staged evaluator prior read-only {file_name}: {error}")
        })?;
        prior_file
            .sync_all()
            .map_err(|error| format!("sync staged evaluator prior {file_name}: {error}"))?;
        drop(prior_file);
        rustix::fs::fsync(&input_fd)
            .map_err(|error| format!("sync evaluator input staging directory: {error}"))?;
        Ok(self.path.join("inputs").join(file_name))
    }

    fn validate_unchanged(&self) -> Result<(), String> {
        validate_directory_path_identity(&self.path, &self.root_fd, "evaluator staging root")?;
        validate_directory_path_identity(
            &self.artifact_dir,
            &self.artifact_fd,
            "evaluator staging artifact directory",
        )
    }
}

impl Drop for EvaluatorStaging {
    fn drop(&mut self) {
        // Never recursively delete a replacement path. A hostile replacement
        // is left for explicit operator cleanup instead of being followed.
        if self.validate_unchanged().is_ok() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn validate_directory_path_identity(
    path: &Path,
    expected: &OwnedFd,
    label: &str,
) -> Result<(), String> {
    let observed = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "{label} disappeared or was redirected at {}: {error}",
            path.display()
        )
    })?;
    let expected_stat =
        rustix::fs::fstat(expected).map_err(|error| format!("inspect held {label}: {error}"))?;
    let observed_stat = rustix::fs::fstat(&observed)
        .map_err(|error| format!("inspect path for {label}: {error}"))?;
    if expected_stat.st_dev != observed_stat.st_dev || expected_stat.st_ino != observed_stat.st_ino
    {
        return Err(format!(
            "{label} was replaced during evaluation at {}",
            path.display()
        ));
    }
    Ok(())
}

fn evaluate_in_staging<E: PredictionEvaluator>(
    mission: &PredictionResearchMission,
    snapshot_dir: &Path,
    prior: Option<(&LlmPriorSpec, &ArtifactRef, &Path)>,
    evaluator: &mut E,
    timeout: Duration,
) -> Result<PredictionEvaluationOutput, String> {
    let staging = EvaluatorStaging::create()?;
    let (prior, prior_artifact_path) = match prior {
        Some((prior, artifact, verified_path)) => (
            Some(prior.clone()),
            Some(staging.materialize_prior(prior, artifact, verified_path)?),
        ),
        None => (None, None),
    };
    let request = PredictionEvaluationRequest {
        mission: mission.clone(),
        snapshot_dir: snapshot_dir.to_path_buf(),
        artifact_dir: staging.artifact_dir.clone(),
        prior,
        prior_artifact_path,
    };
    let output = evaluator.evaluate(&request, timeout);
    if let Err(reason) = staging.validate_unchanged() {
        return Ok(PredictionEvaluationOutput {
            success: false,
            feedback: None,
            stdout: String::new(),
            stderr: String::new(),
            failure_reason: Some(reason),
        });
    }
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopRunStatus {
    Running,
    Paused,
    Provisional,
    BudgetExhausted,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoopRunSummary {
    pub mission_id: String,
    pub status: LoopRunStatus,
    pub llm_calls_used: usize,
    pub candidates_evaluated: usize,
    pub iterations_completed: usize,
    pub provisional_models: Vec<String>,
    pub reason: Option<String>,
    pub state_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RunIdentity {
    mission_id: String,
    mission_hash: String,
    symbol: String,
    data_snapshot_id: String,
    snapshot_hash: String,
    prompt_snapshot_id: String,
    search_policy_snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LlmAttemptStatus {
    Started,
    Responded,
    Accepted,
    Invalid,
    ProviderFailed,
    Abandoned,
    Late,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LlmAttemptRecord {
    call_index: usize,
    prompt: ArtifactRef,
    response: Option<ArtifactRef>,
    provider: Option<String>,
    model: Option<String>,
    usage: Option<serde_json::Value>,
    status: LlmAttemptStatus,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EvaluationEvidence {
    stdout: ArtifactRef,
    stderr: ArtifactRef,
    feedback: Option<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchPassEvidence {
    schema_version: String,
    identity: RunIdentity,
    iteration: usize,
    call_index: usize,
    prior_sha256: String,
    feedback_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CandidateOutcome {
    model: String,
    verdict: String,
    reason_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IterationRecord {
    iteration: usize,
    call_index: usize,
    prior: ArtifactRef,
    evaluation: EvaluationEvidence,
    outcomes: Vec<CandidateOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingIteration {
    iteration: usize,
    call_index: usize,
    prior: ArtifactRef,
    candidate_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FailureRecord {
    stage: String,
    reason: String,
    stdout: Option<ArtifactRef>,
    stderr: Option<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TerminalOutcome {
    status: LoopRunStatusWire,
    reason: String,
    provisional_models: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LoopRunStatusWire {
    Provisional,
    BudgetExhausted,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
enum Frontier {
    Baseline,
    Propose,
    Evaluate { pending: PendingIteration },
    Done { outcome: TerminalOutcome },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredictionLoopState {
    schema_version: String,
    identity: RunIdentity,
    elapsed_millis: u64,
    baseline: Option<EvaluationEvidence>,
    llm_attempts: Vec<LlmAttemptRecord>,
    iterations: Vec<IterationRecord>,
    failures: Vec<FailureRecord>,
    frontier: Frontier,
    pause: Option<FailureRecord>,
}

impl PredictionLoopState {
    fn calls_used(&self) -> usize {
        self.llm_attempts.len()
    }

    fn candidates_evaluated(&self) -> usize {
        self.iterations
            .iter()
            .map(|iteration| iteration.outcomes.len())
            .sum()
    }
}

/// Run a bounded adaptive search until a provisional pass, budget exhaustion,
/// or a retryable pause. A provisional pass is not promotion evidence and must
/// be confirmed on a separate sealed snapshot. Calling this again with the
/// same inputs resumes from the durable frontier; an evaluator pause never
/// spends another LLM call.
pub fn run_or_resume<C: ProposalClient, E: PredictionEvaluator>(
    mission: PredictionResearchMission,
    snapshot_dir: &Path,
    output_dir: &Path,
    client: &mut C,
    evaluator: &mut E,
) -> Result<LoopRunSummary, String> {
    let output_root = OutputRoot::open(output_dir)?;
    let output_path_identity = OutputPathIdentity::capture(&output_root)?;
    let _lock = OutputLock::acquire(&output_root)?;
    let state_relative = Path::new("prediction-loop-state.json");
    let state_path = output_root.path(state_relative)?;
    reject_unbound_output_directory(&output_root, state_relative)?;

    let current_policy = current_prediction_policy_snapshot_id();
    validate_prediction_mission(&mission, &current_policy)?;
    let snapshot = load_research_snapshot(snapshot_dir)
        .map_err(|error| format!("load governed research snapshot: {error:#}"))?;
    validate_prediction_snapshot_sources(&snapshot.manifest, &snapshot.observations)?;
    validate_mission_snapshot_binding(&mission, &snapshot)?;

    let identity = run_identity(&mission, &snapshot.manifest)?;
    let mut state = if output_root.entry_exists(state_relative)? {
        let state: PredictionLoopState = read_json(&output_root, state_relative)?;
        if state.schema_version != PREDICTION_LOOP_STATE_SCHEMA_VERSION {
            return Err(format!(
                "loop state schema mismatch: expected {PREDICTION_LOOP_STATE_SCHEMA_VERSION}, got {}",
                state.schema_version
            ));
        }
        if state.identity != identity {
            return Err(
                "output directory belongs to a different mission, policy, or snapshot".to_string(),
            );
        }
        verify_state(&state, &mission, &output_root)?;
        state
    } else {
        PredictionLoopState {
            schema_version: PREDICTION_LOOP_STATE_SCHEMA_VERSION.to_string(),
            identity,
            elapsed_millis: 0,
            baseline: None,
            llm_attempts: Vec::new(),
            iterations: Vec::new(),
            failures: Vec::new(),
            frontier: Frontier::Baseline,
            pause: None,
        }
    };

    let base_elapsed_millis = state.elapsed_millis;
    let run_started = Instant::now();
    let recovered_response = recover_interrupted_llm_attempt(&mut state, &output_root)?;
    let recovered_search_pass = reconcile_search_pass_evidence(&mut state, &mission, &output_root)?;
    let cleared_pause = state.pause.take().is_some();
    verify_state(&state, &mission, &output_root)?;
    verify_terminal_evidence(&state, &mission, &output_root)?;
    if recovered_response || recovered_search_pass || cleared_pause {
        checkpoint(
            &mut state,
            &output_root,
            state_relative,
            base_elapsed_millis,
            run_started,
        )?;
    }

    loop {
        if let Some(summary) = budget_terminal_if_exhausted(
            &mission,
            &mut state,
            &output_root,
            state_relative,
            &state_path,
            base_elapsed_millis,
            run_started,
        )? {
            return Ok(summary);
        }

        match state.frontier.clone() {
            Frontier::Baseline => {
                let attempt_dir = next_attempt_dir(&output_root, Path::new("baseline"))?;
                let output = evaluate_in_staging(
                    &mission,
                    snapshot_dir,
                    None,
                    evaluator,
                    remaining_time(&mission, &state, base_elapsed_millis, run_started),
                )?;
                output_path_identity.validate_after_external_evaluation()?;
                if output.feedback.is_some() {
                    return Err("baseline evaluator may not emit candidate feedback".to_string());
                }
                let evidence = persist_evaluation_output(
                    &output_root,
                    &attempt_dir,
                    output.feedback.as_ref(),
                    &output.stdout,
                    &output.stderr,
                )?;
                if !output.success {
                    let failure = FailureRecord {
                        stage: "baseline".to_string(),
                        reason: output
                            .failure_reason
                            .unwrap_or_else(|| "baseline evaluator failed".to_string()),
                        stdout: Some(evidence.stdout),
                        stderr: Some(evidence.stderr),
                    };
                    state.failures.push(failure.clone());
                    state.pause = Some(failure);
                    checkpoint(
                        &mut state,
                        &output_root,
                        state_relative,
                        base_elapsed_millis,
                        run_started,
                    )?;
                    return summary_from_state(&mission, &state, &state_path, &output_root);
                }
                state.baseline = Some(evidence);
                state.frontier = Frontier::Propose;
                checkpoint(
                    &mut state,
                    &output_root,
                    state_relative,
                    base_elapsed_millis,
                    run_started,
                )?;
            }
            Frontier::Propose => {
                if !has_unprocessed_response(&state) {
                    let call_index = state.calls_used() + 1;
                    let attempt_dir =
                        PathBuf::from("llm-calls").join(format!("call-{call_index:03}"));
                    let remaining_candidates = mission
                        .search_budget
                        .max_candidates
                        .saturating_sub(state.candidates_evaluated());
                    let prompt = build_prediction_prompt(
                        &mission,
                        remaining_candidates,
                        &qualitative_prior_outcomes(&state),
                    );
                    let prompt_ref = write_content_addressed_text(
                        &output_root,
                        &attempt_dir,
                        "llm-prompt",
                        &prompt,
                    )?;
                    state.llm_attempts.push(LlmAttemptRecord {
                        call_index,
                        prompt: prompt_ref,
                        response: None,
                        provider: None,
                        model: None,
                        usage: None,
                        status: LlmAttemptStatus::Started,
                        error: None,
                    });
                    // The call budget is consumed durably before any network I/O.
                    checkpoint(
                        &mut state,
                        &output_root,
                        state_relative,
                        base_elapsed_millis,
                        run_started,
                    )?;
                    let timeout =
                        remaining_time(&mission, &state, base_elapsed_millis, run_started);
                    match client.propose(&prompt, timeout) {
                        Ok(response) => {
                            validate_proposal_call_output(&response)?;
                            let response_ref = write_content_addressed_json(
                                &output_root,
                                &attempt_dir,
                                "llm-response",
                                &response,
                            )?;
                            let attempt = state
                                .llm_attempts
                                .last_mut()
                                .expect("attempt was just recorded");
                            attempt.response = Some(response_ref);
                            attempt.provider = Some(response.provider);
                            attempt.model = Some(response.model);
                            attempt.usage = Some(response.usage);
                            attempt.status = LlmAttemptStatus::Responded;
                            checkpoint(
                                &mut state,
                                &output_root,
                                state_relative,
                                base_elapsed_millis,
                                run_started,
                            )?;
                            // Re-enter through the budget gate before trusting
                            // a response delivered at the mission deadline.
                            continue;
                        }
                        Err(reason) => {
                            let failure = FailureRecord {
                                stage: "propose".to_string(),
                                reason: reason.clone(),
                                stdout: None,
                                stderr: None,
                            };
                            let attempt = state
                                .llm_attempts
                                .last_mut()
                                .expect("attempt was just recorded");
                            attempt.status = LlmAttemptStatus::ProviderFailed;
                            attempt.error = Some(reason);
                            state.failures.push(failure.clone());
                            state.pause = Some(failure);
                            checkpoint(
                                &mut state,
                                &output_root,
                                state_relative,
                                base_elapsed_millis,
                                run_started,
                            )?;
                            return summary_from_state(&mission, &state, &state_path, &output_root);
                        }
                    }
                }

                let attempt_index = state
                    .llm_attempts
                    .iter()
                    .rposition(|attempt| attempt.status == LlmAttemptStatus::Responded)
                    .ok_or_else(|| {
                        "propose frontier has no unprocessed LLM response".to_string()
                    })?;
                let response_ref = state.llm_attempts[attempt_index]
                    .response
                    .clone()
                    .ok_or_else(|| {
                        "responded LLM attempt is missing response evidence".to_string()
                    })?;
                let response_artifact = verify_artifact(&output_root, &response_ref)?;
                let response: ProposalCallOutput = response_artifact.parse_json()?;
                validate_proposal_call_output(&response)?;
                let remaining_candidates = mission
                    .search_budget
                    .max_candidates
                    .saturating_sub(state.candidates_evaluated());
                let blends = serde_json::from_str::<PredictionProposal>(&response.raw_response)
                    .map_err(|error| format!("strict proposal JSON rejected: {error}"))
                    .and_then(|proposal| {
                        validate_prediction_proposal(proposal, remaining_candidates)
                    });
                let blends = match blends {
                    Ok(blends) => blends,
                    Err(reason) => {
                        let attempt = &mut state.llm_attempts[attempt_index];
                        attempt.status = LlmAttemptStatus::Invalid;
                        attempt.error = Some(reason);
                        checkpoint(
                            &mut state,
                            &output_root,
                            state_relative,
                            base_elapsed_millis,
                            run_started,
                        )?;
                        continue;
                    }
                };
                let prior = mission_prior(&mission, blends);
                validate_prediction_research_prior(&prior).map_err(|reason| {
                    format!("Rust evaluator rejected proposal prior: {reason}")
                })?;
                let iteration = state.iterations.len() + 1;
                let iteration_dir =
                    PathBuf::from("iterations").join(format!("iteration-{iteration:03}"));
                let prior_ref = write_content_addressed_json(
                    &output_root,
                    &iteration_dir,
                    "prediction-prior",
                    &prior,
                )?;
                let call_index = state.llm_attempts[attempt_index].call_index;
                state.llm_attempts[attempt_index].status = LlmAttemptStatus::Accepted;
                state.frontier = Frontier::Evaluate {
                    pending: PendingIteration {
                        iteration,
                        call_index,
                        prior: prior_ref,
                        candidate_count: prior.probability_blends.len(),
                    },
                };
                checkpoint(
                    &mut state,
                    &output_root,
                    state_relative,
                    base_elapsed_millis,
                    run_started,
                )?;
            }
            Frontier::Evaluate { pending } => {
                let prior_artifact = verify_artifact(&output_root, &pending.prior)?;
                let prior: LlmPriorSpec = prior_artifact.parse_json()?;
                validate_pending_prior(&mission, &pending, &prior)?;
                let evaluator_parent = PathBuf::from("iterations")
                    .join(format!("iteration-{:03}", pending.iteration))
                    .join("evaluator-attempts");
                let attempt_dir = next_attempt_dir(&output_root, &evaluator_parent)?;
                let output = evaluate_in_staging(
                    &mission,
                    snapshot_dir,
                    Some((&prior, &pending.prior, prior_artifact.path())),
                    evaluator,
                    remaining_time(&mission, &state, base_elapsed_millis, run_started),
                )?;
                output_path_identity.validate_after_external_evaluation()?;
                if !output.success && output.feedback.is_some() {
                    return Err(
                        "failed evaluator attempt may not emit candidate feedback".to_string()
                    );
                }
                let evidence = persist_evaluation_output(
                    &output_root,
                    &attempt_dir,
                    output.feedback.as_ref(),
                    &output.stdout,
                    &output.stderr,
                )?;
                if !output.success {
                    let failure = FailureRecord {
                        stage: "evaluate".to_string(),
                        reason: output
                            .failure_reason
                            .unwrap_or_else(|| "prediction evaluator failed".to_string()),
                        stdout: Some(evidence.stdout),
                        stderr: Some(evidence.stderr),
                    };
                    state.failures.push(failure.clone());
                    state.pause = Some(failure);
                    checkpoint(
                        &mut state,
                        &output_root,
                        state_relative,
                        base_elapsed_millis,
                        run_started,
                    )?;
                    return summary_from_state(&mission, &state, &state_path, &output_root);
                }
                let feedback = output.feedback.ok_or_else(|| {
                    "successful candidate evaluator emitted no deterministic feedback".to_string()
                })?;
                let outcomes = match validate_feedback(&mission, &prior, &feedback) {
                    Ok(outcomes) => outcomes,
                    Err(reason) => {
                        state.frontier = Frontier::Done {
                            outcome: TerminalOutcome {
                                status: LoopRunStatusWire::Failed,
                                reason: format!("invalid evaluator feedback: {reason}"),
                                provisional_models: Vec::new(),
                            },
                        };
                        checkpoint(
                            &mut state,
                            &output_root,
                            state_relative,
                            base_elapsed_millis,
                            run_started,
                        )?;
                        return summary_from_state(&mission, &state, &state_path, &output_root);
                    }
                };
                let provisional_models = outcomes
                    .iter()
                    .filter(|outcome| outcome.verdict == "search_pass")
                    .map(|outcome| outcome.model.clone())
                    .collect::<Vec<_>>();
                let iteration_record = IterationRecord {
                    iteration: pending.iteration,
                    call_index: pending.call_index,
                    prior: pending.prior.clone(),
                    evaluation: evidence.clone(),
                    outcomes,
                };
                if !provisional_models.is_empty() {
                    persist_search_pass_evidence(
                        &output_root,
                        SearchPassEvidence {
                            schema_version: SEARCH_PASS_EVIDENCE_SCHEMA_VERSION.to_string(),
                            identity: state.identity.clone(),
                            iteration: pending.iteration,
                            call_index: pending.call_index,
                            prior_sha256: pending.prior.sha256.clone(),
                            feedback_sha256: evidence
                                .feedback
                                .as_ref()
                                .expect("successful evaluation has feedback evidence")
                                .sha256
                                .clone(),
                        },
                    )?;
                }
                state.iterations.push(iteration_record);
                state.frontier = if provisional_models.is_empty() {
                    Frontier::Propose
                } else {
                    Frontier::Done {
                        outcome: TerminalOutcome {
                            status: LoopRunStatusWire::Provisional,
                            reason: "adaptive search passed; separate sealed holdout required"
                                .to_string(),
                            provisional_models,
                        },
                    }
                };
                checkpoint(
                    &mut state,
                    &output_root,
                    state_relative,
                    base_elapsed_millis,
                    run_started,
                )?;
            }
            Frontier::Done { .. } => {
                checkpoint(
                    &mut state,
                    &output_root,
                    state_relative,
                    base_elapsed_millis,
                    run_started,
                )?;
                return summary_from_state(&mission, &state, &state_path, &output_root);
            }
        }
    }
}

fn reject_unbound_output_directory(
    output_root: &OutputRoot,
    state_relative: &Path,
) -> Result<(), String> {
    if output_root.entry_exists(state_relative)? {
        return Ok(());
    }
    let unexpected = output_root
        .list_directory(Path::new(""))?
        .unwrap_or_default()
        .into_iter()
        .filter(|name| name != ".prediction-research-loop.lock")
        .collect::<Vec<_>>();
    if unexpected.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "refusing to bind a new LoopRun to non-empty output directory {}; first unexpected path is {}",
            output_root.path(Path::new(""))?.display(),
            unexpected[0]
        ))
    }
}

fn run_identity(
    mission: &PredictionResearchMission,
    manifest: &ResearchSnapshotManifest,
) -> Result<RunIdentity, String> {
    let mission_hash = format!("sha256:{}", sha256_hex(&canonical_json_bytes(mission)?));
    Ok(RunIdentity {
        mission_id: mission.mission_id.clone(),
        mission_hash,
        symbol: mission.symbols[0].clone(),
        data_snapshot_id: mission.data_snapshot_id.clone(),
        snapshot_hash: manifest
            .snapshot_hash
            .clone()
            .ok_or_else(|| "loaded snapshot is missing snapshot_hash".to_string())?,
        prompt_snapshot_id: mission.prompt_snapshot_id.clone(),
        search_policy_snapshot_id: mission.search_policy_snapshot_id.clone(),
    })
}

fn validate_mission_snapshot_binding(
    mission: &PredictionResearchMission,
    snapshot: &ResearchSnapshot,
) -> Result<(), String> {
    let manifest = &snapshot.manifest;
    let contract_hash = manifest
        .snapshot_contract_hash
        .as_deref()
        .ok_or_else(|| "loaded snapshot is missing snapshot_contract_hash".to_string())?;
    if mission.data_snapshot_id != contract_hash {
        return Err(format!(
            "mission.data_snapshot_id {} does not match loaded snapshot contract {}",
            mission.data_snapshot_id, contract_hash
        ));
    }
    if !manifest.immutable_input || !manifest.require_official_settlement {
        return Err(
            "prediction LoopRun requires immutable input and official settlement labels"
                .to_string(),
        );
    }
    let requested = mission.symbols[0].as_str();
    let snapshot_underlyings = manifest
        .symbols
        .iter()
        .map(|symbol| normalized_underlying_symbol(symbol))
        .collect::<BTreeSet<_>>();
    if snapshot_underlyings != BTreeSet::from([requested.to_string()]) {
        return Err(format!(
            "prediction snapshot must contain only mission underlying {requested}; found {:?}",
            snapshot_underlyings
        ));
    }
    if manifest.row_counts.observations != snapshot.observations.len()
        || manifest.row_counts.deribit_snapshots != snapshot.deribit_snapshots.len()
        || manifest.row_counts.pm_book_snapshots != snapshot.pm_book_snapshots.len()
    {
        return Err(
            "snapshot manifest row counts do not match hashed evaluator artifacts".to_string(),
        );
    }
    if snapshot.observations.is_empty()
        || snapshot.pm_book_snapshots.is_empty()
        || snapshot.observations.iter().any(|row| {
            normalized_underlying_symbol(&row.symbol) != requested
                || row.event_window_secs != PREDICTION_EVENT_WINDOW_SECS
        })
    {
        return Err(format!(
            "snapshot observations must be non-empty, five-minute, and isolated to {requested}"
        ));
    }
    if snapshot
        .deribit_snapshots
        .iter()
        .any(|row| normalized_underlying_symbol(&row.symbol) != requested)
    {
        return Err(format!(
            "snapshot Deribit rows must be isolated to {requested}"
        ));
    }
    let mission_events = snapshot
        .observations
        .iter()
        .map(|row| row.event_id.as_str())
        .collect::<BTreeSet<_>>();
    if snapshot
        .pm_book_snapshots
        .iter()
        .any(|row| !mission_events.contains(row.event_id.as_str()))
    {
        return Err(format!(
            "snapshot contains Polymarket book rows outside the {requested} mission event set"
        ));
    }
    Ok(())
}

fn checkpoint(
    state: &mut PredictionLoopState,
    output_root: &OutputRoot,
    state_relative: &Path,
    base_elapsed_millis: u64,
    run_started: Instant,
) -> Result<(), String> {
    state.elapsed_millis = base_elapsed_millis
        .saturating_add(run_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
    atomic_write_json(output_root, state_relative, state)
}

fn remaining_time(
    mission: &PredictionResearchMission,
    state: &PredictionLoopState,
    base_elapsed_millis: u64,
    run_started: Instant,
) -> Duration {
    let used = base_elapsed_millis
        .saturating_add(run_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
    let limit = mission.search_budget.max_seconds.saturating_mul(1_000);
    Duration::from_millis(limit.saturating_sub(used).max(1))
        .min(Duration::from_secs(
            mission.search_budget.max_seconds.max(1),
        ))
        .min(Duration::from_millis(
            limit.saturating_sub(state.elapsed_millis).max(1),
        ))
}

fn budget_terminal_if_exhausted(
    mission: &PredictionResearchMission,
    state: &mut PredictionLoopState,
    output_root: &OutputRoot,
    state_relative: &Path,
    state_path: &Path,
    base_elapsed_millis: u64,
    run_started: Instant,
) -> Result<Option<LoopRunSummary>, String> {
    if matches!(state.frontier, Frontier::Done { .. }) {
        return Ok(None);
    }
    let elapsed = base_elapsed_millis
        .saturating_add(run_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
    let time_exhausted = elapsed >= mission.search_budget.max_seconds.saturating_mul(1_000);
    let at_proposal_boundary =
        matches!(state.frontier, Frontier::Propose) && !has_unprocessed_response(state);
    let call_exhausted =
        at_proposal_boundary && state.calls_used() >= mission.search_budget.max_llm_calls;
    let candidate_exhausted = at_proposal_boundary
        && state.candidates_evaluated() >= mission.search_budget.max_candidates;
    let reason = if time_exhausted {
        Some("mission wall-clock budget exhausted")
    } else if call_exhausted {
        Some("mission LLM-call budget exhausted")
    } else if candidate_exhausted {
        Some("mission candidate budget exhausted")
    } else {
        None
    };
    let Some(reason) = reason else {
        return Ok(None);
    };
    if time_exhausted {
        if let Some(attempt) = state
            .llm_attempts
            .iter_mut()
            .find(|attempt| attempt.status == LlmAttemptStatus::Responded)
        {
            attempt.status = LlmAttemptStatus::Late;
            attempt.error = Some("response arrived after mission wall-clock budget".to_string());
        }
    }
    state.frontier = Frontier::Done {
        outcome: TerminalOutcome {
            status: LoopRunStatusWire::BudgetExhausted,
            reason: reason.to_string(),
            provisional_models: Vec::new(),
        },
    };
    checkpoint(
        state,
        output_root,
        state_relative,
        base_elapsed_millis,
        run_started,
    )?;
    Ok(Some(summary_from_state(
        mission,
        state,
        state_path,
        output_root,
    )?))
}

fn summary_from_state(
    mission: &PredictionResearchMission,
    state: &PredictionLoopState,
    state_path: &Path,
    output_root: &OutputRoot,
) -> Result<LoopRunSummary, String> {
    verify_terminal_evidence(state, mission, output_root)?;
    let provisional_models = final_provisional_models(state, mission, output_root)?;
    let (status, reason) = if let Some(pause) = state.pause.as_ref() {
        (LoopRunStatus::Paused, Some(pause.reason.clone()))
    } else {
        match &state.frontier {
            Frontier::Done { outcome } => (
                match outcome.status {
                    LoopRunStatusWire::Provisional => LoopRunStatus::Provisional,
                    LoopRunStatusWire::BudgetExhausted => LoopRunStatus::BudgetExhausted,
                    LoopRunStatusWire::Failed => LoopRunStatus::Failed,
                },
                Some(outcome.reason.clone()),
            ),
            _ => (LoopRunStatus::Running, None),
        }
    };
    Ok(LoopRunSummary {
        mission_id: mission.mission_id.clone(),
        status,
        llm_calls_used: state.calls_used(),
        candidates_evaluated: state.candidates_evaluated(),
        iterations_completed: state.iterations.len(),
        provisional_models,
        reason,
        state_path: state_path.to_path_buf(),
    })
}

fn qualitative_prior_outcomes(state: &PredictionLoopState) -> Vec<serde_json::Value> {
    let outcomes = state
        .iterations
        .iter()
        .flat_map(|iteration| {
            iteration.outcomes.iter().map(move |outcome| {
                serde_json::json!({
                    "iteration": iteration.iteration,
                    "model": outcome.model,
                    "verdict": outcome.verdict,
                    "reason_codes": outcome.reason_codes,
                })
            })
        })
        .collect::<Vec<_>>();
    outcomes
        .into_iter()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn mission_prior(
    mission: &PredictionResearchMission,
    probability_blends: Vec<LlmProbabilityBlendSpec>,
) -> LlmPriorSpec {
    LlmPriorSpec {
        target: Some(mission.target.clone()),
        mission_id: Some(mission.mission_id.clone()),
        data_snapshot_id: Some(mission.data_snapshot_id.clone()),
        prompt_snapshot_id: Some(mission.prompt_snapshot_id.clone()),
        search_policy_snapshot_id: Some(mission.search_policy_snapshot_id.clone()),
        symbols: mission.symbols.clone(),
        horizon: Some(mission.horizon.clone()),
        mutations: Vec::new(),
        probability_blends,
        runtime_avoid_factors: Vec::new(),
        structural_avoid_signatures: Vec::new(),
    }
}

fn validate_pending_prior(
    mission: &PredictionResearchMission,
    pending: &PendingIteration,
    prior: &LlmPriorSpec,
) -> Result<(), String> {
    validate_prediction_research_prior(prior)
        .map_err(|reason| format!("pending prior is invalid: {reason}"))?;
    let expected = mission_prior(mission, prior.probability_blends.clone());
    if canonical_json_bytes(prior)? != canonical_json_bytes(&expected)? {
        return Err("pending prior provenance does not exactly match mission".to_string());
    }
    if prior.probability_blends.len() != pending.candidate_count {
        return Err("pending prior candidate count does not match durable frontier".to_string());
    }
    Ok(())
}

fn validate_feedback(
    mission: &PredictionResearchMission,
    prior: &LlmPriorSpec,
    feedback: &PredictionResearchFeedback,
) -> Result<Vec<CandidateOutcome>, String> {
    if feedback.schema_version != PREDICTION_FEEDBACK_SCHEMA_VERSION
        || feedback.mission_id != mission.mission_id
        || feedback.target != mission.target
        || feedback.symbols != mission.symbols
        || feedback.horizon != mission.horizon
        || feedback.data_snapshot_id != mission.data_snapshot_id
        || feedback.prompt_snapshot_id != mission.prompt_snapshot_id
        || feedback.search_policy_snapshot_id != mission.search_policy_snapshot_id
    {
        return Err("feedback provenance does not exactly match mission and prior".to_string());
    }
    if feedback.gate_policy != PredictionResearchGatePolicy::governed() {
        return Err("feedback gate policy does not match the governed Rust policy".to_string());
    }
    let expected = prior
        .probability_blends
        .iter()
        .map(|blend| (format!("q_llm_{}", blend.name), blend))
        .collect::<BTreeMap<_, _>>();
    if feedback.candidates.len() != expected.len() {
        return Err("feedback candidate set size differs from pending prior".to_string());
    }
    let mut seen = BTreeSet::new();
    let mut outcomes = Vec::with_capacity(feedback.candidates.len());
    for candidate in &feedback.candidates {
        let blend = expected
            .get(&candidate.model)
            .ok_or_else(|| format!("feedback returned unknown model {}", candidate.model))?;
        if !seen.insert(candidate.model.as_str()) {
            return Err(format!("feedback duplicated model {}", candidate.model));
        }
        if candidate.hypothesis != blend.hypothesis || &candidate.probability_blend != *blend {
            return Err(format!(
                "feedback changed hypothesis or blend for {}",
                candidate.model
            ));
        }
        let (verdict, reason_codes) =
            recompute_prediction_research_outcome(&candidate.metrics, &feedback.gate_policy)?;
        if candidate.verdict != verdict || candidate.reason_codes != reason_codes {
            return Err(format!(
                "feedback verdict for {} does not match independently recomputed metrics",
                candidate.model
            ));
        }
        outcomes.push(CandidateOutcome {
            model: candidate.model.clone(),
            verdict,
            reason_codes,
        });
    }
    Ok(outcomes)
}

fn persist_evaluation_output(
    output_root: &OutputRoot,
    attempt_dir: &Path,
    feedback: Option<&PredictionResearchFeedback>,
    stdout: &str,
    stderr: &str,
) -> Result<EvaluationEvidence, String> {
    Ok(EvaluationEvidence {
        stdout: write_content_addressed_text(output_root, attempt_dir, "evaluator-stdout", stdout)?,
        stderr: write_content_addressed_text(output_root, attempt_dir, "evaluator-stderr", stderr)?,
        feedback: feedback
            .map(|feedback| {
                write_content_addressed_json(
                    output_root,
                    attempt_dir,
                    "prediction-research-feedback",
                    feedback,
                )
            })
            .transpose()?,
    })
}

fn persist_search_pass_evidence(
    output_root: &OutputRoot,
    evidence: SearchPassEvidence,
) -> Result<ArtifactRef, String> {
    if let Some((existing_ref, existing)) = read_search_pass_evidence(output_root)? {
        if canonical_json_bytes(&existing)? == canonical_json_bytes(&evidence)? {
            return Ok(existing_ref);
        }
        return Err(
            "prediction loop already has different append-only search-pass evidence".to_string(),
        );
    }
    write_content_addressed_json(
        output_root,
        Path::new("search-pass-evidence"),
        SEARCH_PASS_EVIDENCE_PREFIX,
        &evidence,
    )
}

fn read_search_pass_evidence(
    output_root: &OutputRoot,
) -> Result<Option<(ArtifactRef, SearchPassEvidence)>, String> {
    let directory = Path::new("search-pass-evidence");
    let matches = output_root
        .list_directory(directory)?
        .unwrap_or_default()
        .into_iter()
        .filter(|name| {
            name.starts_with(&format!("{SEARCH_PASS_EVIDENCE_PREFIX}-")) && name.ends_with(".json")
        })
        .collect::<Vec<_>>();
    let file_name = match matches.as_slice() {
        [] => return Ok(None),
        [file_name] => file_name,
        _ => {
            return Err(format!(
                "expected at most one append-only search-pass record under {}, found {}",
                output_root.path(directory)?.display(),
                matches.len()
            ))
        }
    };
    let digest = file_name
        .strip_prefix(&format!("{SEARCH_PASS_EVIDENCE_PREFIX}-"))
        .and_then(|name| name.strip_suffix(".json"))
        .ok_or_else(|| format!("invalid search-pass filename {file_name}"))?;
    let artifact = ArtifactRef {
        path: directory.join(file_name).to_string_lossy().into_owned(),
        sha256: digest.to_string(),
    };
    let verified = verify_artifact(output_root, &artifact)?;
    let evidence = verified.parse_json()?;
    Ok(Some((artifact, evidence)))
}

fn validated_search_pass_iteration(
    state: &PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &OutputRoot,
    search_pass: &SearchPassEvidence,
) -> Result<(IterationRecord, Vec<String>), String> {
    if search_pass.schema_version != SEARCH_PASS_EVIDENCE_SCHEMA_VERSION
        || search_pass.identity != state.identity
        || search_pass.iteration == 0
        || search_pass.call_index == 0
    {
        return Err("append-only search-pass identity is invalid".to_string());
    }
    let attempt = state
        .llm_attempts
        .get(search_pass.call_index - 1)
        .ok_or_else(|| "search-pass evidence references unknown LLM call".to_string())?;
    if attempt.status != LlmAttemptStatus::Accepted {
        return Err("search-pass evidence references a non-accepted LLM call".to_string());
    }
    let iteration = state
        .iterations
        .get(search_pass.iteration - 1)
        .ok_or_else(|| "search-pass evidence is not bound to a durable iteration".to_string())?;
    let feedback_ref = iteration
        .evaluation
        .feedback
        .as_ref()
        .ok_or_else(|| "search-pass iteration has no evaluator feedback".to_string())?;
    if iteration.iteration != search_pass.iteration
        || iteration.call_index != search_pass.call_index
        || iteration.prior.sha256 != search_pass.prior_sha256
        || feedback_ref.sha256 != search_pass.feedback_sha256
    {
        return Err(
            "append-only search-pass evidence differs from durable iteration hashes".to_string(),
        );
    }
    let prior_artifact = verify_artifact(output_root, &iteration.prior)?;
    let prior: LlmPriorSpec = prior_artifact.parse_json()?;
    let pending = PendingIteration {
        iteration: search_pass.iteration,
        call_index: search_pass.call_index,
        prior: iteration.prior.clone(),
        candidate_count: prior.probability_blends.len(),
    };
    validate_pending_prior(mission, &pending, &prior)?;
    verify_evaluation_evidence(output_root, &iteration.evaluation)?;
    let feedback_artifact = verify_artifact(output_root, feedback_ref)?;
    let feedback: PredictionResearchFeedback = feedback_artifact.parse_json()?;
    let outcomes = validate_feedback(mission, &prior, &feedback)?;
    if canonical_json_bytes(&iteration.outcomes)? != canonical_json_bytes(&outcomes)? {
        return Err("durable search-pass outcomes differ from evaluator feedback".to_string());
    }
    let provisional_models = outcomes
        .iter()
        .filter(|outcome| outcome.verdict == "search_pass")
        .map(|outcome| outcome.model.clone())
        .collect::<Vec<_>>();
    if provisional_models.is_empty() {
        return Err(
            "append-only search-pass evidence contains no recomputed candidate".to_string(),
        );
    }
    Ok((iteration.clone(), provisional_models))
}

fn reconcile_search_pass_evidence(
    state: &mut PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &OutputRoot,
) -> Result<bool, String> {
    let Some((_, search_pass)) = read_search_pass_evidence(output_root)? else {
        return Ok(false);
    };
    if state.iterations.len() < search_pass.iteration {
        // A record not yet bound into durable state may be an orphan.
        // The evaluate frontier reruns deterministically and must reproduce it.
        return Ok(false);
    }
    let (iteration, provisional_models) =
        validated_search_pass_iteration(state, mission, output_root, &search_pass)?;
    let mut changed = false;
    if state.iterations.len() == iteration.iteration {
        let existing = state
            .iterations
            .last()
            .ok_or_else(|| "search-pass iteration is missing from loop state".to_string())?;
        if canonical_json_bytes(existing)? != canonical_json_bytes(&iteration)? {
            return Err(
                "loop state search-pass iteration differs from append-only evidence".to_string(),
            );
        }
    } else {
        return Err("append-only search-pass evidence is not terminal".to_string());
    }
    let desired = Frontier::Done {
        outcome: TerminalOutcome {
            status: LoopRunStatusWire::Provisional,
            reason: "adaptive search passed; separate sealed holdout required".to_string(),
            provisional_models,
        },
    };
    if canonical_json_bytes(&state.frontier)? != canonical_json_bytes(&desired)? {
        state.frontier = desired;
        changed = true;
    }
    Ok(changed)
}

fn final_provisional_models(
    state: &PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &OutputRoot,
) -> Result<Vec<String>, String> {
    let Some((_, search_pass)) = read_search_pass_evidence(output_root)? else {
        return Ok(Vec::new());
    };
    if state.iterations.len() < search_pass.iteration {
        return Ok(Vec::new());
    }
    validated_search_pass_iteration(state, mission, output_root, &search_pass)
        .map(|(_, provisional_models)| provisional_models)
}

fn verify_terminal_evidence(
    state: &PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &OutputRoot,
) -> Result<(), String> {
    let recomputed = final_provisional_models(state, mission, output_root)?;
    match &state.frontier {
        Frontier::Done { outcome } if outcome.status == LoopRunStatusWire::Provisional => {
            if recomputed.is_empty() || outcome.provisional_models != recomputed {
                return Err(
                    "mutable terminal state differs from append-only search-pass evidence"
                        .to_string(),
                );
            }
        }
        _ if !recomputed.is_empty() => {
            return Err("append-only search-pass evidence disagrees with loop frontier".to_string())
        }
        _ => {}
    }
    Ok(())
}

fn has_unprocessed_response(state: &PredictionLoopState) -> bool {
    state
        .llm_attempts
        .iter()
        .any(|attempt| attempt.status == LlmAttemptStatus::Responded)
}

fn recover_interrupted_llm_attempt(
    state: &mut PredictionLoopState,
    output_root: &OutputRoot,
) -> Result<bool, String> {
    let started = state
        .llm_attempts
        .iter()
        .enumerate()
        .filter(|(_, attempt)| attempt.status == LlmAttemptStatus::Started)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if started.len() > 1 {
        return Err("loop state contains multiple in-flight LLM calls".to_string());
    }
    let Some(index) = started.into_iter().next() else {
        return Ok(false);
    };
    verify_artifact(output_root, &state.llm_attempts[index].prompt)?;
    state.llm_attempts[index].status = LlmAttemptStatus::Abandoned;
    state.llm_attempts[index].error = Some(
        "process stopped after durable call reservation; any uncheckpointed response was ignored"
            .to_string(),
    );
    Ok(true)
}

fn verify_evaluation_evidence(
    output_root: &OutputRoot,
    evidence: &EvaluationEvidence,
) -> Result<(), String> {
    verify_artifact(output_root, &evidence.stdout)?;
    verify_artifact(output_root, &evidence.stderr)?;
    if let Some(feedback) = &evidence.feedback {
        verify_artifact(output_root, feedback)?;
    }
    Ok(())
}

fn verify_state(
    state: &PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &OutputRoot,
) -> Result<(), String> {
    if let Some(baseline) = &state.baseline {
        verify_evaluation_evidence(output_root, baseline)?;
        if baseline.feedback.is_some() {
            return Err("baseline state unexpectedly references candidate feedback".to_string());
        }
    }
    let responded = state
        .llm_attempts
        .iter()
        .filter(|attempt| attempt.status == LlmAttemptStatus::Responded)
        .count();
    if responded > 1 {
        return Err("state has more than one unprocessed LLM response".to_string());
    }
    for (index, attempt) in state.llm_attempts.iter().enumerate() {
        if attempt.call_index != index + 1 {
            return Err("LLM call lineage is not contiguous".to_string());
        }
        verify_artifact(output_root, &attempt.prompt)?;
        if let Some(response_ref) = &attempt.response {
            let response_artifact = verify_artifact(output_root, response_ref)?;
            let response: ProposalCallOutput = response_artifact.parse_json()?;
            validate_proposal_call_output(&response)?;
            if attempt.provider.as_deref() != Some(response.provider.as_str())
                || attempt.model.as_deref() != Some(response.model.as_str())
                || attempt.usage.as_ref() != Some(&response.usage)
            {
                return Err(format!(
                    "LLM call {} provenance differs from its append-only response envelope",
                    attempt.call_index
                ));
            }
        } else if attempt.provider.is_some() || attempt.model.is_some() || attempt.usage.is_some() {
            return Err(format!(
                "LLM call {} has provenance without response evidence",
                attempt.call_index
            ));
        }
        let needs_response = matches!(
            attempt.status,
            LlmAttemptStatus::Responded
                | LlmAttemptStatus::Accepted
                | LlmAttemptStatus::Invalid
                | LlmAttemptStatus::Late
        );
        if needs_response != attempt.response.is_some() {
            return Err(format!(
                "LLM call {} response evidence disagrees with status",
                attempt.call_index
            ));
        }
    }
    for (index, iteration) in state.iterations.iter().enumerate() {
        if iteration.iteration != index + 1 {
            return Err("iteration lineage is not contiguous".to_string());
        }
        let attempt = state
            .llm_attempts
            .get(iteration.call_index.saturating_sub(1))
            .ok_or_else(|| "iteration references unknown LLM call".to_string())?;
        if attempt.status != LlmAttemptStatus::Accepted {
            return Err("iteration references a non-accepted LLM call".to_string());
        }
        let prior_artifact = verify_artifact(output_root, &iteration.prior)?;
        let prior: LlmPriorSpec = prior_artifact.parse_json()?;
        let pending = PendingIteration {
            iteration: iteration.iteration,
            call_index: iteration.call_index,
            prior: iteration.prior.clone(),
            candidate_count: iteration.outcomes.len(),
        };
        validate_pending_prior(mission, &pending, &prior)?;
        verify_evaluation_evidence(output_root, &iteration.evaluation)?;
        let feedback_ref = iteration
            .evaluation
            .feedback
            .as_ref()
            .ok_or_else(|| "completed iteration is missing feedback evidence".to_string())?;
        let feedback_artifact = verify_artifact(output_root, feedback_ref)?;
        let feedback: PredictionResearchFeedback = feedback_artifact.parse_json()?;
        let expected_outcomes = validate_feedback(mission, &prior, &feedback)?;
        if canonical_json_bytes(&iteration.outcomes)? != canonical_json_bytes(&expected_outcomes)? {
            return Err("durable candidate outcomes differ from evaluator feedback".to_string());
        }
    }
    if let Frontier::Evaluate { pending } = &state.frontier {
        if pending.iteration != state.iterations.len() + 1 {
            return Err("pending iteration is not the next contiguous iteration".to_string());
        }
        let prior_artifact = verify_artifact(output_root, &pending.prior)?;
        let prior: LlmPriorSpec = prior_artifact.parse_json()?;
        validate_pending_prior(mission, pending, &prior)?;
    }
    for failure in &state.failures {
        if let Some(stdout) = &failure.stdout {
            verify_artifact(output_root, stdout)?;
        }
        if let Some(stderr) = &failure.stderr {
            verify_artifact(output_root, stderr)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research_snapshot::{
        write_research_snapshot, ResearchSnapshot, ResearchSnapshotArtifacts,
        ResearchSnapshotInputArtifact, ResearchSnapshotPhaseTiming, ResearchSnapshotPmBookSource,
        ResearchSnapshotRowCounts, ResearchSnapshotSourceSurface,
    };
    use chrono::Utc;
    use std::collections::VecDeque;

    fn mission() -> PredictionResearchMission {
        let mut mission = PredictionResearchMission {
            schema_version: PREDICTION_MISSION_SCHEMA_VERSION.to_string(),
            mission_id: "polymarket-btc-5m-v1".to_string(),
            lane: "prediction_market".to_string(),
            objective: "Estimate official BTC five-minute settlement probability.".to_string(),
            hypothesis_scope: "Test Chainlink settlement probability with independent Binance spot, agg-trade, and L2 context.".to_string(),
            mutable_scope: vec!["probability_blend_weights".to_string()],
            data_snapshot_id: format!("sha256:{}", "1".repeat(64)),
            target: PREDICTION_LOOP_TARGET.to_string(),
            symbols: vec!["BTC".to_string()],
            horizon: "5m".to_string(),
            prompt_snapshot_id: String::new(),
            search_policy_snapshot_id: format!("sha256:{}", "2".repeat(64)),
            search_budget: PredictionSearchBudget {
                max_candidates: 6,
                max_llm_calls: 2,
                max_seconds: 900,
            },
        };
        mission.prompt_snapshot_id = research_brief_snapshot_id(&mission);
        mission
    }

    fn proposal() -> PredictionProposal {
        PredictionProposal {
            probability_blends: vec![ProposedProbabilityBlend {
                name: "binance_context".to_string(),
                hypothesis: "Binance spot, flow, and L2 improve held-out calibration beyond the Polymarket midpoint.".to_string(),
                market_midpoint_weight: 0.4,
                chainlink_digital_weight: 0.2,
                distance_lob_vol_weight: 0.2,
                event_surface_weight: 0.1,
                existing_model_weight: 0.1,
            }],
        }
    }

    fn snapshot_manifest() -> ResearchSnapshotManifest {
        let now = Utc::now();
        let event_start = now - chrono::Duration::days(2);
        let event_end = event_start + chrono::Duration::seconds(PREDICTION_EVENT_WINDOW_SECS);
        ResearchSnapshotManifest {
            schema_version: crate::RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: Some("snapshot".to_string()),
            snapshot_contract_hash: Some(format!("sha256:{}", "1".repeat(64))),
            generated_at: now,
            git_sha: None,
            symbols: vec!["BTCUSDT".to_string()],
            start: now - chrono::Duration::days(2),
            end: now,
            history_start: now - chrono::Duration::days(4),
            lob_sample_secs: 1,
            pm_book_sample_secs: Some(1),
            observation_sample_secs: 1,
            max_quote_age_secs: 30,
            stake_usd: 15.0,
            require_official_settlement: true,
            chainlink_oracle_settlement_audit: Some(crate::ChainlinkOracleSettlementAudit {
                expected_events: 1,
                accepted_events: 1,
                ..Default::default()
            }),
            chainlink_oracle_settlement_evidence: vec![crate::ChainlinkOracleSettlementEvidence {
                event_id: "event-1".to_string(),
                symbol: "BTC".to_string(),
                policy_version: crate::GOVERNED_CHAINLINK_BOUNDARY_POLICY_VERSION.to_string(),
                start_time: event_start,
                end_time: event_end,
                open: Some(crate::ChainlinkOracleBoundaryEvidence {
                    boundary_ts: event_start,
                    price: 100.0,
                    source_ts: event_start,
                    received_at: event_start,
                    confirmation_source_ts: event_start,
                    confirmation_received_at: event_start,
                }),
                close: Some(crate::ChainlinkOracleBoundaryEvidence {
                    boundary_ts: event_end,
                    price: 101.0,
                    source_ts: event_end,
                    received_at: event_end,
                    confirmation_source_ts: event_end,
                    confirmation_received_at: event_end,
                }),
                chainlink_outcome_up: Some(true),
                official_outcome_up: Some(true),
                reasons: Vec::new(),
            }],
            immutable_input: true,
            source_kind: "unit_test".to_string(),
            optimizer_data_dir: Some("unit-test-immutable-source".to_string()),
            source_surfaces: REQUIRED_BINANCE_SOURCE_SURFACES
                .iter()
                .copied()
                .chain([
                    REQUIRED_CHAINLINK_SOURCE_SURFACE,
                    REQUIRED_POLYMARKET_SOURCE_SURFACE,
                ])
                .map(|name| ResearchSnapshotSourceSurface {
                    name: name.to_string(),
                    role: match name {
                        REQUIRED_CHAINLINK_SOURCE_SURFACE => {
                            "opening_reference_and_settlement_authority".to_string()
                        }
                        REQUIRED_POLYMARKET_SOURCE_SURFACE => "execution_depth_context".to_string(),
                        _ => "binance_prediction_context".to_string(),
                    },
                    gate_category: if name == REQUIRED_POLYMARKET_SOURCE_SURFACE {
                        "required_for_execution".to_string()
                    } else {
                        "required_for_prediction".to_string()
                    },
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(1),
                    row_count: Some(10),
                    notes: "point-in-time governed test surface".to_string(),
                })
                .collect(),
            input_artifacts: vec![ResearchSnapshotInputArtifact {
                name: "unit".to_string(),
                path: "unit".to_string(),
                content_hash: None,
                row_count: Some(30),
            }],
            data_requirements: REQUIRED_BINANCE_DATA_REQUIREMENTS
                .iter()
                .map(ToString::to_string)
                .collect(),
            data_audit_status: Some("ok".to_string()),
            data_audit_report: Some("audit.json".to_string()),
            include_deribit: false,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts: ResearchSnapshotRowCounts {
                observations: 1,
                deribit_snapshots: 0,
                pm_book_snapshots: 1,
            },
            phase_timings: vec![ResearchSnapshotPhaseTiming {
                phase: "unit".to_string(),
                elapsed_ms: 1,
                rows: Some(30),
            }],
            quality_flags: vec![],
            pm_book_source: ResearchSnapshotPmBookSource::default(),
        }
    }

    fn test_prediction_observation(tick_ts: chrono::DateTime<Utc>) -> crate::FactorObservation {
        crate::FactorObservation {
            event_id: "event-1".to_string(),
            symbol: "BTCUSDT".to_string(),
            tick_ts,
            event_window_secs: 300,
            time_remaining_secs: 60,
            signed_distance_to_beat: 0.0,
            abs_distance_to_beat: 0.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            flip_age_secs: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 0.01,
            fair_prob_up: 0.5,
            fair_prob_up_clean: 0.5,
            prob_disagreement: 0.0,
            implied_sigma_horizon: 0.01,
            vol_gap: 0.0,
            distance_over_sigma: 0.0,
            model_prob_up: 0.5,
            chainlink_prob_up: 0.5,
            model_edge_up: 0.0,
            reward_risk_up: 1.0,
            reward_risk_down: 1.0,
            obi: 0.0,
            spread_bps: 1.0,
            microprice_offset_bps: 0.0,
            bid_depth_near: 10.0,
            ask_depth_near: 10.0,
            depth_ratio: 1.0,
            depth_imbalance: 0.0,
            depth_far_ratio: 1.0,
            depth_acceleration: 0.0,
            obi_10: 0.0,
            pm_up_bid: 0.49,
            pm_up_ask: 0.51,
            pm_up_bid_size: 10.0,
            pm_up_ask_size: 10.0,
            pm_down_bid: 0.49,
            pm_down_ask: 0.51,
            pm_down_bid_size: 10.0,
            pm_down_ask_size: 10.0,
            pm_lag_secs: 0.0,
            settlement_up: 1.0,
            future_up_ask_change_30s: None,
            future_up_ask_change_60s: None,
            cum_obi_delta_5m: 0.0,
            cum_depth_delta_5m: 0.0,
            cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0,
            cex_bar_return_30s: 0.0,
            cex_bar_return_60s: 0.0,
            cex_bar_volume_ratio_30s: 0.0,
            cex_bar_volume_trend_3: 0.0,
            cex_signed_volume_ratio_30s: 0.0,
            cex_consecutive_up_bars: 0.0,
            cex_consecutive_down_bars: 0.0,
            cex_breakout_volume_score: 0.0,
        }
    }

    #[test]
    fn mission_accepts_only_isolated_binary_probability_authority() {
        let mission = mission();
        validate_prediction_mission(&mission, &mission.search_policy_snapshot_id)
            .expect("valid prediction mission");

        let mut mixed = mission.clone();
        mixed.symbols.push("SOL".to_string());
        assert!(
            validate_prediction_mission(&mixed, &mission.search_policy_snapshot_id)
                .expect_err("mixed symbol mission must fail")
                .contains("exactly one")
        );

        let mut formula = mission.clone();
        formula.mutable_scope.push("factor_formula".to_string());
        assert!(
            validate_prediction_mission(&formula, &mission.search_policy_snapshot_id)
                .expect_err("formula authority must fail")
                .contains("probability_blend_weights")
        );
    }

    #[test]
    fn prediction_policy_snapshot_binds_market_fee_implementation() {
        let sources = prediction_policy_sources();
        let without_fees = sources
            .into_iter()
            .filter(|(path, _)| *path != "crates/ploy-market-contracts/src/fees.rs")
            .collect::<Vec<_>>();
        assert_eq!(without_fees.len() + 1, sources.len());
        assert_ne!(
            prediction_policy_snapshot_id(&without_fees),
            current_prediction_policy_snapshot_id()
        );
    }

    #[test]
    fn checked_in_btc_and_sol_templates_pin_current_brief_and_rust_policy() {
        for raw in [
            include_str!("../../../config/research_missions/polymarket-btc-5m.example.json"),
            include_str!("../../../config/research_missions/polymarket-sol-5m.example.json"),
        ] {
            let mission: PredictionResearchMission =
                serde_json::from_str(raw).expect("parse checked-in mission template");
            assert_eq!(
                mission.prompt_snapshot_id,
                research_brief_snapshot_id(&mission)
            );
            assert_eq!(
                mission.search_policy_snapshot_id,
                current_prediction_policy_snapshot_id()
            );
        }
    }

    #[test]
    fn proposal_is_typed_bounded_and_schema_has_no_factor_mutation_surface() {
        let blends = validate_prediction_proposal(proposal(), 2).expect("valid proposal");
        assert_eq!(blends.len(), 1);
        assert_eq!(blends[0].name, "binance_context");

        let raw = serde_json::json!({
            "mutations": [{"anything": true}],
            "probability_blends": proposal().probability_blends,
        });
        assert!(serde_json::from_value::<PredictionProposal>(raw)
            .expect_err("mutation field must not exist in prediction schema")
            .to_string()
            .contains("unknown field"));

        let mut negative = proposal();
        negative.probability_blends[0].existing_model_weight = -0.1;
        assert!(validate_prediction_proposal(negative, 2)
            .expect_err("negative weight must fail")
            .contains("non-negative"));

        assert!(validate_prediction_proposal(proposal(), 0)
            .expect_err("candidate budget must fail")
            .contains("remaining candidate budget"));
    }

    #[test]
    fn prompt_exposes_binance_as_predictor_and_chainlink_as_settlement_source() {
        let prompt = build_prediction_prompt(&mission(), 3, &[]);
        let payload: serde_json::Value = serde_json::from_str(&prompt).expect("prompt JSON");
        let components = payload["registered_probability_components"]
            .as_array()
            .expect("component list");
        let by_name = components
            .iter()
            .map(|component| (component["name"].as_str().expect("name"), component))
            .collect::<BTreeMap<_, _>>();

        assert!(by_name["chainlink_digital"]["authority"]
            .as_str()
            .expect("authority")
            .contains("settlement"));
        for component in ["distance_lob_vol", "existing_model"] {
            assert!(by_name[component]["inputs"]
                .as_str()
                .expect("inputs")
                .contains("Binance"));
            assert!(by_name[component]["authority"]
                .as_str()
                .expect("authority")
                .contains("never a settlement oracle"));
        }
        assert!(!prompt.contains("raw labels"));
        assert!(!prompt.contains("gate thresholds"));
    }

    #[test]
    fn prediction_snapshot_fails_closed_without_audited_binance_inputs() {
        let manifest = snapshot_manifest();
        let observations = vec![test_prediction_observation(
            manifest.start + chrono::Duration::minutes(1),
        )];
        validate_prediction_snapshot_sources(&manifest, &observations)
            .expect("complete prediction-source contract");

        let mut missing_oracle_audit = manifest.clone();
        missing_oracle_audit.chainlink_oracle_settlement_audit = None;
        assert!(
            validate_prediction_snapshot_sources(&missing_oracle_audit, &observations)
                .expect_err("missing event-level oracle audit must fail")
                .contains("missing governed Chainlink five-minute settlement audit")
        );

        let mut missing_oracle_evidence = manifest.clone();
        missing_oracle_evidence
            .chainlink_oracle_settlement_evidence
            .clear();
        assert!(
            validate_prediction_snapshot_sources(&missing_oracle_evidence, &observations)
                .expect_err("missing event-level oracle evidence must fail")
                .contains("missing governed Chainlink five-minute settlement evidence")
        );

        let mut zero_expected_events = manifest.clone();
        zero_expected_events.chainlink_oracle_settlement_audit =
            Some(crate::ChainlinkOracleSettlementAudit::default());
        zero_expected_events
            .chainlink_oracle_settlement_evidence
            .clear();
        assert!(
            validate_prediction_snapshot_sources(&zero_expected_events, &observations)
                .expect_err("zero-event prediction snapshot must fail before publication")
                .contains("no expected events")
        );

        let mut payout_mismatch = manifest.clone();
        payout_mismatch.chainlink_oracle_settlement_evidence[0].official_outcome_up = Some(false);
        payout_mismatch.chainlink_oracle_settlement_evidence[0].reasons =
            vec![crate::ChainlinkOracleFailureReason::OfficialPayoutMismatch];
        payout_mismatch.chainlink_oracle_settlement_audit =
            Some(crate::ChainlinkOracleSettlementAudit::from_evidence(
                &payout_mismatch.chainlink_oracle_settlement_evidence,
            ));
        let mismatch = validate_prediction_snapshot_sources(&payout_mismatch, &observations)
            .expect_err("one payout mismatch must reject the whole prediction snapshot");
        assert!(mismatch.contains("payout_mismatch=1"));
        assert!(mismatch.contains("event-1"));

        let mut unknown_event = observations.clone();
        unknown_event[0].event_id = "event-not-in-oracle-evidence".to_string();
        assert!(
            validate_prediction_snapshot_sources(&manifest, &unknown_event)
                .expect_err("observation without matching event evidence must fail")
                .contains("event-not-in-oracle-evidence")
        );

        let mut wrong_label = observations.clone();
        wrong_label[0].settlement_up = 0.0;
        assert!(
            validate_prediction_snapshot_sources(&manifest, &wrong_label)
                .expect_err("observation label must match the official payout")
                .contains("settlement_up")
        );

        let mut bad_boundary = manifest.clone();
        bad_boundary.chainlink_oracle_settlement_evidence[0]
            .open
            .as_mut()
            .expect("open boundary")
            .boundary_ts += chrono::Duration::seconds(1);
        assert!(
            validate_prediction_snapshot_sources(&bad_boundary, &observations)
                .expect_err("tampered opening boundary must fail")
                .contains("open boundary_ts")
        );

        let mut bad_official = manifest.clone();
        bad_official.chainlink_oracle_settlement_evidence[0].official_outcome_up = Some(false);
        assert!(
            validate_prediction_snapshot_sources(&bad_official, &observations)
                .expect_err("official payout must corroborate Chainlink")
                .contains("official outcome")
        );

        let mut missing_lob = manifest.clone();
        missing_lob
            .source_surfaces
            .retain(|surface| surface.name != "binance_lob_ticks");
        assert!(
            validate_prediction_snapshot_sources(&missing_lob, &observations)
                .expect_err("missing LOB must fail")
                .contains("binance_lob_ticks")
        );

        let mut missing_chainlink = manifest.clone();
        missing_chainlink
            .source_surfaces
            .retain(|surface| surface.name != REQUIRED_CHAINLINK_SOURCE_SURFACE);
        assert!(
            validate_prediction_snapshot_sources(&missing_chainlink, &observations)
                .expect_err("missing Chainlink authority must fail")
                .contains(REQUIRED_CHAINLINK_SOURCE_SURFACE)
        );

        let mut missing_clob = manifest.clone();
        missing_clob
            .source_surfaces
            .retain(|surface| surface.name != REQUIRED_POLYMARKET_SOURCE_SURFACE);
        assert!(
            validate_prediction_snapshot_sources(&missing_clob, &observations)
                .expect_err("missing Polymarket execution depth must fail")
                .contains(REQUIRED_POLYMARKET_SOURCE_SURFACE)
        );

        let mut zero_agg_trade = manifest.clone();
        zero_agg_trade
            .source_surfaces
            .iter_mut()
            .find(|surface| surface.name == "binance_agg_trade_ticks")
            .expect("agg-trade surface")
            .row_count = Some(0);
        assert!(
            validate_prediction_snapshot_sources(&zero_agg_trade, &observations)
                .expect_err("zero agg-trade rows must fail")
                .contains("binance_agg_trade_ticks")
        );

        let mut unaudited = manifest;
        unaudited.data_audit_status = Some("critical".to_string());
        assert!(
            validate_prediction_snapshot_sources(&unaudited, &observations)
                .expect_err("critical audit must fail")
                .contains("data_audit_status")
        );
    }

    #[test]
    fn prediction_snapshot_coverage_is_isolated_to_the_mission_underlying() {
        let manifest = snapshot_manifest();
        let tick_ts = manifest.start + chrono::Duration::minutes(1);
        let mut bound_mission = mission();
        bound_mission.data_snapshot_id = manifest
            .snapshot_contract_hash
            .clone()
            .expect("contract hash");
        let snapshot = ResearchSnapshot {
            manifest,
            observations: vec![test_prediction_observation(tick_ts)],
            deribit_snapshots: Vec::new(),
            pm_book_snapshots: vec![crate::ResearchPmBookSnapshot {
                event_id: "event-1".to_string(),
                token_id: "up-token".to_string(),
                side: "up".to_string(),
                ts: tick_ts,
                bids: Vec::new(),
                asks: Vec::new(),
            }],
        };
        validate_mission_snapshot_binding(&bound_mission, &snapshot)
            .expect("single-underlying snapshot");

        let mut mixed = snapshot.clone();
        mixed.manifest.symbols.push("SOLUSDT".to_string());
        assert!(validate_mission_snapshot_binding(&bound_mission, &mixed)
            .expect_err("mixed-underlying coverage must fail")
            .contains("only mission underlying"));

        let mut foreign_book = snapshot.clone();
        foreign_book.pm_book_snapshots[0].event_id = "sol-event".to_string();
        assert!(
            validate_mission_snapshot_binding(&bound_mission, &foreign_book)
                .expect_err("foreign event depth must fail")
                .contains("outside the BTC mission event set")
        );

        let mut foreign_deribit = snapshot;
        foreign_deribit.manifest.row_counts.deribit_snapshots = 1;
        foreign_deribit
            .deribit_snapshots
            .push(crate::DeribitFeatureSnapshot {
                symbol: "SOL-PERPETUAL".to_string(),
                ts: tick_ts,
                mark_iv: 0.5,
                bid_iv: 0.49,
                ask_iv: 0.51,
                underlying_price: 100.0,
                delta: 0.5,
                gamma: 0.0,
                vega: 0.0,
                theta: 0.0,
            });
        assert!(
            validate_mission_snapshot_binding(&bound_mission, &foreign_deribit)
                .expect_err("foreign Deribit underlying must fail")
                .contains("Deribit rows")
        );
    }

    struct FakeProposalClient {
        calls: usize,
        responses: VecDeque<Result<String, String>>,
    }

    impl ProposalClient for FakeProposalClient {
        fn propose(
            &mut self,
            _prompt: &str,
            _timeout: Duration,
        ) -> Result<ProposalCallOutput, String> {
            self.calls += 1;
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err("no fake response queued".to_string()))
                .map(|raw_response| ProposalCallOutput {
                    raw_response,
                    provider: "fake-grok-builder".to_string(),
                    model: "fake-model".to_string(),
                    usage: serde_json::json!({"total_tokens": 12}),
                })
        }
    }

    struct FakeEvaluator {
        calls: usize,
        fail_candidate_once: bool,
        verdict: &'static str,
    }

    fn fake_feedback(prior: &LlmPriorSpec, verdict: &str) -> PredictionResearchFeedback {
        let gate_policy = PredictionResearchGatePolicy::governed();
        let candidates = prior
            .probability_blends
            .iter()
            .cloned()
            .map(|probability_blend| {
                let metrics = crate::PredictionResearchCandidateMetrics {
                    windows: 3,
                    positive_window_ratio: Some(1.0),
                    pass_window_ratio: Some(1.0),
                    avg_test_brier_score: Some(if verdict == "search_pass" { 0.10 } else { 0.40 }),
                    avg_test_log_loss: Some(0.20),
                    avg_test_expected_calibration_error: Some(0.01),
                    avg_test_top_edge_avg_full_depth_settlement_pnl: Some(0.40),
                    min_test_top_edge_avg_full_depth_settlement_pnl: Some(0.20),
                    avg_test_top_edge_avg_conservative_settlement_pnl: Some(0.30),
                    min_test_top_edge_avg_conservative_settlement_pnl: Some(0.10),
                    min_top_edge_conservative_coverage_rate: Some(1.0),
                    best_baseline_brier_score: Some(0.20),
                    best_baseline_log_loss: Some(0.40),
                    best_baseline_avg_conservative_settlement_pnl: Some(0.10),
                };
                let (recomputed_verdict, reason_codes) =
                    recompute_prediction_research_outcome(&metrics, &gate_policy)
                        .expect("valid fake metrics");
                assert_eq!(recomputed_verdict, verdict);
                crate::PredictionResearchCandidateFeedback {
                    model: format!("q_llm_{}", probability_blend.name),
                    hypothesis: probability_blend.hypothesis.clone(),
                    probability_blend,
                    verdict: recomputed_verdict,
                    reason_codes,
                    metrics,
                }
            })
            .collect();
        PredictionResearchFeedback {
            schema_version: PREDICTION_FEEDBACK_SCHEMA_VERSION.to_string(),
            mission_id: prior.mission_id.clone().expect("mission id"),
            target: prior.target.clone().expect("target"),
            symbols: prior.symbols.clone(),
            horizon: prior.horizon.clone().expect("horizon"),
            data_snapshot_id: prior.data_snapshot_id.clone().expect("snapshot id"),
            prompt_snapshot_id: prior.prompt_snapshot_id.clone().expect("prompt id"),
            search_policy_snapshot_id: prior.search_policy_snapshot_id.clone().expect("policy id"),
            gate_policy,
            candidates,
        }
    }

    impl PredictionEvaluator for FakeEvaluator {
        fn evaluate(
            &mut self,
            request: &PredictionEvaluationRequest,
            _timeout: Duration,
        ) -> PredictionEvaluationOutput {
            self.calls += 1;
            let Some(prior) = request.prior.as_ref() else {
                return PredictionEvaluationOutput {
                    success: true,
                    feedback: None,
                    stdout: "baseline ok".to_string(),
                    stderr: String::new(),
                    failure_reason: None,
                };
            };
            if self.fail_candidate_once {
                self.fail_candidate_once = false;
                return PredictionEvaluationOutput {
                    success: false,
                    feedback: None,
                    stdout: String::new(),
                    stderr: "transient evaluator failure".to_string(),
                    failure_reason: Some("transient evaluator failure".to_string()),
                };
            }
            PredictionEvaluationOutput {
                success: true,
                feedback: Some(fake_feedback(prior, self.verdict)),
                stdout: "candidate evaluation ok".to_string(),
                stderr: String::new(),
                failure_reason: None,
            }
        }
    }

    struct RootReplacingEvaluator {
        calls: usize,
        output_dir: PathBuf,
        moved_output_dir: PathBuf,
        staged_artifact_dir: Option<PathBuf>,
        staged_prior_path: Option<PathBuf>,
    }

    impl PredictionEvaluator for RootReplacingEvaluator {
        fn evaluate(
            &mut self,
            request: &PredictionEvaluationRequest,
            _timeout: Duration,
        ) -> PredictionEvaluationOutput {
            self.calls += 1;
            let Some(prior) = request.prior.as_ref() else {
                return PredictionEvaluationOutput {
                    success: true,
                    feedback: None,
                    stdout: "baseline ok".to_string(),
                    stderr: String::new(),
                    failure_reason: None,
                };
            };

            let prior_path = request
                .prior_artifact_path
                .as_ref()
                .expect("candidate evaluator receives staged prior");
            assert!(!request.artifact_dir.starts_with(&self.output_dir));
            assert!(!request.artifact_dir.starts_with(&self.moved_output_dir));
            assert!(!prior_path.starts_with(&self.output_dir));
            assert!(!prior_path.starts_with(&self.moved_output_dir));
            self.staged_artifact_dir = Some(request.artifact_dir.clone());
            self.staged_prior_path = Some(prior_path.clone());
            fs::rename(&self.output_dir, &self.moved_output_dir)
                .expect("replace output root during evaluator call");
            fs::create_dir_all(&self.output_dir).expect("create hostile replacement output root");
            let staged_prior: LlmPriorSpec = serde_json::from_slice(
                &fs::read(prior_path)
                    .expect("read staged content-addressed prior after output replacement"),
            )
            .expect("parse staged content-addressed prior");
            assert_eq!(
                canonical_json_bytes(&staged_prior).expect("canonical staged prior"),
                canonical_json_bytes(prior).expect("canonical governed prior")
            );
            fs::write(
                request.artifact_dir.join("external-evaluator-probe"),
                b"staging-only",
            )
            .expect("external evaluator writes only to private staging");

            PredictionEvaluationOutput {
                success: true,
                feedback: Some(fake_feedback(prior, "search_pass")),
                stdout: "candidate evaluation survived output path replacement".to_string(),
                stderr: String::new(),
                failure_reason: None,
            }
        }
    }

    fn governed_test_fixture() -> (PathBuf, PathBuf, PredictionResearchMission) {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-loop-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let snapshot_dir = root.join("snapshot");
        let output_dir = root.join("run");
        let manifest = snapshot_manifest();
        let tick_ts = manifest.start + chrono::Duration::minutes(1);
        let written = write_research_snapshot(
            &snapshot_dir,
            ResearchSnapshot {
                manifest,
                observations: vec![test_prediction_observation(tick_ts)],
                deribit_snapshots: Vec::new(),
                pm_book_snapshots: vec![crate::ResearchPmBookSnapshot {
                    event_id: "event-1".to_string(),
                    token_id: "up-token".to_string(),
                    side: "up".to_string(),
                    ts: tick_ts,
                    bids: vec![crate::ResearchPmBookLevel {
                        price: 0.49,
                        size: 10.0,
                    }],
                    asks: vec![crate::ResearchPmBookLevel {
                        price: 0.51,
                        size: 10.0,
                    }],
                }],
            },
        )
        .expect("write governed snapshot");
        let mut mission = mission();
        mission.data_snapshot_id = written
            .snapshot_contract_hash
            .expect("snapshot contract hash");
        mission.search_policy_snapshot_id = current_prediction_policy_snapshot_id();
        mission.prompt_snapshot_id = research_brief_snapshot_id(&mission);
        (snapshot_dir, output_dir, mission)
    }

    fn proposal_json() -> String {
        serde_json::to_string(&proposal()).expect("serialize fake proposal")
    }

    #[test]
    fn rust_loop_returns_only_a_provisional_adaptive_search_pass() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: false,
            verdict: "search_pass",
        };

        let summary = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("complete Rust loop");

        assert_eq!(summary.status, LoopRunStatus::Provisional);
        assert_eq!(summary.llm_calls_used, 1);
        assert_eq!(summary.candidates_evaluated, 1);
        assert_eq!(summary.provisional_models, ["q_llm_binance_context"]);
        assert!(summary
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("separate sealed holdout required")));
        assert_eq!(client.calls, 1);
        assert_eq!(evaluator.calls, 2);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn evaluator_output_root_replacement_cannot_split_prior_or_artifacts() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let moved_output_dir = output_dir.with_extension("opened-root");
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = RootReplacingEvaluator {
            calls: 0,
            output_dir: output_dir.clone(),
            moved_output_dir: moved_output_dir.clone(),
            staged_artifact_dir: None,
            staged_prior_path: None,
        };

        let error = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect_err("output path replacement must fail closed");

        assert!(error.contains("output root path changed during external evaluation"));
        assert!(error.contains("must be restored before resume"));
        assert_eq!(evaluator.calls, 2);
        assert!(moved_output_dir
            .join("prediction-loop-state.json")
            .is_file());
        assert!(moved_output_dir.join("iterations").is_dir());
        let abandoned_attempt =
            moved_output_dir.join("iterations/iteration-001/evaluator-attempts/attempt-001");
        assert!(fs::read_dir(&abandoned_attempt)
            .expect("read abandoned evaluator attempt")
            .next()
            .is_none());
        assert!(fs::read_dir(&output_dir)
            .expect("read hostile replacement root")
            .next()
            .is_none());
        assert!(!evaluator
            .staged_artifact_dir
            .expect("record staged artifact directory")
            .exists());
        assert!(!evaluator
            .staged_prior_path
            .expect("record staged prior path")
            .exists());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn evaluator_pause_resumes_pending_prior_without_new_llm_call() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: true,
            verdict: "search_pass",
        };

        let paused = run_or_resume(
            mission.clone(),
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("pause on evaluator failure");
        assert_eq!(paused.status, LoopRunStatus::Paused);
        assert_eq!(paused.llm_calls_used, 1);
        assert_eq!(client.calls, 1);

        let resumed = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("resume pending prior");
        assert_eq!(resumed.status, LoopRunStatus::Provisional);
        assert_eq!(resumed.llm_calls_used, 1);
        assert_eq!(client.calls, 1, "resume must not call the LLM again");
        assert_eq!(evaluator.calls, 3);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn orphan_evaluator_feedback_is_ignored_and_rerun() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: true,
            verdict: "search_pass",
        };

        let paused = run_or_resume(
            mission.clone(),
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("pause before simulated durable evaluator completion");
        assert_eq!(paused.status, LoopRunStatus::Paused);
        let output_root = OutputRoot::open(&output_dir).expect("open output root");
        let state: PredictionLoopState =
            read_json(&output_root, Path::new("prediction-loop-state.json"))
                .expect("read paused state");
        let identity = state.identity.clone();
        let pending = match state.frontier {
            Frontier::Evaluate { pending } => pending,
            _ => panic!("expected pending evaluator frontier"),
        };
        let prior_artifact = verify_artifact(&output_root, &pending.prior).expect("prior evidence");
        let prior: LlmPriorSpec = prior_artifact.parse_json().expect("read pending prior");
        let evaluator_parent = PathBuf::from("iterations")
            .join(format!("iteration-{:03}", pending.iteration))
            .join("evaluator-attempts");
        let completed_attempt =
            next_attempt_dir(&output_root, &evaluator_parent).expect("orphan attempt dir");
        let evidence = persist_evaluation_output(
            &output_root,
            &completed_attempt,
            Some(&fake_feedback(&prior, "search_pass")),
            "durable evaluator stdout",
            "",
        )
        .expect("persist completed evidence before simulated crash");
        persist_search_pass_evidence(
            &output_root,
            SearchPassEvidence {
                schema_version: SEARCH_PASS_EVIDENCE_SCHEMA_VERSION.to_string(),
                identity,
                iteration: pending.iteration,
                call_index: pending.call_index,
                prior_sha256: pending.prior.sha256,
                feedback_sha256: evidence
                    .feedback
                    .expect("orphan evaluation feedback")
                    .sha256,
            },
        )
        .expect("persist orphan search-pass record before simulated crash");

        let resumed = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("rerun pending evaluator");
        assert_eq!(resumed.status, LoopRunStatus::Provisional);
        assert_eq!(client.calls, 1);
        assert_eq!(
            evaluator.calls, 3,
            "uncheckpointed evaluator feedback must not bypass a deterministic rerun"
        );
        drop(output_root);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn feedback_verdict_is_recomputed_from_typed_metrics() {
        let mission = mission();
        let prior = mission_prior(
            &mission,
            validate_prediction_proposal(proposal(), 1).expect("valid proposal"),
        );
        let mut feedback = fake_feedback(&prior, "search_pass");
        feedback.candidates[0].metrics.avg_test_brier_score = Some(0.90);
        assert!(validate_feedback(&mission, &prior, &feedback)
            .expect_err("forged search pass must fail")
            .contains("independently recomputed metrics"));
    }

    #[test]
    fn append_only_search_pass_overrides_mutable_provisional_models() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: false,
            verdict: "search_pass",
        };
        run_or_resume(
            mission.clone(),
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("complete loop");

        let output_root = OutputRoot::open(&output_dir).expect("open output root");
        let state_relative = Path::new("prediction-loop-state.json");
        let mut state: PredictionLoopState =
            read_json(&output_root, state_relative).expect("read state");
        let Frontier::Done { outcome } = &mut state.frontier else {
            panic!("expected terminal state")
        };
        outcome.provisional_models = vec!["forged-model".to_string()];
        atomic_write_json(&output_root, state_relative, &state).expect("overwrite mutable state");

        let mut replay_client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::new(),
        };
        let mut replay_evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: false,
            verdict: "search_pass",
        };
        let summary = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut replay_client,
            &mut replay_evaluator,
        )
        .expect("recompute terminal search-pass evidence");
        assert_eq!(summary.provisional_models, ["q_llm_binance_context"]);
        assert_eq!(replay_client.calls, 0);
        assert_eq!(replay_evaluator.calls, 0);
        drop(output_root);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn interrupted_response_orphan_is_abandoned() {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-response-recovery-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let output_root = OutputRoot::open(&root).expect("open output root");
        let attempt_dir = Path::new("llm-calls/call-001");
        let prompt =
            write_content_addressed_text(&output_root, attempt_dir, "llm-prompt", "prompt")
                .expect("prompt evidence");
        let response = ProposalCallOutput {
            raw_response: proposal_json(),
            provider: "grok-builder".to_string(),
            model: "grok-local-q4".to_string(),
            usage: serde_json::json!({"total_tokens": 17}),
        };
        write_content_addressed_json(&output_root, attempt_dir, "llm-response", &response)
            .expect("response envelope");
        let mut state = PredictionLoopState {
            schema_version: PREDICTION_LOOP_STATE_SCHEMA_VERSION.to_string(),
            identity: RunIdentity {
                mission_id: "mission".to_string(),
                mission_hash: "mission-hash".to_string(),
                symbol: "BTC".to_string(),
                data_snapshot_id: "snapshot".to_string(),
                snapshot_hash: "snapshot-hash".to_string(),
                prompt_snapshot_id: "prompt-hash".to_string(),
                search_policy_snapshot_id: "policy-hash".to_string(),
            },
            elapsed_millis: 0,
            baseline: None,
            llm_attempts: vec![LlmAttemptRecord {
                call_index: 1,
                prompt,
                response: None,
                provider: None,
                model: None,
                usage: None,
                status: LlmAttemptStatus::Started,
                error: None,
            }],
            iterations: Vec::new(),
            failures: Vec::new(),
            frontier: Frontier::Propose,
            pause: None,
        };
        assert!(
            recover_interrupted_llm_attempt(&mut state, &output_root).expect("abandon response")
        );
        let recovered = &state.llm_attempts[0];
        assert!(recovered.response.is_none());
        assert!(recovered.provider.is_none());
        assert!(recovered.model.is_none());
        assert!(recovered.usage.is_none());
        assert_eq!(recovered.status, LlmAttemptStatus::Abandoned);
        assert!(recovered
            .error
            .as_deref()
            .is_some_and(|error| error.contains("ignored")));
        drop(output_root);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn invalid_grok_response_is_counted_before_budget_stops_loop() {
        let (snapshot_dir, output_dir, mut mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        mission.search_budget.max_llm_calls = 1;
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok("{\"mutations\":[]}".to_string())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: false,
            verdict: "search_pass",
        };

        let summary = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("invalid response reaches budget terminal");
        assert_eq!(summary.status, LoopRunStatus::BudgetExhausted);
        assert_eq!(summary.llm_calls_used, 1);
        assert_eq!(summary.candidates_evaluated, 0);
        assert_eq!(client.calls, 1);
        assert_eq!(evaluator.calls, 1, "only deterministic baseline ran");
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
