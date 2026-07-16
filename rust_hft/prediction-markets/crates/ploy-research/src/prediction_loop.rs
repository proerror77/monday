//! Rust-owned, bounded research loop for one BTC or SOL prediction-market mission.
//!
//! This module owns mission validation, proposal budgets, resume state, and
//! deterministic evaluator feedback.  It has no execution or live-trading
//! authority.  Continuous-contract research remains in Monday's separate
//! `alpha-harness` workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::prediction_loop_fs::{
    atomic_write_json, canonical_json_bytes, cleanup_stale_temporary_files, next_attempt_dir,
    read_json, relative_path, sha256_hex, verify_artifact, write_content_addressed_json,
    write_content_addressed_text, ArtifactRef, OutputLock,
};
use crate::{
    load_research_snapshot, normalized_underlying_symbol, validate_prediction_research_prior,
    LlmPriorSpec, LlmProbabilityBlendSpec, PredictionResearchFeedback, ResearchSnapshot,
    ResearchSnapshotManifest,
};

pub const PREDICTION_MISSION_SCHEMA_VERSION: &str = "prediction_research_mission.v1";
pub const PREDICTION_LOOP_TARGET: &str = "full_depth_settlement_executable_pnl";
pub const PREDICTION_EVENT_WINDOW_SECS: i64 = 300;
pub const PREDICTION_LOOP_STATE_SCHEMA_VERSION: &str = "ploy_prediction_research_loop.v3";
const PROBABILITY_WEIGHT_EPSILON: f64 = 1e-9;
const MAX_GOVERNED_CANDIDATES: usize = 64;
const MAX_GOVERNED_LLM_CALLS: usize = 16;
const MAX_GOVERNED_SECONDS: u64 = 86_400;
const LLM_LEDGER_EVENT_SCHEMA_VERSION: &str = "prediction_llm_ledger_event.v1";
const TERMINAL_EVIDENCE_SCHEMA_VERSION: &str = "prediction_loop_terminal.v3";
const RUN_BUDGET_EVIDENCE_SCHEMA_VERSION: &str = "prediction_loop_run_budget.v1";
const ITERATION_COMPLETION_SCHEMA_VERSION: &str = "prediction_iteration_completion.v1";
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
    format!("sha256:{:x}", digest.finalize())
}

fn prediction_policy_sources() -> [(&'static str, &'static [u8]); 27] {
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
            "crates/ploy-research/bin/monday-prediction-evaluator.rs",
            include_bytes!("../examples/factor_walk_forward_v2.rs"),
        ),
        (
            "crates/ploy-research/bin/monday-prediction-research.rs",
            include_bytes!("../examples/prediction_research_loop.rs"),
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
        if !row.chainlink_reference_fresh
            || !row.chainlink_prob_up.is_finite()
            || !(0.0..=1.0).contains(&row.chainlink_prob_up)
        {
            return Err(format!(
                "snapshot observation {} at {} lacks fresh Chainlink reference probability",
                row.event_id, row.tick_ts
            ));
        }
        chainlink_events.insert(row.event_id.as_str());
        if !row.binance_spot_fresh
            || !row.model_prob_up.is_finite()
            || !(0.0..=1.0).contains(&row.model_prob_up)
        {
            return Err(format!(
                "snapshot observation {} at {} lacks fresh Binance spot probability context",
                row.event_id, row.tick_ts
            ));
        }
        binance_spot_events.insert(row.event_id.as_str());
        if !row.binance_lob_fresh
            || !row.obi.is_finite()
            || !row.spread_bps.is_finite()
            || !row.bid_depth_near.is_finite()
            || !row.ask_depth_near.is_finite()
            || row.bid_depth_near < 0.0
            || row.ask_depth_near < 0.0
            || row.bid_depth_near + row.ask_depth_near <= 0.0
        {
            return Err(format!(
                "snapshot observation {} at {} lacks fresh Binance L2 context",
                row.event_id, row.tick_ts
            ));
        }
        binance_lob_events.insert(row.event_id.as_str());
        if !row.binance_agg_trade_fresh || !row.cum_trade_imbalance_5m.is_finite() {
            return Err(format!(
                "snapshot observation {} at {} lacks fresh Binance aggTrade context",
                row.event_id, row.tick_ts
            ));
        }
        binance_agg_trade_events.insert(row.event_id.as_str());
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
        if !binance_spot_events.contains(event_id) {
            return Err(format!(
                "snapshot event {event_id} has no finite Binance spot probability context"
            ));
        }
        if !binance_lob_events.contains(event_id) {
            return Err(format!(
                "snapshot event {event_id} has no fresh Binance L2 context"
            ));
        }
        if !binance_agg_trade_events.contains(event_id) {
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

#[derive(Debug, Clone)]
pub struct ProposalCallOutput {
    /// The assistant's JSON object, before it is trusted or deserialized.
    pub raw_response: String,
    pub provider: String,
    pub model: String,
    pub usage: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmResponseEvidence {
    raw_response: String,
    provider: String,
    model: String,
    usage: serde_json::Value,
}

pub trait ProposalClient {
    /// Perform exactly one provider call. Retries belong to the durable loop ledger.
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

    fn feedback(&self) -> Option<&PredictionResearchFeedback> {
        match &self.outcome {
            PredictionEvaluationOutcome::Success { feedback } => feedback.as_ref(),
            PredictionEvaluationOutcome::Failure { .. } => None,
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
    Running,
    Paused,
    Kept,
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
#[serde(deny_unknown_fields)]
struct RunBudgetEvidence {
    schema_version: String,
    identity: RunIdentity,
    started_unix_millis: u64,
    deadline_unix_millis: u64,
    max_seconds: u64,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmLedgerCursor {
    event_count: usize,
    head_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmLedgerEvent {
    schema_version: String,
    sequence: usize,
    previous_sha256: Option<String>,
    identity: RunIdentity,
    attempt: LlmAttemptRecord,
    frontier_after: Frontier,
    failure: Option<FailureRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct EvaluationEvidence {
    stdout: ArtifactRef,
    stderr: ArtifactRef,
    feedback: Option<ArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CandidateOutcome {
    model: String,
    verdict: String,
    reason_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct IterationRecord {
    iteration: usize,
    call_index: usize,
    prior: ArtifactRef,
    evaluation: EvaluationEvidence,
    outcomes: Vec<CandidateOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "next", rename_all = "snake_case")]
enum IterationCompletionNext {
    Propose,
    Keep { keep_models: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IterationCompletionEvidence {
    schema_version: String,
    sequence: usize,
    previous_sha256: Option<String>,
    identity: RunIdentity,
    accepted_ledger_sha256: String,
    pending: PendingIteration,
    record: IterationRecord,
    next: IterationCompletionNext,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PendingIteration {
    iteration: usize,
    call_index: usize,
    prior: ArtifactRef,
    candidate_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FailureRecord {
    stage: String,
    reason: String,
    stdout: Option<ArtifactRef>,
    stderr: Option<ArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TerminalOutcome {
    status: LoopRunStatusWire,
    reason: String,
    keep_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalDecisionEvidence {
    schema_version: String,
    identity: RunIdentity,
    outcome: TerminalOutcome,
    iterations: Vec<IterationRecord>,
    cancelled_pending: Option<CancelledPendingEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelledPendingEvidence {
    accepted_ledger_sha256: String,
    pending: PendingIteration,
    evaluation: EvaluationEvidence,
    reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LoopRunStatusWire {
    Kept,
    BudgetExhausted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    run_budget_evidence: Option<ArtifactRef>,
    budget_deadline_unix_millis: u64,
    baseline: Option<EvaluationEvidence>,
    llm_ledger: LlmLedgerCursor,
    llm_attempts: Vec<LlmAttemptRecord>,
    iterations: Vec<IterationRecord>,
    failures: Vec<FailureRecord>,
    terminal_evidence: Option<ArtifactRef>,
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

    fn keep_models(&self) -> Vec<String> {
        match &self.frontier {
            Frontier::Done { outcome } => outcome.keep_models.clone(),
            _ => Vec::new(),
        }
    }
}

/// Run a bounded mission until deterministic keep, budget exhaustion, or a
/// retryable pause. Calling this again with the same inputs resumes from the
/// durable frontier; an evaluator pause never spends another LLM call.
pub fn run_or_resume<C: ProposalClient, E: PredictionEvaluator>(
    mission: PredictionResearchMission,
    snapshot_dir: &Path,
    output_dir: &Path,
    client: &mut C,
    evaluator: &mut E,
) -> Result<LoopRunSummary, String> {
    let _lock = OutputLock::acquire(output_dir)?;
    cleanup_stale_temporary_files(output_dir)?;
    let state_path = output_dir.join("prediction-loop-state.json");
    reject_unbound_output_directory(output_dir, &state_path)?;

    let current_policy = current_prediction_policy_snapshot_id();
    validate_prediction_mission(&mission, &current_policy)?;
    let snapshot = load_research_snapshot(snapshot_dir)
        .map_err(|error| format!("load governed research snapshot: {error:#}"))?;
    validate_prediction_snapshot_sources(&snapshot.manifest)?;
    validate_mission_snapshot_binding(&mission, &snapshot.manifest)?;
    validate_prediction_snapshot_coverage(&snapshot, &mission)?;

    let identity = run_identity(&mission, &snapshot.manifest)?;
    let (mut state, new_state) = if state_path.exists() {
        let state: PredictionLoopState = read_json(&state_path)?;
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
        (state, false)
    } else {
        let started_unix_millis = now_unix_millis();
        let budget_millis = mission
            .search_budget
            .max_seconds
            .checked_mul(1_000)
            .ok_or_else(|| "run budget duration overflow".to_string())?;
        let budget_deadline_unix_millis = started_unix_millis
            .checked_add(budget_millis)
            .ok_or_else(|| "run budget deadline overflow".to_string())?;
        (
            PredictionLoopState {
                schema_version: PREDICTION_LOOP_STATE_SCHEMA_VERSION.to_string(),
                identity,
                elapsed_millis: 0,
                run_budget_evidence: None,
                budget_deadline_unix_millis,
                baseline: None,
                llm_ledger: LlmLedgerCursor::default(),
                llm_attempts: Vec::new(),
                iterations: Vec::new(),
                failures: Vec::new(),
                terminal_evidence: None,
                frontier: Frontier::Baseline,
                pause: None,
            },
            true,
        )
    };

    let base_elapsed_millis = state.elapsed_millis;
    let run_started = Instant::now();
    if new_state {
        checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
    }
    let mut recovered_durable_evidence =
        reconcile_run_budget_evidence(&mut state, &mission, output_dir)?;
    recovered_durable_evidence |= verify_state(&mut state, &mission, output_dir)?;
    if recovered_durable_evidence {
        checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
    }
    if recover_interrupted_llm_attempt(&mut state, output_dir)? {
        checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
    }
    if state.pause.take().is_some() {
        checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
    }

    loop {
        if let Some(summary) = budget_terminal_if_exhausted(
            &mission,
            &mut state,
            output_dir,
            &state_path,
            base_elapsed_millis,
            run_started,
        )? {
            return Ok(summary);
        }

        match state.frontier.clone() {
            Frontier::Baseline => {
                let attempt_dir = next_attempt_dir(&output_dir.join("baseline"))?;
                let request = PredictionEvaluationRequest {
                    mission: mission.clone(),
                    snapshot_dir: snapshot_dir.to_path_buf(),
                    artifact_dir: attempt_dir.clone(),
                    prior: None,
                };
                let output = evaluator.evaluate(&request, remaining_time(&mission, &state));
                let evidence = persist_evaluation_output(
                    output_dir,
                    &attempt_dir,
                    output.feedback(),
                    &output.stdout,
                    &output.stderr,
                )?;
                match output.outcome {
                    PredictionEvaluationOutcome::Success { feedback: None } => {}
                    PredictionEvaluationOutcome::Success { feedback: Some(_) } => {
                        return Err(
                            "baseline evaluator may not emit candidate feedback".to_string()
                        );
                    }
                    PredictionEvaluationOutcome::Failure { reason } => {
                        let failure = FailureRecord {
                            stage: "baseline".to_string(),
                            reason,
                            stdout: Some(evidence.stdout),
                            stderr: Some(evidence.stderr),
                        };
                        state.failures.push(failure.clone());
                        state.pause = Some(failure);
                        checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
                        return Ok(summary_from_state(&mission, &state, &state_path));
                    }
                }
                state.baseline = Some(evidence);
                state.frontier = Frontier::Propose;
                checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
            }
            Frontier::Propose => {
                if !has_unprocessed_response(&state) {
                    let call_index = state.calls_used() + 1;
                    let attempt_dir = output_dir
                        .join("llm-calls")
                        .join(format!("call-{call_index:03}"));
                    fs::create_dir_all(&attempt_dir).map_err(|error| {
                        format!(
                            "create LLM attempt directory {}: {error}",
                            attempt_dir.display()
                        )
                    })?;
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
                        output_dir,
                        &attempt_dir,
                        "llm-prompt",
                        &prompt,
                    )?;
                    let attempt = LlmAttemptRecord {
                        call_index,
                        prompt: prompt_ref,
                        response: None,
                        provider: None,
                        model: None,
                        usage: None,
                        status: LlmAttemptStatus::Started,
                        error: None,
                    };
                    append_llm_transition(
                        output_dir,
                        &mut state,
                        attempt,
                        Frontier::Propose,
                        None,
                    )?;
                    // The call budget is consumed durably before any network I/O.
                    checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
                    let timeout = remaining_time(&mission, &state);
                    match client.propose(&prompt, timeout) {
                        Ok(response) => {
                            let response_evidence = LlmResponseEvidence {
                                raw_response: response.raw_response,
                                provider: response.provider,
                                model: response.model,
                                usage: response.usage,
                            };
                            let response_ref = write_content_addressed_json(
                                output_dir,
                                &attempt_dir,
                                "llm-response",
                                &response_evidence,
                            )?;
                            let mut attempt = state
                                .llm_attempts
                                .last_mut()
                                .expect("attempt was just recorded")
                                .clone();
                            attempt.response = Some(response_ref);
                            attempt.provider = Some(response_evidence.provider);
                            attempt.model = Some(response_evidence.model);
                            attempt.usage = Some(response_evidence.usage);
                            attempt.status = LlmAttemptStatus::Responded;
                            append_llm_transition(
                                output_dir,
                                &mut state,
                                attempt,
                                Frontier::Propose,
                                None,
                            )?;
                            checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
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
                            let mut attempt = state
                                .llm_attempts
                                .last_mut()
                                .expect("attempt was just recorded")
                                .clone();
                            attempt.status = LlmAttemptStatus::ProviderFailed;
                            attempt.error = Some(reason);
                            append_llm_transition(
                                output_dir,
                                &mut state,
                                attempt,
                                Frontier::Propose,
                                Some(failure),
                            )?;
                            checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
                            return Ok(summary_from_state(&mission, &state, &state_path));
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
                let response_path = verify_artifact(output_dir, &response_ref)?;
                let response_evidence: LlmResponseEvidence = read_json(&response_path)?;
                if state.llm_attempts[attempt_index].provider.as_deref()
                    != Some(response_evidence.provider.as_str())
                    || state.llm_attempts[attempt_index].model.as_deref()
                        != Some(response_evidence.model.as_str())
                    || state.llm_attempts[attempt_index].usage.as_ref()
                        != Some(&response_evidence.usage)
                {
                    return Err(
                        "LLM response envelope differs from durable attempt metadata".to_string(),
                    );
                }
                let remaining_candidates = mission
                    .search_budget
                    .max_candidates
                    .saturating_sub(state.candidates_evaluated());
                let blends =
                    serde_json::from_str::<PredictionProposal>(&response_evidence.raw_response)
                        .map_err(|error| format!("strict proposal JSON rejected: {error}"))
                        .and_then(|proposal| {
                            validate_prediction_proposal(proposal, remaining_candidates)
                        });
                let blends = match blends {
                    Ok(blends) => blends,
                    Err(reason) => {
                        let mut attempt = state.llm_attempts[attempt_index].clone();
                        attempt.status = LlmAttemptStatus::Invalid;
                        attempt.error = Some(reason);
                        append_llm_transition(
                            output_dir,
                            &mut state,
                            attempt,
                            Frontier::Propose,
                            None,
                        )?;
                        checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
                        continue;
                    }
                };
                let prior = mission_prior(&mission, blends);
                validate_prediction_research_prior(&prior).map_err(|reason| {
                    format!("Rust evaluator rejected proposal prior: {reason}")
                })?;
                let iteration = state.iterations.len() + 1;
                let iteration_dir = output_dir
                    .join("iterations")
                    .join(format!("iteration-{iteration:03}"));
                let prior_ref = write_content_addressed_json(
                    output_dir,
                    &iteration_dir,
                    "prediction-prior",
                    &prior,
                )?;
                let call_index = state.llm_attempts[attempt_index].call_index;
                let pending = PendingIteration {
                    iteration,
                    call_index,
                    prior: prior_ref,
                    candidate_count: prior.probability_blends.len(),
                };
                let mut attempt = state.llm_attempts[attempt_index].clone();
                attempt.status = LlmAttemptStatus::Accepted;
                append_llm_transition(
                    output_dir,
                    &mut state,
                    attempt,
                    Frontier::Evaluate { pending },
                    None,
                )?;
                checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
            }
            Frontier::Evaluate { pending } => {
                let prior_path = verify_artifact(output_dir, &pending.prior)?;
                let prior: LlmPriorSpec = read_json(&prior_path)?;
                validate_pending_prior(&mission, &pending, &prior)?;
                let evaluator_parent = output_dir
                    .join("iterations")
                    .join(format!("iteration-{:03}", pending.iteration))
                    .join("evaluator-attempts");
                let (evidence, feedback) = if let Some(recovered) =
                    recover_completed_evaluation(output_dir, &evaluator_parent)?
                {
                    recovered
                } else {
                    let attempt_dir = next_attempt_dir(&evaluator_parent)?;
                    let request = PredictionEvaluationRequest {
                        mission: mission.clone(),
                        snapshot_dir: snapshot_dir.to_path_buf(),
                        artifact_dir: attempt_dir.clone(),
                        prior: Some(PredictionEvaluationPrior {
                            value: prior.clone(),
                            artifact_path: prior_path,
                        }),
                    };
                    let output = evaluator.evaluate(&request, remaining_time(&mission, &state));
                    let evidence = persist_evaluation_output(
                        output_dir,
                        &attempt_dir,
                        output.feedback(),
                        &output.stdout,
                        &output.stderr,
                    )?;
                    match output.outcome {
                        PredictionEvaluationOutcome::Success {
                            feedback: Some(feedback),
                        } => (evidence, feedback),
                        PredictionEvaluationOutcome::Success { feedback: None } => {
                            return Err(
                                "successful candidate evaluator emitted no deterministic feedback"
                                    .to_string(),
                            );
                        }
                        PredictionEvaluationOutcome::Failure { reason } => {
                            let failure = FailureRecord {
                                stage: "evaluate".to_string(),
                                reason,
                                stdout: Some(evidence.stdout),
                                stderr: Some(evidence.stderr),
                            };
                            state.failures.push(failure.clone());
                            state.pause = Some(failure);
                            checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
                            return Ok(summary_from_state(&mission, &state, &state_path));
                        }
                    }
                };
                let outcomes = match validate_feedback(&mission, &prior, &feedback) {
                    Ok(outcomes) => outcomes,
                    Err(reason) => {
                        let terminal_reason = format!("invalid evaluator feedback: {reason}");
                        let accepted_ledger_sha256 =
                            state.llm_ledger.head_sha256.clone().ok_or_else(|| {
                                "invalid feedback is missing Accepted ledger evidence".to_string()
                            })?;
                        record_terminal_decision_with_cancelled(
                            output_dir,
                            &mut state,
                            TerminalOutcome {
                                status: LoopRunStatusWire::Failed,
                                reason: terminal_reason.clone(),
                                keep_models: Vec::new(),
                            },
                            Some(CancelledPendingEvidence {
                                accepted_ledger_sha256,
                                pending: pending.clone(),
                                evaluation: evidence,
                                reason: terminal_reason,
                            }),
                        )?;
                        checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
                        return Ok(summary_from_state(&mission, &state, &state_path));
                    }
                };
                let keep_models = outcomes
                    .iter()
                    .filter(|outcome| outcome.verdict == "keep")
                    .map(|outcome| outcome.model.clone())
                    .collect::<Vec<_>>();
                let record = IterationRecord {
                    iteration: pending.iteration,
                    call_index: pending.call_index,
                    prior: pending.prior.clone(),
                    evaluation: evidence,
                    outcomes,
                };
                let next = if keep_models.is_empty() {
                    IterationCompletionNext::Propose
                } else {
                    IterationCompletionNext::Keep {
                        keep_models: keep_models.clone(),
                    }
                };
                persist_iteration_completion(output_dir, &state, &pending, &record, next)?;
                state.iterations.push(record);
                // The evaluator may ignore its timeout or return at the exact
                // deadline. Persist its deterministic result for audit, but
                // never promote a keep after the immutable run deadline.
                if let Some(summary) = budget_terminal_if_exhausted(
                    &mission,
                    &mut state,
                    output_dir,
                    &state_path,
                    base_elapsed_millis,
                    run_started,
                )? {
                    return Ok(summary);
                }
                if keep_models.is_empty() {
                    state.frontier = Frontier::Propose;
                } else {
                    record_terminal_decision(
                        output_dir,
                        &mut state,
                        TerminalOutcome {
                            status: LoopRunStatusWire::Kept,
                            reason: "Rust evaluator produced deterministic keep".to_string(),
                            keep_models,
                        },
                    )?;
                }
                checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
            }
            Frontier::Done { .. } => {
                checkpoint(&mut state, &state_path, base_elapsed_millis, run_started)?;
                return Ok(summary_from_state(&mission, &state, &state_path));
            }
        }
    }
}

fn reject_unbound_output_directory(output_dir: &Path, state_path: &Path) -> Result<(), String> {
    if state_path.exists() {
        return Ok(());
    }
    let mut unexpected = Vec::new();
    for entry in fs::read_dir(output_dir)
        .map_err(|error| format!("read output directory {}: {error}", output_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read output entry: {error}"))?;
        if entry.file_name() != ".prediction-research-loop.lock" {
            unexpected.push(entry.path());
        }
    }
    if unexpected.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "refusing to bind a new LoopRun to non-empty output directory {}; first unexpected path is {}",
            output_dir.display(),
            unexpected[0].display()
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

#[cfg(test)]
std::thread_local! {
    static TEST_NOW_UNIX_MILLIS: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
struct TestClockGuard {
    previous: Option<u64>,
}

#[cfg(test)]
impl TestClockGuard {
    fn freeze(now_unix_millis: u64) -> Self {
        let previous = TEST_NOW_UNIX_MILLIS.with(|clock| clock.replace(Some(now_unix_millis)));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for TestClockGuard {
    fn drop(&mut self) {
        TEST_NOW_UNIX_MILLIS.with(|clock| clock.set(self.previous));
    }
}

#[cfg(test)]
fn advance_test_clock(delta_millis: u64) {
    TEST_NOW_UNIX_MILLIS.with(|clock| {
        let now = clock
            .get()
            .expect("test clock must be frozen before advance");
        clock.set(Some(now.saturating_add(delta_millis)));
    });
}

fn now_unix_millis() -> u64 {
    #[cfg(test)]
    if let Some(now) = TEST_NOW_UNIX_MILLIS.with(std::cell::Cell::get) {
        return now;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn load_run_budget_evidence(
    output_root: &Path,
    identity: &RunIdentity,
) -> Result<Option<(ArtifactRef, RunBudgetEvidence)>, String> {
    let directory = output_root.join("run-budget");
    if !directory.exists() {
        return Ok(None);
    }
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| format!("read run budget directory {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read run budget entry: {error}"))?;
    if entries.len() > 1 {
        return Err("prediction LoopRun has multiple immutable budget records".to_string());
    }
    let Some(entry) = entries.pop() else {
        return Ok(None);
    };
    if !entry
        .file_type()
        .map_err(|error| format!("inspect run budget evidence: {error}"))?
        .is_file()
    {
        return Err(format!(
            "run budget evidence is not a file: {}",
            entry.path().display()
        ));
    }
    let path = entry.path();
    let artifact = existing_artifact_ref(output_root, &path)?;
    let expected_name = format!("run-budget-{}.json", artifact.sha256);
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err("run budget evidence is not correctly content-addressed".to_string());
    }
    let evidence: RunBudgetEvidence = read_json(&path)?;
    if evidence.schema_version != RUN_BUDGET_EVIDENCE_SCHEMA_VERSION
        || &evidence.identity != identity
    {
        return Err("run budget evidence belongs to another schema or LoopRun".to_string());
    }
    Ok(Some((artifact, evidence)))
}

fn reconcile_run_budget_evidence(
    state: &mut PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &Path,
) -> Result<bool, String> {
    let durable = load_run_budget_evidence(output_root, &state.identity)?;
    let (artifact, evidence) = match durable {
        Some(durable) => durable,
        None => {
            if state.run_budget_evidence.is_some() {
                return Err(
                    "loop state references missing immutable run budget evidence".to_string(),
                );
            }
            let max_millis = mission
                .search_budget
                .max_seconds
                .checked_mul(1_000)
                .ok_or_else(|| "run budget duration overflow".to_string())?;
            let deadline_unix_millis = if state.budget_deadline_unix_millis == 0 {
                now_unix_millis()
                    .checked_add(max_millis)
                    .ok_or_else(|| "run budget deadline overflow".to_string())?
            } else {
                state.budget_deadline_unix_millis
            };
            let started_unix_millis = deadline_unix_millis
                .checked_sub(max_millis)
                .ok_or_else(|| "run budget start underflow".to_string())?;
            let evidence = RunBudgetEvidence {
                schema_version: RUN_BUDGET_EVIDENCE_SCHEMA_VERSION.to_string(),
                identity: state.identity.clone(),
                started_unix_millis,
                deadline_unix_millis,
                max_seconds: mission.search_budget.max_seconds,
            };
            let artifact = write_content_addressed_json(
                output_root,
                &output_root.join("run-budget"),
                "run-budget",
                &evidence,
            )?;
            (artifact, evidence)
        }
    };
    let expected_deadline = evidence
        .started_unix_millis
        .checked_add(evidence.max_seconds.saturating_mul(1_000))
        .ok_or_else(|| "run budget deadline overflow".to_string())?;
    if evidence.max_seconds != mission.search_budget.max_seconds
        || evidence.deadline_unix_millis != expected_deadline
    {
        return Err("immutable run budget differs from governed mission budget".to_string());
    }
    if state
        .run_budget_evidence
        .as_ref()
        .is_some_and(|bound| bound != &artifact)
        || (state.budget_deadline_unix_millis != 0
            && state.budget_deadline_unix_millis != evidence.deadline_unix_millis)
    {
        return Err("loop state run budget differs from immutable deadline".to_string());
    }
    let recovered = state.run_budget_evidence.as_ref() != Some(&artifact)
        || state.budget_deadline_unix_millis != evidence.deadline_unix_millis;
    state.run_budget_evidence = Some(artifact);
    state.budget_deadline_unix_millis = evidence.deadline_unix_millis;
    Ok(recovered)
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

fn checkpoint(
    state: &mut PredictionLoopState,
    state_path: &Path,
    base_elapsed_millis: u64,
    run_started: Instant,
) -> Result<(), String> {
    state.elapsed_millis = base_elapsed_millis
        .saturating_add(run_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
    atomic_write_json(state_path, state)
}

fn remaining_time(mission: &PredictionResearchMission, state: &PredictionLoopState) -> Duration {
    Duration::from_millis(
        state
            .budget_deadline_unix_millis
            .saturating_sub(now_unix_millis())
            .max(1),
    )
    .min(Duration::from_secs(
        mission.search_budget.max_seconds.max(1),
    ))
}

fn budget_terminal_if_exhausted(
    mission: &PredictionResearchMission,
    state: &mut PredictionLoopState,
    output_root: &Path,
    state_path: &Path,
    base_elapsed_millis: u64,
    run_started: Instant,
) -> Result<Option<LoopRunSummary>, String> {
    if matches!(state.frontier, Frontier::Done { .. }) {
        return Ok(None);
    }
    let time_exhausted = now_unix_millis() >= state.budget_deadline_unix_millis;
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
    let outcome = TerminalOutcome {
        status: LoopRunStatusWire::BudgetExhausted,
        reason: reason.to_string(),
        keep_models: Vec::new(),
    };
    let mut recorded_late = false;
    if time_exhausted {
        if let Some(attempt) = state
            .llm_attempts
            .iter()
            .find(|attempt| attempt.status == LlmAttemptStatus::Responded)
        {
            let mut attempt = attempt.clone();
            attempt.status = LlmAttemptStatus::Late;
            attempt.error = Some("response arrived after mission wall-clock budget".to_string());
            append_llm_transition(
                output_root,
                state,
                attempt,
                Frontier::Done {
                    outcome: outcome.clone(),
                },
                None,
            )?;
            recorded_late = true;
        }
    }
    if !recorded_late {
        record_terminal_decision(output_root, state, outcome)?;
    }
    checkpoint(state, state_path, base_elapsed_millis, run_started)?;
    Ok(Some(summary_from_state(mission, state, state_path)))
}

fn summary_from_state(
    mission: &PredictionResearchMission,
    state: &PredictionLoopState,
    state_path: &Path,
) -> LoopRunSummary {
    let (status, reason) = if let Some(pause) = state.pause.as_ref() {
        (LoopRunStatus::Paused, Some(pause.reason.clone()))
    } else {
        match &state.frontier {
            Frontier::Done { outcome } => (
                match outcome.status {
                    LoopRunStatusWire::Kept => LoopRunStatus::Kept,
                    LoopRunStatusWire::BudgetExhausted => LoopRunStatus::BudgetExhausted,
                    LoopRunStatusWire::Failed => LoopRunStatus::Failed,
                },
                Some(outcome.reason.clone()),
            ),
            _ => (LoopRunStatus::Running, None),
        }
    };
    LoopRunSummary {
        mission_id: mission.mission_id.clone(),
        status,
        llm_calls_used: state.calls_used(),
        candidates_evaluated: state.candidates_evaluated(),
        iterations_completed: state.iterations.len(),
        keep_models: state.keep_models(),
        reason,
        state_path: state_path.to_path_buf(),
    }
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
    if feedback.schema_version != "prediction_research_feedback.v1"
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
        if !matches!(candidate.verdict.as_str(), "keep" | "discard") {
            return Err(format!("invalid verdict for {}", candidate.model));
        }
        if candidate.verdict == "keep" && !candidate.reason_codes.is_empty() {
            return Err(format!(
                "keep verdict for {} must have no failure reason codes",
                candidate.model
            ));
        }
        if candidate.reason_codes.len() > 32
            || candidate.reason_codes.iter().any(|code| {
                code.is_empty()
                    || code.len() > 80
                    || !code.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        {
            return Err(format!(
                "feedback reason codes are not bounded safe identifiers for {}",
                candidate.model
            ));
        }
        outcomes.push(CandidateOutcome {
            model: candidate.model.clone(),
            verdict: candidate.verdict.clone(),
            reason_codes: candidate.reason_codes.clone(),
        });
    }
    Ok(outcomes)
}

fn persist_evaluation_output(
    output_root: &Path,
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

fn load_iteration_completions(
    output_root: &Path,
    identity: &RunIdentity,
) -> Result<Vec<(ArtifactRef, IterationCompletionEvidence)>, String> {
    let directory = output_root.join("iteration-completions");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut completions = Vec::new();
    for entry in fs::read_dir(&directory).map_err(|error| {
        format!(
            "read iteration completions {}: {error}",
            directory.display()
        )
    })? {
        let entry = entry.map_err(|error| format!("read iteration completion entry: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| format!("inspect iteration completion entry: {error}"))?
            .is_file()
        {
            return Err(format!(
                "iteration completion contains non-file entry {}",
                entry.path().display()
            ));
        }
        let path = entry.path();
        let artifact = existing_artifact_ref(output_root, &path)?;
        let evidence: IterationCompletionEvidence = read_json(&path)?;
        let expected_name = format!(
            "iteration-{:03}-{}.json",
            evidence.sequence, artifact.sha256
        );
        if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
            return Err(format!(
                "iteration completion is not correctly content-addressed: {}",
                path.display()
            ));
        }
        if evidence.schema_version != ITERATION_COMPLETION_SCHEMA_VERSION
            || &evidence.identity != identity
        {
            return Err("iteration completion belongs to another schema or LoopRun".to_string());
        }
        completions.push((artifact, evidence));
    }
    completions.sort_by_key(|(_, evidence)| evidence.sequence);
    let mut previous = None;
    for (index, (artifact, evidence)) in completions.iter().enumerate() {
        if evidence.sequence != index + 1
            || evidence.record.iteration != evidence.sequence
            || evidence.pending.iteration != evidence.sequence
            || evidence.previous_sha256 != previous
        {
            return Err("iteration completion sequence or hash chain is broken".to_string());
        }
        previous = Some(artifact.sha256.clone());
    }
    Ok(completions)
}

fn persist_iteration_completion(
    output_root: &Path,
    state: &PredictionLoopState,
    pending: &PendingIteration,
    record: &IterationRecord,
    next: IterationCompletionNext,
) -> Result<ArtifactRef, String> {
    let ledger = load_llm_ledger(output_root, &state.identity)?;
    let (accepted_artifact, accepted_pending) = ledger
        .iter()
        .find_map(
            |(artifact, event)| match (&event.attempt.status, &event.frontier_after) {
                (LlmAttemptStatus::Accepted, Frontier::Evaluate { pending: accepted })
                    if event.attempt.call_index == pending.call_index =>
                {
                    Some((artifact, accepted))
                }
                _ => None,
            },
        )
        .ok_or_else(|| "iteration completion has no durable Accepted event".to_string())?;
    if accepted_pending != pending {
        return Err("iteration completion differs from durable Accepted frontier".to_string());
    }
    let completions = load_iteration_completions(output_root, &state.identity)?;
    if completions.len() + 1 != pending.iteration {
        return Err("iteration completion does not extend contiguous history".to_string());
    }
    let evidence = IterationCompletionEvidence {
        schema_version: ITERATION_COMPLETION_SCHEMA_VERSION.to_string(),
        sequence: pending.iteration,
        previous_sha256: completions
            .last()
            .map(|(artifact, _)| artifact.sha256.clone()),
        identity: state.identity.clone(),
        accepted_ledger_sha256: accepted_artifact.sha256.clone(),
        pending: pending.clone(),
        record: record.clone(),
        next,
    };
    write_content_addressed_json(
        output_root,
        &output_root.join("iteration-completions"),
        &format!("iteration-{:03}", pending.iteration),
        &evidence,
    )
}

fn reconcile_iteration_completions(
    state: &mut PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &Path,
) -> Result<bool, String> {
    let completions = load_iteration_completions(output_root, &state.identity)?;
    let durable_records = completions
        .iter()
        .map(|(_, evidence)| evidence.record.clone())
        .collect::<Vec<_>>();
    if state.iterations.len() > durable_records.len()
        || state.iterations != durable_records[..state.iterations.len()]
    {
        return Err("state iteration history differs from durable completion chain".to_string());
    }

    let accepted = load_llm_ledger(output_root, &state.identity)?
        .into_iter()
        .filter_map(
            |(artifact, event)| match (event.attempt.status, event.frontier_after) {
                (LlmAttemptStatus::Accepted, Frontier::Evaluate { pending }) => {
                    Some((event.attempt.call_index, (artifact, pending)))
                }
                _ => None,
            },
        )
        .collect::<BTreeMap<_, _>>();
    for (_, evidence) in &completions {
        let (accepted_artifact, accepted_pending) = accepted
            .get(&evidence.pending.call_index)
            .ok_or_else(|| "completion references unknown Accepted call".to_string())?;
        if accepted_artifact.sha256 != evidence.accepted_ledger_sha256
            || accepted_pending != &evidence.pending
            || evidence.record.call_index != evidence.pending.call_index
            || evidence.record.prior != evidence.pending.prior
            || evidence.record.outcomes.len() != evidence.pending.candidate_count
        {
            return Err("completion differs from durable Accepted call or record".to_string());
        }
        let prior_path = verify_artifact(output_root, &evidence.record.prior)?;
        let prior: LlmPriorSpec = read_json(&prior_path)?;
        validate_pending_prior(mission, &evidence.pending, &prior)?;
        verify_evaluation_evidence(output_root, &evidence.record.evaluation)?;
        let feedback_ref = evidence
            .record
            .evaluation
            .feedback
            .as_ref()
            .ok_or_else(|| "completion is missing evaluator feedback".to_string())?;
        let feedback_path = verify_artifact(output_root, feedback_ref)?;
        let feedback: PredictionResearchFeedback = read_json(&feedback_path)?;
        let expected_outcomes = validate_feedback(mission, &prior, &feedback)?;
        if canonical_json_bytes(&evidence.record.outcomes)?
            != canonical_json_bytes(&expected_outcomes)?
        {
            return Err("completion outcomes differ from evaluator feedback".to_string());
        }
        let keep_models = evidence
            .record
            .outcomes
            .iter()
            .filter(|outcome| outcome.verdict == "keep")
            .map(|outcome| outcome.model.clone())
            .collect::<Vec<_>>();
        match &evidence.next {
            IterationCompletionNext::Propose if keep_models.is_empty() => {}
            IterationCompletionNext::Keep {
                keep_models: recorded,
            } if recorded == &keep_models && !recorded.is_empty() => {}
            _ => {
                return Err("completion next frontier disagrees with evaluator verdicts".to_string())
            }
        }
    }

    let mut recovered = state.iterations.len() < durable_records.len();
    if recovered {
        state.iterations = durable_records;
    }
    let completed_calls = state
        .iterations
        .iter()
        .map(|iteration| iteration.call_index)
        .collect::<BTreeSet<_>>();
    let uncompleted = accepted
        .iter()
        .filter(|(call_index, _)| !completed_calls.contains(call_index))
        .map(|(_, (_, pending))| pending.clone())
        .collect::<Vec<_>>();
    if uncompleted.len() > 1 {
        return Err("completion chain leaves multiple Accepted calls pending".to_string());
    }

    let has_durable_terminal = load_terminal_decision(output_root, &state.identity)?.is_some();
    if state.terminal_evidence.is_none() && !has_durable_terminal {
        if let Some((_, evidence)) = completions.last() {
            if let IterationCompletionNext::Keep { keep_models } = &evidence.next {
                let outcome = if now_unix_millis() >= state.budget_deadline_unix_millis {
                    TerminalOutcome {
                        status: LoopRunStatusWire::BudgetExhausted,
                        reason: "mission wall-clock budget exhausted before keep promotion"
                            .to_string(),
                        keep_models: Vec::new(),
                    }
                } else {
                    TerminalOutcome {
                        status: LoopRunStatusWire::Kept,
                        reason: "Rust evaluator produced deterministic keep".to_string(),
                        keep_models: keep_models.clone(),
                    }
                };
                record_terminal_decision(output_root, state, outcome)?;
                recovered = true;
            }
        }
        if state.terminal_evidence.is_none() {
            let target = if let Some(pending) = uncompleted.into_iter().next() {
                Frontier::Evaluate { pending }
            } else if state.baseline.is_some() {
                Frontier::Propose
            } else {
                Frontier::Baseline
            };
            if state.frontier != target {
                state.frontier = target;
                recovered = true;
            }
        }
    }
    Ok(recovered)
}

fn recover_completed_evaluation(
    output_root: &Path,
    evaluator_parent: &Path,
) -> Result<Option<(EvaluationEvidence, PredictionResearchFeedback)>, String> {
    if !evaluator_parent.exists() {
        return Ok(None);
    }
    let mut attempt_dirs = Vec::new();
    for entry in fs::read_dir(evaluator_parent).map_err(|error| {
        format!(
            "read evaluator attempt directory {}: {error}",
            evaluator_parent.display()
        )
    })? {
        let entry = entry.map_err(|error| format!("read evaluator attempt entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect evaluator attempt entry: {error}"))?;
        if !file_type.is_dir() {
            return Err(format!(
                "evaluator attempt parent contains non-directory entry {}",
                entry.path().display()
            ));
        }
        attempt_dirs.push(entry.path());
    }
    attempt_dirs.sort();
    for attempt_dir in attempt_dirs.into_iter().rev() {
        let mut files = Vec::new();
        for entry in fs::read_dir(&attempt_dir)
            .map_err(|error| format!("read evaluator attempt {}: {error}", attempt_dir.display()))?
        {
            let entry = entry.map_err(|error| format!("read evaluator evidence entry: {error}"))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("inspect evaluator evidence entry: {error}"))?;
            if file_type.is_dir() {
                continue;
            }
            if !file_type.is_file() {
                return Err(format!(
                    "evaluator attempt contains unsupported entry {}",
                    entry.path().display()
                ));
            }
            files.push(entry.path());
        }
        let feedback = matching_evidence_file(
            &files,
            "prediction-research-feedback-",
            ".json",
            &attempt_dir,
        )?;
        let Some(feedback_path) = feedback else {
            continue;
        };
        let stdout_path =
            matching_evidence_file(&files, "evaluator-stdout-", ".txt", &attempt_dir)?.ok_or_else(
                || {
                    format!(
                        "completed evaluator feedback is missing stdout evidence under {}",
                        attempt_dir.display()
                    )
                },
            )?;
        let stderr_path =
            matching_evidence_file(&files, "evaluator-stderr-", ".txt", &attempt_dir)?.ok_or_else(
                || {
                    format!(
                        "completed evaluator feedback is missing stderr evidence under {}",
                        attempt_dir.display()
                    )
                },
            )?;
        let evidence = EvaluationEvidence {
            stdout: existing_artifact_ref(output_root, &stdout_path)?,
            stderr: existing_artifact_ref(output_root, &stderr_path)?,
            feedback: Some(existing_artifact_ref(output_root, &feedback_path)?),
        };
        verify_evaluation_evidence(output_root, &evidence)?;
        let feedback: PredictionResearchFeedback = read_json(&feedback_path)?;
        return Ok(Some((evidence, feedback)));
    }
    Ok(None)
}

fn matching_evidence_file(
    files: &[PathBuf],
    prefix: &str,
    suffix: &str,
    attempt_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    let matches = files
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(suffix))
        })
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [path] => Ok(Some(path.clone())),
        _ => Err(format!(
            "ambiguous {prefix} evidence under {}",
            attempt_dir.display()
        )),
    }
}

fn existing_artifact_ref(output_root: &Path, path: &Path) -> Result<ArtifactRef, String> {
    let body = fs::read(path)
        .map_err(|error| format!("read recovered evidence {}: {error}", path.display()))?;
    let artifact = ArtifactRef {
        path: relative_path(output_root, path)?,
        sha256: sha256_hex(&body),
    };
    verify_artifact(output_root, &artifact)?;
    Ok(artifact)
}

fn has_unprocessed_response(state: &PredictionLoopState) -> bool {
    state
        .llm_attempts
        .iter()
        .any(|attempt| attempt.status == LlmAttemptStatus::Responded)
}

fn validate_llm_attempt_shape(attempt: &LlmAttemptRecord) -> Result<(), String> {
    let has_response = attempt.response.is_some();
    let requires_response = matches!(
        attempt.status,
        LlmAttemptStatus::Responded
            | LlmAttemptStatus::Accepted
            | LlmAttemptStatus::Invalid
            | LlmAttemptStatus::Late
    );
    if has_response != requires_response {
        return Err(format!(
            "LLM call {} response evidence disagrees with status",
            attempt.call_index
        ));
    }
    if attempt.provider.is_some() != attempt.model.is_some() {
        return Err(format!(
            "LLM call {} provider and model metadata must appear together",
            attempt.call_index
        ));
    }
    if has_response
        && (attempt.provider.is_none() || attempt.model.is_none() || attempt.usage.is_none())
    {
        return Err(format!(
            "LLM call {} response is missing provider, model, or usage provenance",
            attempt.call_index
        ));
    }
    if !has_response
        && (attempt.provider.is_some() || attempt.model.is_some() || attempt.usage.is_some())
    {
        return Err(format!(
            "LLM call {} has provider metadata without response evidence",
            attempt.call_index
        ));
    }
    let requires_error = matches!(
        attempt.status,
        LlmAttemptStatus::Invalid
            | LlmAttemptStatus::ProviderFailed
            | LlmAttemptStatus::Abandoned
            | LlmAttemptStatus::Late
    );
    if requires_error != attempt.error.is_some() {
        return Err(format!(
            "LLM call {} error evidence disagrees with status",
            attempt.call_index
        ));
    }
    Ok(())
}

fn apply_llm_attempt_transition(
    attempts: &mut Vec<LlmAttemptRecord>,
    next: &LlmAttemptRecord,
) -> Result<(), String> {
    validate_llm_attempt_shape(next)?;
    if next.status == LlmAttemptStatus::Started {
        if next.call_index != attempts.len() + 1 {
            return Err("LLM Started event does not extend contiguous call lineage".to_string());
        }
        if attempts.last().is_some_and(|attempt| {
            matches!(
                attempt.status,
                LlmAttemptStatus::Started | LlmAttemptStatus::Responded | LlmAttemptStatus::Late
            )
        }) {
            return Err("LLM Started event follows a non-terminal attempt".to_string());
        }
        attempts.push(next.clone());
        return Ok(());
    }

    let current = attempts
        .last_mut()
        .ok_or_else(|| "LLM transition has no Started event".to_string())?;
    if next.call_index != current.call_index || next.prompt != current.prompt {
        return Err("LLM transition changed call index or prompt evidence".to_string());
    }
    let legal = matches!(
        (&current.status, &next.status),
        (LlmAttemptStatus::Started, LlmAttemptStatus::Responded)
            | (LlmAttemptStatus::Started, LlmAttemptStatus::ProviderFailed)
            | (LlmAttemptStatus::Started, LlmAttemptStatus::Abandoned)
            | (LlmAttemptStatus::Responded, LlmAttemptStatus::Accepted)
            | (LlmAttemptStatus::Responded, LlmAttemptStatus::Invalid)
            | (LlmAttemptStatus::Responded, LlmAttemptStatus::Late)
    );
    if !legal {
        return Err(format!(
            "illegal LLM status transition {:?} -> {:?}",
            current.status, next.status
        ));
    }
    if current.status == LlmAttemptStatus::Responded
        && (next.response != current.response
            || next.provider != current.provider
            || next.model != current.model
            || next.usage != current.usage)
    {
        return Err("LLM terminal transition changed response evidence or metadata".to_string());
    }
    *current = next.clone();
    Ok(())
}

fn validate_llm_event_context(event: &LlmLedgerEvent) -> Result<(), String> {
    if event.schema_version != LLM_LEDGER_EVENT_SCHEMA_VERSION {
        return Err(format!(
            "LLM ledger event schema mismatch at sequence {}",
            event.sequence
        ));
    }
    match event.attempt.status {
        LlmAttemptStatus::Accepted => match &event.frontier_after {
            Frontier::Evaluate { pending }
                if pending.call_index == event.attempt.call_index
                    && pending.candidate_count > 0 => {}
            _ => return Err("Accepted LLM event must bind its evaluator frontier".to_string()),
        },
        LlmAttemptStatus::Late => match &event.frontier_after {
            Frontier::Done { outcome } if outcome.status == LoopRunStatusWire::BudgetExhausted => {}
            _ => return Err("Late LLM event must bind budget terminal outcome".to_string()),
        },
        LlmAttemptStatus::Abandoned => {
            if !matches!(
                event.frontier_after,
                Frontier::Propose
                    | Frontier::Done {
                        outcome: TerminalOutcome {
                            status: LoopRunStatusWire::BudgetExhausted,
                            ..
                        }
                    }
            ) {
                return Err("Abandoned LLM event has invalid frontier".to_string());
            }
        }
        _ if !matches!(event.frontier_after, Frontier::Propose) => {
            return Err("LLM event has invalid proposal frontier".to_string())
        }
        _ => {}
    }
    match (&event.attempt.status, &event.failure) {
        (LlmAttemptStatus::ProviderFailed, Some(failure))
            if failure.stage == "propose"
                && failure.reason == event.attempt.error.as_deref().unwrap_or_default()
                && failure.stdout.is_none()
                && failure.stderr.is_none() => {}
        (LlmAttemptStatus::ProviderFailed, _) => {
            return Err("ProviderFailed event must bind its failure record".to_string())
        }
        (_, Some(_)) => return Err("only ProviderFailed may append an LLM failure".to_string()),
        (_, None) => {}
    }
    Ok(())
}

fn apply_llm_ledger_event(
    state: &mut PredictionLoopState,
    event: &LlmLedgerEvent,
    artifact: &ArtifactRef,
) -> Result<(), String> {
    validate_llm_event_context(event)?;
    apply_llm_attempt_transition(&mut state.llm_attempts, &event.attempt)?;
    state.frontier = event.frontier_after.clone();
    if matches!(event.frontier_after, Frontier::Done { .. }) {
        state.terminal_evidence = Some(artifact.clone());
    }
    if let Some(failure) = event.failure.clone() {
        state.failures.push(failure.clone());
        state.pause = Some(failure);
    }
    state.llm_ledger = LlmLedgerCursor {
        event_count: event.sequence,
        head_sha256: Some(artifact.sha256.clone()),
    };
    Ok(())
}

fn append_llm_transition(
    output_root: &Path,
    state: &mut PredictionLoopState,
    attempt: LlmAttemptRecord,
    frontier_after: Frontier,
    failure: Option<FailureRecord>,
) -> Result<(), String> {
    let event = LlmLedgerEvent {
        schema_version: LLM_LEDGER_EVENT_SCHEMA_VERSION.to_string(),
        sequence: state.llm_ledger.event_count + 1,
        previous_sha256: state.llm_ledger.head_sha256.clone(),
        identity: state.identity.clone(),
        attempt,
        frontier_after,
        failure,
    };
    let mut projected = state.llm_attempts.clone();
    validate_llm_event_context(&event)?;
    apply_llm_attempt_transition(&mut projected, &event.attempt)?;
    let artifact = write_content_addressed_json(
        output_root,
        &output_root.join("llm-ledger"),
        &format!("event-{:06}", event.sequence),
        &event,
    )?;
    apply_llm_ledger_event(state, &event, &artifact)
}

fn load_llm_ledger(
    output_root: &Path,
    identity: &RunIdentity,
) -> Result<Vec<(ArtifactRef, LlmLedgerEvent)>, String> {
    let directory = output_root.join("llm-ledger");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    for entry in fs::read_dir(&directory)
        .map_err(|error| format!("read LLM ledger {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("read LLM ledger entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect LLM ledger entry: {error}"))?;
        if !file_type.is_file() {
            return Err(format!(
                "LLM ledger contains non-file entry {}",
                entry.path().display()
            ));
        }
        let path = entry.path();
        let body = fs::read(&path)
            .map_err(|error| format!("read LLM ledger event {}: {error}", path.display()))?;
        let event: LlmLedgerEvent = serde_json::from_slice(&body)
            .map_err(|error| format!("parse LLM ledger event {}: {error}", path.display()))?;
        if &event.identity != identity {
            return Err("LLM ledger event belongs to another run identity".to_string());
        }
        let digest = sha256_hex(&body);
        let expected_name = format!("event-{:06}-{digest}.json", event.sequence);
        if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
            return Err(format!(
                "LLM ledger event is not correctly content-addressed: {}",
                path.display()
            ));
        }
        let artifact = ArtifactRef {
            path: relative_path(output_root, &path)?,
            sha256: digest,
        };
        verify_artifact(output_root, &artifact)?;
        events.push((artifact, event));
    }
    events.sort_by_key(|(_, event)| event.sequence);
    let mut previous = None;
    let mut projection = Vec::new();
    for (index, (artifact, event)) in events.iter().enumerate() {
        if event.sequence != index + 1 || event.previous_sha256 != previous {
            return Err("LLM ledger sequence or hash chain is broken".to_string());
        }
        validate_llm_event_context(event)?;
        apply_llm_attempt_transition(&mut projection, &event.attempt)?;
        previous = Some(artifact.sha256.clone());
    }
    Ok(events)
}

fn reconcile_llm_ledger(
    state: &mut PredictionLoopState,
    output_root: &Path,
) -> Result<bool, String> {
    let events = load_llm_ledger(output_root, &state.identity)?;
    if state.llm_ledger.event_count > events.len() {
        return Err("loop state LLM ledger cursor is ahead of durable ledger".to_string());
    }
    let cursor_head = state
        .llm_ledger
        .event_count
        .checked_sub(1)
        .map(|index| events[index].0.sha256.clone());
    if state.llm_ledger.head_sha256 != cursor_head {
        return Err("loop state LLM ledger head does not match durable chain".to_string());
    }
    let mut prefix_projection = Vec::new();
    let mut prefix_failures = Vec::new();
    for (_, event) in events.iter().take(state.llm_ledger.event_count) {
        apply_llm_attempt_transition(&mut prefix_projection, &event.attempt)?;
        if let Some(failure) = &event.failure {
            prefix_failures.push(failure.clone());
        }
    }
    if state.llm_attempts != prefix_projection {
        return Err("loop state LLM attempts differ from durable ledger projection".to_string());
    }
    let state_proposal_failures = state
        .failures
        .iter()
        .filter(|failure| failure.stage == "propose")
        .cloned()
        .collect::<Vec<_>>();
    if state_proposal_failures != prefix_failures {
        return Err(
            "loop state proposal failures differ from durable ledger projection".to_string(),
        );
    }
    let remaining = events.len() - state.llm_ledger.event_count;
    if remaining > 1 {
        return Err("loop state trails the durable LLM ledger by more than one event".to_string());
    }
    if let Some((artifact, event)) = events.last().filter(|_| remaining == 1) {
        apply_llm_ledger_event(state, event, artifact)?;
        return Ok(true);
    }
    Ok(false)
}

fn reconcile_orphan_call_reservation(
    state: &mut PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &Path,
) -> Result<bool, String> {
    let parent = output_root.join("llm-calls");
    if !parent.exists() {
        return Ok(false);
    }

    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(&parent)
        .map_err(|error| format!("read LLM calls {}: {error}", parent.display()))?
    {
        let entry = entry.map_err(|error| format!("read LLM call entry: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| format!("inspect LLM call entry: {error}"))?
            .is_dir()
        {
            return Err(format!(
                "llm-calls contains non-directory entry {}",
                entry.path().display()
            ));
        }
        actual.insert(entry.file_name());
    }
    let expected = state
        .llm_attempts
        .iter()
        .map(|attempt| std::ffi::OsString::from(format!("call-{:03}", attempt.call_index)))
        .collect::<BTreeSet<_>>();
    if expected.iter().any(|directory| !actual.contains(directory)) {
        return Err("llm-calls is missing a durable call directory".to_string());
    }
    let extras = actual.difference(&expected).cloned().collect::<Vec<_>>();
    let Some(extra) = extras.first() else {
        return Ok(false);
    };
    if extras.len() != 1
        || !matches!(state.frontier, Frontier::Propose)
        || has_unprocessed_response(state)
        || state.calls_used() >= mission.search_budget.max_llm_calls
        || state.candidates_evaluated() >= mission.search_budget.max_candidates
    {
        return Err("llm-calls contains an invalid orphan call reservation".to_string());
    }

    let call_index = state.calls_used() + 1;
    let expected_name = std::ffi::OsString::from(format!("call-{call_index:03}"));
    if extra != &expected_name {
        return Err("orphan LLM call directory is not the next contiguous call".to_string());
    }
    let call_dir = parent.join(extra);
    let mut entries = fs::read_dir(&call_dir)
        .map_err(|error| format!("read orphan LLM call {}: {error}", call_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read orphan LLM call entry: {error}"))?;
    if entries.is_empty() {
        fs::remove_dir(&call_dir).map_err(|error| {
            format!(
                "remove empty orphan LLM call {}: {error}",
                call_dir.display()
            )
        })?;
        fs::File::open(&parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync LLM calls directory {}: {error}", parent.display()))?;
        return Ok(false);
    }
    if entries.len() != 1 {
        return Err(format!(
            "orphan LLM call contains ambiguous artifacts under {}",
            call_dir.display()
        ));
    }
    let entry = entries.pop().expect("one orphan call artifact");
    if !entry
        .file_type()
        .map_err(|error| format!("inspect orphan LLM prompt: {error}"))?
        .is_file()
    {
        return Err("orphan LLM prompt must be a regular file".to_string());
    }

    let remaining_candidates = mission
        .search_budget
        .max_candidates
        .saturating_sub(state.candidates_evaluated());
    let prompt = build_prediction_prompt(
        mission,
        remaining_candidates,
        &qualitative_prior_outcomes(state),
    );
    let digest = sha256_hex(prompt.as_bytes());
    let expected_prompt_name = format!("llm-prompt-{digest}.txt");
    if entry.file_name().to_str() != Some(expected_prompt_name.as_str()) {
        return Err("orphan LLM prompt does not match the governed next prompt".to_string());
    }
    let body = fs::read(entry.path())
        .map_err(|error| format!("read orphan LLM prompt {}: {error}", entry.path().display()))?;
    if body != prompt.as_bytes() {
        return Err("orphan LLM prompt content differs from the governed next prompt".to_string());
    }
    let prompt = existing_artifact_ref(output_root, &entry.path())?;
    append_llm_transition(
        output_root,
        state,
        LlmAttemptRecord {
            call_index,
            prompt,
            response: None,
            provider: None,
            model: None,
            usage: None,
            status: LlmAttemptStatus::Started,
            error: None,
        },
        Frontier::Propose,
        None,
    )?;
    Ok(true)
}

fn verify_llm_call_directories(
    state: &PredictionLoopState,
    output_root: &Path,
) -> Result<(), String> {
    let parent = output_root.join("llm-calls");
    if !parent.exists() {
        return if state.llm_attempts.is_empty() {
            Ok(())
        } else {
            Err("durable LLM ledger has calls but llm-calls directory is missing".to_string())
        };
    }
    let mut actual_directories = BTreeSet::new();
    for entry in fs::read_dir(&parent)
        .map_err(|error| format!("read LLM calls {}: {error}", parent.display()))?
    {
        let entry = entry.map_err(|error| format!("read LLM call entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect LLM call entry: {error}"))?;
        if !file_type.is_dir() {
            return Err(format!(
                "llm-calls contains non-directory entry {}",
                entry.path().display()
            ));
        }
        actual_directories.insert(entry.file_name());
    }
    let expected_directories = state
        .llm_attempts
        .iter()
        .map(|attempt| std::ffi::OsString::from(format!("call-{:03}", attempt.call_index)))
        .collect::<BTreeSet<_>>();
    if actual_directories != expected_directories {
        return Err("llm-calls contains missing or orphan call directories".to_string());
    }

    for attempt in &state.llm_attempts {
        let directory = parent.join(format!("call-{:03}", attempt.call_index));
        let prompt_path = verify_artifact(output_root, &attempt.prompt)?;
        verify_llm_artifact_binding(&prompt_path, &directory, "llm-prompt-", ".txt")?;
        let mut expected_files = BTreeSet::from([prompt_path]);
        if let Some(response) = &attempt.response {
            let response_path = verify_artifact(output_root, response)?;
            verify_llm_artifact_binding(&response_path, &directory, "llm-response-", ".json")?;
            expected_files.insert(response_path);
        }
        let mut orphan_responses = 0_usize;
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("read LLM call {}: {error}", directory.display()))?
        {
            let entry = entry.map_err(|error| format!("read LLM call artifact: {error}"))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("inspect LLM call artifact: {error}"))?;
            if !file_type.is_file() {
                return Err(format!(
                    "LLM call contains non-file artifact {}",
                    entry.path().display()
                ));
            }
            let path = entry.path();
            if expected_files.contains(&path) {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if attempt.status == LlmAttemptStatus::Started
                && name.starts_with("llm-response-")
                && name.ends_with(".json")
            {
                existing_artifact_ref(output_root, &path)?;
                orphan_responses += 1;
                continue;
            }
            return Err(format!(
                "LLM call contains unbound artifact {}",
                path.display()
            ));
        }
        if orphan_responses > 1 {
            return Err(format!(
                "LLM call {} has ambiguous orphan response evidence",
                attempt.call_index
            ));
        }
    }
    Ok(())
}

fn verify_llm_artifact_binding(
    path: &Path,
    expected_directory: &Path,
    prefix: &str,
    suffix: &str,
) -> Result<(), String> {
    if path.parent() != Some(expected_directory) {
        return Err(format!(
            "LLM artifact {} is bound to the wrong call directory",
            path.display()
        ));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !name.starts_with(prefix) || !name.ends_with(suffix) {
        return Err(format!(
            "LLM artifact {} has the wrong filename contract",
            path.display()
        ));
    }
    Ok(())
}

fn record_terminal_decision(
    output_root: &Path,
    state: &mut PredictionLoopState,
    outcome: TerminalOutcome,
) -> Result<(), String> {
    if outcome.status == LoopRunStatusWire::Failed {
        return Err("Failed terminal decision requires cancelled pending evidence".to_string());
    }
    record_terminal_decision_with_cancelled(output_root, state, outcome, None)
}

fn record_terminal_decision_with_cancelled(
    output_root: &Path,
    state: &mut PredictionLoopState,
    outcome: TerminalOutcome,
    cancelled_pending: Option<CancelledPendingEvidence>,
) -> Result<(), String> {
    if state.terminal_evidence.is_some() || matches!(state.frontier, Frontier::Done { .. }) {
        return Err("prediction loop already has a terminal decision".to_string());
    }
    match (&outcome.status, cancelled_pending.as_ref()) {
        (LoopRunStatusWire::Failed, Some(cancelled)) => {
            if !outcome.keep_models.is_empty()
                || cancelled.reason != outcome.reason
                || !matches!(
                    &state.frontier,
                    Frontier::Evaluate { pending } if pending == &cancelled.pending
                )
            {
                return Err(
                    "Failed terminal decision does not exactly cancel its pending evaluator frontier"
                        .to_string(),
                );
            }
            let accepted = load_llm_ledger(output_root, &state.identity)?
                .into_iter()
                .any(|(artifact, event)| {
                    artifact.sha256 == cancelled.accepted_ledger_sha256
                        && event.attempt.status == LlmAttemptStatus::Accepted
                        && matches!(
                            event.frontier_after,
                            Frontier::Evaluate { pending } if pending == cancelled.pending
                        )
                });
            if !accepted {
                return Err(
                    "Failed terminal decision is not bound to its durable Accepted event"
                        .to_string(),
                );
            }
            verify_evaluation_evidence(output_root, &cancelled.evaluation)?;
        }
        (LoopRunStatusWire::Failed, None) => {
            return Err("Failed terminal decision requires cancelled pending evidence".to_string())
        }
        (_, Some(_)) => {
            return Err(
                "only a Failed terminal decision may contain cancelled pending evidence"
                    .to_string(),
            )
        }
        (_, None) => {}
    }
    let evidence = TerminalDecisionEvidence {
        schema_version: TERMINAL_EVIDENCE_SCHEMA_VERSION.to_string(),
        identity: state.identity.clone(),
        outcome: outcome.clone(),
        iterations: state.iterations.clone(),
        cancelled_pending,
    };
    let artifact = write_content_addressed_json(
        output_root,
        &output_root.join("terminal"),
        "terminal-outcome",
        &evidence,
    )?;
    state.terminal_evidence = Some(artifact);
    state.frontier = Frontier::Done { outcome };
    Ok(())
}

fn validate_terminal_decision_evidence(
    evidence: &TerminalDecisionEvidence,
    mission: &PredictionResearchMission,
    identity: &RunIdentity,
    output_root: &Path,
) -> Result<(), String> {
    if evidence.schema_version != TERMINAL_EVIDENCE_SCHEMA_VERSION
        || &evidence.identity != identity
        || evidence.outcome.reason.is_empty()
    {
        return Err("terminal evidence schema, identity, or reason is invalid".to_string());
    }

    let completed = load_iteration_completions(output_root, identity)?
        .into_iter()
        .map(|(_, completion)| completion.record)
        .collect::<Vec<_>>();
    if evidence.iterations != completed {
        return Err(
            "terminal evidence iterations differ from the durable completion chain".to_string(),
        );
    }

    match (
        &evidence.outcome.status,
        evidence.cancelled_pending.as_ref(),
    ) {
        (LoopRunStatusWire::Failed, Some(cancelled)) => {
            if !evidence.outcome.keep_models.is_empty()
                || cancelled.reason != evidence.outcome.reason
                || cancelled.pending.iteration != evidence.iterations.len() + 1
                || evidence
                    .iterations
                    .iter()
                    .any(|record| record.call_index == cancelled.pending.call_index)
            {
                return Err(
                    "Failed terminal evidence does not uniquely cancel its next pending iteration"
                        .to_string(),
                );
            }

            let accepted = load_llm_ledger(output_root, identity)?
                .into_iter()
                .find(|(artifact, event)| {
                    artifact.sha256 == cancelled.accepted_ledger_sha256
                        && event.attempt.status == LlmAttemptStatus::Accepted
                        && event.attempt.call_index == cancelled.pending.call_index
                })
                .ok_or_else(|| {
                    "cancelled pending evidence has no matching durable Accepted event".to_string()
                })?;
            if !matches!(
                accepted.1.frontier_after,
                Frontier::Evaluate { pending } if pending == cancelled.pending
            ) {
                return Err(
                    "cancelled pending evidence differs from its durable Accepted frontier"
                        .to_string(),
                );
            }

            let prior_path = verify_artifact(output_root, &cancelled.pending.prior)?;
            let prior: LlmPriorSpec = read_json(&prior_path)?;
            validate_pending_prior(mission, &cancelled.pending, &prior)?;
            verify_evaluation_evidence(output_root, &cancelled.evaluation)?;
            let feedback_ref =
                cancelled.evaluation.feedback.as_ref().ok_or_else(|| {
                    "cancelled evaluator evidence is missing feedback".to_string()
                })?;
            let feedback_path = verify_artifact(output_root, feedback_ref)?;
            let feedback: PredictionResearchFeedback = read_json(&feedback_path)?;
            let invalid_reason = match validate_feedback(mission, &prior, &feedback) {
                Err(reason) => reason,
                Ok(_) => {
                    return Err(
                        "Failed cancellation unexpectedly binds valid evaluator feedback"
                            .to_string(),
                    )
                }
            };
            let expected_reason = format!("invalid evaluator feedback: {invalid_reason}");
            if cancelled.reason != expected_reason {
                return Err(
                    "cancelled evaluator reason differs from its durable invalid feedback"
                        .to_string(),
                );
            }
        }
        (LoopRunStatusWire::Failed, None) => {
            return Err(
                "Failed terminal evidence is missing cancelled pending evidence".to_string(),
            )
        }
        (LoopRunStatusWire::Kept, None) if evidence.outcome.keep_models.is_empty() => {
            return Err("Kept terminal evidence must name at least one kept model".to_string())
        }
        (LoopRunStatusWire::BudgetExhausted, None) if !evidence.outcome.keep_models.is_empty() => {
            return Err("budget terminal evidence may not keep models".to_string())
        }
        (_, Some(_)) => {
            return Err(
                "only Failed terminal evidence may contain cancelled pending evidence".to_string(),
            )
        }
        (_, None) => {}
    }
    Ok(())
}

fn load_terminal_decision(
    output_root: &Path,
    identity: &RunIdentity,
) -> Result<Option<(ArtifactRef, TerminalDecisionEvidence)>, String> {
    let directory = output_root.join("terminal");
    if !directory.exists() {
        return Ok(None);
    }
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| format!("read terminal evidence {}: {error}", directory.display()))?;
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    let entry = entry.map_err(|error| format!("read terminal evidence entry: {error}"))?;
    if entries.next().is_some() {
        return Err("terminal evidence directory contains more than one decision".to_string());
    }
    let file_type = entry
        .file_type()
        .map_err(|error| format!("inspect terminal evidence: {error}"))?;
    if !file_type.is_file() {
        return Err("terminal evidence entry must be a regular file".to_string());
    }
    let path = entry.path();
    let body = fs::read(&path)
        .map_err(|error| format!("read terminal evidence {}: {error}", path.display()))?;
    let evidence: TerminalDecisionEvidence = serde_json::from_slice(&body)
        .map_err(|error| format!("parse terminal evidence {}: {error}", path.display()))?;
    if evidence.schema_version != TERMINAL_EVIDENCE_SCHEMA_VERSION || &evidence.identity != identity
    {
        return Err("terminal evidence schema or run identity mismatch".to_string());
    }
    let digest = sha256_hex(&body);
    let expected_name = format!("terminal-outcome-{digest}.json");
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err("terminal decision is not correctly content-addressed".to_string());
    }
    let artifact = ArtifactRef {
        path: relative_path(output_root, &path)?,
        sha256: digest,
    };
    verify_artifact(output_root, &artifact)?;
    Ok(Some((artifact, evidence)))
}

fn reconcile_terminal_evidence(
    state: &mut PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &Path,
) -> Result<bool, String> {
    let durable = load_terminal_decision(output_root, &state.identity)?;
    if let Some((artifact, evidence)) = durable {
        validate_terminal_decision_evidence(&evidence, mission, &state.identity, output_root)?;
        if state.iterations.len() > evidence.iterations.len()
            || state.iterations != evidence.iterations[..state.iterations.len()]
        {
            return Err(
                "state iteration history diverges from durable terminal decision".to_string(),
            );
        }
        let recovered_iterations = state.iterations.len() < evidence.iterations.len();
        if recovered_iterations {
            state.iterations = evidence.iterations.clone();
        }
        match &state.terminal_evidence {
            Some(bound) if bound != &artifact => {
                return Err("state terminal evidence differs from durable decision".to_string())
            }
            Some(_) => {}
            None => {
                state.terminal_evidence = Some(artifact);
                state.frontier = Frontier::Done {
                    outcome: evidence.outcome,
                };
                return Ok(true);
            }
        }
        if recovered_iterations {
            return Ok(true);
        }
    }

    match (&state.frontier, &state.terminal_evidence) {
        (Frontier::Done { .. }, None) => {
            return Err("terminal frontier is missing content-addressed evidence".to_string())
        }
        (Frontier::Done { outcome }, Some(artifact)) => {
            let path = verify_artifact(output_root, artifact)?;
            if artifact.path.starts_with("llm-ledger/") {
                let event: LlmLedgerEvent = read_json(&path)?;
                match event.frontier_after {
                    Frontier::Done {
                        outcome: ledger_outcome,
                    } if event.attempt.status == LlmAttemptStatus::Late
                        && &ledger_outcome == outcome => {}
                    _ => {
                        return Err(
                            "terminal state is not bound to its Late ledger event".to_string()
                        )
                    }
                }
            } else {
                let evidence: TerminalDecisionEvidence = read_json(&path)?;
                validate_terminal_decision_evidence(
                    &evidence,
                    mission,
                    &state.identity,
                    output_root,
                )?;
                if evidence.schema_version != TERMINAL_EVIDENCE_SCHEMA_VERSION
                    || evidence.identity != state.identity
                    || &evidence.outcome != outcome
                    || evidence.iterations != state.iterations
                {
                    return Err("terminal state differs from terminal evidence".to_string());
                }
            }
        }
        (_, Some(_)) => {
            return Err("non-terminal frontier references terminal evidence".to_string())
        }
        (_, None) => {}
    }
    Ok(false)
}

fn recover_interrupted_llm_attempt(
    state: &mut PredictionLoopState,
    output_root: &Path,
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
    let prompt_path = verify_artifact(output_root, &state.llm_attempts[index].prompt)?;
    let attempt_dir = prompt_path
        .parent()
        .ok_or_else(|| "LLM prompt evidence has no parent directory".to_string())?;
    let mut responses = Vec::new();
    for entry in fs::read_dir(attempt_dir)
        .map_err(|error| format!("read interrupted LLM attempt: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read interrupted LLM artifact: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect interrupted LLM artifact: {error}"))?;
        if !file_type.is_file() {
            return Err(format!(
                "interrupted LLM call contains non-file entry {}",
                entry.path().display()
            ));
        }
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("llm-response-") && name.ends_with(".json"))
        {
            responses.push(path);
        }
    }
    match responses.as_slice() {
        [] => {
            let mut attempt = state.llm_attempts[index].clone();
            attempt.status = LlmAttemptStatus::Abandoned;
            attempt.error = Some(
                "process stopped after durable call reservation and before response evidence"
                    .to_string(),
            );
            let frontier = state.frontier.clone();
            append_llm_transition(output_root, state, attempt, frontier, None)?;
        }
        [path] => {
            let response: LlmResponseEvidence = read_json(path)?;
            let response_ref = existing_artifact_ref(output_root, path)?;
            let mut attempt = state.llm_attempts[index].clone();
            attempt.response = Some(response_ref);
            attempt.provider = Some(response.provider);
            attempt.model = Some(response.model);
            attempt.usage = Some(response.usage);
            attempt.status = LlmAttemptStatus::Responded;
            let frontier = state.frontier.clone();
            append_llm_transition(output_root, state, attempt, frontier, None)?;
        }
        _ => {
            return Err(format!(
                "interrupted LLM call has ambiguous response evidence under {}",
                attempt_dir.display()
            ));
        }
    }
    Ok(true)
}

fn verify_evaluation_evidence(
    output_root: &Path,
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
    state: &mut PredictionLoopState,
    mission: &PredictionResearchMission,
    output_root: &Path,
) -> Result<bool, String> {
    let mut recovered = reconcile_llm_ledger(state, output_root)?;
    recovered |= reconcile_orphan_call_reservation(state, mission, output_root)?;
    verify_llm_call_directories(state, output_root)?;
    recovered |= reconcile_iteration_completions(state, mission, output_root)?;
    recovered |= reconcile_terminal_evidence(state, mission, output_root)?;
    if state.baseline.is_none()
        && (!state.llm_attempts.is_empty() || !matches!(state.frontier, Frontier::Baseline))
    {
        return Err("loop state advanced before durable baseline evidence".to_string());
    }
    if state.baseline.is_some() && matches!(state.frontier, Frontier::Baseline) {
        return Err("loop state rolled back behind durable baseline evidence".to_string());
    }
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
        if let Some(response) = &attempt.response {
            verify_artifact(output_root, response)?;
        }
        validate_llm_attempt_shape(attempt)?;
    }
    let accepted_priors = load_llm_ledger(output_root, &state.identity)?
        .into_iter()
        .filter_map(
            |(_, event)| match (event.attempt.status, event.frontier_after) {
                (LlmAttemptStatus::Accepted, Frontier::Evaluate { pending }) => {
                    Some((event.attempt.call_index, pending))
                }
                _ => None,
            },
        )
        .collect::<BTreeMap<_, _>>();
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
        let accepted = accepted_priors
            .get(&iteration.call_index)
            .ok_or_else(|| "iteration has no durable Accepted ledger event".to_string())?;
        if accepted.iteration != iteration.iteration
            || accepted.prior != iteration.prior
            || accepted.candidate_count != iteration.outcomes.len()
        {
            return Err("iteration differs from its durable Accepted frontier".to_string());
        }
        let prior_path = verify_artifact(output_root, &iteration.prior)?;
        let prior: LlmPriorSpec = read_json(&prior_path)?;
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
        let feedback_path = verify_artifact(output_root, feedback_ref)?;
        let feedback: PredictionResearchFeedback = read_json(&feedback_path)?;
        let expected_outcomes = validate_feedback(mission, &prior, &feedback)?;
        if canonical_json_bytes(&iteration.outcomes)? != canonical_json_bytes(&expected_outcomes)? {
            return Err("durable candidate outcomes differ from evaluator feedback".to_string());
        }
    }
    if let Frontier::Evaluate { pending } = &state.frontier {
        if pending.iteration != state.iterations.len() + 1 {
            return Err("pending iteration is not the next contiguous iteration".to_string());
        }
        let prior_path = verify_artifact(output_root, &pending.prior)?;
        let prior: LlmPriorSpec = read_json(&prior_path)?;
        validate_pending_prior(mission, pending, &prior)?;
        if accepted_priors.get(&pending.call_index) != Some(pending) {
            return Err(
                "pending evaluator frontier differs from durable Accepted event".to_string(),
            );
        }
    }
    let failed_cancelled_call = match (&state.frontier, &state.terminal_evidence) {
        (
            Frontier::Done {
                outcome:
                    TerminalOutcome {
                        status: LoopRunStatusWire::Failed,
                        ..
                    },
            },
            Some(artifact),
        ) => {
            let path = verify_artifact(output_root, artifact)?;
            let evidence: TerminalDecisionEvidence = read_json(&path)?;
            evidence
                .cancelled_pending
                .map(|cancelled| cancelled.pending.call_index)
        }
        _ => None,
    };
    for (call_index, accepted) in &accepted_priors {
        let completed = state
            .iterations
            .iter()
            .filter(|iteration| iteration.call_index == *call_index)
            .count();
        let pending = matches!(
            &state.frontier,
            Frontier::Evaluate { pending } if pending == accepted
        ) as usize;
        let budget_cancelled = usize::from(
            completed == 0
                && pending == 0
                && matches!(
                    &state.frontier,
                    Frontier::Done { outcome }
                        if outcome.status == LoopRunStatusWire::BudgetExhausted
                            && accepted_priors.keys().next_back() == Some(call_index)
                ),
        );
        let invalid_feedback_cancelled = usize::from(
            completed == 0 && pending == 0 && failed_cancelled_call == Some(*call_index),
        );
        if completed + pending + budget_cancelled + invalid_feedback_cancelled != 1 {
            return Err(format!(
                "durable Accepted LLM call {call_index} is neither exactly completed, pending, nor terminally cancelled"
            ));
        }
    }
    for failure in &state.failures {
        if let Some(stdout) = &failure.stdout {
            verify_artifact(output_root, stdout)?;
        }
        if let Some(stderr) = &failure.stderr {
            verify_artifact(output_root, stderr)?;
        }
    }
    if state
        .pause
        .as_ref()
        .is_some_and(|pause| !state.failures.iter().any(|failure| failure == pause))
    {
        return Err("loop pause is not backed by durable failure history".to_string());
    }
    Ok(recovered)
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

    fn test_prediction_observation(tick_ts: chrono::DateTime<Utc>) -> crate::FactorObservation {
        crate::FactorObservation {
            event_id: "event-1".to_string(),
            symbol: "BTCUSDT".to_string(),
            tick_ts,
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
        assert!(paths
            .iter()
            .any(|path| path.contains("monday-prediction-evaluator")));
        assert!(paths
            .iter()
            .any(|path| path.contains("monday-prediction-research")));
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
        assert_initial_state_exists: bool,
        advance_clock_on_candidate_millis: u64,
    }

    fn fake_feedback(prior: &LlmPriorSpec, verdict: &str) -> PredictionResearchFeedback {
        let candidates = prior
            .probability_blends
            .iter()
            .cloned()
            .map(
                |probability_blend| crate::PredictionResearchCandidateFeedback {
                    model: format!("q_llm_{}", probability_blend.name),
                    hypothesis: probability_blend.hypothesis.clone(),
                    probability_blend,
                    verdict: verdict.to_string(),
                    reason_codes: if verdict == "keep" {
                        Vec::new()
                    } else {
                        vec!["brier_gate_failed".to_string()]
                    },
                    metrics: serde_json::json!({"withheld_from_llm": true}),
                },
            )
            .collect();
        PredictionResearchFeedback {
            schema_version: "prediction_research_feedback.v1".to_string(),
            mission_id: prior.mission_id.clone().expect("mission id"),
            target: prior.target.clone().expect("target"),
            symbols: prior.symbols.clone(),
            horizon: prior.horizon.clone().expect("horizon"),
            data_snapshot_id: prior.data_snapshot_id.clone().expect("snapshot id"),
            prompt_snapshot_id: prior.prompt_snapshot_id.clone().expect("prompt id"),
            search_policy_snapshot_id: prior.search_policy_snapshot_id.clone().expect("policy id"),
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
            let Some(prior) = request.prior.as_ref().map(|prior| &prior.value) else {
                if self.assert_initial_state_exists {
                    let output_root = request
                        .artifact_dir
                        .parent()
                        .and_then(Path::parent)
                        .expect("baseline attempt is nested under output/baseline");
                    assert!(output_root.join("prediction-loop-state.json").is_file());
                    assert!(output_root.join("run-budget").is_dir());
                }
                return PredictionEvaluationOutput::success(
                    None,
                    "baseline ok".to_string(),
                    String::new(),
                );
            };
            if self.advance_clock_on_candidate_millis > 0 {
                advance_test_clock(self.advance_clock_on_candidate_millis);
            }
            if self.fail_candidate_once {
                self.fail_candidate_once = false;
                return PredictionEvaluationOutput::failure(
                    "transient evaluator failure".to_string(),
                    String::new(),
                    "transient evaluator failure".to_string(),
                );
            }
            PredictionEvaluationOutput::success(
                Some(fake_feedback(prior, self.verdict)),
                "candidate evaluation ok".to_string(),
                String::new(),
            )
        }
    }

    struct InvalidFeedbackEvaluator {
        calls: usize,
    }

    impl PredictionEvaluator for InvalidFeedbackEvaluator {
        fn evaluate(
            &mut self,
            request: &PredictionEvaluationRequest,
            _timeout: Duration,
        ) -> PredictionEvaluationOutput {
            self.calls += 1;
            let Some(prior) = request.prior.as_ref().map(|prior| &prior.value) else {
                return PredictionEvaluationOutput::success(
                    None,
                    "baseline ok".to_string(),
                    String::new(),
                );
            };
            let mut feedback = fake_feedback(prior, "keep");
            feedback.mission_id.push_str("-tampered");
            PredictionEvaluationOutput::success(
                Some(feedback),
                "invalid candidate feedback".to_string(),
                String::new(),
            )
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
                pm_book_snapshots: test_prediction_books(tick_ts),
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

    fn empty_test_loop_state() -> PredictionLoopState {
        PredictionLoopState {
            schema_version: PREDICTION_LOOP_STATE_SCHEMA_VERSION.to_string(),
            identity: RunIdentity {
                mission_id: "ledger-test".to_string(),
                mission_hash: format!("sha256:{}", "1".repeat(64)),
                symbol: "BTC".to_string(),
                data_snapshot_id: format!("sha256:{}", "2".repeat(64)),
                snapshot_hash: "snapshot".to_string(),
                prompt_snapshot_id: format!("sha256:{}", "3".repeat(64)),
                search_policy_snapshot_id: format!("sha256:{}", "4".repeat(64)),
            },
            elapsed_millis: 0,
            run_budget_evidence: None,
            budget_deadline_unix_millis: 0,
            baseline: None,
            llm_ledger: LlmLedgerCursor::default(),
            llm_attempts: Vec::new(),
            iterations: Vec::new(),
            failures: Vec::new(),
            terminal_evidence: None,
            frontier: Frontier::Propose,
            pause: None,
        }
    }

    fn append_test_started_call(root: &Path, state: &mut PredictionLoopState) -> PathBuf {
        let call_dir = root.join("llm-calls/call-001");
        let prompt = write_content_addressed_text(root, &call_dir, "llm-prompt", "test prompt")
            .expect("write test prompt");
        append_llm_transition(
            root,
            state,
            LlmAttemptRecord {
                call_index: 1,
                prompt,
                response: None,
                provider: None,
                model: None,
                usage: None,
                status: LlmAttemptStatus::Started,
                error: None,
            },
            Frontier::Propose,
            None,
        )
        .expect("append Started event");
        call_dir
    }

    #[test]
    fn durable_llm_ledger_restores_reserved_call_budget_after_state_rollback() {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-ledger-rollback-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut state = empty_test_loop_state();
        append_test_started_call(&root, &mut state);

        state.llm_attempts.clear();
        state.llm_ledger = LlmLedgerCursor::default();
        assert!(reconcile_llm_ledger(&mut state, &root).expect("replay durable call reservation"));
        assert_eq!(
            state.calls_used(),
            1,
            "durable Started event spends the call budget"
        );
        assert_eq!(state.llm_attempts[0].status, LlmAttemptStatus::Started);
        verify_llm_call_directories(&state, &root).expect("replayed call directory binding");
        fs::remove_dir_all(root).expect("remove ledger fixture");
    }

    #[test]
    fn orphan_prompt_reservation_is_bound_without_another_provider_call() {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-orphan-prompt-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mission = mission();
        let mut state = empty_test_loop_state();
        let call_dir = root.join("llm-calls/call-001");

        fs::create_dir_all(&call_dir).expect("create empty orphan reservation");
        assert!(
            !reconcile_orphan_call_reservation(&mut state, &mission, &root)
                .expect("remove empty crash reservation")
        );
        assert!(!call_dir.exists());

        let prompt = build_prediction_prompt(&mission, mission.search_budget.max_candidates, &[]);
        write_content_addressed_text(&root, &call_dir, "llm-prompt", &prompt)
            .expect("write prompt before simulated ledger crash");
        assert!(
            reconcile_orphan_call_reservation(&mut state, &mission, &root)
                .expect("bind durable orphan prompt")
        );
        assert_eq!(state.calls_used(), 1);
        assert_eq!(state.llm_attempts[0].status, LlmAttemptStatus::Started);
        verify_llm_call_directories(&state, &root).expect("orphan prompt is ledger-bound");

        assert!(recover_interrupted_llm_attempt(&mut state, &root)
            .expect("finish interrupted reservation without network I/O"));
        assert_eq!(state.calls_used(), 1);
        assert_eq!(state.llm_attempts[0].status, LlmAttemptStatus::Abandoned);
        fs::remove_dir_all(root).expect("remove orphan prompt fixture");
    }

    #[test]
    fn orphan_llm_response_is_recovered_without_another_provider_call() {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-ledger-response-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut state = empty_test_loop_state();
        let call_dir = append_test_started_call(&root, &mut state);
        write_content_addressed_json(
            &root,
            &call_dir,
            "llm-response",
            &LlmResponseEvidence {
                raw_response: proposal_json(),
                provider: "fake-grok-builder".to_string(),
                model: "fake-model".to_string(),
                usage: serde_json::json!({"total_tokens": 12}),
            },
        )
        .expect("write orphan response");

        verify_llm_call_directories(&state, &root).expect("single orphan response is recoverable");
        assert!(recover_interrupted_llm_attempt(&mut state, &root).expect("recover response"));
        assert_eq!(state.calls_used(), 1);
        assert_eq!(state.llm_attempts[0].status, LlmAttemptStatus::Responded);
        assert_eq!(
            state.llm_attempts[0].provider.as_deref(),
            Some("fake-grok-builder")
        );
        assert_eq!(
            load_llm_ledger(&root, &state.identity)
                .expect("load ledger")
                .len(),
            2
        );
        verify_llm_call_directories(&state, &root).expect("recovered response is bound");
        fs::remove_dir_all(root).expect("remove ledger fixture");
    }

    #[test]
    fn llm_ledger_rejects_content_tampering() {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-ledger-tamper-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut state = empty_test_loop_state();
        append_test_started_call(&root, &mut state);
        let event_path = fs::read_dir(root.join("llm-ledger"))
            .expect("read ledger")
            .next()
            .expect("ledger event")
            .expect("ledger entry")
            .path();
        fs::write(&event_path, b"{}\n").expect("tamper ledger event");

        assert!(load_llm_ledger(&root, &state.identity)
            .expect_err("tampered ledger must fail closed")
            .contains("parse LLM ledger event"));
        fs::remove_dir_all(root).expect("remove ledger fixture");
    }

    #[test]
    fn rust_loop_runs_baseline_proposal_evaluation_and_deterministic_keep() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: false,
            verdict: "keep",
            assert_initial_state_exists: true,
            advance_clock_on_candidate_millis: 0,
        };

        let summary = run_or_resume(
            mission.clone(),
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("complete Rust loop");

        assert_eq!(summary.status, LoopRunStatus::Kept);
        assert_eq!(summary.llm_calls_used, 1);
        assert_eq!(summary.candidates_evaluated, 1);
        assert_eq!(summary.keep_models, ["q_llm_binance_context"]);
        assert_eq!(client.calls, 1);
        assert_eq!(evaluator.calls, 2);

        let mut rolled_back: PredictionLoopState =
            read_json(&output_dir.join("prediction-loop-state.json")).expect("read terminal state");
        rolled_back.iterations.clear();
        rolled_back.terminal_evidence = None;
        rolled_back.frontier = Frontier::Propose;
        assert!(
            reconcile_terminal_evidence(&mut rolled_back, &mission, &output_dir)
                .expect("recover terminal decision")
        );
        assert_eq!(rolled_back.iterations.len(), 1);
        assert!(matches!(rolled_back.frontier, Frontier::Done { .. }));
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn invalid_evaluator_feedback_resumes_from_a_bound_failed_terminal() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = InvalidFeedbackEvaluator { calls: 0 };

        let failed = run_or_resume(
            mission.clone(),
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("invalid feedback reaches a durable Failed terminal");
        assert_eq!(failed.status, LoopRunStatus::Failed);
        assert_eq!(failed.llm_calls_used, 1);
        assert_eq!(client.calls, 1);
        assert_eq!(evaluator.calls, 2);

        let resumed = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("Failed terminal resumes without re-evaluation");
        assert_eq!(resumed.status, LoopRunStatus::Failed);
        assert_eq!(resumed.llm_calls_used, 1);
        assert_eq!(client.calls, 1, "resume must not call the provider again");
        assert_eq!(evaluator.calls, 2, "resume must not rerun the evaluator");
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
            verdict: "keep",
            assert_initial_state_exists: false,
            advance_clock_on_candidate_millis: 0,
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
        assert_eq!(resumed.status, LoopRunStatus::Kept);
        assert_eq!(resumed.llm_calls_used, 1);
        assert_eq!(client.calls, 1, "resume must not call the LLM again");
        assert_eq!(evaluator.calls, 3);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn durable_evaluator_feedback_is_replayed_after_state_checkpoint_crash() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: true,
            verdict: "keep",
            assert_initial_state_exists: false,
            advance_clock_on_candidate_millis: 0,
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
        let state_path = output_dir.join("prediction-loop-state.json");
        let state: PredictionLoopState = read_json(&state_path).expect("read paused state");
        let pending = match state.frontier {
            Frontier::Evaluate { pending } => pending,
            _ => panic!("expected pending evaluator frontier"),
        };
        let prior_path = verify_artifact(&output_dir, &pending.prior).expect("prior evidence");
        let prior: LlmPriorSpec = read_json(&prior_path).expect("read pending prior");
        let evaluator_parent = output_dir
            .join("iterations")
            .join(format!("iteration-{:03}", pending.iteration))
            .join("evaluator-attempts");
        let completed_attempt = next_attempt_dir(&evaluator_parent).expect("orphan attempt dir");
        persist_evaluation_output(
            &output_dir,
            &completed_attempt,
            Some(&fake_feedback(&prior, "keep")),
            "durable evaluator stdout",
            "",
        )
        .expect("persist completed evidence before simulated crash");

        let resumed = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("recover durable evaluator evidence");
        assert_eq!(resumed.status, LoopRunStatus::Kept);
        assert_eq!(client.calls, 1);
        assert_eq!(
            evaluator.calls, 2,
            "durable feedback must replay without another evaluator invocation"
        );
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
            verdict: "keep",
            assert_initial_state_exists: false,
            advance_clock_on_candidate_millis: 0,
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
        let mut ledger_statuses = fs::read_dir(output_dir.join("llm-ledger"))
            .expect("LLM ledger directory")
            .map(|entry| {
                let path = entry.expect("ledger entry").path();
                read_json::<LlmLedgerEvent>(&path)
                    .expect("typed ledger event")
                    .attempt
                    .status
            })
            .collect::<Vec<_>>();
        ledger_statuses.sort_by_key(|status| match status {
            LlmAttemptStatus::Started => 0,
            LlmAttemptStatus::Responded => 1,
            LlmAttemptStatus::Invalid => 2,
            _ => 3,
        });
        assert_eq!(
            ledger_statuses,
            [
                LlmAttemptStatus::Started,
                LlmAttemptStatus::Responded,
                LlmAttemptStatus::Invalid,
            ]
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn immutable_deadline_rejects_elapsed_budget_rollback() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Err("provider unavailable".to_string())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: false,
            verdict: "keep",
            assert_initial_state_exists: false,
            advance_clock_on_candidate_millis: 0,
        };
        let paused = run_or_resume(
            mission.clone(),
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("provider failure pauses loop");
        assert_eq!(paused.status, LoopRunStatus::Paused);

        let state_path = output_dir.join("prediction-loop-state.json");
        let mut rolled_back: PredictionLoopState = read_json(&state_path).expect("read state");
        rolled_back.elapsed_millis = 0;
        rolled_back.budget_deadline_unix_millis = rolled_back
            .budget_deadline_unix_millis
            .saturating_add(60_000);
        atomic_write_json(&state_path, &rolled_back).expect("simulate budget rollback");

        let error = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect_err("immutable deadline must reject rollback");
        assert!(error.contains("immutable deadline"));
        assert_eq!(client.calls, 1, "rollback must not authorize another call");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn bootstrap_checkpoint_reconstructs_budget_without_resetting_deadline() {
        let root = std::env::temp_dir().join(format!(
            "ploy-prediction-budget-bootstrap-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("create fixture");
        let mission = mission();
        let max_millis = mission.search_budget.max_seconds * 1_000;
        let expected_deadline = now_unix_millis() + max_millis;
        let mut state = empty_test_loop_state();
        state.budget_deadline_unix_millis = expected_deadline;

        assert!(reconcile_run_budget_evidence(&mut state, &mission, &root)
            .expect("rebuild budget evidence after first-checkpoint crash"));
        assert_eq!(state.budget_deadline_unix_millis, expected_deadline);
        let (_, evidence) = load_run_budget_evidence(&root, &state.identity)
            .expect("load evidence")
            .expect("budget evidence");
        assert_eq!(evidence.deadline_unix_millis, expected_deadline);
        assert_eq!(evidence.started_unix_millis, expected_deadline - max_millis);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn accepted_frontier_rollback_cannot_spend_another_llm_call() {
        let (snapshot_dir, output_dir, mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: true,
            verdict: "keep",
            assert_initial_state_exists: false,
            advance_clock_on_candidate_millis: 0,
        };
        run_or_resume(
            mission.clone(),
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("pause accepted prior");

        let state_path = output_dir.join("prediction-loop-state.json");
        let mut rolled_back: PredictionLoopState = read_json(&state_path).expect("read state");
        rolled_back.frontier = Frontier::Propose;
        rolled_back.pause = None;
        atomic_write_json(&state_path, &rolled_back).expect("simulate frontier rollback");
        let recovered = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("durable Accepted frontier must recover");
        assert_eq!(recovered.status, LoopRunStatus::Kept);
        assert_eq!(client.calls, 1, "rollback must not spend another LLM call");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn evaluator_keep_after_immutable_deadline_is_not_promoted() {
        let (snapshot_dir, output_dir, mut mission) = governed_test_fixture();
        let root = output_dir.parent().expect("fixture root").to_path_buf();
        let _clock = TestClockGuard::freeze(1_700_000_000_000);
        mission.search_budget.max_seconds = 1;
        let mut client = FakeProposalClient {
            calls: 0,
            responses: VecDeque::from([Ok(proposal_json())]),
        };
        let mut evaluator = FakeEvaluator {
            calls: 0,
            fail_candidate_once: false,
            verdict: "keep",
            assert_initial_state_exists: false,
            advance_clock_on_candidate_millis: 1_100,
        };

        let summary = run_or_resume(
            mission,
            &snapshot_dir,
            &output_dir,
            &mut client,
            &mut evaluator,
        )
        .expect("late evaluator reaches budget terminal");
        assert_eq!(summary.status, LoopRunStatus::BudgetExhausted);
        assert!(summary.keep_models.is_empty());
        assert_eq!(summary.candidates_evaluated, 1);
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
