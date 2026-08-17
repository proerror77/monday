//! Rust-owned policy and shared contracts for BTC or SOL prediction-market research.
//!
//! The active search loop is the bounded MCTS runner. This module has no execution or
//! live-trading authority. Continuous-contract research remains in Monday's separate
//! `alpha-harness` workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    normalized_underlying_symbol, LlmPriorSpec, LlmProbabilityBlendSpec,
    PredictionResearchFeedback, ResearchSnapshot, ResearchSnapshotManifest,
};

pub const PREDICTION_MISSION_SCHEMA_VERSION: &str = "prediction_research_mission.v2";
pub const PREDICTION_LOOP_TARGET: &str = "full_depth_settlement_executable_pnl";
pub const PREDICTION_EVENT_WINDOW_SECS: i64 = 300;
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
pub const REQUIRED_SETTLEMENT_SOURCE_SURFACE: &str = "pm_token_settlements";

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
    pub time_cohort_boundary_ms: i64,
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
            authority: "contract reference-price probability; Chainlink defines the opening reference and expiry-price semantics, while Polymarket official resolution owns the binary label",
        },
        ProbabilityComponentDescription {
            name: "distance_lob_vol",
            inputs: "Binance CEX spot distance to the Chainlink-defined price-to-beat, Binance L2 imbalance and depth, side-aggregated Binance aggTrade flow, and realized volatility",
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
            inputs: "Binance CEX spot, Chainlink-defined price-to-beat, remaining time, and horizon volatility",
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

pub fn current_prediction_policy_snapshot_id() -> String {
    let mut digest = Sha256::new();
    for (path, body) in prediction_policy_sources() {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(body);
        digest.update([0]);
    }
    digest.update(b"prediction-policy-dependency-profile");
    digest.update([0]);
    digest.update(crate::prediction_policy_identity::prediction_dependency_fingerprint());
    digest.update([0]);
    format!("sha256:{:x}", digest.finalize())
}

fn prediction_policy_sources() -> [(&'static str, &'static [u8]); 41] {
    [
        (
            "crates/ploy-research/src/autofactor.rs",
            include_bytes!("autofactor.rs"),
        ),
        (
            "crates/ploy-research/src/alpha_search.rs",
            include_bytes!("alpha_search.rs"),
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
            "crates/ploy-research/src/event_cohort_partition.rs",
            include_bytes!("event_cohort_partition.rs"),
        ),
        (
            "crates/ploy-research/src/polymarket_evidence_projection.rs",
            include_bytes!("polymarket_evidence_projection.rs"),
        ),
        (
            "crates/ploy-research/src/verified_artifact_audit.rs",
            include_bytes!("verified_artifact_audit.rs"),
        ),
        (
            "crates/ploy-research/src/verified_binance_projection.rs",
            include_bytes!("verified_binance_projection.rs"),
        ),
        (
            "crates/ploy-market-data/src/diagnostics.rs",
            include_bytes!("../../ploy-market-data/src/diagnostics.rs"),
        ),
        (
            "crates/ploy-market-data/src/polymarket_evidence/mod.rs",
            include_bytes!("../../ploy-market-data/src/polymarket_evidence/mod.rs"),
        ),
        (
            "crates/ploy-market-data/src/polymarket_evidence/artifact.rs",
            include_bytes!("../../ploy-market-data/src/polymarket_evidence/artifact.rs"),
        ),
        (
            "crates/ploy-market-data/src/polymarket_evidence/wire.rs",
            include_bytes!("../../ploy-market-data/src/polymarket_evidence/wire.rs"),
        ),
        (
            "crates/ploy-market-data/src/polymarket_evidence/verified.rs",
            include_bytes!("../../ploy-market-data/src/polymarket_evidence/verified.rs"),
        ),
        (
            "crates/ploy-market-data/src/polymarket_evidence/set.rs",
            include_bytes!("../../ploy-market-data/src/polymarket_evidence/set.rs"),
        ),
        (
            "crates/ploy-market-data/src/polymarket_evidence/catalog.rs",
            include_bytes!("../../ploy-market-data/src/polymarket_evidence/catalog.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_loop.rs",
            include_bytes!("prediction_loop.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_mission_v3.rs",
            include_bytes!("prediction_mission_v3.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_mcts.rs",
            include_bytes!("prediction_mcts.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_mcts_run.rs",
            include_bytes!("prediction_mcts_run.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_mcts_authenticated.rs",
            include_bytes!("prediction_mcts_authenticated.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_loop_fs.rs",
            include_bytes!("prediction_loop_fs.rs"),
        ),
        (
            "crates/ploy-research/src/prediction_policy_identity.rs",
            include_bytes!("prediction_policy_identity.rs"),
        ),
        (
            "crates/ploy-research/build.rs",
            include_bytes!("../build.rs"),
        ),
        (
            "crates/ploy-research/src/research_snapshot.rs",
            include_bytes!("research_snapshot.rs"),
        ),
        (
            "crates/ploy-research/src/bin/monday-prediction-evaluator.rs",
            include_bytes!("bin/monday-prediction-evaluator.rs"),
        ),
        (
            "crates/ploy-research/src/bin/monday-prediction-research.rs",
            include_bytes!("bin/monday-prediction-research.rs"),
        ),
        (
            "crates/ploy-feed-loaders/src/database.rs",
            include_bytes!("../../ploy-feed-loaders/src/database.rs"),
        ),
        (
            "crates/ploy-feed-loaders/Cargo.toml",
            include_bytes!("../../ploy-feed-loaders/Cargo.toml"),
        ),
        (
            "crates/ploy-feed-loaders/src/lib.rs",
            include_bytes!("../../ploy-feed-loaders/src/lib.rs"),
        ),
        (
            "config/autofactor_accounting_catalog.json",
            include_bytes!("../../../config/autofactor_accounting_catalog.json"),
        ),
        (
            "config/autofactor_runtime_contract_catalog.json",
            include_bytes!("../../../config/autofactor_runtime_contract_catalog.json"),
        ),
        (
            "crates/ploy-market-contracts/Cargo.toml",
            include_bytes!("../../ploy-market-contracts/Cargo.toml"),
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
            "crates/ploy-market-contracts/src/family.rs",
            include_bytes!("../../ploy-market-contracts/src/family.rs"),
        ),
        (
            "crates/ploy-market-contracts/src/feed.rs",
            include_bytes!("../../ploy-market-contracts/src/feed.rs"),
        ),
        (
            "crates/ploy-market-contracts/src/fees.rs",
            include_bytes!("../../ploy-market-contracts/src/fees.rs"),
        ),
        (
            "crates/ploy-market-contracts/src/instrument.rs",
            include_bytes!("../../ploy-market-contracts/src/instrument.rs"),
        ),
        (
            "crates/ploy-market-contracts/src/regime.rs",
            include_bytes!("../../ploy-market-contracts/src/regime.rs"),
        ),
        (
            "crates/ploy-market-contracts/src/venue.rs",
            include_bytes!("../../ploy-market-contracts/src/venue.rs"),
        ),
    ]
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
    if mission.time_cohort_boundary_ms <= 0 {
        return Err("mission.time_cohort_boundary_ms must be positive".to_string());
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
    validate_prediction_search_budget(&mission.search_budget)?;
    Ok(())
}

pub(crate) fn validate_prediction_search_budget(
    search_budget: &PredictionSearchBudget,
) -> Result<(), String> {
    if search_budget.max_seconds == 0 {
        return Err("mission.search_budget.max_seconds must be positive".to_string());
    }
    let baseline_only_budget =
        search_budget.max_candidates == 0 && search_budget.max_llm_calls == 0;
    if (search_budget.max_candidates == 0 || search_budget.max_llm_calls == 0)
        && !baseline_only_budget
    {
        return Err(
            "mission.search_budget baseline-only mode requires max_candidates and max_llm_calls to both be zero"
                .to_string(),
        );
    }
    if search_budget.max_candidates > MAX_GOVERNED_CANDIDATES
        || search_budget.max_llm_calls > MAX_GOVERNED_LLM_CALLS
        || search_budget.max_seconds > MAX_GOVERNED_SECONDS
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

pub(crate) fn validate_sha256_id(value: &str, field: &str) -> Result<(), String> {
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

/// Enforce that a settlement mission contains Chainlink reference prices,
/// official Polymarket outcomes, and the Binance CEX context consumed by Rust.
///
/// Binance is an independent predictor/repricing clock only.  This contract
/// does not grant it settlement-oracle or execution authority.
pub fn validate_prediction_snapshot_sources(
    manifest: &ResearchSnapshotManifest,
) -> Result<(), String> {
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
    let baseline = manifest.source_kind == crate::POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND;
    if baseline {
        if requirements != BTreeSet::from([crate::POLYMARKET_CHAINLINK_BASELINE_REQUIREMENT])
            || !manifest
                .quality_flags
                .iter()
                .any(|flag| flag == crate::BINANCE_SURFACES_OMITTED_QUALITY_FLAG)
        {
            return Err(
                "Polymarket + Chainlink baseline must declare its exact reduced-authority profile"
                    .to_string(),
            );
        }
        for required in REQUIRED_BINANCE_SOURCE_SURFACES {
            let surface = manifest
                .source_surfaces
                .iter()
                .find(|surface| surface.name == required)
                .ok_or_else(|| {
                    format!(
                        "Polymarket + Chainlink baseline is missing intentionally omitted surface {required}"
                    )
                })?;
            if surface.role != "intentionally_omitted"
                || surface.gate_category != "optional_context"
                || surface.snapshot_sampled
                || surface.sample_secs.is_some()
                || surface.row_count != Some(0)
            {
                return Err(format!(
                    "baseline Binance surface {required} must be intentionally omitted, optional, unsampled, and empty"
                ));
            }
        }
    } else if !requirements.contains("all") {
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
    if !baseline {
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
    }
    let chainlink = manifest
        .source_surfaces
        .iter()
        .find(|surface| surface.name == REQUIRED_CHAINLINK_SOURCE_SURFACE)
        .ok_or_else(|| {
            format!(
                "prediction snapshot is missing Chainlink reference surface {REQUIRED_CHAINLINK_SOURCE_SURFACE}"
            )
        })?;
    if chainlink.gate_category != "required_for_prediction"
        || !chainlink.snapshot_sampled
        || chainlink.row_count.unwrap_or_default() == 0
    {
        return Err(format!(
            "Chainlink reference surface {REQUIRED_CHAINLINK_SOURCE_SURFACE} must be sampled, prediction-gated, and non-empty"
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
    let settlement = manifest
        .source_surfaces
        .iter()
        .find(|surface| surface.name == REQUIRED_SETTLEMENT_SOURCE_SURFACE)
        .ok_or_else(|| {
            format!(
                "prediction snapshot is missing official outcome surface {REQUIRED_SETTLEMENT_SOURCE_SURFACE}"
            )
        })?;
    if settlement.gate_category != "required_for_prediction" || !settlement.raw_full_fidelity {
        return Err(format!(
            "official outcome surface {REQUIRED_SETTLEMENT_SOURCE_SURFACE} must be full-fidelity and prediction-gated"
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

/// Bind evaluator-visible rows to the mission's single underlying and prove
/// event-level coverage for every authority consumed by the five-minute lane.
pub fn validate_prediction_snapshot_coverage(
    snapshot: &ResearchSnapshot,
    mission: &PredictionResearchMission,
) -> Result<(), String> {
    let baseline =
        snapshot.manifest.source_kind == crate::POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND;
    let requested = mission
        .symbols
        .first()
        .map(String::as_str)
        .ok_or_else(|| "prediction coverage requires one mission symbol".to_string())?;
    if snapshot.observations.is_empty() {
        return Err("prediction snapshot contains no evaluator observations".to_string());
    }

    let mut labels = BTreeMap::<&str, f64>::new();
    let mut label_observation_clocks = BTreeMap::<&str, chrono::DateTime<chrono::Utc>>::new();
    let mut token_ids = BTreeMap::<&str, (&str, &str)>::new();
    let mut chainlink_events = BTreeSet::new();
    let mut binance_spot_events = BTreeSet::new();
    let mut binance_lob_events = BTreeSet::new();
    let mut binance_agg_trade_events = BTreeSet::new();
    let mut quote_events = BTreeSet::new();
    let mut up_book_events = BTreeSet::new();
    let mut down_book_events = BTreeSet::new();
    let max_book_age_secs = snapshot.manifest.max_quote_age_secs.max(0) as f64;
    let has_fresh_book = |row: &crate::FactorObservation, token_id: &str, side: &str| {
        snapshot.pm_book_snapshots.iter().any(|book| {
            let age_secs = (row.tick_ts - book.ts).num_milliseconds() as f64 / 1000.0;
            book.event_id == row.event_id
                && book.token_id == token_id
                && book.side.eq_ignore_ascii_case(side)
                && age_secs >= 0.0
                && age_secs <= max_book_age_secs
                && !book.bids.is_empty()
                && !book.asks.is_empty()
                && book.bids.iter().chain(&book.asks).all(|level| {
                    level.price.is_finite()
                        && level.price > 0.0
                        && level.price < 1.0
                        && level.size.is_finite()
                        && level.size > 0.0
                })
        })
    };

    for row in &snapshot.observations {
        if normalized_underlying_symbol(&row.symbol) != requested {
            return Err(format!(
                "snapshot observation {} belongs to {}, not isolated mission underlying {requested}",
                row.event_id, row.symbol
            ));
        }
        if row.event_window_secs != PREDICTION_EVENT_WINDOW_SECS {
            return Err(format!(
                "snapshot event {} has {}s horizon; prediction LoopRun requires {}s",
                row.event_id, row.event_window_secs, PREDICTION_EVENT_WINDOW_SECS
            ));
        }
        if !matches!(row.settlement_up, 0.0 | 1.0) {
            return Err(format!(
                "snapshot event {} lacks an official binary settlement label",
                row.event_id
            ));
        }
        if labels
            .insert(&row.event_id, row.settlement_up)
            .is_some_and(|previous| previous != row.settlement_up)
        {
            return Err(format!(
                "snapshot event {} has inconsistent official settlement labels",
                row.event_id
            ));
        }
        let settlement_at = row.tick_ts + chrono::Duration::seconds(row.time_remaining_secs);
        let resolution_observed_at = row.official_resolution_observed_at.ok_or_else(|| {
            format!(
                "snapshot event {} lacks an official resolution observation clock",
                row.event_id
            )
        })?;
        if resolution_observed_at < settlement_at
            || resolution_observed_at > snapshot.manifest.generated_at
        {
            return Err(format!(
                "snapshot event {} has an invalid official resolution observation clock",
                row.event_id
            ));
        }
        if label_observation_clocks
            .insert(row.event_id.as_str(), resolution_observed_at)
            .is_some_and(|previous| previous != resolution_observed_at)
        {
            return Err(format!(
                "snapshot event {} has inconsistent official resolution observation clocks",
                row.event_id
            ));
        }
        if row.up_token_id.trim().is_empty()
            || row.down_token_id.trim().is_empty()
            || row.up_token_id == row.down_token_id
        {
            return Err(format!(
                "snapshot event {} lacks distinct Polymarket UP/DOWN token identities",
                row.event_id
            ));
        }
        if let Some((up_token_id, down_token_id)) = token_ids.get(row.event_id.as_str()) {
            if *up_token_id != row.up_token_id || *down_token_id != row.down_token_id {
                return Err(format!(
                    "snapshot event {} has inconsistent Polymarket token identities",
                    row.event_id
                ));
            }
        } else {
            token_ids.insert(
                row.event_id.as_str(),
                (row.up_token_id.as_str(), row.down_token_id.as_str()),
            );
        }
        if baseline {
            if !row.chainlink_reference_fresh || row.chainlink_prob_up.is_finite() {
                return Err(format!(
                    "baseline observation {} at {} must carry a fresh Chainlink opening reference without a fabricated continuous probability",
                    row.event_id, row.tick_ts
                ));
            }
        } else if !row.chainlink_reference_fresh
            || !row.chainlink_prob_up.is_finite()
            || !(0.0..=1.0).contains(&row.chainlink_prob_up)
        {
            return Err(format!(
                "snapshot observation {} at {} lacks fresh Chainlink reference probability",
                row.event_id, row.tick_ts
            ));
        }
        chainlink_events.insert(row.event_id.as_str());
        if baseline {
            let unavailable_cex_features = [
                row.signed_distance_to_beat,
                row.abs_distance_to_beat,
                row.drift_10s,
                row.drift_30s,
                row.flip_age_secs,
                row.post_flip_drift,
                row.sigma_horizon,
                row.implied_sigma_horizon,
                row.vol_gap,
                row.distance_over_sigma,
                row.model_prob_up,
                row.model_edge_up,
                row.obi,
                row.spread_bps,
                row.microprice_offset_bps,
                row.bid_depth_near,
                row.ask_depth_near,
                row.depth_ratio,
                row.depth_imbalance,
                row.depth_far_ratio,
                row.depth_acceleration,
                row.obi_10,
                row.cum_obi_delta_5m,
                row.cum_depth_delta_5m,
                row.cum_mprice_drift_5m,
                row.cum_trade_imbalance_5m,
                row.cex_bar_return_30s,
                row.cex_bar_return_60s,
                row.cex_bar_volume_ratio_30s,
                row.cex_bar_volume_trend_3,
                row.cex_signed_volume_ratio_30s,
                row.cex_consecutive_up_bars,
                row.cex_consecutive_down_bars,
                row.cex_breakout_volume_score,
            ];
            if row.binance_spot_fresh
                || row.binance_lob_fresh
                || row.binance_agg_trade_fresh
                || unavailable_cex_features
                    .iter()
                    .any(|value| value.is_finite())
            {
                return Err(format!(
                    "baseline observation {} at {} exposes unavailable CEX-derived inputs",
                    row.event_id, row.tick_ts
                ));
            }
        } else if !row.binance_spot_fresh
            || !row.model_prob_up.is_finite()
            || !(0.0..=1.0).contains(&row.model_prob_up)
        {
            return Err(format!(
                "snapshot observation {} at {} lacks fresh Binance spot probability context",
                row.event_id, row.tick_ts
            ));
        }
        if !baseline {
            binance_spot_events.insert(row.event_id.as_str());
        }
        if !baseline
            && (!row.binance_lob_fresh
                || !row.obi.is_finite()
                || !row.spread_bps.is_finite()
                || !row.bid_depth_near.is_finite()
                || !row.ask_depth_near.is_finite()
                || row.bid_depth_near < 0.0
                || row.ask_depth_near < 0.0
                || row.bid_depth_near + row.ask_depth_near <= 0.0)
        {
            return Err(format!(
                "snapshot observation {} at {} lacks fresh Binance L2 context",
                row.event_id, row.tick_ts
            ));
        }
        if !baseline {
            binance_lob_events.insert(row.event_id.as_str());
        }
        if !baseline && (!row.binance_agg_trade_fresh || !row.cum_trade_imbalance_5m.is_finite()) {
            return Err(format!(
                "snapshot observation {} at {} lacks fresh Binance aggTrade context",
                row.event_id, row.tick_ts
            ));
        }
        if !baseline {
            binance_agg_trade_events.insert(row.event_id.as_str());
        }
        if row.pm_up_bid.is_finite()
            || row.pm_up_ask.is_finite()
            || row.pm_down_bid.is_finite()
            || row.pm_down_ask.is_finite()
        {
            quote_events.insert(row.event_id.as_str());
        }
        if has_fresh_book(row, &row.up_token_id, "up") {
            up_book_events.insert(row.event_id.as_str());
        }
        if has_fresh_book(row, &row.down_token_id, "down") {
            down_book_events.insert(row.event_id.as_str());
        }
    }

    for book in &snapshot.pm_book_snapshots {
        let Some((up_token_id, down_token_id)) = token_ids.get(book.event_id.as_str()) else {
            continue;
        };
        let expected_token_id = if book.side.eq_ignore_ascii_case("up") {
            *up_token_id
        } else if book.side.eq_ignore_ascii_case("down") {
            *down_token_id
        } else {
            return Err(format!(
                "snapshot event {} has unknown Polymarket book side {}",
                book.event_id, book.side
            ));
        };
        if book.token_id != expected_token_id {
            return Err(format!(
                "snapshot event {} has Polymarket token-side mapping mismatch: side {} expects {}, found {}",
                book.event_id, book.side, expected_token_id, book.token_id
            ));
        }
    }

    for event_id in labels.keys().copied() {
        if !chainlink_events.contains(event_id) {
            return Err(format!(
                "snapshot event {event_id} has no fresh Chainlink reference probability"
            ));
        }
        if !baseline && !binance_spot_events.contains(event_id) {
            return Err(format!(
                "snapshot event {event_id} has no finite Binance spot probability context"
            ));
        }
        if !baseline && !binance_lob_events.contains(event_id) {
            return Err(format!(
                "snapshot event {event_id} has no fresh Binance L2 context"
            ));
        }
        if !baseline && !binance_agg_trade_events.contains(event_id) {
            return Err(format!(
                "snapshot event {event_id} has no fresh Binance aggTrade context"
            ));
        }
        if !quote_events.contains(event_id)
            || !up_book_events.contains(event_id)
            || !down_book_events.contains(event_id)
        {
            return Err(format!(
                "snapshot event {event_id} lacks fresh matching nonempty Polymarket UP/DOWN quote/full-depth evidence"
            ));
        }
    }
    Ok(())
}

pub fn validate_prediction_run_inputs(
    mission: &PredictionResearchMission,
    snapshot: &ResearchSnapshot,
) -> Result<(), String> {
    validate_prediction_mission(mission, &current_prediction_policy_snapshot_id())?;
    validate_prediction_snapshot_sources(&snapshot.manifest)?;
    validate_mission_snapshot_binding(mission, &snapshot.manifest)?;
    validate_prediction_snapshot_coverage(snapshot, mission)
}

#[derive(Debug, Clone)]
pub struct ProposalCallOutput {
    /// The assistant's JSON object, before it is trusted or deserialized.
    pub raw_response: String,
    pub provider: String,
    pub model: String,
    pub usage: serde_json::Value,
}

pub trait ProposalClient {
    /// Perform exactly one provider call. Retry policy belongs to the bounded caller.
    fn propose(&mut self, prompt: &str, timeout: Duration) -> Result<ProposalCallOutput, String>;
}

#[derive(Debug, Clone)]
pub struct PredictionEvaluationPrior {
    pub value: LlmPriorSpec,
    pub artifact_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PredictionEvaluationRequest {
    pub mission: PredictionResearchMission,
    pub snapshot_dir: PathBuf,
    pub artifact_dir: PathBuf,
    pub prior: Option<PredictionEvaluationPrior>,
    pub training_candidate_json: Option<PathBuf>,
    pub selected_candidate_json: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum PredictionEvaluationOutcome {
    Success {
        feedback: Option<PredictionResearchFeedback>,
    },
    Failure {
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct PredictionEvaluationOutput {
    pub outcome: PredictionEvaluationOutcome,
    pub stdout: String,
    pub stderr: String,
}

impl PredictionEvaluationOutput {
    pub fn success(
        feedback: Option<PredictionResearchFeedback>,
        stdout: String,
        stderr: String,
    ) -> Self {
        Self {
            outcome: PredictionEvaluationOutcome::Success { feedback },
            stdout,
            stderr,
        }
    }

    pub fn failure(reason: String, stdout: String, stderr: String) -> Self {
        Self {
            outcome: PredictionEvaluationOutcome::Failure { reason },
            stdout,
            stderr,
        }
    }
}

pub trait PredictionEvaluator {
    fn evaluate(
        &mut self,
        request: &PredictionEvaluationRequest,
        timeout: Duration,
    ) -> PredictionEvaluationOutput;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopRunStatus {
    Paused,
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
    pub keep_models: Vec<String>,
    pub reason: Option<String>,
    pub state_path: PathBuf,
}

fn validate_mission_snapshot_binding(
    mission: &PredictionResearchMission,
    manifest: &ResearchSnapshotManifest,
) -> Result<(), String> {
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
    if manifest.symbols.len() != 1
        || manifest
            .symbols
            .iter()
            .any(|symbol| normalized_underlying_symbol(symbol) != requested)
    {
        return Err(format!(
            "prediction LoopRun requires a snapshot isolated to {requested}; found {:?}",
            manifest.symbols
        ));
    }
    Ok(())
}

pub fn prediction_prior_for_blends(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research_snapshot::{
        ResearchSnapshot, ResearchSnapshotArtifacts, ResearchSnapshotInputArtifact,
        ResearchSnapshotPhaseTiming, ResearchSnapshotPmBookSource, ResearchSnapshotRowCounts,
        ResearchSnapshotSourceSurface,
    };
    use chrono::Utc;

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
            time_cohort_boundary_ms: 1_700_001_000_000,
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
            immutable_input: true,
            source_kind: "unit_test".to_string(),
            optimizer_data_dir: Some("unit-test-immutable-source".to_string()),
            source_surfaces: REQUIRED_BINANCE_SOURCE_SURFACES
                .iter()
                .copied()
                .chain([
                    REQUIRED_CHAINLINK_SOURCE_SURFACE,
                    REQUIRED_POLYMARKET_SOURCE_SURFACE,
                    REQUIRED_SETTLEMENT_SOURCE_SURFACE,
                ])
                .map(|name| ResearchSnapshotSourceSurface {
                    name: name.to_string(),
                    role: match name {
                        REQUIRED_CHAINLINK_SOURCE_SURFACE => {
                            "opening_reference_and_expiry_price_source".to_string()
                        }
                        REQUIRED_POLYMARKET_SOURCE_SURFACE => "execution_depth_context".to_string(),
                        REQUIRED_SETTLEMENT_SOURCE_SURFACE => "settlement_labels".to_string(),
                        _ => "binance_prediction_context".to_string(),
                    },
                    gate_category: if name == REQUIRED_POLYMARKET_SOURCE_SURFACE {
                        "required_for_execution".to_string()
                    } else {
                        "required_for_prediction".to_string()
                    },
                    raw_full_fidelity: name == REQUIRED_SETTLEMENT_SOURCE_SURFACE,
                    snapshot_sampled: name != REQUIRED_SETTLEMENT_SOURCE_SURFACE,
                    sample_secs: (name != REQUIRED_SETTLEMENT_SOURCE_SURFACE).then_some(1),
                    row_count: (name != REQUIRED_SETTLEMENT_SOURCE_SURFACE).then_some(10),
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
                pm_book_snapshots: 2,
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

    fn baseline_snapshot() -> ResearchSnapshot {
        let tick_ts = Utc::now();
        let mut manifest = snapshot_manifest();
        manifest.generated_at = tick_ts + chrono::Duration::seconds(61);
        manifest.source_kind = crate::POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND.to_string();
        manifest.data_requirements =
            vec![crate::POLYMARKET_CHAINLINK_BASELINE_REQUIREMENT.to_string()];
        manifest.quality_flags = vec![crate::BINANCE_SURFACES_OMITTED_QUALITY_FLAG.to_string()];
        for surface in &mut manifest.source_surfaces {
            if surface.name.starts_with("binance_") {
                surface.role = "intentionally_omitted".to_string();
                surface.gate_category = "optional_context".to_string();
                surface.snapshot_sampled = false;
                surface.sample_secs = None;
                surface.row_count = Some(0);
            }
        }
        let mut observation = test_prediction_observation(tick_ts);
        observation.binance_spot_fresh = false;
        observation.binance_lob_fresh = false;
        observation.binance_agg_trade_fresh = false;
        crate::research_snapshot::clear_polymarket_chainlink_baseline_unavailable_features(
            &mut observation,
        );
        ResearchSnapshot {
            manifest,
            observations: vec![observation],
            deribit_snapshots: Vec::new(),
            pm_book_snapshots: test_prediction_books(tick_ts),
        }
    }

    fn test_prediction_observation(tick_ts: chrono::DateTime<Utc>) -> crate::FactorObservation {
        crate::FactorObservation {
            event_id: "event-1".to_string(),
            symbol: "BTCUSDT".to_string(),
            tick_ts,
            event_end_ts: None,
            up_token_id: "up-token".to_string(),
            down_token_id: "down-token".to_string(),
            chainlink_reference_fresh: true,
            binance_spot_fresh: true,
            binance_lob_fresh: true,
            binance_agg_trade_fresh: true,
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
            official_resolution_observed_at: Some(tick_ts + chrono::Duration::seconds(60)),
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

    fn test_prediction_books(tick_ts: chrono::DateTime<Utc>) -> Vec<crate::ResearchPmBookSnapshot> {
        [("up-token", "up"), ("down-token", "down")]
            .into_iter()
            .map(|(token_id, side)| crate::ResearchPmBookSnapshot {
                event_id: "event-1".to_string(),
                token_id: token_id.to_string(),
                side: side.to_string(),
                ts: tick_ts,
                bids: vec![crate::ResearchPmBookLevel {
                    price: 0.49,
                    size: 10.0,
                }],
                asks: vec![crate::ResearchPmBookLevel {
                    price: 0.51,
                    size: 10.0,
                }],
            })
            .collect()
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

        let mut missing_boundary = mission.clone();
        missing_boundary.time_cohort_boundary_ms = 0;
        assert!(
            validate_prediction_mission(&missing_boundary, &mission.search_policy_snapshot_id)
                .expect_err("missing shared time boundary must fail")
                .contains("time_cohort_boundary_ms")
        );
    }

    #[test]
    fn prediction_policy_identity_excludes_runtime_and_oms_authority() {
        let paths = prediction_policy_sources()
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>();

        assert!(paths
            .iter()
            .all(|path| !path.contains("operator-contracts")));
        assert!(paths.iter().all(|path| !path.contains("ploy-trading")));
        assert!(!paths.contains(&"Cargo.lock"));
        assert!(!paths.contains(&"Cargo.toml"));
        assert!(!paths.contains(&"crates/ploy-research/Cargo.toml"));
        assert!(paths
            .iter()
            .any(|path| path.contains("monday-prediction-evaluator")));
        assert!(paths
            .iter()
            .any(|path| path.contains("monday-prediction-research")));
        assert!(paths.contains(&"crates/ploy-research/src/polymarket_evidence_projection.rs"));
        assert!(paths.contains(&"crates/ploy-research/src/event_cohort_partition.rs"));
        assert!(paths.contains(&"crates/ploy-research/src/verified_artifact_audit.rs"));
        assert!(paths.contains(&"crates/ploy-research/src/verified_binance_projection.rs"));
        assert!(paths.contains(&"crates/ploy-research/src/prediction_mcts.rs"));
        assert!(paths.contains(&"crates/ploy-research/src/prediction_mcts_run.rs"));
        assert!(paths.contains(&"crates/ploy-research/src/prediction_mcts_authenticated.rs"));
        for path in [
            "crates/ploy-market-data/src/diagnostics.rs",
            "crates/ploy-market-data/src/polymarket_evidence/mod.rs",
            "crates/ploy-market-data/src/polymarket_evidence/artifact.rs",
            "crates/ploy-market-data/src/polymarket_evidence/wire.rs",
            "crates/ploy-market-data/src/polymarket_evidence/verified.rs",
            "crates/ploy-market-data/src/polymarket_evidence/set.rs",
            "crates/ploy-market-data/src/polymarket_evidence/catalog.rs",
        ] {
            assert!(paths.contains(&path));
        }
    }

    #[test]
    fn baseline_only_budget_requires_both_search_counts_zero_and_positive_time() {
        let mission = mission();

        let mut zero_candidates_only = mission.clone();
        zero_candidates_only.search_budget.max_candidates = 0;
        assert!(validate_prediction_mission(
            &zero_candidates_only,
            &zero_candidates_only.search_policy_snapshot_id
        )
        .expect_err("0/N budget must fail")
        .contains("both be zero"));

        let mut zero_calls_only = mission.clone();
        zero_calls_only.search_budget.max_llm_calls = 0;
        assert!(validate_prediction_mission(
            &zero_calls_only,
            &zero_calls_only.search_policy_snapshot_id
        )
        .expect_err("N/0 budget must fail")
        .contains("both be zero"));

        let mut zero_everything = mission;
        zero_everything.search_budget.max_candidates = 0;
        zero_everything.search_budget.max_llm_calls = 0;
        zero_everything.search_budget.max_seconds = 0;
        assert!(validate_prediction_mission(
            &zero_everything,
            &zero_everything.search_policy_snapshot_id
        )
        .expect_err("0/0/0 budget must fail")
        .contains("max_seconds"));
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
            .contains("official resolution"));
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
        validate_prediction_snapshot_sources(&manifest)
            .expect("complete prediction-source contract");

        let mut missing_lob = manifest.clone();
        missing_lob
            .source_surfaces
            .retain(|surface| surface.name != "binance_lob_ticks");
        assert!(validate_prediction_snapshot_sources(&missing_lob)
            .expect_err("missing LOB must fail")
            .contains("binance_lob_ticks"));

        let mut missing_chainlink = manifest.clone();
        missing_chainlink
            .source_surfaces
            .retain(|surface| surface.name != REQUIRED_CHAINLINK_SOURCE_SURFACE);
        assert!(validate_prediction_snapshot_sources(&missing_chainlink)
            .expect_err("missing Chainlink authority must fail")
            .contains(REQUIRED_CHAINLINK_SOURCE_SURFACE));

        let mut missing_clob = manifest.clone();
        missing_clob
            .source_surfaces
            .retain(|surface| surface.name != REQUIRED_POLYMARKET_SOURCE_SURFACE);
        assert!(validate_prediction_snapshot_sources(&missing_clob)
            .expect_err("missing Polymarket execution depth must fail")
            .contains(REQUIRED_POLYMARKET_SOURCE_SURFACE));

        let mut missing_settlement = manifest.clone();
        missing_settlement
            .source_surfaces
            .retain(|surface| surface.name != REQUIRED_SETTLEMENT_SOURCE_SURFACE);
        assert!(validate_prediction_snapshot_sources(&missing_settlement)
            .expect_err("missing official outcome surface must fail")
            .contains(REQUIRED_SETTLEMENT_SOURCE_SURFACE));

        let mut zero_agg_trade = manifest.clone();
        zero_agg_trade
            .source_surfaces
            .iter_mut()
            .find(|surface| surface.name == "binance_agg_trade_ticks")
            .expect("agg-trade surface")
            .row_count = Some(0);
        assert!(validate_prediction_snapshot_sources(&zero_agg_trade)
            .expect_err("zero agg-trade rows must fail")
            .contains("binance_agg_trade_ticks"));

        let mut unaudited = manifest;
        unaudited.data_audit_status = Some("critical".to_string());
        assert!(validate_prediction_snapshot_sources(&unaudited)
            .expect_err("critical audit must fail")
            .contains("data_audit_status"));
    }

    #[test]
    fn polymarket_chainlink_baseline_requires_explicit_omissions_and_nan_cex_features() {
        let snapshot = baseline_snapshot();
        validate_prediction_snapshot_sources(&snapshot.manifest)
            .expect("explicit reduced-authority source contract");
        validate_prediction_snapshot_coverage(&snapshot, &mission())
            .expect("baseline coverage with independent token books");

        let mut disguised_cex = snapshot.clone();
        disguised_cex.observations[0].model_prob_up = 0.5;
        assert!(
            validate_prediction_snapshot_coverage(&disguised_cex, &mission())
                .expect_err("finite CEX placeholder must fail before scoring")
                .contains("unavailable CEX-derived")
        );

        let mut sampled_omission = snapshot;
        let binance = sampled_omission
            .manifest
            .source_surfaces
            .iter_mut()
            .find(|surface| surface.name == "binance_price_ticks")
            .expect("baseline Binance surface");
        binance.snapshot_sampled = true;
        assert!(
            validate_prediction_snapshot_sources(&sampled_omission.manifest)
                .expect_err("malformed omission marker must fail")
                .contains("intentionally omitted")
        );
    }

    #[test]
    fn prediction_snapshot_coverage_is_symbol_and_event_scoped() {
        let tick_ts = Utc::now();
        let observation = test_prediction_observation(tick_ts);
        let mut manifest = snapshot_manifest();
        manifest.generated_at = tick_ts + chrono::Duration::seconds(61);
        let snapshot = ResearchSnapshot {
            manifest,
            observations: vec![observation.clone()],
            deribit_snapshots: Vec::new(),
            pm_book_snapshots: test_prediction_books(tick_ts),
        };
        validate_prediction_snapshot_coverage(&snapshot, &mission())
            .expect("event-scoped authority coverage");

        let mut missing_resolution_clock = snapshot.clone();
        missing_resolution_clock.observations[0].official_resolution_observed_at = None;
        assert!(
            validate_prediction_snapshot_coverage(&missing_resolution_clock, &mission())
                .expect_err("missing official label availability must fail")
                .contains("official resolution observation clock")
        );

        let mut future_resolution_clock = snapshot.clone();
        future_resolution_clock.observations[0].official_resolution_observed_at =
            Some(future_resolution_clock.manifest.generated_at + chrono::Duration::milliseconds(1));
        assert!(
            validate_prediction_snapshot_coverage(&future_resolution_clock, &mission())
                .expect_err("a label observed after snapshot generation must fail")
                .contains("invalid official resolution observation clock")
        );

        let mut wrong_symbol = snapshot.clone();
        wrong_symbol.observations[0].symbol = "SOLUSDT".to_string();
        assert!(
            validate_prediction_snapshot_coverage(&wrong_symbol, &mission())
                .expect_err("another underlying must not borrow BTC coverage")
                .contains("not isolated")
        );

        let mut stale_chainlink = snapshot.clone();
        stale_chainlink.observations[0].chainlink_prob_up = f64::NAN;
        assert!(
            validate_prediction_snapshot_coverage(&stale_chainlink, &mission())
                .expect_err("event without Chainlink coverage must fail")
                .contains("Chainlink")
        );

        let mut missing_lob = snapshot.clone();
        missing_lob.observations[0].binance_lob_fresh = false;
        assert!(
            validate_prediction_snapshot_coverage(&missing_lob, &mission())
                .expect_err("event without fresh Binance L2 must fail")
                .contains("Binance L2")
        );

        let mut missing_agg_trade = snapshot.clone();
        missing_agg_trade.observations[0].binance_agg_trade_fresh = false;
        assert!(
            validate_prediction_snapshot_coverage(&missing_agg_trade, &mission())
                .expect_err("event without fresh Binance aggTrade must fail")
                .contains("aggTrade")
        );

        let mut mixed_freshness = snapshot.clone();
        let mut stale_row = observation.clone();
        stale_row.tick_ts += chrono::Duration::seconds(1);
        stale_row.time_remaining_secs -= 1;
        stale_row.binance_lob_fresh = false;
        mixed_freshness.observations.push(stale_row);
        assert!(
            validate_prediction_snapshot_coverage(&mixed_freshness, &mission())
                .expect_err("one fresh row must not mask stale evaluator input")
                .contains("Binance L2")
        );

        let mut empty_book = snapshot.clone();
        empty_book.pm_book_snapshots[0].asks.clear();
        assert!(
            validate_prediction_snapshot_coverage(&empty_book, &mission())
                .expect_err("empty UP depth must fail")
                .contains("full-depth")
        );

        let mut stale_books = snapshot.clone();
        for book in &mut stale_books.pm_book_snapshots {
            book.ts = tick_ts - chrono::Duration::seconds(31);
        }
        assert!(
            validate_prediction_snapshot_coverage(&stale_books, &mission())
                .expect_err("stale PM books must fail")
                .contains("full-depth")
        );

        let mut wrong_token = snapshot.clone();
        wrong_token.pm_book_snapshots[0].token_id = "down-token".to_string();
        assert!(
            validate_prediction_snapshot_coverage(&wrong_token, &mission())
                .expect_err("UP book bound to wrong token must fail")
                .contains("token-side mapping")
        );

        let mut wrong_side = snapshot.clone();
        wrong_side.pm_book_snapshots[0].side = "down".to_string();
        assert!(
            validate_prediction_snapshot_coverage(&wrong_side, &mission())
                .expect_err("UP token book marked DOWN must fail")
                .contains("token-side mapping")
        );

        let mut extra_wrong_token = snapshot.clone();
        let mut wrong_book = extra_wrong_token.pm_book_snapshots[0].clone();
        wrong_book.ts = tick_ts + chrono::Duration::milliseconds(1);
        wrong_book.token_id = "other-token".to_string();
        extra_wrong_token.pm_book_snapshots.push(wrong_book);
        assert!(
            validate_prediction_snapshot_coverage(&extra_wrong_token, &mission())
                .expect_err("an extra newer wrong-token book must fail")
                .contains("token-side mapping")
        );

        let mut no_book = snapshot.clone();
        no_book.pm_book_snapshots.clear();
        assert!(validate_prediction_snapshot_coverage(&no_book, &mission())
            .expect_err("event without matching depth must fail")
            .contains("full-depth"));

        let mut mixed_manifest = snapshot_manifest();
        mixed_manifest.symbols.push("SOLUSDT".to_string());
        assert!(
            validate_mission_snapshot_binding(&mission(), &mixed_manifest)
                .expect_err("mixed snapshot must fail isolated mission binding")
                .contains("isolated")
        );

        let mut duplicate_underlying = snapshot_manifest();
        duplicate_underlying.symbols.push("BTCUSDC".to_string());
        assert!(
            validate_mission_snapshot_binding(&mission(), &duplicate_underlying)
                .expect_err("multiple CEX symbols must not be silently subsetted")
                .contains("isolated")
        );
    }
}
