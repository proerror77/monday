//! Prediction-market adapter for the shared deterministic UCT kernel.
//!
//! The adapter owns typed probability-blend candidates and prediction-domain
//! evidence. The shared kernel owns only tree mechanics. Settlement training
//! evidence is the sole reward source; held-out settlement and token execution
//! evidence are returned for analysis but never enter backpropagation.

use std::collections::{BTreeMap, BTreeSet};

use hft_search_kernel::{
    backpropagate, select_expandable, validate_tree, DeterministicRng, UctNode, UctStats,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::autofactor::LlmProbabilityBlendSpec;
use crate::factors_v2::SettlementProbabilityComponentProfile;
use crate::prediction_loop::{
    current_prediction_policy_snapshot_id, validate_prediction_mission, validate_probability_blend,
    validate_sha256_id, PredictionResearchMission, ProposedProbabilityBlend,
};
use crate::prediction_mission_v3::{
    prediction_mission_v3_sha256, validate_prediction_mission_v3, AdmittedPredictionMissionV3,
    AdmittedPredictionTask, PredictionProductSymbol, PredictionResearchMissionV3,
};

const CHECKPOINT_VERSION: u32 = 4;
const MUTATION_STEP: f64 = 0.25;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMctsIdentity {
    pub mission_id: String,
    pub data_snapshot_id: String,
    pub symbol: String,
    pub horizon: String,
    #[serde(default)]
    pub task: PredictionMctsTask,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed_mission: Option<PredictionMctsSealedMissionIdentity>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionMctsTask {
    #[default]
    SettlementProbability,
    UpExecution {
        prediction_horizon_secs: u32,
    },
    DownExecution {
        prediction_horizon_secs: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredictionMctsSealedMissionIdentity {
    mission_sha256: String,
    cohort_manifest_id: String,
    partition_digest: String,
    causal_projection_policy_id: String,
    snapshot_contract_id: String,
    snapshot_hash: String,
    search_policy_snapshot_id: String,
}

impl PredictionMctsIdentity {
    pub fn from_mission(mission: &PredictionResearchMission) -> Result<Self, String> {
        validate_prediction_mission(mission, &current_prediction_policy_snapshot_id())?;
        Ok(Self {
            mission_id: mission.mission_id.clone(),
            data_snapshot_id: mission.data_snapshot_id.clone(),
            symbol: mission.symbols[0].clone(),
            horizon: mission.horizon.clone(),
            task: PredictionMctsTask::SettlementProbability,
            sealed_mission: None,
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_admitted_mission(
        mission: &AdmittedPredictionMissionV3,
    ) -> Result<Self, String> {
        let task = match mission.task {
            AdmittedPredictionTask::SettlementProbability => {
                PredictionMctsTask::SettlementProbability
            }
            AdmittedPredictionTask::UpExecution {
                prediction_horizon_secs,
            } => PredictionMctsTask::UpExecution {
                prediction_horizon_secs,
            },
            AdmittedPredictionTask::DownExecution {
                prediction_horizon_secs,
            } => PredictionMctsTask::DownExecution {
                prediction_horizon_secs,
            },
        };
        let identity = Self {
            mission_id: mission.mission_id.clone(),
            data_snapshot_id: mission.snapshot_contract_id.clone(),
            symbol: match mission.product.symbol {
                PredictionProductSymbol::Btc => "BTC",
            }
            .to_string(),
            horizon: match mission.product.event_horizon_secs {
                300 => "5m",
                other => return Err(format!("unsupported Mission v4 event horizon {other}s")),
            }
            .to_string(),
            task,
            sealed_mission: Some(PredictionMctsSealedMissionIdentity {
                mission_sha256: mission.mission_sha256.clone(),
                cohort_manifest_id: mission.cohort_manifest_id.clone(),
                partition_digest: mission.partition_digest.clone(),
                causal_projection_policy_id: mission.causal_projection_policy_id.clone(),
                snapshot_contract_id: mission.snapshot_contract_id.clone(),
                snapshot_hash: mission.snapshot_hash.clone(),
                search_policy_snapshot_id: mission.search_policy_snapshot_id.clone(),
            }),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.mission_id.trim().is_empty()
            || self.mission_id.trim() != self.mission_id
            || !matches!(self.symbol.as_str(), "BTC" | "SOL")
            || self.horizon != "5m"
            || !self.data_snapshot_id.starts_with("sha256:")
            || self.data_snapshot_id.len() != 71
        {
            return Err("invalid prediction MCTS identity".to_string());
        }
        match self.task {
            PredictionMctsTask::SettlementProbability => {}
            PredictionMctsTask::UpExecution {
                prediction_horizon_secs,
            }
            | PredictionMctsTask::DownExecution {
                prediction_horizon_secs,
            } if matches!(prediction_horizon_secs, 5 | 10 | 15 | 30) => {}
            _ => return Err("invalid prediction MCTS task identity".to_string()),
        }
        match &self.sealed_mission {
            None if self.task != PredictionMctsTask::SettlementProbability => {
                return Err("execution MCTS identity requires a sealed Mission v4".to_string())
            }
            None => {}
            Some(sealed) => {
                for (value, field) in [
                    (&sealed.mission_sha256, "sealed mission"),
                    (&sealed.cohort_manifest_id, "sealed cohort manifest"),
                    (&sealed.partition_digest, "sealed partition"),
                    (
                        &sealed.causal_projection_policy_id,
                        "sealed causal projection policy",
                    ),
                    (&sealed.snapshot_contract_id, "sealed snapshot contract"),
                    (&sealed.search_policy_snapshot_id, "sealed search policy"),
                ] {
                    validate_sha256_id(value, field)?;
                }
                if sealed.snapshot_contract_id != self.data_snapshot_id
                    || sealed.snapshot_hash.len() != 16
                    || !sealed
                        .snapshot_hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err("sealed prediction MCTS identity is inconsistent".to_string());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn sealed_mission_sha256(&self) -> Option<&str> {
        self.sealed_mission
            .as_ref()
            .map(|sealed| sealed.mission_sha256.as_str())
    }

    pub(crate) fn reject_unadmitted_legacy_bridge(&self) -> Result<(), String> {
        if self.sealed_mission.is_some() {
            return Err(
                "sealed prediction MCTS identity requires an authenticated Mission v4 runner"
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionExpansionSource {
    Baseline,
    DeterministicMutation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMctsCandidate {
    pub candidate_id: String,
    pub identity: PredictionMctsIdentity,
    pub probability_blend_sha256: String,
    pub source: PredictionExpansionSource,
    pub probability_blend: LlmProbabilityBlendSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettlementTrainingEvidence {
    pub candidate_id: String,
    pub identity: PredictionMctsIdentity,
    pub probability_blend_sha256: String,
    pub training_cohort_id: String,
    pub event_count: usize,
    pub mean_brier_score: f64,
    pub mean_log_loss: f64,
}

impl SettlementTrainingEvidence {
    fn reward(&self) -> Result<f64, String> {
        if self.training_cohort_id.trim().is_empty()
            || self.training_cohort_id.trim() != self.training_cohort_id
            || self.event_count == 0
            || !self.mean_brier_score.is_finite()
            || !(0.0..=1.0).contains(&self.mean_brier_score)
            || !self.mean_log_loss.is_finite()
            || self.mean_log_loss < 0.0
        {
            return Err("invalid training-cohort settlement evidence".to_string());
        }
        Ok(-(self.mean_brier_score + self.mean_log_loss))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTrainingEvidence {
    pub candidate_id: String,
    pub identity: PredictionMctsIdentity,
    pub probability_blend_sha256: String,
    pub training_cohort_id: String,
    pub event_count: usize,
    pub prediction_horizon_secs: u32,
    pub mean_fill_rate: f64,
    pub mean_fee_usd: f64,
    pub mean_entry_slippage_bps: f64,
    pub mean_exit_slippage_bps: f64,
    pub mean_capacity_usd: f64,
    pub mean_reprice_pnl: f64,
}

impl ExecutionTrainingEvidence {
    fn reward(&self) -> Result<f64, String> {
        if self.training_cohort_id.trim().is_empty()
            || self.training_cohort_id.trim() != self.training_cohort_id
            || self.event_count == 0
            || !matches!(self.prediction_horizon_secs, 5 | 10 | 15 | 30)
            || !self.mean_fill_rate.is_finite()
            || !(0.0..=1.0).contains(&self.mean_fill_rate)
            || !self.mean_fee_usd.is_finite()
            || self.mean_fee_usd < 0.0
            || !self.mean_entry_slippage_bps.is_finite()
            || !self.mean_exit_slippage_bps.is_finite()
            || !self.mean_capacity_usd.is_finite()
            || self.mean_capacity_usd < 0.0
            || !self.mean_reprice_pnl.is_finite()
        {
            return Err("invalid training-cohort execution evidence".to_string());
        }
        Ok(self.mean_reprice_pnl)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PredictionMctsTrainingEvidence {
    SettlementProbability(SettlementTrainingEvidence),
    UpExecution(ExecutionTrainingEvidence),
    DownExecution(ExecutionTrainingEvidence),
}

impl PredictionMctsTrainingEvidence {
    pub(crate) fn candidate_id(&self) -> &str {
        match self {
            Self::SettlementProbability(evidence) => &evidence.candidate_id,
            Self::UpExecution(evidence) | Self::DownExecution(evidence) => &evidence.candidate_id,
        }
    }

    pub(crate) fn identity(&self) -> &PredictionMctsIdentity {
        match self {
            Self::SettlementProbability(evidence) => &evidence.identity,
            Self::UpExecution(evidence) | Self::DownExecution(evidence) => &evidence.identity,
        }
    }

    pub(crate) fn probability_blend_sha256(&self) -> &str {
        match self {
            Self::SettlementProbability(evidence) => &evidence.probability_blend_sha256,
            Self::UpExecution(evidence) | Self::DownExecution(evidence) => {
                &evidence.probability_blend_sha256
            }
        }
    }

    pub(crate) fn reward(&self) -> Result<f64, String> {
        match self {
            Self::SettlementProbability(evidence) => evidence.reward(),
            Self::UpExecution(evidence) | Self::DownExecution(evidence) => evidence.reward(),
        }
    }

    pub(crate) fn matches_task(&self, task: PredictionMctsTask) -> bool {
        match (task, self) {
            (PredictionMctsTask::SettlementProbability, Self::SettlementProbability(_)) => true,
            (
                PredictionMctsTask::UpExecution {
                    prediction_horizon_secs: expected,
                },
                Self::UpExecution(evidence),
            ) => expected == evidence.prediction_horizon_secs,
            (
                PredictionMctsTask::DownExecution {
                    prediction_horizon_secs: expected,
                },
                Self::DownExecution(evidence),
            ) => expected == evidence.prediction_horizon_secs,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMctsEvaluation {
    pub training: PredictionMctsTrainingEvidence,
}

pub trait PredictionTrainingEvaluator {
    fn evaluate_training(
        &self,
        candidate: &PredictionMctsCandidate,
    ) -> Result<PredictionMctsEvaluation, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WeightComponent {
    MarketMidpoint,
    ChainlinkDigital,
    DistanceLobVol,
    EventSurface,
    ExistingModel,
}

impl WeightComponent {
    const ALL: [Self; 5] = [
        Self::MarketMidpoint,
        Self::ChainlinkDigital,
        Self::DistanceLobVol,
        Self::EventSurface,
        Self::ExistingModel,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::MarketMidpoint => "market_midpoint",
            Self::ChainlinkDigital => "chainlink_digital",
            Self::DistanceLobVol => "distance_lob_vol",
            Self::EventSurface => "event_surface",
            Self::ExistingModel => "existing_model",
        }
    }
}

const MARKET_MIDPOINT_ONLY: [WeightComponent; 1] = [WeightComponent::MarketMidpoint];

fn eligible_components(
    profile: SettlementProbabilityComponentProfile,
) -> &'static [WeightComponent] {
    match profile {
        SettlementProbabilityComponentProfile::FullSurface => &WeightComponent::ALL,
        SettlementProbabilityComponentProfile::MarketMidpointOnly => &MARKET_MIDPOINT_ONLY,
    }
}

fn deterministic_actions(profile: SettlementProbabilityComponentProfile) -> Vec<WeightComponent> {
    match profile {
        SettlementProbabilityComponentProfile::FullSurface => eligible_components(profile).to_vec(),
        SettlementProbabilityComponentProfile::MarketMidpointOnly => Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredictionNode {
    blend: LlmProbabilityBlendSpec,
    source: PredictionExpansionSource,
    parent: Option<usize>,
    children: Vec<usize>,
    unexpanded_actions: Vec<WeightComponent>,
    depth: usize,
    visits: u64,
    total_reward: f64,
    best_reward: Option<f64>,
}

impl UctNode for PredictionNode {
    fn parent(&self) -> Option<usize> {
        self.parent
    }

    fn children(&self) -> &[usize] {
        &self.children
    }

    fn is_expandable(&self) -> bool {
        !self.unexpanded_actions.is_empty()
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn stats(&self) -> Result<UctStats, hft_search_kernel::UctError> {
        UctStats::from_parts(self.visits, self.total_reward, self.best_reward)
    }

    fn replace_stats(&mut self, stats: UctStats) {
        self.visits = stats.visits();
        self.total_reward = stats.total_reward();
        self.best_reward = stats.best_reward();
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredictionMctsConfig {
    identity: PredictionMctsIdentity,
    seed: u64,
    exploration: f64,
    max_depth: usize,
    max_candidates: usize,
    baseline_blend_sha256: String,
    #[serde(default)]
    component_profile: SettlementProbabilityComponentProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMctsCheckpoint {
    version: u32,
    config: PredictionMctsConfig,
    rng: DeterministicRng,
    nodes: Vec<PredictionNode>,
    seen_blends: BTreeSet<[u64; 5]>,
    pending: BTreeMap<String, usize>,
    proposed: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PredictionMctsCheckpointArtifact {
    pub version: u32,
    pub identity: PredictionMctsIdentity,
    pub proposed: usize,
    pub nodes: Vec<PredictionMctsCheckpointNodeArtifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PredictionMctsCheckpointNodeArtifact {
    pub node_id: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_node_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_candidate_id: Option<String>,
    pub source: PredictionExpansionSource,
    pub probability_blend_sha256: String,
    pub depth: usize,
    pub visits: u64,
    pub total_reward: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_reward: Option<f64>,
}

impl PredictionMctsCheckpoint {
    fn validate(&self) -> Result<(), String> {
        if self.version != CHECKPOINT_VERSION
            || !self.config.exploration.is_finite()
            || self.config.exploration < 0.0
            || self.config.max_depth == 0
            || self.config.max_candidates == 0
            || self.proposed > self.config.max_candidates
        {
            return Err("invalid prediction MCTS checkpoint configuration".to_string());
        }
        self.config.identity.validate()?;
        if self.nodes.first().is_none_or(|root| {
            blend_digest(&root.blend) != self.config.baseline_blend_sha256
                || root.source != PredictionExpansionSource::Baseline
        }) {
            return Err("prediction MCTS baseline identity mismatch".to_string());
        }
        let market_midpoint_only = self.config.component_profile
            == SettlementProbabilityComponentProfile::MarketMidpointOnly;
        if market_midpoint_only
            && (self.proposed > 1
                || self.nodes.len() != 1
                || self.seen_blends.len() != 1
                || !self.nodes[0].children.is_empty()
                || !self.nodes[0].unexpanded_actions.is_empty())
        {
            return Err(
                "market-midpoint-only search evaluates only its canonical baseline".to_string(),
            );
        }
        validate_tree(&self.nodes, 0, self.config.max_depth)
            .map_err(|error| format!("invalid prediction MCTS tree: {error}"))?;
        for (node_id, node) in self.nodes.iter().enumerate() {
            validate_blend_spec(&node.blend)?;
            if !self.seen_blends.contains(&blend_key(&node.blend))
                || (node.depth == self.config.max_depth && !node.unexpanded_actions.is_empty())
            {
                return Err(format!("invalid prediction MCTS node {node_id}"));
            }
            if node_id > 0 {
                let parent = &self.nodes[node.parent.expect("validated tree has a parent")];
                let valid_source = match node.source {
                    PredictionExpansionSource::DeterministicMutation => {
                        eligible_components(self.config.component_profile)
                            .iter()
                            .copied()
                            .any(|component| {
                                mutate_blend(&parent.blend, component, node.depth)
                                    .is_ok_and(|blend| blend == node.blend)
                            })
                    }
                    PredictionExpansionSource::Baseline => false,
                };
                if !valid_source {
                    return Err(format!("invalid prediction MCTS node source {node_id}"));
                }
            }
            for component in &node.unexpanded_actions {
                if !eligible_components(self.config.component_profile).contains(component) {
                    return Err("unauthorized prediction MCTS component action".to_string());
                }
            }
        }
        let mut pending_nodes = BTreeSet::new();
        let mut pending_ordinals = BTreeSet::new();
        for (candidate_id, node_id) in &self.pending {
            let Some(node) = self.nodes.get(*node_id) else {
                return Err("invalid prediction MCTS pending candidate map".to_string());
            };
            let prefix = candidate_id_prefix(&self.config, &node.blend);
            let ordinal = candidate_id
                .strip_prefix(&prefix)
                .and_then(|value| value.parse::<usize>().ok());
            let canonical_baseline_pending = *node_id == 0
                && self.config.component_profile
                    == SettlementProbabilityComponentProfile::MarketMidpointOnly
                && self.pending.len() == 1
                && self.proposed == 1
                && ordinal == Some(0)
                && node.visits == 0
                && node.total_reward == 0.0
                && node.best_reward.is_none();
            if (*node_id == 0 && !canonical_baseline_pending)
                || !pending_nodes.insert(*node_id)
                || ordinal.is_none_or(|ordinal| {
                    ordinal >= self.proposed || !pending_ordinals.insert(ordinal)
                })
            {
                return Err("invalid prediction MCTS pending candidate map".to_string());
            }
        }
        Ok(())
    }

    pub fn read_only_artifact(&self) -> Result<PredictionMctsCheckpointArtifact, String> {
        self.validate()?;
        let pending_by_node = self
            .pending
            .iter()
            .map(|(candidate_id, node_id)| (*node_id, candidate_id.clone()))
            .collect::<BTreeMap<_, _>>();
        Ok(PredictionMctsCheckpointArtifact {
            version: self.version,
            identity: self.config.identity.clone(),
            proposed: self.proposed,
            nodes: self
                .nodes
                .iter()
                .enumerate()
                .map(|(node_id, node)| PredictionMctsCheckpointNodeArtifact {
                    node_id,
                    parent_node_id: node.parent,
                    pending_candidate_id: pending_by_node.get(&node_id).cloned(),
                    source: node.source,
                    probability_blend_sha256: blend_digest(&node.blend),
                    depth: node.depth,
                    visits: node.visits,
                    total_reward: node.total_reward,
                    best_reward: node.best_reward,
                })
                .collect(),
        })
    }

    pub(crate) fn validate_pending_candidate(
        &self,
        candidate: &PredictionMctsCandidate,
    ) -> Result<(), String> {
        self.validate()?;
        let node_id = *self
            .pending
            .get(&candidate.candidate_id)
            .ok_or_else(|| "unknown prediction MCTS pending candidate".to_string())?;
        let pending = &self.nodes[node_id];
        let expected_digest = blend_digest(&pending.blend);
        if candidate.identity != self.config.identity
            || candidate.source != pending.source
            || candidate.probability_blend_sha256 != expected_digest
            || blend_digest(&candidate.probability_blend) != expected_digest
        {
            return Err("prediction MCTS pending candidate payload mismatch".to_string());
        }
        Ok(())
    }
}

pub struct PredictionMctsEngine {
    checkpoint: PredictionMctsCheckpoint,
}

impl PredictionMctsEngine {
    pub fn new(
        mission: &PredictionResearchMission,
        baseline: ProposedProbabilityBlend,
        seed: u64,
        exploration: f64,
        max_depth: usize,
    ) -> Result<Self, String> {
        Self::new_with_component_profile(
            mission,
            baseline,
            seed,
            exploration,
            max_depth,
            SettlementProbabilityComponentProfile::FullSurface,
        )
    }

    pub fn new_with_component_profile(
        mission: &PredictionResearchMission,
        baseline: ProposedProbabilityBlend,
        seed: u64,
        exploration: f64,
        max_depth: usize,
        component_profile: SettlementProbabilityComponentProfile,
    ) -> Result<Self, String> {
        let identity = PredictionMctsIdentity::from_mission(mission)?;
        Self::new_with_identity_and_component_profile(
            mission,
            identity,
            baseline,
            seed,
            exploration,
            max_depth,
            component_profile,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_identity_and_component_profile(
        mission: &PredictionResearchMission,
        identity: PredictionMctsIdentity,
        baseline: ProposedProbabilityBlend,
        seed: u64,
        exploration: f64,
        max_depth: usize,
        component_profile: SettlementProbabilityComponentProfile,
    ) -> Result<Self, String> {
        identity.reject_unadmitted_legacy_bridge()?;
        Self::new_core(
            mission,
            identity,
            baseline,
            seed,
            exploration,
            max_depth,
            component_profile,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_admitted_mission(
        mission: &PredictionResearchMissionV3,
        admitted: &AdmittedPredictionMissionV3,
        bridge: &PredictionResearchMission,
        baseline: ProposedProbabilityBlend,
        seed: u64,
        exploration: f64,
        max_depth: usize,
        component_profile: SettlementProbabilityComponentProfile,
    ) -> Result<Self, String> {
        validate_prediction_mission_v3(mission)?;
        if prediction_mission_v3_sha256(mission)? != admitted.mission_sha256
            || mission.search_budget != bridge.search_budget
            || mission.search_policy_snapshot_id != current_prediction_policy_snapshot_id()
            || admitted.search_policy_snapshot_id != mission.search_policy_snapshot_id
            || bridge.search_policy_snapshot_id != mission.search_policy_snapshot_id
        {
            return Err("admitted Mission v4 does not match its MCTS bridge".to_string());
        }
        Self::new_core(
            bridge,
            PredictionMctsIdentity::from_admitted_mission(admitted)?,
            baseline,
            seed,
            exploration,
            max_depth,
            component_profile,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_core(
        mission: &PredictionResearchMission,
        identity: PredictionMctsIdentity,
        baseline: ProposedProbabilityBlend,
        seed: u64,
        exploration: f64,
        max_depth: usize,
        component_profile: SettlementProbabilityComponentProfile,
    ) -> Result<Self, String> {
        validate_prediction_mission(mission, &current_prediction_policy_snapshot_id())?;
        identity.validate()?;
        if identity.mission_id != mission.mission_id
            || identity.data_snapshot_id != mission.data_snapshot_id
        {
            return Err("prediction MCTS identity does not match its legacy bridge mission".into());
        }
        if mission.search_budget.max_candidates == 0 {
            return Err("prediction MCTS requires a non-zero candidate budget".to_string());
        }
        if !exploration.is_finite() || exploration < 0.0 || max_depth == 0 {
            return Err("invalid prediction MCTS configuration".to_string());
        }

        let baseline = validate_probability_blend(baseline)?;
        if !blend_allowed(component_profile, &baseline) {
            return Err(
                "prediction MCTS baseline uses an ineligible probability component".to_string(),
            );
        }
        let root_actions = deterministic_actions(component_profile);
        let seen_blends = BTreeSet::from([blend_key(&baseline)]);
        let checkpoint = PredictionMctsCheckpoint {
            version: CHECKPOINT_VERSION,
            config: PredictionMctsConfig {
                identity,
                seed: seed.max(1),
                exploration,
                max_depth,
                max_candidates: mission.search_budget.max_candidates,
                baseline_blend_sha256: blend_digest(&baseline),
                component_profile,
            },
            rng: DeterministicRng::new(seed),
            nodes: vec![PredictionNode {
                blend: baseline,
                source: PredictionExpansionSource::Baseline,
                parent: None,
                children: Vec::new(),
                unexpanded_actions: root_actions,
                depth: 0,
                visits: 0,
                total_reward: 0.0,
                best_reward: None,
            }],
            seen_blends,
            pending: BTreeMap::new(),
            proposed: 0,
        };
        checkpoint.validate()?;
        Ok(Self { checkpoint })
    }

    pub fn propose(&mut self) -> Result<Option<PredictionMctsCandidate>, String> {
        if self.checkpoint.proposed >= self.checkpoint.config.max_candidates {
            return Ok(None);
        }
        if self.checkpoint.config.component_profile
            == SettlementProbabilityComponentProfile::MarketMidpointOnly
        {
            if self.checkpoint.proposed == 0 {
                return Ok(Some(self.mark_candidate_pending(0)));
            }
            return Ok(None);
        }
        for _ in 0..256 {
            let Some(parent) = select_expandable(
                &self.checkpoint.nodes,
                0,
                self.checkpoint.config.exploration,
            )
            .map_err(|error| error.to_string())?
            else {
                return Ok(None);
            };
            let node_id = self.expand(parent)?;
            let blend = self.checkpoint.nodes[node_id].blend.clone();
            if !self.checkpoint.seen_blends.insert(blend_key(&blend)) {
                continue;
            }
            return Ok(Some(self.mark_candidate_pending(node_id)));
        }
        Err("prediction MCTS could not produce a novel candidate".to_string())
    }

    fn mark_candidate_pending(&mut self, node_id: usize) -> PredictionMctsCandidate {
        let blend = self.checkpoint.nodes[node_id].blend.clone();
        let candidate_id = format!(
            "{}{}",
            candidate_id_prefix(&self.checkpoint.config, &blend),
            self.checkpoint.proposed
        );
        self.checkpoint.proposed += 1;
        self.checkpoint
            .pending
            .insert(candidate_id.clone(), node_id);
        PredictionMctsCandidate {
            candidate_id,
            identity: self.checkpoint.config.identity.clone(),
            probability_blend_sha256: blend_digest(&blend),
            source: self.checkpoint.nodes[node_id].source,
            probability_blend: blend,
        }
    }

    pub fn evaluate_and_observe<E: PredictionTrainingEvaluator>(
        &mut self,
        evaluator: &E,
        candidate: &PredictionMctsCandidate,
    ) -> Result<PredictionMctsEvaluation, String> {
        let node_id = *self
            .checkpoint
            .pending
            .get(&candidate.candidate_id)
            .ok_or_else(|| "unknown prediction MCTS candidate".to_string())?;
        let pending = &self.checkpoint.nodes[node_id];
        let expected_digest = blend_digest(&pending.blend);
        if candidate.identity != self.checkpoint.config.identity
            || candidate.source != pending.source
            || candidate.probability_blend_sha256 != expected_digest
            || blend_digest(&candidate.probability_blend) != expected_digest
        {
            return Err("prediction candidate payload digest mismatch".to_string());
        }
        let evaluation = evaluator.evaluate_training(candidate)?;
        self.observe(&candidate.candidate_id, &evaluation)?;
        Ok(evaluation)
    }

    pub fn observe(
        &mut self,
        candidate_id: &str,
        evaluation: &PredictionMctsEvaluation,
    ) -> Result<(), String> {
        let node_id = *self
            .checkpoint
            .pending
            .get(candidate_id)
            .ok_or_else(|| "unknown prediction MCTS candidate".to_string())?;
        let expected_blend_sha256 = blend_digest(&self.checkpoint.nodes[node_id].blend);
        if !evaluation
            .training
            .matches_task(self.checkpoint.config.identity.task)
            || evaluation.training.candidate_id() != candidate_id
            || evaluation.training.identity() != &self.checkpoint.config.identity
            || evaluation.training.probability_blend_sha256() != expected_blend_sha256
        {
            return Err("training evidence identity does not match the candidate".to_string());
        }
        let reward = evaluation.training.reward()?;
        backpropagate(&mut self.checkpoint.nodes, 0, node_id, reward)
            .map_err(|error| format!("prediction MCTS backpropagation failed: {error}"))?;
        self.checkpoint.pending.remove(candidate_id);
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<PredictionMctsCheckpoint, String> {
        self.checkpoint.validate()?;
        Ok(self.checkpoint.clone())
    }

    pub fn restore_checkpoint(
        &mut self,
        checkpoint: PredictionMctsCheckpoint,
    ) -> Result<(), String> {
        checkpoint.validate()?;
        if checkpoint.config != self.checkpoint.config {
            return Err("prediction MCTS checkpoint configuration mismatch".to_string());
        }
        self.checkpoint = checkpoint;
        Ok(())
    }

    fn expand(&mut self, parent_id: usize) -> Result<usize, String> {
        let action_index = self
            .checkpoint
            .rng
            .index(self.checkpoint.nodes[parent_id].unexpanded_actions.len());
        let component = self.checkpoint.nodes[parent_id]
            .unexpanded_actions
            .swap_remove(action_index);
        let depth = self.checkpoint.nodes[parent_id].depth + 1;
        let blend = mutate_blend(&self.checkpoint.nodes[parent_id].blend, component, depth)?;
        let node_id = self.checkpoint.nodes.len();
        self.checkpoint.nodes.push(PredictionNode {
            blend,
            source: PredictionExpansionSource::DeterministicMutation,
            parent: Some(parent_id),
            children: Vec::new(),
            unexpanded_actions: if depth < self.checkpoint.config.max_depth {
                deterministic_actions(self.checkpoint.config.component_profile)
            } else {
                Vec::new()
            },
            depth,
            visits: 0,
            total_reward: 0.0,
            best_reward: None,
        });
        self.checkpoint.nodes[parent_id].children.push(node_id);
        Ok(node_id)
    }
}

fn blend_allowed(
    profile: SettlementProbabilityComponentProfile,
    blend: &LlmProbabilityBlendSpec,
) -> bool {
    profile == SettlementProbabilityComponentProfile::FullSurface
        || (blend.market_midpoint_weight > 0.0
            && blend.chainlink_digital_weight == 0.0
            && blend.distance_lob_vol_weight == 0.0
            && blend.event_surface_weight == 0.0
            && blend.existing_model_weight == 0.0)
}

fn validate_blend_spec(blend: &LlmProbabilityBlendSpec) -> Result<(), String> {
    validate_probability_blend(ProposedProbabilityBlend {
        name: blend.name.clone(),
        hypothesis: blend.hypothesis.clone(),
        market_midpoint_weight: blend.market_midpoint_weight,
        chainlink_digital_weight: blend.chainlink_digital_weight,
        distance_lob_vol_weight: blend.distance_lob_vol_weight,
        event_surface_weight: blend.event_surface_weight,
        existing_model_weight: blend.existing_model_weight,
    })?;
    Ok(())
}

fn mutate_blend(
    parent: &LlmProbabilityBlendSpec,
    component: WeightComponent,
    depth: usize,
) -> Result<LlmProbabilityBlendSpec, String> {
    let mut proposed = ProposedProbabilityBlend {
        name: format!("mcts_d{depth}_{}", component.name()),
        hypothesis: format!(
            "Increase the {} probability component within the fixed mission and training cohort",
            component.name()
        ),
        market_midpoint_weight: parent.market_midpoint_weight,
        chainlink_digital_weight: parent.chainlink_digital_weight,
        distance_lob_vol_weight: parent.distance_lob_vol_weight,
        event_surface_weight: parent.event_surface_weight,
        existing_model_weight: parent.existing_model_weight,
    };
    match component {
        WeightComponent::MarketMidpoint => proposed.market_midpoint_weight += MUTATION_STEP,
        WeightComponent::ChainlinkDigital => proposed.chainlink_digital_weight += MUTATION_STEP,
        WeightComponent::DistanceLobVol => proposed.distance_lob_vol_weight += MUTATION_STEP,
        WeightComponent::EventSurface => proposed.event_surface_weight += MUTATION_STEP,
        WeightComponent::ExistingModel => proposed.existing_model_weight += MUTATION_STEP,
    }
    validate_probability_blend(proposed)
}

fn blend_key(blend: &LlmProbabilityBlendSpec) -> [u64; 5] {
    [
        blend.market_midpoint_weight.to_bits(),
        blend.chainlink_digital_weight.to_bits(),
        blend.distance_lob_vol_weight.to_bits(),
        blend.event_surface_weight.to_bits(),
        blend.existing_model_weight.to_bits(),
    ]
}

fn blend_digest(blend: &LlmProbabilityBlendSpec) -> String {
    let bytes = serde_json::to_vec(blend).expect("validated probability blend is serializable");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn candidate_id_prefix(config: &PredictionMctsConfig, blend: &LlmProbabilityBlendSpec) -> String {
    format!(
        "{}:{}:{}:{}:{}:mcts-",
        config.identity.mission_id,
        config.identity.data_snapshot_id,
        config.identity.symbol,
        config.identity.horizon,
        blend_digest(blend)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prediction_loop::{
        research_brief_snapshot_id, PredictionSearchBudget, PREDICTION_LOOP_TARGET,
        PREDICTION_MISSION_SCHEMA_VERSION,
    };

    fn mission() -> PredictionResearchMission {
        let mut mission = PredictionResearchMission {
            schema_version: PREDICTION_MISSION_SCHEMA_VERSION.to_string(),
            mission_id: "btc-5m-shared-mcts".to_string(),
            lane: "prediction_market".to_string(),
            objective: "Estimate BTC five-minute settlement probability".to_string(),
            hypothesis_scope: "Mutate only typed probability blend weights".to_string(),
            mutable_scope: vec!["probability_blend_weights".to_string()],
            data_snapshot_id: format!("sha256:{}", "1".repeat(64)),
            target: PREDICTION_LOOP_TARGET.to_string(),
            symbols: vec!["BTC".to_string()],
            horizon: "5m".to_string(),
            time_cohort_boundary_ms: 300_000,
            prompt_snapshot_id: String::new(),
            search_policy_snapshot_id: current_prediction_policy_snapshot_id(),
            search_budget: PredictionSearchBudget {
                max_candidates: 8,
                max_seconds: 60,
            },
        };
        mission.prompt_snapshot_id = research_brief_snapshot_id(&mission);
        mission
    }

    fn blend(name: &str) -> ProposedProbabilityBlend {
        ProposedProbabilityBlend {
            name: name.to_string(),
            hypothesis: "bounded probability blend".to_string(),
            market_midpoint_weight: 1.0,
            chainlink_digital_weight: 1.0,
            distance_lob_vol_weight: 1.0,
            event_surface_weight: 1.0,
            existing_model_weight: 1.0,
        }
    }

    fn market_midpoint_blend(name: &str) -> ProposedProbabilityBlend {
        ProposedProbabilityBlend {
            name: name.to_string(),
            hypothesis: "market midpoint only".to_string(),
            market_midpoint_weight: 1.0,
            chainlink_digital_weight: 0.0,
            distance_lob_vol_weight: 0.0,
            event_surface_weight: 0.0,
            existing_model_weight: 0.0,
        }
    }

    struct FakeEvaluator;

    impl PredictionTrainingEvaluator for FakeEvaluator {
        fn evaluate_training(
            &self,
            candidate: &PredictionMctsCandidate,
        ) -> Result<PredictionMctsEvaluation, String> {
            Ok(PredictionMctsEvaluation {
                training: PredictionMctsTrainingEvidence::SettlementProbability(
                    SettlementTrainingEvidence {
                        candidate_id: candidate.candidate_id.clone(),
                        identity: candidate.identity.clone(),
                        probability_blend_sha256: candidate.probability_blend_sha256.clone(),
                        training_cohort_id: "train-cohort-0".to_string(),
                        event_count: 12,
                        mean_brier_score: 0.2,
                        mean_log_loss: 0.3,
                    },
                ),
            })
        }
    }

    #[test]
    fn market_midpoint_profile_evaluates_only_its_canonical_baseline() {
        let mut mission = mission();
        mission.search_budget.max_candidates = 3;
        mission.prompt_snapshot_id = research_brief_snapshot_id(&mission);
        let mut engine = PredictionMctsEngine::new_with_component_profile(
            &mission,
            market_midpoint_blend("baseline"),
            7,
            1.4,
            3,
            SettlementProbabilityComponentProfile::MarketMidpointOnly,
        )
        .expect("market-midpoint-only engine");

        let candidate = engine
            .propose()
            .expect("canonical baseline proposal")
            .expect("canonical baseline candidate");
        assert_eq!(candidate.source, PredictionExpansionSource::Baseline);
        assert_eq!(
            candidate.probability_blend,
            market_midpoint_blend("baseline").into()
        );
        engine.checkpoint().expect("durable pending baseline");
        let evaluation = FakeEvaluator
            .evaluate_training(&candidate)
            .expect("training evidence");
        engine
            .observe(&candidate.candidate_id, &evaluation)
            .expect("observe canonical baseline");
        let before = engine.checkpoint().expect("observed baseline checkpoint");
        assert!(engine.propose().unwrap().is_none());
        assert_eq!(engine.checkpoint().unwrap(), before);

        let mut forged = before.clone();
        forged.proposed = 2;
        let error = engine
            .restore_checkpoint(forged)
            .expect_err("expanded midpoint-only checkpoint must fail closed");
        assert!(error.contains("canonical baseline"), "{error}");
        assert_eq!(engine.checkpoint().unwrap(), before);
    }

    #[test]
    fn typed_adapter_resumes_deterministically_through_shared_kernel() {
        let mission = mission();
        let mut engine = PredictionMctsEngine::new(&mission, blend("baseline"), 7, 1.4, 3).unwrap();
        let candidate = engine.propose().unwrap().unwrap();
        assert_eq!(candidate.identity.mission_id, mission.mission_id);
        assert_eq!(
            candidate.identity.data_snapshot_id,
            mission.data_snapshot_id
        );
        assert_eq!(candidate.identity.symbol, "BTC");
        assert_eq!(candidate.identity.horizon, "5m");
        let evaluation = engine
            .evaluate_and_observe(&FakeEvaluator, &candidate)
            .unwrap();
        assert!(matches!(
            evaluation.training,
            PredictionMctsTrainingEvidence::SettlementProbability(_)
        ));
        assert_eq!(engine.checkpoint.nodes[0].visits, 1);
        assert_eq!(engine.checkpoint.nodes[0].total_reward, -0.5);

        let checkpoint = engine.checkpoint().unwrap();
        let mut restored =
            PredictionMctsEngine::new(&mission, blend("baseline"), 7, 1.4, 3).unwrap();
        restored.restore_checkpoint(checkpoint).unwrap();
        let expected = engine.propose().unwrap().unwrap();
        let actual = restored.propose().unwrap().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(restored.checkpoint.nodes, engine.checkpoint.nodes);
    }

    #[test]
    fn sealed_v4_identity_binds_the_execution_side_and_admission_digests() {
        use crate::prediction_mission_v3::{
            AdmittedPredictionMissionV3, AdmittedPredictionTask, PredictionAuthorityProfile,
            PredictionProductIdentity, PredictionProductSymbol, PredictionRunMode,
        };

        let admitted = AdmittedPredictionMissionV3 {
            mission_id: "btc-5m-up".to_string(),
            mission_sha256: format!("sha256:{}", "1".repeat(64)),
            product: PredictionProductIdentity {
                symbol: PredictionProductSymbol::Btc,
                event_horizon_secs: 300,
            },
            task: AdmittedPredictionTask::UpExecution {
                prediction_horizon_secs: 15,
            },
            run_mode: PredictionRunMode::ResearchTrial,
            authority_profile: PredictionAuthorityProfile::PolymarketChainlinkBaseline,
            cohort_manifest_id: format!("sha256:{}", "2".repeat(64)),
            partition_digest: format!("sha256:{}", "3".repeat(64)),
            causal_projection_policy_id: format!("sha256:{}", "4".repeat(64)),
            snapshot_contract_id: format!("sha256:{}", "5".repeat(64)),
            snapshot_hash: "6".repeat(16),
            search_policy_snapshot_id: format!("sha256:{}", "7".repeat(64)),
        };

        let identity = PredictionMctsIdentity::from_admitted_mission(&admitted)
            .expect("admitted Mission v4 becomes a sealed MCTS identity");

        assert_eq!(
            identity.task,
            PredictionMctsTask::UpExecution {
                prediction_horizon_secs: 15
            }
        );
        assert_eq!(identity.symbol, "BTC");
        assert_eq!(identity.horizon, "5m");
        assert_eq!(
            identity
                .sealed_mission
                .as_ref()
                .expect("v4 identity carries sealed admission")
                .partition_digest,
            admitted.partition_digest
        );

        let mut legacy_bridge = mission();
        legacy_bridge.mission_id = admitted.mission_id.clone();
        legacy_bridge.data_snapshot_id = admitted.snapshot_contract_id.clone();
        legacy_bridge.prompt_snapshot_id = research_brief_snapshot_id(&legacy_bridge);
        let error = PredictionMctsEngine::new_with_identity_and_component_profile(
            &legacy_bridge,
            identity.clone(),
            market_midpoint_blend("baseline"),
            7,
            1.4,
            3,
            SettlementProbabilityComponentProfile::MarketMidpointOnly,
        )
        .err()
        .expect("sealed identity cannot consume an unadmitted legacy bridge");
        assert!(error.contains("authenticated Mission v4 runner"));
    }

    #[test]
    fn checkpoint_round_trips_through_json() {
        let mission = mission();
        let engine = PredictionMctsEngine::new(&mission, blend("baseline"), 7, 1.4, 3).unwrap();
        let checkpoint = engine.checkpoint().unwrap();
        let bytes = serde_json::to_vec(&checkpoint).unwrap();
        let restored: PredictionMctsCheckpoint = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(restored, checkpoint);
        let mut old = checkpoint;
        old.version = 3;
        let mut resumed =
            PredictionMctsEngine::new(&mission, blend("baseline"), 7, 1.4, 3).unwrap();
        assert!(resumed.restore_checkpoint(old).is_err());
    }

    #[test]
    fn training_evidence_is_the_only_observation_surface() {
        let mission = mission();
        let mut left = PredictionMctsEngine::new(&mission, blend("baseline"), 9, 1.4, 2).unwrap();
        let mut right = PredictionMctsEngine::new(&mission, blend("baseline"), 9, 1.4, 2).unwrap();
        let left_candidate = left.propose().unwrap().unwrap();
        let right_candidate = right.propose().unwrap().unwrap();
        assert_eq!(left_candidate, right_candidate);
        let left_evaluation = FakeEvaluator.evaluate_training(&left_candidate).unwrap();
        let right_evaluation = left_evaluation.clone();
        left.observe(&left_candidate.candidate_id, &left_evaluation)
            .unwrap();
        right
            .observe(&right_candidate.candidate_id, &right_evaluation)
            .unwrap();
        assert_eq!(left.checkpoint().unwrap(), right.checkpoint().unwrap());
    }

    #[test]
    fn settlement_candidate_rejects_execution_metric_substitution() {
        let mission = mission();
        let mut engine = PredictionMctsEngine::new(&mission, blend("baseline"), 9, 1.4, 2).unwrap();
        let candidate = engine.propose().unwrap().unwrap();
        let before = engine.checkpoint().unwrap();
        let evaluation = PredictionMctsEvaluation {
            training: PredictionMctsTrainingEvidence::UpExecution(ExecutionTrainingEvidence {
                candidate_id: candidate.candidate_id.clone(),
                identity: candidate.identity.clone(),
                probability_blend_sha256: candidate.probability_blend_sha256.clone(),
                training_cohort_id: "train-before-boundary".to_string(),
                event_count: 12,
                prediction_horizon_secs: 15,
                mean_fill_rate: 0.8,
                mean_fee_usd: 0.01,
                mean_entry_slippage_bps: 1.0,
                mean_exit_slippage_bps: 2.0,
                mean_capacity_usd: 15.0,
                mean_reprice_pnl: 0.2,
            }),
        };

        assert!(evaluation
            .training
            .matches_task(PredictionMctsTask::UpExecution {
                prediction_horizon_secs: 15,
            }));
        assert!(!evaluation
            .training
            .matches_task(PredictionMctsTask::DownExecution {
                prediction_horizon_secs: 15,
            }));

        assert!(engine
            .observe(&candidate.candidate_id, &evaluation)
            .is_err());
        assert_eq!(engine.checkpoint().unwrap(), before);
    }

    #[test]
    fn mismatched_training_evidence_fails_without_state_mutation() {
        let mission = mission();
        let mut engine = PredictionMctsEngine::new(&mission, blend("baseline"), 5, 1.4, 2).unwrap();
        let candidate = engine.propose().unwrap().unwrap();
        let before = engine.checkpoint().unwrap();
        let mut evaluation = FakeEvaluator.evaluate_training(&candidate).unwrap();
        match &mut evaluation.training {
            PredictionMctsTrainingEvidence::SettlementProbability(evidence) => {
                evidence.identity.data_snapshot_id = format!("sha256:{}", "2".repeat(64));
            }
            _ => panic!("fake evaluator produces settlement evidence"),
        }
        assert!(engine
            .observe(&candidate.candidate_id, &evaluation)
            .is_err());
        assert_eq!(engine.checkpoint().unwrap(), before);
    }

    #[test]
    fn forged_candidate_payload_fails_without_state_mutation() {
        let mission = mission();
        let mut engine = PredictionMctsEngine::new(&mission, blend("baseline"), 5, 1.4, 2).unwrap();
        let candidate = engine.propose().unwrap().unwrap();
        let before = engine.checkpoint().unwrap();
        struct MustNotEvaluate;
        impl PredictionTrainingEvaluator for MustNotEvaluate {
            fn evaluate_training(
                &self,
                _candidate: &PredictionMctsCandidate,
            ) -> Result<PredictionMctsEvaluation, String> {
                panic!("forged payload must be rejected before evaluation")
            }
        }
        let mut forged_payload = candidate.clone();
        forged_payload.probability_blend.market_midpoint_weight += 1.0;
        forged_payload.probability_blend_sha256 = blend_digest(&forged_payload.probability_blend);
        assert!(engine
            .evaluate_and_observe(&MustNotEvaluate, &forged_payload)
            .is_err());
        let mut forged_source = candidate.clone();
        forged_source.source = PredictionExpansionSource::Baseline;
        assert!(engine
            .evaluate_and_observe(&MustNotEvaluate, &forged_source)
            .is_err());
        let mut forged_identity = candidate.clone();
        forged_identity.identity.symbol = "SOL".to_string();
        assert!(engine
            .evaluate_and_observe(&MustNotEvaluate, &forged_identity)
            .is_err());
        let mut forged_id = candidate;
        forged_id.candidate_id.push_str("-forged");
        assert!(engine
            .evaluate_and_observe(&MustNotEvaluate, &forged_id)
            .is_err());
        assert_eq!(engine.checkpoint().unwrap(), before);
    }

    #[test]
    fn checkpoint_rejects_a_forged_pending_candidate_key() {
        let mission = mission();
        let mut engine = PredictionMctsEngine::new(&mission, blend("baseline"), 5, 1.4, 2).unwrap();
        engine.propose().unwrap().unwrap();
        let before = engine.checkpoint().unwrap();
        let mut forged = before.clone();
        let (candidate_id, node_id) = forged.pending.pop_first().unwrap();
        forged
            .pending
            .insert(format!("forged:{candidate_id}"), node_id);
        assert!(engine.restore_checkpoint(forged).is_err());
        assert_eq!(engine.checkpoint().unwrap(), before);
    }
}
