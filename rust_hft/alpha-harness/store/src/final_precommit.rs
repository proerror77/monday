//! Common sealed-access binding for typed formula and fitted-model precommits.
use super::*;
use alpha_domain::frozen_model::ModelFinalPrecommitV1;

pub(super) const MODEL_PRECOMMIT_KIND: &str = "model_final_precommit";

pub(super) trait SealedPrecommit {
    fn dataset_manifest(&self) -> &str;
    fn precommit_hash(&self) -> Result<String, StoreError>;
}
impl SealedPrecommit for CexFinalPrecommitV1 {
    fn dataset_manifest(&self) -> &str {
        self.dataset_manifest_id.as_str()
    }
    fn precommit_hash(&self) -> Result<String, StoreError> {
        canonical_json_hash(self).map_err(domain_error)
    }
}
impl SealedPrecommit for ModelFinalPrecommitV1 {
    fn dataset_manifest(&self) -> &str {
        self.dataset_manifest_id.as_str()
    }
    fn precommit_hash(&self) -> Result<String, StoreError> {
        canonical_json_hash(self).map_err(domain_error)
    }
}

pub(super) struct FinalPrecommitBinding {
    pub dataset_manifest_id: String,
    pub final_candidate: CexResearchContentRefV1,
    pub evaluation_protocol: CexResearchContentRefV1,
    pub holdout_id: String,
    pub content_hash: String,
    pub is_model: bool,
}
impl SealedPrecommit for FinalPrecommitBinding {
    fn dataset_manifest(&self) -> &str {
        self.dataset_manifest_id.as_str()
    }
    fn precommit_hash(&self) -> Result<String, StoreError> {
        Ok(self.content_hash.clone())
    }
}

impl FinalPrecommitBinding {
    pub fn from_revision(revision: &RegistryRevision) -> Result<Self, StoreError> {
        let (binding, mission_id, precommit_id) = match revision.registry_kind.as_str() {
            CEX_FINAL_PRECOMMIT_REGISTRY_KIND => {
                let typed: CexFinalPrecommitV1 = serde_json::from_value(revision.payload.clone())
                    .map_err(serialization_error)?;
                typed.validate().map_err(domain_error)?;
                (
                    Self {
                        content_hash: typed.precommit_hash()?,
                        is_model: false,
                        dataset_manifest_id: typed.dataset_manifest_id.as_str().to_string(),
                        final_candidate: typed.final_candidate,
                        evaluation_protocol: typed.evaluation_protocol,
                        holdout_id: typed.holdout_id,
                    },
                    typed.mission.id,
                    typed.precommit_id,
                )
            }
            MODEL_PRECOMMIT_KIND => {
                let typed: ModelFinalPrecommitV1 = serde_json::from_value(revision.payload.clone())
                    .map_err(serialization_error)?;
                typed.validate().map_err(domain_error)?;
                (
                    Self {
                        content_hash: typed.precommit_hash()?,
                        is_model: true,
                        dataset_manifest_id: typed.dataset_manifest_id.as_str().to_string(),
                        final_candidate: typed.final_candidate,
                        evaluation_protocol: typed.evaluation_protocol,
                        holdout_id: typed.holdout_id,
                    },
                    typed.mission_id,
                    typed.precommit_id,
                )
            }
            _ => return Err(StoreError::Domain("unknown final precommit kind".into())),
        };
        if revision.asset_id != mission_id || revision.revision_id != precommit_id {
            return Err(StoreError::Domain(
                "final precommit registry identity changed".into(),
            ));
        }
        Ok(binding)
    }
}

use alpha_domain::campaign_finalization::{
    verify_campaign_final_evaluation_grant, SignedCampaignFinalEvaluationGrantV1,
    VerifiedCampaignFinalEvaluationGrant,
};
use alpha_domain::frozen_model::{
    FrozenModelStrategyV1, ModelSelectionReportV1, INDEPENDENT_SELECTION_EVALUATOR_VERSION,
};

const FINAL_GRANT_KIND: &str = "model_final_grant";
const SELECTION_KIND: &str = "model_independent_selection";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFinalGrant {
    signed: SignedCampaignFinalEvaluationGrantV1,
    verifying_key_hex: String,
}

impl AlphaStore {
    pub fn put_model_final_authority(
        &mut self,
        grant: &VerifiedCampaignFinalEvaluationGrant,
        selection: &ModelSelectionReportV1,
        at: DateTime<Utc>,
    ) -> Result<(CexResearchContentRefV1, CexResearchContentRefV1), StoreError> {
        grant.validate_active_at(at).map_err(invalid)?;
        selection
            .validate_against(grant)
            .map_err(StoreError::Domain)?;
        let authority = RegistryRevision {
            revision_id: format!("model-final-authority:{}", grant.grant().grant_id),
            registry_kind: FINAL_GRANT_KIND.into(),
            asset_id: grant.grant().family_id.clone(),
            parent_revision_id: None,
            payload: serde_json::to_value(StoredFinalGrant {
                signed: grant.signed_grant().clone(),
                verifying_key_hex: hex::encode(grant.verifying_key().as_bytes()),
            })
            .map_err(serialization_error)?,
            created_at: at,
        };
        let report = RegistryRevision {
            revision_id: selection.artifact_id.clone(),
            registry_kind: SELECTION_KIND.into(),
            asset_id: grant.grant().family_id.clone(),
            parent_revision_id: Some(authority.revision_id.clone()),
            payload: serde_json::to_value(selection).map_err(serialization_error)?,
            created_at: at,
        };
        for revision in [&authority, &report] {
            match self.get_registry_revision(&revision.revision_id) {
                Ok(existing) if existing == *revision => {}
                Ok(_) => {
                    return Err(StoreError::Domain(
                        "final model authority or selection conflicts with stored truth".into(),
                    ))
                }
                Err(StoreError::NotFound) => self.put_registry_revision(revision)?,
                Err(error) => return Err(error),
            }
        }
        Ok((
            payload_reference(&authority)?,
            CexResearchContentRefV1 {
                id: selection.artifact_id.clone(),
                content_sha256: canonical_json_hash(selection).map_err(domain_error)?,
            },
        ))
    }

    pub fn put_model_final_precommit(
        &mut self,
        iteration: &ResearchIteration,
        candidate_id: &str,
        candidate: &CandidateArtifact,
        evaluation: &EvaluationRecord,
        precommit: &ModelFinalPrecommitV1,
    ) -> Result<RegistryRevision, StoreError> {
        precommit.validate().map_err(domain_error)?;
        validate_iteration_records(iteration, Some((candidate_id, candidate)), Some(evaluation))?;
        let CandidateArtifact::FrozenModel(strategy) = candidate else {
            return Err(StoreError::Domain(
                "model precommit requires a frozen model candidate".into(),
            ));
        };
        strategy.validate().map_err(domain_error)?;
        let mission = self.get_mission(&iteration.mission_id)?;
        let selected =
            validate_model_precommit_dependencies(&self.connection, precommit, strategy)?;
        let typed: CandidateEvaluation =
            serde_json::from_value(evaluation.payload.clone()).map_err(serialization_error)?;
        typed.validate().map_err(domain_error)?;
        if !matches!(
            mission.status,
            MissionStatus::Completed | MissionStatus::BudgetExhausted
        ) || iteration.engine != EngineKind::FinalEvaluation
            || iteration.verdict != IterationVerdict::Keep
            || mission.mission_id != precommit.mission_id
            || iteration.mission_id != precommit.mission_id
            || mission.dataset_manifest_id != precommit.dataset_manifest_id
            || candidate_id != precommit.final_candidate.id
            || canonical_json_hash(candidate).map_err(domain_error)?
                != precommit.final_candidate.content_sha256
            || typed != selected
            || !typed.passed
            || typed.evaluator_version != INDEPENDENT_SELECTION_EVALUATOR_VERSION
            || typed.formula_config().map_err(domain_error)? != strategy.frozen.evaluator_config
        {
            return Err(StoreError::Domain(
                "model precommit differs from its terminal mission and independent selection"
                    .into(),
            ));
        }
        self.require_mission_evaluation_protocol(
            &mission.mission_id,
            &strategy.evaluation_protocol,
        )?;
        let revision = RegistryRevision {
            revision_id: precommit.precommit_id.clone(),
            registry_kind: MODEL_PRECOMMIT_KIND.into(),
            asset_id: precommit.mission_id.clone(),
            parent_revision_id: Some(precommit.replay_receipt.id.clone()),
            payload: serde_json::to_value(precommit).map_err(serialization_error)?,
            created_at: iteration.created_at,
        };
        self.persist_final_precommit(iteration, candidate_id, candidate, evaluation, revision)
    }
}

fn payload_reference(revision: &RegistryRevision) -> Result<CexResearchContentRefV1, StoreError> {
    Ok(CexResearchContentRefV1 {
        id: revision.revision_id.clone(),
        content_sha256: canonical_json_hash(&revision.payload).map_err(domain_error)?,
    })
}

pub(super) fn validate_model_precommit_dependencies(
    connection: &Connection,
    precommit: &ModelFinalPrecommitV1,
    strategy: &FrozenModelStrategyV1,
) -> Result<CandidateEvaluation, StoreError> {
    strategy.validate().map_err(domain_error)?;
    precommit.validate().map_err(domain_error)?;
    let authority =
        require_registry_payload_reference(connection, &precommit.final_grant, FINAL_GRANT_KIND)?;
    let stored: StoredFinalGrant =
        serde_json::from_value(authority.payload).map_err(serialization_error)?;
    let bytes: [u8; 32] = hex::decode(stored.verifying_key_hex)
        .map_err(invalid)?
        .try_into()
        .map_err(|_| StoreError::Domain("invalid final verification key".into()))?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes).map_err(invalid)?;
    let grant = verify_campaign_final_evaluation_grant(
        &stored.signed,
        &std::collections::BTreeMap::from([(stored.signed.key_id.clone(), key)]),
        authority.created_at,
    )
    .map_err(invalid)?;
    let report_revision: RegistryRevision = read_json_row(
        connection,
        "SELECT payload_json, content_hash FROM registry_revisions WHERE revision_id = ?",
        &precommit.selection_report.id,
    )?;
    let selection: ModelSelectionReportV1 =
        serde_json::from_value(report_revision.payload).map_err(serialization_error)?;
    selection
        .validate_against(&grant)
        .map_err(StoreError::Domain)?;
    if report_revision.registry_kind != SELECTION_KIND
        || canonical_json_hash(&selection).map_err(domain_error)?
            != precommit.selection_report.content_sha256
    {
        return Err(invalid("selection registry payload identity changed"));
    }
    let source_mission: ResearchMission = read_json_row(
        connection,
        "SELECT payload_json, content_hash FROM missions WHERE mission_id = ?",
        &precommit.mission_id,
    )?;
    let expected_config = alpha_domain::frozen_model::final_evaluator_config(
        &source_mission,
        grant.grant().max_candidates,
    )
    .map_err(domain_error)?;
    if strategy.frozen.evaluator_config != expected_config {
        return Err(invalid(
            "final model evaluator differs from the task and signed shortlist trial bounds",
        ));
    }
    let frozen_reference = CexResearchContentRefV1 {
        id: strategy.frozen.artifact_id.clone(),
        content_sha256: canonical_json_hash(&strategy.frozen).map_err(domain_error)?,
    };
    let selected = selection
        .entries
        .iter()
        .find(|entry| entry.frozen_candidate.as_ref() == Some(&frozen_reference))
        .ok_or_else(|| {
            StoreError::Domain("model precommit is not in independent selection".into())
        })?;
    if strategy.precommit_id != precommit.precommit_id
        || strategy.mission_id != precommit.mission_id
        || precommit.family_id != grant.grant().family_id
        || authority.asset_id != precommit.family_id
        || report_revision.asset_id != precommit.family_id
        || report_revision.parent_revision_id.as_deref() != Some(&precommit.final_grant.id)
        || selection.selected_candidate.as_ref() != Some(&precommit.frozen_candidate)
        || frozen_reference != precommit.frozen_candidate
        || selected.source_candidate != strategy.frozen.source_candidate
        || selected.source_operation_id != precommit.source_result.id
        || selected.source_result_sha256 != precommit.source_result.content_sha256
        || strategy.frozen.evaluation_protocol_sha256
            != precommit.evaluation_protocol.content_sha256
        || precommit.evaluation_protocol.content_sha256
            != grant.grant().execution.evaluation_protocol_sha256
        || precommit.implementation_source_revision != grant.grant().execution.source_revision
    {
        return Err(StoreError::Domain(
            "model precommit grant, source, winner or protocol changed".into(),
        ));
    }
    let mission_revision: RegistryRevision = read_json_row(
        connection,
        "SELECT payload_json, content_hash FROM registry_revisions WHERE revision_id = ?",
        &precommit.mission_id,
    )?;
    let control_mission: CexResearchMissionArtifactV1 =
        serde_json::from_value(mission_revision.payload).map_err(serialization_error)?;
    control_mission.validate().map_err(domain_error)?;
    if mission_revision.registry_kind != "cex_research_mission"
        || control_mission.semantic_id().map_err(domain_error)? != precommit.mission_id
        || control_mission.spec.policies.evaluation != precommit.evaluation_protocol
        || control_mission.spec.holdout.holdout_id != precommit.holdout_id
        || control_mission.spec.holdout.state != alpha_domain::CexResearchHoldoutStateV1::Unopened
        || control_mission.spec.instrument.symbol != strategy.frozen.program.symbol
        || control_mission.spec.instrument.venue.as_str() != strategy.frozen.program.venue
        || control_mission.spec.instrument.market.as_str() != strategy.frozen.program.market
    {
        return Err(StoreError::Domain(
            "model precommit differs from its original Mission scope".into(),
        ));
    }
    let source: alpha_domain::frozen_model::CexSupervisedModelCandidateV2 =
        require_typed_registry_payload_reference(
            connection,
            &strategy.frozen.source_candidate,
            "cex_supervised_model_candidate",
        )?;
    source.validate().map_err(StoreError::Domain)?;
    if source.model_artifact != strategy.frozen.source_model
        || source.research_dataset != strategy.frozen.research_dataset
        || source.mission_id != precommit.mission_id
        || source.decision_policy != strategy.frozen.program.decision_policy
        || !source.evaluation.passed
    {
        return Err(invalid(
            "frozen model differs from original supervised candidate policy",
        ));
    }
    // Re-read the original fitted parameters, not merely the new candidate hash.
    let baseline_kind = match strategy.frozen.program.model.kind() {
        alpha_domain::CexBaselineModelKindV1::Ridge => "cex_baseline_ridge",
        alpha_domain::CexBaselineModelKindV1::ShallowCart => "cex_baseline_cart",
        alpha_domain::CexBaselineModelKindV1::BurnMlp => "cex_baseline_burn_mlp",
    };
    let baseline: CexBaselineArtifactV1 = require_typed_registry_payload_reference(
        connection,
        &strategy.frozen.source_model,
        baseline_kind,
    )?;
    baseline.validate().map_err(domain_error)?;
    if source.evaluation.formula_config().map_err(domain_error)?
        != baseline.baseline_policy.evaluator_config
    {
        return Err(invalid("supervised benchmark evaluator changed"));
    }
    let bank: CexFactorBankRevisionV2 = require_typed_registry_payload_reference(
        connection,
        &strategy.frozen.source_factor_bank,
        "cex_factor_bank",
    )?;
    bank.validate().map_err(domain_error)?;
    let fold = baseline
        .folds
        .last()
        .ok_or_else(|| StoreError::Domain("fitted model has no fold".into()))?;
    if fold.model != strategy.frozen.program.model
        || fold.fold_id != strategy.frozen.source_fold
        || baseline.factor_bank_revision_id != bank.revision_id
        || baseline.mission_id != strategy.mission_id
        || baseline.research_dataset != strategy.frozen.research_dataset
        || baseline.factor_ids.len() != strategy.frozen.program.factors.len()
        || baseline
            .factor_ids
            .iter()
            .zip(&strategy.frozen.program.factors)
            .any(|(id, factor)| {
                bank.entries
                    .iter()
                    .find(|entry| &entry.factor_id == id)
                    .is_none_or(|entry| {
                        entry.canonical_ast != factor.ast
                            || (entry.orientation == alpha_domain::CexFactorOrientationV1::Negative)
                                != factor.negative
                    })
            })
    {
        return Err(StoreError::Domain(
            "frozen model differs from the original fitted parameters or ordered features".into(),
        ));
    }
    let replay = require_registry_payload_reference(
        connection,
        &precommit.replay_receipt,
        "cex_frozen_model_event_replay_receipt",
    )?;
    let replay_candidate: CexResearchContentRefV1 = serde_json::from_value(
        replay
            .payload
            .get("strategy")
            .cloned()
            .ok_or_else(|| StoreError::Domain("model replay lacks candidate".into()))?,
    )
    .map_err(serialization_error)?;
    if replay_candidate != frozen_reference
        || replay.asset_id != precommit.mission_id
        || replay.parent_revision_id.as_deref() != Some(&frozen_reference.id)
        || replay
            .payload
            .get("decision_scope")
            .and_then(serde_json::Value::as_str)
            != Some("independent_selection_frozen_model")
        || replay
            .payload
            .pointer("/gate/passed")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || replay
            .payload
            .get("holdout_id")
            .and_then(serde_json::Value::as_str)
            != Some(&precommit.holdout_id)
        || replay
            .payload
            .get("holdout_state")
            .and_then(serde_json::Value::as_str)
            != Some("unopened")
        || replay
            .payload
            .get("deployment_authority")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || replay
            .payload
            .get("order_submission_authority")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
    {
        return Err(StoreError::Domain(
            "model precommit lacks exact passing frozen replay evidence".into(),
        ));
    }
    let evaluation = selected
        .evaluation
        .clone()
        .ok_or_else(|| invalid("selected model lacks evaluation"))?;
    if evaluation.formula_config().map_err(domain_error)? != expected_config {
        return Err(invalid(
            "independent selection evaluator changed its signed trial bound",
        ));
    }
    Ok(evaluation)
}

fn invalid(error: impl std::fmt::Display) -> StoreError {
    StoreError::Domain(error.to_string())
}

#[derive(Debug, Clone)]
pub struct ModelFinalizationEvidenceV1 {
    pub precommit: ModelFinalPrecommitV1,
    pub candidate: CandidateArtifact,
    pub claim: CexSealedHoldoutClaimV1,
    pub sealed: RegistryRevision,
}

impl AlphaStore {
    /// Read-only proof of the same typed dependencies required for promotion.
    pub fn read_model_finalization_evidence(
        &self,
        precommit_id: &str,
    ) -> Result<ModelFinalizationEvidenceV1, StoreError> {
        let revision = self.get_registry_revision(precommit_id)?;
        let precommit: ModelFinalPrecommitV1 =
            serde_json::from_value(revision.payload).map_err(serialization_error)?;
        precommit.validate().map_err(domain_error)?;
        let candidate: CandidateArtifact = read_json_row(
            &self.connection,
            "SELECT payload_json, content_hash FROM candidate_artifacts WHERE candidate_id = ?",
            &precommit.final_candidate.id,
        )?;
        let CandidateArtifact::FrozenModel(strategy) = &candidate else {
            return Err(invalid("finalization is not a frozen model"));
        };
        validate_model_precommit_dependencies(&self.connection, &precommit, strategy)?;
        let claim =
            CexSealedHoldoutClaimV1::from_model_precommit(&precommit).map_err(domain_error)?;
        let stored_claim = self.get_registry_revision(&claim.claim_id)?;
        let claim_value: CexSealedHoldoutClaimV1 =
            serde_json::from_value(stored_claim.payload).map_err(serialization_error)?;
        if revision.registry_kind != MODEL_PRECOMMIT_KIND
            || revision.asset_id != precommit.mission_id
            || revision.revision_id != precommit.precommit_id
            || canonical_json_hash(&candidate).map_err(domain_error)?
                != precommit.final_candidate.content_sha256
            || stored_claim.registry_kind != CEX_SEALED_HOLDOUT_CLAIM_REGISTRY_KIND
            || claim_value != claim
        {
            return Err(invalid(
                "finalization candidate or unique holdout claim changed",
            ));
        }
        let sealed = self.get_registry_revision(&sealed_evaluation_revision_id(
            &claim.candidate.id,
            SEALED_HOLDOUT_EVALUATOR_VERSION,
        ))?;
        let evaluation = validate_cex_sealed_revision(&sealed, &claim, &precommit)?;
        if evaluation.formula_config().map_err(domain_error)? != strategy.frozen.evaluator_config {
            return Err(invalid(
                "sealed model evaluator differs from final selection policy",
            ));
        }
        Ok(ModelFinalizationEvidenceV1 {
            precommit,
            candidate,
            claim,
            sealed,
        })
    }
}

impl AlphaStore {
    pub(super) fn governed_candidate_evaluator_config(
        &self,
        mission: &ResearchMission,
        candidate: &CandidateArtifact,
    ) -> Result<FormulaEvaluatorConfig, StoreError> {
        match candidate {
            CandidateArtifact::FrozenModel(strategy) => {
                let revision = self.get_registry_revision(&strategy.precommit_id)?;
                if revision.registry_kind != MODEL_PRECOMMIT_KIND {
                    return Err(invalid("model evaluation precommit kind changed"));
                }
                let precommit: ModelFinalPrecommitV1 =
                    serde_json::from_value(revision.payload).map_err(serialization_error)?;
                validate_model_precommit_dependencies(&self.connection, &precommit, strategy)?;
                Ok(strategy.frozen.evaluator_config.clone())
            }
            _ => FormulaEvaluatorConfig::for_mission(mission).map_err(domain_error),
        }
    }
}
