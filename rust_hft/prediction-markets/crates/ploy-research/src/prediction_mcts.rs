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
use crate::prediction_loop::{
    current_prediction_policy_snapshot_id, validate_prediction_mission,
    validate_prediction_proposal, PredictionProposal, PredictionResearchMission,
    ProposedProbabilityBlend,
};

const CHECKPOINT_VERSION: u32 = 1;
const MUTATION_STEP: f64 = 0.25;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMctsIdentity {
    pub mission_id: String,
    pub data_snapshot_id: String,
    pub symbol: String,
    pub horizon: String,
}

impl PredictionMctsIdentity {
    pub fn from_mission(mission: &PredictionResearchMission) -> Result<Self, String> {
        validate_prediction_mission(mission, &current_prediction_policy_snapshot_id())?;
        Ok(Self {
            mission_id: mission.mission_id.clone(),
            data_snapshot_id: mission.data_snapshot_id.clone(),
            symbol: mission.symbols[0].clone(),
            horizon: mission.horizon.clone(),
        })
    }

    fn validate(&self) -> Result<(), String> {
        if self.mission_id.trim().is_empty()
            || self.mission_id.trim() != self.mission_id
            || !matches!(self.symbol.as_str(), "BTC" | "SOL")
            || self.horizon != "5m"
            || !self.data_snapshot_id.starts_with("sha256:")
            || self.data_snapshot_id.len() != 71
        {
            return Err("invalid prediction MCTS identity".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionExpansionSource {
    Baseline,
    DeterministicMutation,
    LlmAdvisor,
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
pub struct HeldOutSettlementEvidence {
    pub event_count: usize,
    pub mean_brier_score: f64,
    pub mean_log_loss: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenExecutionEvidence {
    pub fill_rate: f64,
    pub mean_slippage_bps: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionExecutionEvidence {
    pub up: TokenExecutionEvidence,
    pub down: TokenExecutionEvidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMctsEvaluation {
    pub training_settlement: SettlementTrainingEvidence,
    pub held_out_settlement: Option<HeldOutSettlementEvidence>,
    pub execution: Option<PredictionExecutionEvidence>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ExpansionAction {
    Increase { component: WeightComponent },
    LlmAdvisor { blend: LlmProbabilityBlendSpec },
}

fn deterministic_actions() -> Vec<ExpansionAction> {
    WeightComponent::ALL
        .into_iter()
        .map(|component| ExpansionAction::Increase { component })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredictionNode {
    blend: LlmProbabilityBlendSpec,
    source: PredictionExpansionSource,
    parent: Option<usize>,
    children: Vec<usize>,
    unexpanded_actions: Vec<ExpansionAction>,
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
    llm_advisor_sha256: Vec<String>,
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
                        WeightComponent::ALL.into_iter().any(|component| {
                            mutate_blend(&parent.blend, component, node.depth)
                                .is_ok_and(|blend| blend == node.blend)
                        })
                    }
                    PredictionExpansionSource::LlmAdvisor => {
                        node.parent == Some(0)
                            && self
                                .config
                                .llm_advisor_sha256
                                .contains(&blend_digest(&node.blend))
                    }
                    PredictionExpansionSource::Baseline => false,
                };
                if !valid_source {
                    return Err(format!("invalid prediction MCTS node source {node_id}"));
                }
            }
            for action in &node.unexpanded_actions {
                if let ExpansionAction::LlmAdvisor { blend } = action {
                    if node_id != 0
                        || !self
                            .config
                            .llm_advisor_sha256
                            .contains(&blend_digest(blend))
                    {
                        return Err("unauthorized prediction MCTS LLM action".to_string());
                    }
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
            if *node_id == 0
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
}

pub struct PredictionMctsEngine {
    checkpoint: PredictionMctsCheckpoint,
}

impl PredictionMctsEngine {
    pub fn new(
        mission: &PredictionResearchMission,
        baseline: ProposedProbabilityBlend,
        llm_advice: Vec<ProposedProbabilityBlend>,
        seed: u64,
        exploration: f64,
        max_depth: usize,
    ) -> Result<Self, String> {
        let identity = PredictionMctsIdentity::from_mission(mission)?;
        if mission.search_budget.max_candidates == 0 {
            return Err("prediction MCTS requires a non-zero candidate budget".to_string());
        }
        if !llm_advice.is_empty() && mission.search_budget.max_llm_calls == 0 {
            return Err("LLM advice requires a non-zero governed LLM-call budget".to_string());
        }
        if !exploration.is_finite() || exploration < 0.0 || max_depth == 0 {
            return Err("invalid prediction MCTS configuration".to_string());
        }

        let baseline = validate_single_proposal(baseline)?;
        let advisors = if llm_advice.is_empty() {
            Vec::new()
        } else {
            validate_prediction_proposal(
                PredictionProposal {
                    probability_blends: llm_advice,
                },
                mission.search_budget.max_candidates,
            )?
        };
        let mut root_actions = deterministic_actions();
        let llm_advisor_sha256 = advisors.iter().map(blend_digest).collect();
        root_actions.extend(
            advisors
                .into_iter()
                .map(|blend| ExpansionAction::LlmAdvisor { blend }),
        );
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
                llm_advisor_sha256,
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

    pub fn propose(&mut self) -> Result<PredictionMctsCandidate, String> {
        if self.checkpoint.proposed >= self.checkpoint.config.max_candidates {
            return Err("prediction MCTS candidate budget is exhausted".to_string());
        }
        for _ in 0..256 {
            let parent = select_expandable(
                &self.checkpoint.nodes,
                0,
                self.checkpoint.config.exploration,
            )
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "prediction MCTS tree has no expandable node".to_string())?;
            let node_id = self.expand(parent)?;
            let blend = self.checkpoint.nodes[node_id].blend.clone();
            if !self.checkpoint.seen_blends.insert(blend_key(&blend)) {
                continue;
            }
            let probability_blend_sha256 = blend_digest(&blend);
            let candidate_id = format!(
                "{}{}",
                candidate_id_prefix(&self.checkpoint.config, &blend),
                self.checkpoint.proposed
            );
            self.checkpoint.proposed += 1;
            self.checkpoint
                .pending
                .insert(candidate_id.clone(), node_id);
            return Ok(PredictionMctsCandidate {
                candidate_id,
                identity: self.checkpoint.config.identity.clone(),
                probability_blend_sha256,
                source: self.checkpoint.nodes[node_id].source,
                probability_blend: blend,
            });
        }
        Err("prediction MCTS could not produce a novel candidate".to_string())
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
        if evaluation.training_settlement.candidate_id != candidate_id
            || evaluation.training_settlement.identity != self.checkpoint.config.identity
            || evaluation.training_settlement.probability_blend_sha256 != expected_blend_sha256
        {
            return Err("training evidence identity does not match the candidate".to_string());
        }
        let reward = evaluation.training_settlement.reward()?;
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
        let action = self.checkpoint.nodes[parent_id]
            .unexpanded_actions
            .swap_remove(action_index);
        let depth = self.checkpoint.nodes[parent_id].depth + 1;
        let (blend, source) = match action {
            ExpansionAction::Increase { component } => (
                mutate_blend(&self.checkpoint.nodes[parent_id].blend, component, depth)?,
                PredictionExpansionSource::DeterministicMutation,
            ),
            ExpansionAction::LlmAdvisor { blend } => {
                validate_blend_spec(&blend)?;
                (blend, PredictionExpansionSource::LlmAdvisor)
            }
        };
        let node_id = self.checkpoint.nodes.len();
        self.checkpoint.nodes.push(PredictionNode {
            blend,
            source,
            parent: Some(parent_id),
            children: Vec::new(),
            unexpanded_actions: if depth < self.checkpoint.config.max_depth {
                deterministic_actions()
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

fn validate_single_proposal(
    proposal: ProposedProbabilityBlend,
) -> Result<LlmProbabilityBlendSpec, String> {
    validate_prediction_proposal(
        PredictionProposal {
            probability_blends: vec![proposal],
        },
        1,
    )?
    .pop()
    .ok_or_else(|| "validated probability proposal is empty".to_string())
}

fn validate_blend_spec(blend: &LlmProbabilityBlendSpec) -> Result<(), String> {
    validate_single_proposal(ProposedProbabilityBlend {
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
    validate_single_proposal(proposed)
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
                max_llm_calls: 1,
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

    struct FakeEvaluator;

    impl PredictionTrainingEvaluator for FakeEvaluator {
        fn evaluate_training(
            &self,
            candidate: &PredictionMctsCandidate,
        ) -> Result<PredictionMctsEvaluation, String> {
            Ok(PredictionMctsEvaluation {
                training_settlement: SettlementTrainingEvidence {
                    candidate_id: candidate.candidate_id.clone(),
                    identity: candidate.identity.clone(),
                    probability_blend_sha256: candidate.probability_blend_sha256.clone(),
                    training_cohort_id: "train-cohort-0".to_string(),
                    event_count: 12,
                    mean_brier_score: 0.2,
                    mean_log_loss: 0.3,
                },
                held_out_settlement: Some(HeldOutSettlementEvidence {
                    event_count: 4,
                    mean_brier_score: 0.9,
                    mean_log_loss: 2.0,
                }),
                execution: Some(PredictionExecutionEvidence {
                    up: TokenExecutionEvidence {
                        fill_rate: 0.8,
                        mean_slippage_bps: 4.0,
                    },
                    down: TokenExecutionEvidence {
                        fill_rate: 0.7,
                        mean_slippage_bps: 5.0,
                    },
                }),
            })
        }
    }

    #[test]
    fn typed_adapter_resumes_deterministically_through_shared_kernel() {
        let mission = mission();
        let mut engine =
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![], 7, 1.4, 3).unwrap();
        let candidate = engine.propose().unwrap();
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
        assert!(evaluation.held_out_settlement.is_some());
        assert!(evaluation.execution.is_some());
        assert_eq!(engine.checkpoint.nodes[0].visits, 1);
        assert_eq!(engine.checkpoint.nodes[0].total_reward, -0.5);

        let checkpoint = engine.checkpoint().unwrap();
        let mut restored =
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![], 7, 1.4, 3).unwrap();
        restored.restore_checkpoint(checkpoint).unwrap();
        let expected = engine.propose().unwrap();
        let actual = restored.propose().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(restored.checkpoint.nodes, engine.checkpoint.nodes);
    }

    #[test]
    fn held_out_and_execution_outputs_cannot_change_reward() {
        let mission = mission();
        let mut left =
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![], 9, 1.4, 2).unwrap();
        let mut right =
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![], 9, 1.4, 2).unwrap();
        let left_candidate = left.propose().unwrap();
        let right_candidate = right.propose().unwrap();
        assert_eq!(left_candidate, right_candidate);
        let mut left_evaluation = FakeEvaluator.evaluate_training(&left_candidate).unwrap();
        let mut right_evaluation = left_evaluation.clone();
        left_evaluation.held_out_settlement = None;
        left_evaluation.execution = None;
        right_evaluation.held_out_settlement = Some(HeldOutSettlementEvidence {
            event_count: 1,
            mean_brier_score: f64::NAN,
            mean_log_loss: f64::INFINITY,
        });
        right_evaluation.execution = Some(PredictionExecutionEvidence {
            up: TokenExecutionEvidence {
                fill_rate: f64::NAN,
                mean_slippage_bps: f64::INFINITY,
            },
            down: TokenExecutionEvidence {
                fill_rate: -1.0,
                mean_slippage_bps: -1.0,
            },
        });
        left.observe(&left_candidate.candidate_id, &left_evaluation)
            .unwrap();
        right
            .observe(&right_candidate.candidate_id, &right_evaluation)
            .unwrap();
        assert_eq!(left.checkpoint().unwrap(), right.checkpoint().unwrap());
    }

    #[test]
    fn llm_advice_is_validated_before_becoming_an_expansion_action() {
        let mission = mission();
        let mut invalid = blend("advisor");
        invalid.market_midpoint_weight = -1.0;
        assert!(
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![invalid], 3, 1.4, 2,)
                .is_err()
        );
    }

    #[test]
    fn mismatched_training_evidence_fails_without_state_mutation() {
        let mission = mission();
        let mut engine =
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![], 5, 1.4, 2).unwrap();
        let candidate = engine.propose().unwrap();
        let before = engine.checkpoint().unwrap();
        let mut evaluation = FakeEvaluator.evaluate_training(&candidate).unwrap();
        evaluation.training_settlement.identity.data_snapshot_id =
            format!("sha256:{}", "2".repeat(64));
        assert!(engine
            .observe(&candidate.candidate_id, &evaluation)
            .is_err());
        assert_eq!(engine.checkpoint().unwrap(), before);
    }

    #[test]
    fn forged_candidate_payload_fails_without_state_mutation() {
        let mission = mission();
        let mut engine =
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![], 5, 1.4, 2).unwrap();
        let candidate = engine.propose().unwrap();
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
    fn checkpoint_cannot_inject_unapproved_llm_advice() {
        let mission = mission();
        let mut engine =
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![], 5, 1.4, 2).unwrap();
        let before = engine.checkpoint().unwrap();
        let mut forged = before.clone();
        forged.nodes[0]
            .unexpanded_actions
            .push(ExpansionAction::LlmAdvisor {
                blend: validate_single_proposal(blend("forged-advisor")).unwrap(),
            });
        assert!(engine.restore_checkpoint(forged).is_err());
        assert_eq!(engine.checkpoint().unwrap(), before);
    }

    #[test]
    fn checkpoint_rejects_a_forged_pending_candidate_key() {
        let mission = mission();
        let mut engine =
            PredictionMctsEngine::new(&mission, blend("baseline"), vec![], 5, 1.4, 2).unwrap();
        engine.propose().unwrap();
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
