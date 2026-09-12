//! Content-bound model artifacts, distinct from four-stage formula precommits.
use crate::{canonical_json_hash, CexResearchContentRefV1, FormulaEvaluatorConfig};
use hft_factor_dsl::model_program::FrozenFactorModelV1;
use serde::{Deserialize, Serialize};

pub const FROZEN_SUPERVISED_CANDIDATE_SCHEMA: &str = "monday.frozen_supervised_candidate.v1";
pub const INDEPENDENT_SELECTION_EVALUATOR_VERSION: &str = "cex-independent-selection-v1";
pub const FROZEN_FORMULA_SELECTION_PREFIX: &str = "cex-frozen-formula-";
pub const FROZEN_MODEL_SELECTION_PREFIX: &str = "cex-frozen-model-";

fn valid_frozen_selection_id(id: &str) -> bool {
    [FROZEN_MODEL_SELECTION_PREFIX, FROZEN_FORMULA_SELECTION_PREFIX]
        .into_iter()
        .any(|prefix| id.strip_prefix(prefix).is_some_and(crate::valid_content_sha256))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenSupervisedCandidateV1 {
    pub schema_version: String,
    pub artifact_id: String,
    pub source_candidate: CexResearchContentRefV1,
    pub source_model: CexResearchContentRefV1,
    pub source_factor_bank: CexResearchContentRefV1,
    pub source_fold: CexResearchContentRefV1,
    pub research_dataset: CexResearchContentRefV1,
    pub evaluation_protocol_sha256: String,
    pub program: FrozenFactorModelV1,
    pub evaluator_config: FormulaEvaluatorConfig,
}

impl FrozenSupervisedCandidateV1 {
    pub fn finalize(mut self) -> Result<Self, String> {
        self.artifact_id = self.expected_artifact_id()?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        for reference in [
            &self.source_candidate,
            &self.source_model,
            &self.source_factor_bank,
            &self.source_fold,
            &self.research_dataset,
        ] {
            reference.validate().map_err(|e| e.to_string())?;
        }
        self.program.validate()?;
        self.evaluator_config
            .validate()
            .map_err(|e| e.to_string())?;
        if self.schema_version != FROZEN_SUPERVISED_CANDIDATE_SCHEMA
            || self.evaluation_protocol_sha256.len() != 64
            || !self
                .evaluation_protocol_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.artifact_id != self.expected_artifact_id()?
        {
            return Err("invalid frozen supervised candidate identity".into());
        }
        Ok(())
    }

    pub fn validate_against_protocol(
        &self,
        protocol: &crate::EvaluationProtocolV1,
    ) -> Result<(), String> {
        self.validate()?;
        protocol.validate().map_err(|e| e.to_string())?;
        if self.evaluation_protocol_sha256 != protocol.content_hash().map_err(|e| e.to_string())?
            || self.program.observation_frequency_millis
                != protocol.labels.observation_frequency_millis
            || self.program.label_horizon_buckets != protocol.labels.horizon_buckets
            || self.program.cross_spread != protocol.costs.cross_spread
            || self.program.base_costs != frozen_decision_costs(protocol)
        {
            return Err("frozen model protocol, costs or clock changed".into());
        }
        Ok(())
    }

    pub fn validate_fitted_origin(
        &self,
        baseline: &crate::CexBaselineArtifactV1,
        bank: &crate::CexFactorBankRevisionV2,
    ) -> Result<(), String> {
        self.validate()?;
        baseline.validate().map_err(|e| e.to_string())?;
        bank.validate().map_err(|e| e.to_string())?;
        let fold = baseline.folds.last().ok_or("fitted model has no fold")?;
        if self.source_model.id != baseline.artifact_id
            || self.source_model.content_sha256
                != canonical_json_hash(baseline).map_err(|e| e.to_string())?
            || self.source_factor_bank.id != bank.revision_id
            || self.source_factor_bank.content_sha256
                != canonical_json_hash(bank).map_err(|e| e.to_string())?
            || fold.fold_id != self.source_fold
            || fold.model != self.program.model
            || baseline.factor_bank_revision_id != bank.revision_id
            || baseline.research_dataset != self.research_dataset
            || baseline.evaluation_policy.content_sha256 != self.evaluation_protocol_sha256
            || baseline.factor_ids.len() != self.program.factors.len()
            || baseline
                .factor_ids
                .iter()
                .zip(&self.program.factors)
                .any(|(id, factor)| {
                    bank.entries
                        .iter()
                        .find(|entry| &entry.factor_id == id)
                        .is_none_or(|entry| {
                            entry.canonical_ast != factor.ast
                                || (entry.orientation == crate::CexFactorOrientationV1::Negative)
                                    != factor.negative
                        })
                })
        {
            return Err(
                "frozen model differs from its original fitted parameters and ordered factors"
                    .into(),
            );
        }
        Ok(())
    }

    fn expected_artifact_id(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.artifact_id.clear();
        canonical_json_hash(&semantic)
            .map(|hash| format!("cex-frozen-model-{hash}"))
            .map_err(|e| e.to_string())
    }
}

pub fn frozen_decision_costs(
    protocol: &crate::EvaluationProtocolV1,
) -> hft_research_manifest::model::CexDecisionCostsV1 {
    let costs = &protocol.costs;
    hft_research_manifest::model::CexDecisionCostsV1 {
        one_way_cost_bps: costs.fee_bps.max(0.0) - costs.rebate_bps
            + costs.latency_bps.max(0.0)
            + costs.slippage_bps,
        funding_bps: costs.funding_bps,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelectionEntryV1 {
    pub source_operation_id: String,
    pub source_result_sha256: String,
    pub source_candidate: CexResearchContentRefV1,
    pub frozen_candidate: Option<CexResearchContentRefV1>,
    pub evaluation: Option<crate::CandidateEvaluation>,
    /// Structural admission failure, never a fabricated score or zero feature.
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelectionReportV1 {
    pub schema_version: String,
    pub artifact_id: String,
    pub final_grant_sha256: String,
    pub selection_view_sha256: String,
    pub entries: Vec<ModelSelectionEntryV1>,
    pub selected_candidate: Option<CexResearchContentRefV1>,
}

impl ModelSelectionReportV1 {
    pub fn new(
        grant: &crate::campaign_finalization::VerifiedCampaignFinalEvaluationGrant,
        mut entries: Vec<ModelSelectionEntryV1>,
    ) -> Result<Self, String> {
        entries.sort_by(|a, b| {
            (&a.source_operation_id, &a.source_candidate.id)
                .cmp(&(&b.source_operation_id, &b.source_candidate.id))
        });
        let mut report = Self {
            schema_version: "monday.model_selection_report.v1".into(),
            artifact_id: String::new(),
            final_grant_sha256: grant.content_sha256().into(),
            selection_view_sha256: grant
                .grant()
                .execution
                .evaluation_views
                .selection_view_sha256
                .clone(),
            entries,
            selected_candidate: None,
        };
        report.selected_candidate = report.winner();
        report.artifact_id = report.expected_id()?;
        report.validate_against(grant)?;
        Ok(report)
    }

    pub fn validate_against(
        &self,
        grant: &crate::campaign_finalization::VerifiedCampaignFinalEvaluationGrant,
    ) -> Result<(), String> {
        let definition = grant.grant();
        if self.schema_version != "monday.model_selection_report.v1"
            || self.artifact_id != self.expected_id()?
            || self.final_grant_sha256 != grant.content_sha256()
            || self.selection_view_sha256
                != definition.execution.evaluation_views.selection_view_sha256
            || self.entries.is_empty()
            || self.entries.len() > definition.max_candidates as usize
            || self.entries.windows(2).any(|pair| {
                (&pair[0].source_operation_id, &pair[0].source_candidate.id)
                    >= (&pair[1].source_operation_id, &pair[1].source_candidate.id)
            })
        {
            return Err("invalid final selection identity, ordering or budget".into());
        }
        let mut sources = std::collections::BTreeMap::new();
        for entry in &self.entries {
            entry
                .source_candidate
                .validate()
                .map_err(|e| e.to_string())?;
            if definition.selected_results.get(&entry.source_operation_id)
                != Some(&entry.source_result_sha256)
            {
                return Err("selection source is outside the closed family".into());
            }
            sources.insert(
                entry.source_operation_id.clone(),
                entry.source_result_sha256.clone(),
            );
            match (
                &entry.frozen_candidate,
                &entry.evaluation,
                &entry.rejection_reason,
            ) {
                (Some(candidate), Some(evaluation), None) => {
                    candidate.validate().map_err(|e| e.to_string())?;
                    if !valid_frozen_selection_id(&candidate.id) {
                        return Err("invalid frozen selection artifact path identity".into());
                    }
                    evaluation.validate().map_err(|e| e.to_string())?;
                    let formula_walk_forward = candidate
                        .id
                        .starts_with(FROZEN_FORMULA_SELECTION_PREFIX)
                        && evaluation.evaluator_version == crate::WALK_FORWARD_EVALUATOR_VERSION;
                    if (!formula_walk_forward
                        && evaluation.evaluator_version != INDEPENDENT_SELECTION_EVALUATOR_VERSION)
                        || evaluation.protocol_binding().map_err(|e| e.to_string())?.1
                            != definition.execution.evaluation_protocol_sha256
                    {
                        return Err(
                            "selection evaluation does not bind the reserved protocol".into()
                        );
                    }
                }
                (None, None, Some(reason)) if !reason.trim().is_empty() && reason.len() <= 4096 => {
                }
                _ => {
                    return Err(
                        "selection entry lacks evaluation or explicit admission rejection".into(),
                    )
                }
            }
        }
        if sources != definition.selected_results || self.selected_candidate != self.winner() {
            return Err(
                "selection omitted a settled source or changed its deterministic winner".into(),
            );
        }
        Ok(())
    }

    fn winner(&self) -> Option<CexResearchContentRefV1> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let evaluation = entry.evaluation.as_ref()?;
                evaluation
                    .passed
                    .then_some((entry.frozen_candidate.as_ref(), evaluation.score))
            })
            .filter_map(|(candidate, score)| candidate.map(|candidate| (candidate, score)))
            .max_by(|(a, score_a), (b, score_b)| {
                score_a.total_cmp(score_b).then_with(|| b.id.cmp(&a.id))
            })
            .map(|(candidate, _)| candidate.clone())
    }

    fn expected_id(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.artifact_id.clear();
        canonical_json_hash(&semantic)
            .map(|hash| format!("cex-independent-selection-{hash}"))
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenModelStrategyV1 {
    pub schema_version: String,
    pub mission_id: String,
    pub precommit_id: String,
    pub frozen: FrozenSupervisedCandidateV1,
    pub evaluation_protocol: crate::EvaluationProtocolV1,
    pub instrument_rules: hft_research_manifest::CexInstrumentRulesV2,
}

impl FrozenModelStrategyV1 {
    pub fn validate(&self) -> Result<(), crate::DomainError> {
        self.frozen
            .validate_against_protocol(&self.evaluation_protocol)
            .map_err(|_| crate::DomainError::InvalidStrategyBundle)?;
        self.instrument_rules
            .validate()
            .map_err(|_| crate::DomainError::InvalidStrategyBundle)?;
        if self.schema_version != "monday.frozen_model_strategy.v1"
            || self.mission_id.trim().is_empty()
            || self.precommit_id != format!("cex-final-precommit:{}", self.mission_id)
        {
            return Err(crate::DomainError::InvalidStrategyBundle);
        }
        Ok(())
    }

    pub fn runtime_contract(&self) -> Result<crate::CexRuntimeContractV1, crate::DomainError> {
        self.validate()?;
        Ok(crate::CexRuntimeContractV1 {
            zero_epsilon: 0.0,
            observation_frequency_millis: self.frozen.program.observation_frequency_millis,
            tick_size: self.instrument_rules.tick_size.clone(),
            step_size: self.instrument_rules.step_size.clone(),
            min_notional: self.instrument_rules.min_notional.clone(),
            costs: self.evaluation_protocol.costs.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFinalPrecommitV1 {
    pub schema_version: String,
    pub precommit_id: String,
    pub mission_id: String,
    pub family_id: String,
    pub final_grant: CexResearchContentRefV1,
    pub selection_report: CexResearchContentRefV1,
    pub source_result: CexResearchContentRefV1,
    pub final_candidate: CexResearchContentRefV1,
    pub frozen_candidate: CexResearchContentRefV1,
    pub replay_receipt: CexResearchContentRefV1,
    pub evaluation_protocol: CexResearchContentRefV1,
    pub dataset_manifest_id: hft_research_manifest::ManifestId,
    pub holdout_id: String,
    pub implementation_source_revision: String,
}

impl ModelFinalPrecommitV1 {
    pub fn validate(&self) -> Result<(), crate::DomainError> {
        for reference in [
            &self.final_grant,
            &self.selection_report,
            &self.source_result,
            &self.final_candidate,
            &self.frozen_candidate,
            &self.replay_receipt,
            &self.evaluation_protocol,
        ] {
            reference.validate()?;
        }
        self.dataset_manifest_id
            .validate()
            .map_err(|_| crate::DomainError::InvalidStrategyBundle)?;
        if self.schema_version != "monday.model_final_precommit.v1"
            || self.mission_id.trim().is_empty()
            || self.family_id.trim().is_empty()
            || self.holdout_id.trim().is_empty()
            || self.precommit_id != format!("cex-final-precommit:{}", self.mission_id)
            || !crate::valid_source_revision(&self.implementation_source_revision)
        {
            return Err(crate::DomainError::InvalidStrategyBundle);
        }
        Ok(())
    }

    pub fn content_reference(&self) -> Result<CexResearchContentRefV1, crate::DomainError> {
        self.validate()?;
        Ok(CexResearchContentRefV1 {
            id: self.precommit_id.clone(),
            content_sha256: canonical_json_hash(self)?,
        })
    }
}

pub const CEX_SUPERVISED_CANDIDATE_SCHEMA_V2: &str = "cex-supervised-model-candidate-v2";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexSupervisedModelCandidateV2 {
    pub schema_version: String,
    pub artifact_id: String,
    pub mission_id: String,
    pub model_artifact: CexResearchContentRefV1,
    pub model_kind: crate::CexBaselineModelKindV1,
    pub factor_bank_revision_id: String,
    pub research_dataset: CexResearchContentRefV1,
    pub walk_forward_partition: CexResearchContentRefV1,
    pub evaluation_policy: CexResearchContentRefV1,
    pub decision_policy: hft_research_manifest::model::CexSupervisedDecisionPolicyV2,
    pub predictions_sha256: String,
    pub target_positions_sha256: String,
    pub return_accounting: crate::ReturnAccountingBasis,
    pub evaluation: crate::CandidateEvaluation,
    pub deployment_authority: bool,
    pub order_submission_authority: bool,
}

impl CexSupervisedModelCandidateV2 {
    pub fn finalize(mut self) -> Result<Self, String> {
        self.artifact_id = self.expected_artifact_id()?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.model_artifact
            .validate()
            .map_err(|error| error.to_string())?;
        self.research_dataset
            .validate()
            .map_err(|error| error.to_string())?;
        self.walk_forward_partition
            .validate()
            .map_err(|error| error.to_string())?;
        self.evaluation_policy
            .validate()
            .map_err(|error| error.to_string())?;
        self.decision_policy.validate()?;
        self.evaluation
            .validate()
            .map_err(|error| error.to_string())?;
        if self.return_accounting != crate::ReturnAccountingBasis::ObservedMidPrice
            || self.schema_version != CEX_SUPERVISED_CANDIDATE_SCHEMA_V2
            || self.artifact_id != self.expected_artifact_id()?
            || self.mission_id.trim().is_empty()
            || self.factor_bank_revision_id.trim().is_empty()
            || !crate::valid_content_sha256(&self.predictions_sha256)
            || !crate::valid_content_sha256(&self.target_positions_sha256)
            || self.deployment_authority
            || self.order_submission_authority
            || self.evaluation.evaluator_version
                != crate::CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION
            || self
                .evaluation
                .protocol_binding()
                .map_err(|error| error.to_string())?
                .1
                != self.evaluation_policy.content_sha256
        {
            return Err("CEX supervised model candidate is invalid".to_string());
        }
        Ok(())
    }

    fn expected_artifact_id(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.artifact_id.clear();
        Ok(format!(
            "cex-supervised-model-candidate-{}",
            canonical_json_hash(&semantic).map_err(|error| error.to_string())?
        ))
    }
}

/// Final selection retains the source task's correction and also covers the
/// maximum shortlist explicitly authorized before its withheld rows are read.
pub fn final_evaluator_config(
    mission: &crate::ResearchMission,
    max_candidates: u32,
) -> Result<FormulaEvaluatorConfig, crate::DomainError> {
    let mut config = FormulaEvaluatorConfig::for_mission(mission)?;
    config.multiple_testing_trials = config.multiple_testing_trials.max(max_candidates as usize);
    config.validate()?;
    Ok(config)
}
