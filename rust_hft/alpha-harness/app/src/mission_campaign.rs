Warning: truncated output (original token count: 95411)
Total output lines: 9247

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
    let (request, campaign_inputs_sha256) = freeze_request(&args)?;
    let mut plan = FrozenCampaignPlan {
        preparation_authentication_tag: None,
        schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.to_string(),
        campaign_inputs_sha256,
        signing_plan: signing_plan(&request)?,
        canonical_request: request.clone(),
    };
    if let Some(path) = &args.preparation_ledger {
        let ledger = alpha_store::AlphaStore::open_read_only(path)?;
        plan.preparation_authentication_tag = Some(preparation::authenticate(&ledger, &plan)?);
    }
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
    let loaded = load_request(&args.request)?;
    validate_request(&loaded.request)?;
    let result = load_campaign_result(&args.result)?;
    let result_sha256 = hft_research_artifacts::sha256_file(&args.result)?;
    if result_sha256 != normalized_sha256("parent Campaign result", &args.result_sha256)? {
        bail!("parent Campaign result SHA256 mismatch");
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
    let result = load_campaign_result(&args.parent_result)?;
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
    let mut classified = None;
    for round in &result.rounds {
        let selected = selected_supervised_feedback(&round.feedback)
            .context("Campaign failure has no supervised model evidence")?;
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
    let plan = load_freeze_plan(&args.freeze)?;
    validate_request(&plan.canonical_request)?;
    if expected_campaign_id(&plan.canonical_request)? != plan.canonical_request.campaign_id {
        bail!("frozen campaign request campaign_id does not match its semantic identity");
    }

    let loaded = load_request(&args.signed_request)?;
    validate_request_matches_freeze(&loaded.request, &plan)?;
    if loaded.request.prepared_inputs.is_some() {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(Policy::none())
            .build()?;
        let readback = tempfile::tempdir().context("native finalized input readback")?;
        let verified = prepared_inputs::acquire_native_prepared(
            &loaded.request,
            &loaded.sha256,
            &client,
            readback.path(),
        )?;
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
pub(crate) fn validate_request_for_execute(request: &CampaignRequest) -> anyhow::Result<()> {
    validate_request(request)
}

#[cfg(test)]
pub(crate) fn validate_request_for_execute(request: &CampaignRequest) -> anyhow::Result<()> {
    validate_request(request).or_else(|_| validate_local_test_request(request))
}

#[cfg(feature = "scientific")]
fn execute_loaded_request(args: CampaignExecuteArgs, loaded: LoadedRequest) -> anyhow::Result<()> {
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
        schema_version: CAMPAIGN_RESULT_SCHEMA_V8.to_string(),
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
            || report.supervised_replay_r…45411 tokens truncated…lid_producer = receipt.clone();
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
        assert_eq!(result["schema_version"], CAMPAIGN_RESULT_SCHEMA_V8);
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

    fn assert_calendar_h1_readback(negative: bool) {
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
        let error = prepare_fixture_for_execute(&mut fixture).unwrap_err();
        assert!(
            error.to_string().contains("withheld selection or holdout"),
            "{error:#}"
        );
        assert!(!fixture.work_dir.exists());
        assert!(!fixture.global_claim_path.exists());
        assert_eq!(
            load_request(&fixture.args.request)
                .unwrap()
                .request
                .prepared_inputs,
            None
        );
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
            assert!(next_campaign_policy_revision(
                &loaded,
                &hft_research_artifacts::sha256_file(
                    &fixture.work_dir.join("campaign-result.json")
                )
                .unwrap(),
                classify_campaign_failure(&result).unwrap()
            )
            .is_err());
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
        assert_eq!(result["schema_version"], CAMPAIGN_RESULT_SCHEMA_V8);
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

    fn rebind_materialization_feature_artifact(materialization_path: &Path, feature_path: &Path) {
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
