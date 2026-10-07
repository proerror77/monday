use crate::mission_objects::{cex_campaign_round_root, cex_global_holdout_claim_object};
#[cfg(feature = "scientific")]
use hft_research_artifacts::publish_immutable_file;
use hft_research_artifacts::{fetch_to_file, normalized_sha256};
use hft_research_dispatch_io::{canonical_tokyo_oss_internal_object, validate_dns_label};
pub(crate) mod final_evaluation;
pub(crate) mod market_encoder;
#[cfg(feature = "scientific")]
mod platform_output;
pub(crate) mod preparation;
pub(crate) mod prepared_inputs;
pub(crate) mod representation;
pub(crate) mod sequence;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod workflow;
use crate::cli::print_json;
use crate::cli::CampaignFinalizeArgs;
use crate::cli::CampaignFreezeArgs;
use crate::cli::CampaignIdArgs;
use crate::cli::CampaignLearnArgs;
use crate::cli::CampaignStudyProposeArgs;
use crate::cli::BUILD_SOURCE_REVISION;
use crate::mission_dispatch;
use crate::mission_render::allowed_research_feature_fields;
use crate::mission_render::render_cex_bundle;
use crate::mission_render::render_prepared_cex_bundle;
use crate::mission_render::validate_render_instrument_scope;
use crate::mission_render::CexCampaignFailureClassV1;
use crate::mission_render::CexCampaignLearningDirectiveV1;
use crate::mission_render::CexCampaignPositionPolicyV1;
use crate::mission_render::CexCampaignResearchDeltaV1;
use crate::mission_render::CexCampaignResearchEvidenceSignatureV2;
use crate::mission_render::CexCampaignResearchParentV1;
use crate::mission_render::CexCampaignResearchPlanV1;
use crate::mission_render::CexCampaignSearchPolicyRevisionV1;
use crate::mission_render::PreparedCexInputs;
use crate::mission_render::MAX_RESEARCH_PLAN_GENERATION;
use crate::mission_runner::decode_materialization;
use crate::mission_runner::recover_execution_report_from_cached_result;
use crate::mission_runner::recover_execution_report_from_published_result;
use crate::mission_runner::research_event;
use crate::mission_runner::valid_git_revision;
use crate::mission_runner::validate_cex_holdout_id;
use crate::mission_runner::validate_supervised_candidate_binding;
use crate::mission_runner::validate_supervised_replay_binding;
use crate::mission_runner::CexEventReplayReceiptV1;
use crate::mission_runner::CexSupervisedModelSelectionV1;
use crate::mission_runner::ExecutionBinding;
use crate::mission_runner::CEX_SUPERVISED_MODEL_NAMES;
use crate::mission_runner::MAX_RESULT_BUNDLE_BYTES;
use alpha_domain::{
    campaign_horizon::{
        CampaignLabelHorizonV1, CampaignNextFamilyInputWindowV1, CampaignNextFamilyParentV1,
        CampaignNextFamilyProposalV1,
    },
    canonical_json_hash, factor_ast_source_features, CandidateEvaluation, CexBaselineFailureCodeV1,
    CexBaselineGateV1, CexFactorBankRevisionV2, CexFactorRejectionCodeV1,
};
use alpha_engine::{baselines::CexSupervisedModelCandidateV2, engines::CexFactorBankMctsResultV1};
use anyhow::{bail, Context};
use hft_backtest::config::verify_canonical_replay_artifact_streaming;
#[cfg(feature = "scientific")]
use reqwest::StatusCode;
use reqwest::{blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use zip::ZipArchive;
#[cfg(feature = "scientific")]
use {crate::cli::CampaignExecuteArgs, crate::cli::ExecuteMissionArgs, crate::data_mission};

const CAMPAIGN_FREEZE_SCHEMA_V1: &str = "cex-campaign-freeze-v1";
const CAMPAIGN_INPUTS_SCHEMA_V1: &str = "monday.cex_campaign_inputs.v1";
const CAMPAIGN_REQUEST_SCHEMA_V5: &str = "cex-campaign-request-v5";
const CAMPAIGN_REQUEST_SCHEMA_V6: &str = "cex-campaign-request-v6-prepared";
const CAMPAIGN_RESULT_SCHEMA_V8: &str = "cex-campaign-result-v8";
const CAMPAIGN_RESULT_SCHEMA_V9: &str = "cex-campaign-result-v9";
const CAMPAIGN_IDENTITY_SCHEMA_V5: &str = "cex-campaign-identity-v5";
const CAMPAIGN_ROUND_IDENTITY_SCHEMA_V1: &str = "cex-campaign-round-identity-v1";
const CAMPAIGN_DATA_FINGERPRINT_SCHEMA_V1: &str = "cex-campaign-31h-data-fingerprint-v1";
const CAMPAIGN_DATA_WINDOW_HOURS: u8 = 31;
const STOP_RULE_V2: &str = "bounded_multi_round_single_finalize_v2";
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_CAMPAIGN_RESULT_BYTES: u64 = 1024 * 1024;
const MAX_RESULT_BUNDLE_FILES: usize = 256;

fn declared_total_trials_for_rounds(
    research_plan: &CexCampaignResearchPlanV1,
    round_count: usize,
) -> anyhow::Result<usize> {
    let gp_trials = research_plan.max_candidates()?;
    declared_trials_from_candidate_count(research_plan, round_count, gp_trials)
}

fn declared_total_trials_for_validated_plan(
    base: &crate::mission_render::ValidatedCexResearchPlanBase<'_>,
    round_count: usize,
) -> anyhow::Result<usize> {
    declared_trials_from_candidate_count(base.plan(), round_count, base.max_candidates()?)
}

fn declared_trials_from_candidate_count(
    research_plan: &CexCampaignResearchPlanV1,
    round_count: usize,
    gp_trials: usize,
) -> anyhow::Result<usize> {
    let per_round = if !research_plan.supervised_model_scope.is_default() {
        gp_trials
            .checked_add(research_plan.supervised_model_scope.names().len())
            .context("Campaign scoped model trial overflow")?
    } else if research_plan
        .search_policy_revision
        .research_delta
        .is_some()
    {
        gp_trials
            .checked_add(CEX_SUPERVISED_MODEL_NAMES.len())
            .context("campaign supervised model trial bound overflowed")?
    } else {
        gp_trials
            .checked_mul(2)
            .context("campaign legacy trial bound overflowed")?
    };
    per_round
        .checked_mul(round_count)
        .context("campaign declared_total_trials overflowed")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignRequest {
    pub(crate) schema_version: String,
    pub(crate) campaign_id: String,
    pub(crate) build_source_revision: String,
    pub(crate) image_identity: String,
    pub(crate) campaign_inputs_sha256: String,
    pub(crate) producer_source_revision: String,
    pub(crate) producer_image_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prepared_inputs: Option<prepared_inputs::NativePreparedCampaignRefV1>,
    pub(crate) research_plan: CexCampaignResearchPlanV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) study_proposal: Option<CampaignNextFamilyProposalV1>,
    pub(crate) feature_url: String,
    pub(crate) feature_sha256: String,
    pub(crate) materialization_url: String,
    pub(crate) materialization_sha256: String,
    pub(crate) replay_artifact_url: String,
    pub(crate) replay_artifact_sha256: String,
    pub(crate) replay_manifest_url: String,
    pub(crate) replay_manifest_sha256: String,
    pub(crate) holdout_id: String,
    pub(crate) declared_total_trials: usize,
    pub(crate) rounds: Vec<CampaignRoundRequest>,
    pub(crate) holdout_claim_put_url: String,
    pub(crate) holdout_claim_readback_url: String,
    pub(crate) campaign_result_put_url: String,
    pub(crate) campaign_result_readback_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignRoundRequest {
    pub(crate) round_id: String,
    pub(crate) seed: u64,
    pub(crate) identity: CampaignRoundIdentityV1,
    pub(crate) mission_put_url: String,
    pub(crate) mission_readback_url: String,
    pub(crate) result_put_url: String,
    pub(crate) result_readback_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignRoundIdentityV1 {
    pub(crate) schema_version: String,
    pub(crate) data_window_hours: u8,
    pub(crate) data_fingerprint_sha256: String,
    pub(crate) image_identity: String,
    pub(crate) build_source_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenCampaignPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preparation_authentication_tag: Option<String>,
    schema_version: String,
    campaign_inputs_sha256: String,
    canonical_request: CampaignRequest,
    signing_plan: CampaignSigningPlan,
}

/// Complete request identity verified against the original producer's ledger
/// attestation. Caller hashes and JSON cannot construct this process-local value.
pub(crate) struct VerifiedPlanningRequest<'a> {
    request: &'a CampaignRequest,
    request_sha256: String,
}

impl VerifiedPlanningRequest<'_> {
    pub(crate) fn request(&self) -> &CampaignRequest {
        self.request
    }
    pub(crate) fn request_sha256(&self) -> &str {
        &self.request_sha256
    }
}

pub(crate) fn verify_planning_request<'a>(
    ledger: &alpha_store::AlphaStore,
    freeze_path: Option<&Path>,
    request: &'a CampaignRequest,
) -> anyhow::Result<VerifiedPlanningRequest<'a>> {
    let frozen = load_freeze_plan(
        freeze_path.context("planning requires an authenticated original freeze")?,
    )?;
    preparation::verify_authentication(
        ledger,
        &frozen,
        frozen.preparation_authentication_tag.as_deref(),
    )?;
    validate_request_matches_freeze(request, &frozen)?;
    Ok(VerifiedPlanningRequest {
        request,
        request_sha256: hft_cex_research_input::sha256(&serialize_request(request)?),
    })
}

fn authenticated_freeze_plan(
    request: &CampaignRequest,
    ledger: Option<&alpha_store::AlphaStore>,
) -> anyhow::Result<FrozenCampaignPlan> {
    let mut plan = FrozenCampaignPlan {
        preparation_authentication_tag: None,
        schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.to_string(),
        campaign_inputs_sha256: request.campaign_inputs_sha256.clone(),
        signing_plan: signing_plan(request)?,
        canonical_request: request.clone(),
    };
    if let Some(ledger) = ledger {
        plan.preparation_authentication_tag = Some(preparation::authenticate(ledger, &plan)?);
    }
    Ok(plan)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignInputsReceipt {
    schema_version: String,
    run_id: String,
    source_revision: String,
    image_ref: String,
    mission_id: String,
    market: String,
    symbol: String,
    output_prefix: String,
    output_object_base_url: String,
    readback_scope: String,
    feature: CampaignInputReceiptItem,
    materialization: CampaignInputReceiptItem,
    replay_artifact: CampaignInputReceiptItem,
    replay_manifest: CampaignInputReceiptItem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepared_inputs: Option<prepared_inputs::NativePreparedCampaignRefV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CampaignInputReceiptItem {
    relative_path: PathBuf,
    object_url: String,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CampaignSigningPlan {
    actions: Vec<CampaignSigningAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CampaignSigningAction {
    name: String,
    object: String,
    method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    required_headers: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct CampaignFreezeReport {
    campaign_id: String,
    holdout_id: String,
    declared_total_trials: usize,
    output: String,
}

#[derive(Debug, Serialize)]
struct CampaignFinalizeReport {
    campaign_id: String,
    holdout_id: String,
    request_sha256: String,
    submission_identity_sha256: String,
    job_name: String,
    request_out: String,
    submission_out: String,
}

#[derive(Debug, Serialize)]
struct CampaignIdReport {
    campaign_id: String,
    matches_request: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CampaignEvaluationFeedbackV1 {
    passed: bool,
    score: f64,
    time_series_ic: Option<f64>,
    time_series_rank_ic: Option<f64>,
    cumulative_net_return: f64,
    max_drawdown: f64,
    net_sharpe: f64,
    trade_count: usize,
    #[serde(default)]
    total_turnover: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_book_depth_fraction: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_book_depth_fraction_limit: Option<f64>,
    #[serde(default)]
    capacity_breached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CampaignSelectedEvaluationProofV1 {
    evaluation: CandidateEvaluation,
    evaluation_content_sha256: String,
}

impl CampaignSelectedEvaluationProofV1 {
    fn from_evaluation(evaluation: &CandidateEvaluation) -> anyhow::Result<Self> {
        evaluation.validate()?;
        Ok(Self {
            evaluation: evaluation.clone(),
            evaluation_content_sha256: canonical_json_hash(evaluation)?,
        })
    }

    fn screening_facts(
        &self,
        summary: &CampaignEvaluationFeedbackV1,
    ) -> anyhow::Result<alpha_domain::PredictiveScreeningGateFacts> {
        if canonical_json_hash(&self.evaluation)? != self.evaluation_content_sha256
            || campaign_evaluation_feedback(&self.evaluation) != *summary
        {
            bail!("selected evaluation proof differs from its complete content hash or summary");
        }
        self.evaluation
            .predictive_screening_gate_facts()
            .map_err(anyhow::Error::from)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CampaignFactorFeedbackV1 {
    factor_signature_sha256: String,
    source_features: Vec<String>,
    rejection_codes: Vec<CexFactorRejectionCodeV1>,
    evaluation: Option<CampaignEvaluationFeedbackV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CampaignReplayFeedbackV1 {
    passed: bool,
    failures: Vec<String>,
    position_changes: usize,
    total_turnover: f64,
    mean_net_return: f64,
    cumulative_net_return: f64,
    max_drawdown: f64,
    net_sharpe: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CampaignRoundFeedbackV1 {
    factor_attempts: usize,
    #[serde(default)]
    model_attempts: Option<usize>,
    accepted_factors: usize,
    factors: Vec<CampaignFactorFeedbackV1>,
    baseline_gate_passed: bool,
    baseline_failure_codes: Vec<CexBaselineFailureCodeV1>,
    ridge: Option<CampaignEvaluationFeedbackV1>,
    cart: Option<CampaignEvaluationFeedbackV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    burn: Option<CampaignEvaluationFeedbackV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_ridge: Option<CampaignEvaluationFeedbackV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_cart: Option<CampaignEvaluationFeedbackV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_burn: Option<CampaignEvaluationFeedbackV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_selected: Option<CampaignEvaluationFeedbackV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_selected_evaluation_proof: Option<CampaignSelectedEvaluationProofV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_selected_candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_replay: Option<CampaignReplayFeedbackV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    calendar_validation: Option<CampaignEvaluationFeedbackV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct CampaignMissionLedgerV1 {
    round_id: String,
    seed: u64,
    identity: CampaignRoundIdentityV1,
    mission_id: String,
    mission_sha256: String,
    request_sha256: Option<String>,
    result_bundle_sha256: String,
    result_readback_bundle_sha256: String,
    replay_receipt_id: Option<String>,
    replay_gate_passed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_replay_receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supervised_replay_gate_passed: Option<bool>,
    final_precommit_id: Option<String>,
    sealed_receipt_id: Option<String>,
    sealed_passed: Option<bool>,
    strategy_bundle_id: Option<String>,
    promotion_id: Option<String>,
    selected_candidate_id: Option<String>,
    selected_candidate_content_hash: Option<String>,
    selected_score: Option<f64>,
    consumed_trials: usize,
    termination_reason: String,
    feedback: CampaignRoundFeedbackV1,
}

fn round_log_summary(round: &CampaignMissionLedgerV1) -> serde_json::Value {
    serde_json::json!({
        "round_id": &round.round_id,
        "seed": round.seed,
        "identity": &round.identity,
        "mission_id": &round.mission_id,
        "mission_sha256": &round.mission_sha256,
        "result_bundle_sha256": &round.result_bundle_sha256,
        "consumed_trials": round.consumed_trials,
        "termination_reason": &round.termination_reason,
        "factor_attempts": round.feedback.factor_attempts,
        "model_attempts": round.feedback.model_attempts,
        "accepted_factors": round.feedback.accepted_factors,
        "baseline_gate_passed": round.feedback.baseline_gate_passed,
        "baseline_failure_codes": &round.feedback.baseline_failure_codes,
        "ridge": &round.feedback.ridge,
        "cart": &round.feedback.cart,
        "burn": &round.feedback.burn,
        "supervised_ridge": &round.feedback.supervised_ridge,
        "supervised_cart": &round.feedback.supervised_cart,
        "supervised_burn": &round.feedback.supervised_burn,
        "supervised_selected": &round.feedback.supervised_selected,
        "supervised_replay": &round.feedback.supervised_replay,
        "selected_candidate_id": &round.selected_candidate_id,
        "selected_score": round.selected_score,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct CampaignFinalizationV1 {
    round_id: String,
    precommit_id: String,
    sealed_receipt_id: String,
    sealed_passed: bool,
    strategy_bundle_id: Option<String>,
    promotion_id: Option<String>,
    final_precommit: serde_json::Value,
    sealed_holdout_claim: serde_json::Value,
    sealed_holdout_receipt: serde_json::Value,
    strategy_bundle: Option<serde_json::Value>,
    promotion_record: Option<serde_json::Value>,
    final_precommit_sha256: String,
    sealed_holdout_claim_sha256: String,
    sealed_holdout_receipt_sha256: String,
    strategy_bundle_sha256: Option<String>,
    promotion_record_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct CampaignResultV1 {
    schema_version: String,
    campaign_id: String,
    request_sha256: String,
    build_source_revision: String,
    image_identity: String,
    campaign_inputs_sha256: String,
    producer_source_revision: String,
    producer_image_identity: String,
    research_plan_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    learning_directive: Option<CexCampaignLearningDirectiveV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    learning_directive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    search_policy_revision: Option<CexCampaignSearchPolicyRevisionV1>,
    holdout_id: String,
    declared_total_trials: usize,
    consumed_trials: usize,
    stop_rule: String,
    termination_reason: String,
    rounds: Vec<CampaignMissionLedgerV1>,
    selected_round_id: Option<String>,
    selected_candidate_id: Option<String>,
    selected_candidate_content_hash: Option<String>,
    finalization: Option<CampaignFinalizationV1>,
}

#[derive(Debug, Clone, Serialize)]
struct CampaignLearnReport {
    parent_campaign_id: String,
    parent_request_sha256: String,
    parent_campaign_result_sha256: String,
    failure_class: CexCampaignFailureClassV1,
    outcome: CampaignLearnOutcomeV1,
    evidence_signature: CexCampaignResearchEvidenceSignatureV2,
    #[serde(skip_serializing_if = "Option::is_none")]
    learning_directive_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_policy_revision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    research_plan_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    reused_existing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct CampaignStudyProposalReport {
    schema_version: String,
    status: String,
    reason: Option<String>,
    study_id: String,
    parent: CampaignNextFamilyParentV1,
    target_family_id: Option<String>,
    proposal_sha256: Option<String>,
    research_plan_sha256: Option<String>,
    proposal_path: String,
    research_plan_path: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CampaignLearnOutcomeV1 {
    FollowUp,
    NoImprovement,
    FixedComparisonComplete,
}

#[derive(Debug)]
struct LoadedRequest {
    request: CampaignRequest,
    sha256: String,
}

#[cfg(feature = "scientific")]
pub fn execute(args: CampaignExecuteArgs) -> anyhow::Result<()> {
    if std::env::var_os("MONDAY_ATTEMPT_CONTEXT").is_some() {
        let plain = load_request(&args.request)
            .context("platform CexCampaign requires the plain finalized native V6 request")?;
        if args.final_evaluation
            || !args.pre_holdout
            || plain.request.schema_version != CAMPAIGN_REQUEST_SCHEMA_V6
            || plain.request.prepared_inputs.is_none()
        {
            bail!(
                "platform CexCampaign cannot execute another native schema or withheld evaluation"
            );
        }
    }
    if args.final_evaluation {
        return final_evaluation::execute(args);
    }
    if !args.pre_holdout {
        bail!(
            "campaign-execute cannot open sealed holdout; pass --pre-holdout, or --final-evaluation with an independent grant"
        );
    }
    if market_encoder::is_market_encoder_request(&args.request)? {
        return market_encoder::execute(args);
    }
    if sequence::is_sequence_request(&args.request)? {
        return sequence::execute(args);
    }
    let loaded = load_request(&args.request)?;
    if loaded.sha256 != normalized_sha256("campaign request", &args.request_sha256)? {
        bail!("campaign request SHA256 mismatch");
    }
    if loaded.request.schema_version != CAMPAIGN_REQUEST_SCHEMA_V6
        || loaded.request.prepared_inputs.is_none()
    {
        bail!("plain Campaign execution requires the frozen V6 prepared collection");
    }
    validate_request_for_execute(&loaded.request)?;
    if loaded.request.research_plan.calendar.is_some() && !args.pre_holdout {
        bail!("fixed calendar H1 must stop at pre-holdout");
    }
    if loaded.request.campaign_id != args.campaign_id {
        bail!("campaign request does not match the requested Campaign ID");
    }
    if loaded.request.image_identity
        != normalized_sha256("campaign image identity", &args.image_identity)?
    {
        bail!("campaign request image identity does not match the requested image identity");
    }
    if loaded.request.build_source_revision != BUILD_SOURCE_REVISION {
        bail!("campaign request source revision does not match this build");
    }
    if !valid_git_revision(BUILD_SOURCE_REVISION) {
        bail!("alpha-harness was built without an exact source revision");
    }
    research_event(
        "alpha-harness",
        "campaign_execution_started",
        serde_json::json!({
            "campaign_id": &loaded.request.campaign_id,
            "request_sha256": &loaded.sha256,
            "build_source_revision": &loaded.request.build_source_revision,
            "image_identity": &loaded.request.image_identity,
            "campaign_inputs_sha256": &loaded.request.campaign_inputs_sha256,
            "research_plan": &loaded.request.research_plan,
            "round_count": loaded.request.rounds.len(),
            "declared_total_trials": loaded.request.declared_total_trials,
            "holdout_id": &loaded.request.holdout_id,
        }),
    );
    execute_loaded_request(args, loaded)
}

pub fn freeze(args: CampaignFreezeArgs) -> anyhow::Result<()> {
    if market_encoder::is_market_encoder_plan(args.research_plan.as_deref())? {
        return market_encoder::freeze(args);
    }
    if args.stage_authority.is_some() {
        bail!("stage authority requires the typed market encoder plan");
    }
    if args.reuse.is_none()
        && args.final_evaluation_control.is_none()
        && sequence::is_sequence_plan(args.research_plan.as_deref())?
    {
        return sequence::freeze(args);
    }
    if args.reuse.is_some() != args.reuse_sha256.is_some()
        || (args.final_evaluation_control.is_some() && args.reuse.is_some())
    {
        bail!("prepared Campaign reuse requires its digest and cannot replace final evaluation admission");
    }
    if args.final_evaluation_control.is_some() {
        return final_evaluation::freeze(args);
    }
    let (request, _) = freeze_request(&args)?;
    let ledger = args
        .preparation_ledger
        .as_ref()
        .map(alpha_store::AlphaStore::open_read_only)
        .transpose()?;
    let plan = authenticated_freeze_plan(&request, ledger.as_ref())?;
    hft_research_artifacts::write_json_atomic(&args.output, &plan)?;
    research_event(
        "alpha-harness",
        "campaign_freeze_completed",
        serde_json::json!({
            "prepared_input_reused": args.reuse.is_some(),
            "campaign_id": &request.campaign_id,
            "campaign_inputs_sha256": &plan.campaign_inputs_sha256,
            "research_plan": &request.research_plan,
            "round_count": request.rounds.len(),
            "declared_total_trials": request.declared_total_trials,
            "holdout_id": &request.holdout_id,
            "build_source_revision": &request.build_source_revision,
            "image_identity": &request.image_identity,
        }),
    );
    print_json(&CampaignFreezeReport {
        campaign_id: request.campaign_id.clone(),
        holdout_id: request.holdout_id.clone(),
        declared_total_trials: request.declared_total_trials,
        output: args.output.display().to_string(),
    })
}

pub fn learn(args: CampaignLearnArgs) -> anyhow::Result<()> {
    let source = mission_dispatch::read_authenticated_settled_campaign_source(
        &args.control,
        &args.submission,
        &args.result,
        &args.settlement,
        &args.namespace,
    )?;
    if !source.is_negative() {
        bail!("cost learning requires an authenticated negative settled Campaign");
    }
    let loaded = load_request(&args.request)?;
    validate_request(&loaded.request)?;
    let result: CampaignResultV1 = serde_json::from_slice(source.result_bytes())?;
    let result_sha256 = source.result_sha256().to_string();
    if result_sha256 != normalized_sha256("parent Campaign result", &args.result_sha256)? {
        bail!("parent Campaign result SHA256 mismatch");
    }
    if source.request() != &loaded.request
        || source.request_sha256() != loaded.sha256
        || source.result_sha256() != result_sha256
    {
        bail!("learning request/result differs from the authenticated settled source");
    }
    validate_negative_campaign_result(&loaded, &result, &result_sha256)?;
    let failure_class = classify_campaign_failure(&result)?;
    let evidence_signature = campaign_research_evidence_signature(&loaded.request, &result)?;
    if loaded.request.research_plan.holding.is_some()
        || campaign_has_no_improvement(&loaded.request, &evidence_signature)
    {
        let report = CampaignLearnReport {
            parent_campaign_id: loaded.request.campaign_id.clone(),
            parent_request_sha256: loaded.sha256.clone(),
            parent_campaign_result_sha256: result_sha256,
            failure_class,
            outcome: if loaded.request.research_plan.holding.is_some() {
                CampaignLearnOutcomeV1::FixedComparisonComplete
            } else {
                CampaignLearnOutcomeV1::NoImprovement
            },
            evidence_signature,
            learning_directive_sha256: None,
            search_policy_revision_id: None,
            research_plan_sha256: None,
            output: None,
            reused_existing: false,
        };
        research_event(
            "alpha-harness",
            "campaign_learning_stopped",
            serde_json::json!({
                "parent_campaign_id": &report.parent_campaign_id,
                "failure_class": report.failure_class,
                "outcome": report.outcome,
                "evidence_signature": &report.evidence_signature,
            }),
        );
        return print_json(&report);
    }
    let (search_policy_revision, learning_directive) =
        next_campaign_policy_revision(&loaded, &result_sha256, failure_class)?;
    research_event(
        "alpha-harness",
        "campaign_learning_started",
        serde_json::json!({
            "parent_campaign_id": &loaded.request.campaign_id,
            "parent_request_sha256": &loaded.sha256,
            "parent_campaign_result_sha256": &result_sha256,
            "parent_generation": loaded.request.research_plan.generation,
            "termination_reason": &result.termination_reason,
            "failure_class": failure_class,
            "learning_directive_sha256": learning_directive.content_hash()?,
            "search_policy_revision_id": &search_policy_revision.revision_id,
            "round_feedback": result.rounds.iter().map(round_log_summary).collect::<Vec<_>>(),
        }),
    );

    if args.output.try_exists()? {
        let plan = load_research_plan(&args.output)?;
        validate_existing_follow_up_plan(
            &plan,
            &loaded,
            &result_sha256,
            &learning_directive,
            &search_policy_revision,
            &evidence_signature,
        )?;
        let report = campaign_learn_report(
            &args.output,
            &loaded,
            &result_sha256,
            &evidence_signature,
            &plan,
            true,
        )?;
        research_event(
            "alpha-harness",
            "campaign_learning_completed",
            serde_json::json!({
                "parent_campaign_id": &report.parent_campaign_id,
                "research_plan_sha256": &report.research_plan_sha256,
                "child_research_plan": &plan,
                "reused_existing": true,
            }),
        );
        return print_json(&report);
    }

    if loaded.request.research_plan.generation >= MAX_RESEARCH_PLAN_GENERATION {
        bail!("Campaign learning exhausted the bounded follow-up generations");
    }
    let plan = follow_up_plan(
        &loaded,
        &result_sha256,
        learning_directive,
        search_policy_revision,
        evidence_signature.clone(),
    )?;
    write_research_plan_create_once(&args.output, &plan)?;
    let report = campaign_learn_report(
        &args.output,
        &loaded,
        &result_sha256,
        &evidence_signature,
        &plan,
        false,
    )?;
    research_event(
        "alpha-harness",
        "campaign_learning_completed",
        serde_json::json!({
            "parent_campaign_id": &report.parent_campaign_id,
            "research_plan_sha256": &report.research_plan_sha256,
            "child_research_plan": &plan,
            "reused_existing": false,
        }),
    );
    print_json(&report)
}

pub fn propose_next_family(args: CampaignStudyProposeArgs) -> anyhow::Result<()> {
    let authenticated = mission_dispatch::read_authenticated_campaign_parent(
        &args.parent_control,
        &args.parent_submission,
        &args.parent_result,
        &args.parent_settlement,
        &args.study_id,
        &args.parent_namespace,
    )?;
    let parent = authenticated.parent;
    let loaded = LoadedRequest {
        request: authenticated.request,
        sha256: parent.request_sha256.clone(),
    };
    let result: CampaignResultV1 = serde_json::from_slice(&authenticated.result_bytes)?;
    validate_negative_campaign_result(&loaded, &result, &parent.campaign_result_sha256)?;

    let needs_authority = |reason: &str, target_family_id: Option<String>| {
        let report = CampaignStudyProposalReport {
            schema_version: "monday.campaign_study_proposal_report.v1".into(),
            status: "needs_authority".into(),
            reason: Some(reason.into()),
            study_id: args.study_id.clone(),
            parent: parent.clone(),
            target_family_id,
            proposal_sha256: None,
            research_plan_sha256: None,
            proposal_path: args.output.display().to_string(),
            research_plan_path: args.research_plan_output.display().to_string(),
        };
        write_json_create_once(&args.output.with_extension("report.json"), &report)?;
        print_json(&report)
    };

    let Some(target_family_id) = args
        .target_family_id
        .as_deref()
        .filter(|family| !family.trim().is_empty())
    else {
        return needs_authority("target_family_member_missing", None);
    };
    if target_family_id == parent.family_id {
        return needs_authority(
            "target_family_must_differ_from_parent",
            Some(target_family_id.into()),
        );
    }
    let Some(member) = authenticated
        .study_grant
        .grant
        .members
        .iter()
        .find(|member| member.family_id == target_family_id)
    else {
        return needs_authority(
            "target_family_is_not_a_predeclared_study_member",
            Some(target_family_id.into()),
        );
    };
    let Some(horizon_path) = args.target_horizon.as_deref() else {
        return needs_authority(
            "target_label_horizon_missing",
            Some(target_family_id.into()),
        );
    };
    let horizon = load_label_horizon(horizon_path)?;
    let horizon_sha256 = horizon.content_hash().map_err(anyhow::Error::msg)?;
    if horizon_sha256 != member.label_horizon_sha256 {
        return needs_authority(
            "target_label_horizon_is_not_predeclared",
            Some(target_family_id.into()),
        );
    }
    let Some(start_received_at_ns) = args.target_start_received_at_ns else {
        return needs_authority(
            "target_explicit_window_missing",
            Some(target_family_id.into()),
        );
    };
    let Some(end_received_at_ns) = args.target_end_received_at_ns else {
        return needs_authority(
            "target_explicit_window_missing",
            Some(target_family_id.into()),
        );
    };
    let Some(target_mission_id) = args.target_mission_id.as_deref() else {
        return needs_authority(
            "target_materialization_identity_missing",
            Some(target_family_id.into()),
        );
    };
    let Some(target_output_prefix) = args.target_output_prefix.as_deref() else {
        return needs_authority(
            "target_materialization_identity_missing",
            Some(target_family_id.into()),
        );
    };
    let Some(target_bucket_ms) = args.target_bucket_ms else {
        return needs_authority(
            "target_materialization_identity_missing",
            Some(target_family_id.into()),
        );
    };
    let target_window = CampaignNextFamilyInputWindowV1 {
        mission_id: target_mission_id.into(),
        output_prefix: target_output_prefix.into(),
        start_received_at_ns,
        end_received_at_ns,
        bucket_ms: target_bucket_ms,
        top_depth: args.target_top_depth,
    };
    if target_window
        .validate()
        .map_err(anyhow::Error::msg)
        .is_err()
        || target_window.bucket_ms != horizon.labels.observation_frequency_millis
        || target_window.bucket_ms != 1_000
        || target_window.top_depth != 5
    {
        return needs_authority(
            "target_materialization_window_is_invalid",
            Some(target_family_id.into()),
        );
    }
    if member.execution.source_revision != BUILD_SOURCE_REVISION
        || member.execution.campaign_inputs_sha256.is_empty()
    {
        return needs_authority(
            "target_study_member_execution_is_not_current",
            Some(target_family_id.into()),
        );
    }
    let failure_class = classify_campaign_failure(&result)?;
    let evidence_signature = campaign_research_evidence_signature(&loaded.request, &result)?;
    let (search_policy_revision, learning_directive) =
        next_campaign_policy_revision(&loaded, &parent.campaign_result_sha256, failure_class)?;
    let mut plan = follow_up_plan(
        &loaded,
        &parent.campaign_result_sha256,
        learning_directive,
        search_policy_revision,
        evidence_signature,
    )?;
    plan.label_horizon = Some(horizon.clone());
    plan.validate()?;
    if let Err(error) = validate_next_family_materialization(&args, member, &target_window, &plan) {
        return needs_authority(
            &format!("target_materialization_invalid:{error}"),
            Some(target_family_id.into()),
        );
    }
    let target_research_plan_sha256 = plan.content_hash()?;
    let target_member_sha256 = member.content_hash().map_err(anyhow::Error::msg)?;
    let proposal = CampaignNextFamilyProposalV1 {
        schema_version: alpha_domain::campaign_horizon::CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1
            .into(),
        study_id: args.study_id.clone(),
        study_grant_sha256: authenticated.study_grant.content_sha256.clone(),
        parent,
        target_family_id: target_family_id.into(),
        target_root_grant_sha256: member.root_grant_sha256.clone(),
        target_member_sha256,
        target_execution: member.execution.clone(),
        target_horizon: horizon,
        target_horizon_sha256: horizon_sha256,
        target_window,
        target_research_plan_sha256: target_research_plan_sha256.clone(),
    };
    proposal.validate().map_err(anyhow::Error::msg)?;
    let proposal_sha256 = proposal.content_hash().map_err(anyhow::Error::msg)?;
    write_research_plan_create_once(&args.research_plan_output, &plan)?;
    let report = CampaignStudyProposalReport {
        schema_version: "monday.campaign_study_proposal_report.v1".into(),
        status: "ready".into(),
        reason: None,
        study_id: args.study_id,
        parent: proposal.parent.clone(),
        target_family_id: Some(proposal.target_family_id.clone()),
        proposal_sha256: Some(proposal_sha256.clone()),
        research_plan_sha256: Some(target_research_plan_sha256),
        proposal_path: args.output.display().to_string(),
        research_plan_path: args.research_plan_output.display().to_string(),
    };
    write_json_create_once(&args.output, &proposal)?;
    write_json_create_once(&args.output.with_extension("report.json"), &report)?;
    print_json(&report)
}

fn load_label_horizon(path: &Path) -> anyhow::Result<CampaignLabelHorizonV1> {
    let mut file = File::open(path)
        .with_context(|| format!("open typed Campaign label horizon {}", path.display()))?;
    if file.metadata()?.len() > MAX_REQUEST_BYTES {
        bail!("typed Campaign label horizon exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let horizon: CampaignLabelHorizonV1 = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse typed Campaign label horizon {}", path.display()))?;
    horizon.validate().map_err(anyhow::Error::msg)?;
    Ok(horizon)
}

fn validate_next_family_materialization(
    args: &CampaignStudyProposeArgs,
    member: &alpha_domain::campaign_study::CampaignStudyMemberV1,
    target_window: &CampaignNextFamilyInputWindowV1,
    plan: &CexCampaignResearchPlanV1,
) -> anyhow::Result<()> {
    let campaign_inputs_path = args
        .target_campaign_inputs
        .as_deref()
        .context("target campaign-inputs receipt is required")?;
    let input_root = args
        .target_input_root
        .as_deref()
        .context("target input root is required")?;
    let (receipt, receipt_sha256) = load_campaign_inputs_receipt(campaign_inputs_path)?;
    validate_campaign_inputs_receipt(&receipt)?;
    if receipt_sha256 != member.execution.campaign_inputs_sha256 {
        bail!("target campaign-inputs receipt hash is not predeclared by the Study member");
    }
    if receipt.mission_id != target_window.mission_id
        || receipt.output_prefix.trim_matches('/') != target_window.output_prefix
    {
        bail!("target input receipt identity differs from the selected Study window");
    }
    let feature_path = input_root.join(&receipt.feature.relative_path);
    let materialization_path = input_root.join(&receipt.materialization.relative_path);
    verify_local_receipt_item("target feature", &feature_path, &receipt.feature.sha256)?;
    verify_local_receipt_item(
        "target materialization",
        &materialization_path,
        &receipt.materialization.sha256,
    )?;
    let materialization_bytes = std::fs::read(&materialization_path)?;
    let materialization = decode_materialization(&materialization_bytes)?;
    if materialization.mission_id != target_window.mission_id
        || materialization.bucket_ms != target_window.bucket_ms
        || materialization.top_depth != target_window.top_depth
        || materialization.label_horizon_buckets
            != plan
                .label_horizon
                .as_ref()
                .context("target plan is missing its typed label horizon")?
                .labels
                .horizon_buckets
    {
        bail!("target materialization does not match the selected Study horizon");
    }
    let metadata: serde_json::Value = serde_json::from_slice(&materialization_bytes)?;
    let segments = metadata["source_segments"]
        .as_array()
        .context("target materialization source segments are missing")?;
    let start = segments
        .iter()
        .filter_map(|segment| segment["start_received_at_ns"].as_u64())
        .min()
        .context("target materialization has no source start")?;
    let end = segments
        .iter()
        .filter_map(|segment| segment["end_received_at_ns"].as_u64())
        .max()
        .context("target materialization has no source end")?;
    if start < target_window.start_received_at_ns || end > target_window.end_received_at_ns {
        bail!("target materialization source window is outside the selected Study window");
    }
    let declared_trials = declared_total_trials_for_rounds(plan, 2)?;
    let rendered = render_cex_bundle(
        &feature_path,
        &materialization_path,
        plan,
        args.target_seed,
        declared_trials,
    )?;
    if rendered.mission.spec.evaluation_protocol.content_hash()?
        != member.execution.evaluation_protocol_sha256
    {
        bail!("target materialization evaluation protocol is not predeclared by the Study member");
    }
    Ok(())
}

fn campaign_has_no_improvement(
    request: &CampaignRequest,
    evidence_signature: &CexCampaignResearchEvidenceSignatureV2,
) -> bool {
    request
        .research_plan
        .parent_evidence_signature
        .as_ref()
        .is_some_and(|parent| {
            parent.campaign_inputs_sha256 == evidence_signature.campaign_inputs_sha256
                && parent.feature_fields_sha256 == evidence_signature.feature_fields_sha256
                && parent.factor_signatures_sha256 == evidence_signature.factor_signatures_sha256
                && parent.evaluation_feedback_sha256
                    == evidence_signature.evaluation_feedback_sha256
        })
}

fn classify_campaign_failure(
    result: &CampaignResultV1,
) -> anyhow::Result<CexCampaignFailureClassV1> {
    if result.schema_version != CAMPAIGN_RESULT_SCHEMA_V9 {
        bail!("cost learning requires a current result with complete selected evaluation evidence");
    }
    let mut classified = None;
    for round in &result.rounds {
        let selected = selected_supervised_feedback(&round.feedback)
            .context("Campaign failure has no supervised model evidence")?;
        let proof = round
            .feedback
            .supervised_selected_evaluation_proof
            .as_ref()
            .context("cost learning requires complete selected evaluation evidence")?;
        let facts = proof.screening_facts(selected)?;
        if !facts.predictive_passed || !facts.coverage_passed {
            bail!("selected prediction or coverage screening failed; cost attribution is not admitted");
        }
        let failure_class = if selected.trade_count == 0 {
            CexCampaignFailureClassV1::NoTradesAfterCosts
        } else if selected.capacity_breached {
            CexCampaignFailureClassV1::OvertradeCapacity
        } else if selected.time_series_ic.is_some_and(|value| value > 0.0)
            && selected.cumulative_net_return < 0.0
            && selected.net_sharpe < 0.0
        {
            CexCampaignFailureClassV1::PositiveIcNegativeNet
        } else {
            bail!("Campaign failure is outside the admitted learning classes");
        };
        if classified.is_some_and(|existing| existing != failure_class) {
            bail!("Campaign rounds do not agree on one admitted failure class");
        }
        classified = Some(failure_class);
    }
    classified.context("Campaign result contains no rounds to classify")
}

fn selected_supervised_feedback(
    feedback: &CampaignRoundFeedbackV1,
) -> Option<&CampaignEvaluationFeedbackV1> {
    feedback.supervised_selected.as_ref()
}

fn campaign_research_evidence_signature(
    request: &CampaignRequest,
    result: &CampaignResultV1,
) -> anyhow::Result<CexCampaignResearchEvidenceSignatureV2> {
    let feature_fields_sha256 = canonical_json_hash(&request.research_plan.feature_fields)?;
    let mut factor_signatures = result
        .rounds
        .iter()
        .flat_map(|round| round.feedback.factors.iter())
        .map(|factor| factor.factor_signature_sha256.as_str())
        .collect::<Vec<_>>();
    factor_signatures.sort_unstable();
    let evaluation_feedback_sha256 = canonical_json_hash(&serde_json::json!({
        "termination_reason": &result.termination_reason,
        "rounds": result.rounds.iter().map(|round| serde_json::json!({
            "round_id": &round.round_id,
            "termination_reason": &round.termination_reason,
            "selected_candidate_id": &round.feedback.supervised_selected_candidate_id,
            "selected": &round.feedback.supervised_selected,
            "replay": &round.feedback.supervised_replay,
        })).collect::<Vec<_>>(),
    }))?;
    CexCampaignResearchEvidenceSignatureV2::new(
        request.campaign_inputs_sha256.clone(),
        request
            .research_plan
            .search_policy_revision
            .revision_id
            .clone(),
        feature_fields_sha256,
        canonical_json_hash(&factor_signatures)?,
        evaluation_feedback_sha256,
    )
}

fn next_campaign_policy_revision(
    loaded: &LoadedRequest,
    result_sha256: &str,
    failure_class: CexCampaignFailureClassV1,
) -> anyhow::Result<(
    CexCampaignSearchPolicyRevisionV1,
    CexCampaignLearningDirectiveV1,
)> {
    if loaded.request.research_plan.holding.is_some() {
        bail!("fixed holding comparison has no automatic follow-up; retain all arm results");
    }
    let preferred_position_policy = match failure_class {
        CexCampaignFailureClassV1::NoTradesAfterCosts => {
            CexCampaignPositionPolicyV1::PredictionIdentity
        }
        CexCampaignFailureClassV1::OvertradeCapacity => {
            CexCampaignPositionPolicyV1::HystereticCostAware
        }
        CexCampaignFailureClassV1::PositiveIcNegativeNet => {
            CexCampaignPositionPolicyV1::HystereticCostAware
        }
    };
    let current = &loaded.request.research_plan.search_policy_revision;
    let attempted = loaded
        .request
        .research_plan
        .attempted_search_policy_revision_ids
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let candidate = loaded
        .request
        .research_plan
        .allowed_search_policy_revisions
        .iter()
        .filter(|revision| {
            revision.position_policy == preferred_position_policy
                && !attempted.contains(&revision.revision_id)
        })
        .min_by_key(|revision| {
            (
                usize::from(revision.position_policy != preferred_position_policy),
                usize::from(revision.research_delta.is_none()),
                revision.revision_id.clone(),
            )
        })
        .context("Campaign learning has no untried declared research delta")?;
    let rollback_policy_revision_id = current.revision_id.clone();
    let delta = candidate
        .research_delta
        .as_ref()
        .context("declared follow-up revision is missing its typed delta")?;
    let revision = CexCampaignSearchPolicyRevisionV1::new_typed(
        Some(rollback_policy_revision_id.clone()),
        candidate.position_policy,
        CexCampaignResearchDeltaV1 {
            feature_fields: delta.feature_fields.clone(),
            operators: delta.operators.clone(),
            windows: delta.windows.clone(),
            ridge_l2: delta.ridge_l2,
            cart_max_depth: delta.cart_max_depth,
            cart_min_leaf: delta.cart_min_leaf,
        },
    )?;
    if revision.revision_id != candidate.revision_id {
        bail!("Campaign learning candidate revision identity drifted");
    }
    let parent = CexCampaignResearchParentV1 {
        campaign_id: loaded.request.campaign_id.clone(),
        request_sha256: loaded.sha256.clone(),
        campaign_result_sha256: result_sha256.to_string(),
    };
    let directive = CexCampaignLearningDirectiveV1::new(
        &parent,
        failure_class,
        rollback_policy_revision_id,
        revision.revision_id.clone(),
    )?;
    Ok((revision, directive))
}

fn follow_up_plan(
    loaded: &LoadedRequest,
    result_sha256: &str,
    learning_directive: CexCampaignLearningDirectiveV1,
    search_policy_revision: CexCampaignSearchPolicyRevisionV1,
    parent_evidence_signature: CexCampaignResearchEvidenceSignatureV2,
) -> anyhow::Result<CexCampaignResearchPlanV1> {
    if loaded
        .request
        .research_plan
        .representation_binding
        .is_some()
    {
        bail!("representation comparison requires a new validated proposal; automatic follow-up cannot discard its frozen goal");
    }
    if loaded.request.research_plan.mlp_training.is_some() {
        bail!("paired MLP diagnostics require a new root plan with matched factors and initialization; automatic follow-up is not supported");
    }
    let delta_scope = search_policy_revision
        .research_delta
        .as_ref()
        .map(|delta| {
            format!(
                "the declared bounded delta (features {:?}, operators {:?}, windows {:?}, Ridge ridge_l2 {}, CART max_depth {}, CART min_leaf {})",
                delta.feature_fields,
                delta.operators,
                delta.windows,
                delta.ridge_l2,
                delta.cart_max_depth,
                delta.cart_min_leaf,
            )
        })
        .unwrap_or_else(|| "the registered position policy".to_string());
    let hypothesis = match learning_directive.failure_class {
        CexCampaignFailureClassV1::NoTradesAfterCosts => {
            format!(
                "The {delta_scope} with prediction-identity positioning will restore non-zero replay trades after costs while fees, labels, partitions, and holdout remain fixed"
            )
        }
        CexCampaignFailureClassV1::OvertradeCapacity => {
            format!(
                "The {delta_scope} with hysteretic cost-aware positioning will reduce replay turnover and capacity breaches while fees, labels, partitions, and holdout remain fixed"
            )
        }
        CexCampaignFailureClassV1::PositiveIcNegativeNet => {
            format!(
                "The {delta_scope} with hysteretic cost-aware positioning will improve replay net returns by reducing turnover costs while fees, labels, partitions, and holdout remain fixed"
            )
        }
    };
    let mut attempted_search_policy_revision_ids = loaded
        .request
        .research_plan
        .attempted_search_policy_revision_ids
        .clone();
    attempted_search_policy_revision_ids.push(search_policy_revision.revision_id.clone());
    let feature_fields = search_policy_revision
        .research_delta
        .as_ref()
        .map(|delta| delta.feature_fields.clone())
        .unwrap_or_else(|| loaded.request.research_plan.feature_fields.clone());
    if !feature_fields
        .iter()
        .any(|field| field == &loaded.request.research_plan.focus_field)
    {
        bail!("declared follow-up feature subset removed the focus field");
    }
    let plan = CexCampaignResearchPlanV1 {
        calendar: None,
        development_precheck: None,
        supervised_model_scope: loaded.request.research_plan.supervised_model_scope,
        holding: loaded.request.research_plan.holding.clone(),
        comparison_family_trials: loaded.request.research_plan.comparison_family_trials,
        schema_version: "cex-campaign-research-plan-v2".to_string(),
        generation: loaded.request.research_plan.generation + 1,
        objective: format!(
            "Evaluate {:?} bounded research delta follow-up after {}",
            learning_directive.failure_class, loaded.request.campaign_id
        ),
        hypothesis,
        focus_field: loaded.request.research_plan.focus_field.clone(),
        feature_fields,
        label_horizon: loaded.request.research_plan.label_horizon.clone(),
        mlp_training: None,
        search_policy_revision,
        attempted_search_policy_revision_ids,
        allowed_search_policy_revisions: loaded
            .request
            .research_plan
            .allowed_search_policy_revisions
            .clone(),
        parent_evidence_signature: Some(parent_evidence_signature),
        parent: Some(CexCampaignResearchParentV1 {
            campaign_id: loaded.request.campaign_id.clone(),
            request_sha256: loaded.sha256.clone(),
            campaign_result_sha256: result_sha256.to_string(),
        }),
        learning_directive: Some(learning_directive),
        llm: None,
        representation_binding: None,
    };
    plan.validate()?;
    Ok(plan)
}

fn campaign_learn_report(
    output: &Path,
    loaded: &LoadedRequest,
    result_sha256: &str,
    evidence_signature: &CexCampaignResearchEvidenceSignatureV2,
    plan: &CexCampaignResearchPlanV1,
    reused_existing: bool,
) -> anyhow::Result<CampaignLearnReport> {
    let directive = plan
        .learning_directive
        .as_ref()
        .context("Campaign follow-up plan is missing its learning directive")?;
    Ok(CampaignLearnReport {
        parent_campaign_id: loaded.request.campaign_id.clone(),
        parent_request_sha256: loaded.sha256.clone(),
        parent_campaign_result_sha256: result_sha256.to_string(),
        failure_class: directive.failure_class,
        outcome: CampaignLearnOutcomeV1::FollowUp,
        evidence_signature: evidence_signature.clone(),
        learning_directive_sha256: Some(directive.content_hash()?),
        search_policy_revision_id: Some(plan.search_policy_revision.revision_id.clone()),
        research_plan_sha256: Some(plan.content_hash()?),
        output: Some(output.display().to_string()),
        reused_existing,
    })
}

pub fn finalize(args: CampaignFinalizeArgs) -> anyhow::Result<()> {
    if market_encoder::is_market_encoder_freeze(&args.freeze)? {
        return market_encoder::finalize(args);
    }
    if sequence::is_sequence_freeze(&args.freeze)? {
        return sequence::finalize(args);
    }
    if final_evaluation::is_final_freeze(&args.freeze)? {
        return final_evaluation::finalize(args);
    }
    finalize_with_native_readback(args, |request, request_sha256| {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(Policy::none())
            .build()?;
        let readback = tempfile::tempdir().context("native finalized input readback")?;
        prepared_inputs::acquire_native_prepared(request, request_sha256, &client, readback.path())
    })
}

fn finalize_with_native_readback(
    args: CampaignFinalizeArgs,
    acquire: impl FnOnce(
        &CampaignRequest,
        &str,
    ) -> anyhow::Result<prepared_inputs::VerifiedNativeCampaignPreparedInputs>,
) -> anyhow::Result<()> {
    let plan = load_freeze_plan(&args.freeze)?;
    validate_request(&plan.canonical_request)?;
    if expected_campaign_id(&plan.canonical_request)? != plan.canonical_request.campaign_id {
        bail!("frozen campaign request campaign_id does not match its semantic identity");
    }

    let loaded = load_request(&args.signed_request)?;
    validate_request_matches_freeze(&loaded.request, &plan)?;
    if loaded.request.prepared_inputs.is_some() {
        let verified = acquire(&loaded.request, &loaded.sha256)?;
        if verified.finalized_request() != &loaded.request
            || verified.request_sha256() != loaded.sha256
        {
            bail!("native finalized readback verified a different request identity");
        }
        verified.render_inputs();
        research_event(
            "alpha-harness",
            "campaign_prepared_input_readback_verified",
            serde_json::json!({
                "request_sha256":verified.request_sha256(), "campaign_inputs_sha256":verified.campaign_inputs_sha256(),
                "collection_sha256":verified.collection_id(), "view_sha256":verified.view_ids(),
                "evaluation_protocol_sha256":verified.evaluation_protocol_sha256(),
                "source_revision":verified.source_revision(), "runner_image_identity":verified.runner_image_identity(),
                "declared_trials":verified.declared_trials(),
            }),
        );
    }
    hft_research_artifacts::write_json_atomic(&args.request_out, &loaded.request)?;
    let rendered = mission_dispatch::write_submission(
        &args.submission_out,
        &args.attempt_id,
        &args.image,
        loaded.request.clone(),
    )?;
    research_event(
        "alpha-harness",
        "campaign_finalize_completed",
        serde_json::json!({
            "campaign_id": &loaded.request.campaign_id,
            "request_sha256": &rendered.request_sha256,
            "submission_identity_sha256": &rendered.submission_identity_sha256,
            "job_name": &rendered.job_name,
            "attempt_id": &args.attempt_id,
            "image": &args.image,
        }),
    );
    print_json(&CampaignFinalizeReport {
        campaign_id: loaded.request.campaign_id.clone(),
        holdout_id: loaded.request.holdout_id.clone(),
        request_sha256: rendered.request_sha256,
        submission_identity_sha256: rendered.submission_identity_sha256,
        job_name: rendered.job_name,
        request_out: args.request_out.display().to_string(),
        submission_out: args.submission_out.display().to_string(),
    })
}

#[cfg(not(test))]
pub(crate) fn validate_request_for_source(request: &CampaignRequest) -> anyhow::Result<()> {
    validate_request(request)
}

#[cfg(test)]
pub(crate) fn validate_request_for_source(request: &CampaignRequest) -> anyhow::Result<()> {
    validate_request(request).or_else(|_| validate_local_test_request(request))
}

pub(crate) fn validate_execution_readiness(request: &CampaignRequest) -> anyhow::Result<()> {
    if request.prepared_inputs.is_some() && request.research_plan.calendar.is_some() {
        bail!("native calendar execution requires an independently admitted calendar validation projection; normalized search preparation alone is not executable");
    }
    Ok(())
}

pub(crate) fn validate_request_for_execute(request: &CampaignRequest) -> anyhow::Result<()> {
    validate_request_for_source(request)?;
    validate_execution_readiness(request)
}

pub(crate) fn validate_serialized_execution_readiness(
    request_json: &str,
    request_sha256: &str,
) -> anyhow::Result<()> {
    let value: serde_json::Value = serde_json::from_str(request_json)?;
    // Sequence and encoder requests retain their original owning admission.
    if value["schema_version"] != CAMPAIGN_REQUEST_SCHEMA_V6 {
        return Ok(());
    }
    if hex::encode(Sha256::digest(request_json.as_bytes())) != request_sha256 {
        bail!("native execution readiness request differs from inspected request identity");
    }
    let request: CampaignRequest = serde_json::from_value(value)?;
    validate_execution_readiness(&request)
}

#[cfg(feature = "scientific")]
fn execute_loaded_request(args: CampaignExecuteArgs, loaded: LoadedRequest) -> anyhow::Result<()> {
    validate_request_for_execute(&loaded.request)?;
    if !args.pre_holdout {
        bail!(
            "campaign-execute cannot open sealed holdout; pass --pre-holdout, or --final-evaluation with an independent grant"
        );
    }
    let shared_input_dir = args.work_dir.join("shared-inputs");
    let mission_dir = args.work_dir.join("mission");
    let local_request_path = args.work_dir.join("campaign-request.json");
    let local_result_path = args.work_dir.join("campaign-result.json");
    let local_result_readback_path = args.work_dir.join("campaign-result-readback.json");
    for path in [
        &shared_input_dir,
        &mission_dir,
        &local_request_path,
        &local_result_path,
        &local_result_readback_path,
    ] {
        if path.try_exists()? {
            bail!(
                "Campaign execution requires empty campaign paths; existing path: {}",
                path.display()
            );
        }
    }
    std::fs::create_dir_all(&args.work_dir)?;
    std::fs::create_dir_all(&shared_input_dir)?;
    std::fs::create_dir_all(&mission_dir)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(Policy::none())
        .build()?;
    hft_research_artifacts::write_json_atomic(&local_request_path, &loaded.request)?;

    research_event(
        "alpha-harness",
        "campaign_input_download_started",
        serde_json::json!({
            "campaign_id": &loaded.request.campaign_id,
            "request_sha256": &loaded.sha256,
            "expected": {
                "feature_sha256": &loaded.request.feature_sha256,
                "materialization_sha256": &loaded.request.materialization_sha256,
                "replay_artifact_sha256": &loaded.request.replay_artifact_sha256,
                "replay_manifest_sha256": &loaded.request.replay_manifest_sha256,
            },
        }),
    );
    let native_inputs = prepared_inputs::acquire_native_prepared(
        &loaded.request,
        &loaded.sha256,
        &client,
        &shared_input_dir,
    )?;
    let platform_output = platform_output::BoundOutput::from_environment(&loaded, &native_inputs)?;
    let render_inputs = native_inputs.render_inputs();
    research_event(
        "alpha-harness",
        "campaign_input_download_completed",
        serde_json::json!({
            "campaign_id":loaded.request.campaign_id,
            "request_sha256":native_inputs.request_sha256(),
            "prepared_collection_sha256":native_inputs.collection_id(),
            "view_sha256":native_inputs.view_ids(),
            "development_rows":native_inputs.prepared().rows().len(),
            "original_rows":native_inputs.prepared().original_metadata().total_rows,
            "selection_bytes_loaded":false,"holdout_bytes_loaded":false,
        }),
    );
    let mut ledgers = Vec::with_capacity(loaded.request.rounds.len());
    let mut selected_round = None;
    for (round_index, round) in loaded.request.rounds.iter().enumerate() {
        research_event(
            "alpha-harness",
            "campaign_round_started",
            serde_json::json!({
                "campaign_id": &loaded.request.campaign_id,
                "request_sha256": &loaded.sha256,
                "round_id": &round.round_id,
                "round_index": round_index + 1,
                "round_total": loaded.request.rounds.len(),
                "seed": round.seed,
                "research_plan": &loaded.request.research_plan,
            }),
        );
        let rendered = render_prepared_cex_bundle(
            render_inputs,
            &loaded.request.research_plan,
            round.seed,
            loaded.request.declared_total_trials,
        )?;
        if rendered.mission.spec.holdout.holdout_id != loaded.request.holdout_id {
            bail!("rendered Mission holdout ID drifted from the Campaign request");
        }
        let round_dir = mission_dir.join(&round.round_id);
        let mission_publish_dir = round_dir.join("admission");
        std::fs::create_dir_all(&mission_publish_dir)?;
        let mission_local_path = mission_publish_dir.join("mission.json");
        hft_research_artifacts::write_json_atomic(&mission_local_path, &rendered.mission)?;
        let mission_readback_path = mission_publish_dir.join("mission-readback.json");
        let mission_sha256 = publish_create_once_json(
            &client,
            "Mission",
            &round.mission_put_url,
            &round.mission_readback_url,
            &mission_local_path,
            &mission_readback_path,
        )?;
        research_event(
            "alpha-harness",
            "campaign_round_mission_readback_completed",
            serde_json::json!({
                "campaign_id": &loaded.request.campaign_id,
                "round_id": &round.round_id,
                "mission_id": &rendered.mission_id,
                "mission_sha256": &mission_sha256,
            }),
        );

        let binding = ExecutionBinding::Campaign {
            campaign_id: loaded.request.campaign_id.clone(),
            round_id: round.round_id.clone(),
            request_sha256: loaded.sha256.clone(),
        };
        let recovered_result_path = round_dir.join("published-result-readback.zip");
        let execute_dir = round_dir.join("execute");
        let (report, reused_published_result) = if let Some(report) =
            recover_execution_report_from_published_result(
                &client,
                &round.result_readback_url,
                &recovered_result_path,
                &rendered.mission_id,
                &mission_sha256,
                &binding,
                Some((
                    native_inputs.finalized_request(),
                    native_inputs.request_sha256(),
                )),
            )? {
            extract_bundle(&recovered_result_path, &execute_dir)?;
            (report, true)
        } else {
            let (round_claim_put_url, round_claim_readback_url) =
                campaign_round_claim_urls(&loaded.request, round)?;
            (
                crate::mission_runner::execute_prepared_report(
                    ExecuteMissionArgs {
                        work_dir: execute_dir.clone(),
                        mission_id: rendered.mission_id.clone(),
                        holdout_id: loaded.request.holdout_id.clone(),
                        mission_url: mission_readback_path.to_string_lossy().into_owned(),
                        mission_sha256: mission_sha256.clone(),
                        feature_url: String::new(),
                        materialization_url: String::new(),
                        replay_artifact_url: String::new(),
                        replay_artifact_sha256: loaded.request.replay_artifact_sha256.clone(),
                        replay_manifest_url: String::new(),
                        replay_manifest_sha256: loaded.request.replay_manifest_sha256.clone(),
                        resume_url: None,
                        resume_sha256: None,
                        result_put_url: round.result_put_url.clone(),
                        result_readback_url: round.result_readback_url.clone(),
                        holdout_claim_put_url: round_claim_put_url,
                        holdout_claim_readback_url: round_claim_readback_url,
                    },
                    binding,
                    &native_inputs,
                    render_inputs,
                    &shared_input_dir,
                )?,
                false,
            )
        };
        crate::mission_runner::validate_native_campaign_result_binding(
            &execute_dir.join("results"),
            &native_inputs,
        )?;
        let ledger = collect_round_ledger(&execute_dir, round, &report)?;
        research_event(
            "alpha-harness",
            "campaign_round_completed",
            serde_json::json!({
                "campaign_id": &loaded.request.campaign_id,
                "round_id": &round.round_id,
                "mission_id": &ledger.mission_id,
                "mission_sha256": &ledger.mission_sha256,
                "result_bundle_sha256": &ledger.result_bundle_sha256,
                "result_readback_bundle_sha256": &ledger.result_readback_bundle_sha256,
                "reused_published_result": reused_published_result,
                "consumed_trials": ledger.consumed_trials,
                "termination_reason": &ledger.termination_reason,
                "selected_candidate_id": &ledger.selected_candidate_id,
                "selected_score": ledger.selected_score,
                "feedback": round_log_summary(&ledger),
                "replay_gate_passed": ledger
                    .supervised_replay_gate_passed
                    .or(ledger.replay_gate_passed),
            }),
        );
        let is_better = selected_round
            .as_ref()
            .is_none_or(|current: &CampaignMissionLedgerV1| {
                compare_round_selection(&ledger, current).is_gt()
            });
        let selected_gate = ledger
            .supervised_replay_gate_passed
            .or(ledger.replay_gate_passed);
        if selected_gate == Some(true) && ledger.selected_score.is_some() && is_better {
            research_event(
                "alpha-harness",
                "campaign_pre_holdout_selection_updated",
                serde_json::json!({
                    "campaign_id": &loaded.request.campaign_id,
                    "round_id": &ledger.round_id,
                    "candidate_id": &ledger.selected_candidate_id,
                    "selected_score": ledger.selected_score,
                    "replay_gate_passed": selected_gate,
                }),
            );
            selected_round = Some(ledger.clone());
        }
        ledgers.push(ledger);
    }

    let consumed_trials = ledgers.iter().map(|round| round.consumed_trials).sum();
    if consumed_trials > loaded.request.declared_total_trials {
        bail!("campaign consumed trials exceeded declared_total_trials");
    }
    research_event(
        "alpha-harness",
        "campaign_rounds_completed",
        serde_json::json!({
            "campaign_id": &loaded.request.campaign_id,
            "round_count": ledgers.len(),
            "consumed_trials": consumed_trials,
            "declared_total_trials": loaded.request.declared_total_trials,
            "selected_round_id": selected_round.as_ref().map(|round| round.round_id.as_str()),
            "selected_candidate_id": selected_round
                .as_ref()
                .and_then(|round| round.selected_candidate_id.as_deref()),
        }),
    );
    let finalization = None;

    let result = CampaignResultV1 {
        schema_version: CAMPAIGN_RESULT_SCHEMA_V9.to_string(),
        campaign_id: loaded.request.campaign_id.clone(),
        request_sha256: loaded.sha256.clone(),
        build_source_revision: loaded.request.build_source_revision.clone(),
        image_identity: loaded.request.image_identity.clone(),
        campaign_inputs_sha256: loaded.request.campaign_inputs_sha256.clone(),
        producer_source_revision: loaded.request.producer_source_revision.clone(),
        producer_image_identity: loaded.request.producer_image_identity.clone(),
        research_plan_sha256: loaded.request.research_plan.content_hash()?,
        learning_directive: loaded.request.research_plan.learning_directive.clone(),
        learning_directive_sha256: loaded
            .request
            .research_plan
            .learning_directive
            .as_ref()
            .map(CexCampaignLearningDirectiveV1::content_hash)
            .transpose()?,
        search_policy_revision: Some(loaded.request.research_plan.search_policy_revision.clone()),
        holdout_id: loaded.request.holdout_id.clone(),
        declared_total_trials: loaded.request.declared_total_trials,
        consumed_trials,
        stop_rule: STOP_RULE_V2.to_string(),
        termination_reason: if selected_round.is_some() {
            "campaign_selected_pre_holdout".to_string()
        } else {
            "campaign_no_candidate".to_string()
        },
        rounds: ledgers,
        selected_round_id: selected_round.as_ref().map(|round| round.round_id.clone()),
        selected_candidate_id: selected_round
            .as_ref()
            .and_then(|round| round.selected_candidate_id.clone()),
        selected_candidate_content_hash: selected_round
            .as_ref()
            .and_then(|round| round.selected_candidate_content_hash.clone()),
        finalization,
    };
    research_event(
        "alpha-harness",
        "campaign_result_publish_started",
        serde_json::json!({
            "campaign_id": &result.campaign_id,
            "request_sha256": &result.request_sha256,
            "termination_reason": &result.termination_reason,
            "consumed_trials": result.consumed_trials,
            "declared_total_trials": result.declared_total_trials,
            "selected_round_id": &result.selected_round_id,
            "selected_candidate_id": &result.selected_candidate_id,
        }),
    );
    hft_research_artifacts::write_json_atomic(&local_result_path, &result)?;
    if local_result_path.metadata()?.len() > MAX_CAMPAIGN_RESULT_BYTES {
        bail!("campaign result exceeds {MAX_CAMPAIGN_RESULT_BYTES} bytes");
    }
    let result_sha256 = publish_create_once_json(
        &client,
        "campaign result",
        &loaded.request.campaign_result_put_url,
        &loaded.request.campaign_result_readback_url,
        &local_result_path,
        &local_result_readback_path,
    )?;
    research_event(
        "alpha-harness",
        "campaign_execution_completed",
        serde_json::json!({
            "campaign_id": &result.campaign_id,
            "request_sha256": &result.request_sha256,
            "campaign_result_sha256": &result_sha256,
            "campaign_result_readback_sha256": &result_sha256,
            "termination_reason": &result.termination_reason,
            "selected_round_id": &result.selected_round_id,
            "selected_candidate_id": &result.selected_candidate_id,
            "round_count": result.rounds.len(),
            "consumed_trials": result.consumed_trials,
        }),
    );
    if let Some(output) = platform_output {
        tokio::runtime::Handle::try_current()
            .context("platform worker requires the admitted async runtime")?
            .block_on(output.publish(
                &loaded,
                &native_inputs,
                &result,
                &result_sha256,
                &args.work_dir,
            ))?;
    }
    print_json(&serde_json::json!({
        "campaign_id": result.campaign_id,
        "request_sha256": result.request_sha256,
        "campaign_result_sha256": result_sha256,
        "campaign_result_readback_sha256": result_sha256,
        "termination_reason": result.termination_reason,
        "selected_round_id": result.selected_round_id,
        "selected_candidate_id": result.selected_candidate_id,
        "finalization": result.finalization,
        "rounds": result.rounds,
    }))
}

struct ValidatedCampaignInputSet {
    native_prepared:
        std::collections::BTreeMap<String, prepared_inputs::NativePreparedCampaignRefV1>,
    input_root: PathBuf,
    replay_artifact_path: PathBuf,
    replay_manifest_path: PathBuf,
    receipt: CampaignInputsReceipt,
    campaign_inputs_sha256: String,
    build_source_revision: String,
    campaign_root: String,
    image_identity: String,
    producer_image_identity: String,
    render_inputs: PreparedCexInputs,
    feature_url: String,
    materialization_url: String,
    replay_artifact_url: String,
    replay_manifest_url: String,
    feature_sha256: String,
    materialization_sha256: String,
    replay_artifact_sha256: String,
    replay_manifest_sha256: String,
}

fn validated_campaign_inputs(
    args: &CampaignFreezeArgs,
    retain_calendar_rows: bool,
) -> anyhow::Result<ValidatedCampaignInputSet> {
    let (receipt, campaign_inputs_sha256) = load_campaign_inputs_receipt(&args.campaign_inputs)?;
    validate_campaign_inputs_receipt(&receipt)?;
    let build_source_revision =
        normalized_source_revision("campaign source revision", &args.source_revision)?;
    if build_source_revision != BUILD_SOURCE_REVISION {
        bail!("campaign source revision does not match this build");
    }
    let feature_url =
        canonical_tokyo_oss_internal_object("campaign feature", &receipt.feature.object_url)?;
    let materialization_url = canonical_tokyo_oss_internal_object(
        "campaign materialization",
        &receipt.materialization.object_url,
    )?;
    let replay_artifact_url = canonical_tokyo_oss_internal_object(
        "campaign replay artifact",
        &receipt.replay_artifact.object_url,
    )?;
    let replay_manifest_url = canonical_tokyo_oss_internal_object(
        "campaign replay manifest",
        &receipt.replay_manifest.object_url,
    )?;
    let campaign_root = canonical_tokyo_oss_internal_object("campaign root", &args.campaign_root)?;
    let image_identity = mission_dispatch::image_digest(&args.image)?;
    let producer_image_identity = mission_dispatch::image_digest(&receipt.image_ref)?;
    let feature_path = args.input_root.join(&receipt.feature.relative_path);
    let materialization_path = args.input_root.join(&receipt.materialization.relative_path);
    let replay_artifact_path = args.input_root.join(&receipt.replay_artifact.relative_path);
    let replay_manifest_path = args.input_root.join(&receipt.replay_manifest.relative_path);
    let render_inputs =
        PreparedCexInputs::load(&feature_path, &materialization_path, retain_calendar_rows)?;
    let feature_sha256 = render_inputs.feature_sha256().to_string();
    let materialization_sha256 = render_inputs.materialization_sha256().to_string();
    for (label, actual, expected) in [
        ("campaign feature", &feature_sha256, &receipt.feature.sha256),
        (
            "campaign materialization",
            &materialization_sha256,
            &receipt.materialization.sha256,
        ),
    ] {
        if *actual != normalized_sha256(label, expected)? {
            bail!("{label} local file SHA256 does not match the receipt");
        }
    }
    if render_inputs.materialization().market != receipt.market
        || render_inputs.materialization().symbol != receipt.symbol
    {
        bail!("campaign inputs receipt instrument does not match its materialization");
    }
    let replay_artifact_sha256 =
        normalized_sha256("campaign replay artifact", &receipt.replay_artifact.sha256)?;
    let replay_manifest_sha256 =
        normalized_sha256("campaign replay manifest", &receipt.replay_manifest.sha256)?;
    // The canonical verifier already hashes both files before checking replay
    // structure. Do not hash them once more in this caller.
    verify_canonical_replay_artifact_streaming(
        &replay_artifact_path,
        &replay_manifest_path,
        Some(&replay_artifact_sha256),
        &replay_manifest_sha256,
        None,
        None,
    )?;
    Ok(ValidatedCampaignInputSet {
        native_prepared: std::collections::BTreeMap::new(),
        input_root: args.input_root.clone(),
        replay_artifact_path,
        replay_manifest_path,
        receipt,
        campaign_inputs_sha256,
        build_source_revision,
        campaign_root,
        image_identity,
        producer_image_identity,
        render_inputs,
        feature_url,
        materialization_url,
        replay_artifact_url,
        replay_manifest_url,
        feature_sha256,
        materialization_sha256,
        replay_artifact_sha256,
        replay_manifest_sha256,
    })
}

fn freeze_request(args: &CampaignFreezeArgs) -> anyhow::Result<(CampaignRequest, String)> {
    let research_plan = args
        .research_plan
        .as_deref()
        .map(load_research_plan)
        .transpose()?
        .unwrap_or_else(CexCampaignResearchPlanV1::canonical);
    if let Some(plan) = &research_plan.mlp_training {
        plan.validate_requested_seeds(&args.seeds)
            .map_err(anyhow::Error::msg)?;
    }
    let study_proposal = args
        .study_proposal
        .as_deref()
        .map(load_study_proposal)
        .transpose()?;
    validate_study_proposal_for_plan(study_proposal.as_ref(), &research_plan)?;
    if args.reuse.is_some() || args.reuse_sha256.is_some() {
        return reuse_frozen_request(args, &research_plan, study_proposal.as_ref());
    }
    let inputs = validated_campaign_inputs(args, true)?;
    freeze_prepared_request(
        &inputs,
        &research_plan,
        &args.seeds,
        study_proposal.as_ref(),
    )
}

fn freeze_prepared_request(
    inputs: &ValidatedCampaignInputSet,
    research_plan: &CexCampaignResearchPlanV1,
    seeds: &[u64],
    study_proposal: Option<&CampaignNextFamilyProposalV1>,
) -> anyhow::Result<(CampaignRequest, String)> {
    research_plan.validate()?;
    representation::validate_campaign_seeds(research_plan, seeds)?;
    if research_plan.calendar.is_some() && seeds != [7, 11] {
        bail!("calendar H1 requires exactly seeds 7 and 11");
    }
    if let Some(plan) = &research_plan.mlp_training {
        plan.validate_requested_seeds(seeds)
            .map_err(anyhow::Error::msg)?;
    }
    validate_study_proposal_for_plan(study_proposal, research_plan)?;
    let declared_total_trials = declared_total_trials_for_rounds(research_plan, seeds.len())?;
    let probe_seed = *seeds
        .first()
        .context("campaign freeze requires at least one seed")?;
    let rendered = render_prepared_cex_bundle(
        &inputs.render_inputs,
        research_plan,
        probe_seed,
        declared_total_trials,
    )?;
    let protocol = crate::mission_render::approved_evaluation_protocol_for_plan(
        inputs.render_inputs.materialization(),
        research_plan,
    )?;
    let prepared = if let Some(prepared) = inputs.native_prepared.get(&protocol.content_hash()?) {
        let source = &prepared.expected_native.source;
        if source.preparation_receipt_sha256 != inputs.campaign_inputs_sha256
            || source.feature_sha256 != inputs.feature_sha256
            || source.materialization_sha256 != inputs.materialization_sha256
            || source.replay_artifact_sha256 != inputs.replay_artifact_sha256
            || source.replay_manifest_sha256 != inputs.replay_manifest_sha256
            || prepared.expected_native.native_protocol_sha256 != protocol.content_hash()?
        {
            bail!("trusted prepared collection differs from original native source/protocol");
        }
        prepared.clone()
    } else {
        prepared_inputs::freeze_native_prepared_reference(inputs, research_plan)?
    };
    let mut frozen_inputs = inputs.receipt.clone();
    frozen_inputs.prepared_inputs = Some(prepared.clone());
    let frozen_inputs_sha =
        hft_cex_research_input::sha256(&serde_json::to_vec_pretty(&frozen_inputs)?);
    // Trusted cached preparation can render from an authenticated collection
    // while the original source mount is offline. Do not create a shadow mount.
    if inputs.input_root.try_exists()? {
        write_json_create_once(
            &inputs
                .input_root
                .join(format!("native-prepared/{frozen_inputs_sha}.inputs.json")),
            &frozen_inputs,
        )?;
    }
    Ok((
        build_request_from_parts(
            &inputs.feature_url,
            &inputs.feature_sha256,
            &inputs.materialization_url,
            &inputs.materialization_sha256,
            &inputs.replay_artifact_url,
            &inputs.replay_artifact_sha256,
            &inputs.replay_manifest_url,
            &inputs.replay_manifest_sha256,
            &frozen_inputs_sha,
            &inputs.receipt.source_revision,
            &inputs.producer_image_identity,
            Some(&prepared),
            research_plan,
            &inputs.build_source_revision,
            &inputs.image_identity,
            &inputs.campaign_root,
            &rendered.mission.spec.holdout.holdout_id,
            seeds,
            study_proposal,
        )?,
        frozen_inputs_sha,
    ))
}

fn reuse_frozen_request(
    args: &CampaignFreezeArgs,
    research_plan: &CexCampaignResearchPlanV1,
    study_proposal: Option<&CampaignNextFamilyProposalV1>,
) -> anyhow::Result<(CampaignRequest, String)> {
    let path = args
        .reuse
        .as_deref()
        .context("prepared freeze path is required")?;
    let expected_sha = normalized_sha256(
        "prepared freeze",
        args.reuse_sha256
            .as_deref()
            .context("prepared freeze SHA256 is required")?,
    )?;
    let (frozen, actual_sha) = load_freeze_plan_with_sha(path)?;
    if actual_sha != expected_sha {
        bail!("prepared freeze SHA256 mismatch");
    }
    let ledger = alpha_store::AlphaStore::open_read_only(
        args.preparation_ledger
            .as_deref()
            .context("prepared freeze requires an independently selected trusted ledger")?,
    )?;
    preparation::verify_authentication(
        &ledger,
        &frozen,
        frozen.preparation_authentication_tag.as_deref(),
    )?;
    let (mut receipt, original_receipt_sha) = load_campaign_inputs_receipt(&args.campaign_inputs)?;
    validate_campaign_inputs_receipt(&receipt)?;
    let receipt_sha = if let Some(prepared) = &frozen.canonical_request.prepared_inputs {
        if receipt.prepared_inputs.is_some()
            || prepared.expected_native.source.preparation_receipt_sha256 != original_receipt_sha
        {
            bail!("authenticated prepared freeze differs from the original native source receipt");
        }
        receipt.prepared_inputs = Some(prepared.clone());
        hft_cex_research_input::sha256(&serde_json::to_vec_pretty(&receipt)?)
    } else {
        original_receipt_sha
    };
    let source = normalized_source_revision("campaign source revision", &args.source_revision)?;
    if source != BUILD_SOURCE_REVISION {
        bail!("campaign source revision does not match this build");
    }
    let expected = build_request_from_parts(
        &receipt.feature.object_url,
        &receipt.feature.sha256,
        &receipt.materialization.object_url,
        &receipt.materialization.sha256,
        &receipt.replay_artifact.object_url,
        &receipt.replay_artifact.sha256,
        &receipt.replay_manifest.object_url,
        &receipt.replay_manifest.sha256,
        &receipt_sha,
        &receipt.source_revision,
        &mission_dispatch::image_digest(&receipt.image_ref)?,
        receipt.prepared_inputs.as_ref(),
        research_plan,
        &source,
        &mission_dispatch::image_digest(&args.image)?,
        &args.campaign_root,
        &frozen.canonical_request.holdout_id,
        &args.seeds,
        study_proposal,
    )?;
    if frozen.campaign_inputs_sha256 != receipt_sha
        || frozen.canonical_request != expected
        || frozen.signing_plan != signing_plan(&expected)?
    {
        bail!("prepared freeze differs from requested input, source, image, plan, seeds or output identity");
    }
    validate_request(&expected)?;
    Ok((expected, receipt_sha))
}

fn load_campaign_inputs_receipt(path: &Path) -> anyhow::Result<(CampaignInputsReceipt, String)> {
    let mut file = File::open(path)
        .with_context(|| format!("open campaign inputs receipt {}", path.display()))?;
    if file.metadata()?.len() > MAX_REQUEST_BYTES {
        bail!("campaign inputs receipt exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    let receipt: CampaignInputsReceipt = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse campaign inputs receipt {}", path.display()))?;
    Ok((receipt, hex::encode(Sha256::digest(&bytes))))
}

fn validate_campaign_inputs_receipt(receipt: &CampaignInputsReceipt) -> anyhow::Result<()> {
    if receipt.schema_version != CAMPAIGN_INPUTS_SCHEMA_V1 {
        bail!("campaign inputs receipt schema_version must be {CAMPAIGN_INPUTS_SCHEMA_V1}");
    }
    validate_receipt_identifier("campaign inputs run_id", &receipt.run_id)?;
    normalized_source_revision(
        "campaign inputs receipt source_revision",
        &receipt.source_revision,
    )?;
    validate_receipt_identifier("campaign inputs mission_id", &receipt.mission_id)?;
    if receipt.market != "usdm" {
        bail!("campaign inputs receipt market must be usdm");
    }
    validate_render_instrument_scope(&receipt.market, &receipt.symbol)?;
    if receipt.readback_scope != "same-mounted-ossfs-prefix" {
        bail!("campaign inputs receipt readback_scope must be same-mounted-ossfs-prefix");
    }
    let output_root = campaign_inputs_output_root(receipt)?;
    mission_dispatch::image_digest(&receipt.image_ref)?;
    validate_campaign_input_receipt_item("campaign feature", &receipt.feature, &output_root)?;
    validate_campaign_input_receipt_item(
        "campaign materialization",
        &receipt.materialization,
        &output_root,
    )?;
    validate_campaign_input_receipt_item(
        "campaign replay artifact",
        &receipt.replay_artifact,
        &output_root,
    )?;
    validate_campaign_input_receipt_item(
        "campaign replay manifest",
        &receipt.replay_manifest,
        &output_root,
    )?;
    Ok(())
}

fn validate_campaign_input_receipt_item(
    label: &str,
    item: &CampaignInputReceiptItem,
    output_root: &str,
) -> anyhow::Result<()> {
    let object = canonical_tokyo_oss_internal_object(label, &item.object_url)?;
    normalized_sha256(label, &item.sha256)?;
    if item.relative_path.as_os_str().is_empty()
        || item.relative_path.is_absolute()
        || item
            .relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("{label} relative_path must be a safe relative path");
    }
    if !object.starts_with(&format!("{output_root}/")) {
        bail!("{label} object_url must live under the campaign inputs output root");
    }
    Ok(())
}

fn campaign_inputs_output_root(receipt: &CampaignInputsReceipt) -> anyhow::Result<String> {
    let base = canonical_https_object_prefix(
        "campaign inputs output_object_base_url",
        &receipt.output_object_base_url,
    )?;
    validate_relative_output_prefix(&receipt.output_prefix)?;
    Ok(format!(
        "{base}/{}",
        receipt.output_prefix.trim_matches('/')
    ))
}

fn canonical_https_object_prefix(label: &str, value: &str) -> anyhow::Result<String> {
    if value != value.trim() || value.chars().any(char::is_control) {
        bail!("{label} must not contain surrounding whitespace or control characters");
    }
    let mut url = reqwest::Url::parse(value).with_context(|| format!("{label} is invalid"))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        bail!("{label} must be HTTPS with a host");
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        bail!("{label} must not contain credentials or a query");
    }
    if url.fragment().is_some() || url.path() == "/" || url.path().ends_with('/') {
        bail!("{label} must identify a canonical prefix path");
    }
    url.set_query(None);
    url.set_fragment(None);
    let canonical = url.to_string();
    let host = url
        .host_str()
        .context("campaign inputs output root host is missing")?;
    if !host.ends_with(&format!(
        ".{}",
        hft_research_dispatch_io::TOKYO_OSS_INTERNAL_ENDPOINT
    )) {
        bail!("{label} must target the Tokyo OSS internal endpoint");
    }
    Ok(canonical)
}

fn validate_relative_output_prefix(value: &str) -> anyhow::Result<()> {
    let value = value.trim_matches('/');
    if value.is_empty() {
        bail!("campaign inputs output_prefix is invalid");
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir
                    | std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        bail!("campaign inputs output_prefix must be a safe relative path");
    }
    Ok(())
}

fn validate_receipt_identifier(label: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() || value != value.trim() || value.chars().any(char::is_control) {
        bail!("{label} is invalid");
    }
    Ok(())
}

fn verify_local_receipt_item(
    label: &str,
    path: &Path,
    expected_sha256: &str,
) -> anyhow::Result<String> {
    let actual = hft_research_artifacts::sha256_file(path)?;
    if actual != normalized_sha256(label, expected_sha256)? {
        bail!("{label} local file SHA256 does not match the receipt");
    }
    Ok(actual)
}

fn normalized_source_revision(label: &str, source_revision: &str) -> anyhow::Result<String> {
    if source_revision != source_revision.trim() || source_revision.chars().any(char::is_control) {
        bail!("{label} must not contain surrounding whitespace or control characters");
    }
    if !valid_git_revision(source_revision) {
        bail!("{label} must be an exact git revision");
    }
    Ok(source_revision.to_string())
}

fn extract_bundle(bundle: &Path, destination: &Path) -> anyhow::Result<()> {
    extract_bundle_with_file_limit(bundle, destination, MAX_RESULT_BUNDLE_FILES)
}

fn extract_bundle_with_file_limit(
    bundle: &Path,
    destination: &Path,
    max_files: usize,
) -> anyhow::Result<()> {
    if destination.try_exists()? {
        return Ok(());
    }
    std::fs::create_dir_all(destination)?;
    let mut archive = ZipArchive::new(File::open(bundle)?)?;
    if archive.len() > max_files {
        bail!("published result bundle contains too many entries");
    }
    let mut extracted_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let enclosed = entry
            .enclosed_name()
            .context("published result bundle contains a non-enclosed path")?
            .to_owned();
        let output = destination.join(&enclosed);
        if entry.is_dir() {
            std::fs::create_dir_all(&output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&output)
            .context("published bundle has a duplicate file or unsafe destination")?;
        let remaining = MAX_RESULT_BUNDLE_BYTES
            .checked_sub(extracted_bytes)
            .context("published result bundle exceeds its extracted-size limit")?;
        let bytes = std::io::copy(&mut entry.by_ref().take(remaining + 1), &mut file)?;
        if bytes > remaining {
            bail!("published result bundle exceeds its extracted-size limit");
        }
        extracted_bytes += bytes;
    }
    Ok(())
}

struct SupervisedRoundEvidence {
    ridge: CexSupervisedModelCandidateV2,
    cart: Option<CexSupervisedModelCandidateV2>,
    burn: Option<CexSupervisedModelCandidateV2>,
    selected: CexSupervisedModelCandidateV2,
    replay: Option<CexEventReplayReceiptV1>,
    calendar_validation: Option<alpha_engine::final_models::CalendarValidationReportV1>,
    calendar_replay: Option<CexEventReplayReceiptV1>,
    selection_required: bool,
}

fn load_supervised_round_evidence(
    results: &Path,
    mission: &alpha_domain::CexResearchMissionArtifactV1,
    factor_bank: &CexFactorBankRevisionV2,
    ridge_baseline: Option<&alpha_domain::CexBaselineArtifactV1>,
    cart_baseline: Option<&alpha_domain::CexBaselineArtifactV1>,
    burn_baseline: Option<&alpha_domain::CexBaselineArtifactV1>,
    report: &crate::mission_runner::ExecutionReport,
) -> anyhow::Result<Option<SupervisedRoundEvidence>> {
    if factor_bank.entries.is_empty() {
        if report.supervised_candidate_id.is_some()
            || report.supervised_replay_receipt_id.is_some()
            || report.supervised_replay_gate_passed.is_some()
            || results
                .join("supervised-model-selection.json")
                .try_exists()?
        {
            bail!("empty Factor Bank cannot produce supervised ML artifacts");
        }
        return Ok(None);
    }
    let read_candidate = |name: &str,
                          baseline: Option<&alpha_domain::CexBaselineArtifactV1>|
     -> anyhow::Result<Option<CexSupervisedModelCandidateV2>> {
        let path = results.join(format!("{name}-supervised-candidate.json"));
        if !mission.spec.supervised_model_scope.names().contains(&name) {
            if baseline.is_some() || path.exists() {
                bail!("result contains an unrequested supervised model");
            }
            return Ok(None);
        }
        let baseline = baseline.context("requested supervised model baseline is missing")?;
        let candidate = serde_json::from_slice(&std::fs::read(path)?)?;
        validate_supervised_candidate_binding(&candidate, mission, factor_bank, baseline)?;
        Ok(Some(candidate))
    };
    let ridge = read_candidate("ridge", ridge_baseline)?.context("Ridge candidate missing")?;
    let cart = read_candidate("cart", cart_baseline)?;
    let burn = read_candidate("burn_mlp", burn_baseline)?;
    let selection: CexSupervisedModelSelectionV1 = serde_json::from_slice(&std::fs::read(
        results.join("supervised-model-selection.json"),
    )?)?;
    selection.validate()?;
    if selection.mission_id != mission.semantic_id()? {
        bail!("supervised selection does not match its Campaign Mission");
    }
    let selected = [Some(&ridge), cart.as_ref(), burn.as_ref()]
        .into_iter()
        .flatten()
        .find(|candidate| {
            candidate.artifact_id == selection.selected_candidate.id
                && canonical_json_hash(*candidate)
                    .is_ok_and(|hash| hash == selection.selected_candidate.content_sha256)
        })
        .cloned()
        .context("supervised selection does not bind Ridge, CART, or Burn MLP")?;
    if selection.replay_eligible != selected.evaluation.passed
        || report.supervised_candidate_id.as_deref() != Some(selected.artifact_id.as_str())
    {
        bail!("supervised selection verdict drifted from its model evidence");
    }
    let replay_path = results.join("supervised-event-replay-receipt.json");
    let replay = replay_path
        .try_exists()?
        .then(|| std::fs::read(&replay_path))
        .transpose()?
        .map(|bytes| serde_json::from_slice::<CexEventReplayReceiptV1>(&bytes))
        .transpose()?;
    if let Some(replay) = &replay {
        validate_supervised_replay_binding(
            replay,
            &selection,
            &selected,
            mission,
            &selection.mission_id,
        )?;
    } else if selection.replay_eligible {
        bail!("eligible supervised selection is missing its event replay receipt");
    }
    if report.supervised_replay_receipt_id.as_deref()
        != replay.as_ref().map(|receipt| receipt.receipt_id.as_str())
        || report.supervised_replay_gate_passed
            != replay.as_ref().map(|receipt| receipt.gate.passed)
    {
        bail!("supervised replay report drifted from its receipt");
    }
    let calendar_validation = if let Some(calendar) = &mission.spec.evaluation_protocol.calendar {
        let path = results.join("calendar-validation.json");
        if !path.try_exists()?
            && results
                .join("native-prepared-admission.json")
                .try_exists()?
        {
            None
        } else {
            let validation: alpha_engine::final_models::CalendarValidationReportV1 =
                serde_json::from_slice(&std::fs::read(path)?)?;
            validation.report.evaluation.validate()?;
            if &validation.calendar != calendar
                || validation.promotion_authority
                || validation.source_candidate.id != ridge.artifact_id
                || validation.source_candidate.content_sha256 != canonical_json_hash(&ridge)?
                || validation.report.evaluation.protocol_binding()?.1
                    != mission.spec.evaluation_protocol.content_hash()?
                || validation.report.evaluation.evaluator_version
                    != alpha_domain::frozen_model::INDEPENDENT_SELECTION_EVALUATOR_VERSION
            {
                bail!("calendar validation is not bound to this Ridge and calendar");
            }
            Some(validation)
        }
    } else {
        None
    };
    let calendar_replay = if let Some(validation) = &calendar_validation {
        let path = results.join("calendar-validation-event-replay-receipt.json");
        if crate::mission_runner::calendar_replay_required(validation) {
            let replay = serde_json::from_slice(&std::fs::read(path)?)?;
            crate::mission_runner::validate_calendar_replay_binding(&replay, validation, mission)?;
            Some(replay)
        } else {
            if path.exists() {
                bail!("zero-position calendar validation has unexpected replay");
            }
            None
        }
    } else {
        None
    };
    Ok(Some(SupervisedRoundEvidence {
        ridge,
        cart,
        burn,
        selected,
        replay,
        calendar_validation,
        calendar_replay,
        selection_required: mission.spec.evaluation_protocol.calendar.is_some(),
    }))
}

fn collect_round_ledger(
    execute_dir: &Path,
    round: &CampaignRoundRequest,
    report: &crate::mission_runner::ExecutionReport,
) -> anyhow::Result<CampaignMissionLedgerV1> {
    let results = execute_dir.join("results");
    let factor_bank: CexFactorBankRevisionV2 =
        serde_json::from_slice(&std::fs::read(results.join("factor-bank.json"))?)?;
    let baseline_gate: CexBaselineGateV1 =
        serde_json::from_slice(&std::fs::read(results.join("baseline-gate.json"))?)?;
    let control_mission: alpha_domain::CexResearchMissionArtifactV1 =
        serde_json::from_slice(&std::fs::read(results.join("control-plane-mission.json"))?)?;
    let baseline_policy: alpha_domain::CexBaselinePolicyV1 =
        serde_json::from_slice(&std::fs::read(results.join("baseline-policy.json"))?)?;
    let ridge = if factor_bank.entries.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice::<alpha_domain::CexBaselineArtifactV1>(&std::fs::read(
                results.join("ridge-baseline.json"),
            )?)?,
        )
    };
    let cart = if factor_bank.entries.is_empty()
        || !control_mission.spec.supervised_model_scope.is_default()
    {
        None
    } else {
        Some(
            serde_json::from_slice::<alpha_domain::CexBaselineArtifactV1>(&std::fs::read(
                results.join("cart-baseline.json"),
            )?)?,
        )
    };
    let burn = if factor_bank.entries.is_empty()
        || !control_mission.spec.supervised_model_scope.is_default()
        || !matches!(
            factor_bank.gp_policy.schema_version.as_str(),
            alpha_domain::CEX_GP_POLICY_SCHEMA_V4 | alpha_domain::CEX_GP_POLICY_SCHEMA_V5
        ) {
        None
    } else {
        Some(
            serde_json::from_slice::<alpha_domain::CexBaselineArtifactV1>(&std::fs::read(
                results.join("burn-mlp-baseline.json"),
            )?)?,
        )
    };
    if report.mission_id != control_mission.semantic_id()? {
        bail!("round execution report mission identity drifted");
    }
    baseline_gate
        .validate_binding(
            &control_mission,
            &baseline_policy,
            &factor_bank,
            ridge.as_ref(),
            cart.as_ref(),
        )
        .map_err(anyhow::Error::msg)?;
    let supervised_ml = matches!(
        factor_bank.gp_policy.schema_version.as_str(),
        alpha_domain::CEX_GP_POLICY_SCHEMA_V4 | alpha_domain::CEX_GP_POLICY_SCHEMA_V5
    );
    let supervised = if supervised_ml {
        load_supervised_round_evidence(
            &results,
            &control_mission,
            &factor_bank,
            ridge.as_ref(),
            cart.as_ref(),
            burn.as_ref(),
            report,
        )?
    } else {
        if report.supervised_candidate_id.is_some()
            || report.supervised_replay_receipt_id.is_some()
            || report.supervised_replay_gate_passed.is_some()
        {
            bail!("legacy Campaign round contains supervised ML evidence");
        }
        None
    };
    let model_attempts = persisted_supervised_model_attempt_count(
        &results,
        control_mission.spec.supervised_model_scope.names(),
    )?;
    if supervised_ml
        && !factor_bank.entries.is_empty()
        && model_attempts.is_none_or(|attempts| attempts == 0)
    {
        bail!("supervised ML round has no persisted model attempts");
    }
    let feedback = CampaignRoundFeedbackV1 {
        factor_attempts: factor_bank.attempts.len(),
        model_attempts,
        accepted_factors: factor_bank.entries.len(),
        factors: factor_bank
            .attempts
            .iter()
            .map(|attempt| {
                Ok(CampaignFactorFeedbackV1 {
                    factor_signature_sha256: canonical_json_hash(&attempt.canonical_ast)?,
                    source_features: factor_ast_source_features(&attempt.canonical_ast),
                    rejection_codes: attempt.rejection_codes.clone(),
                    evaluation: attempt
                        .evaluation
                        .as_ref()
                        .map(|evaluation| campaign_evaluation_feedback(&evaluation.evidence)),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        baseline_gate_passed: baseline_gate.passed,
        baseline_failure_codes: baseline_gate.failure_codes.clone(),
        ridge: ridge
            .as_ref()
            .map(|artifact| campaign_evaluation_feedback(&artifact.evaluation)),
        cart: cart
            .as_ref()
            .map(|artifact| campaign_evaluation_feedback(&artifact.evaluation)),
        burn: burn
            .as_ref()
            .map(|artifact| campaign_evaluation_feedback(&artifact.evaluation)),
        supervised_ridge: supervised
            .as_ref()
            .map(|evidence| campaign_evaluation_feedback(&evidence.ridge.evaluation)),
        supervised_cart: supervised
            .as_ref()
            .and_then(|evidence| evidence.cart.as_ref())
            .map(|candidate| campaign_evaluation_feedback(&candidate.evaluation)),
        supervised_burn: supervised
            .as_ref()
            .and_then(|evidence| evidence.burn.as_ref())
            .map(|candidate| campaign_evaluation_feedback(&candidate.evaluation)),
        supervised_selected: supervised
            .as_ref()
            .map(|evidence| campaign_evaluation_feedback(&evidence.selected.evaluation)),
        supervised_selected_evaluation_proof: supervised
            .as_ref()
            .map(|evidence| {
                CampaignSelectedEvaluationProofV1::from_evaluation(&evidence.selected.evaluation)
            })
            .transpose()?,
        supervised_selected_candidate_id: supervised
            .as_ref()
            .map(|evidence| evidence.selected.artifact_id.clone()),
        supervised_replay: supervised
            .as_ref()
            .and_then(|evidence| evidence.replay.as_ref())
            .map(campaign_replay_feedback),
        calendar_validation: supervised
            .as_ref()
            .and_then(|evidence| evidence.calendar_validation.as_ref())
            .map(|validation| campaign_evaluation_feedback(&validation.report.evaluation)),
    };
    let strategy_path = results.join("combination-walk-forward.json");
    let subset_result = load_round_subset_result(&results)?;
    let model_attempt_count = model_attempts.unwrap_or(0);
    let consumed_trials = factor_bank
        .attempts
        .len()
        .checked_add(model_attempt_count)
        .and_then(|count| {
            count.checked_add(if supervised_ml {
                0
            } else {
                subset_result
                    .as_ref()
                    .map(|result| result.candidates_evaluated)
                    .unwrap_or(0)
            })
        })
        .context("Campaign round trial count overflowed")?;
    let strategy_exists = strategy_path.try_exists()?;
    let (
        termination_reason,
        selected_candidate_id,
        selected_candidate_content_hash,
        selected_score,
    ) = if factor_bank.entries.is_empty() {
        if subset_result.is_some() || strategy_exists || report.replay_receipt_id.is_some() {
            bail!("empty Factor Bank cannot produce subset search artifacts");
        }
        ("no_accepted_factors".to_string(), None, None, None)
    } else if supervised_ml {
        if subset_result.is_some()
            || strategy_exists
            || report.replay_receipt_id.is_some()
            || report.replay_gate_passed.is_some()
        {
            bail!("supervised ML round cannot produce legacy subset-search artifacts");
        }
        let evidence = supervised
            .as_ref()
            .context("supervised ML round is missing model selection evidence")?;
        if evidence.selection_required && evidence.calendar_validation.is_none() {
            (
                "insufficient_selection_evidence".to_string(),
                None,
                None,
                None,
            )
        } else if evidence
            .calendar_validation
            .as_ref()
            .is_some_and(|validation| !validation.report.evaluation.passed)
        {
            (
                "calendar_validation_gate_failed".to_string(),
                None,
                None,
                None,
            )
        } else if evidence
            .calendar_replay
            .as_ref()
            .is_some_and(|replay| !replay.gate.passed)
        {
            (
                "calendar_validation_replay_gate_failed".to_string(),
                None,
                None,
                None,
            )
        } else if !evidence.selected.evaluation.passed {
            ("no_passing_supervised_model".to_string(), None, None, None)
        } else {
            let replay = evidence
                .replay
                .as_ref()
                .context("passing supervised model is missing event replay")?;
            if replay.gate.passed {
                (
                    "supervised_pre_holdout_candidate_kept".to_string(),
                    Some(evidence.selected.artifact_id.clone()),
                    Some(canonical_json_hash(&evidence.selected)?),
                    Some(evidence.selected.evaluation.score),
                )
            } else {
                (
                    "supervised_replay_gate_failed".to_string(),
                    None,
                    None,
                    None,
                )
            }
        }
    } else if !baseline_gate.passed {
        if subset_result.is_some() || strategy_exists || report.replay_receipt_id.is_some() {
            bail!("failed baseline gate cannot produce subset search artifacts");
        }
        ("baseline_gate_failed".to_string(), None, None, None)
    } else {
        let subset_result =
            subset_result.context("non-empty Factor Bank is missing factor subset MCTS result")?;
        if subset_result.selected.is_none() {
            if strategy_exists {
                bail!("subset search produced no passing selection but strategy artifact exists");
            }
            if report.replay_receipt_id.is_some() {
                bail!("subset search produced no passing selection but replay receipt exists");
            }
            ("no_passing_subset".to_string(), None, None, None)
        } else {
            if !strategy_exists {
                bail!("passing subset selection is missing combination strategy artifact");
            }
            if report.replay_receipt_id.is_none() {
                bail!("passing subset selection is missing event replay receipt");
            }
            let replay_gate_passed = report
                .replay_gate_passed
                .context("replay receipt is missing its gate verdict")?;
            if replay_gate_passed {
                let strategy: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&strategy_path)?)?;
                (
                    "pre_holdout_candidate_kept".to_string(),
                    strategy["artifact_id"].as_str().map(str::to_string),
                    Some(canonical_json_hash(&strategy)?),
                    strategy["walk_forward_evidence"]["selected"]["evaluation"]["score"].as_f64(),
                )
            } else {
                ("replay_gate_failed".to_string(), None, None, None)
            }
        }
    };
    Ok(CampaignMissionLedgerV1 {
        round_id: round.round_id.clone(),
        seed: round.seed,
        identity: round.identity.clone(),
        mission_id: report.mission_id.clone(),
        mission_sha256: report.mission_sha256.clone(),
        request_sha256: report.request_sha256.clone(),
        result_bundle_sha256: report.bundle_sha256.clone(),
        result_readback_bundle_sha256: report.readback_bundle_sha256.clone(),
        replay_receipt_id: report.replay_receipt_id.clone(),
        replay_gate_passed: report.replay_gate_passed,
        supervised_candidate_id: report.supervised_candidate_id.clone(),
        supervised_replay_receipt_id: report.supervised_replay_receipt_id.clone(),
        supervised_replay_gate_passed: report.supervised_replay_gate_passed,
        final_precommit_id: report.final_precommit_id.clone(),
        sealed_receipt_id: report.sealed_receipt_id.clone(),
        sealed_passed: report.sealed_passed,
        strategy_bundle_id: report.strategy_bundle_id.clone(),
        promotion_id: report.promotion_id.clone(),
        selected_candidate_id,
        selected_candidate_content_hash,
        selected_score,
        consumed_trials,
        termination_reason,
        feedback,
    })
}

fn preserve_result_feedback_shape(
    observed: &mut CampaignMissionLedgerV1,
    schema_version: &str,
) -> anyhow::Result<()> {
    match schema_version {
        CAMPAIGN_RESULT_SCHEMA_V8 => {
            observed.feedback.supervised_selected_evaluation_proof = None;
            Ok(())
        }
        CAMPAIGN_RESULT_SCHEMA_V9 => Ok(()),
        _ => bail!("unsupported Campaign result feedback version"),
    }
}

fn campaign_evaluation_feedback(evaluation: &CandidateEvaluation) -> CampaignEvaluationFeedbackV1 {
    let max_book_depth_fraction = evaluation
        .metrics
        .folds
        .iter()
        .filter_map(|fold| fold.max_book_depth_fraction)
        .max_by(f64::total_cmp);
    let max_book_depth_fraction_limit = evaluation
        .evaluation_protocol
        .as_ref()
        .filter(|protocol| protocol.costs.capacity_enabled())
        .map(|protocol| protocol.costs.max_book_depth_fraction);
    CampaignEvaluationFeedbackV1 {
        passed: evaluation.passed,
        score: evaluation.score,
        time_series_ic: evaluation.metrics.predictive.time_series_ic,
        time_series_rank_ic: evaluation.metrics.predictive.time_series_rank_ic,
        cumulative_net_return: evaluation.metrics.cumulative_net_return,
        max_drawdown: evaluation.metrics.max_drawdown,
        net_sharpe: evaluation.metrics.net_sharpe,
        trade_count: evaluation.metrics.trade_count,
        total_turnover: evaluation.metrics.total_turnover,
        max_book_depth_fraction,
        max_book_depth_fraction_limit,
        capacity_breached: max_book_depth_fraction
            .zip(max_book_depth_fraction_limit)
            .is_some_and(|(observed, limit)| observed > limit),
    }
}

fn campaign_replay_feedback(receipt: &CexEventReplayReceiptV1) -> CampaignReplayFeedbackV1 {
    CampaignReplayFeedbackV1 {
        passed: receipt.gate.passed,
        failures: receipt.gate.failures.clone(),
        position_changes: receipt.metrics.position_changes,
        total_turnover: receipt.metrics.total_turnover,
        mean_net_return: receipt.metrics.mean_net_return,
        cumulative_net_return: receipt.metrics.cumulative_net_return,
        max_drawdown: receipt.metrics.max_drawdown,
        net_sharpe: receipt.metrics.net_sharpe,
    }
}

#[cfg(feature = "scientific")]
fn campaign_round_claim_urls(
    request: &CampaignRequest,
    round: &CampaignRoundRequest,
) -> anyhow::Result<(String, String)> {
    let remote =
        round.result_put_url.starts_with("https://") || round.result_put_url.starts_with("http://");
    if remote {
        Ok((
            request.holdout_claim_put_url.clone(),
            request.holdout_claim_readback_url.clone(),
        ))
    } else {
        let claim = crate::mission_objects::cex_campaign_round_result_and_holdout_claim(
            &round.result_put_url,
            &request.campaign_id,
            &round.round_id,
            &request.holdout_id,
        )?;
        Ok((claim.clone(), claim))
    }
}

fn load_round_subset_result(results: &Path) -> anyhow::Result<Option<CexFactorBankMctsResultV1>> {
    let subset_path = results.join("factor-subset-mcts-result.json");
    if !subset_path.try_exists()? {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&std::fs::read(subset_path)?)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisedModelAttemptLedgerV1 {
    schema_version: String,
    attempts: Vec<SupervisedModelAttemptEntryV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisedModelAttemptEntryV1 {
    model: String,
    outcome: String,
}

fn persisted_supervised_model_attempt_count(
    results: &Path,
    model_names: &[&str],
) -> anyhow::Result<Option<usize>> {
    let attempts_path = results.join("supervised-model-attempts.json");
    if attempts_path.try_exists()? {
        let ledger: SupervisedModelAttemptLedgerV1 =
            serde_json::from_slice(&std::fs::read(attempts_path)?)?;
        if ledger.schema_version != "cex-supervised-model-attempts-v1" {
            bail!("supervised model attempt ledger schema is invalid");
        }
        let expected_models = model_names
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if ledger.attempts.len() != expected_models.len() {
            bail!("supervised model attempt ledger model count is invalid");
        }
        let mut models = std::collections::BTreeSet::new();
        for attempt in &ledger.attempts {
            let model = attempt.model.as_str();
            let outcome = attempt.outcome.as_str();
            if model.trim().is_empty()
                || !expected_models.contains(model)
                || !matches!(outcome, "admitted" | "started" | "completed" | "failed")
                || !models.insert(model)
            {
                bail!("supervised model attempt ledger is invalid");
            }
            let backtest = results.join(format!("{model}-supervised-backtest.json"));
            let candidate = results.join(format!("{model}-supervised-candidate.json"));
            let has_backtest = backtest.try_exists()?;
            let has_candidate = candidate.try_exists()?;
            match outcome {
                "completed" if has_backtest && has_candidate => {}
                "completed" => {
                    bail!("completed supervised model attempt is missing its artifacts")
                }
                _ if has_backtest || has_candidate => {
                    bail!("non-completed supervised model attempt has model artifacts")
                }
                _ => {}
            }
        }
        if models != expected_models {
            bail!("supervised model attempt ledger model set is incomplete");
        }
        if results
            .join("supervised-model-selection.json")
            .try_exists()?
            && ledger
                .attempts
                .iter()
                .any(|attempt| attempt.outcome != "completed")
        {
            bail!("supervised model selection has incomplete model attempts");
        }
        return Ok(Some(models.len()));
    }
    Ok(None)
}

fn compare_round_selection(
    left: &CampaignMissionLedgerV1,
    right: &CampaignMissionLedgerV1,
) -> std::cmp::Ordering {
    left.selected_score
        .partial_cmp(&right.selected_score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            right
                .selected_candidate_content_hash
                .cmp(&left.selected_candidate_content_hash)
        })
        .then_with(|| right.round_id.cmp(&left.round_id))
}

fn load_freeze_plan(path: &Path) -> anyhow::Result<FrozenCampaignPlan> {
    Ok(load_freeze_plan_with_sha(path)?.0)
}

fn load_freeze_plan_with_sha(path: &Path) -> anyhow::Result<(FrozenCampaignPlan, String)> {
    let file = File::open(path)
        .with_context(|| format!("open campaign freeze plan {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        bail!("campaign freeze plan exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let sha = hex::encode(Sha256::digest(&bytes));
    let plan: FrozenCampaignPlan = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse campaign freeze plan {}", path.display()))?;
    if plan.schema_version != CAMPAIGN_FREEZE_SCHEMA_V1 {
        bail!("campaign freeze plan schema_version must be {CAMPAIGN_FREEZE_SCHEMA_V1}");
    }
    Ok((plan, sha))
}

pub fn print_expected_id(args: CampaignIdArgs) -> anyhow::Result<()> {
    let loaded = load_request(&args.request)?;
    let expected = expected_campaign_id(&loaded.request)?;
    print_json(&CampaignIdReport {
        campaign_id: expected.clone(),
        matches_request: loaded.request.campaign_id == expected,
    })
}

fn load_request(path: &Path) -> anyhow::Result<LoadedRequest> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open campaign request {}", path.display()))?;
    if file.metadata()?.len() > MAX_REQUEST_BYTES {
        bail!("campaign request exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    let request: CampaignRequest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse campaign request {}", path.display()))?;
    Ok(LoadedRequest {
        request,
        sha256: hex::encode(Sha256::digest(&bytes)),
    })
}

fn load_campaign_result(path: &Path) -> anyhow::Result<CampaignResultV1> {
    let mut file =
        File::open(path).with_context(|| format!("open Campaign result {}", path.display()))?;
    if file.metadata()?.len() > MAX_CAMPAIGN_RESULT_BYTES {
        bail!("Campaign result exceeds {MAX_CAMPAIGN_RESULT_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse Campaign result {}", path.display()))
}

fn load_research_plan(path: &Path) -> anyhow::Result<CexCampaignResearchPlanV1> {
    let mut file = File::open(path)
        .with_context(|| format!("open CEX Campaign research plan {}", path.display()))?;
    if file.metadata()?.len() > MAX_REQUEST_BYTES {
        bail!("CEX Campaign research plan exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let plan: CexCampaignResearchPlanV1 = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse CEX Campaign research plan {}", path.display()))?;
    plan.validate()?;
    Ok(plan)
}

fn load_study_proposal(path: &Path) -> anyhow::Result<CampaignNextFamilyProposalV1> {
    let mut file = File::open(path)
        .with_context(|| format!("open next-family Campaign proposal {}", path.display()))?;
    if file.metadata()?.len() > MAX_REQUEST_BYTES {
        bail!("next-family Campaign proposal exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let proposal: CampaignNextFamilyProposalV1 = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse next-family Campaign proposal {}", path.display()))?;
    proposal.validate().map_err(anyhow::Error::msg)?;
    Ok(proposal)
}

fn validate_study_proposal_for_plan(
    proposal: Option<&CampaignNextFamilyProposalV1>,
    plan: &CexCampaignResearchPlanV1,
) -> anyhow::Result<()> {
    let Some(proposal) = proposal else {
        return Ok(());
    };
    proposal.validate().map_err(anyhow::Error::msg)?;
    let plan_sha256 = plan.content_hash()?;
    if proposal.target_research_plan_sha256 != plan_sha256 {
        bail!("next-family proposal research plan hash differs from the frozen plan");
    }
    if plan.label_horizon.as_ref() != Some(&proposal.target_horizon) {
        bail!("next-family proposal horizon differs from the frozen research plan");
    }
    let parent = plan
        .parent
        .as_ref()
        .context("next-family proposal requires a parent-bound research plan")?;
    if proposal.parent.campaign_id != parent.campaign_id
        || proposal.parent.request_sha256 != parent.request_sha256
        || proposal.parent.campaign_result_sha256 != parent.campaign_result_sha256
    {
        bail!("next-family proposal parent identity differs from the research plan");
    }
    let evidence = plan
        .parent_evidence_signature
        .as_ref()
        .context("next-family proposal requires parent evidence")?;
    if evidence.campaign_inputs_sha256 != proposal.parent.campaign_inputs_sha256 {
        bail!("next-family proposal parent input identity differs from parent evidence");
    }
    Ok(())
}

fn write_research_plan_create_once(
    path: &Path,
    plan: &CexCampaignResearchPlanV1,
) -> anyhow::Result<()> {
    plan.validate()?;
    write_json_create_once(path, plan)
}

fn write_json_create_once<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if path.try_exists()? {
        let existing = std::fs::read(path)
            .with_context(|| format!("read existing create-once JSON {}", path.display()))?;
        if existing == bytes {
            return Ok(());
        }
        bail!("existing create-once JSON differs: {}", path.display());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.as_file_mut().write_all(&bytes)?;
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(path)?;
            if existing == bytes {
                Ok(())
            } else {
                bail!("existing create-once JSON differs: {}", path.display())
            }
        }
        Err(error) => {
            Err(error.error).with_context(|| format!("create create-once JSON {}", path.display()))
        }
    }
}

pub(crate) fn validate_terminal_mission_revision_binding(
    mission: &alpha_domain::CexResearchMissionArtifactV1,
    request: &CampaignRequest,
) -> anyhow::Result<()> {
    if let Some(precheck) = &request.research_plan.development_precheck {
        if mission.spec.evaluation_protocol != precheck.protocol
            || precheck.feature_sha256 != request.feature_sha256
            || precheck.materialization_sha256 != request.materialization_sha256
        {
            bail!("terminal Mission calendar or precheck differs from the admitted request");
        }
    } else if mission.spec.evaluation_protocol.calendar.is_some() {
        bail!("terminal Mission added an unrequested calendar");
    }
    let expected_mlp = request
        .research_plan
        .mlp_training
        .as_ref()
        .map(|plan| plan.resolve(mission.spec.search.seed))
        .transpose()
        .map_err(anyhow::Error::msg)?;
    if mission.spec.mlp_training != expected_mlp {
        bail!("terminal Mission MLP profile differs from the admitted paired training plan");
    }
    if mission.spec.supervised_model_scope != request.research_plan.supervised_model_scope {
        bail!("terminal Mission changed the approved model scope");
    }
    if request.research_plan.comparison_family_trials.is_some()
        || !request.research_plan.supervised_model_scope.is_default()
    {
        let expected_trials = request
            .research_plan
            .effective_multiple_testing_trials(request.declared_total_trials)?;
        let policy = crate::mission_runner::bound_baseline_policy(mission)?;
        if policy.evaluator_config.multiple_testing_trials != expected_trials
            || !matches!(
                policy.schema_version.as_str(),
                alpha_domain::CEX_BASELINE_POLICY_SCHEMA_V4
                    | alpha_domain::CEX_BASELINE_POLICY_SCHEMA_V5
            )
        {
            bail!("terminal supervised score omitted the registered comparison family");
        }
    }
    let expected = &request.research_plan.search_policy_revision;
    match (
        expected.research_delta.as_ref(),
        mission.spec.research_delta.as_ref(),
    ) {
        (None, None) => {}
        (Some(expected_delta), Some(observed_delta)) if expected_delta == observed_delta => {
            let rebuilt = CexCampaignSearchPolicyRevisionV1::new_typed(
                None,
                expected.position_policy,
                CexCampaignResearchDeltaV1 {
                    feature_fields: expected_delta.feature_fields.clone(),
                    operators: expected_delta.operators.clone(),
                    windows: expected_delta.windows.clone(),
                    ridge_l2: expected_delta.ridge_l2,
                    cart_max_depth: expected_delta.cart_max_depth,
                    cart_min_leaf: expected_delta.cart_min_leaf,
                },
            )?;
            if rebuilt.revision_id != expected.revision_id {
                bail!("terminal Mission delta does not recompute the approved revision ID");
            }
        }
        _ => bail!("terminal Mission research delta differs from the approved revision"),
    }
    let expected_decision_hash = request
        .research_plan
        .decision_policy_for_market(mission.spec.instrument.market.clone())?
        .content_hash()
        .map_err(anyhow::Error::msg)?;
    if mission.spec.policies.supervised_decision.id != expected.revision_id
        || mission.spec.policies.supervised_decision.content_sha256 != expected_decision_hash
    {
        bail!("terminal Mission decision policy revision differs from the approved revision");
    }
    Ok(())
}

fn validate_existing_follow_up_plan(
    plan: &CexCampaignResearchPlanV1,
    loaded: &LoadedRequest,
    result_sha256: &str,
    expected_directive: &CexCampaignLearningDirectiveV1,
    expected_revision: &CexCampaignSearchPolicyRevisionV1,
    expected_evidence_signature: &CexCampaignResearchEvidenceSignatureV2,
) -> anyhow::Result<()> {
    plan.validate()?;
    let parent = plan
        .parent
        .as_ref()
        .context("existing research plan is not a follow-up")?;
    if parent.campaign_id != loaded.request.campaign_id
        || parent.request_sha256 != loaded.sha256
        || parent.campaign_result_sha256 != result_sha256
        || plan.generation != loaded.request.research_plan.generation + 1
        || plan.learning_directive.as_ref() != Some(expected_directive)
        || plan.parent_evidence_signature.as_ref() != Some(expected_evidence_signature)
        || &plan.search_policy_revision != expected_revision
        || plan.allowed_search_policy_revisions
            != loaded.request.research_plan.allowed_search_policy_revisions
        || plan.supervised_model_scope != loaded.request.research_plan.supervised_model_scope
        || plan.holding != loaded.request.research_plan.holding
        || plan.mlp_training != loaded.request.research_plan.mlp_training
        || plan
            .attempted_search_policy_revision_ids
            .strip_suffix(std::slice::from_ref(&expected_revision.revision_id))
            != Some(
                loaded
                    .request
                    .research_plan
                    .attempted_search_policy_revision_ids
                    .as_slice(),
            )
    {
        bail!("existing research plan does not match the parent Campaign evidence");
    }
    let expected_feature_fields = expected_revision
        .research_delta
        .as_ref()
        .map(|delta| delta.feature_fields.clone())
        .unwrap_or_else(|| loaded.request.research_plan.feature_fields.clone());
    if plan.focus_field != loaded.request.research_plan.focus_field
        || plan.feature_fields != expected_feature_fields
    {
        bail!("existing research plan changed fields outside the learning directive");
    }
    Ok(())
}

/// Reconstructs every round from independently read-back immutable artifacts.
/// This validates evidence only: it does not train, replay, open holdout or promote.
#[cfg(test)]
#[cfg_attr(all(test, not(feature = "scientific")), allow(dead_code))]
pub(crate) fn readback_pre_holdout_terminal(
    client: &Client,
    request: &CampaignRequest,
    request_sha256: &str,
    evaluation_protocol_sha256: &str,
) -> anyhow::Result<(
    alpha_domain::campaign_control::CampaignAttemptOutcomeV1,
    u64,
    String,
)> {
    let root = tempfile::tempdir()?;
    readback_pre_holdout_terminal_into(
        client,
        request,
        request_sha256,
        evaluation_protocol_sha256,
        root.path(),
    )
}

pub(crate) fn readback_pre_holdout_terminal_into(
    client: &Client,
    request: &CampaignRequest,
    request_sha256: &str,
    evaluation_protocol_sha256: &str,
    root: &Path,
) -> anyhow::Result<(
    alpha_domain::campaign_control::CampaignAttemptOutcomeV1,
    u64,
    String,
)> {
    readback_pre_holdout_terminal_impl(
        client,
        request,
        request_sha256,
        evaluation_protocol_sha256,
        root,
        None,
    )
}

pub(crate) fn readback_pre_holdout_terminal_cached(
    client: &Client,
    request: &CampaignRequest,
    request_sha256: &str,
    evaluation_protocol_sha256: &str,
    cache: &Path,
) -> anyhow::Result<(
    alpha_domain::campaign_control::CampaignAttemptOutcomeV1,
    u64,
    String,
)> {
    validate_terminal_cache(cache)?;
    let temporary = tempfile::tempdir()?;
    readback_pre_holdout_terminal_impl(
        client,
        request,
        request_sha256,
        evaluation_protocol_sha256,
        temporary.path(),
        Some(cache),
    )
}

fn validate_terminal_cache(cache: &Path) -> anyhow::Result<()> {
    for path in [cache.to_path_buf(), cache.join("round-readback")] {
        let metadata = std::fs::symlink_metadata(&path).context("inspect ACK terminal cache")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!("ACK terminal cache must use regular directories");
        }
    }
    Ok(())
}

fn verify_cached_terminal_file(path: &Path, digest: &str, limit: u64) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path).context("inspect cached terminal artifact")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        bail!("cached terminal artifact is not a bounded regular file");
    }
    if hft_research_artifacts::sha256_file(path)?
        != normalized_sha256("cached terminal artifact", digest)?
    {
        bail!("cached terminal artifact differs from the authenticated Campaign result");
    }
    Ok(())
}

fn readback_pre_holdout_terminal_impl(
    client: &Client,
    request: &CampaignRequest,
    request_sha256: &str,
    evaluation_protocol_sha256: &str,
    root: &Path,
    cache: Option<&Path>,
) -> anyhow::Result<(
    alpha_domain::campaign_control::CampaignAttemptOutcomeV1,
    u64,
    String,
)> {
    use alpha_domain::campaign_control::CampaignAttemptOutcomeV1;
    let result_path = root.join("campaign-result.json");
    let (_, result_sha256) = fetch_to_file(
        client,
        &request.campaign_result_readback_url,
        &result_path,
        MAX_CAMPAIGN_RESULT_BYTES,
    )
    .map_err(terminal_readback_error)?;
    let result = load_campaign_result(&result_path)?;
    let loaded = LoadedRequest {
        request: request.clone(),
        sha256: request_sha256.into(),
    };
    validate_campaign_result_identity(&loaded, &result, &result_sha256)?;
    if let Some(cache) = cache {
        verify_cached_terminal_file(
            &cache.join("campaign-result.json"),
            &result_sha256,
            MAX_CAMPAIGN_RESULT_BYTES,
        )?;
    }
    if result.finalization.is_some() || result.rounds.len() != request.rounds.len() {
        bail!("dispatch settlement requires complete pre-holdout rounds");
    }
    let mut consumed = 0usize;
    let mut selected: Option<&CampaignMissionLedgerV1> = None;
    for (index, (round, expected)) in request.rounds.iter().zip(&result.rounds).enumerate() {
        if round.round_id != expected.round_id
            || round.seed != expected.seed
            || round.identity != expected.identity
        {
            bail!("terminal Campaign round identity differs from the request");
        }
        let dir = root.join(format!("round-{index}"));
        std::fs::create_dir_all(&dir)?;
        let mission_path = if let Some(cache) = cache {
            let path = cache
                .join("round-readback")
                .join(format!("round-{index}-mission.json"));
            verify_cached_terminal_file(&path, &expected.mission_sha256, MAX_REQUEST_BYTES)?;
            path
        } else {
            let path = dir.join("mission.json");
            fetch_verified(
                client,
                "terminal Mission",
                &round.mission_readback_url,
                &path,
                &expected.mission_sha256,
                MAX_REQUEST_BYTES,
            )
            .map_err(terminal_readback_error)?;
            path
        };
        let mission: alpha_domain::CexResearchMissionArtifactV1 =
            serde_json::from_slice(&std::fs::read(&mission_path)?)?;
        mission.validate()?;
        validate_terminal_mission_revision_binding(&mission, request)?;
        if mission.semantic_id()? != expected.mission_id
            || mission.spec.inputs.feature.content_sha256 != request.feature_sha256
            || mission.spec.inputs.materialization.content_sha256 != request.materialization_sha256
            || mission.spec.evaluation_protocol.content_hash()? != evaluation_protocol_sha256
            || mission.spec.feature_fields != request.research_plan.feature_fields
            || mission.spec.search.seed != round.seed
            || mission.spec.search.multiple_testing_trials
                != request
                    .research_plan
                    .effective_multiple_testing_trials(request.declared_total_trials)?
            || mission.spec.holdout.holdout_id != request.holdout_id
            || mission.spec.holdout.state != alpha_domain::CexResearchHoldoutStateV1::Unopened
        {
            bail!("terminal Mission does not bind the reserved data, policy, trials and evaluation protocol");
        }
        let bundle_path = cache.map_or_else(
            || dir.join("result.zip"),
            |cache| {
                cache
                    .join("round-readback")
                    .join(format!("round-{index}-results.zip"))
            },
        );
        let binding = ExecutionBinding::Campaign {
            campaign_id: request.campaign_id.clone(),
            round_id: round.round_id.clone(),
            request_sha256: request_sha256.into(),
        };
        let report = if cache.is_some() {
            recover_execution_report_from_cached_result(
                &bundle_path,
                &expected.result_bundle_sha256,
                &expected.mission_id,
                &expected.mission_sha256,
                &binding,
                request
                    .prepared_inputs
                    .as_ref()
                    .map(|_| (request, request_sha256)),
            )?
        } else {
            recover_execution_report_from_published_result(
                client,
                &round.result_readback_url,
                &bundle_path,
                &expected.mission_id,
                &expected.mission_sha256,
                &binding,
                request
                    .prepared_inputs
                    .as_ref()
                    .map(|_| (request, request_sha256)),
            )
            .map_err(terminal_readback_error)?
            .context("terminal round bundle is absent")?
        };
        if report.bundle_sha256 != expected.result_bundle_sha256
            || report.bundle_sha256 != expected.result_readback_bundle_sha256
        {
            bail!("terminal round bundle SHA256 differs from the Campaign result");
        }
        let extracted = dir.join("extracted");
        extract_bundle(&bundle_path, &extracted)?;
        for artifact in [
            "sealed-holdout-claim.json",
            "sealed-holdout-claim-readback.json",
            "sealed-holdout-receipt.json",
            "sealed-evaluations.jsonl",
            "strategy-bundle.json",
            "promotion-record.json",
            "finalization-report.json",
        ] {
            if extracted.join("results").join(artifact).try_exists()? {
                bail!("pre-holdout settlement rejects terminal artifact {artifact}");
            }
        }
        let mut observed = collect_round_ledger(&extracted, round, &report)?;
        // Retain the historical shape without rewriting any original receipt.
        preserve_result_feedback_shape(&mut observed, &result.schema_version)?;
        if observed != *expected
            || observed.sealed_receipt_id.is_some()
            || observed.sealed_passed.is_some()
            || observed.strategy_bundle_id.is_some()
            || observed.promotion_id.is_some()
        {
            bail!("terminal round evidence differs from its reconstructed pre-holdout artifacts");
        }
        consumed = consumed
            .checked_add(observed.consumed_trials)
            .context("terminal trial count overflow")?;
        if observed
            .supervised_replay_gate_passed
            .or(observed.replay_gate_passed)
            == Some(true)
            && observed.selected_score.is_some()
            && selected.is_none_or(|best| compare_round_selection(expected, best).is_gt())
        {
            selected = Some(expected);
        }
    }
    if consumed != result.consumed_trials || consumed > request.declared_total_trials {
        bail!("terminal Campaign trial total differs from reconstructed rounds");
    }
    let outcome = if let Some(selected) = selected {
        if result.termination_reason != "campaign_selected_pre_holdout"
            || result.selected_round_id.as_deref() != Some(selected.round_id.as_str())
            || result.selected_candidate_id != selected.selected_candidate_id
            || result.selected_candidate_content_hash != selected.selected_candidate_content_hash
        {
            bail!("terminal Campaign selection differs from its deterministic pre-holdout winner");
        }
        CampaignAttemptOutcomeV1::SelectedPreHoldout
    } else {
        validate_negative_campaign_result(&loaded, &result, &result_sha256)?;
        CampaignAttemptOutcomeV1::NoCandidate
    };
    Ok((outcome, u64::try_from(consumed)?, result_sha256))
}

/// The hash comes from fresh authenticated terminal readback or the durable
/// settlement, never an untrusted report. Build metadata without refitting or rerunning
/// position accounting, including after a report-only publication interruption.
pub(crate) fn report_settled_campaign_cache(
    request: &CampaignRequest,
    request_sha256: &str,
    settled_result_sha256: &str,
    cache: &Path,
    output: &Path,
) -> anyhow::Result<serde_json::Value> {
    validate_terminal_cache(cache)?;
    let result_path = cache.join("campaign-result.json");
    verify_cached_terminal_file(
        &result_path,
        settled_result_sha256,
        MAX_CAMPAIGN_RESULT_BYTES,
    )?;
    let result = load_campaign_result(&result_path)?;
    let loaded = LoadedRequest {
        request: request.clone(),
        sha256: request_sha256.into(),
    };
    validate_campaign_result_identity(&loaded, &result, settled_result_sha256)?;
    if result.rounds.len() != request.rounds.len() || result.finalization.is_some() {
        bail!("Campaign report requires the complete settled pre-holdout rounds");
    }
    let mut rounds = Vec::with_capacity(result.rounds.len());
    for (index, (planned, round)) in request.rounds.iter().zip(&result.rounds).enumerate() {
        if planned.round_id != round.round_id
            || planned.seed != round.seed
            || planned.identity != round.identity
        {
            bail!("Campaign report round identity differs from the settled request");
        }
        let bundle = cache
            .join("round-readback")
            .join(format!("round-{index}-results.zip"));
        verify_cached_terminal_file(
            &bundle,
            &round.result_bundle_sha256,
            MAX_RESULT_BUNDLE_BYTES,
        )?;
        rounds.push(crate::mission_metrics::campaign::collect_verified_archive(
            &bundle,
            &round.round_id,
            round.seed,
            &round.mission_id,
            &round.result_bundle_sha256,
        )?);
    }
    crate::mission_metrics::campaign::CampaignEvidenceReport::new(
        request.campaign_id.clone(),
        request_sha256.into(),
        settled_result_sha256.into(),
        request.build_source_revision.clone(),
        result.termination_reason,
        result.consumed_trials,
        rounds,
    )?
    .persist(output)
}

fn terminal_readback_error(error: anyhow::Error) -> anyhow::Error {
    if let Some(network) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
    {
        anyhow::anyhow!(
            "terminal artifact network readback failed (HTTP {:?})",
            network.status()
        )
    } else {
        error.context("terminal artifact readback")
    }
}

fn validate_campaign_result_identity(
    loaded: &LoadedRequest,
    result: &CampaignResultV1,
    result_sha256: &str,
) -> anyhow::Result<()> {
    if !matches!(
        result.schema_version.as_str(),
        CAMPAIGN_RESULT_SCHEMA_V8 | CAMPAIGN_RESULT_SCHEMA_V9
    ) || result.campaign_id != loaded.request.campaign_id
        || result.request_sha256 != loaded.sha256
        || result.build_source_revision != loaded.request.build_source_revision
        || result.image_identity != loaded.request.image_identity
        || result.campaign_inputs_sha256 != loaded.request.campaign_inputs_sha256
        || result.producer_source_revision != loaded.request.producer_source_revision
        || result.producer_image_identity != loaded.request.producer_image_identity
        || result.research_plan_sha256 != loaded.request.research_plan.content_hash()?
        || result.holdout_id != loaded.request.holdout_id
        || result.declared_total_trials != loaded.request.declared_total_trials
        || result.stop_rule != STOP_RULE_V2
    {
        bail!("Campaign result does not match the parent request identity");
    }
    for round in &result.rounds {
        let proof = round.feedback.supervised_selected_evaluation_proof.as_ref();
        if result.schema_version == CAMPAIGN_RESULT_SCHEMA_V8 {
            if proof.is_some() {
                bail!("historical result cannot acquire new selected evaluation proof");
            }
        } else {
            match (&round.feedback.supervised_selected, proof) {
                (Some(summary), Some(proof)) => {
                    proof.screening_facts(summary)?;
                }
                (None, None) => {}
                _ => bail!("current result selected evaluation proof is missing or unbound"),
            }
        }
    }
    let expected_directive_sha256 = loaded
        .request
        .research_plan
        .learning_directive
        .as_ref()
        .map(CexCampaignLearningDirectiveV1::content_hash)
        .transpose()?;
    if result.learning_directive != loaded.request.research_plan.learning_directive
        || result.learning_directive_sha256 != expected_directive_sha256
        || result.search_policy_revision.as_ref()
            != Some(&loaded.request.research_plan.search_policy_revision)
    {
        bail!("Campaign result learning lineage does not match the parent request");
    }
    normalized_sha256("parent Campaign result", result_sha256)?;
    Ok(())
}

fn validate_negative_campaign_result(
    loaded: &LoadedRequest,
    result: &CampaignResultV1,
    result_sha256: &str,
) -> anyhow::Result<()> {
    validate_campaign_result_identity(loaded, result, result_sha256)?;
    if result.termination_reason != "campaign_no_candidate"
        || result.selected_round_id.is_some()
        || result.selected_candidate_id.is_some()
        || result.selected_candidate_content_hash.is_some()
        || result.finalization.is_some()
        || result.rounds.len() != loaded.request.rounds.len()
    {
        bail!("Campaign learning requires one terminal no-candidate result");
    }
    let consumed_trials = result
        .rounds
        .iter()
        .try_fold(0_usize, |total, round| {
            total.checked_add(round.consumed_trials)
        })
        .context("Campaign result consumed trial count overflowed")?;
    if consumed_trials != result.consumed_trials || consumed_trials > result.declared_total_trials {
        bail!("Campaign result consumed trial count is invalid");
    }
    for (round, expected) in result.rounds.iter().zip(&loaded.request.rounds) {
        if round.round_id != expected.round_id
            || round.seed != expected.seed
            || round.identity != expected.identity
            || round.request_sha256.as_deref() != Some(loaded.sha256.as_str())
            || round.result_bundle_sha256 != round.result_readback_bundle_sha256
            || round.selected_candidate_id.is_some()
            || round.selected_candidate_content_hash.is_some()
            || round.selected_score.is_some()
            || round.supervised_candidate_id != round.feedback.supervised_selected_candidate_id
            || round.supervised_replay_gate_passed
                != round
                    .feedback
                    .supervised_replay
                    .as_ref()
                    .map(|replay| replay.passed)
            || round.supervised_replay_receipt_id.is_some()
                != round.feedback.supervised_replay.is_some()
        {
            bail!("Campaign result round evidence does not match the parent request");
        }
        normalized_sha256("Campaign round Mission", &round.mission_sha256)?;
        normalized_sha256("Campaign round result", &round.result_bundle_sha256)?;
        validate_campaign_round_feedback(
            &round.feedback,
            loaded.request.research_plan.max_candidates()?,
            loaded.request.research_plan.supervised_model_scope,
        )?;
    }
    Ok(())
}

fn validate_campaign_round_feedback(
    feedback: &CampaignRoundFeedbackV1,
    max_factors: usize,
    scope: alpha_domain::CexSupervisedModelScopeV1,
) -> anyhow::Result<()> {
    let allowed_fields = allowed_research_feature_fields();
    if feedback.factor_attempts != feedback.factors.len()
        || feedback.factor_attempts > max_factors
        || feedback.accepted_factors
            != feedback
                .factors
                .iter()
                .filter(|factor| factor.rejection_codes.is_empty())
                .count()
        || feedback.factors.iter().any(|factor| {
            normalized_sha256("Campaign factor signature", &factor.factor_signature_sha256).is_err()
                || factor.source_features.is_empty()
                || factor
                    .source_features
                    .iter()
                    .any(|field| !allowed_fields.contains(field))
                || (factor.rejection_codes.is_empty() && factor.evaluation.is_none())
                || factor
                    .evaluation
                    .as_ref()
                    .is_some_and(|evaluation| !valid_campaign_evaluation_feedback(evaluation))
        })
    {
        bail!("Campaign result factor feedback is invalid");
    }
    let baselines_match = match (&feedback.ridge, &feedback.cart) {
        (None, None) => {
            feedback.accepted_factors == 0
                && !feedback.baseline_gate_passed
                && feedback.baseline_failure_codes == [CexBaselineFailureCodeV1::EmptyFactorBank]
        }
        (Some(ridge), None) if !scope.is_default() => {
            feedback.accepted_factors > 0
                && valid_campaign_evaluation_feedback(ridge)
                && feedback.baseline_gate_passed == ridge.passed
                && if ridge.passed {
                    feedback.baseline_failure_codes.is_empty()
                } else {
                    feedback.baseline_failure_codes
                        == [CexBaselineFailureCodeV1::InsufficientEvidence]
                }
        }
        (Some(ridge), Some(cart)) if scope.is_default() => {
            feedback.accepted_factors > 0
                && valid_campaign_evaluation_feedback(ridge)
                && valid_campaign_evaluation_feedback(cart)
                && feedback.baseline_gate_passed == (ridge.passed && cart.passed)
                && feedback.baseline_gate_passed == feedback.baseline_failure_codes.is_empty()
                && (feedback.baseline_gate_passed
                    || feedback.baseline_failure_codes
                        == [CexBaselineFailureCodeV1::InsufficientEvidence])
        }
        _ => false,
    };
    if !baselines_match {
        bail!("Campaign result baseline feedback is invalid");
    }
    let supervised_match = match (
        &feedback.supervised_ridge,
        &feedback.supervised_cart,
        &feedback.supervised_burn,
        &feedback.supervised_selected,
        &feedback.supervised_selected_candidate_id,
    ) {
        (None, None, None, None, None) => {
            feedback.supervised_replay.is_none()
                && feedback.burn.is_none()
                && (scope.is_default() || feedback.accepted_factors == 0)
        }
        (Some(ridge), None, None, Some(selected), Some(candidate_id)) if !scope.is_default() => {
            feedback.accepted_factors > 0
                && !candidate_id.trim().is_empty()
                && valid_campaign_evaluation_feedback(ridge)
                && ridge == selected
                && feedback.burn.is_none()
                && feedback.model_attempts == Some(1)
                && feedback
                    .supervised_replay
                    .as_ref()
                    .is_none_or(valid_campaign_replay_feedback)
        }
        (Some(ridge), Some(cart), Some(burn), Some(selected), Some(candidate_id))
            if scope.is_default() =>
        {
            feedback.accepted_factors > 0
                && !candidate_id.trim().is_empty()
                && valid_campaign_evaluation_feedback(ridge)
                && valid_campaign_evaluation_feedback(cart)
                && valid_campaign_evaluation_feedback(burn)
                && valid_campaign_evaluation_feedback(selected)
                && [ridge, cart, burn].contains(&selected)
                && feedback
                    .burn
                    .as_ref()
                    .is_some_and(valid_campaign_evaluation_feedback)
                && feedback
                    .supervised_replay
                    .as_ref()
                    .is_none_or(valid_campaign_replay_feedback)
        }
        _ => false,
    };
    if !supervised_match {
        bail!("Campaign result supervised ML feedback is invalid");
    }
    if feedback
        .model_attempts
        .is_some_and(|attempts| attempts == 0)
    {
        bail!("Campaign result model attempt count is zero");
    }
    Ok(())
}

fn valid_campaign_evaluation_feedback(feedback: &CampaignEvaluationFeedbackV1) -> bool {
    let capacity_valid = match (
        feedback.max_book_depth_fraction,
        feedback.max_book_depth_fraction_limit,
    ) {
        (None, None) => !feedback.capacity_breached,
        (Some(observed), Some(limit)) => {
            observed.is_finite()
                && observed >= 0.0
                && limit.is_finite()
                && limit > 0.0
                && limit <= 1.0
                && feedback.capacity_breached == (observed > limit)
        }
        _ => false,
    };
    feedback.score.is_finite()
        && feedback
            .time_series_ic
            .is_none_or(|value| value.is_finite() && (-1.0..=1.0).contains(&value))
        && feedback
            .time_series_rank_ic
            .is_none_or(|value| value.is_finite() && (-1.0..=1.0).contains(&value))
        && feedback.cumulative_net_return.is_finite()
        && valid_feedback_drawdown(feedback.max_drawdown, feedback.passed)
        && feedback.net_sharpe.is_finite()
        && feedback.total_turnover.is_finite()
        && feedback.total_turnover >= 0.0
        && capacity_valid
}

fn valid_campaign_replay_feedback(feedback: &CampaignReplayFeedbackV1) -> bool {
    feedback.passed == feedback.failures.is_empty()
        && feedback
            .failures
            .iter()
            .all(|failure| !failure.trim().is_empty())
        && feedback.total_turnover.is_finite()
        && feedback.total_turnover >= 0.0
        && feedback.mean_net_return.is_finite()
        && feedback.cumulative_net_return.is_finite()
        && valid_feedback_drawdown(feedback.max_drawdown, feedback.passed)
        && feedback.net_sharpe.is_finite()
}

fn valid_feedback_drawdown(drawdown: f64, passed: bool) -> bool {
    // Fixed-notional additive losses can exceed the initial unit. Preserve
    // those failed outcomes for accounting without admitting a passing result
    // outside the existing bound or changing any evaluation risk threshold.
    drawdown.is_finite() && drawdown >= 0.0 && (!passed || drawdown <= 1.0)
}

pub(crate) fn serialize_request(request: &CampaignRequest) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec_pretty(request).map_err(anyhow::Error::new)
}

#[cfg(test)]
pub(crate) fn valid_request_for_tests() -> CampaignRequest {
    test_support::valid_request()
}

#[cfg(test)]
pub(crate) fn request_for_materialization_for_tests(path: &Path) -> CampaignRequest {
    let base = valid_request_for_tests();
    let materialization =
        crate::mission_runner::decode_materialization(&std::fs::read(path).unwrap()).unwrap();
    build_request_from_parts(
        &base.feature_url,
        &materialization.artifact_sha256,
        &base.materialization_url,
        &hft_research_artifacts::sha256_file(path).unwrap(),
        &base.replay_artifact_url,
        &base.replay_artifact_sha256,
        &base.replay_manifest_url,
        &base.replay_manifest_sha256,
        &base.campaign_inputs_sha256,
        &base.producer_source_revision,
        &base.producer_image_identity,
        base.prepared_inputs.as_ref(),
        &base.research_plan,
        BUILD_SOURCE_REVISION,
        &base.image_identity,
        &campaign_output_root(
            &canonical_tokyo_oss_internal_object("test result", &base.campaign_result_readback_url)
                .unwrap(),
        )
        .unwrap(),
        &base.holdout_id,
        &[7, 11],
        None,
    )
    .unwrap()
}

pub(crate) fn validate_request(request: &CampaignRequest) -> anyhow::Result<()> {
    if !matches!(
        request.schema_version.as_str(),
        CAMPAIGN_REQUEST_SCHEMA_V5 | CAMPAIGN_REQUEST_SCHEMA_V6
    ) || (request.schema_version == CAMPAIGN_REQUEST_SCHEMA_V6)
        != request.prepared_inputs.is_some()
    {
        bail!("campaign request schema and prepared input kind disagree");
    }
    request.research_plan.validate()?;
    representation::validate_campaign_request(request)?;
    if request.research_plan.calendar.is_some() {
        if request
            .rounds
            .iter()
            .map(|round| round.seed)
            .collect::<Vec<_>>()
            != [7, 11]
        {
            bail!("calendar H1 requires exactly seeds 7 and 11");
        }
        let precheck = request
            .research_plan
            .development_precheck
            .as_ref()
            .context("calendar request requires its published development precheck")?;
        if precheck.feature_sha256 != request.feature_sha256
            || precheck.materialization_sha256 != request.materialization_sha256
            || precheck.source_revision != request.build_source_revision
        {
            bail!("calendar precheck differs from the requested input and source identities");
        }
    }
    if let Some(plan) = &request.research_plan.mlp_training {
        plan.validate_requested_seeds(
            &request
                .rounds
                .iter()
                .map(|round| round.seed)
                .collect::<Vec<_>>(),
        )
        .map_err(anyhow::Error::msg)?;
    }
    validate_study_proposal_for_plan(request.study_proposal.as_ref(), &request.research_plan)?;
    if let Some(proposal) = &request.study_proposal {
        if proposal.target_execution.campaign_inputs_sha256 != request.campaign_inputs_sha256 {
            bail!("next-family proposal input identity differs from the Campaign request");
        }
    }
    validate_campaign_id(&request.campaign_id)?;
    if request.image_identity
        != normalized_sha256("campaign image identity", &request.image_identity)?
    {
        bail!("campaign image identity must be a normalized SHA256");
    }
    normalized_source_revision("campaign source revision", &request.build_source_revision)?;
    if request.campaign_inputs_sha256
        != normalized_sha256(
            "campaign inputs receipt SHA256",
            &request.campaign_inputs_sha256,
        )?
    {
        bail!("campaign inputs receipt SHA256 must be normalized");
    }
    if request
        .research_plan
        .parent_evidence_signature
        .as_ref()
        .is_some_and(|parent| parent.campaign_inputs_sha256 != request.campaign_inputs_sha256)
        && request.study_proposal.is_none()
    {
        bail!("CEX Campaign follow-up parent evidence does not match Campaign inputs");
    }
    normalized_source_revision(
        "campaign producer_source_revision",
        &request.producer_source_revision,
    )?;
    if request.producer_image_identity
        != normalized_sha256(
            "campaign producer image identity",
            &request.producer_image_identity,
        )?
    {
        bail!("campaign producer image identity must be a normalized SHA256");
    }
    validate_cex_holdout_id(&request.holdout_id)?;
    legacy_input_object(request, "campaign feature", &request.feature_url)?;
    normalized_sha256("campaign feature", &request.feature_sha256)?;
    legacy_input_object(
        request,
        "campaign materialization",
        &request.materialization_url,
    )?;
    normalized_sha256("campaign materialization", &request.materialization_sha256)?;
    legacy_input_object(
        request,
        "campaign replay artifact",
        &request.replay_artifact_url,
    )?;
    normalized_sha256("campaign replay artifact", &request.replay_artifact_sha256)?;
    legacy_input_object(
        request,
        "campaign replay manifest",
        &request.replay_manifest_url,
    )?;
    normalized_sha256("campaign replay manifest", &request.replay_manifest_sha256)?;

    if let Some(prepared) = &request.prepared_inputs {
        normalized_sha256("native prepared collection", &prepared.collection_sha256)?;
        let collection_object = canonical_tokyo_oss_internal_object(
            "native prepared collection",
            &prepared.collection_url,
        )?;
        let suffix = format!("/native-prepared/{}.json", prepared.collection_sha256);
        let root = collection_object
            .strip_suffix(&suffix)
            .context("native collection object is not content addressed")?;
        if prepared.block_urls.is_empty() || prepared.block_urls.len() > 50_000 {
            bail!("native prepared block transport count exceeds its bound");
        }
        for (sha, url) in &prepared.block_urls {
            normalized_sha256("native prepared block", sha)?;
            if canonical_tokyo_oss_internal_object("native prepared block", url)?
                != format!("{root}/native-prepared/{sha}.mondaybin")
            {
                bail!("native prepared transport does not bind its exact content address and source root");
            }
        }
        let source = &prepared.expected_native.source;
        if source.feature_sha256 != request.feature_sha256
            || source.materialization_sha256 != request.materialization_sha256
            || source.replay_artifact_sha256 != request.replay_artifact_sha256
            || source.replay_manifest_sha256 != request.replay_manifest_sha256
            || source.build.source_revision != request.producer_source_revision
            || mission_dispatch::image_digest(&source.build.image_identity)?
                != request.producer_image_identity
        {
            bail!("native prepared expectation differs from original source Build/input identity");
        }
    }
    let claim_object = canonical_tokyo_oss_internal_object(
        "campaign holdout claim",
        &request.holdout_claim_put_url,
    )?;
    let claim_readback_object = canonical_tokyo_oss_internal_object(
        "campaign holdout claim readback",
        &request.holdout_claim_readback_url,
    )?;
    if claim_object != claim_readback_object {
        bail!("campaign holdout claim readback URL must identify the same immutable object");
    }
    let campaign_result_object =
        canonical_tokyo_oss_internal_object("campaign result", &request.campaign_result_put_url)?;
    let campaign_result_readback_object = canonical_tokyo_oss_internal_object(
        "campaign result readback",
        &request.campaign_result_readback_url,
    )?;
    if campaign_result_object != campaign_result_readback_object {
        bail!("campaign result readback URL must identify the same immutable object");
    }
    let campaign_root = campaign_output_root(&campaign_result_object)?;
    let expected_campaign_result_object = format!(
        "{campaign_root}/campaign-id={}/campaign-result.json",
        request.campaign_id
    );
    if campaign_result_object != expected_campaign_result_object {
        bail!("campaign result object must bind the exact Campaign ID");
    }
    let expected_claim_object = cex_global_holdout_claim_object(&request.holdout_id)?;
    if claim_object != expected_claim_object {
        bail!("campaign holdout claim object must use the global sealed holdout namespace");
    }

    if request.rounds.len() < 2 {
        bail!("campaign request must declare at least two rounds");
    }
    let minimum_total_trials =
        declared_total_trials_for_rounds(&request.research_plan, request.rounds.len())?;
    request
        .research_plan
        .effective_multiple_testing_trials(request.declared_total_trials)?;
    if request.declared_total_trials < minimum_total_trials {
        bail!("campaign declared_total_trials is below the minimum multi-round trial family");
    }
    let mut round_ids = std::collections::BTreeSet::new();
    let mut seeds = std::collections::BTreeSet::new();
    let expected_data_fingerprint_sha256 = campaign_data_fingerprint_sha256(
        &request.campaign_inputs_sha256,
        &request.producer_source_revision,
        &request.feature_sha256,
        &request.materialization_sha256,
        &request.replay_artifact_sha256,
        &request.replay_manifest_sha256,
    )?;
    for round in &request.rounds {
        validate_dns_label("campaign round id", &round.round_id)?;
        if !round_ids.insert(round.round_id.as_str()) || !seeds.insert(round.seed) {
            bail!("campaign rounds must have unique ids and seeds");
        }
        if round.identity.schema_version != CAMPAIGN_ROUND_IDENTITY_SCHEMA_V1
            || round.identity.data_window_hours != CAMPAIGN_DATA_WINDOW_HOURS
            || round.identity.data_fingerprint_sha256 != expected_data_fingerprint_sha256
            || round.identity.image_identity != request.image_identity
            || round.identity.build_source_revision != request.build_source_revision
        {
            bail!("campaign round identity does not bind the 31h data, image, and source");
        }
        let mission_object =
            canonical_tokyo_oss_internal_object("campaign mission", &round.mission_put_url)?;
        let mission_readback_object = canonical_tokyo_oss_internal_object(
            "campaign mission readback",
            &round.mission_readback_url,
        )?;
        if mission_object != mission_readback_object {
            bail!("campaign Mission readback URL must identify the same immutable object");
        }
        let expected_mission_object = format!(
            "{campaign_root}/campaign-id={}/round={}/mission.json",
            request.campaign_id, round.round_id,
        );
        if mission_object != expected_mission_object {
            bail!("campaign Mission object must live at campaign-id=<id>/round=<id>/mission.json");
        }
        let result_object =
            canonical_tokyo_oss_internal_object("campaign result", &round.result_put_url)?;
        let result_readback_object = canonical_tokyo_oss_internal_object(
            "campaign result readback",
            &round.result_readback_url,
        )?;
        if result_object != result_readback_object {
            bail!("campaign result readback URL must identify the same immutable object");
        }
        let root = cex_campaign_round_root(
            &result_object,
            &request.campaign_id,
            &round.round_id,
            "results.zip",
        )?;
        if root != campaign_root {
            bail!("campaign result must share one Campaign root");
        }
    }
    let expected_claim = cex_global_holdout_claim_object(&request.holdout_id)?;
    if expected_claim != claim_object {
        bail!("campaign result and holdout claim must bind the same global holdout fence");
    }
    let expected_id = expected_campaign_id(request)?;
    if request.campaign_id != expected_id {
        bail!("campaign request campaign_id does not match its semantic identity");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_request_from_parts(
    feature_url: &str,
    feature_sha256: &str,
    materialization_url: &str,
    materialization_sha256: &str,
    replay_artifact_url: &str,
    replay_artifact_sha256: &str,
    replay_manifest_url: &str,
    replay_manifest_sha256: &str,
    campaign_inputs_sha256: &str,
    producer_source_revision: &str,
    producer_image_identity: &str,
    prepared_inputs: Option<&prepared_inputs::NativePreparedCampaignRefV1>,
    research_plan: &CexCampaignResearchPlanV1,
    build_source_revision: &str,
    image_identity: &str,
    campaign_root: &str,
    holdout_id: &str,
    seeds: &[u64],
    study_proposal: Option<&CampaignNextFamilyProposalV1>,
) -> anyhow::Result<CampaignRequest> {
    research_plan.validate()?;
    if let Some(plan) = &research_plan.mlp_training {
        plan.validate_requested_seeds(seeds)
            .map_err(anyhow::Error::msg)?;
    }
    let data_fingerprint_sha256 = campaign_data_fingerprint_sha256(
        campaign_inputs_sha256,
        producer_source_revision,
        feature_sha256,
        materialization_sha256,
        replay_artifact_sha256,
        replay_manifest_sha256,
    )?;
    let mut request = CampaignRequest {
        prepared_inputs: prepared_inputs.cloned(),
        schema_version: if prepared_inputs.is_some() {
            CAMPAIGN_REQUEST_SCHEMA_V6
        } else {
            CAMPAIGN_REQUEST_SCHEMA_V5
        }
        .to_string(),
        campaign_id: "placeholder".to_string(),
        build_source_revision: build_source_revision.to_string(),
        image_identity: image_identity.to_string(),
        campaign_inputs_sha256: campaign_inputs_sha256.to_string(),
        producer_source_revision: producer_source_revision.to_string(),
        producer_image_identity: producer_image_identity.to_string(),
        research_plan: research_plan.clone(),
        study_proposal: study_proposal.cloned(),
        feature_url: if prepared_inputs.is_some() {
            String::new()
        } else {
            feature_url.to_string()
        },
        feature_sha256: feature_sha256.to_string(),
        materialization_url: if prepared_inputs.is_some() {
            String::new()
        } else {
            materialization_url.to_string()
        },
        materialization_sha256: materialization_sha256.to_string(),
        replay_artifact_url: if prepared_inputs.is_some() {
            String::new()
        } else {
            replay_artifact_url.to_string()
        },
        replay_artifact_sha256: replay_artifact_sha256.to_string(),
        replay_manifest_url: if prepared_inputs.is_some() {
            String::new()
        } else {
            replay_manifest_url.to_string()
        },
        replay_manifest_sha256: replay_manifest_sha256.to_string(),
        holdout_id: holdout_id.to_string(),
        declared_total_trials: declared_total_trials_for_rounds(research_plan, seeds.len())?,
        rounds: seeds
            .iter()
            .enumerate()
            .map(|(index, seed)| CampaignRoundRequest {
                round_id: format!("r{}", index + 1),
                seed: *seed,
                identity: CampaignRoundIdentityV1 {
                    schema_version: CAMPAIGN_ROUND_IDENTITY_SCHEMA_V1.to_string(),
                    data_window_hours: CAMPAIGN_DATA_WINDOW_HOURS,
                    data_fingerprint_sha256: data_fingerprint_sha256.clone(),
                    image_identity: image_identity.to_string(),
                    build_source_revision: build_source_revision.to_string(),
                },
                mission_put_url: format!(
                    "{campaign_root}/campaign-id=placeholder/round=r{}/mission.json",
                    index + 1
                ),
                mission_readback_url: format!(
                    "{campaign_root}/campaign-id=placeholder/round=r{}/mission.json",
                    index + 1
                ),
                result_put_url: format!(
                    "{campaign_root}/campaign-id=placeholder/round=r{}/results.zip",
                    index + 1
                ),
                result_readback_url: format!(
                    "{campaign_root}/campaign-id=placeholder/round=r{}/results.zip",
                    index + 1
                ),
            })
            .collect(),
        holdout_claim_put_url: cex_global_holdout_claim_object(holdout_id)?,
        holdout_claim_readback_url: cex_global_holdout_claim_object(holdout_id)?,
        campaign_result_put_url: format!(
            "{campaign_root}/campaign-id=placeholder/campaign-result.json"
        ),
        campaign_result_readback_url: format!(
            "{campaign_root}/campaign-id=placeholder/campaign-result.json"
        ),
    };
    request.campaign_id = expected_campaign_id(&request)?;
    for round in &mut request.rounds {
        round.mission_put_url = format!(
            "{campaign_root}/campaign-id={}/round={}/mission.json",
            request.campaign_id, round.round_id
        );
        round.mission_readback_url = round.mission_put_url.clone();
        round.result_put_url = format!(
            "{campaign_root}/campaign-id={}/round={}/results.zip",
            request.campaign_id, round.round_id
        );
        round.result_readback_url = round.result_put_url.clone();
    }
    request.campaign_result_put_url = format!(
        "{campaign_root}/campaign-id={}/campaign-result.json",
        request.campaign_id
    );
    request.campaign_result_readback_url = request.campaign_result_put_url.clone();
    validate_request(&request)?;
    Ok(request)
}

pub(crate) fn campaign_data_fingerprint_sha256(
    campaign_inputs_sha256: &str,
    producer_source_revision: &str,
    feature_sha256: &str,
    materialization_sha256: &str,
    replay_artifact_sha256: &str,
    replay_manifest_sha256: &str,
) -> anyhow::Result<String> {
    canonical_json_hash(&serde_json::json!({
        "schema_version": CAMPAIGN_DATA_FINGERPRINT_SCHEMA_V1,
        "window_hours": CAMPAIGN_DATA_WINDOW_HOURS,
        "campaign_inputs_sha256": normalized_sha256(
            "campaign inputs receipt SHA256",
            campaign_inputs_sha256,
        )?,
        "producer_source_revision": normalized_source_revision(
            "campaign producer_source_revision",
            producer_source_revision,
        )?,
        "feature_sha256": normalized_sha256("campaign feature", feature_sha256)?,
        "materialization_sha256": normalized_sha256(
            "campaign materialization",
            materialization_sha256,
        )?,
        "replay_artifact_sha256": normalized_sha256(
            "campaign replay artifact",
            replay_artifact_sha256,
        )?,
        "replay_manifest_sha256": normalized_sha256(
            "campaign replay manifest",
            replay_manifest_sha256,
        )?,
    }))
    .map_err(anyhow::Error::new)
}

fn canonicalize_request_transport(request: &CampaignRequest) -> anyhow::Result<CampaignRequest> {
    let mut canonical = request.clone();
    canonical.build_source_revision =
        normalized_source_revision("campaign source revision", &canonical.build_source_revision)?;
    canonical.image_identity =
        normalized_sha256("campaign image identity", &canonical.image_identity)?;
    canonical.campaign_inputs_sha256 = normalized_sha256(
        "campaign inputs receipt SHA256",
        &canonical.campaign_inputs_sha256,
    )?;
    canonical.producer_source_revision = normalized_source_revision(
        "campaign producer_source_revision",
        &canonical.producer_source_revision,
    )?;
    canonical.producer_image_identity = normalized_sha256(
        "campaign producer image identity",
        &canonical.producer_image_identity,
    )?;
    if let Some(prepared) = &mut canonical.prepared_inputs {
        prepared.collection_url = canonical_tokyo_oss_internal_object(
            "native prepared collection",
            &prepared.collection_url,
        )?;
        for url in prepared.block_urls.values_mut() {
            *url = canonical_tokyo_oss_internal_object("native prepared block", url)?;
        }
    } else {
        canonical.feature_url =
            canonical_tokyo_oss_internal_object("campaign feature", &canonical.feature_url)?;
        canonical.materialization_url = canonical_tokyo_oss_internal_object(
            "campaign materialization",
            &canonical.materialization_url,
        )?;
        canonical.replay_artifact_url = canonical_tokyo_oss_internal_object(
            "campaign replay artifact",
            &canonical.replay_artifact_url,
        )?;
        canonical.replay_manifest_url = canonical_tokyo_oss_internal_object(
            "campaign replay manifest",
            &canonical.replay_manifest_url,
        )?;
    }
    canonical.holdout_claim_put_url = canonical_tokyo_oss_internal_object(
        "campaign holdout claim",
        &canonical.holdout_claim_put_url,
    )?;
    canonical.holdout_claim_readback_url = canonical_tokyo_oss_internal_object(
        "campaign holdout claim readback",
        &canonical.holdout_claim_readback_url,
    )?;
    canonical.campaign_result_put_url =
        canonical_tokyo_oss_internal_object("campaign result", &canonical.campaign_result_put_url)?;
    canonical.campaign_result_readback_url = canonical_tokyo_oss_internal_object(
        "campaign result readback",
        &canonical.campaign_result_readback_url,
    )?;
    for round in &mut canonical.rounds {
        round.mission_put_url =
            canonical_tokyo_oss_internal_object("campaign mission", &round.mission_put_url)?;
        round.mission_readback_url = canonical_tokyo_oss_internal_object(
            "campaign mission readback",
            &round.mission_readback_url,
        )?;
        round.result_put_url =
            canonical_tokyo_oss_internal_object("campaign result", &round.result_put_url)?;
        round.result_readback_url = canonical_tokyo_oss_internal_object(
            "campaign result readback",
            &round.result_readback_url,
        )?;
    }
    Ok(canonical)
}

fn signing_plan(request: &CampaignRequest) -> anyhow::Result<CampaignSigningPlan> {
    let holdout_claim_object = canonical_tokyo_oss_internal_object(
        "campaign holdout claim",
        &request.holdout_claim_put_url,
    )?;
    let campaign_result_object =
        canonical_tokyo_oss_internal_object("campaign result", &request.campaign_result_put_url)?;
    let _campaign_root = campaign_output_root(&campaign_result_object)?;
    let mut actions = Vec::new();
    if let Some(prepared) = &request.prepared_inputs {
        actions.push(signing_action_get(
            "prepared_collection_get",
            canonical_tokyo_oss_internal_object(
                "native prepared collection",
                &prepared.collection_url,
            )?,
        ));
        for (sha, url) in &prepared.block_urls {
            actions.push(signing_action_get(
                &format!("prepared_block_{sha}_get"),
                canonical_tokyo_oss_internal_object("native prepared block", url)?,
            ));
        }
    } else {
        for (name, label, url) in [
            ("feature_get", "campaign feature", &request.feature_url),
            (
                "materialization_get",
                "campaign materialization",
                &request.materialization_url,
            ),
            (
                "replay_artifact_get",
                "campaign replay artifact",
                &request.replay_artifact_url,
            ),
            (
                "replay_manifest_get",
                "campaign replay manifest",
                &request.replay_manifest_url,
            ),
        ] {
            actions.push(signing_action_get(
                name,
                canonical_tokyo_oss_internal_object(label, url)?,
            ));
        }
    }
    for round in &request.rounds {
        actions.push(signing_action_put_json(
            &format!("{}_mission_put", round.round_id),
            canonical_tokyo_oss_internal_object("campaign mission", &round.mission_put_url)?,
        ));
        actions.push(signing_action_get(
            &format!("{}_mission_readback_get", round.round_id),
            canonical_tokyo_oss_internal_object(
                "campaign mission readback",
                &round.mission_readback_url,
            )?,
        ));
        actions.push(signing_action_put_zip(
            &format!("{}_result_put", round.round_id),
            canonical_tokyo_oss_internal_object("campaign result", &round.result_put_url)?,
        ));
        actions.push(signing_action_get(
            &format!("{}_result_readback_get", round.round_id),
            canonical_tokyo_oss_internal_object(
                "campaign result readback",
                &round.result_readback_url,
            )?,
        ));
    }
    actions.push(signing_action_put_json(
        "holdout_claim_put",
        holdout_claim_object.clone(),
    ));
    actions.push(signing_action_get(
        "holdout_claim_readback_get",
        canonical_tokyo_oss_internal_object(
            "campaign holdout claim readback",
            &request.holdout_claim_readback_url,
        )?,
    ));
    actions.push(signing_action_put_json(
        "campaign_result_put",
        campaign_result_object.clone(),
    ));
    actions.push(signing_action_get(
        "campaign_result_readback_get",
        canonical_tokyo_oss_internal_object(
            "campaign result readback",
            &request.campaign_result_readback_url,
        )?,
    ));
    Ok(CampaignSigningPlan { actions })
}

fn validate_request_matches_freeze(
    signed_request: &CampaignRequest,
    plan: &FrozenCampaignPlan,
) -> anyhow::Result<()> {
    validate_request(signed_request)?;
    let canonical_signed = canonicalize_request_transport(signed_request)?;
    if canonical_signed != plan.canonical_request {
        bail!("signed campaign request drifted from the frozen canonical identity");
    }
    if signing_plan(&canonical_signed)? != plan.signing_plan {
        bail!("signed campaign request signing plan drifted from the frozen execution plan");
    }
    if expected_campaign_id(signed_request)? != plan.canonical_request.campaign_id {
        bail!("signed campaign request campaign_id drifted from the frozen identity");
    }
    Ok(())
}

fn signing_action_get(name: &str, object: String) -> CampaignSigningAction {
    CampaignSigningAction {
        name: name.to_string(),
        object,
        method: "GET".to_string(),
        content_type: None,
        required_headers: std::collections::BTreeMap::new(),
    }
}

fn signing_action_put_json(name: &str, object: String) -> CampaignSigningAction {
    signing_action_put(name, object, "application/json")
}

fn signing_action_put_zip(name: &str, object: String) -> CampaignSigningAction {
    signing_action_put(name, object, "application/zip")
}

fn signing_action_put(name: &str, object: String, content_type: &str) -> CampaignSigningAction {
    CampaignSigningAction {
        name: name.to_string(),
        object,
        method: "PUT".to_string(),
        content_type: Some(content_type.to_string()),
        required_headers: std::collections::BTreeMap::from([(
            "x-oss-forbid-overwrite".to_string(),
            "true".to_string(),
        )]),
    }
}

pub(crate) fn expected_campaign_id(request: &CampaignRequest) -> anyhow::Result<String> {
    let mut identity = serde_json::json!({
        "identity_schema_version": CAMPAIGN_IDENTITY_SCHEMA_V5,
        "request_schema_version": request.schema_version,
        "build_source_revision": normalized_source_revision(
            "campaign source revision",
            &request.build_source_revision,
        )?,
        "image_identity": normalized_sha256("campaign image identity", &request.image_identity)?,
        "campaign_inputs_sha256": normalized_sha256(
            "campaign inputs receipt SHA256",
            &request.campaign_inputs_sha256,
        )?,
        "producer_source_revision": normalized_source_revision(
            "campaign producer_source_revision",
            &request.producer_source_revision,
        )?,
        "producer_image_identity": normalized_sha256(
            "campaign producer image identity",
            &request.producer_image_identity,
        )?,
        "research_plan": &request.research_plan,
        "prepared_inputs": request.prepared_inputs.as_ref().map(|p| serde_json::json!({"collection":p.collection_sha256,"expected_native":p.expected_native})),
        "feature": {
            "object": legacy_input_object(request,"campaign feature", &request.feature_url)?,
            "sha256": normalized_sha256("campaign feature", &request.feature_sha256)?,
        },
        "materialization": {
            "object": legacy_input_object(request,"campaign materialization", &request.materialization_url)?,
            "sha256": normalized_sha256("campaign materialization", &request.materialization_sha256)?,
        },
        "replay_artifact": {
            "object": legacy_input_object(request,"campaign replay artifact", &request.replay_artifact_url)?,
            "sha256": normalized_sha256("campaign replay artifact", &request.replay_artifact_sha256)?,
        },
        "replay_manifest": {
            "object": legacy_input_object(request,"campaign replay manifest", &request.replay_manifest_url)?,
            "sha256": normalized_sha256("campaign replay manifest", &request.replay_manifest_sha256)?,
        },
        "holdout_id": request.holdout_id,
        "declared_total_trials": request.declared_total_trials,
        "output_root": campaign_output_root(&canonical_tokyo_oss_internal_object(
            "campaign result",
            &request.campaign_result_put_url,
        )?)?,
        "rounds": request
            .rounds
            .iter()
            .map(|round| serde_json::json!({
                "round_id": round.round_id,
                "seed": round.seed,
                "identity": round.identity,
            }))
            .collect::<Vec<_>>(),
        "stop_rule": STOP_RULE_V2,
    });
    if request.prepared_inputs.is_none() {
        identity
            .as_object_mut()
            .context("Campaign identity object")?
            .remove("prepared_inputs");
    }
    Ok(format!(
        "cex-campaign-{}",
        &canonical_json_hash(&identity)?[..32]
    ))
}

fn validate_campaign_id(value: &str) -> anyhow::Result<()> {
    validate_dns_label("campaign id", value)?;
    if !value.starts_with("cex-campaign-") {
        bail!("campaign id must start with cex-campaign-");
    }
    Ok(())
}

fn campaign_output_root(result_object: &str) -> anyhow::Result<String> {
    const SEGMENT: &str = "/campaign-id=";
    let mut matches = result_object.match_indices(SEGMENT);
    let (index, _) = matches
        .next()
        .context("campaign result object must contain one Campaign ID binding")?;
    if matches.next().is_some() {
        bail!("campaign result object contains duplicate Campaign ID bindings");
    }
    let suffix = &result_object[index + SEGMENT.len()..];
    let (_, file_name) = suffix
        .split_once('/')
        .context("campaign result object must end with campaign-id=<id>/campaign-result.json")?;
    if file_name != "campaign-result.json" {
        bail!("campaign result object must end with campaign-id=<id>/campaign-result.json");
    }
    let root = result_object[..index].trim_end_matches('/');
    if root.is_empty() {
        bail!("campaign result object requires a Campaign root");
    }
    Ok(root.to_string())
}

fn fetch_verified(
    client: &Client,
    label: &str,
    source: &str,
    destination: &Path,
    expected_sha256: &str,
    max_bytes: u64,
) -> anyhow::Result<()> {
    let (_, sha256) = fetch_to_file(client, source, destination, max_bytes)?;
    if sha256 != normalized_sha256(label, expected_sha256)? {
        bail!("{label} SHA256 mismatch");
    }
    Ok(())
}

#[cfg(feature = "scientific")]
fn publish_create_once_json(
    client: &Client,
    label: &str,
    destination: &str,
    readback_url: &str,
    source: &Path,
    readback_path: &Path,
) -> anyhow::Result<String> {
    let published_sha256 = hft_research_artifacts::sha256_file(source)?;
    let already_exists =
        match publish_immutable_file(client, destination, source, "application/json") {
            Ok(()) => false,
            Err(error) if immutable_publish_conflict(&error) => true,
            Err(error) => return Err(error).with_context(|| format!("publish {label}")),
        };
    let (_, readback_sha256) = fetch_to_file(
        client,
        readback_url,
        readback_path,
        source.metadata()?.len().max(MAX_CAMPAIGN_RESULT_BYTES),
    )?;
    if readback_sha256 != published_sha256 {
        if already_exists {
            bail!("published {label} already exists with different bytes");
        }
        bail!("published {label} readback SHA256 mismatch");
    }
    Ok(published_sha256)
}

#[cfg(feature = "scientific")]
fn immutable_publish_conflict(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .starts_with("result destination already exists:")
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .and_then(reqwest::Error::status)
                .is_some_and(|status| status == StatusCode::CONFLICT)
        })
}

#[cfg(test)]
fn validate_local_test_request(request: &CampaignRequest) -> anyhow::Result<()> {
    if !matches!(
        request.schema_version.as_str(),
        CAMPAIGN_REQUEST_SCHEMA_V5 | CAMPAIGN_REQUEST_SCHEMA_V6
    ) || (request.schema_version == CAMPAIGN_REQUEST_SCHEMA_V6)
        != request.prepared_inputs.is_some()
    {
        bail!("local test request input kind is invalid");
    }
    request.research_plan.validate()?;
    representation::validate_campaign_request(request)?;
    validate_campaign_id(&request.campaign_id)?;
    normalized_sha256("campaign image identity", &request.image_identity)?;
    normalized_sha256(
        "campaign inputs receipt SHA256",
        &request.campaign_inputs_sha256,
    )?;
    normalized_source_revision(
        "campaign producer_source_revision",
        &request.producer_source_revision,
    )?;
    normalized_sha256(
        "campaign producer image identity",
        &request.producer_image_identity,
    )?;
    if request.build_source_revision != BUILD_SOURCE_REVISION
        || !valid_git_revision(&request.build_source_revision)
    {
        bail!("campaign source revision must match the test build");
    }
    validate_cex_holdout_id(&request.holdout_id)?;
    if request.rounds.len() < 2 {
        bail!("campaign request must declare at least two rounds");
    }
    let minimum_total_trials =
        declared_total_trials_for_rounds(&request.research_plan, request.rounds.len())?;
    if request.declared_total_trials < minimum_total_trials {
        bail!("campaign declared_total_trials is below the minimum multi-round trial family");
    }
    let mut round_ids = std::collections::BTreeSet::new();
    let mut seeds = std::collections::BTreeSet::new();
    let expected_data_fingerprint_sha256 = campaign_data_fingerprint_sha256(
        &request.campaign_inputs_sha256,
        &request.producer_source_revision,
        &request.feature_sha256,
        &request.materialization_sha256,
        &request.replay_artifact_sha256,
        &request.replay_manifest_sha256,
    )?;
    for path in [
        &request.holdout_claim_put_url,
        &request.holdout_claim_readback_url,
        &request.campaign_result_put_url,
        &request.campaign_result_readback_url,
    ] {
        if path.trim().is_empty() {
            bail!("local test request paths must be non-empty");
        }
    }
    if request.prepared_inputs.is_none()
        && [
            &request.feature_url,
            &request.materialization_url,
            &request.replay_artifact_url,
            &request.replay_manifest_url,
        ]
        .iter()
        .any(|url| url.is_empty())
    {
        bail!("legacy test input paths are required");
    }
    if request.holdout_claim_put_url != request.holdout_claim_readback_url
        || request.campaign_result_put_url != request.campaign_result_readback_url
    {
        bail!("local test request readback paths must match their put paths");
    }
    for round in &request.rounds {
        validate_dns_label("campaign round id", &round.round_id)?;
        if !round_ids.insert(round.round_id.as_str()) || !seeds.insert(round.seed) {
            bail!("campaign rounds must have unique ids and seeds");
        }
        if round.identity.schema_version != CAMPAIGN_ROUND_IDENTITY_SCHEMA_V1
            || round.identity.data_window_hours != CAMPAIGN_DATA_WINDOW_HOURS
            || round.identity.data_fingerprint_sha256 != expected_data_fingerprint_sha256
            || round.identity.image_identity != request.image_identity
            || round.identity.build_source_revision != request.build_source_revision
        {
            bail!("campaign round identity does not bind the 31h data, image, and source");
        }
        if round.mission_put_url != round.mission_readback_url
            || round.result_put_url != round.result_readback_url
        {
            bail!("local test round readback paths must match their put paths");
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "scientific"))]
pub(crate) mod tests {
    use super::test_support::{paired_mlp_plan_for_tests, valid_request};
    use super::*;
    use crate::mission_render;
    use parquet::{
        data_type::{ByteArray, ByteArrayType, Int64Type},
        file::{
            properties::WriterProperties,
            writer::{SerializedFileWriter, SerializedRowGroupWriter},
        },
        schema::parser::parse_message_type,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::{fs::File, path::PathBuf, sync::Arc};

    #[test]
    fn expected_campaign_id_ignores_signed_queries_and_output_transports() {
        let mut request = valid_request();
        let original = expected_campaign_id(&request).unwrap();
        request.feature_url.push_str("?signature=feature");
        request
            .materialization_url
            .push_str("?signature=materialization");
        request.replay_artifact_url.push_str("?signature=replay");
        request.replay_manifest_url.push_str("?signature=manifest");
        request
            .campaign_result_put_url
            .push_str("?signature=ignored");
        request
            .campaign_result_readback_url
            .push_str("?signature=ignored");
        for round in &mut request.rounds {
            round.mission_put_url.push_str("?signature=ignored");
            round.mission_readback_url.push_str("?signature=ignored");
            round.result_put_url.push_str("?signature=ignored");
            round.result_readback_url.push_str("?signature=ignored");
        }
        request.holdout_claim_put_url.push_str("?signature=ignored");
        request
            .holdout_claim_readback_url
            .push_str("?signature=ignored");

        assert_eq!(expected_campaign_id(&request).unwrap(), original);
    }

    #[test]
    fn expected_campaign_id_binds_the_output_root() {
        let mut request = valid_request();
        let original = expected_campaign_id(&request).unwrap();
        request.campaign_result_put_url = request
            .campaign_result_put_url
            .replace("/research/", "/other-root/");

        assert_ne!(expected_campaign_id(&request).unwrap(), original);
    }

    #[test]
    fn expected_campaign_id_binds_producer_lineage() {
        let mut request = valid_request();
        let original = expected_campaign_id(&request).unwrap();
        request.campaign_inputs_sha256 = "9".repeat(64);
        assert_ne!(expected_campaign_id(&request).unwrap(), original);

        let mut request = valid_request();
        let original = expected_campaign_id(&request).unwrap();
        request.producer_source_revision = "c".repeat(40);
        assert_ne!(expected_campaign_id(&request).unwrap(), original);

        let mut request = valid_request();
        let original = expected_campaign_id(&request).unwrap();
        request.producer_image_identity = "8".repeat(64);
        assert_ne!(expected_campaign_id(&request).unwrap(), original);
    }

    #[test]
    fn round_identity_binds_31h_data_image_digest_and_code_sha() {
        let request = valid_request();
        assert_eq!(request.rounds[0].identity.data_window_hours, 31);
        validate_request(&request).unwrap();
        let original = expected_campaign_id(&request).unwrap();

        for mutate in [
            |round: &mut CampaignRoundIdentityV1| round.data_fingerprint_sha256 = "9".repeat(64),
            |round: &mut CampaignRoundIdentityV1| round.image_identity = "8".repeat(64),
            |round: &mut CampaignRoundIdentityV1| round.build_source_revision = "c".repeat(40),
        ] {
            let mut drifted = request.clone();
            mutate(&mut drifted.rounds[0].identity);
            assert_ne!(expected_campaign_id(&drifted).unwrap(), original);
            drifted.campaign_id = expected_campaign_id(&drifted).unwrap();
            assert!(validate_request(&drifted).is_err());
        }
    }

    #[test]
    fn fixed_holding_comparison_never_changes_the_entry_policy_on_failure() {
        let mut loaded = loaded_request_for_learning();
        loaded.request.research_plan.holding =
            Some(hft_research_manifest::model::HorizonHoldingPolicyV1 {
                horizon_millis: 5000,
            });
        for failure in [
            CexCampaignFailureClassV1::NoTradesAfterCosts,
            CexCampaignFailureClassV1::OvertradeCapacity,
            CexCampaignFailureClassV1::PositiveIcNegativeNet,
        ] {
            assert!(
                next_campaign_policy_revision(&loaded, &"c".repeat(64), failure)
                    .unwrap_err()
                    .to_string()
                    .contains("no automatic follow-up")
            );
        }
    }

    #[test]
    fn parent_bound_follow_up_plan_composition_preserves_declared_policy() {
        let loaded = loaded_request_for_learning();
        let result_sha256 = "9".repeat(64);
        let result = negative_campaign_result(&loaded);
        validate_negative_campaign_result(&loaded, &result, &result_sha256).unwrap();
        assert!(classify_campaign_failure(&result).is_err());
        // Composition is independent from admission of this historical summary.
        let failure_class = CexCampaignFailureClassV1::OvertradeCapacity;
        let (search_policy_revision, learning_directive) =
            next_campaign_policy_revision(&loaded, &result_sha256, failure_class).unwrap();

        let plan = follow_up_plan(
            &loaded,
            &result_sha256,
            learning_directive.clone(),
            search_policy_revision.clone(),
            campaign_research_evidence_signature(&loaded.request, &result).unwrap(),
        )
        .unwrap();
        validate_existing_follow_up_plan(
            &plan,
            &loaded,
            &result_sha256,
            &learning_directive,
            &search_policy_revision,
            plan.parent_evidence_signature.as_ref().unwrap(),
        )
        .unwrap();
        assert_eq!(plan.generation, 1);
        let mut profiled = LoadedRequest {
            request: loaded.request.clone(),
            sha256: loaded.sha256.clone(),
        };
        profiled.request.research_plan.mlp_training = Some(paired_mlp_plan_for_tests());
        assert!(follow_up_plan(
            &profiled,
            &result_sha256,
            learning_directive.clone(),
            search_policy_revision.clone(),
            plan.parent_evidence_signature.as_ref().unwrap().clone()
        )
        .unwrap_err()
        .to_string()
        .contains("new root plan"));
        let mut forged_child = plan.clone();
        forged_child.mlp_training = profiled.request.research_plan.mlp_training;
        assert!(forged_child
            .validate()
            .unwrap_err()
            .to_string()
            .contains("new root plan"));
        assert_eq!(plan.parent.as_ref().unwrap().request_sha256, loaded.sha256);
        assert_eq!(
            plan.max_candidates().unwrap(),
            CexCampaignResearchDeltaV1::canonical()
                .gp_template_count()
                .unwrap()
        );
        assert_eq!(
            plan.search_policy_revision.position_policy,
            CexCampaignPositionPolicyV1::HystereticCostAware
        );
        assert_eq!(plan.learning_directive, Some(learning_directive.clone()));
        assert!(plan.hypothesis.contains("declared bounded delta"));
        assert!(plan
            .hypothesis
            .contains("fees, labels, partitions, and holdout remain fixed"));
        assert!(plan.llm.is_none());
        assert_eq!(plan.focus_field, loaded.request.research_plan.focus_field);
        assert_eq!(
            plan.feature_fields,
            loaded.request.research_plan.feature_fields
        );

        let mut child = loaded.request.clone();
        child.research_plan = plan.clone();
        assert_ne!(
            expected_campaign_id(&child).unwrap(),
            loaded.request.campaign_id
        );
        child.campaign_inputs_sha256 = "e".repeat(64);
        assert!(validate_request(&child)
            .unwrap_err()
            .to_string()
            .contains("follow-up parent evidence does not match Campaign inputs"));

        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("next-plan.json");
        write_research_plan_create_once(&output, &plan).unwrap();
        assert_eq!(load_research_plan(&output).unwrap(), plan);
        assert!(write_research_plan_create_once(&output, &plan).is_ok());
        let mut changed_plan = plan.clone();
        changed_plan.objective.push_str(" with drift");
        assert!(write_research_plan_create_once(&output, &changed_plan).is_err());

        let request_path = root.path().join("request.json");
        let result_path = root.path().join("result.json");
        std::fs::write(&request_path, serialize_request(&loaded.request).unwrap()).unwrap();
        hft_research_artifacts::write_json_atomic(&result_path, &result).unwrap();
        let args = CampaignLearnArgs {
            control: root.path().join("missing-control.json"),
            submission: root.path().join("missing-submission.json"),
            settlement: root.path().join("missing-settlement.json"),
            namespace: "monday-research".into(),
            request: request_path,
            result_sha256: hft_research_artifacts::sha256_file(&result_path).unwrap(),
            result: result_path,
            output: root.path().join("learned-plan.json"),
        };
        assert!(learn(args.clone()).is_err());
        assert!(!args.output.exists());
    }

    #[test]
    fn study_proposal_binds_authenticated_parent_input_separately_from_target_input() {
        use alpha_domain::campaign_control::{
            CampaignEvaluationViewsV1, CampaignExecutionBindingV1, CampaignSelectionFeedbackV1,
        };

        let loaded = loaded_request_for_learning();
        let result = negative_campaign_result(&loaded);
        let result_sha256 = "9".repeat(64);
        assert!(classify_campaign_failure(&result).is_err());
        let failure_class = CexCampaignFailureClassV1::OvertradeCapacity;
        let (revision, directive) =
            next_campaign_policy_revision(&loaded, &result_sha256, failure_class).unwrap();
        let mut plan = follow_up_plan(
            &loaded,
            &result_sha256,
            directive,
            revision,
            campaign_research_evidence_signature(&loaded.request, &result).unwrap(),
        )
        .unwrap();
        let horizon = CampaignLabelHorizonV1::canonical();
        plan.label_horizon = Some(horizon.clone());
        plan.validate().unwrap();

        let parent = CampaignNextFamilyParentV1 {
            campaign_id: loaded.request.campaign_id.clone(),
            family_id: "parent-family".into(),
            root_grant_sha256: "1".repeat(64),
            request_sha256: loaded.sha256.clone(),
            campaign_inputs_sha256: loaded.request.campaign_inputs_sha256.clone(),
            campaign_result_sha256: result_sha256.clone(),
            family_settlement_receipt_sha256: "2".repeat(64),
            study_settlement_receipt_sha256: "3".repeat(64),
            study_snapshot_sha256: "4".repeat(64),
            terminal_job_uid: "parent-job".into(),
            terminal_pod_uid: "parent-pod".into(),
        };
        let target_inputs_sha256 = "e".repeat(64);
        let target_execution = CampaignExecutionBindingV1 {
            campaign_inputs_sha256: target_inputs_sha256.clone(),
            evaluation_protocol_sha256: "5".repeat(64),
            evaluation_views: CampaignEvaluationViewsV1 {
                search_view_sha256: "6".repeat(64),
                selection_view_sha256: "7".repeat(64),
                selection_feedback: CampaignSelectionFeedbackV1::IndependentSelectionWithheld,
            },
            source_revision: loaded.request.build_source_revision.clone(),
            runner_image: format!("registry/runner@sha256:{}", "8".repeat(64)),
            controller_image: format!("registry/controller@sha256:{}", "9".repeat(64)),
            job_cpu_millis: 1,
            job_memory_mib: 1,
            accelerator: alpha_domain::research_accelerator::ResearchAcceleratorV1::Cpu,
        };
        let proposal_for = |parent: CampaignNextFamilyParentV1| CampaignNextFamilyProposalV1 {
            schema_version: alpha_domain::campaign_horizon::CAMPAIGN_NEXT_FAMILY_PROPOSAL_SCHEMA_V1
                .into(),
            study_id: "study-cross-input".into(),
            study_grant_sha256: "a".repeat(64),
            parent,
            target_family_id: "target-family".into(),
            target_root_grant_sha256: "b".repeat(64),
            target_member_sha256: "c".repeat(64),
            target_execution: target_execution.clone(),
            target_horizon: horizon.clone(),
            target_horizon_sha256: horizon.content_hash().unwrap(),
            target_window: CampaignNextFamilyInputWindowV1 {
                mission_id: "target-mission".into(),
                output_prefix: "target".into(),
                start_received_at_ns: 1,
                end_received_at_ns: 2,
                bucket_ms: 1_000,
                top_depth: 5,
            },
            target_research_plan_sha256: plan.content_hash().unwrap(),
        };
        let campaign_root = loaded
            .request
            .campaign_result_put_url
            .split_once("/campaign-id=")
            .unwrap()
            .0;
        let build_target_request =
            |proposal: Option<&CampaignNextFamilyProposalV1>,
             target_inputs: &str,
             target_plan: &CexCampaignResearchPlanV1| {
                build_request_from_parts(
                    &loaded.request.feature_url,
                    &loaded.request.feature_sha256,
                    &loaded.request.materialization_url,
                    &loaded.request.materialization_sha256,
                    &loaded.request.replay_artifact_url,
                    &loaded.request.replay_artifact_sha256,
                    &loaded.request.replay_manifest_url,
                    &loaded.request.replay_manifest_sha256,
                    target_inputs,
                    &loaded.request.producer_source_revision,
                    &loaded.request.producer_image_identity,
                    loaded.request.prepared_inputs.as_ref(),
                    target_plan,
                    &loaded.request.build_source_revision,
                    &loaded.request.image_identity,
                    campaign_root,
                    &loaded.request.holdout_id,
                    &[11, 17],
                    proposal,
                )
            };

        let proposal = proposal_for(parent.clone());
        let target_request = build_target_request(Some(&proposal), &target_inputs_sha256, &plan);
        assert!(
            target_request.is_ok(),
            "cross-input Study handoff should validate"
        );

        let mut wrong_parent = parent.clone();
        wrong_parent.campaign_id = "cex-campaign-wrong-parent".into();
        let wrong_parent_proposal = proposal_for(wrong_parent);
        assert!(
            build_target_request(Some(&wrong_parent_proposal), &target_inputs_sha256, &plan)
                .is_err()
        );

        let mut wrong_input = parent.clone();
        wrong_input.campaign_inputs_sha256 = "d".repeat(64);
        let wrong_input_proposal = proposal_for(wrong_input);
        assert!(
            build_target_request(Some(&wrong_input_proposal), &target_inputs_sha256, &plan)
                .is_err()
        );

        assert!(build_target_request(None, &target_inputs_sha256, &plan).is_err());
    }

    #[test]
    fn repeated_no_trades_keeps_prediction_identity_across_generations() {
        let loaded = loaded_request_for_learning();
        let result = negative_campaign_result(&loaded);
        let result_sha256 = "9".repeat(64);
        let evidence = campaign_research_evidence_signature(&loaded.request, &result).unwrap();
        let (first_revision, first_directive) = next_campaign_policy_revision(
            &loaded,
            &result_sha256,
            CexCampaignFailureClassV1::NoTradesAfterCosts,
        )
        .unwrap();
        assert_eq!(
            first_revision.position_policy,
            CexCampaignPositionPolicyV1::PredictionIdentity
        );
        let first_plan = follow_up_plan(
            &loaded,
            &result_sha256,
            first_directive,
            first_revision,
            evidence.clone(),
        )
        .unwrap();
        let mut second_parent = LoadedRequest {
            request: loaded.request.clone(),
            sha256: loaded.sha256.clone(),
        };
        second_parent.request.research_plan = first_plan;

        let (second_revision, second_directive) = next_campaign_policy_revision(
            &second_parent,
            &result_sha256,
            CexCampaignFailureClassV1::NoTradesAfterCosts,
        )
        .unwrap();
        assert_eq!(
            second_revision.position_policy,
            CexCampaignPositionPolicyV1::PredictionIdentity
        );
        let second_plan = follow_up_plan(
            &second_parent,
            &result_sha256,
            second_directive,
            second_revision,
            evidence,
        )
        .unwrap();
        assert_eq!(second_plan.generation, 2);
        assert_eq!(
            second_plan.search_policy_revision.position_policy,
            CexCampaignPositionPolicyV1::PredictionIdentity
        );
        second_plan.validate().unwrap();
    }

    #[test]
    fn typed_trial_reservation_includes_the_supervised_model_upper_bound() {
        let mut loaded = loaded_request_for_learning();
        let parent_revision_id = loaded
            .request
            .research_plan
            .search_policy_revision
            .revision_id
            .clone();
        let revision = CexCampaignSearchPolicyRevisionV1::new_typed(
            Some(parent_revision_id.clone()),
            CexCampaignPositionPolicyV1::HystereticCostAware,
            CexCampaignResearchDeltaV1 {
                feature_fields: vec!["book_imbalance_top5".to_string()],
                operators: vec![hft_factor_dsl::FactorOperator::ZScore],
                windows: vec![20],
                ridge_l2: 1.0e-6,
                cart_max_depth: 3,
                cart_min_leaf: 5,
            },
        )
        .unwrap();
        let declared_revision = CexCampaignSearchPolicyRevisionV1::new_typed(
            None,
            revision.position_policy,
            CexCampaignResearchDeltaV1 {
                feature_fields: revision
                    .research_delta
                    .as_ref()
                    .unwrap()
                    .feature_fields
                    .clone(),
                operators: revision.research_delta.as_ref().unwrap().operators.clone(),
                windows: revision.research_delta.as_ref().unwrap().windows.clone(),
                ridge_l2: revision.research_delta.as_ref().unwrap().ridge_l2,
                cart_max_depth: revision.research_delta.as_ref().unwrap().cart_max_depth,
                cart_min_leaf: revision.research_delta.as_ref().unwrap().cart_min_leaf,
            },
        )
        .unwrap();
        loaded.request.research_plan.allowed_search_policy_revisions = vec![
            CexCampaignSearchPolicyRevisionV1::canonical(),
            declared_revision,
        ];
        let result = negative_campaign_result(&loaded);
        let result_sha256 = "9".repeat(64);
        let parent = CexCampaignResearchParentV1 {
            campaign_id: loaded.request.campaign_id.clone(),
            request_sha256: loaded.sha256.clone(),
            campaign_result_sha256: result_sha256.clone(),
        };
        let directive = CexCampaignLearningDirectiveV1::new(
            &parent,
            CexCampaignFailureClassV1::OvertradeCapacity,
            parent_revision_id,
            revision.revision_id.clone(),
        )
        .unwrap();
        let plan = follow_up_plan(
            &loaded,
            &result_sha256,
            directive,
            revision,
            campaign_research_evidence_signature(&loaded.request, &result).unwrap(),
        )
        .unwrap();
        assert_eq!(plan.max_candidates().unwrap(), 1);
        assert_eq!(declared_total_trials_for_rounds(&plan, 2).unwrap(), 8);
    }

    #[test]
    fn no_trades_rejects_when_all_prediction_identity_deltas_are_exhausted() {
        let mut loaded = loaded_request_for_learning();
        let identity_ids = loaded
            .request
            .research_plan
            .allowed_search_policy_revisions
            .iter()
            .filter(|revision| {
                revision.position_policy == CexCampaignPositionPolicyV1::PredictionIdentity
            })
            .map(|revision| revision.revision_id.clone())
            .collect::<Vec<_>>();
        loaded
            .request
            .research_plan
            .attempted_search_policy_revision_ids =
            std::iter::once(CexCampaignSearchPolicyRevisionV1::canonical().revision_id)
                .chain(identity_ids)
                .collect();
        assert!(next_campaign_policy_revision(
            &loaded,
            &"9".repeat(64),
            CexCampaignFailureClassV1::NoTradesAfterCosts,
        )
        .is_err());
    }

    #[test]
    fn historical_capacity_summary_cannot_override_learning_qualification() {
        let loaded = loaded_request_for_learning();
        let result_sha256 = "9".repeat(64);
        let result = negative_campaign_result(&loaded);
        validate_negative_campaign_result(&loaded, &result, &result_sha256).unwrap();

        assert!(classify_campaign_failure(&result).is_err());
        // Composition is independent from admission of this historical summary.
        let failure_class = CexCampaignFailureClassV1::OvertradeCapacity;
        let (revision, directive) =
            next_campaign_policy_revision(&loaded, &result_sha256, failure_class).unwrap();
        assert_eq!(
            revision.position_policy,
            CexCampaignPositionPolicyV1::HystereticCostAware
        );
        assert_eq!(directive.failure_class, failure_class);
        assert_eq!(directive.search_policy_revision_id, revision.revision_id);
    }

    #[test]
    fn declared_feature_subset_flows_into_the_parent_bound_child_plan() {
        let mut loaded = loaded_request_for_learning();
        let allowlist = CexCampaignSearchPolicyRevisionV1::bounded_allowlist();
        let subset = allowlist
            .iter()
            .find(|revision| {
                revision.position_policy == CexCampaignPositionPolicyV1::HystereticCostAware
                    && revision
                        .research_delta
                        .as_ref()
                        .is_some_and(|delta| delta.feature_fields.len() < 9)
            })
            .cloned()
            .unwrap();
        loaded.request.research_plan.allowed_search_policy_revisions = vec![
            CexCampaignSearchPolicyRevisionV1::canonical(),
            subset.clone(),
        ];
        let result = negative_campaign_result(&loaded);
        let result_sha256 = "9".repeat(64);
        let (revision, directive) = next_campaign_policy_revision(
            &loaded,
            &result_sha256,
            CexCampaignFailureClassV1::OvertradeCapacity,
        )
        .unwrap();
        assert_eq!(revision.revision_id, subset.revision_id);
        let plan = follow_up_plan(
            &loaded,
            &result_sha256,
            directive,
            revision,
            campaign_research_evidence_signature(&loaded.request, &result).unwrap(),
        )
        .unwrap();
        assert_eq!(
            plan.feature_fields,
            subset.research_delta.unwrap().feature_fields
        );
        assert!(plan
            .feature_fields
            .iter()
            .any(|field| field == &plan.focus_field));
        plan.validate().unwrap();
        validate_existing_follow_up_plan(
            &plan,
            &loaded,
            &result_sha256,
            plan.learning_directive.as_ref().unwrap(),
            &plan.search_policy_revision,
            plan.parent_evidence_signature.as_ref().unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn positive_ic_negative_net_creates_a_hysteretic_directive() {
        let loaded = loaded_request_for_learning();
        let result_sha256 = "9".repeat(64);
        let result = positive_ic_negative_net_result(&loaded);
        validate_negative_campaign_result(&loaded, &result, &result_sha256).unwrap();

        assert!(classify_campaign_failure(&result).is_err());
        let failure_class = CexCampaignFailureClassV1::PositiveIcNegativeNet;
        let (revision, directive) =
            next_campaign_policy_revision(&loaded, &result_sha256, failure_class).unwrap();
        assert_eq!(
            revision.position_policy,
            CexCampaignPositionPolicyV1::HystereticCostAware
        );
        assert_eq!(directive.failure_class, failure_class);
        assert_eq!(directive.search_policy_revision_id, revision.revision_id);
        let plan = follow_up_plan(
            &loaded,
            &result_sha256,
            directive,
            revision,
            campaign_research_evidence_signature(&loaded.request, &result).unwrap(),
        )
        .unwrap();
        assert!(plan.llm.is_none());
        assert!(plan.hypothesis.contains("improve replay net returns"));
        assert_eq!(
            plan.search_policy_revision.position_policy,
            CexCampaignPositionPolicyV1::HystereticCostAware
        );
    }

    fn actual_diagnosis_evaluation(fee_bps: f64, fold_count: usize) -> CandidateEvaluation {
        use alpha_domain::{
            CandidateArtifact, EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationProtocolV1,
            EvaluationWalkForwardV1, FormulaEvaluatorConfig,
        };
        use alpha_engine::{CandidateEvaluator, EngineProposal};
        use std::collections::BTreeMap;
        let start = chrono::Utc::now();
        let rows = (0..500)
            .map(|index| {
                let signal = if index % 2 == 0 { 1.0 } else { -1.0 };
                alpha_engine::evaluation::ResearchRow {
                    series_id: 1,
                    available_time: start + chrono::Duration::minutes(index),
                    label_available_time: start + chrono::Duration::minutes(index + 1),
                    signal,
                    features: BTreeMap::from([("book_imbalance".into(), signal)]),
                    label: signal * 0.01,
                    fee_bps,
                    funding_bps: 0.0,
                    pit_funding: false,
                    latency_bps: 0.0,
                }
            })
            .collect();
        let protocol = EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 200,
                validation_rows: 64,
                fold_count,
                purge_rows: 1,
                embargo_rows: 1,
                sealed_holdout_rows: 64,
            },
            EvaluationCostsV1 {
                fee_bps,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.0,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 1,
                observation_frequency_millis: 60_000,
            },
        )
        .unwrap();
        let prepared = alpha_engine::evaluation::prepare_dataset(rows, &protocol).unwrap();
        let evaluator = alpha_engine::formula_evaluator::FormulaEvaluator::new(
            FormulaEvaluatorConfig::default(),
        )
        .unwrap();
        let proposal = EngineProposal {
            candidate_id: "diagnosis-original-evaluator".into(),
            hypothesis: "original cost fixture".into(),
            artifact: CandidateArtifact::Formula(hft_factor_dsl::FactorAst::Terminal(
                hft_factor_dsl::FactorTerminal::Field("book_imbalance".into()),
            )),
            expansions: 1,
            tokens: 0,
            elapsed_ms: 0,
        };
        let evaluation = evaluator
            .evaluate(&proposal, &prepared.engine_context())
            .unwrap();
        evaluation.validate().unwrap();
        evaluation
    }

    pub(crate) fn qualified_cost_learning_result_bytes(
        request: &CampaignRequest,
        feature: &Path,
        materialization: &Path,
    ) -> Vec<u8> {
        use alpha_engine::{CandidateEvaluator, EngineProposal};
        let inputs = PreparedCexInputs::load(feature, materialization, true).unwrap();
        assert_eq!(inputs.feature_sha256(), request.feature_sha256);
        assert_eq!(
            inputs.materialization_sha256(),
            request.materialization_sha256
        );
        let protocol = mission_render::approved_evaluation_protocol_for_plan(
            inputs.materialization(),
            &request.research_plan,
        )
        .unwrap();
        let prepared = alpha_engine::evaluation::prepare_dataset(
            inputs.native_source_rows(&protocol).unwrap(),
            &protocol,
        )
        .unwrap();
        let evaluator = alpha_engine::formula_evaluator::FormulaEvaluator::new(
            alpha_domain::FormulaEvaluatorConfig {
                multiple_testing_trials: request
                    .research_plan
                    .effective_multiple_testing_trials(request.declared_total_trials)
                    .unwrap(),
                ..Default::default()
            },
        )
        .unwrap();
        let proposal = EngineProposal {
            candidate_id: "original-protocol-cost-interface-fixture".into(),
            hypothesis: "software composition with original admitted data and protocol".into(),
            artifact: alpha_domain::CandidateArtifact::Formula(
                hft_factor_dsl::FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                    "book_imbalance".into(),
                )),
            ),
            expansions: 1,
            tokens: 0,
            elapsed_ms: 0,
        };
        let evaluation = evaluator
            .evaluate(&proposal, &prepared.engine_context())
            .unwrap();
        assert_eq!(
            evaluation.evaluation_protocol_hash,
            Some(protocol.content_hash().unwrap())
        );
        let facts = evaluation.predictive_screening_gate_facts().unwrap();
        assert!(facts.predictive_passed && facts.coverage_passed);
        assert!(!evaluation.passed && evaluation.metrics.trade_count > 0);
        assert!(
            evaluation.metrics.cumulative_net_return < 0.0 && evaluation.metrics.net_sharpe < 0.0
        );
        let loaded = LoadedRequest {
            request: request.clone(),
            sha256: hex::encode(Sha256::digest(serialize_request(request).unwrap())),
        };
        // The surrounding terminal/model layout is a software fixture. Its selected
        // metrics are from the original evaluator, not a claim of a trained model Run.
        let mut result = negative_campaign_result(&loaded);
        result.schema_version = CAMPAIGN_RESULT_SCHEMA_V9.into();
        let summary = campaign_evaluation_feedback(&evaluation);
        for round in &mut result.rounds {
            round.feedback.supervised_ridge = Some(summary.clone());
            round.feedback.supervised_cart = Some(summary.clone());
            round.feedback.supervised_burn = Some(summary.clone());
            round.feedback.burn = Some(summary.clone());
            round.feedback.supervised_selected = Some(summary.clone());
            round.feedback.supervised_selected_evaluation_proof =
                Some(CampaignSelectedEvaluationProofV1::from_evaluation(&evaluation).unwrap());
            round.supervised_candidate_id = Some(proposal.candidate_id.clone());
            round.feedback.supervised_selected_candidate_id = Some(proposal.candidate_id.clone());
        }
        let bytes = serde_json::to_vec(&result).unwrap();
        validate_negative_campaign_result(
            &loaded,
            &result,
            &hft_cex_research_input::sha256(&bytes),
        )
        .unwrap();
        assert_eq!(
            classify_campaign_failure(&result).unwrap(),
            CexCampaignFailureClassV1::PositiveIcNegativeNet
        );
        bytes
    }

    #[test]
    fn positive_ic_with_missing_icir_and_negative_net_is_not_a_cost_failure() {
        let evaluation = actual_diagnosis_evaluation(100.0, 1);
        assert_eq!(evaluation.metrics.predictive.time_series_ic, Some(1.0));
        assert_eq!(evaluation.metrics.predictive.time_series_rank_ic, Some(1.0));
        assert_eq!(evaluation.metrics.predictive.time_series_icir, None);
        assert_eq!(evaluation.metrics.predictive.time_series_rank_icir, None);
        assert!(evaluation.metrics.trade_count > 0);
        assert!(evaluation.metrics.cumulative_net_return < 0.0);
        assert!(evaluation.metrics.net_sharpe < 0.0);
        assert!(!evaluation.passed);
        let loaded = loaded_request_for_learning();
        let mut result = negative_campaign_result(&loaded);
        result.schema_version = CAMPAIGN_RESULT_SCHEMA_V9.into();
        for round in &mut result.rounds {
            round.feedback.supervised_selected = Some(campaign_evaluation_feedback(&evaluation));
            round.feedback.supervised_selected_evaluation_proof =
                Some(CampaignSelectedEvaluationProofV1::from_evaluation(&evaluation).unwrap());
        }
        assert!(
            classify_campaign_failure(&result).is_err(),
            "predictive rejection was misclassified as a cost-learning failure"
        );
    }

    #[test]
    fn complete_original_prediction_gates_then_transaction_cost_failure_are_distinct() {
        let evaluation = actual_diagnosis_evaluation(100.0, 3);
        let config = evaluation.formula_config().unwrap();
        let predictive = &evaluation.metrics.predictive;
        assert!(predictive.time_series_ic.unwrap() >= config.min_time_series_ic);
        assert!(predictive.time_series_rank_ic.unwrap() >= config.min_time_series_rank_ic);
        assert!(predictive.time_series_icir.unwrap() >= config.min_time_series_icir);
        assert!(predictive.time_series_rank_icir.unwrap() >= config.min_time_series_rank_icir);
        assert!(predictive.positive_ic_ratio >= config.min_positive_ic_ratio);
        assert!(evaluation
            .metrics
            .folds
            .iter()
            .all(|fold| fold.row_count >= config.min_validation_rows));
        assert!(!evaluation.passed);
        assert!(evaluation.metrics.trade_count > 0);
        assert!(evaluation.metrics.cumulative_net_return < 0.0);
        assert!(evaluation.metrics.net_sharpe < 0.0);
        let facts = evaluation.predictive_screening_gate_facts().unwrap();
        assert!(facts.predictive_passed && facts.coverage_passed);
        let loaded = loaded_request_for_learning();
        let mut result = negative_campaign_result(&loaded);
        result.schema_version = CAMPAIGN_RESULT_SCHEMA_V9.into();
        for round in &mut result.rounds {
            round.feedback.supervised_selected = Some(campaign_evaluation_feedback(&evaluation));
            round.feedback.supervised_selected_evaluation_proof =
                Some(CampaignSelectedEvaluationProofV1::from_evaluation(&evaluation).unwrap());
        }
        assert_eq!(
            classify_campaign_failure(&result).unwrap(),
            CexCampaignFailureClassV1::PositiveIcNegativeNet
        );
    }

    #[test]
    fn selected_cost_failure_proof_rejects_content_summary_and_version_drift() {
        let evaluation = actual_diagnosis_evaluation(100.0, 3);
        let summary = campaign_evaluation_feedback(&evaluation);
        let proof = CampaignSelectedEvaluationProofV1::from_evaluation(&evaluation).unwrap();
        assert!(proof.screening_facts(&summary).unwrap().predictive_passed);
        let mut changed = proof.clone();
        changed.evaluation_content_sha256 = "0".repeat(64);
        assert!(changed.screening_facts(&summary).is_err());
        let mut changed_summary = summary.clone();
        changed_summary.trade_count += 1;
        assert!(proof.screening_facts(&changed_summary).is_err());
        let mut changed = proof;
        changed.evaluation.evaluator_version = "unknown-predictive-evaluator".into();
        changed.evaluation_content_sha256 = canonical_json_hash(&changed.evaluation).unwrap();
        assert!(changed
            .screening_facts(&campaign_evaluation_feedback(&changed.evaluation))
            .is_err());
    }

    #[test]
    fn historical_cost_failure_keeps_original_shape_without_learning_qualification() {
        let loaded = loaded_request_for_learning();
        let historical = negative_campaign_result(&loaded);
        assert_eq!(historical.schema_version, CAMPAIGN_RESULT_SCHEMA_V8);
        let original = serde_json::to_vec(&historical).unwrap();
        let original_sha = hft_cex_research_input::sha256(&original);
        let mut observed = historical.clone();
        let evaluation = actual_diagnosis_evaluation(100.0, 3);
        for round in &mut observed.rounds {
            round.feedback.supervised_selected_evaluation_proof =
                Some(CampaignSelectedEvaluationProofV1::from_evaluation(&evaluation).unwrap());
            preserve_result_feedback_shape(round, &historical.schema_version).unwrap();
        }
        assert_eq!(observed, historical);
        assert_eq!(serde_json::to_vec(&observed).unwrap(), original);
        assert_eq!(
            hft_cex_research_input::sha256(&serde_json::to_vec(&observed).unwrap()),
            original_sha
        );
        assert!(classify_campaign_failure(&historical).is_err());
    }

    #[test]
    fn evidence_signature_binds_inputs_policy_factors_and_evaluation() {
        let mut loaded = loaded_request_for_learning();
        let mut result = negative_campaign_result(&loaded);
        let parent_signature =
            campaign_research_evidence_signature(&loaded.request, &result).unwrap();
        assert!(!campaign_has_no_improvement(
            &loaded.request,
            &parent_signature
        ));

        loaded.request.research_plan.parent_evidence_signature = Some(parent_signature.clone());
        assert!(campaign_has_no_improvement(
            &loaded.request,
            &parent_signature
        ));

        let parent_revision_id = loaded
            .request
            .research_plan
            .search_policy_revision
            .revision_id
            .clone();
        loaded.request.research_plan.search_policy_revision =
            CexCampaignSearchPolicyRevisionV1::new_typed(
                Some(parent_revision_id),
                CexCampaignPositionPolicyV1::HystereticCostAware,
                CexCampaignResearchDeltaV1::canonical(),
            )
            .unwrap();
        let child_signature =
            campaign_research_evidence_signature(&loaded.request, &result).unwrap();
        assert_ne!(child_signature, parent_signature);
        assert!(campaign_has_no_improvement(
            &loaded.request,
            &child_signature
        ));

        let original_factor_signature = result.rounds[0].feedback.factors[0]
            .factor_signature_sha256
            .clone();
        result.rounds[0].feedback.factors[0].factor_signature_sha256 = "f".repeat(64);
        let changed_factors =
            campaign_research_evidence_signature(&loaded.request, &result).unwrap();
        assert!(!campaign_has_no_improvement(
            &loaded.request,
            &changed_factors
        ));
        result.rounds[0].feedback.factors[0].factor_signature_sha256 = original_factor_signature;

        loaded.request.campaign_inputs_sha256 = "e".repeat(64);
        let changed_inputs =
            campaign_research_evidence_signature(&loaded.request, &result).unwrap();
        assert_ne!(
            changed_inputs.campaign_inputs_sha256,
            parent_signature.campaign_inputs_sha256
        );
        assert!(!campaign_has_no_improvement(
            &loaded.request,
            &changed_inputs
        ));

        loaded.request.campaign_inputs_sha256 = result.campaign_inputs_sha256.clone();
        result.rounds[0]
            .feedback
            .supervised_selected
            .as_mut()
            .unwrap()
            .net_sharpe -= 0.1;
        let changed_evaluation =
            campaign_research_evidence_signature(&loaded.request, &result).unwrap();
        assert!(!campaign_has_no_improvement(
            &loaded.request,
            &changed_evaluation
        ));
    }

    #[test]
    fn campaign_learning_continues_after_position_policy_was_already_tried() {
        let mut loaded = loaded_request_for_learning();
        let canonical_revision_id = loaded
            .request
            .research_plan
            .search_policy_revision
            .revision_id
            .clone();
        let hysteretic = CexCampaignSearchPolicyRevisionV1::new_typed(
            Some(canonical_revision_id.clone()),
            CexCampaignPositionPolicyV1::HystereticCostAware,
            CexCampaignResearchDeltaV1::canonical(),
        )
        .unwrap();
        let identity = CexCampaignSearchPolicyRevisionV1::new_typed(
            Some(canonical_revision_id.clone()),
            CexCampaignPositionPolicyV1::PredictionIdentity,
            CexCampaignResearchDeltaV1::canonical(),
        )
        .unwrap();
        loaded.request.research_plan.search_policy_revision = identity.clone();
        loaded
            .request
            .research_plan
            .attempted_search_policy_revision_ids = vec![
            canonical_revision_id,
            hysteretic.revision_id,
            identity.revision_id,
        ];

        assert!(next_campaign_policy_revision(
            &loaded,
            &"9".repeat(64),
            CexCampaignFailureClassV1::NoTradesAfterCosts,
        )
        .is_ok());
        assert!(next_campaign_policy_revision(
            &loaded,
            &"9".repeat(64),
            CexCampaignFailureClassV1::OvertradeCapacity,
        )
        .is_ok());
        assert!(next_campaign_policy_revision(
            &loaded,
            &"9".repeat(64),
            CexCampaignFailureClassV1::PositiveIcNegativeNet,
        )
        .is_ok());
    }

    #[test]
    fn campaign_feedback_accepts_predictive_factor_that_failed_trading_gates() {
        let failed_trading_evaluation = CampaignEvaluationFeedbackV1 {
            passed: false,
            score: 0.1,
            time_series_ic: Some(0.05),
            time_series_rank_ic: Some(0.04),
            cumulative_net_return: -0.01,
            max_drawdown: 0.02,
            net_sharpe: -0.2,
            trade_count: 10,
            total_turnover: 1.0,
            max_book_depth_fraction: None,
            max_book_depth_fraction_limit: None,
            capacity_breached: false,
        };
        let mut feedback = CampaignRoundFeedbackV1 {
            factor_attempts: 1,
            accepted_factors: 1,
            factors: vec![CampaignFactorFeedbackV1 {
                factor_signature_sha256: "7".repeat(64),
                source_features: vec!["book_imbalance".to_string()],
                rejection_codes: Vec::new(),
                evaluation: Some(failed_trading_evaluation.clone()),
            }],
            baseline_gate_passed: false,
            baseline_failure_codes: vec![CexBaselineFailureCodeV1::InsufficientEvidence],
            ridge: Some(failed_trading_evaluation.clone()),
            cart: Some(failed_trading_evaluation),
            ..CampaignRoundFeedbackV1::default()
        };

        validate_campaign_round_feedback(
            &feedback,
            1,
            alpha_domain::CexSupervisedModelScopeV1::default(),
        )
        .unwrap();
        // Fixed-notional additive accounting can lose more than the initial
        // unit. SOL5 produced this rejected trading evaluation in a real run.
        feedback.factors[0]
            .evaluation
            .as_mut()
            .unwrap()
            .max_drawdown = 1.048_747_746_845_106;
        validate_campaign_round_feedback(
            &feedback,
            1,
            alpha_domain::CexSupervisedModelScopeV1::default(),
        )
        .unwrap();
        assert!(!feedback.factors[0].evaluation.as_ref().unwrap().passed);
        let mut invalid = feedback.clone();
        invalid.factors[0].evaluation.as_mut().unwrap().passed = true;
        assert!(validate_campaign_round_feedback(
            &invalid,
            1,
            alpha_domain::CexSupervisedModelScopeV1::default()
        )
        .is_err());
        for drawdown in [-0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut invalid = feedback.clone();
            invalid.factors[0].evaluation.as_mut().unwrap().max_drawdown = drawdown;
            assert!(validate_campaign_round_feedback(
                &invalid,
                1,
                alpha_domain::CexSupervisedModelScopeV1::default()
            )
            .is_err());
        }
        feedback.accepted_factors = 0;
        assert!(validate_campaign_round_feedback(
            &feedback,
            1,
            alpha_domain::CexSupervisedModelScopeV1::default()
        )
        .is_err());
    }

    #[test]
    fn campaign_feedback_preserves_rejected_additive_drawdown_in_replay() {
        let mut replay = CampaignReplayFeedbackV1 {
            passed: false,
            failures: vec!["maximum drawdown exceeded".into()],
            position_changes: 5_267,
            total_turnover: 10_526.0,
            mean_net_return: -0.000_3,
            cumulative_net_return: -3.129_691_227_555_395_6,
            max_drawdown: 1.048_747_746_845_106,
            net_sharpe: -0.9,
        };
        assert!(valid_campaign_replay_feedback(&replay));
        replay.passed = true;
        replay.failures.clear();
        assert!(!valid_campaign_replay_feedback(&replay));
        replay.passed = false;
        replay.failures.push("maximum drawdown exceeded".into());
        for drawdown in [-0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            replay.max_drawdown = drawdown;
            assert!(!valid_campaign_replay_feedback(&replay));
        }
    }

    #[test]
    fn model_attempt_ledger_counts_failed_and_unmaterialized_models() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("supervised-model-attempts.json"),
            serde_json::json!({
                "schema_version": "cex-supervised-model-attempts-v1",
                "attempts": [
                    {"model": "ridge", "outcome": "failed"},
                    {"model": "cart", "outcome": "admitted"},
                    {"model": "burn_mlp", "outcome": "started"}
                ]
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            persisted_supervised_model_attempt_count(root.path(), &CEX_SUPERVISED_MODEL_NAMES)
                .unwrap(),
            Some(3)
        );
    }

    #[test]
    fn missing_model_attempt_ledger_does_not_infer_from_success_artifacts() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("ridge-supervised-candidate.json"), b"{}").unwrap();
        assert_eq!(
            persisted_supervised_model_attempt_count(root.path(), &CEX_SUPERVISED_MODEL_NAMES)
                .unwrap(),
            None
        );
    }

    #[test]
    fn model_attempt_ledger_rejects_fake_or_incomplete_model_sets() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("supervised-model-attempts.json"),
            serde_json::json!({
                "schema_version": "cex-supervised-model-attempts-v1",
                "attempts": [{"model": "x", "outcome": "admitted"}]
            })
            .to_string(),
        )
        .unwrap();
        assert!(
            persisted_supervised_model_attempt_count(root.path(), &CEX_SUPERVISED_MODEL_NAMES)
                .is_err()
        );

        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("supervised-model-attempts.json"),
            serde_json::json!({
                "schema_version": "cex-supervised-model-attempts-v1",
                "attempts": [
                    {"model": "ridge", "outcome": "completed"},
                    {"model": "cart", "outcome": "completed"},
                    {"model": "burn_mlp", "outcome": "completed"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        assert!(
            persisted_supervised_model_attempt_count(root.path(), &CEX_SUPERVISED_MODEL_NAMES)
                .is_err()
        );
    }

    #[test]
    fn campaign_learning_rejects_result_identity_drift() {
        let loaded = loaded_request_for_learning();
        let mut result = negative_campaign_result(&loaded);
        result.request_sha256 = "7".repeat(64);

        assert!(validate_negative_campaign_result(&loaded, &result, &"9".repeat(64)).is_err());
    }

    #[test]
    fn validate_request_rejects_http_transport() {
        let mut request = valid_request();
        request.feature_url = request.feature_url.replacen("https://", "http://", 1);

        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn validate_request_rejects_non_oss_campaign_inputs() {
        let mut request = valid_request();
        request.feature_url = "https://example.com/research/features.jsonl".to_string();
        assert!(validate_request(&request).is_err());

        let mut request = valid_request();
        request.materialization_url =
            "https://example.com/research/materialization.json".to_string();
        assert!(validate_request(&request).is_err());

        let mut request = valid_request();
        request.replay_artifact_url = "https://example.com/research/replay.parquet".to_string();
        assert!(validate_request(&request).is_err());

        let mut request = valid_request();
        request.replay_manifest_url =
            "https://example.com/research/replay-manifest.json".to_string();
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn validate_request_rejects_noncanonical_producer_digests() {
        let mut request = valid_request();
        request.campaign_inputs_sha256 = "A".repeat(64);
        assert!(validate_request(&request).is_err());

        let mut request = valid_request();
        request.producer_image_identity = format!(" {} ", "a".repeat(64));
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn round_result_path_binds_the_shared_holdout_claim() {
        let request = valid_request();
        assert_eq!(
            cex_global_holdout_claim_object(&request.holdout_id).unwrap(),
            canonical_tokyo_oss_internal_object("holdout", &request.holdout_claim_put_url).unwrap()
        );
    }

    #[test]
    fn validate_request_rejects_non_oss_campaign_outputs() {
        let mut request = valid_request();
        request.holdout_claim_put_url =
            "https://example.com/research/sealed-holdout-claim.json".to_string();
        request.holdout_claim_readback_url = request.holdout_claim_put_url.clone();
        assert!(validate_request(&request).is_err());

        let mut request = valid_request();
        request.campaign_result_put_url =
            "https://example.com/research/campaign-id=placeholder/campaign-result.json".to_string();
        request.campaign_result_readback_url = request.campaign_result_put_url.clone();
        assert!(validate_request(&request).is_err());

        let mut request = valid_request();
        request.rounds[0].mission_put_url =
            "https://example.com/research/campaign-id=placeholder/round=r1/mission.json"
                .to_string();
        request.rounds[0].mission_readback_url = request.rounds[0].mission_put_url.clone();
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn validate_request_rejects_single_round_campaign() {
        let mut request = valid_request();
        request.rounds.truncate(1);
        request.campaign_id = expected_campaign_id(&request).unwrap();
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn validate_request_rejects_underdeclared_total_trials() {
        let mut request = valid_request();
        request.declared_total_trials =
            declared_total_trials_for_rounds(&request.research_plan, 1).unwrap();
        request.campaign_id = expected_campaign_id(&request).unwrap();
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn selection_tie_break_is_deterministic() {
        let lower_hash = CampaignMissionLedgerV1 {
            round_id: "r1".to_string(),
            seed: 11,
            identity: valid_request().rounds[0].identity.clone(),
            request_sha256: Some("0".repeat(64)),
            mission_id: "m1".to_string(),
            mission_sha256: "a".repeat(64),
            result_bundle_sha256: "b".repeat(64),
            result_readback_bundle_sha256: "b".repeat(64),
            replay_receipt_id: Some("receipt-1".to_string()),
            replay_gate_passed: Some(true),
            supervised_candidate_id: None,
            supervised_replay_receipt_id: None,
            supervised_replay_gate_passed: None,
            final_precommit_id: None,
            sealed_receipt_id: None,
            sealed_passed: None,
            strategy_bundle_id: None,
            promotion_id: None,
            selected_candidate_id: Some("candidate-1".to_string()),
            selected_candidate_content_hash: Some("0".repeat(64)),
            selected_score: Some(10.0),
            consumed_trials: 4,
            termination_reason: "pre_holdout_candidate_kept".to_string(),
            feedback: CampaignRoundFeedbackV1::default(),
        };
        let higher_hash = CampaignMissionLedgerV1 {
            round_id: "r2".to_string(),
            seed: 17,
            identity: valid_request().rounds[1].identity.clone(),
            request_sha256: Some("1".repeat(64)),
            mission_id: "m2".to_string(),
            mission_sha256: "c".repeat(64),
            result_bundle_sha256: "d".repeat(64),
            result_readback_bundle_sha256: "d".repeat(64),
            replay_receipt_id: Some("receipt-2".to_string()),
            replay_gate_passed: Some(true),
            supervised_candidate_id: None,
            supervised_replay_receipt_id: None,
            supervised_replay_gate_passed: None,
            final_precommit_id: None,
            sealed_receipt_id: None,
            sealed_passed: None,
            strategy_bundle_id: None,
            promotion_id: None,
            selected_candidate_id: Some("candidate-2".to_string()),
            selected_candidate_content_hash: Some("f".repeat(64)),
            selected_score: Some(10.0),
            consumed_trials: 4,
            termination_reason: "pre_holdout_candidate_kept".to_string(),
            feedback: CampaignRoundFeedbackV1::default(),
        };

        assert!(compare_round_selection(&higher_hash, &lower_hash).is_lt());
        assert!(compare_round_selection(&lower_hash, &higher_hash).is_gt());
    }

    #[test]
    fn same_holdout_keeps_one_global_claim_across_output_roots() {
        let request = valid_request();
        let claim = request.holdout_claim_put_url.clone();
        let rebased = rebind_request_to_output_root(request, "other-root");

        assert_eq!(rebased.holdout_claim_put_url, claim);
        assert_eq!(rebased.holdout_claim_readback_url, claim);
        validate_request(&rebased).unwrap();
    }

    pub(crate) struct NativePreparedFixture {
        pub(crate) request: CampaignRequest,
        pub(crate) inputs: prepared_inputs::VerifiedNativeCampaignPreparedInputs,
        _source: CampaignE2eFixture,
        _root: tempfile::TempDir,
    }

    impl NativePreparedFixture {
        pub(crate) fn materialization_path(&self) -> &Path {
            &self._source._render_fixture.materialization_path
        }
        pub(crate) fn bind_representation_for_tests(
            &mut self,
            root: &alpha_domain::campaign_control::VerifiedCampaignRootGrant,
            store: &alpha_store::AlphaStore,
            freeze_path: &Path,
        ) -> anyhow::Result<()> {
            let original_request_sha = self.inputs.request_sha256().to_owned();
            let original_inputs_sha = self.request.campaign_inputs_sha256.clone();
            let original_reference = self.request.prepared_inputs.clone();
            let objects = published_objects_for_tests(self)?;
            let request = representation_https_request_for_tests(
                &self.request,
                root,
                store,
                freeze_path,
                &objects,
            )?;
            if request.campaign_inputs_sha256 != original_inputs_sha
                || request.campaign_inputs_sha256 != root.grant().execution.campaign_inputs_sha256
                || request.prepared_inputs != original_reference
            {
                bail!("column selection changed the frozen signed DataReady identity");
            }
            let reference = request
                .prepared_inputs
                .as_ref()
                .context("missing fixture reference")?;
            let mut bytes = std::collections::BTreeMap::new();
            for (sha, url) in &reference.block_urls {
                bytes.insert(
                    sha.clone(),
                    objects.get(url).context("missing frozen block")?.clone(),
                );
            }
            let expected = hft_cex_research_input::sha256(&serialize_request(&request)?);
            self.inputs = prepared_inputs::inspect_finalized_campaign_prepared_inputs(
                &request,
                &expected,
                self.inputs.prepared().manifest().clone(),
                &mut hft_cex_research_input::prepared::AcquiredBlocks { bytes },
                1024 * 1024 * 1024,
            )?;
            if self.inputs.request_sha256() == original_request_sha {
                bail!("representation fixture did not bind the new complete request identity");
            }
            validate_request_for_execute(&request)?;
            self.request = request;
            Ok(())
        }

        pub(crate) fn original_receipt_path(&self) -> PathBuf {
            self._root.path().join("native-source-inputs.json")
        }
        pub(crate) fn augmented_receipt_path(&self) -> PathBuf {
            self._root.path().join("native-campaign-inputs.json")
        }
    }

    pub(crate) fn native_prepared_fixture_for_tests() -> NativePreparedFixture {
        let source_fixture = campaign_e2e_fixture("native-body-equivalence", false, false, true);
        let (request, inputs, root) = prepare_native_source_fixture(&source_fixture).unwrap();
        NativePreparedFixture {
            request,
            inputs,
            _source: source_fixture,
            _root: root,
        }
    }

    pub(crate) fn native_prepared_planning_fixture_for_tests() -> NativePreparedFixture {
        let source_fixture = campaign_e2e_fixture("native-planning-freeze", false, false, true);
        let (request, inputs, root) = prepare_native_source_fixture_with_publication(
            &source_fixture,
            Some("https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/native-planning-fixture"),
        ).unwrap();
        NativePreparedFixture {
            request,
            inputs,
            _source: source_fixture,
            _root: root,
        }
    }

    pub(crate) fn native_prepared_calendar_planning_fixture_for_tests() -> NativePreparedFixture {
        let source_fixture = calendar_source_fixture(false);
        let (request, inputs, root) = prepare_native_source_fixture_with_publication(
            &source_fixture,
            Some("https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/native-calendar-planning-fixture"),
        ).unwrap();
        NativePreparedFixture {
            request,
            inputs,
            _source: source_fixture,
            _root: root,
        }
    }

    pub(crate) fn write_planning_freeze_for_tests(
        ledger: &alpha_store::AlphaStore,
        request: &CampaignRequest,
        path: &Path,
    ) {
        validate_request(request).unwrap();
        assert_eq!(expected_campaign_id(request).unwrap(), request.campaign_id);
        let plan = authenticated_freeze_plan(request, Some(ledger)).unwrap();
        hft_research_artifacts::write_json_atomic(path, &plan).unwrap();
    }

    fn prepare_native_source_fixture(
        source_fixture: &CampaignE2eFixture,
    ) -> anyhow::Result<(
        CampaignRequest,
        prepared_inputs::VerifiedNativeCampaignPreparedInputs,
        tempfile::TempDir,
    )> {
        prepare_native_source_fixture_with_publication(source_fixture, None)
    }

    fn prepare_native_source_fixture_with_publication(
        source_fixture: &CampaignE2eFixture,
        publication: Option<&str>,
    ) -> anyhow::Result<(
        CampaignRequest,
        prepared_inputs::VerifiedNativeCampaignPreparedInputs,
        tempfile::TempDir,
    )> {
        use hft_cex_research_input::{
            campaign::NativeSourceBindingV1, campaign::SourceBuildRefV1, prepared::AcquiredBlocks,
        };
        let mut request = load_request(&source_fixture.args.request).unwrap().request;
        let render = PreparedCexInputs::load(
            &source_fixture._render_fixture.feature_path,
            &source_fixture._render_fixture.materialization_path,
            true,
        )
        .unwrap();
        let protocol = crate::mission_render::approved_evaluation_protocol_for_plan(
            render.materialization(),
            &request.research_plan,
        )
        .unwrap();
        let rows = render.native_source_rows(&protocol).unwrap();
        let partitions = protocol.row_partitions(rows.len()).unwrap();
        let end = rows[partitions
            .selection
            .as_ref()
            .map_or(partitions.sealed_holdout.start, |p| p.start)]
        .available_time
        .timestamp_micros()
            - 1;
        let canonical = hft_backtest::config::verify_canonical_replay_artifact(
            &source_fixture.replay_artifact_path,
            &source_fixture.replay_manifest_path,
            Some(&request.replay_artifact_sha256),
            &request.replay_manifest_sha256,
            None,
            Some(end),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let item = |path: &Path, sha: &str| CampaignInputReceiptItem {
            relative_path: path.file_name().unwrap().into(),
            object_url: publication.map_or_else(
                || path.to_string_lossy().into_owned(),
                |base| format!("{base}/{}", path.file_name().unwrap().to_string_lossy()),
            ),
            sha256: sha.into(),
        };
        let mut receipt = CampaignInputsReceipt {
            prepared_inputs: None,
            schema_version: CAMPAIGN_INPUTS_SCHEMA_V1.into(),
            run_id: "native-source-test".into(),
            source_revision: request.producer_source_revision.clone(),
            image_ref: format!("registry/source@sha256:{}", request.producer_image_identity),
            mission_id: render.materialization().mission_id.clone(),
            market: render.materialization().market.clone(),
            symbol: render.materialization().symbol.clone(),
            output_prefix: "native-source-test".into(),
            output_object_base_url: publication
                .map_or_else(|| root.path().to_string_lossy().into_owned(), String::from),
            readback_scope: "same-mounted-ossfs-prefix".into(),
            feature: item(
                &source_fixture._render_fixture.feature_path,
                &request.feature_sha256,
            ),
            materialization: item(
                &source_fixture._render_fixture.materialization_path,
                &request.materialization_sha256,
            ),
            replay_artifact: item(
                &source_fixture.replay_artifact_path,
                &request.replay_artifact_sha256,
            ),
            replay_manifest: item(
                &source_fixture.replay_manifest_path,
                &request.replay_manifest_sha256,
            ),
        };
        let original_receipt_bytes = serde_json::to_vec_pretty(&receipt).unwrap();
        let original_receipt_sha = hft_cex_research_input::sha256(&original_receipt_bytes);
        std::fs::write(
            root.path().join("native-source-inputs.json"),
            &original_receipt_bytes,
        )
        .unwrap();
        let source = NativeSourceBindingV1 {
            build: SourceBuildRefV1 {
                source_revision: receipt.source_revision.clone(),
                image_identity: receipt.image_ref.clone(),
            },
            preparation_run_id: receipt.run_id.clone(),
            preparation_receipt_sha256: original_receipt_sha,
            feature_sha256: request.feature_sha256.clone(),
            materialization_sha256: request.materialization_sha256.clone(),
            replay_artifact_sha256: request.replay_artifact_sha256.clone(),
            replay_manifest_sha256: request.replay_manifest_sha256.clone(),
        };
        let artifacts = prepared_inputs::export_trusted_source(source, rows, &protocol, canonical)?;
        let collection_path = root.path().join(format!("{}.json", artifacts.id));
        std::fs::write(
            &collection_path,
            serde_json::to_vec(&artifacts.manifest).unwrap(),
        )
        .unwrap();
        let mut block_urls = std::collections::BTreeMap::new();
        for (sha, bytes) in &artifacts.blocks {
            let path = root.path().join(format!("{sha}.mondaybin"));
            std::fs::write(&path, bytes).unwrap();
            block_urls.insert(
                sha.clone(),
                publication.map_or_else(
                    || path.to_string_lossy().into_owned(),
                    |base| format!("{base}/native-prepared/{sha}.mondaybin"),
                ),
            );
        }
        let reference = prepared_inputs::NativePreparedCampaignRefV1 {
            collection_sha256: artifacts.id.clone(),
            collection_url: publication.map_or_else(
                || collection_path.to_string_lossy().into_owned(),
                |base| format!("{base}/native-prepared/{}.json", artifacts.id),
            ),
            expected_native: artifacts.manifest.expected_native().unwrap(),
            block_urls,
            render_metadata: render.native_metadata().unwrap(),
            planning_metadata: Some(artifacts.manifest.original.clone()),
        };
        receipt.prepared_inputs = Some(reference.clone());
        let augmented_receipt_bytes = serde_json::to_vec_pretty(&receipt).unwrap();
        request.campaign_inputs_sha256 = hft_cex_research_input::sha256(&augmented_receipt_bytes);
        std::fs::write(
            root.path().join("native-campaign-inputs.json"),
            &augmented_receipt_bytes,
        )
        .unwrap();
        request.prepared_inputs = Some(reference);
        request.schema_version = CAMPAIGN_REQUEST_SCHEMA_V6.into();
        request.feature_url.clear();
        request.materialization_url.clear();
        request.replay_artifact_url.clear();
        request.replay_manifest_url.clear();
        if let Some(base) = publication {
            request = build_request_from_parts(
                "",
                &request.feature_sha256,
                "",
                &request.materialization_sha256,
                "",
                &request.replay_artifact_sha256,
                "",
                &request.replay_manifest_sha256,
                &request.campaign_inputs_sha256,
                &request.producer_source_revision,
                &request.producer_image_identity,
                request.prepared_inputs.as_ref(),
                &request.research_plan,
                &request.build_source_revision,
                &request.image_identity,
                base,
                &request.holdout_id,
                &request
                    .rounds
                    .iter()
                    .map(|round| round.seed)
                    .collect::<Vec<_>>(),
                request.study_proposal.as_ref(),
            )?;
        }
        let fingerprint = campaign_data_fingerprint_sha256(
            &request.campaign_inputs_sha256,
            &request.producer_source_revision,
            &request.feature_sha256,
            &request.materialization_sha256,
            &request.replay_artifact_sha256,
            &request.replay_manifest_sha256,
        )
        .unwrap();
        for round in &mut request.rounds {
            round.identity.data_fingerprint_sha256 = fingerprint.clone();
        }
        validate_request_for_source(&request)?;
        let expected_request_sha =
            hft_cex_research_input::sha256(&serialize_request(&request).unwrap());
        let inputs = prepared_inputs::inspect_finalized_campaign_prepared_inputs(
            &request,
            &expected_request_sha,
            artifacts.manifest,
            &mut AcquiredBlocks {
                bytes: artifacts.blocks,
            },
            1024 * 1024 * 1024,
        )
        .unwrap();
        Ok((request, inputs, root))
    }

    fn prepare_fixture_for_execute(fixture: &mut CampaignE2eFixture) -> anyhow::Result<()> {
        let (request, inputs, root) = prepare_native_source_fixture(fixture)?;
        std::fs::write(&fixture.args.request, serialize_request(&request).unwrap()).unwrap();
        fixture.args.request_sha256 = inputs.request_sha256().into();
        fixture._prepared_root = Some(root);
        Ok(())
    }

    /// Software publication objects; no network request or cloud readback occurs.
    const PLANNING_FIXTURE_ORIGIN: &str = "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/native-planning-fixture";

    pub(crate) fn published_objects_for_tests(
        fixture: &NativePreparedFixture,
    ) -> anyhow::Result<std::collections::BTreeMap<String, Vec<u8>>> {
        let reference = fixture
            .request
            .prepared_inputs
            .as_ref()
            .context("missing published reference")?;
        let collection_url = format!(
            "{PLANNING_FIXTURE_ORIGIN}/native-prepared/{}.json",
            reference.collection_sha256
        );
        if reference.collection_url != collection_url {
            bail!("fixture is not the original producer's exact publication");
        }
        let read_object = |name: String, sha: &str| -> anyhow::Result<Vec<u8>> {
            let path = fixture._root.path().join(name);
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                bail!("published fixture object is not an original regular file");
            }
            let bytes = std::fs::read(path)?;
            if hft_cex_research_input::sha256(&bytes) != sha {
                bail!("original published fixture object bytes changed");
            }
            Ok(bytes)
        };
        let mut objects = std::collections::BTreeMap::from([(
            collection_url,
            read_object(
                format!("{}.json", reference.collection_sha256),
                &reference.collection_sha256,
            )?,
        )]);
        for (sha, url) in &reference.block_urls {
            if *url != format!("{PLANNING_FIXTURE_ORIGIN}/native-prepared/{sha}.mondaybin") {
                bail!("published block URI differs from the original producer identity");
            }
            objects.insert(url.clone(), read_object(format!("{sha}.mondaybin"), sha)?);
        }
        let receipt = std::fs::read(fixture.augmented_receipt_path())?;
        if hft_cex_research_input::sha256(&receipt) != fixture.request.campaign_inputs_sha256 {
            bail!("original published DataReady receipt bytes changed");
        }
        objects.insert(
            format!("{PLANNING_FIXTURE_ORIGIN}/data-ready.json"),
            receipt,
        );
        Ok(objects)
    }

    pub(crate) fn representation_https_request_for_tests(
        request: &CampaignRequest,
        authority: &alpha_domain::campaign_control::VerifiedCampaignRootGrant,
        store: &alpha_store::AlphaStore,
        freeze_path: &Path,
        objects: &std::collections::BTreeMap<String, Vec<u8>>,
    ) -> anyhow::Result<CampaignRequest> {
        let keys_dir = tempfile::tempdir()?;
        let keys = keys_dir.path().join("planning-current-trust.json");
        std::fs::write(
            &keys,
            serde_json::to_vec(&std::collections::BTreeMap::from([(
                authority.signed_grant().key_id.clone(),
                hex::encode(authority.verifying_key().as_bytes()),
            )]))?,
        )?;
        let receipt = objects
            .get(&format!("{PLANNING_FIXTURE_ORIGIN}/data-ready.json"))
            .context("missing exact published DataReady fixture")?;
        let plan = crate::mission_dispatch::admission::planning_view::with_fixture_view(
            store,
            authority,
            &keys,
            (request, freeze_path),
            receipt,
            |expected, guard| {
                guard()?;
                let reference = request
                    .prepared_inputs
                    .as_ref()
                    .context("missing collection")?;
                let collection: hft_cex_research_input::campaign::CampaignPreparedInputsV1 =
                    serde_json::from_slice(
                        objects
                            .get(&reference.collection_url)
                            .context("missing exact fixture collection")?,
                    )?;
                let mut bytes = std::collections::BTreeMap::new();
                for (sha, url) in &reference.block_urls {
                    guard()?;
                    bytes.insert(
                        sha.clone(),
                        objects
                            .get(url)
                            .context("missing exact fixture block")?
                            .clone(),
                    );
                }
                let verified = prepared_inputs::inspect_finalized_campaign_prepared_inputs(
                    request,
                    expected,
                    collection,
                    &mut hft_cex_research_input::prepared::AcquiredBlocks { bytes },
                    1024 * 1024 * 1024,
                )?;
                guard()?;
                Ok(verified)
            },
            |scope| representation::prepared_plan_for_fixture(scope, authority),
        )?;
        build_request_from_parts(
            "",
            &request.feature_sha256,
            "",
            &request.materialization_sha256,
            "",
            &request.replay_artifact_sha256,
            "",
            &request.replay_manifest_sha256,
            &request.campaign_inputs_sha256,
            &request.producer_source_revision,
            &request.producer_image_identity,
            request.prepared_inputs.as_ref(),
            &plan,
            &request.build_source_revision,
            &request.image_identity,
            &campaign_output_root(&canonical_tokyo_oss_internal_object(
                "HTTPS fixture result",
                &request.campaign_result_put_url,
            )?)?,
            &request.holdout_id,
            &request
                .rounds
                .iter()
                .map(|round| round.seed)
                .collect::<Vec<_>>(),
            None,
        )
    }

    pub(crate) fn assert_https_finalize_binding_for_tests(
        request: &CampaignRequest,
        objects: &std::collections::BTreeMap<String, Vec<u8>>,
    ) -> anyhow::Result<()> {
        use hft_cex_research_input::{
            campaign::CampaignPreparedInputsV1, prepared::AcquiredBlocks,
        };
        let root = tempfile::tempdir()?;
        let canonical = canonicalize_request_transport(request)?;
        let frozen = FrozenCampaignPlan {
            preparation_authentication_tag: None,
            schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.into(),
            campaign_inputs_sha256: canonical.campaign_inputs_sha256.clone(),
            signing_plan: signing_plan(&canonical)?,
            canonical_request: canonical.clone(),
        };
        let mut signed = canonical.clone();
        let prepared = signed
            .prepared_inputs
            .as_mut()
            .context("HTTPS finalize needs actual native collection")?;
        prepared
            .collection_url
            .push_str("?fixture-signature=collection");
        for url in prepared.block_urls.values_mut() {
            url.push_str("?fixture-signature=block");
        }
        signed
            .holdout_claim_put_url
            .push_str("?fixture-signature=put");
        signed
            .holdout_claim_readback_url
            .push_str("?fixture-signature=read");
        signed
            .campaign_result_put_url
            .push_str("?fixture-signature=result-put");
        signed
            .campaign_result_readback_url
            .push_str("?fixture-signature=result-read");
        for round in &mut signed.rounds {
            round
                .mission_put_url
                .push_str("?fixture-signature=mission-put");
            round
                .mission_readback_url
                .push_str("?fixture-signature=mission-read");
            round
                .result_put_url
                .push_str("?fixture-signature=result-put");
            round
                .result_readback_url
                .push_str("?fixture-signature=result-read");
        }
        validate_request(&signed)?;
        validate_request_matches_freeze(&signed, &frozen)?;
        for kind in ["data", "plan", "runner", "object"] {
            let mut changed = signed.clone();
            match kind {
                "data" => changed.feature_sha256 = "0".repeat(64),
                "plan" => {
                    changed
                        .research_plan
                        .representation_binding
                        .as_mut()
                        .unwrap()
                        .goal
                        .model
                        .content_sha256 = "0".repeat(64)
                }
                "runner" => {
                    changed.build_source_revision =
                        "abcdef0123456789abcdef0123456789abcdef01".into()
                }
                "object" => {
                    changed.prepared_inputs.as_mut().unwrap().collection_url =
                        "https://foreign.oss-ap-northeast-1-internal.aliyuncs.com/other.json".into()
                }
                _ => unreachable!(),
            }
            if validate_request_matches_freeze(&changed, &frozen).is_ok() {
                bail!("HTTPS freeze accepted {kind} drift");
            }
        }
        let acquire = |observed: &CampaignRequest,
                       expected_sha: &str,
                       source_objects: &std::collections::BTreeMap<String, Vec<u8>>|
         -> anyhow::Result<_> {
            let reference = observed
                .prepared_inputs
                .as_ref()
                .context("missing frozen collection")?;
            let collection_object = canonical_tokyo_oss_internal_object(
                "fixture collection",
                &reference.collection_url,
            )?;
            let bytes = source_objects
                .get(&collection_object)
                .context("unregistered HTTPS collection")?;
            let collection: CampaignPreparedInputsV1 = serde_json::from_slice(bytes)?;
            let mut blocks = std::collections::BTreeMap::new();
            for (sha, url) in &reference.block_urls {
                let object = canonical_tokyo_oss_internal_object("fixture block", url)?;
                let bytes = source_objects
                    .get(&object)
                    .context("unregistered HTTPS block")?;
                blocks.insert(sha.clone(), bytes.clone());
            }
            prepared_inputs::inspect_finalized_campaign_prepared_inputs(
                observed,
                expected_sha,
                collection,
                &mut AcquiredBlocks { bytes: blocks },
                1024 * 1024 * 1024,
            )
        };
        let signed_sha = hft_cex_research_input::sha256(&serialize_request(&signed)?);
        acquire(&signed, &signed_sha, objects)?;
        let mut corrupt = objects.clone();
        let block_uri = canonical
            .prepared_inputs
            .as_ref()
            .unwrap()
            .block_urls
            .values()
            .next()
            .unwrap();
        let bytes = corrupt
            .get_mut(block_uri)
            .context("missing declared corruption target")?;
        bytes[0] ^= 1;
        if acquire(&signed, &signed_sha, &corrupt).is_ok() {
            bail!("HTTPS native verifier accepted changed acquisition bytes");
        }
        let freeze_path = root.path().join("freeze.json");
        let signed_path = root.path().join("signed.json");
        let request_out = root.path().join("request.json");
        let submission_out = root.path().join("submission.json");
        hft_research_artifacts::write_json_atomic(&freeze_path, &frozen)?;
        hft_research_artifacts::write_json_atomic(&signed_path, &signed)?;
        finalize_with_native_readback(
            CampaignFinalizeArgs {
                freeze: freeze_path,
                signed_request: signed_path,
                attempt_id: "https-represented-fixture".into(),
                image: format!("registry/worker@sha256:{}", request.image_identity),
                request_out: request_out.clone(),
                submission_out: submission_out.clone(),
            },
            |observed, sha| acquire(observed, sha, objects),
        )?;
        let finalized_bytes = std::fs::read(&request_out)?;
        let finalized: CampaignRequest = serde_json::from_slice(&finalized_bytes)?;
        if canonicalize_request_transport(&finalized)? != canonical
            || finalized.campaign_id != expected_campaign_id(&canonical)?
            || hft_cex_research_input::sha256(&finalized_bytes) != signed_sha
        {
            bail!("finalized HTTPS fixture lost its exact canonical binding");
        }
        let submission: serde_json::Value =
            serde_json::from_slice(&std::fs::read(submission_out)?)?;
        if submission["request"] != serde_json::to_value(&finalized)? {
            bail!("HTTPS submission does not retain the full finalized request");
        }
        Ok(())
    }

    #[test]
    fn native_finalized_fixture_matches_real_body_and_rejects_changed_request() {
        let fixture = native_prepared_fixture_for_tests();
        assert_eq!(
            fixture.inputs.campaign_inputs_sha256(),
            fixture.request.campaign_inputs_sha256
        );
        assert_eq!(
            fixture.inputs.collection_id(),
            fixture
                .request
                .prepared_inputs
                .as_ref()
                .unwrap()
                .collection_sha256
        );
        assert!(fixture.materialization_path().is_file());
        assert_eq!(
            hft_research_artifacts::sha256_file(&fixture.original_receipt_path()).unwrap(),
            fixture
                .inputs
                .prepared()
                .manifest()
                .source
                .preparation_receipt_sha256
        );
        assert_eq!(
            hft_research_artifacts::sha256_file(&fixture.augmented_receipt_path()).unwrap(),
            fixture.request.campaign_inputs_sha256
        );
        let mut changed = fixture.request.clone();
        changed.declared_total_trials += 1;
        let result = prepared_inputs::inspect_finalized_campaign_prepared_inputs(
            &changed,
            fixture.inputs.request_sha256(),
            fixture.inputs.prepared().manifest().clone(),
            &mut hft_cex_research_input::prepared::AcquiredBlocks {
                bytes: Default::default(),
            },
            1024 * 1024 * 1024,
        );
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("admission/witness"));
    }

    #[test]
    fn execute_native_prepared_development_retains_exact_round_readbacks() {
        let fixture = native_prepared_fixture_for_tests();
        // Preparation is complete. Neither execution nor scientific ZIP
        // recovery may reopen the original files containing withheld data.
        for path in [
            &fixture._source._render_fixture.feature_path,
            &fixture._source._render_fixture.materialization_path,
            &fixture._source.replay_artifact_path,
            &fixture._source.replay_manifest_path,
        ] {
            std::fs::remove_file(path).unwrap();
        }
        let request_path = fixture._root.path().join("native-execute-request.json");
        let request_bytes = serialize_request(&fixture.request).unwrap();
        std::fs::write(&request_path, &request_bytes).unwrap();
        let work_dir = fixture._root.path().join("native-execute");
        execute(CampaignExecuteArgs {
            final_evaluation: false,
            final_trusted_keys: None,
            pre_holdout: true,
            work_dir: work_dir.clone(),
            campaign_id: fixture.request.campaign_id.clone(),
            image_identity: fixture.request.image_identity.clone(),
            request: request_path,
            request_sha256: hft_cex_research_input::sha256(&request_bytes),
        })
        .unwrap();

        let result: CampaignResultV1 = serde_json::from_slice(
            &std::fs::read(work_dir.join("campaign-result-readback.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(result.request_sha256, fixture.inputs.request_sha256());
        assert_eq!(result.rounds.len(), fixture.request.rounds.len());
        assert!(result.finalization.is_none());
        assert!(!fixture._source.global_claim_path.exists());
        for name in ["features.jsonl", "materialization.json", "replay.parquet"] {
            assert!(!work_dir.join("shared-inputs").join(name).exists());
        }
        for round in &fixture.request.rounds {
            let execute_dir = work_dir.join(format!("mission/{}/execute", round.round_id));
            crate::mission_runner::validate_native_campaign_result_binding(
                &execute_dir.join("results"),
                &fixture.inputs,
            )
            .unwrap();
            let report = recover_round_report(&work_dir, &fixture.request, round);
            let ledger = collect_round_ledger(&execute_dir, round, &report).unwrap();
            let published = result
                .rounds
                .iter()
                .find(|item| item.round_id == round.round_id)
                .unwrap();
            assert_eq!(
                serde_json::to_value(ledger).unwrap(),
                serde_json::to_value(published).unwrap()
            );
        }
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let (_, consumed, hash) = readback_pre_holdout_terminal(
            &client,
            &fixture.request,
            fixture.inputs.request_sha256(),
            fixture.inputs.evaluation_protocol_sha256(),
        )
        .unwrap();
        assert_eq!(consumed, result.consumed_trials as u64);
        assert_eq!(
            hash,
            hft_research_artifacts::sha256_file(&work_dir.join("campaign-result-readback.json"))
                .unwrap()
        );

        let round = &result.rounds[0];
        let binding = ExecutionBinding::Campaign {
            campaign_id: fixture.request.campaign_id.clone(),
            round_id: round.round_id.clone(),
            request_sha256: fixture.inputs.request_sha256().into(),
        };
        let source_zip = Path::new(&fixture.request.rounds[0].result_readback_url);
        let expected_native = Some((
            fixture.inputs.finalized_request(),
            fixture.inputs.request_sha256(),
        ));
        assert!(
            recover_execution_report_from_cached_result(
                source_zip,
                &round.result_bundle_sha256,
                &round.mission_id,
                &round.mission_sha256,
                &binding,
                None,
            )
            .is_err(),
            "a native collection cannot fall through the generic full-source verifier"
        );
        let block_entry = format!(
            "results/native-prepared-blocks/{}.mondaybin",
            fixture
                .inputs
                .prepared()
                .manifest()
                .features
                .manifest
                .blocks[0]
                .sha256
        );
        for mutation in 0..7 {
            let changed = fixture
                ._root
                .path()
                .join(format!("changed-native-{mutation}.zip"));
            let mut source = ZipArchive::new(File::open(source_zip).unwrap()).unwrap();
            let mut output = zip::ZipWriter::new(File::create(&changed).unwrap());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for index in 0..source.len() {
                let mut entry = source.by_index(index).unwrap();
                let name = entry.name().to_string();
                if mutation == 0 && name == block_entry {
                    continue;
                }
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                if mutation == 1 && name == block_entry {
                    bytes[0] ^= 1;
                }
                if mutation == 2 && name == "results/native-prepared-admission.json" {
                    let mut admission: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    admission["loaded_development_rows"] = serde_json::json!(
                        admission["loaded_development_rows"].as_u64().unwrap() + 1
                    );
                    bytes = serde_json::to_vec(&admission).unwrap();
                }
                if mutation == 4 && name == "results/feature-manifest.json" {
                    let mut metadata: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    metadata["symbol"] = serde_json::json!("FOREIGNUSDT");
                    bytes = serde_json::to_vec(&metadata).unwrap();
                }
                if mutation == 5 && name == "results/materialization.json" {
                    bytes.push(b' ');
                }
                if mutation == 6 && name == "results/cex-replay-dataset-manifest.json" {
                    let mut metadata: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    metadata["manifest_id"] = serde_json::json!("foreign-dataset");
                    bytes = serde_json::to_vec(&metadata).unwrap();
                }
                output.start_file(name, options).unwrap();
                output.write_all(&bytes).unwrap();
            }
            if mutation == 3 {
                output
                    .start_file("results/sealed-holdout-receipt.json", options)
                    .unwrap();
                output.write_all(b"{}").unwrap();
            }
            output.finish().unwrap();
            // Use the actual changed archive hash: rejection must come from
            // independently decoded scientific evidence, not a stale ZIP hash.
            assert!(
                recover_execution_report_from_cached_result(
                    &changed,
                    &hft_research_artifacts::sha256_file(&changed).unwrap(),
                    &round.mission_id,
                    &round.mission_sha256,
                    &binding,
                    expected_native,
                )
                .is_err(),
                "native evidence mutation {mutation} was accepted"
            );
        }
        platform_output::assert_publication(
            &LoadedRequest {
                request: fixture.request.clone(),
                sha256: fixture.inputs.request_sha256().into(),
            },
            &fixture.inputs,
            &result,
            &hash,
            &work_dir,
        )
        .unwrap();
    }

    #[test]
    fn native_collection_rebuilds_actual_rows_preserves_original_schedule_and_rejects_false_label()
    {
        use hft_cex_research_input::{
            campaign::NativeSourceBindingV1, campaign::SourceBuildRefV1, prepared::AcquiredBlocks,
        };
        let fixture = campaign_e2e_fixture("native-prepared-rows", false, false, true);
        let loaded = load_request(&fixture.args.request).unwrap();
        let render = PreparedCexInputs::load(
            &fixture._render_fixture.feature_path,
            &fixture._render_fixture.materialization_path,
            true,
        )
        .unwrap();
        let protocol = crate::mission_render::approved_evaluation_protocol_for_plan(
            render.materialization(),
            &loaded.request.research_plan,
        )
        .unwrap();
        let original_rows = render.native_source_rows(&protocol).unwrap();
        let partitions = protocol.row_partitions(original_rows.len()).unwrap();
        let end = original_rows[partitions
            .selection
            .as_ref()
            .map_or(partitions.sealed_holdout.start, |p| p.start)]
        .available_time
        .timestamp_micros()
            - 1;
        let canonical = || {
            hft_backtest::config::verify_canonical_replay_artifact(
                &fixture.replay_artifact_path,
                &fixture.replay_manifest_path,
                Some(&loaded.request.replay_artifact_sha256),
                &loaded.request.replay_manifest_sha256,
                None,
                Some(end),
            )
            .unwrap()
        };
        let source = NativeSourceBindingV1 {
            build: SourceBuildRefV1 {
                source_revision: loaded.request.producer_source_revision.clone(),
                image_identity: format!(
                    "registry/native@sha256:{}",
                    loaded.request.producer_image_identity
                ),
            },
            preparation_run_id: "native-source-test".into(),
            preparation_receipt_sha256: loaded.request.campaign_inputs_sha256.clone(),
            feature_sha256: loaded.request.feature_sha256.clone(),
            materialization_sha256: loaded.request.materialization_sha256.clone(),
            replay_artifact_sha256: loaded.request.replay_artifact_sha256.clone(),
            replay_manifest_sha256: loaded.request.replay_manifest_sha256.clone(),
        };
        let artifacts = prepared_inputs::export_trusted_source(
            source.clone(),
            original_rows.clone(),
            &protocol,
            canonical(),
        )
        .unwrap();
        let expected = artifacts.manifest.expected_native().unwrap();
        let verified = artifacts
            .manifest
            .verify(
                &artifacts.id,
                &expected,
                &mut AcquiredBlocks {
                    bytes: artifacts.blocks,
                },
                1024 * 1024 * 1024,
            )
            .unwrap();
        assert_eq!(verified.rows(), &original_rows[..partitions.search.end]);
        let native =
            alpha_engine::evaluation::prepare_native_campaign_dataset(&verified, &protocol)
                .unwrap();
        let original =
            alpha_engine::evaluation::prepare_dataset(original_rows.clone(), &protocol).unwrap();
        assert_eq!(
            native.engine_context().rows(),
            original.engine_context().rows()
        );
        assert_eq!(
            native.engine_context().folds(),
            original.engine_context().folds()
        );
        assert_eq!(
            native.withheld_metadata().unwrap().total_rows,
            original_rows.len()
        );
        assert_eq!(
            native.withheld_metadata().unwrap().holdout.original_rows,
            partitions.sealed_holdout
        );
        assert!(native.calendar_validation_rows().is_none());
        let mut false_rows = original_rows;
        false_rows[0].label += 0.01;
        assert!(
            prepared_inputs::export_trusted_source(source, false_rows, &protocol, canonical())
                .err()
                .unwrap()
                .to_string()
                .contains("label")
        );
    }

    #[test]
    fn finalize_preserves_frozen_campaign_identity() {
        let root = tempfile::tempdir().unwrap();
        let freeze_path = root.path().join("freeze.json");
        let request_out = root.path().join("request.json");
        let submission_out = root.path().join("submission.json");
        let canonical_request = canonicalize_request_transport(&valid_request()).unwrap();
        let frozen = FrozenCampaignPlan {
            preparation_authentication_tag: None,
            schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.to_string(),
            campaign_inputs_sha256: "a".repeat(64),
            signing_plan: signing_plan(&canonical_request).unwrap(),
            canonical_request: canonical_request.clone(),
        };
        hft_research_artifacts::write_json_atomic(&freeze_path, &frozen).unwrap();

        let mut signed = valid_request();
        signed.feature_url.push_str("?feature-signature=1");
        signed
            .materialization_url
            .push_str("?materialization-signature=1");
        signed.replay_artifact_url.push_str("?replay-signature=1");
        signed.replay_manifest_url.push_str("?manifest-signature=1");
        signed.holdout_claim_put_url.push_str("?claim-signature=1");
        signed
            .holdout_claim_readback_url
            .push_str("?claim-readback-signature=1");
        signed
            .campaign_result_put_url
            .push_str("?campaign-result-signature=1");
        signed
            .campaign_result_readback_url
            .push_str("?campaign-result-readback-signature=1");
        for round in &mut signed.rounds {
            round.mission_put_url.push_str("?mission-signature=1");
            round
                .mission_readback_url
                .push_str("?mission-readback-signature=1");
            round.result_put_url.push_str("?result-signature=1");
            round
                .result_readback_url
                .push_str("?result-readback-signature=1");
        }
        let signed_request_path = root.path().join("signed-request.json");
        hft_research_artifacts::write_json_atomic(&signed_request_path, &signed).unwrap();

        finalize(CampaignFinalizeArgs {
            freeze: freeze_path,
            signed_request: signed_request_path,
            attempt_id: "attempt-001".to_string(),
            image: format!("registry/research-runner@sha256:{}", "1".repeat(64)),
            request_out: request_out.clone(),
            submission_out: submission_out.clone(),
        })
        .unwrap();

        let finalized_request: CampaignRequest =
            serde_json::from_slice(&std::fs::read(&request_out).unwrap()).unwrap();
        assert_eq!(
            canonicalize_request_transport(&finalized_request).unwrap(),
            canonical_request
        );
        let submission: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&submission_out).unwrap()).unwrap();
        assert_eq!(
            submission["request"]["campaign_id"],
            serde_json::json!(finalized_request.campaign_id)
        );
        assert_eq!(
            submission["request"]["holdout_id"],
            serde_json::json!(finalized_request.holdout_id)
        );
    }

    #[test]
    fn build_request_from_parts_derives_campaign_id_and_global_holdout_claim() {
        let request = build_request_from_parts(
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/features.jsonl",
            &"1".repeat(64),
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/materialization.json",
            &"2".repeat(64),
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/replay.parquet",
            &"3".repeat(64),
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/replay-manifest.json",
            &"4".repeat(64),
            &"5".repeat(64),
            &"a".repeat(40),
            &"6".repeat(64),
            None,
            &CexCampaignResearchPlanV1::canonical(),
            BUILD_SOURCE_REVISION,
            &"1".repeat(64),
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns",
            "cex-holdout-test",
            &[7, 11],
            None,
        )
        .unwrap();

        assert_eq!(request.campaign_id, expected_campaign_id(&request).unwrap());
        assert_eq!(
            canonical_tokyo_oss_internal_object("holdout", &request.holdout_claim_put_url).unwrap(),
            cex_global_holdout_claim_object(&request.holdout_id).unwrap()
        );
        validate_request(&request).unwrap();
    }

    #[test]
    fn freeze_from_receipt_derives_identity_and_plan() {
        const TEST_ROOT: &str =
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research";
        let producer_revision = "b".repeat(40);
        let producer_image_ref = format!("registry/research-runner@sha256:{}", "1".repeat(64));
        let executor_image_ref = format!("registry/research-runner@sha256:{}", "2".repeat(64));
        let fixture = campaign_e2e_fixture("campaign-freeze", false, false, true);
        let root = tempfile::tempdir().unwrap();
        let input_root = root.path().join("remounted-run");
        std::fs::create_dir_all(&input_root).unwrap();
        let feature_relative = PathBuf::from("features.jsonl");
        let materialization_relative = PathBuf::from("materialization.json");
        let replay_artifact_relative: PathBuf =
            fixture.replay_artifact_path.file_name().unwrap().into();
        let replay_manifest_relative: PathBuf =
            fixture.replay_manifest_path.file_name().unwrap().into();
        std::fs::copy(
            &fixture._render_fixture.feature_path,
            input_root.join(&feature_relative),
        )
        .unwrap();
        std::fs::copy(
            &fixture._render_fixture.materialization_path,
            input_root.join(&materialization_relative),
        )
        .unwrap();
        std::fs::copy(
            &fixture.replay_artifact_path,
            input_root.join(&replay_artifact_relative),
        )
        .unwrap();
        std::fs::copy(
            &fixture.replay_manifest_path,
            input_root.join(&replay_manifest_relative),
        )
        .unwrap();
        let receipt_path = root.path().join("campaign-inputs.json");
        let receipt = CampaignInputsReceipt {
            prepared_inputs: None,
            schema_version: CAMPAIGN_INPUTS_SCHEMA_V1.to_string(),
            run_id: "20260819t000000z-1".to_string(),
            source_revision: producer_revision.clone(),
            image_ref: producer_image_ref.clone(),
            mission_id: "campaign-inputs-test".to_string(),
            market: "usdm".to_string(),
            symbol: "BTCUSDT".to_string(),
            output_prefix: "runs/campaign-freeze".to_string(),
            output_object_base_url: TEST_ROOT.to_string(),
            readback_scope: "same-mounted-ossfs-prefix".to_string(),
            feature: CampaignInputReceiptItem {
                relative_path: feature_relative.clone(),
                object_url: format!("{TEST_ROOT}/runs/campaign-freeze/features.jsonl"),
                sha256: hft_research_artifacts::sha256_file(&input_root.join(&feature_relative))
                    .unwrap(),
            },
            materialization: CampaignInputReceiptItem {
                relative_path: materialization_relative.clone(),
                object_url: format!("{TEST_ROOT}/runs/campaign-freeze/materialization.json"),
                sha256: hft_research_artifacts::sha256_file(
                    &input_root.join(&materialization_relative),
                )
                .unwrap(),
            },
            replay_artifact: CampaignInputReceiptItem {
                relative_path: replay_artifact_relative.clone(),
                object_url: format!(
                    "{TEST_ROOT}/runs/campaign-freeze/{}",
                    replay_artifact_relative.display()
                ),
                sha256: hft_research_artifacts::sha256_file(
                    &input_root.join(&replay_artifact_relative),
                )
                .unwrap(),
            },
            replay_manifest: CampaignInputReceiptItem {
                relative_path: replay_manifest_relative.clone(),
                object_url: format!(
                    "{TEST_ROOT}/runs/campaign-freeze/{}",
                    replay_manifest_relative.display()
                ),
                sha256: hft_research_artifacts::sha256_file(
                    &input_root.join(&replay_manifest_relative),
                )
                .unwrap(),
            },
        };
        hft_research_artifacts::write_json_atomic(&receipt_path, &receipt).unwrap();
        let output = root.path().join("freeze.json");
        let preparation_ledger = root.path().join("preparation.duckdb");
        drop(alpha_store::AlphaStore::open(&preparation_ledger).unwrap());

        freeze(CampaignFreezeArgs {
            stage_authority: None,
            preparation_ledger: Some(preparation_ledger.clone()),
            reuse: None,
            reuse_sha256: None,
            final_evaluation_control: None,
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: BUILD_SOURCE_REVISION.to_string(),
            image: executor_image_ref.clone(),
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
            research_plan: None,
            study_proposal: None,
            output: output.clone(),
        })
        .unwrap();

        let (receipt_again, receipt_sha256) = load_campaign_inputs_receipt(&receipt_path).unwrap();
        assert_eq!(receipt_again.source_revision, producer_revision);
        assert_eq!(receipt_again.image_ref, producer_image_ref);
        let frozen = load_freeze_plan(&output).unwrap();
        let prepared = frozen.canonical_request.prepared_inputs.as_ref().unwrap();
        assert_eq!(
            prepared.expected_native.source.preparation_receipt_sha256,
            receipt_sha256
        );
        let augmented = input_root.join(format!(
            "native-prepared/{}.inputs.json",
            frozen.campaign_inputs_sha256
        ));
        let (augmented_receipt, augmented_sha) = load_campaign_inputs_receipt(&augmented).unwrap();
        assert_eq!(augmented_receipt.prepared_inputs.as_ref(), Some(prepared));
        assert_eq!(frozen.campaign_inputs_sha256, augmented_sha);
        assert_eq!(
            frozen.canonical_request.campaign_inputs_sha256,
            augmented_sha
        );
        assert_ne!(augmented_sha, receipt_sha256);
        assert!(frozen.canonical_request.feature_url.is_empty());
        assert!(frozen.canonical_request.materialization_url.is_empty());
        assert!(frozen.canonical_request.replay_artifact_url.is_empty());
        assert!(frozen.canonical_request.replay_manifest_url.is_empty());
        assert!(frozen.signing_plan.actions.iter().all(|action| ![
            "feature_get",
            "materialization_get",
            "replay_artifact_get",
            "replay_manifest_get"
        ]
        .contains(&action.name.as_str())));

        assert_eq!(
            frozen.canonical_request.producer_source_revision,
            producer_revision
        );
        assert_eq!(
            frozen.canonical_request.producer_image_identity,
            mission_dispatch::image_digest(&producer_image_ref).unwrap()
        );
        assert_eq!(
            frozen.canonical_request.build_source_revision,
            BUILD_SOURCE_REVISION
        );
        assert_eq!(
            frozen.canonical_request.image_identity,
            mission_dispatch::image_digest(&executor_image_ref).unwrap()
        );
        assert_eq!(
            frozen.signing_plan,
            signing_plan(&frozen.canonical_request).unwrap()
        );

        let mut reuse = CampaignFreezeArgs {
            stage_authority: None,
            preparation_ledger: Some(preparation_ledger.clone()),
            reuse: Some(output.clone()),
            reuse_sha256: Some(hft_research_artifacts::sha256_file(&output).unwrap()),
            final_evaluation_control: None,
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: BUILD_SOURCE_REVISION.to_string(),
            image: executor_image_ref.clone(),
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
            research_plan: None,
            study_proposal: None,
            output: root.path().join("reused-freeze.json"),
        };
        let offline_inputs = root.path().join("offline-inputs");
        std::fs::rename(&input_root, &offline_inputs).unwrap();
        freeze(reuse.clone()).unwrap();
        assert_eq!(
            std::fs::read(&output).unwrap(),
            std::fs::read(&reuse.output).unwrap()
        );
        for mutation in 0..6 {
            let mut wrong = reuse.clone();
            match mutation {
                0 => wrong.reuse_sha256 = Some("0".repeat(64)),
                1 => wrong.reuse_sha256 = None,
                2 => wrong.source_revision = "b".repeat(40),
                3 => wrong.image = format!("registry/research@sha256:{}", "3".repeat(64)),
                4 => wrong.seeds = vec![7, 12],
                _ => wrong.campaign_root = format!("{TEST_ROOT}/other-campaigns"),
            }
            assert!(
                freeze_request(&wrong).is_err(),
                "reuse accepted changed binding {mutation}"
            );
        }
        let mut changed_plan = CexCampaignResearchPlanV1::canonical();
        changed_plan.hypothesis.push_str(" Revised hypothesis.");
        let changed_plan_path = root.path().join("changed-plan.json");
        hft_research_artifacts::write_json_atomic(&changed_plan_path, &changed_plan).unwrap();
        reuse.research_plan = Some(changed_plan_path);
        assert!(freeze_request(&reuse).is_err());
        let mut changed_receipt = receipt.clone();
        changed_receipt.feature.sha256 = "9".repeat(64);
        hft_research_artifacts::write_json_atomic(&receipt_path, &changed_receipt).unwrap();
        reuse.research_plan = None;
        assert!(freeze_request(&reuse).is_err());
        hft_research_artifacts::write_json_atomic(&receipt_path, &receipt).unwrap();
        std::fs::rename(&offline_inputs, &input_root).unwrap();

        // Exercise the fixed matrix preparation entrypoint with shared input
        // validation, completed-index reuse, and a changed parameter matrix.
        let mut base_plan = CexCampaignResearchPlanV1::canonical();
        let mut training = paired_mlp_plan_for_tests();
        training.updates = 4096;
        training.optimization = Some(alpha_domain::mlp_training::CexMlpOptimizationV1 {
            learning_rate: 0.0003,
            controls: hft_research_manifest::mlp_training::MlpOptimizationControlsV1::default(),
        });
        base_plan.mlp_training = Some(training);
        let matrix_path = root.path().join("matrix.json");
        let matrix_root = root.path().join("matrix-output");
        let mut matrix = serde_json::json!({
            "schema_version":"monday.cex_campaign_preparation_plan.v1",
            "source_revision":BUILD_SOURCE_REVISION,"image":executor_image_ref,
            "campaign_root":format!("{TEST_ROOT}/campaigns"),
            "campaign_inputs":{"path":receipt_path,"sha256":receipt_sha256},
            "input_root":input_root,"seeds":[7,11],"base_research_plan":base_plan,
            "members":[{"id":"short","mlp":{"updates":4096,"learning_rate":0.0003}},
                       {"id":"long","mlp":{"updates":8192,"learning_rate":0.0003}}]
        });
        hft_research_artifacts::write_json_atomic(&matrix_path, &matrix).unwrap();
        let prepare_matrix = || {
            preparation::prepare(crate::cli::CampaignPrepareArgs {
                ledger: preparation_ledger.clone(),
                plan: matrix_path.clone(),
                output_root: matrix_root.clone(),
            })
        };
        prepare_matrix().unwrap();
        let prepared_root = std::fs::read_dir(&matrix_root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let index_path = prepared_root.join("preparation.json");
        let first_index = std::fs::read(&index_path).unwrap();
        let index: serde_json::Value = serde_json::from_slice(&first_index).unwrap();
        assert_eq!(index["members"].as_array().unwrap().len(), 2);
        assert_ne!(
            index["members"][0]["campaign_id"],
            index["members"][1]["campaign_id"]
        );
        assert_eq!(index["seeds"], serde_json::json!(["7", "11"]));
        let aggregate_trials = index["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| member["declared_trials"].as_u64().unwrap())
            .sum::<u64>();
        for member in index["members"].as_array().unwrap() {
            let frozen =
                load_freeze_plan(&prepared_root.join(member["freeze"]["path"].as_str().unwrap()))
                    .unwrap();
            assert_eq!(
                frozen
                    .canonical_request
                    .research_plan
                    .comparison_family_trials,
                Some(aggregate_trials as usize)
            );
            assert!(frozen.canonical_request.declared_total_trials < aggregate_trials as usize);
        }
        let mut from_matrix = reuse.clone();
        from_matrix.reuse =
            Some(prepared_root.join(index["members"][0]["freeze"]["path"].as_str().unwrap()));
        from_matrix.reuse_sha256 = Some(
            index["members"][0]["freeze"]["sha256"]
                .as_str()
                .unwrap()
                .into(),
        );
        from_matrix.research_plan = Some(
            prepared_root.join(
                index["members"][0]["research_plan"]["path"]
                    .as_str()
                    .unwrap(),
            ),
        );
        from_matrix.output = root.path().join("native-from-matrix.json");
        freeze(from_matrix.clone()).unwrap();
        assert_eq!(
            std::fs::read(from_matrix.reuse.unwrap()).unwrap(),
            std::fs::read(from_matrix.output).unwrap()
        );
        std::fs::rename(&input_root, &offline_inputs).unwrap();
        prepare_matrix().unwrap();
        assert_eq!(std::fs::read(&index_path).unwrap(), first_index);
        // Inputs were committed, but the final index was not: resume without
        // touching the now-unavailable bulk input or replacing existing members.
        std::fs::remove_file(&index_path).unwrap();
        prepare_matrix().unwrap();
        assert_eq!(std::fs::read(&index_path).unwrap(), first_index);
        matrix["prepared_inputs"] = serde_json::json!({
            "path":prepared_root.join(index["prepared_inputs"]["path"].as_str().unwrap()),
            "sha256":index["prepared_inputs"]["sha256"],
        });
        matrix["members"][0]["mlp"]["updates"] = 8192.into();
        matrix["members"][1]["mlp"]["updates"] = 16384.into();
        hft_research_artifacts::write_json_atomic(&matrix_path, &matrix).unwrap();
        prepare_matrix().unwrap();
        assert_eq!(std::fs::read(&index_path).unwrap(), first_index);
        let second_root = std::fs::read_dir(&matrix_root)
            .unwrap()
            .map(|p| p.unwrap().path())
            .find(|p| *p != prepared_root)
            .unwrap();
        let second_index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(second_root.join("preparation.json")).unwrap())
                .unwrap();
        let cached_inputs =
            second_root.join(second_index["prepared_inputs"]["path"].as_str().unwrap());
        std::fs::write(&cached_inputs, b"{}\n").unwrap();
        assert!(prepare_matrix().is_err());
        std::fs::rename(&offline_inputs, &input_root).unwrap();

        for missing_later_seed in [true, false] {
            let mut plan = CexCampaignResearchPlanV1::canonical();
            let mut training = paired_mlp_plan_for_tests();
            if missing_later_seed {
                training.initializations.remove(&11);
            } else {
                training
                    .initializations
                    .get_mut(&11)
                    .unwrap()
                    .fold_seeds
                    .pop();
            }
            plan.mlp_training = Some(training);
            let plan_path = root
                .path()
                .join(format!("invalid-mlp-{missing_later_seed}.json"));
            std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
            let invalid_output = root
                .path()
                .join(format!("invalid-freeze-{missing_later_seed}.json"));
            let error = freeze(CampaignFreezeArgs {
                stage_authority: None,
                preparation_ledger: None,
                reuse: None,
                reuse_sha256: None,
                final_evaluation_control: None,
                campaign_inputs: receipt_path.clone(),
                input_root: input_root.clone(),
                source_revision: BUILD_SOURCE_REVISION.to_string(),
                image: executor_image_ref.clone(),
                campaign_root: format!("{TEST_ROOT}/campaigns"),
                seeds: vec![7, 11],
                research_plan: Some(plan_path),
                study_proposal: None,
                output: invalid_output.clone(),
            })
            .unwrap_err()
            .to_string();
            let expected = if missing_later_seed {
                "Campaign seed"
            } else {
                "fold count"
            };
            assert!(
                error.contains(expected),
                "unexpected freeze rejection: {error}"
            );
            assert!(
                !invalid_output.exists(),
                "invalid later rounds must not freeze an immutable request"
            );
        }

        for symbol in ["BTCUSDT", "SOLUSDT", "BNBUSDT"] {
            let mut admitted = receipt.clone();
            admitted.symbol = symbol.to_string();
            validate_campaign_inputs_receipt(&admitted).unwrap();
        }
        let mut unsupported = receipt.clone();
        unsupported.symbol = "ETHUSDT".to_string();
        assert!(validate_campaign_inputs_receipt(&unsupported).is_err());

        let mut wrong_symbol = receipt.clone();
        wrong_symbol.symbol = "SOLUSDT".to_string();
        hft_research_artifacts::write_json_atomic(&receipt_path, &wrong_symbol).unwrap();
        let mismatch = freeze_request(&CampaignFreezeArgs {
            stage_authority: None,
            preparation_ledger: None,
            reuse: None,
            reuse_sha256: None,
            final_evaluation_control: None,
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: BUILD_SOURCE_REVISION.to_string(),
            image: executor_image_ref.clone(),
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
            research_plan: None,
            study_proposal: None,
            output: root.path().join("wrong-symbol-freeze.json"),
        })
        .unwrap_err();
        assert!(mismatch
            .to_string()
            .contains("receipt instrument does not match"));
        hft_research_artifacts::write_json_atomic(&receipt_path, &receipt).unwrap();

        let mut invalid_producer = receipt.clone();
        invalid_producer.source_revision = "not-a-git-sha".to_string();
        assert!(validate_campaign_inputs_receipt(&invalid_producer)
            .unwrap_err()
            .to_string()
            .contains("source_revision must be an exact git revision"));

        let invalid_executor = freeze_request(&CampaignFreezeArgs {
            stage_authority: None,
            preparation_ledger: None,
            reuse: None,
            reuse_sha256: None,
            final_evaluation_control: None,
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: BUILD_SOURCE_REVISION.to_string(),
            image: "registry/research-runner:latest".to_string(),
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
            research_plan: None,
            study_proposal: None,
            output: root.path().join("invalid-freeze.json"),
        })
        .unwrap_err();
        assert!(invalid_executor
            .to_string()
            .contains("mission image must be pinned by @sha256 digest"));

        let replay_manifest_path = input_root.join(&replay_manifest_relative);
        let mut invalid_manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&replay_manifest_path).unwrap()).unwrap();
        invalid_manifest["artifact_path"] = serde_json::json!("wrong.parquet");
        hft_research_artifacts::write_json_atomic(&replay_manifest_path, &invalid_manifest)
            .unwrap();
        let mut invalid_replay_receipt = receipt.clone();
        invalid_replay_receipt.replay_manifest.sha256 =
            hft_research_artifacts::sha256_file(&replay_manifest_path).unwrap();
        hft_research_artifacts::write_json_atomic(&receipt_path, &invalid_replay_receipt).unwrap();
        let invalid_replay = freeze_request(&CampaignFreezeArgs {
            stage_authority: None,
            preparation_ledger: None,
            reuse: None,
            reuse_sha256: None,
            final_evaluation_control: None,
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: BUILD_SOURCE_REVISION.to_string(),
            image: executor_image_ref.clone(),
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
            research_plan: None,
            study_proposal: None,
            output: root.path().join("invalid-replay-freeze.json"),
        })
        .unwrap_err();
        assert!(invalid_replay
            .chain()
            .any(|cause| cause.to_string().contains("artifact")));

        let invalid_source = freeze_request(&CampaignFreezeArgs {
            stage_authority: None,
            preparation_ledger: None,
            reuse: None,
            reuse_sha256: None,
            final_evaluation_control: None,
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: "c".repeat(40),
            image: executor_image_ref,
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
            research_plan: None,
            study_proposal: None,
            output: root.path().join("invalid-source-freeze.json"),
        })
        .unwrap_err();
        assert!(invalid_source
            .to_string()
            .contains("campaign source revision does not match this build"));

        let mut sibling = receipt.clone();
        sibling.feature.object_url =
            format!("{TEST_ROOT}/runs/campaign-freeze-sibling/features.jsonl");
        assert!(validate_campaign_inputs_receipt(&sibling)
            .unwrap_err()
            .to_string()
            .contains("must live under the campaign inputs output root"));

        let mut escaped = receipt;
        escaped.feature.relative_path = PathBuf::from("../features.jsonl");
        assert!(validate_campaign_inputs_receipt(&escaped)
            .unwrap_err()
            .to_string()
            .contains("relative_path must be a safe relative path"));
    }

    #[test]
    fn finalize_rejects_signing_plan_drift() {
        let root = tempfile::tempdir().unwrap();
        let freeze_path = root.path().join("freeze.json");
        let canonical_request = canonicalize_request_transport(&valid_request()).unwrap();
        let mut signing_plan = signing_plan(&canonical_request).unwrap();
        signing_plan.actions[0].method = "PUT".to_string();
        let frozen = FrozenCampaignPlan {
            preparation_authentication_tag: None,
            schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.to_string(),
            campaign_inputs_sha256: "a".repeat(64),
            signing_plan,
            canonical_request: canonical_request.clone(),
        };
        hft_research_artifacts::write_json_atomic(&freeze_path, &frozen).unwrap();
        let signed_request_path = root.path().join("signed-request.json");
        hft_research_artifacts::write_json_atomic(&signed_request_path, &valid_request()).unwrap();

        let error = finalize(CampaignFinalizeArgs {
            freeze: freeze_path,
            signed_request: signed_request_path,
            attempt_id: "attempt-001".to_string(),
            image: format!("registry/research-runner@sha256:{}", "1".repeat(64)),
            request_out: root.path().join("request.json"),
            submission_out: root.path().join("submission.json"),
        })
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("signing plan drifted from the frozen execution plan"));
    }

    #[test]
    fn validate_request_rejects_a_campaign_root_scoped_claim() {
        let mut request = valid_request();
        let campaign_root = campaign_output_root(
            &canonical_tokyo_oss_internal_object("result", &request.campaign_result_put_url)
                .unwrap(),
        )
        .unwrap();
        request.holdout_claim_put_url = format!(
            "{campaign_root}/holdout-id-sha256={}/sealed-holdout-claim.json",
            hft_research_dispatch_io::sha256_text(&request.holdout_id)
        );
        request.holdout_claim_readback_url = request.holdout_claim_put_url.clone();

        let error = validate_request(&request).unwrap_err();

        assert!(error
            .to_string()
            .contains("global sealed holdout namespace"));
    }

    #[test]
    fn publish_round_mission_accepts_an_existing_identical_object() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("rendered-mission.json");
        let destination = root.path().join("published-mission.json");
        let readback = root.path().join("mission-readback.json");
        std::fs::write(&source, br#"{"mission":"same"}"#).unwrap();
        std::fs::write(&destination, br#"{"mission":"same"}"#).unwrap();

        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let mission_sha256 = publish_create_once_json(
            &client,
            "Mission",
            &destination.to_string_lossy(),
            &destination.to_string_lossy(),
            &source,
            &readback,
        )
        .unwrap();

        assert_eq!(
            mission_sha256,
            hft_research_artifacts::sha256_file(&destination).unwrap()
        );
        assert_eq!(
            hft_research_artifacts::sha256_file(&readback).unwrap(),
            mission_sha256
        );
    }

    #[test]
    fn publish_round_mission_rejects_an_existing_different_object() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("rendered-mission.json");
        let destination = root.path().join("published-mission.json");
        let readback = root.path().join("mission-readback.json");
        std::fs::write(&source, br#"{"mission":"new"}"#).unwrap();
        let mut file = std::fs::File::create(&destination).unwrap();
        file.write_all(br#"{"mission":"old"}"#).unwrap();
        file.flush().unwrap();

        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let error = publish_create_once_json(
            &client,
            "Mission",
            &destination.to_string_lossy(),
            &destination.to_string_lossy(),
            &source,
            &readback,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("published Mission already exists with different bytes"));
    }

    #[test]
    fn publish_campaign_result_accepts_an_existing_identical_object() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("campaign-result.json");
        let destination = root.path().join("published-campaign-result.json");
        let readback = root.path().join("campaign-result-readback.json");
        std::fs::write(&source, br#"{"campaign":"same"}"#).unwrap();
        std::fs::write(&destination, br#"{"campaign":"same"}"#).unwrap();

        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let result_sha256 = publish_create_once_json(
            &client,
            "campaign result",
            &destination.to_string_lossy(),
            &destination.to_string_lossy(),
            &source,
            &readback,
        )
        .unwrap();

        assert_eq!(
            result_sha256,
            hft_research_artifacts::sha256_file(&destination).unwrap()
        );
        assert_eq!(
            hft_research_artifacts::sha256_file(&readback).unwrap(),
            result_sha256
        );
    }

    #[test]
    fn publish_campaign_result_rejects_an_existing_different_object() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("campaign-result.json");
        let destination = root.path().join("published-campaign-result.json");
        let readback = root.path().join("campaign-result-readback.json");
        std::fs::write(&source, br#"{"campaign":"new"}"#).unwrap();
        let mut file = std::fs::File::create(&destination).unwrap();
        file.write_all(br#"{"campaign":"old"}"#).unwrap();
        file.flush().unwrap();

        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let error = publish_create_once_json(
            &client,
            "campaign result",
            &destination.to_string_lossy(),
            &destination.to_string_lossy(),
            &source,
            &readback,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("published campaign result already exists with different bytes"));
    }

    #[test]
    fn campaign_execute_without_pre_holdout_refuses_to_open_holdout() {
        let error = execute(CampaignExecuteArgs {
            final_evaluation: false,
            final_trusted_keys: None,
            pre_holdout: false,
            work_dir: PathBuf::from("/tmp/monday-campaign-execute-holdout-bypass"),
            campaign_id: "cex-campaign-1234567890abcdef1234567890abcdef".into(),
            image_identity: "a".repeat(64),
            request: PathBuf::from("/tmp/missing-campaign-request.json"),
            request_sha256: "b".repeat(64),
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("campaign-execute cannot open sealed holdout"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn plain_v5_request_cannot_start_a_worker_or_create_work_paths() {
        let root = tempfile::tempdir().unwrap();
        let request = valid_request();
        let path = root.path().join("legacy-request.json");
        let bytes = serialize_request(&request).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let work_dir = root.path().join("worker");
        let error = execute(CampaignExecuteArgs {
            final_evaluation: false,
            final_trusted_keys: None,
            pre_holdout: true,
            work_dir: work_dir.clone(),
            campaign_id: request.campaign_id,
            image_identity: request.image_identity,
            request: path,
            request_sha256: hft_cex_research_input::sha256(&bytes),
        })
        .unwrap_err();
        assert!(error.to_string().contains("frozen V6 prepared collection"));
        assert!(!work_dir.exists());
    }

    #[test]
    fn execute_rejects_prediction_edge_that_fails_actual_event_replay() {
        let mut fixture = campaign_e2e_fixture("campaign-e2e-positive", false, false, false);
        prepare_fixture_for_execute(&mut fixture).unwrap();
        let request = load_request(&fixture.args.request).unwrap().request;
        execute(fixture.args.clone()).unwrap();
        for round in &request.rounds {
            let recovered = recover_round_report(&fixture.work_dir, &request, round);
            let selection: CexSupervisedModelSelectionV1 = serde_json::from_slice(
                &std::fs::read(fixture.work_dir.join(format!(
                    "mission/{}/execute/results/supervised-model-selection.json",
                    round.round_id
                )))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                recovered.supervised_candidate_id.as_deref(),
                Some(selection.selected_candidate.id.as_str())
            );
            assert!(recovered.supervised_replay_receipt_id.is_some());
            assert_eq!(recovered.supervised_replay_gate_passed, Some(false));
        }
        let work_dir = fixture.work_dir;
        assert!(work_dir
            .join("shared-inputs/native-prepared-inputs.json")
            .exists());
        assert!(!work_dir.join("shared-inputs/features.jsonl").exists());
        assert!(!work_dir.join("shared-inputs/materialization.json").exists());
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(work_dir.join("campaign-result.json")).unwrap())
                .unwrap();
        assert_eq!(result["schema_version"], CAMPAIGN_RESULT_SCHEMA_V9);
        assert_eq!(
            result["campaign_inputs_sha256"],
            serde_json::json!(request.campaign_inputs_sha256)
        );
        assert_eq!(
            result["producer_source_revision"],
            serde_json::json!(request.producer_source_revision)
        );
        assert_eq!(
            result["producer_image_identity"],
            serde_json::json!(request.producer_image_identity)
        );
        let declared_total_trials =
            declared_total_trials_for_rounds(&request.research_plan, 2).unwrap();
        assert_eq!(result["rounds"].as_array().unwrap().len(), 2);
        assert_eq!(result["declared_total_trials"], declared_total_trials);
        let observed_consumed_trials: usize = result["rounds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|round| {
                round["feedback"]["factor_attempts"]
                    .as_u64()
                    .unwrap()
                    .saturating_add(round["feedback"]["model_attempts"].as_u64().unwrap())
            })
            .map(|value| usize::try_from(value).unwrap())
            .sum();
        assert_eq!(
            result["consumed_trials"],
            serde_json::json!(observed_consumed_trials)
        );
        for round in ["r1", "r2"] {
            let results = work_dir.join(format!("mission/{round}/execute/results"));
            assert!(results.join("factor-bank.json").exists());
            assert!(results.join("supervised-model-selection.json").exists());
            assert!(results.join("burn-mlp-baseline.json").exists());
            assert!(results.join("burn_mlp-supervised-candidate.json").exists());
            assert!(results
                .join("supervised-event-replay-receipt.json")
                .exists());
            assert!(!results.join("factor-subset-mcts-result.json").exists());
            assert!(!results.join("cex-event-replay-receipt.json").exists());
        }
        assert_eq!(result["termination_reason"], "campaign_no_candidate");
        assert!(result["rounds"].as_array().unwrap().iter().all(|round| {
            round["termination_reason"] == "supervised_replay_gate_failed"
                && round["supervised_replay_gate_passed"] == false
                && round["feedback"]["supervised_replay"]["mean_net_return"]
                    .as_f64()
                    .unwrap()
                    <= 0.0
        }));
        assert!(result["selected_round_id"].is_null());
        assert!(result["finalization"].is_null());
        assert!(!fixture.global_claim_path.exists());
    }

    #[test]
    fn execute_ridge_only_holding_retains_precheck_and_one_model_through_readback() {
        assert_ridge_holding_campaign(
            campaign_e2e_fixture("ridge-only-holding", false, false, true),
            false,
        );
    }

    #[test]
    fn execute_ridge_only_holding_zero_trades_is_a_completed_negative() {
        assert_ridge_holding_campaign(
            campaign_e2e_fixture_with_price_step(
                "ridge-only-zero-trades",
                false,
                false,
                true,
                false,
                0.00001,
            ),
            true,
        );
    }

    #[test]
    fn calendar_h1_negative_cannot_borrow_a_withheld_future_mark() {
        assert_calendar_h1_readback(true);
    }

    #[test]
    fn calendar_h1_positive_cannot_borrow_a_withheld_future_mark() {
        assert_calendar_h1_readback(false);
    }

    pub(crate) fn native_prepared_calendar_fixture_for_tests(
        negative: bool,
    ) -> NativePreparedFixture {
        let source_fixture = calendar_source_fixture(negative);
        let (request, inputs, root) = prepare_native_source_fixture(&source_fixture).unwrap();
        NativePreparedFixture {
            request,
            inputs,
            _source: source_fixture,
            _root: root,
        }
    }

    fn assert_calendar_h1_readback(negative: bool) {
        let fixture = native_prepared_calendar_fixture_for_tests(negative);
        let protocol = crate::mission_render::approved_evaluation_protocol_for_plan(
            fixture.inputs.render_inputs().materialization(),
            &fixture.request.research_plan,
        )
        .unwrap();
        let parts = protocol
            .row_partitions(fixture.inputs.prepared().original_metadata().total_rows)
            .unwrap();
        assert!(protocol.calendar.as_ref().unwrap().develop_end_row > parts.search.end);
        assert_eq!(
            fixture.inputs.prepared().original_metadata().visible_rows,
            parts.search
        );
        assert_eq!(fixture.inputs.prepared().rows().len(), parts.search.len());
        let dataset = alpha_engine::evaluation::prepare_native_campaign_dataset(
            fixture.inputs.prepared(),
            &protocol,
        )
        .unwrap();
        assert_eq!(dataset.proposal_context().row_count(), parts.search.len());
        assert_eq!(dataset.plan().folds.len(), 3);
        assert!(dataset.calendar_validation_rows().is_none());
        prepared_inputs::validate_signed_planning_metadata(&fixture.request).unwrap();
        prepared_inputs::validate_planning_projection_metadata(
            fixture.inputs.prepared().manifest(),
            &fixture.request,
        )
        .unwrap();
        assert!(!fixture._source.work_dir.exists());
        assert!(!fixture._source.global_claim_path.exists());
    }

    fn calendar_source_fixture(negative: bool) -> CampaignE2eFixture {
        let render_fixture = mission_render::tests::Fixture::new(28_795);
        let mut rows = mission_render::tests::read_feature_rows(&render_fixture.feature_path);
        for (index, row) in rows.iter_mut().enumerate() {
            row.features.insert(
                alpha_domain::CEX_RESEARCH_AGGREGATE_TRADE_FLOW_IMBALANCE_FIELD.into(),
                if (index / 100).is_multiple_of(2) {
                    1.0
                } else {
                    -1.0
                },
            );
        }
        mission_render::tests::rewrite_feature_rows(&render_fixture.feature_path, &rows);
        let mut fixture = campaign_e2e_fixture_with_input(
            if negative {
                "h1-calendar-negative"
            } else {
                "h1-calendar-positive"
            },
            false,
            false,
            true,
            false,
            if negative { 0.00001 } else { 0.0005 },
            render_fixture,
        );
        let rows = mission_render::tests::read_feature_rows(&fixture._render_fixture.feature_path);
        let start = rows[0].feature_available_time;
        let mut request: CampaignRequest =
            serde_json::from_slice(&std::fs::read(&fixture.args.request).unwrap()).unwrap();
        let plan = &mut request.research_plan;
        plan.supervised_model_scope = alpha_domain::CexSupervisedModelScopeV1::RidgeOnly;
        plan.holding = Some(hft_research_manifest::model::HorizonHoldingPolicyV1 {
            horizon_millis: 5000,
        });
        plan.label_horizon =
            Some(alpha_domain::campaign_horizon::CampaignLabelHorizonV1::canonical());
        plan.comparison_family_trials = Some(138);
        plan.calendar = Some(alpha_domain::EvaluationCalendarV1 {
            start,
            develop_end: start + chrono::TimeDelta::hours(4),
            validation_end: start + chrono::TimeDelta::hours(6),
            end: start + chrono::TimeDelta::hours(8),
        });
        let inputs = PreparedCexInputs::load(
            &fixture._render_fixture.feature_path,
            &fixture._render_fixture.materialization_path,
            true,
        )
        .unwrap();
        plan.development_precheck = Some(inputs.development_precheck(plan).unwrap());
        request.holdout_id = render_prepared_cex_bundle(&inputs, plan, 7, 46)
            .unwrap()
            .mission
            .spec
            .holdout
            .holdout_id;
        request.declared_total_trials = declared_total_trials_for_rounds(plan, 2).unwrap();
        assert_eq!(request.declared_total_trials, 46);
        std::fs::write(&fixture.args.request, serde_json::to_vec(&request).unwrap()).unwrap();
        fixture.args.request_sha256 =
            hft_research_artifacts::sha256_file(&fixture.args.request).unwrap();
        fixture
    }

    fn assert_ridge_holding_campaign(mut fixture: CampaignE2eFixture, negative: bool) {
        let mut request: CampaignRequest =
            serde_json::from_slice(&std::fs::read(&fixture.args.request).unwrap()).unwrap();
        request.research_plan.supervised_model_scope =
            alpha_domain::CexSupervisedModelScopeV1::RidgeOnly;
        request.research_plan.holding =
            Some(hft_research_manifest::model::HorizonHoldingPolicyV1 {
                horizon_millis: 5000,
            });
        request.declared_total_trials =
            declared_total_trials_for_rounds(&request.research_plan, request.rounds.len()).unwrap();
        request.research_plan.comparison_family_trials = Some(request.declared_total_trials * 3);
        std::fs::write(&fixture.args.request, serde_json::to_vec(&request).unwrap()).unwrap();
        fixture.args.request_sha256 =
            hft_research_artifacts::sha256_file(&fixture.args.request).unwrap();
        prepare_fixture_for_execute(&mut fixture).unwrap();
        execute(fixture.args.clone()).unwrap();
        let loaded = load_request(&fixture.args.request).unwrap();
        let materialization = crate::mission_runner::decode_materialization(
            &std::fs::read(&fixture._render_fixture.materialization_path).unwrap(),
        )
        .unwrap();
        let protocol = crate::mission_render::approved_evaluation_protocol(&materialization)
            .unwrap()
            .content_hash()
            .unwrap();
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let (_, trials, _) =
            readback_pre_holdout_terminal(&client, &loaded.request, &loaded.sha256, &protocol)
                .unwrap();
        assert!(trials <= u64::try_from(request.declared_total_trials).unwrap());
        assert!(!fixture.global_claim_path.exists());
        if negative {
            let result: CampaignResultV1 =
                load_campaign_result(&fixture.work_dir.join("campaign-result.json")).unwrap();
            validate_negative_campaign_result(
                &loaded,
                &result,
                &hft_research_artifacts::sha256_file(
                    &fixture.work_dir.join("campaign-result.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert!(result.rounds.iter().all(|r| r.feedback.accepted_factors > 0
                && r.feedback.supervised_ridge.as_ref().unwrap().trade_count == 0));
            for round in &result.rounds {
                let selected = round.feedback.supervised_selected.as_ref().unwrap();
                let proof = round
                    .feedback
                    .supervised_selected_evaluation_proof
                    .as_ref()
                    .unwrap();
                let facts = proof.screening_facts(selected).unwrap();
                assert!(facts.predictive_passed && facts.coverage_passed);
                assert_eq!(selected.trade_count, 0);
            }
            assert_eq!(
                classify_campaign_failure(&result).unwrap(),
                CexCampaignFailureClassV1::NoTradesAfterCosts,
            );
            // The qualified diagnosis does not permit a fixed comparison to change policy.
            assert!(next_campaign_policy_revision(
                &loaded,
                &hft_research_artifacts::sha256_file(
                    &fixture.work_dir.join("campaign-result.json")
                )
                .unwrap(),
                CexCampaignFailureClassV1::NoTradesAfterCosts,
            )
            .unwrap_err()
            .to_string()
            .contains("no automatic follow-up"));
        } else {
            for model in ["cart", "burn_mlp"] {
                assert_metric_bundle_rejected(
                    &loaded,
                    &protocol,
                    &client,
                    |entries| {
                        entries.insert(
                            format!("results/{model}-supervised-backtest.json"),
                            b"{}".to_vec(),
                        );
                    },
                    "outside its declared scope",
                );
            }
        }
        for round in &loaded.request.rounds {
            let file = std::fs::File::open(&round.result_readback_url).unwrap();
            let mut archive = zip::ZipArchive::new(file).unwrap();
            assert!(archive.by_name("results/ridge-baseline.json").is_ok());
            assert!(archive.by_name("results/cart-baseline.json").is_err());
            assert!(archive.by_name("results/burn-mlp-baseline.json").is_err());
            let attempts: serde_json::Value = serde_json::from_reader(
                archive
                    .by_name("results/supervised-model-attempts.json")
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(attempts["attempts"].as_array().unwrap().len(), 1);
            let precheck: alpha_engine::label_precheck::LabelSpacePrecheckV1 =
                serde_json::from_reader(
                    archive
                        .by_name("results/label-space-precheck.json")
                        .unwrap(),
                )
                .unwrap();
            assert!(!precheck.cancels_experiment);
            let model: alpha_engine::baselines::CexSupervisedModelEvaluationV2 =
                serde_json::from_reader(
                    archive
                        .by_name("results/ridge-supervised-backtest.json")
                        .unwrap(),
                )
                .unwrap();
            assert_eq!(
                model.report.return_accounting,
                alpha_domain::ReturnAccountingBasis::HeldQuantityWithQuotedEntryExit
            );
            assert_eq!(
                model
                    .candidate
                    .evaluation
                    .formula_config()
                    .unwrap()
                    .multiple_testing_trials,
                request.declared_total_trials * 3
            );
        }
    }

    #[test]
    fn execute_keeps_profitable_ml_replay_without_legacy_finalization() {
        let mut fixture = campaign_e2e_fixture("campaign-e2e-ml-positive", false, false, true);
        prepare_fixture_for_execute(&mut fixture).unwrap();
        execute(fixture.args.clone()).unwrap();

        let result: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.work_dir.join("campaign-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(result["schema_version"], CAMPAIGN_RESULT_SCHEMA_V9);
        assert_eq!(
            result["termination_reason"],
            "campaign_selected_pre_holdout"
        );
        assert!(result["selected_round_id"].is_string());
        assert!(result["selected_candidate_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("cex-supervised-model-candidate-")));
        assert!(result["finalization"].is_null());
        assert!(!fixture.global_claim_path.exists());
        let loaded = load_request(&fixture.args.request).unwrap();
        let materialization = crate::mission_runner::decode_materialization(
            &std::fs::read(&fixture._render_fixture.materialization_path).unwrap(),
        )
        .unwrap();
        let protocol = crate::mission_render::approved_evaluation_protocol(&materialization)
            .unwrap()
            .content_hash()
            .unwrap();
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let (outcome, trials, hash) =
            readback_pre_holdout_terminal(&client, &loaded.request, &loaded.sha256, &protocol)
                .unwrap();
        assert_eq!(
            outcome,
            alpha_domain::campaign_control::CampaignAttemptOutcomeV1::SelectedPreHoldout
        );
        assert_eq!(trials, result["consumed_trials"].as_u64().unwrap());
        assert_eq!(
            hash,
            hft_research_artifacts::sha256_file(&fixture.work_dir.join("campaign-result.json"))
                .unwrap()
        );

        assert!(result["rounds"].as_array().unwrap().iter().all(|round| {
            round["termination_reason"] == "supervised_pre_holdout_candidate_kept"
                && round["supervised_replay_gate_passed"] == true
                && round["feedback"]["supervised_replay"]["mean_net_return"]
                    .as_f64()
                    .is_some_and(|value| value > 0.0)
                && round["feedback"]["supervised_replay"]["net_sharpe"]
                    .as_f64()
                    .is_some_and(|value| value >= 2.0)
        }));
        for round in ["r1", "r2"] {
            let results = fixture
                .work_dir
                .join(format!("mission/{round}/execute/results"));
            assert!(
                results.join("supervised-model-metrics.json").exists(),
                "each supervised round must publish a unified comparison report"
            );
            assert!(results.join("supervised-model-metrics.csv").exists());
            let metrics: alpha_engine::model_metrics::CexModelMetricsReportV1 =
                serde_json::from_slice(
                    &std::fs::read(results.join("supervised-model-metrics.json")).unwrap(),
                )
                .unwrap();
            assert_eq!(metrics.model_count(), 3);
            assert_eq!(metrics.groups.len(), 1);
            assert_eq!(
                std::fs::read(results.join("supervised-model-metrics.csv")).unwrap(),
                metrics.to_csv().as_bytes(),
            );
            let original_ridge: alpha_engine::baselines::CexSupervisedModelEvaluationV2 =
                serde_json::from_slice(
                    &std::fs::read(results.join("ridge-supervised-backtest.json")).unwrap(),
                )
                .unwrap();
            let ridge_metrics = metrics.groups[0]
                .models
                .iter()
                .find(|model| model.model_kind == alpha_domain::CexBaselineModelKindV1::Ridge)
                .unwrap();
            assert_eq!(
                ridge_metrics.native,
                original_ridge.report.evaluation.metrics
            );
            assert_eq!(
                metrics.groups[0].cohort.evaluation_protocol.metrics,
                original_ridge
                    .candidate
                    .evaluation
                    .evaluation_protocol
                    .as_ref()
                    .unwrap()
                    .metrics,
            );
            assert_eq!(ridge_metrics.complete_utc_days, 0);
            assert!(ridge_metrics.daily_net_sharpe.value().is_none());
            let mut changed_ledger = original_ridge.clone();
            changed_ledger.report.ledger[0].net_return += 0.001;
            let error = alpha_engine::model_metrics::summarize_model_evaluation(
                &changed_ledger,
                &"f".repeat(64),
            )
            .err()
            .expect("a derived report must reject inconsistent source accounting");
            assert!(error.contains("ledger accounting"));
            assert!(!results.join("factor-subset-mcts-result.json").exists());
            assert!(!results.join("cex-event-replay-receipt.json").exists());
            assert!(!results.join("finalization-report.json").exists());
        }

        assert_metric_bundle_rejected(
            &loaded,
            &protocol,
            &client,
            |entries| {
                let mut forged: alpha_engine::model_metrics::CexModelMetricsReportV1 =
                    serde_json::from_slice(&entries["results/supervised-model-metrics.json"])
                        .unwrap();
                forged.groups[0].models[0]
                    .native
                    .predictive
                    .time_series_icir = Some(999.0);
                forged.report_id.clear();
                forged.report_id = format!(
                    "cex-model-metrics-{}",
                    canonical_json_hash(&forged).unwrap()
                );
                entries.insert(
                    "results/supervised-model-metrics.json".into(),
                    serde_json::to_vec_pretty(&forged).unwrap(),
                );
                entries.insert(
                    "results/supervised-model-metrics.csv".into(),
                    forged.to_csv().into_bytes(),
                );
            },
            "published model metrics differ",
        );
        assert_metric_bundle_rejected(
            &loaded,
            &protocol,
            &client,
            |entries| {
                entries.remove("results/supervised-model-metrics.json");
                entries.remove("results/supervised-model-metrics.csv");
            },
            "published model metrics JSON is missing",
        );
        assert_metric_bundle_rejected(
            &loaded,
            &protocol,
            &client,
            |entries| {
                for name in crate::mission_runner::CEX_SUPERVISED_MODEL_NAMES {
                    let path = format!("results/{name}-supervised-backtest.json");
                    let mut evaluation: alpha_engine::baselines::CexSupervisedModelEvaluationV2 =
                        serde_json::from_slice(&entries[&path]).unwrap();
                    for point in &mut evaluation.report.ledger {
                        point.available_time += chrono::Duration::days(1);
                    }
                    entries.insert(path, serde_json::to_vec_pretty(&evaluation).unwrap());
                }
                regenerate_metric_entries(entries);
            },
            "model ledger differs from the admitted feature data",
        );
        assert_metric_bundle_rejected(
            &loaded,
            &protocol,
            &client,
            |entries| {
                let path = "results/ridge-supervised-backtest.json";
                let mut evaluation: alpha_engine::baselines::CexSupervisedModelEvaluationV2 =
                    serde_json::from_slice(&entries[path]).unwrap();
                evaluation.report.ledger[0].net_return += 0.001;
                evaluation.report.ledger[0].gross_return += 0.001;
                evaluation.report.ledger[1].net_return -= 0.001;
                evaluation.report.ledger[1].gross_return -= 0.001;
                let mut equity = 1.0;
                for point in &mut evaluation.report.ledger {
                    equity += point.net_return;
                    point.equity = equity;
                }
                entries.insert(path.into(), serde_json::to_vec_pretty(&evaluation).unwrap());
                regenerate_metric_entries(entries);
            },
            "model ledger differs from the admitted feature data",
        );
        assert_metric_bundle_rejected(
            &loaded,
            &protocol,
            &client,
            |entries| {
                let path = entries
                    .keys()
                    .find(|name| {
                        name.starts_with("results/native-prepared-blocks/")
                            && name.ends_with(".mondaybin")
                    })
                    .unwrap()
                    .clone();
                entries.get_mut(&path).unwrap().push(b' ');
            },
            "native archive changed a declared block length",
        );

        assert_cached_terminal_reporting(&fixture, &loaded, &protocol, &client, &hash);

        assert_development_request_requires_withheld_inputs(&fixture, loaded, hash);
    }

    #[test]
    fn execute_retains_paired_mlp_training_diagnostics() {
        let mut control =
            campaign_e2e_fixture("campaign-e2e-ml-profile-control", false, false, true);
        prepare_fixture_for_execute(&mut control).unwrap();
        execute(control.args.clone()).unwrap();
        assert_paired_training_roundtrip(&control);
    }

    fn assert_paired_training_roundtrip(control: &CampaignE2eFixture) {
        use alpha_domain::mlp_training::{CexMlpInitializationV1, CexMlpTrainingPlanV1};
        use alpha_domain::{CexBaselineArtifactV1, CexBaselineModelV1};
        use hft_research_manifest::mlp_training::MlpTargetScaleV1;
        let mut initializations = std::collections::BTreeMap::new();
        let mut control_initial = std::collections::BTreeMap::new();
        let control_request = load_request(&control.args.request).unwrap();
        for (round, seed) in [("r1", 7), ("r2", 11)] {
            let request = control_request
                .request
                .rounds
                .iter()
                .find(|entry| entry.round_id == round)
                .unwrap();
            let bytes = std::fs::read(&request.result_readback_url).unwrap();
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
            let baseline: CexBaselineArtifactV1 =
                serde_json::from_reader(archive.by_name("results/burn-mlp-baseline.json").unwrap())
                    .unwrap();
            let bank: alpha_domain::CexFactorBankRevisionV2 =
                serde_json::from_reader(archive.by_name("results/factor-bank.json").unwrap())
                    .unwrap();
            let mut fold_seeds = Vec::new();
            for fold in &baseline.folds {
                let CexBaselineModelV1::BurnMlpPortableV2 {
                    seed: actual,
                    learning,
                    ..
                } = &fold.model
                else {
                    unreachable!()
                };
                fold_seeds.push(*actual);
                control_initial.insert(
                    (round.to_string(), fold.fold_index),
                    learning.initial_parameters_sha256.clone(),
                );
            }
            initializations.insert(
                seed,
                CexMlpInitializationV1 {
                    fold_seeds,
                    expected_factor_ids: baseline.factor_ids,
                    expected_factor_columns_sha256:
                        alpha_domain::mlp_training::factor_columns_sha256(&bank.entries).unwrap(),
                },
            );
        }
        let mut fixture =
            campaign_e2e_fixture("campaign-e2e-ml-training-profile", false, false, true);
        let mut request: CampaignRequest =
            serde_json::from_slice(&std::fs::read(&fixture.args.request).unwrap()).unwrap();
        request.research_plan.mlp_training = Some(CexMlpTrainingPlanV1 {
            schema_version: "cex-mlp-training-plan-v1".into(),
            updates: 64,
            target_scale: MlpTargetScaleV1::TrainStandardized,
            optimization: None,
            initializations,
        });
        std::fs::write(&fixture.args.request, serde_json::to_vec(&request).unwrap()).unwrap();
        fixture.args.request_sha256 =
            hft_research_artifacts::sha256_file(&fixture.args.request).unwrap();
        prepare_fixture_for_execute(&mut fixture).unwrap();
        execute(fixture.args.clone()).unwrap();
        let loaded = load_request(&fixture.args.request).unwrap();
        let materialization = crate::mission_runner::decode_materialization(
            &std::fs::read(&fixture._render_fixture.materialization_path).unwrap(),
        )
        .unwrap();
        let protocol = crate::mission_render::approved_evaluation_protocol(&materialization)
            .unwrap()
            .content_hash()
            .unwrap();
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        readback_pre_holdout_terminal(&client, &loaded.request, &loaded.sha256, &protocol).unwrap();
        assert!(!fixture.global_claim_path.exists());
        for round in ["r1", "r2"] {
            let request = loaded
                .request
                .rounds
                .iter()
                .find(|entry| entry.round_id == round)
                .unwrap();
            let bytes = std::fs::read(&request.result_readback_url).unwrap();
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
            let baseline: CexBaselineArtifactV1 =
                serde_json::from_reader(archive.by_name("results/burn-mlp-baseline.json").unwrap())
                    .unwrap();
            let bank: alpha_domain::CexFactorBankRevisionV2 =
                serde_json::from_reader(archive.by_name("results/factor-bank.json").unwrap())
                    .unwrap();
            let profile = baseline.baseline_policy.mlp_training.as_ref().unwrap();
            profile.validate_factor_bank(&bank).unwrap();
            let mut changed = bank;
            changed.entries[0].orientation = match changed.entries[0].orientation {
                alpha_domain::CexFactorOrientationV1::Positive => {
                    alpha_domain::CexFactorOrientationV1::Negative
                }
                alpha_domain::CexFactorOrientationV1::Negative => {
                    alpha_domain::CexFactorOrientationV1::Positive
                }
            };
            assert!(profile
                .validate_factor_bank(&changed)
                .unwrap_err()
                .contains("orientations"));
            assert_eq!(
                baseline.baseline_policy.schema_version,
                alpha_domain::CEX_BASELINE_POLICY_SCHEMA_V3
            );
            let mut wrong_seed = baseline.clone();
            let CexBaselineModelV1::BurnMlpPortableV2 { seed, .. } = &mut wrong_seed.folds[0].model
            else {
                unreachable!()
            };
            *seed = seed.wrapping_add(1);
            wrong_seed.artifact_id.clear();
            wrong_seed.artifact_id = format!(
                "cex-baseline-artifact-{}",
                alpha_domain::canonical_json_hash(&wrong_seed).unwrap()
            );
            assert!(
                wrong_seed.validate().is_err(),
                "rebinding an artifact cannot change its declared paired seed"
            );
            for fold in baseline.folds {
                let CexBaselineModelV1::BurnMlpPortableV2 {
                    epochs, learning, ..
                } = fold.model
                else {
                    unreachable!()
                };
                assert_eq!(epochs, 64);
                assert_eq!(learning.updates_completed, 64);
                assert_eq!(
                    learning.target_transform.mode,
                    MlpTargetScaleV1::TrainStandardized
                );
                assert_eq!(
                    learning.initial_parameters_sha256,
                    control_initial[&(round.to_string(), fold.fold_index)]
                );
                assert_eq!(
                    fold.mlp_observation
                        .unwrap()
                        .validation_prediction
                        .row_count,
                    fold.predictions.len()
                );
            }
        }
    }

    #[test]
    fn terminal_cache_requires_bounded_regular_files() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        assert!(validate_terminal_cache(&cache).is_err());
        std::fs::create_dir_all(cache.join("round-readback")).unwrap();
        validate_terminal_cache(&cache).unwrap();
        let artifact = cache.join("artifact");
        std::fs::write(&artifact, b"bound evidence").unwrap();
        let digest = hft_research_artifacts::sha256_file(&artifact).unwrap();
        verify_cached_terminal_file(&artifact, &digest, 14).unwrap();
        assert!(verify_cached_terminal_file(&artifact, &digest, 13).is_err());
        std::fs::write(&artifact, b"other evidence").unwrap();
        assert!(verify_cached_terminal_file(&artifact, &digest, 14).is_err());
        #[cfg(unix)]
        {
            let linked = cache.join("linked");
            std::os::unix::fs::symlink(&artifact, &linked).unwrap();
            let actual = hft_research_artifacts::sha256_file(&artifact).unwrap();
            assert!(verify_cached_terminal_file(&linked, &actual, 14).is_err());
            let linked_cache = root.path().join("linked-cache");
            std::os::unix::fs::symlink(&cache, &linked_cache).unwrap();
            assert!(validate_terminal_cache(&linked_cache).is_err());
        }
    }

    fn assert_cached_terminal_reporting(
        fixture: &CampaignE2eFixture,
        loaded: &LoadedRequest,
        protocol: &str,
        client: &Client,
        result_sha256: &str,
    ) {
        let cache = fixture.work_dir.join("ack-cache");
        std::fs::create_dir_all(cache.join("round-readback")).unwrap();
        std::fs::copy(
            &loaded.request.campaign_result_readback_url,
            cache.join("campaign-result.json"),
        )
        .unwrap();
        let mut originals = Vec::new();
        for (index, round) in loaded.request.rounds.iter().enumerate() {
            for (url, suffix) in [
                (&round.mission_readback_url, "mission.json"),
                (&round.result_readback_url, "results.zip"),
            ] {
                let destination = cache
                    .join("round-readback")
                    .join(format!("round-{index}-{suffix}"));
                std::fs::copy(url, destination).unwrap();
                let original = PathBuf::from(url);
                let backup = original.with_extension(format!("cache-test-{index}-{suffix}"));
                std::fs::rename(&original, &backup).unwrap();
                originals.push((original, backup));
            }
        }
        // The published large objects are unavailable. The bound ACK cache must
        // be sufficient; accidental duplicate network/file fetches fail here.
        let (_, _, observed) = readback_pre_holdout_terminal_cached(
            client,
            &loaded.request,
            &loaded.sha256,
            protocol,
            &cache,
        )
        .unwrap();
        assert_eq!(observed, result_sha256);
        let output = cache.join("model-report.json");
        let report = report_settled_campaign_cache(
            &loaded.request,
            &loaded.sha256,
            result_sha256,
            &cache,
            &output,
        )
        .unwrap();
        assert!(report["bytes"].as_u64().unwrap() < 4 * 1024 * 1024);
        let summary: crate::mission_metrics::campaign::CampaignEvidenceReport =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(summary.rounds.len(), 2);
        assert_eq!(
            summary
                .rounds
                .iter()
                .map(|r| r.model_metrics.as_ref().unwrap().model_count())
                .sum::<usize>(),
            6
        );
        assert_eq!(
            summary
                .rounds
                .iter()
                .map(|r| r.mlp_folds.len())
                .sum::<usize>(),
            6
        );
        assert!(
            !summary.training_performed
                && !summary.metrics_recomputed
                && !summary.raw_data_required_by_report_consumer
        );
        // A larger valid-shaped summary stays complete in ACK/OSS. Its receipt
        // blocks an automatic workstation download instead of stranding settlement.
        let mut aggregate = summary.clone();
        while serde_json::to_vec_pretty(&aggregate).unwrap().len()
            <= crate::mission_metrics::campaign::MAX_WORKSTATION_REPORT_BYTES
        {
            aggregate.rounds.extend(aggregate.rounds.clone());
        }
        for (index, round) in aggregate.rounds.iter_mut().enumerate() {
            round.round_id = format!("report-size-fixture-{index}");
            round.seed = index as u64;
        }
        let aggregate = crate::mission_metrics::campaign::CampaignEvidenceReport::new(
            aggregate.campaign_id,
            aggregate.request_sha256,
            aggregate.campaign_result_sha256,
            aggregate.execution_source_revision,
            aggregate.termination_reason,
            aggregate.consumed_trials,
            aggregate.rounds,
        )
        .unwrap();
        let large_output = cache.join("large-cloud-report.json");
        let large_receipt = aggregate.persist(&large_output).unwrap();
        assert_eq!(large_receipt["fits_workstation_byte_limit"], false);
        assert!(large_receipt["bytes"].as_u64().unwrap() > 4 * 1024 * 1024);
        let preserved: crate::mission_metrics::campaign::CampaignEvidenceReport =
            serde_json::from_slice(&std::fs::read(&large_output).unwrap()).unwrap();
        assert_eq!(preserved.rounds.len(), aggregate.rounds.len());
        assert_eq!(aggregate.persist(&large_output).unwrap(), large_receipt);
        let first_bytes = std::fs::read(&output).unwrap();
        assert_eq!(
            report_settled_campaign_cache(
                &loaded.request,
                &loaded.sha256,
                result_sha256,
                &cache,
                &output
            )
            .unwrap(),
            report
        );
        assert_eq!(std::fs::read(&output).unwrap(), first_bytes);
        assert!(report_settled_campaign_cache(
            &loaded.request,
            &loaded.sha256,
            &"f".repeat(64),
            &cache,
            &output
        )
        .is_err());
        let corrupt = cache.join("round-readback/round-0-results.zip");
        let bytes = std::fs::read(&corrupt).unwrap();
        std::fs::write(&corrupt, b"corrupt cache").unwrap();
        assert!(readback_pre_holdout_terminal_cached(
            client,
            &loaded.request,
            &loaded.sha256,
            protocol,
            &cache
        )
        .unwrap_err()
        .to_string()
        .contains("SHA256"));
        std::fs::write(&corrupt, bytes).unwrap();
        for (original, backup) in originals {
            std::fs::rename(backup, original).unwrap();
        }
    }

    fn regenerate_metric_entries(entries: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        let selection: crate::mission_runner::CexSupervisedModelSelectionV1 =
            serde_json::from_slice(&entries["results/supervised-model-selection.json"]).unwrap();
        let inputs = crate::mission_runner::CEX_SUPERVISED_MODEL_NAMES
            .into_iter()
            .map(|name| {
                crate::mission_metrics::summarize_bytes(
                    &entries[&format!("results/{name}-supervised-backtest.json")],
                )
                .unwrap()
            })
            .collect();
        let report = crate::mission_metrics::build_report(
            inputs,
            &[selection],
            Some(alpha_domain::CexBaselineModelKindV1::Ridge),
        )
        .unwrap();
        entries.insert(
            "results/supervised-model-metrics.json".into(),
            serde_json::to_vec_pretty(&report).unwrap(),
        );
        entries.insert(
            "results/supervised-model-metrics.csv".into(),
            report.to_csv().into_bytes(),
        );
    }

    fn assert_metric_bundle_rejected(
        loaded: &LoadedRequest,
        protocol: &str,
        client: &Client,
        mutate: impl FnOnce(&mut std::collections::BTreeMap<String, Vec<u8>>),
        expected: &str,
    ) {
        let bundle_path = Path::new(&loaded.request.rounds[0].result_readback_url);
        let result_path = Path::new(&loaded.request.campaign_result_readback_url);
        let original_bundle = std::fs::read(bundle_path).unwrap();
        let original_result = std::fs::read(result_path).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&original_bundle)).unwrap();
        let mut entries = std::collections::BTreeMap::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            entries.insert(entry.name().to_string(), bytes);
        }
        mutate(&mut entries);
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        std::fs::write(bundle_path, writer.finish().unwrap().into_inner()).unwrap();
        let changed_hash = hft_research_artifacts::sha256_file(bundle_path).unwrap();
        let mut changed: serde_json::Value = serde_json::from_slice(&original_result).unwrap();
        changed["rounds"][0]["result_bundle_sha256"] = serde_json::json!(changed_hash);
        changed["rounds"][0]["result_readback_bundle_sha256"] = serde_json::json!(changed_hash);
        std::fs::write(result_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        let rejection =
            readback_pre_holdout_terminal(client, &loaded.request, &loaded.sha256, protocol)
                .err()
                .map(|error| format!("{error:#}"));
        std::fs::write(bundle_path, original_bundle).unwrap();
        std::fs::write(result_path, original_result).unwrap();
        let rejection = rejection.expect("rewritten model evidence must be rejected");
        assert!(
            rejection.contains(expected),
            "unexpected rejection: {rejection}"
        );
    }

    fn assert_development_request_requires_withheld_inputs(
        fixture: &CampaignE2eFixture,
        loaded: LoadedRequest,
        hash: String,
    ) {
        // A signed final grant is not a withheld data capability. The V6
        // search request cannot create a final worker that reads those bytes.
        use alpha_domain::campaign_finalization::{
            sign_campaign_final_evaluation_grant, CampaignFinalEvaluationGrantV1,
            FINAL_EVALUATION_GRANT_SCHEMA,
        };
        let final_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let image = format!(
            "registry/research-runner@sha256:{}",
            loaded.request.image_identity
        );
        let controller_image = format!("registry/controller@sha256:{}", "2".repeat(64));
        let execution = crate::mission_dispatch::final_admission::source_execution_binding(
            &loaded.request,
            &fixture._render_fixture.materialization_path,
            &image,
            &controller_image,
        )
        .unwrap();
        let operation = format!("campaign-attempt-{}", "a".repeat(64));
        let now = chrono::Utc::now();
        let signed = sign_campaign_final_evaluation_grant(
            CampaignFinalEvaluationGrantV1 {
                schema_version: FINAL_EVALUATION_GRANT_SCHEMA.into(),
                grant_id: "final-e2e-grant".into(),
                family_id: "final-e2e-family".into(),
                family_definition_sha256: "c".repeat(64),
                family_head_sha256: "d".repeat(64),
                execution,
                selected_results: std::collections::BTreeMap::from([(operation.clone(), hash)]),
                max_candidates: 4,
                max_job_seconds: 3600,
                valid_from: now - chrono::TimeDelta::minutes(1),
                expires_at: now + chrono::TimeDelta::hours(2),
            },
            "final-key".into(),
            &final_key,
        )
        .unwrap();
        let result = final_evaluation::FinalRequest::new(
            signed,
            std::collections::BTreeMap::from([(operation, loaded.request)]),
            fixture
                ._root
                .path()
                .join("final-published")
                .to_string_lossy()
                .into_owned(),
        );
        assert!(
            result.is_err(),
            "development-only source cannot invent withheld read capabilities"
        );
        assert!(!fixture.global_claim_path.exists());
        assert!(!fixture._root.path().join("final-work").exists());
    }

    #[test]
    fn development_only_request_cannot_open_holdout_with_a_final_grant() {
        let mut fixture = campaign_e2e_fixture_with_rejected_holdout(
            "campaign-final-negative",
            false,
            false,
            true,
            true,
        );
        prepare_fixture_for_execute(&mut fixture).unwrap();
        execute(fixture.args.clone()).unwrap();
        let loaded = load_request(&fixture.args.request).unwrap();
        let hash =
            hft_research_artifacts::sha256_file(&fixture.work_dir.join("campaign-result.json"))
                .unwrap();
        assert_development_request_requires_withheld_inputs(&fixture, loaded, hash);
    }

    #[test]
    fn execute_negative_campaign_creates_no_claim() {
        let mut fixture = campaign_e2e_fixture("campaign-e2e-negative", true, false, false);
        prepare_fixture_for_execute(&mut fixture).unwrap();
        execute(fixture.args.clone()).unwrap();

        let result: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.work_dir.join("campaign-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(result["termination_reason"], "campaign_no_candidate");
        assert!(result["rounds"].as_array().unwrap().iter().all(|round| {
            round["termination_reason"] == serde_json::json!("no_accepted_factors")
        }));
        assert!(result["finalization"].is_null());
        assert!(!fixture.global_claim_path.exists());
        let loaded = load_request(&fixture.args.request).unwrap();
        let materialization = crate::mission_runner::decode_materialization(
            &std::fs::read(&fixture._render_fixture.materialization_path).unwrap(),
        )
        .unwrap();
        let protocol = crate::mission_render::approved_evaluation_protocol(&materialization)
            .unwrap()
            .content_hash()
            .unwrap();
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let (outcome, trials, hash) =
            readback_pre_holdout_terminal(&client, &loaded.request, &loaded.sha256, &protocol)
                .unwrap();
        assert_eq!(
            outcome,
            alpha_domain::campaign_control::CampaignAttemptOutcomeV1::NoCandidate
        );
        assert_eq!(trials, result["consumed_trials"].as_u64().unwrap());
        assert_eq!(
            hash,
            hft_research_artifacts::sha256_file(&fixture.work_dir.join("campaign-result.json"))
                .unwrap()
        );
        let result_path = Path::new(&loaded.request.campaign_result_readback_url);
        let original = std::fs::read(result_path).unwrap();
        let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
        changed["rounds"][0]["consumed_trials"] =
            serde_json::json!(changed["rounds"][0]["consumed_trials"].as_u64().unwrap() + 1);
        changed["consumed_trials"] =
            serde_json::json!(changed["consumed_trials"].as_u64().unwrap() + 1);
        std::fs::write(result_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            readback_pre_holdout_terminal(&client, &loaded.request, &loaded.sha256, &protocol)
                .is_err(),
            "self-consistent summary counts cannot replace round artifact evidence"
        );
        std::fs::write(result_path, &original).unwrap();
        let bundle_path = Path::new(&loaded.request.rounds[0].result_readback_url);
        let original_bundle = std::fs::read(bundle_path).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(bundle_path)
            .unwrap();
        let mut archive = zip::ZipWriter::new_append(file).unwrap();
        archive
            .start_file(
                "results/sealed-holdout-receipt.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(b"{}").unwrap();
        archive.finish().unwrap();
        let changed_hash = hft_research_artifacts::sha256_file(bundle_path).unwrap();
        let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
        changed["rounds"][0]["result_bundle_sha256"] = serde_json::json!(changed_hash);
        changed["rounds"][0]["result_readback_bundle_sha256"] = serde_json::json!(changed_hash);
        std::fs::write(result_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            readback_pre_holdout_terminal(&client, &loaded.request, &loaded.sha256, &protocol)
                .is_err(),
            "a standalone sealed receipt cannot be hidden by omitting the finalization report"
        );
        std::fs::write(bundle_path, original_bundle).unwrap();
        std::fs::write(result_path, original).unwrap();
    }

    #[test]
    fn collect_round_ledger_rejects_supervised_replay_report_drift() {
        let mut fixture = campaign_e2e_fixture("campaign-ledger-no-selection", false, false, true);
        prepare_fixture_for_execute(&mut fixture).unwrap();
        execute(fixture.args.clone()).unwrap();

        let request = load_request(&fixture.args.request).unwrap().request;
        let round = request.rounds[0].clone();
        let execute_dir = fixture
            .work_dir
            .join(format!("mission/{}/execute", round.round_id));
        let mut report = recover_round_report(&fixture.work_dir, &request, &round);
        report.supervised_replay_gate_passed = Some(false);
        let error = collect_round_ledger(&execute_dir, &round, &report).unwrap_err();
        assert!(error
            .to_string()
            .contains("supervised replay report drifted from its receipt"));
    }

    #[test]
    fn collect_round_ledger_rejects_missing_supervised_replay() {
        let mut fixture =
            campaign_e2e_fixture("campaign-ledger-missing-result", false, false, true);
        prepare_fixture_for_execute(&mut fixture).unwrap();
        execute(fixture.args.clone()).unwrap();

        let request = load_request(&fixture.args.request).unwrap().request;
        let round = request.rounds[0].clone();
        let execute_dir = fixture
            .work_dir
            .join(format!("mission/{}/execute", round.round_id));
        let results = execute_dir.join("results");
        std::fs::remove_file(results.join("supervised-event-replay-receipt.json")).unwrap();
        let mut report = recover_round_report(&fixture.work_dir, &request, &round);
        report.supervised_replay_receipt_id = None;
        report.supervised_replay_gate_passed = None;

        let error = collect_round_ledger(&execute_dir, &round, &report).unwrap_err();

        assert!(error
            .to_string()
            .contains("eligible supervised selection is missing its event replay receipt"));
    }

    #[test]
    fn supervised_search_does_not_touch_an_existing_global_claim() {
        let mut fixture = campaign_e2e_fixture("campaign-e2e-existing-claim", false, true, false);
        prepare_fixture_for_execute(&mut fixture).unwrap();
        execute(fixture.args).unwrap();

        assert_eq!(
            std::fs::read(&fixture.global_claim_path).unwrap(),
            b"claimed"
        );
        assert!(fixture
            .work_dir
            .join("mission/r1/execute/results/factor-bank.json")
            .exists());
        assert!(fixture
            .work_dir
            .join("mission/r2/execute/results/factor-bank.json")
            .exists());
        assert!(!fixture
            .work_dir
            .join("mission/finalization/final-precommit.json")
            .exists());
    }

    #[test]
    fn extract_bundle_rejects_zip_slip_entries() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("bundle.zip");
        let file = File::create(&bundle).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("../escape.txt", options).unwrap();
        writer.write_all(b"escape").unwrap();
        writer.finish().unwrap();

        let error = extract_bundle(&bundle, &root.path().join("extract")).unwrap_err();

        assert!(error.to_string().contains("non-enclosed path"));
    }

    #[test]
    fn extract_bundle_rejects_too_many_entries() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("bundle.zip");
        let mut writer = zip::ZipWriter::new(File::create(&bundle).unwrap());
        for index in 0..=MAX_RESULT_BUNDLE_FILES {
            writer
                .start_file(
                    format!("entry-{index}"),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
        }
        writer.finish().unwrap();

        let error = extract_bundle(&bundle, &root.path().join("extract")).unwrap_err();

        assert!(error.to_string().contains("too many entries"));
    }

    #[test]
    fn load_round_subset_result_rejects_invalid_json() {
        let root = tempfile::tempdir().unwrap();
        let results = root.path().join("results");
        std::fs::create_dir_all(&results).unwrap();
        std::fs::write(results.join("factor-subset-mcts-result.json"), b"{").unwrap();

        assert!(load_round_subset_result(&results).is_err());
    }

    fn recover_round_report(
        work_dir: &Path,
        request: &CampaignRequest,
        round: &CampaignRoundRequest,
    ) -> crate::mission_runner::ExecutionReport {
        let mission_readback = work_dir.join(format!(
            "mission/{}/admission/mission-readback.json",
            round.round_id
        ));
        let mission: alpha_domain::CexResearchMissionArtifactV1 =
            serde_json::from_slice(&std::fs::read(&mission_readback).unwrap()).unwrap();
        let mission_id = mission.semantic_id().unwrap();
        let mission_sha256 = hft_research_artifacts::sha256_file(&mission_readback).unwrap();
        let request_sha256 =
            hft_research_artifacts::sha256_file(&work_dir.join("campaign-request.json")).unwrap();
        let binding = ExecutionBinding::Campaign {
            campaign_id: request.campaign_id.clone(),
            round_id: round.round_id.clone(),
            request_sha256: request_sha256.clone(),
        };
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        recover_execution_report_from_published_result(
            &client,
            &round.result_readback_url,
            &work_dir.join(format!("recover-{}.zip", round.round_id)),
            &mission_id,
            &mission_sha256,
            &binding,
            request
                .prepared_inputs
                .as_ref()
                .map(|_| (request, request_sha256.as_str())),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn immutable_publish_conflict_recognizes_http_conflict() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let error = Client::new()
            .put(format!("http://{address}/mission.json"))
            .body("mission")
            .send()
            .unwrap()
            .error_for_status()
            .unwrap_err();
        server.join().unwrap();

        assert!(immutable_publish_conflict(&error.into()));
    }

    fn loaded_request_for_learning() -> LoadedRequest {
        let request = valid_request();
        let sha256 = hex::encode(Sha256::digest(serialize_request(&request).unwrap()));
        LoadedRequest { request, sha256 }
    }

    fn negative_campaign_result(loaded: &LoadedRequest) -> CampaignResultV1 {
        let factor_attempts = loaded.request.research_plan.max_candidates().unwrap();
        let consumed_per_round = factor_attempts + 3;
        let no_trades = CampaignEvaluationFeedbackV1 {
            passed: false,
            score: -1.0,
            time_series_ic: Some(0.05),
            time_series_rank_ic: Some(0.04),
            cumulative_net_return: 0.0,
            max_drawdown: 0.0,
            net_sharpe: 0.0,
            trade_count: 0,
            total_turnover: 0.0,
            max_book_depth_fraction: Some(0.0),
            max_book_depth_fraction_limit: Some(0.05),
            capacity_breached: false,
        };
        let overtrade = CampaignEvaluationFeedbackV1 {
            passed: false,
            score: -20.0,
            time_series_ic: Some(0.02),
            time_series_rank_ic: Some(0.01),
            cumulative_net_return: -0.5,
            max_drawdown: 0.2,
            net_sharpe: -0.3,
            trade_count: 10_804,
            total_turnover: 10_804.0,
            max_book_depth_fraction: Some(0.2),
            max_book_depth_fraction_limit: Some(0.05),
            capacity_breached: true,
        };
        CampaignResultV1 {
            schema_version: CAMPAIGN_RESULT_SCHEMA_V8.to_string(),
            campaign_id: loaded.request.campaign_id.clone(),
            request_sha256: loaded.sha256.clone(),
            build_source_revision: loaded.request.build_source_revision.clone(),
            image_identity: loaded.request.image_identity.clone(),
            campaign_inputs_sha256: loaded.request.campaign_inputs_sha256.clone(),
            producer_source_revision: loaded.request.producer_source_revision.clone(),
            producer_image_identity: loaded.request.producer_image_identity.clone(),
            research_plan_sha256: loaded.request.research_plan.content_hash().unwrap(),
            learning_directive: loaded.request.research_plan.learning_directive.clone(),
            learning_directive_sha256: loaded
                .request
                .research_plan
                .learning_directive
                .as_ref()
                .map(CexCampaignLearningDirectiveV1::content_hash)
                .transpose()
                .unwrap(),
            search_policy_revision: Some(
                loaded.request.research_plan.search_policy_revision.clone(),
            ),
            holdout_id: loaded.request.holdout_id.clone(),
            declared_total_trials: loaded.request.declared_total_trials,
            consumed_trials: consumed_per_round * loaded.request.rounds.len(),
            stop_rule: STOP_RULE_V2.to_string(),
            termination_reason: "campaign_no_candidate".to_string(),
            rounds: loaded
                .request
                .rounds
                .iter()
                .map(|round| CampaignMissionLedgerV1 {
                    round_id: round.round_id.clone(),
                    seed: round.seed,
                    identity: round.identity.clone(),
                    mission_id: format!("mission-{}", round.round_id),
                    mission_sha256: "5".repeat(64),
                    request_sha256: Some(loaded.sha256.clone()),
                    result_bundle_sha256: "6".repeat(64),
                    result_readback_bundle_sha256: "6".repeat(64),
                    replay_receipt_id: None,
                    replay_gate_passed: Some(false),
                    supervised_candidate_id: Some("selected-burn".to_string()),
                    supervised_replay_receipt_id: None,
                    supervised_replay_gate_passed: None,
                    final_precommit_id: None,
                    sealed_receipt_id: None,
                    sealed_passed: None,
                    strategy_bundle_id: None,
                    promotion_id: None,
                    selected_candidate_id: None,
                    selected_candidate_content_hash: None,
                    selected_score: None,
                    consumed_trials: consumed_per_round,
                    termination_reason: "no_passing_supervised_model".to_string(),
                    feedback: CampaignRoundFeedbackV1 {
                        calendar_validation: None,
                        factor_attempts,
                        model_attempts: Some(3),
                        accepted_factors: 1,
                        factors: (0..factor_attempts)
                            .map(|index| CampaignFactorFeedbackV1 {
                                factor_signature_sha256: format!("{index:064x}"),
                                source_features: vec!["book_imbalance".to_string()],
                                rejection_codes: if index == 0 {
                                    Vec::new()
                                } else {
                                    vec![CexFactorRejectionCodeV1::EvaluationFailed]
                                },
                                evaluation: (index == 0).then(|| no_trades.clone()),
                            })
                            .collect(),
                        baseline_gate_passed: false,
                        baseline_failure_codes: vec![
                            CexBaselineFailureCodeV1::InsufficientEvidence,
                        ],
                        ridge: Some(no_trades.clone()),
                        cart: Some(no_trades.clone()),
                        burn: Some(overtrade.clone()),
                        supervised_ridge: Some(no_trades.clone()),
                        supervised_cart: Some(no_trades.clone()),
                        supervised_burn: Some(overtrade.clone()),
                        supervised_selected: Some(overtrade.clone()),
                        supervised_selected_evaluation_proof: None,
                        supervised_selected_candidate_id: Some("selected-burn".to_string()),
                        supervised_replay: None,
                    },
                })
                .collect(),
            selected_round_id: None,
            selected_candidate_id: None,
            selected_candidate_content_hash: None,
            finalization: None,
        }
    }

    fn positive_ic_negative_net_result(loaded: &LoadedRequest) -> CampaignResultV1 {
        let mut result = negative_campaign_result(loaded);
        let selected = CampaignEvaluationFeedbackV1 {
            passed: false,
            score: -20.605987220447407,
            time_series_ic: Some(0.21292829708865732),
            time_series_rank_ic: Some(0.2764773346418556),
            cumulative_net_return: -0.000019082591104964222,
            max_drawdown: 0.000006843160778702284,
            net_sharpe: -0.32380961996553226,
            trade_count: 3_175,
            total_turnover: 0.09975051317061064,
            max_book_depth_fraction: Some(0.00007752219388635406),
            max_book_depth_fraction_limit: Some(0.05),
            capacity_breached: false,
        };
        for round in &mut result.rounds {
            round.supervised_candidate_id = Some("selected-cart".to_string());
            round.feedback.supervised_cart = Some(selected.clone());
            round.feedback.supervised_selected = Some(selected.clone());
            round.feedback.supervised_selected_candidate_id = Some("selected-cart".to_string());
        }
        result
    }

    struct CampaignE2eFixture {
        _prepared_root: Option<tempfile::TempDir>,
        _root: tempfile::TempDir,
        _replay_root: tempfile::TempDir,
        _render_fixture: mission_render::tests::Fixture,
        replay_artifact_path: PathBuf,
        replay_manifest_path: PathBuf,
        args: CampaignExecuteArgs,
        work_dir: PathBuf,
        global_claim_path: PathBuf,
    }

    fn campaign_e2e_fixture(
        name: &str,
        zero_labels: bool,
        preexisting_claim: bool,
        replay_tracks_features: bool,
    ) -> CampaignE2eFixture {
        campaign_e2e_fixture_with_rejected_holdout(
            name,
            zero_labels,
            preexisting_claim,
            replay_tracks_features,
            false,
        )
    }

    fn campaign_e2e_fixture_with_rejected_holdout(
        name: &str,
        zero_labels: bool,
        preexisting_claim: bool,
        replay_tracks_features: bool,
        rejected_holdout: bool,
    ) -> CampaignE2eFixture {
        campaign_e2e_fixture_with_price_step(
            name,
            zero_labels,
            preexisting_claim,
            replay_tracks_features,
            rejected_holdout,
            0.0005,
        )
    }

    fn campaign_e2e_fixture_with_price_step(
        name: &str,
        zero_labels: bool,
        preexisting_claim: bool,
        replay_tracks_features: bool,
        rejected_holdout: bool,
        price_step: f64,
    ) -> CampaignE2eFixture {
        let render_fixture = mission_render::tests::Fixture::canonical();
        campaign_e2e_fixture_with_input(
            name,
            zero_labels,
            preexisting_claim,
            replay_tracks_features,
            rejected_holdout,
            price_step,
            render_fixture,
        )
    }

    fn campaign_e2e_fixture_with_input(
        name: &str,
        zero_labels: bool,
        preexisting_claim: bool,
        replay_tracks_features: bool,
        rejected_holdout: bool,
        price_step: f64,
        render_fixture: mission_render::tests::Fixture,
    ) -> CampaignE2eFixture {
        let mut rows = mission_render::tests::read_feature_rows(&render_fixture.feature_path);
        if zero_labels {
            for row in &mut rows {
                row.features.insert("mid_price".into(), 60_000.0);
                row.label = 0.0;
            }
        } else {
            let mut mid_price = 60_000.0_f64;
            for (index, row) in rows.iter_mut().enumerate() {
                let direction: f64 = if (index / 100).is_multiple_of(2) {
                    1.0
                } else {
                    -1.0
                };
                row.features
                    .insert("ask_depth_top5".to_string(), 10.0 + direction);
                row.features
                    .insert("bid_depth_top5".to_string(), 10.0 - direction);
                row.features.insert("book_imbalance".to_string(), direction);
                row.features
                    .insert("book_imbalance_top5".to_string(), direction);
                row.features
                    .insert("near_depth_concentration_skew_top5".to_string(), direction);
                row.features
                    .insert("spread_bps".to_string(), 0.5 + direction * 0.05);
                row.features
                    .insert("vwap_center_deviation_top5_bps".to_string(), direction);
                row.features
                    .insert("weighted_book_imbalance_top5".to_string(), direction);
                row.features.insert("mid_price".to_string(), mid_price);
                mid_price *= 1.0 + direction * price_step;
            }
            let observations = rows
                .iter()
                .map(|row| (row.feature_available_time, row.features["mid_price"]))
                .collect::<std::collections::BTreeMap<_, _>>();
            for row in &mut rows {
                if let Some(future) = observations.get(&row.label_available_time) {
                    row.label = future / row.features["mid_price"] - 1.0;
                }
            }
        }
        if rejected_holdout {
            let materialization = crate::mission_runner::decode_materialization(
                &std::fs::read(&render_fixture.materialization_path).unwrap(),
            )
            .unwrap();
            let count = mission_render::approved_evaluation_protocol(&materialization)
                .unwrap()
                .walk_forward
                .sealed_holdout_rows;
            let start = rows.len() - count;
            for row in &mut rows[start..] {
                row.label = -row.label;
            }
        }
        mission_render::tests::rewrite_feature_rows(&render_fixture.feature_path, &rows);
        rebind_materialization_feature_artifact(
            &render_fixture.materialization_path,
            &render_fixture.feature_path,
        );
        let replay_root = tempfile::tempdir().unwrap();
        let (replay_artifact_path, replay_manifest_path) = write_campaign_replay_fixture(
            replay_root.path(),
            &render_fixture.feature_path,
            &render_fixture.materialization_path,
            replay_tracks_features,
        );
        let rendered = render_cex_bundle(
            &render_fixture.feature_path,
            &render_fixture.materialization_path,
            &CexCampaignResearchPlanV1::canonical(),
            7,
            declared_total_trials_for_rounds(&CexCampaignResearchPlanV1::canonical(), 2).unwrap(),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let global_claim_path = root.path().join("global-holdout-claim.json");
        if preexisting_claim {
            std::fs::write(&global_claim_path, b"claimed").unwrap();
        }
        let request = local_request_from_paths(
            root.path(),
            &render_fixture.feature_path,
            &render_fixture.materialization_path,
            &replay_artifact_path,
            &replay_manifest_path,
            &rendered.mission.spec.holdout.holdout_id,
            &[7, 11],
        );
        let request_path = root.path().join(format!("{name}-request.json"));
        let request_bytes = serialize_request(&request).unwrap();
        std::fs::write(&request_path, &request_bytes).unwrap();
        let request_sha256 = hex::encode(Sha256::digest(&request_bytes));
        let work_dir = root.path().join("campaign-work");
        CampaignE2eFixture {
            _prepared_root: None,
            _root: root,
            _replay_root: replay_root,
            _render_fixture: render_fixture,
            replay_artifact_path,
            replay_manifest_path,
            args: CampaignExecuteArgs {
                final_evaluation: false,
                final_trusted_keys: None,
                pre_holdout: true,
                work_dir: work_dir.clone(),
                campaign_id: request.campaign_id.clone(),
                image_identity: request.image_identity.clone(),
                request: request_path,
                request_sha256,
            },
            work_dir,
            global_claim_path,
        }
    }

    pub(crate) fn rebind_materialization_feature_artifact(
        materialization_path: &Path,
        feature_path: &Path,
    ) {
        let feature_sha256 = hft_research_artifacts::sha256_file(feature_path).unwrap();
        let mut materialization: serde_json::Value =
            serde_json::from_slice(&std::fs::read(materialization_path).unwrap()).unwrap();
        materialization["artifact_sha256"] = serde_json::json!(feature_sha256.clone());
        materialization["snapshot"]["feature_artifact_sha256"] = serde_json::json!(feature_sha256);
        let snapshot: hft_research_manifest::CexReplaySnapshotV5 =
            serde_json::from_value(materialization["snapshot"].clone()).unwrap();
        materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
        std::fs::write(
            materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();
    }

    fn local_request_from_paths(
        root: &Path,
        feature_path: &Path,
        materialization_path: &Path,
        replay_artifact_path: &Path,
        replay_manifest_path: &Path,
        holdout_id: &str,
        seeds: &[u64],
    ) -> CampaignRequest {
        let seed_bytes = seeds
            .iter()
            .flat_map(|seed| seed.to_be_bytes())
            .collect::<Vec<_>>();
        let campaign_id = format!(
            "cex-campaign-local-{}",
            &hex::encode(Sha256::digest(&seed_bytes))[..16]
        );
        let published = root.join("published");
        let research_plan = CexCampaignResearchPlanV1::canonical();
        let feature_sha256 = hft_research_artifacts::sha256_file(feature_path).unwrap();
        let materialization_sha256 =
            hft_research_artifacts::sha256_file(materialization_path).unwrap();
        let replay_artifact_sha256 =
            hft_research_artifacts::sha256_file(replay_artifact_path).unwrap();
        let replay_manifest_sha256 =
            hft_research_artifacts::sha256_file(replay_manifest_path).unwrap();
        let round_identity = CampaignRoundIdentityV1 {
            schema_version: CAMPAIGN_ROUND_IDENTITY_SCHEMA_V1.to_string(),
            data_window_hours: CAMPAIGN_DATA_WINDOW_HOURS,
            data_fingerprint_sha256: campaign_data_fingerprint_sha256(
                &"f".repeat(64),
                BUILD_SOURCE_REVISION,
                &feature_sha256,
                &materialization_sha256,
                &replay_artifact_sha256,
                &replay_manifest_sha256,
            )
            .unwrap(),
            image_identity: "1".repeat(64),
            build_source_revision: BUILD_SOURCE_REVISION.to_string(),
        };
        CampaignRequest {
            prepared_inputs: None,
            schema_version: CAMPAIGN_REQUEST_SCHEMA_V5.to_string(),
            campaign_id: campaign_id.clone(),
            build_source_revision: BUILD_SOURCE_REVISION.to_string(),
            image_identity: "1".repeat(64),
            campaign_inputs_sha256: "f".repeat(64),
            producer_source_revision: BUILD_SOURCE_REVISION.to_string(),
            producer_image_identity: "e".repeat(64),
            research_plan: research_plan.clone(),
            study_proposal: None,
            feature_url: feature_path.to_string_lossy().into_owned(),
            feature_sha256,
            materialization_url: materialization_path.to_string_lossy().into_owned(),
            materialization_sha256,
            replay_artifact_url: replay_artifact_path.to_string_lossy().into_owned(),
            replay_artifact_sha256,
            replay_manifest_url: replay_manifest_path.to_string_lossy().into_owned(),
            replay_manifest_sha256,
            holdout_id: holdout_id.to_string(),
            declared_total_trials: declared_total_trials_for_rounds(&research_plan, seeds.len())
                .unwrap(),
            rounds: seeds
                .iter()
                .enumerate()
                .map(|(index, seed)| CampaignRoundRequest {
                    round_id: format!("r{}", index + 1),
                    seed: *seed,
                    identity: round_identity.clone(),
                    mission_put_url: published
                        .join(format!(
                            "campaign-id={campaign_id}/round=r{}/mission.json",
                            index + 1
                        ))
                        .to_string_lossy()
                        .into_owned(),
                    mission_readback_url: published
                        .join(format!(
                            "campaign-id={campaign_id}/round=r{}/mission.json",
                            index + 1
                        ))
                        .to_string_lossy()
                        .into_owned(),
                    result_put_url: published
                        .join(format!(
                            "campaign-id={campaign_id}/round=r{}/results.zip",
                            index + 1
                        ))
                        .to_string_lossy()
                        .into_owned(),
                    result_readback_url: published
                        .join(format!(
                            "campaign-id={campaign_id}/round=r{}/results.zip",
                            index + 1
                        ))
                        .to_string_lossy()
                        .into_owned(),
                })
                .collect(),
            holdout_claim_put_url: root
                .join("global-holdout-claim.json")
                .to_string_lossy()
                .into_owned(),
            holdout_claim_readback_url: root
                .join("global-holdout-claim.json")
                .to_string_lossy()
                .into_owned(),
            campaign_result_put_url: published
                .join(format!("campaign-id={campaign_id}/campaign-result.json"))
                .to_string_lossy()
                .into_owned(),
            campaign_result_readback_url: published
                .join(format!("campaign-id={campaign_id}/campaign-result.json"))
                .to_string_lossy()
                .into_owned(),
        }
    }

    fn write_campaign_replay_fixture(
        root: &Path,
        feature_path: &Path,
        materialization_path: &Path,
        replay_tracks_features: bool,
    ) -> (PathBuf, PathBuf) {
        const MESSAGE: &str = "
message binance_replay {
  REQUIRED INT64 timestamp_us;
  REQUIRED INT64 sequence;
  REQUIRED BINARY event (UTF8);
  REQUIRED BINARY payload_json (UTF8);
}
";
        let rows = mission_render::tests::read_feature_rows(feature_path);
        let materialization: serde_json::Value =
            serde_json::from_slice(&std::fs::read(materialization_path).unwrap()).unwrap();
        let source_revision = materialization["source_revision"]
            .as_str()
            .unwrap()
            .to_string();
        let source_segments = materialization["source_segments"].as_array().unwrap();
        let source_content_sha256 = source_segments[0]["sha256"].as_str().unwrap().to_string();
        let source_manifest_sha256 = source_segments[0]["collector_manifest_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        let source_start_ns = source_segments[0]["start_received_at_ns"].as_u64().unwrap();
        let source_end_ns = source_segments[0]["end_received_at_ns"].as_u64().unwrap();
        let source_events = source_segments[0]["events"].as_u64().unwrap();
        let initial_mid = if replay_tracks_features {
            rows[0].features["mid_price"]
        } else {
            60_000.0
        };
        let initial_levels = replay_book_levels(initial_mid, "10");
        let mut previous_mid = initial_mid;
        let mut timestamps = Vec::with_capacity(rows.len() + 2);
        let mut sequences = Vec::with_capacity(rows.len() + 2);
        let mut events = Vec::with_capacity(rows.len() + 2);
        let mut payloads = Vec::with_capacity(rows.len() + 2);
        for (index, row) in rows.iter().enumerate() {
            if index == 0 {
                timestamps.push(
                    i64::try_from(
                        source_start_ns / 1_000 + u64::from(!source_start_ns.is_multiple_of(1_000)),
                    )
                    .unwrap(),
                );
                sequences.push(1);
                events.push("snapshot".to_string());
                payloads.push(serde_json::to_string(&initial_levels).unwrap());
            }
            let current_mid = if replay_tracks_features {
                row.features["mid_price"]
            } else {
                initial_mid
            };
            let update = if replay_tracks_features && index > 0 {
                replay_book_delta(previous_mid, current_mid)
            } else {
                replay_book_levels(current_mid, "10")
            };
            timestamps.push(row.feature_available_time.timestamp_micros() + 100);
            sequences.push(i64::try_from(index + 2).unwrap());
            events.push("l2_update".to_string());
            payloads.push(serde_json::to_string(&update).unwrap());
            previous_mid = current_mid;
        }
        timestamps.push(rows.last().unwrap().label_available_time.timestamp_micros() + 100);
        sequences.push(i64::try_from(timestamps.len()).unwrap());
        events.push("l2_update".to_string());
        payloads.push(serde_json::to_string(&replay_book_levels(previous_mid, "10")).unwrap());
        let temporary_artifact = root.join("replay.parquet");
        let schema = Arc::new(parse_message_type(MESSAGE).unwrap());
        let properties = Arc::new(WriterProperties::builder().build());
        let mut writer = SerializedFileWriter::new(
            File::create(&temporary_artifact).unwrap(),
            schema,
            properties,
        )
        .unwrap();
        let mut group = writer.next_row_group().unwrap();
        write_replay_i64_column(&mut group, &timestamps);
        write_replay_i64_column(&mut group, &sequences);
        write_replay_utf8_column(&mut group, &events);
        write_replay_utf8_column(&mut group, &payloads);
        group.close().unwrap();
        writer.close().unwrap();
        let replay_artifact_sha256 =
            hft_research_artifacts::sha256_file(&temporary_artifact).unwrap();
        let replay_artifact_path = root.join(format!("{replay_artifact_sha256}.parquet"));
        std::fs::rename(&temporary_artifact, &replay_artifact_path).unwrap();
        let replay_manifest = serde_json::json!({
            "dataset_kind": "backtest_canonical_replay_parquet",
            "schema_version": "binance-replay-parquet-v1",
            "format": "parquet",
            "parquet_schema": "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8",
            "mission_id": materialization["mission_id"],
            "market": materialization["market"],
            "symbol": materialization["symbol"],
            "dataset": "binance_usdm_lob",
            "modalities": ["lob"],
            "source_revision": source_revision,
            "source_segments": [{
                "file": "segment.jsonl.zst",
                "sha256": source_content_sha256,
                "collector_manifest_sha256": source_manifest_sha256,
                "success_marker_sha256": hex::encode(Sha256::digest(format!("{source_content_sha256}\n"))),
                "start_received_at_ns": source_start_ns,
                "end_received_at_ns": source_end_ns,
                "events": source_events
            }],
            "rows": timestamps.len(),
            "first_event_time_us": timestamps[0],
            "last_event_time_us": *timestamps.last().unwrap(),
            "sequence_start": 1,
            "sequence_end": timestamps.len(),
            "artifact_path": replay_artifact_path.file_name().unwrap().to_str().unwrap(),
            "artifact_sha256": &replay_artifact_sha256,
            "point_in_time": true
        });
        let replay_manifest_path = root.join("replay-manifest.json");
        std::fs::write(
            &replay_manifest_path,
            serde_json::to_vec_pretty(&replay_manifest).unwrap(),
        )
        .unwrap();
        (replay_artifact_path, replay_manifest_path)
    }

    fn replay_book_levels(mid: f64, quantity: &str) -> serde_json::Value {
        let bids = (1..=5)
            .map(|offset| {
                [
                    format!("{:.8}", mid - f64::from(offset)),
                    quantity.to_string(),
                ]
            })
            .collect::<Vec<_>>();
        let asks = (1..=5)
            .map(|offset| {
                [
                    format!("{:.8}", mid + f64::from(offset)),
                    quantity.to_string(),
                ]
            })
            .collect::<Vec<_>>();
        serde_json::json!({"bids": bids, "asks": asks})
    }

    fn replay_book_delta(previous_mid: f64, current_mid: f64) -> serde_json::Value {
        let previous = replay_book_levels(previous_mid, "0");
        let current = replay_book_levels(current_mid, "10");
        let mut bids = previous["bids"].as_array().unwrap().clone();
        bids.extend(current["bids"].as_array().unwrap().iter().cloned());
        let mut asks = previous["asks"].as_array().unwrap().clone();
        asks.extend(current["asks"].as_array().unwrap().iter().cloned());
        serde_json::json!({"bids": bids, "asks": asks})
    }

    fn write_replay_i64_column(group: &mut SerializedRowGroupWriter<'_, File>, values: &[i64]) {
        let mut column = group.next_column().unwrap().unwrap();
        column
            .typed::<Int64Type>()
            .write_batch(values, None, None)
            .unwrap();
        column.close().unwrap();
    }

    fn write_replay_utf8_column(group: &mut SerializedRowGroupWriter<'_, File>, values: &[String]) {
        let values = values
            .iter()
            .map(|value| ByteArray::from(value.as_str()))
            .collect::<Vec<_>>();
        let mut column = group.next_column().unwrap().unwrap();
        column
            .typed::<ByteArrayType>()
            .write_batch(&values, None, None)
            .unwrap();
        column.close().unwrap();
    }

    fn rebind_request_to_output_root(
        mut request: CampaignRequest,
        root_name: &str,
    ) -> CampaignRequest {
        let root = format!(
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/{root_name}"
        );
        for round in &mut request.rounds {
            round.mission_put_url = format!(
                "{root}/campaign-id=placeholder/round={}/mission.json",
                round.round_id
            );
            round.mission_readback_url = format!(
                "{root}/campaign-id=placeholder/round={}/mission.json?readback=1",
                round.round_id
            );
            round.result_put_url = format!(
                "{root}/campaign-id=placeholder/round={}/results.zip",
                round.round_id
            );
            round.result_readback_url = format!(
                "{root}/campaign-id=placeholder/round={}/results.zip?readback=1",
                round.round_id
            );
        }
        request.campaign_result_put_url =
            format!("{root}/campaign-id=placeholder/campaign-result.json");
        request.campaign_result_readback_url =
            format!("{root}/campaign-id=placeholder/campaign-result.json?readback=1");
        request.campaign_id = expected_campaign_id(&request).unwrap();
        for round in &mut request.rounds {
            round.mission_put_url = format!(
                "{root}/campaign-id={}/round={}/mission.json",
                request.campaign_id, round.round_id
            );
            round.mission_readback_url = format!(
                "{root}/campaign-id={}/round={}/mission.json?readback=1",
                request.campaign_id, round.round_id
            );
            round.result_put_url = format!(
                "{root}/campaign-id={}/round={}/results.zip",
                request.campaign_id, round.round_id
            );
            round.result_readback_url = format!(
                "{root}/campaign-id={}/round={}/results.zip?readback=1",
                request.campaign_id, round.round_id
            );
        }
        request.campaign_result_put_url = format!(
            "{root}/campaign-id={}/campaign-result.json",
            request.campaign_id
        );
        request.campaign_result_readback_url = format!(
            "{root}/campaign-id={}/campaign-result.json?readback=1",
            request.campaign_id
        );
        request
    }
}

fn legacy_input_object(
    request: &CampaignRequest,
    label: &str,
    value: &str,
) -> anyhow::Result<Option<String>> {
    if request.prepared_inputs.is_some() {
        if !value.is_empty() {
            bail!("native request exposes whole-source/withheld input URI");
        }
        Ok(None)
    } else {
        Ok(Some(canonical_tokyo_oss_internal_object(label, value)?))
    }
}
