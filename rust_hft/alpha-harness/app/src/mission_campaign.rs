use crate::{
    cli::{
        print_json, CampaignExecuteArgs, CampaignFinalizeArgs, CampaignFreezeArgs, CampaignIdArgs,
        ExecuteMissionArgs, BUILD_SOURCE_REVISION,
    },
    data_mission, mission_dispatch,
    mission_render::render_cex_bundle,
    mission_runner::{
        execute_report, fetch_to_file, finalize_existing_search_round, normalized_sha256,
        publish_immutable_file, recover_execution_report_from_published_result, valid_git_revision,
        validate_cex_holdout_id, ExecutionBinding, MAX_RESULT_BUNDLE_BYTES,
    },
    prediction_dispatch::{
        canonical_tokyo_oss_internal_object, cex_campaign_round_root,
        cex_global_holdout_claim_object, validate_dns_label,
    },
};
use alpha_domain::{canonical_json_hash, CexFactorBankRevisionV2};
use alpha_engine::engines::CexFactorBankMctsResultV1;
use anyhow::{bail, Context};
use hft_backtest::config::verify_canonical_replay_artifact_streaming;
use reqwest::{blocking::Client, redirect::Policy, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use zip::ZipArchive;

const CAMPAIGN_FREEZE_SCHEMA_V1: &str = "cex-campaign-freeze-v1";
const CAMPAIGN_INPUTS_SCHEMA_V1: &str = "monday.cex_campaign_inputs.v1";
const CAMPAIGN_REQUEST_SCHEMA_V3: &str = "cex-campaign-request-v3";
const CAMPAIGN_RESULT_SCHEMA_V3: &str = "cex-campaign-result-v3";
const CAMPAIGN_IDENTITY_SCHEMA_V3: &str = "cex-campaign-identity-v3";
const STOP_RULE_V2: &str = "bounded_multi_round_single_finalize_v2";
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_CAMPAIGN_RESULT_BYTES: u64 = 1024 * 1024;
const MAX_RESULT_BUNDLE_FILES: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CampaignRequest {
    pub(crate) schema_version: String,
    pub(crate) campaign_id: String,
    pub(crate) build_source_revision: String,
    pub(crate) image_identity: String,
    pub(crate) campaign_inputs_sha256: String,
    pub(crate) producer_source_revision: String,
    pub(crate) producer_image_identity: String,
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
    pub(crate) mission_put_url: String,
    pub(crate) mission_readback_url: String,
    pub(crate) result_put_url: String,
    pub(crate) result_readback_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenCampaignPlan {
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

#[derive(Debug, Clone, Serialize)]
struct CampaignMissionLedgerV1 {
    round_id: String,
    seed: u64,
    mission_id: String,
    mission_sha256: String,
    request_sha256: Option<String>,
    result_bundle_sha256: String,
    result_readback_bundle_sha256: String,
    replay_receipt_id: Option<String>,
    replay_gate_passed: Option<bool>,
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
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
struct CampaignResultV1 {
    schema_version: &'static str,
    campaign_id: String,
    request_sha256: String,
    build_source_revision: String,
    image_identity: String,
    campaign_inputs_sha256: String,
    producer_source_revision: String,
    producer_image_identity: String,
    holdout_id: String,
    declared_total_trials: usize,
    consumed_trials: usize,
    stop_rule: &'static str,
    termination_reason: String,
    rounds: Vec<CampaignMissionLedgerV1>,
    selected_round_id: Option<String>,
    selected_candidate_id: Option<String>,
    selected_candidate_content_hash: Option<String>,
    finalization: Option<CampaignFinalizationV1>,
}

#[derive(Debug)]
struct LoadedRequest {
    request: CampaignRequest,
    sha256: String,
}

pub fn execute(args: CampaignExecuteArgs) -> anyhow::Result<()> {
    let loaded = load_request(&args.request)?;
    if loaded.sha256 != normalized_sha256("campaign request", &args.request_sha256)? {
        bail!("campaign request SHA256 mismatch");
    }
    validate_request_for_execute(&loaded.request)?;
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
    execute_loaded_request(args, loaded)
}

pub fn freeze(args: CampaignFreezeArgs) -> anyhow::Result<()> {
    let (request, campaign_inputs_sha256) = freeze_request(&args)?;
    let plan = FrozenCampaignPlan {
        schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.to_string(),
        campaign_inputs_sha256,
        signing_plan: signing_plan(&request)?,
        canonical_request: request.clone(),
    };
    data_mission::write_json_atomic(&args.output, &plan)?;
    print_json(&CampaignFreezeReport {
        campaign_id: request.campaign_id.clone(),
        holdout_id: request.holdout_id.clone(),
        declared_total_trials: request.declared_total_trials,
        output: args.output.display().to_string(),
    })
}

pub fn finalize(args: CampaignFinalizeArgs) -> anyhow::Result<()> {
    let plan = load_freeze_plan(&args.freeze)?;
    validate_request(&plan.canonical_request)?;
    if expected_campaign_id(&plan.canonical_request)? != plan.canonical_request.campaign_id {
        bail!("frozen campaign request campaign_id does not match its semantic identity");
    }

    let loaded = load_request(&args.signed_request)?;
    validate_request_matches_freeze(&loaded.request, &plan)?;
    data_mission::write_json_atomic(&args.request_out, &loaded.request)?;
    let rendered = mission_dispatch::write_submission(
        &args.submission_out,
        &args.attempt_id,
        &args.image,
        loaded.request.clone(),
    )?;
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
fn validate_request_for_execute(request: &CampaignRequest) -> anyhow::Result<()> {
    validate_request(request)
}

#[cfg(test)]
fn validate_request_for_execute(request: &CampaignRequest) -> anyhow::Result<()> {
    validate_request(request).or_else(|_| validate_local_test_request(request))
}

fn execute_loaded_request(args: CampaignExecuteArgs, loaded: LoadedRequest) -> anyhow::Result<()> {
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
    data_mission::write_json_atomic(&local_request_path, &loaded.request)?;

    let feature_path = shared_input_dir.join("features.jsonl");
    fetch_verified(
        &client,
        "campaign feature",
        &loaded.request.feature_url,
        &feature_path,
        &loaded.request.feature_sha256,
        crate::mission_runner::MAX_FEATURE_BYTES,
    )?;
    let materialization_path = shared_input_dir.join("materialization.json");
    fetch_verified(
        &client,
        "campaign materialization",
        &loaded.request.materialization_url,
        &materialization_path,
        &loaded.request.materialization_sha256,
        crate::mission_runner::MAX_MATERIALIZATION_BYTES,
    )?;
    let replay_artifact_path = shared_input_dir.join(format!(
        "{}.parquet",
        normalized_sha256(
            "campaign replay artifact",
            &loaded.request.replay_artifact_sha256
        )?
    ));
    fetch_verified(
        &client,
        "campaign replay artifact",
        &loaded.request.replay_artifact_url,
        &replay_artifact_path,
        &loaded.request.replay_artifact_sha256,
        1024 * 1024 * 1024,
    )?;
    let replay_manifest_path = shared_input_dir.join("replay-manifest.json");
    fetch_verified(
        &client,
        "campaign replay manifest",
        &loaded.request.replay_manifest_url,
        &replay_manifest_path,
        &loaded.request.replay_manifest_sha256,
        16 * 1024 * 1024,
    )?;

    let mut ledgers = Vec::with_capacity(loaded.request.rounds.len());
    let mut selected_round = None;
    let mut selected_mission = None;
    let mut selected_execute_dir = None;
    for round in &loaded.request.rounds {
        let rendered = render_cex_bundle(
            &feature_path,
            &materialization_path,
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
        data_mission::write_json_atomic(&mission_local_path, &rendered.mission)?;
        let mission_readback_path = mission_publish_dir.join("mission-readback.json");
        let mission_sha256 = publish_create_once_json(
            &client,
            "Mission",
            &round.mission_put_url,
            &round.mission_readback_url,
            &mission_local_path,
            &mission_readback_path,
        )?;

        let binding = ExecutionBinding::Campaign {
            campaign_id: loaded.request.campaign_id.clone(),
            round_id: round.round_id.clone(),
            request_sha256: loaded.sha256.clone(),
        };
        let recovered_result_path = round_dir.join("published-result-readback.zip");
        let execute_dir = round_dir.join("execute");
        let report = if let Some(report) = recover_execution_report_from_published_result(
            &client,
            &round.result_readback_url,
            &recovered_result_path,
            &rendered.mission_id,
            &mission_sha256,
            &binding,
        )? {
            extract_bundle(&recovered_result_path, &execute_dir)?;
            report
        } else {
            let (round_claim_put_url, round_claim_readback_url) =
                campaign_round_claim_urls(&loaded.request, round)?;
            execute_report(
                ExecuteMissionArgs {
                    work_dir: execute_dir.clone(),
                    mission_id: rendered.mission_id.clone(),
                    holdout_id: loaded.request.holdout_id.clone(),
                    mission_url: mission_readback_path.to_string_lossy().into_owned(),
                    mission_sha256: mission_sha256.clone(),
                    feature_url: feature_path.to_string_lossy().into_owned(),
                    materialization_url: materialization_path.to_string_lossy().into_owned(),
                    replay_artifact_url: replay_artifact_path.to_string_lossy().into_owned(),
                    replay_artifact_sha256: loaded.request.replay_artifact_sha256.clone(),
                    replay_manifest_url: replay_manifest_path.to_string_lossy().into_owned(),
                    replay_manifest_sha256: loaded.request.replay_manifest_sha256.clone(),
                    resume_url: None,
                    resume_sha256: None,
                    result_put_url: round.result_put_url.clone(),
                    result_readback_url: round.result_readback_url.clone(),
                    holdout_claim_put_url: round_claim_put_url,
                    holdout_claim_readback_url: round_claim_readback_url,
                },
                binding,
            )?
        };
        let ledger = collect_round_ledger(&execute_dir, round, &report)?;
        let is_better = selected_round
            .as_ref()
            .is_none_or(|current: &CampaignMissionLedgerV1| {
                compare_round_selection(&ledger, current).is_gt()
            });
        if ledger.replay_gate_passed == Some(true) && ledger.selected_score.is_some() && is_better {
            selected_round = Some(ledger.clone());
            selected_mission = Some(rendered.mission);
            selected_execute_dir = Some(execute_dir.clone());
        }
        ledgers.push(ledger);
    }

    let consumed_trials = ledgers.iter().map(|round| round.consumed_trials).sum();
    if consumed_trials > loaded.request.declared_total_trials {
        bail!("campaign consumed trials exceeded declared_total_trials");
    }
    let finalization =
        if let (Some(selected_round), Some(selected_mission), Some(selected_execute_dir)) =
            (&selected_round, &selected_mission, &selected_execute_dir)
        {
            let finalization_dir = mission_dir.join("finalization");
            let report = finalize_existing_search_round(
                selected_execute_dir,
                &finalization_dir,
                &loaded.request.holdout_claim_put_url,
                &loaded.request.holdout_claim_readback_url,
                selected_mission,
            )?;
            let final_precommit = read_json_value(&finalization_dir.join("final-precommit.json"))?;
            let sealed_holdout_claim =
                read_json_value(&finalization_dir.join("sealed-holdout-claim.json"))?;
            let sealed_holdout_receipt =
                read_json_value(&finalization_dir.join("sealed-holdout-receipt.json"))?;
            let strategy_bundle_path = finalization_dir.join("strategy-bundle.json");
            let promotion_record_path = finalization_dir.join("promotion-record.json");
            Some(CampaignFinalizationV1 {
                round_id: selected_round.round_id.clone(),
                precommit_id: report.precommit_id.clone(),
                sealed_receipt_id: report.sealed_receipt_id.clone(),
                sealed_passed: report.sealed_passed,
                strategy_bundle_id: report.strategy_bundle_id.clone(),
                promotion_id: report.promotion_id.clone(),
                final_precommit,
                sealed_holdout_claim,
                sealed_holdout_receipt,
                strategy_bundle: strategy_bundle_path
                    .try_exists()?
                    .then(|| read_json_value(&strategy_bundle_path))
                    .transpose()?,
                promotion_record: promotion_record_path
                    .try_exists()?
                    .then(|| read_json_value(&promotion_record_path))
                    .transpose()?,
                final_precommit_sha256: crate::mission_runner::sha256_file(
                    &finalization_dir.join("final-precommit.json"),
                )?,
                sealed_holdout_claim_sha256: crate::mission_runner::sha256_file(
                    &finalization_dir.join("sealed-holdout-claim.json"),
                )?,
                sealed_holdout_receipt_sha256: crate::mission_runner::sha256_file(
                    &finalization_dir.join("sealed-holdout-receipt.json"),
                )?,
                strategy_bundle_sha256: strategy_bundle_path
                    .try_exists()?
                    .then(|| crate::mission_runner::sha256_file(&strategy_bundle_path))
                    .transpose()?,
                promotion_record_sha256: promotion_record_path
                    .try_exists()?
                    .then(|| crate::mission_runner::sha256_file(&promotion_record_path))
                    .transpose()?,
            })
        } else {
            None
        };

    let result = CampaignResultV1 {
        schema_version: CAMPAIGN_RESULT_SCHEMA_V3,
        campaign_id: loaded.request.campaign_id.clone(),
        request_sha256: loaded.sha256.clone(),
        build_source_revision: loaded.request.build_source_revision.clone(),
        image_identity: loaded.request.image_identity.clone(),
        campaign_inputs_sha256: loaded.request.campaign_inputs_sha256.clone(),
        producer_source_revision: loaded.request.producer_source_revision.clone(),
        producer_image_identity: loaded.request.producer_image_identity.clone(),
        holdout_id: loaded.request.holdout_id.clone(),
        declared_total_trials: loaded.request.declared_total_trials,
        consumed_trials,
        stop_rule: STOP_RULE_V2,
        termination_reason: if finalization.is_some() {
            "campaign_finalized".to_string()
        } else if selected_round.is_some() {
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
    data_mission::write_json_atomic(&local_result_path, &result)?;
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

fn freeze_request(args: &CampaignFreezeArgs) -> anyhow::Result<(CampaignRequest, String)> {
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
    let feature_sha256 =
        verify_local_receipt_item("campaign feature", &feature_path, &receipt.feature.sha256)?;
    let materialization_sha256 = verify_local_receipt_item(
        "campaign materialization",
        &materialization_path,
        &receipt.materialization.sha256,
    )?;
    let replay_artifact_sha256 = verify_local_receipt_item(
        "campaign replay artifact",
        &replay_artifact_path,
        &receipt.replay_artifact.sha256,
    )?;
    let replay_manifest_sha256 = verify_local_receipt_item(
        "campaign replay manifest",
        &replay_manifest_path,
        &receipt.replay_manifest.sha256,
    )?;
    verify_canonical_replay_artifact_streaming(
        &replay_artifact_path,
        &replay_manifest_path,
        Some(&replay_artifact_sha256),
        &replay_manifest_sha256,
        None,
        None,
    )?;
    let declared_total_trials = crate::mission_render::max_candidates_for_tests()
        .checked_mul(2)
        .and_then(|count| count.checked_mul(args.seeds.len()))
        .context("campaign declared_total_trials overflowed")?;
    let probe_seed = *args
        .seeds
        .first()
        .context("campaign freeze requires at least one seed")?;
    let rendered = render_cex_bundle(
        &feature_path,
        &materialization_path,
        probe_seed,
        declared_total_trials,
    )?;
    Ok((
        build_request_from_parts(
            &feature_url,
            &feature_sha256,
            &materialization_url,
            &materialization_sha256,
            &replay_artifact_url,
            &replay_artifact_sha256,
            &replay_manifest_url,
            &replay_manifest_sha256,
            &campaign_inputs_sha256,
            &receipt.source_revision,
            &producer_image_identity,
            &build_source_revision,
            &image_identity,
            &campaign_root,
            &rendered.mission.spec.holdout.holdout_id,
            &args.seeds,
        )?,
        campaign_inputs_sha256,
    ))
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
    if receipt.symbol != "BTCUSDT" {
        bail!("campaign inputs receipt symbol must be BTCUSDT");
    }
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
        crate::prediction_dispatch::TOKYO_OSS_INTERNAL_ENDPOINT
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
    let actual = crate::mission_runner::sha256_file(path)?;
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
    if destination.try_exists()? {
        return Ok(());
    }
    std::fs::create_dir_all(destination)?;
    let mut archive = ZipArchive::new(File::open(bundle)?)?;
    if archive.len() > MAX_RESULT_BUNDLE_FILES {
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
        let mut file = File::create(&output)?;
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

fn collect_round_ledger(
    execute_dir: &Path,
    round: &CampaignRoundRequest,
    report: &crate::mission_runner::ExecutionReport,
) -> anyhow::Result<CampaignMissionLedgerV1> {
    let results = execute_dir.join("results");
    let factor_bank: CexFactorBankRevisionV2 =
        serde_json::from_slice(&std::fs::read(results.join("factor-bank.json"))?)?;
    let strategy_path = results.join("combination-walk-forward.json");
    let subset_result = load_round_subset_result(&results)?;
    let consumed_trials = factor_bank.attempts.len()
        + subset_result
            .as_ref()
            .map(|result| result.candidates_evaluated)
            .unwrap_or(0);
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
        mission_id: report.mission_id.clone(),
        mission_sha256: report.mission_sha256.clone(),
        request_sha256: report.request_sha256.clone(),
        result_bundle_sha256: report.bundle_sha256.clone(),
        result_readback_bundle_sha256: report.readback_bundle_sha256.clone(),
        replay_receipt_id: report.replay_receipt_id.clone(),
        replay_gate_passed: report.replay_gate_passed,
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
    })
}

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
        let claim = crate::prediction_dispatch::cex_campaign_round_result_and_holdout_claim(
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

fn read_json_value(path: &Path) -> anyhow::Result<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path)?).map_err(anyhow::Error::new)
}

fn load_freeze_plan(path: &Path) -> anyhow::Result<FrozenCampaignPlan> {
    let mut file = File::open(path)
        .with_context(|| format!("open campaign freeze plan {}", path.display()))?;
    if file.metadata()?.len() > MAX_REQUEST_BYTES {
        bail!("campaign freeze plan exceeds {MAX_REQUEST_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    let plan: FrozenCampaignPlan = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse campaign freeze plan {}", path.display()))?;
    if plan.schema_version != CAMPAIGN_FREEZE_SCHEMA_V1 {
        bail!("campaign freeze plan schema_version must be {CAMPAIGN_FREEZE_SCHEMA_V1}");
    }
    Ok(plan)
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

pub(crate) fn serialize_request(request: &CampaignRequest) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec_pretty(request).map_err(anyhow::Error::new)
}

#[cfg(test)]
pub(crate) fn valid_request_for_tests() -> CampaignRequest {
    tests::valid_request_for_other_modules()
}

pub(crate) fn validate_request(request: &CampaignRequest) -> anyhow::Result<()> {
    if request.schema_version != CAMPAIGN_REQUEST_SCHEMA_V3 {
        bail!("campaign request schema_version must be {CAMPAIGN_REQUEST_SCHEMA_V3}");
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
    canonical_tokyo_oss_internal_object("campaign feature", &request.feature_url)?;
    normalized_sha256("campaign feature", &request.feature_sha256)?;
    canonical_tokyo_oss_internal_object("campaign materialization", &request.materialization_url)?;
    normalized_sha256("campaign materialization", &request.materialization_sha256)?;
    canonical_tokyo_oss_internal_object("campaign replay artifact", &request.replay_artifact_url)?;
    normalized_sha256("campaign replay artifact", &request.replay_artifact_sha256)?;
    canonical_tokyo_oss_internal_object("campaign replay manifest", &request.replay_manifest_url)?;
    normalized_sha256("campaign replay manifest", &request.replay_manifest_sha256)?;

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
    let per_round_trials = crate::mission_render::max_candidates_for_tests() * 2;
    let minimum_total_trials = per_round_trials
        .checked_mul(request.rounds.len())
        .context("campaign total trials overflowed")?;
    if request.declared_total_trials < minimum_total_trials {
        bail!("campaign declared_total_trials is below the minimum multi-round trial family");
    }
    let mut round_ids = std::collections::BTreeSet::new();
    let mut seeds = std::collections::BTreeSet::new();
    for round in &request.rounds {
        validate_dns_label("campaign round id", &round.round_id)?;
        if !round_ids.insert(round.round_id.as_str()) || !seeds.insert(round.seed) {
            bail!("campaign rounds must have unique ids and seeds");
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
    build_source_revision: &str,
    image_identity: &str,
    campaign_root: &str,
    holdout_id: &str,
    seeds: &[u64],
) -> anyhow::Result<CampaignRequest> {
    let mut request = CampaignRequest {
        schema_version: CAMPAIGN_REQUEST_SCHEMA_V3.to_string(),
        campaign_id: "placeholder".to_string(),
        build_source_revision: build_source_revision.to_string(),
        image_identity: image_identity.to_string(),
        campaign_inputs_sha256: campaign_inputs_sha256.to_string(),
        producer_source_revision: producer_source_revision.to_string(),
        producer_image_identity: producer_image_identity.to_string(),
        feature_url: feature_url.to_string(),
        feature_sha256: feature_sha256.to_string(),
        materialization_url: materialization_url.to_string(),
        materialization_sha256: materialization_sha256.to_string(),
        replay_artifact_url: replay_artifact_url.to_string(),
        replay_artifact_sha256: replay_artifact_sha256.to_string(),
        replay_manifest_url: replay_manifest_url.to_string(),
        replay_manifest_sha256: replay_manifest_sha256.to_string(),
        holdout_id: holdout_id.to_string(),
        declared_total_trials: crate::mission_render::max_candidates_for_tests()
            .checked_mul(2)
            .and_then(|count| count.checked_mul(seeds.len()))
            .context("campaign declared_total_trials overflowed")?,
        rounds: seeds
            .iter()
            .enumerate()
            .map(|(index, seed)| CampaignRoundRequest {
                round_id: format!("r{}", index + 1),
                seed: *seed,
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
    let feature_object =
        canonical_tokyo_oss_internal_object("campaign feature", &request.feature_url)?;
    let materialization_object = canonical_tokyo_oss_internal_object(
        "campaign materialization",
        &request.materialization_url,
    )?;
    let replay_artifact_object = canonical_tokyo_oss_internal_object(
        "campaign replay artifact",
        &request.replay_artifact_url,
    )?;
    let replay_manifest_object = canonical_tokyo_oss_internal_object(
        "campaign replay manifest",
        &request.replay_manifest_url,
    )?;
    let holdout_claim_object = canonical_tokyo_oss_internal_object(
        "campaign holdout claim",
        &request.holdout_claim_put_url,
    )?;
    let campaign_result_object =
        canonical_tokyo_oss_internal_object("campaign result", &request.campaign_result_put_url)?;
    let _campaign_root = campaign_output_root(&campaign_result_object)?;
    let mut actions = vec![
        signing_action_get("feature_get", feature_object),
        signing_action_get("materialization_get", materialization_object),
        signing_action_get("replay_artifact_get", replay_artifact_object),
        signing_action_get("replay_manifest_get", replay_manifest_object),
    ];
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
    let identity = serde_json::json!({
        "identity_schema_version": CAMPAIGN_IDENTITY_SCHEMA_V3,
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
        "feature": {
            "object": canonical_tokyo_oss_internal_object("campaign feature", &request.feature_url)?,
            "sha256": normalized_sha256("campaign feature", &request.feature_sha256)?,
        },
        "materialization": {
            "object": canonical_tokyo_oss_internal_object("campaign materialization", &request.materialization_url)?,
            "sha256": normalized_sha256("campaign materialization", &request.materialization_sha256)?,
        },
        "replay_artifact": {
            "object": canonical_tokyo_oss_internal_object("campaign replay artifact", &request.replay_artifact_url)?,
            "sha256": normalized_sha256("campaign replay artifact", &request.replay_artifact_sha256)?,
        },
        "replay_manifest": {
            "object": canonical_tokyo_oss_internal_object("campaign replay manifest", &request.replay_manifest_url)?,
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
            }))
            .collect::<Vec<_>>(),
        "stop_rule": STOP_RULE_V2,
    });
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

fn publish_create_once_json(
    client: &Client,
    label: &str,
    destination: &str,
    readback_url: &str,
    source: &Path,
    readback_path: &Path,
) -> anyhow::Result<String> {
    let published_sha256 = crate::mission_runner::sha256_file(source)?;
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
    if request.schema_version != CAMPAIGN_REQUEST_SCHEMA_V3 {
        bail!("campaign request schema_version must be {CAMPAIGN_REQUEST_SCHEMA_V3}");
    }
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
    let per_round_trials = crate::mission_render::max_candidates_for_tests() * 2;
    let minimum_total_trials = per_round_trials
        .checked_mul(request.rounds.len())
        .context("campaign total trials overflowed")?;
    if request.declared_total_trials < minimum_total_trials {
        bail!("campaign declared_total_trials is below the minimum multi-round trial family");
    }
    let mut round_ids = std::collections::BTreeSet::new();
    let mut seeds = std::collections::BTreeSet::new();
    for path in [
        &request.feature_url,
        &request.materialization_url,
        &request.replay_artifact_url,
        &request.replay_manifest_url,
        &request.holdout_claim_put_url,
        &request.holdout_claim_readback_url,
        &request.campaign_result_put_url,
        &request.campaign_result_readback_url,
    ] {
        if path.trim().is_empty() {
            bail!("local test request paths must be non-empty");
        }
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
        if round.mission_put_url != round.mission_readback_url
            || round.result_put_url != round.result_readback_url
        {
            bail!("local test round readback paths must match their put paths");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission_render;
    use alpha_domain::CexResearchMissionArtifactV1;
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
        request.declared_total_trials = crate::mission_render::max_candidates_for_tests() * 2;
        request.campaign_id = expected_campaign_id(&request).unwrap();
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn selection_tie_break_is_deterministic() {
        let lower_hash = CampaignMissionLedgerV1 {
            round_id: "r1".to_string(),
            seed: 11,
            request_sha256: Some("0".repeat(64)),
            mission_id: "m1".to_string(),
            mission_sha256: "a".repeat(64),
            result_bundle_sha256: "b".repeat(64),
            result_readback_bundle_sha256: "b".repeat(64),
            replay_receipt_id: Some("receipt-1".to_string()),
            replay_gate_passed: Some(true),
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
        };
        let higher_hash = CampaignMissionLedgerV1 {
            round_id: "r2".to_string(),
            seed: 17,
            request_sha256: Some("1".repeat(64)),
            mission_id: "m2".to_string(),
            mission_sha256: "c".repeat(64),
            result_bundle_sha256: "d".repeat(64),
            result_readback_bundle_sha256: "d".repeat(64),
            replay_receipt_id: Some("receipt-2".to_string()),
            replay_gate_passed: Some(true),
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

    #[test]
    fn finalize_preserves_frozen_campaign_identity() {
        let root = tempfile::tempdir().unwrap();
        let freeze_path = root.path().join("freeze.json");
        let request_out = root.path().join("request.json");
        let submission_out = root.path().join("submission.json");
        let canonical_request = canonicalize_request_transport(&valid_request()).unwrap();
        let frozen = FrozenCampaignPlan {
            schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.to_string(),
            campaign_inputs_sha256: "a".repeat(64),
            signing_plan: signing_plan(&canonical_request).unwrap(),
            canonical_request: canonical_request.clone(),
        };
        data_mission::write_json_atomic(&freeze_path, &frozen).unwrap();

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
        data_mission::write_json_atomic(&signed_request_path, &signed).unwrap();

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
            BUILD_SOURCE_REVISION,
            &"1".repeat(64),
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns",
            "cex-holdout-test",
            &[7, 11],
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
        let fixture = campaign_e2e_fixture("campaign-freeze", false, false);
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
                sha256: crate::mission_runner::sha256_file(&input_root.join(&feature_relative))
                    .unwrap(),
            },
            materialization: CampaignInputReceiptItem {
                relative_path: materialization_relative.clone(),
                object_url: format!("{TEST_ROOT}/runs/campaign-freeze/materialization.json"),
                sha256: crate::mission_runner::sha256_file(
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
                sha256: crate::mission_runner::sha256_file(
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
                sha256: crate::mission_runner::sha256_file(
                    &input_root.join(&replay_manifest_relative),
                )
                .unwrap(),
            },
        };
        data_mission::write_json_atomic(&receipt_path, &receipt).unwrap();
        let output = root.path().join("freeze.json");

        freeze(CampaignFreezeArgs {
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: BUILD_SOURCE_REVISION.to_string(),
            image: executor_image_ref.clone(),
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
            output: output.clone(),
        })
        .unwrap();

        let (receipt_again, receipt_sha256) = load_campaign_inputs_receipt(&receipt_path).unwrap();
        assert_eq!(receipt_again.source_revision, producer_revision);
        assert_eq!(receipt_again.image_ref, producer_image_ref);
        let frozen = load_freeze_plan(&output).unwrap();
        assert_eq!(frozen.campaign_inputs_sha256, receipt_sha256);
        assert_eq!(
            frozen.canonical_request.campaign_inputs_sha256,
            receipt_sha256
        );
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

        let mut invalid_producer = receipt.clone();
        invalid_producer.source_revision = "not-a-git-sha".to_string();
        assert!(validate_campaign_inputs_receipt(&invalid_producer)
            .unwrap_err()
            .to_string()
            .contains("source_revision must be an exact git revision"));

        let invalid_executor = freeze_request(&CampaignFreezeArgs {
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: BUILD_SOURCE_REVISION.to_string(),
            image: "registry/research-runner:latest".to_string(),
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
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
        data_mission::write_json_atomic(&replay_manifest_path, &invalid_manifest).unwrap();
        let mut invalid_replay_receipt = receipt.clone();
        invalid_replay_receipt.replay_manifest.sha256 =
            crate::mission_runner::sha256_file(&replay_manifest_path).unwrap();
        data_mission::write_json_atomic(&receipt_path, &invalid_replay_receipt).unwrap();
        let invalid_replay = freeze_request(&CampaignFreezeArgs {
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: BUILD_SOURCE_REVISION.to_string(),
            image: executor_image_ref.clone(),
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
            output: root.path().join("invalid-replay-freeze.json"),
        })
        .unwrap_err();
        assert!(invalid_replay
            .chain()
            .any(|cause| cause.to_string().contains("artifact")));

        let invalid_source = freeze_request(&CampaignFreezeArgs {
            campaign_inputs: receipt_path.clone(),
            input_root: input_root.clone(),
            source_revision: "c".repeat(40),
            image: executor_image_ref,
            campaign_root: format!("{TEST_ROOT}/campaigns"),
            seeds: vec![7, 11],
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
            schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.to_string(),
            campaign_inputs_sha256: "a".repeat(64),
            signing_plan,
            canonical_request: canonical_request.clone(),
        };
        data_mission::write_json_atomic(&freeze_path, &frozen).unwrap();
        let signed_request_path = root.path().join("signed-request.json");
        data_mission::write_json_atomic(&signed_request_path, &valid_request()).unwrap();

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
            crate::prediction_dispatch::sha256_text(&request.holdout_id)
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
            crate::mission_runner::sha256_file(&destination).unwrap()
        );
        assert_eq!(
            crate::mission_runner::sha256_file(&readback).unwrap(),
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
            crate::mission_runner::sha256_file(&destination).unwrap()
        );
        assert_eq!(
            crate::mission_runner::sha256_file(&readback).unwrap(),
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
    fn execute_runs_two_rounds_and_finalizes_exactly_once() {
        let fixture = campaign_e2e_fixture("campaign-e2e-positive", false, false);
        let request = load_request(&fixture.args.request).unwrap().request;
        execute(fixture.args).unwrap();

        let work_dir = fixture.work_dir;
        assert!(work_dir.join("shared-inputs/features.jsonl").exists());
        assert!(work_dir.join("shared-inputs/materialization.json").exists());
        assert!(work_dir
            .join("mission/r1/admission/mission-readback.json")
            .exists());
        assert!(work_dir
            .join("mission/r2/admission/mission-readback.json")
            .exists());
        assert!(work_dir
            .join("mission/r1/execute/results/factor-bank.json")
            .exists());
        assert!(work_dir
            .join("mission/r2/execute/results/factor-bank.json")
            .exists());
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(work_dir.join("campaign-result.json")).unwrap())
                .unwrap();
        assert_eq!(result["schema_version"], CAMPAIGN_RESULT_SCHEMA_V3);
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
        assert_eq!(result["rounds"].as_array().unwrap().len(), 2);
        assert_eq!(result["declared_total_trials"], 64);
        assert_eq!(result["consumed_trials"], 64);
        for round in ["r1", "r2"] {
            let mission: serde_json::Value = serde_json::from_slice(
                &std::fs::read(
                    work_dir.join(format!("mission/{round}/admission/mission-readback.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(mission["spec"]["search"]["multiple_testing_trials"], 64);
            let subset: serde_json::Value = serde_json::from_slice(
                &std::fs::read(work_dir.join(format!(
                    "mission/{round}/execute/results/factor-subset-mcts-result.json"
                )))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                subset["selected"]["evaluation"]["evaluator_config"]["multiple_testing_trials"],
                64
            );
        }
        assert!(result["selected_round_id"].is_string());
        assert!(result["finalization"].is_object());
        let finalization = &result["finalization"];
        assert!(finalization["final_precommit"].is_object());
        assert!(finalization["sealed_holdout_claim"].is_object());
        assert!(finalization["sealed_holdout_receipt"].is_object());
        assert!(fixture.global_claim_path.exists());
    }

    #[test]
    fn execute_negative_campaign_creates_no_claim() {
        let fixture = campaign_e2e_fixture("campaign-e2e-negative", true, false);
        execute(fixture.args).unwrap();

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
    }

    #[test]
    fn collect_round_ledger_marks_missing_selection_as_no_passing_subset() {
        let fixture = campaign_e2e_fixture("campaign-ledger-no-selection", false, false);
        execute(fixture.args.clone()).unwrap();

        let request = load_request(&fixture.args.request).unwrap().request;
        let round = request.rounds[0].clone();
        let execute_dir = fixture
            .work_dir
            .join(format!("mission/{}/execute", round.round_id));
        let results = execute_dir.join("results");
        let subset_path = results.join("factor-subset-mcts-result.json");
        let mut report = recover_round_report(&fixture.work_dir, &request, &round);
        report.replay_gate_passed = Some(false);
        let failed_replay = collect_round_ledger(&execute_dir, &round, &report).unwrap();
        assert_eq!(failed_replay.termination_reason, "replay_gate_failed");
        assert!(failed_replay.selected_candidate_id.is_none());
        assert!(failed_replay.selected_score.is_none());

        let mut subset: CexFactorBankMctsResultV1 =
            serde_json::from_slice(&std::fs::read(&subset_path).unwrap()).unwrap();
        subset.selected = None;
        data_mission::write_json_atomic(&subset_path, &subset).unwrap();
        std::fs::remove_file(results.join("combination-walk-forward.json")).unwrap();

        report.replay_receipt_id = None;
        report.replay_gate_passed = None;
        let ledger = collect_round_ledger(&execute_dir, &round, &report).unwrap();

        assert_eq!(ledger.termination_reason, "no_passing_subset");
        assert!(ledger.selected_candidate_id.is_none());
        assert!(ledger.selected_candidate_content_hash.is_none());
        assert!(ledger.selected_score.is_none());
    }

    #[test]
    fn collect_round_ledger_rejects_missing_subset_result_for_nonempty_factor_bank() {
        let fixture = campaign_e2e_fixture("campaign-ledger-missing-result", false, false);
        execute(fixture.args.clone()).unwrap();

        let request = load_request(&fixture.args.request).unwrap().request;
        let round = request.rounds[0].clone();
        let execute_dir = fixture
            .work_dir
            .join(format!("mission/{}/execute", round.round_id));
        let results = execute_dir.join("results");
        std::fs::remove_file(results.join("factor-subset-mcts-result.json")).unwrap();

        let error = collect_round_ledger(
            &execute_dir,
            &round,
            &recover_round_report(&fixture.work_dir, &request, &round),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("non-empty Factor Bank is missing factor subset MCTS result"));
    }

    #[test]
    fn existing_global_claim_blocks_finalize_but_search_rounds_complete() {
        let fixture = campaign_e2e_fixture("campaign-e2e-existing-claim", false, true);
        let error = execute(fixture.args).unwrap_err();

        assert!(
            error.to_string().contains("already claimed")
                || error.to_string().contains("terminal and inconclusive")
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
        let mission: CexResearchMissionArtifactV1 =
            serde_json::from_slice(&std::fs::read(&mission_readback).unwrap()).unwrap();
        let mission_id = mission.semantic_id().unwrap();
        let mission_sha256 = crate::mission_runner::sha256_file(&mission_readback).unwrap();
        let request_sha256 =
            crate::mission_runner::sha256_file(&work_dir.join("campaign-request.json")).unwrap();
        let binding = ExecutionBinding::Campaign {
            campaign_id: request.campaign_id.clone(),
            round_id: round.round_id.clone(),
            request_sha256,
        };
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        recover_execution_report_from_published_result(
            &client,
            &round.result_readback_url,
            &work_dir.join(format!("recover-{}.zip", round.round_id)),
            &mission_id,
            &mission_sha256,
            &binding,
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

    pub(crate) fn valid_request_for_other_modules() -> CampaignRequest {
        valid_request()
    }

    fn valid_request() -> CampaignRequest {
        const TEST_ROOT: &str =
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research";
        let mut request = CampaignRequest {
            schema_version: CAMPAIGN_REQUEST_SCHEMA_V3.to_string(),
            campaign_id: String::new(),
            build_source_revision: "a".repeat(40),
            image_identity: "1".repeat(64),
            campaign_inputs_sha256: "f".repeat(64),
            producer_source_revision: "b".repeat(40),
            producer_image_identity: "e".repeat(64),
            feature_url: format!("{TEST_ROOT}/features.jsonl"),
            feature_sha256: "1".repeat(64),
            materialization_url: format!("{TEST_ROOT}/materialization.json"),
            materialization_sha256: "2".repeat(64),
            replay_artifact_url: format!("{TEST_ROOT}/replay.parquet"),
            replay_artifact_sha256: "3".repeat(64),
            replay_manifest_url: format!("{TEST_ROOT}/replay-manifest.json"),
            replay_manifest_sha256: "4".repeat(64),
            holdout_id: "cex-holdout-test".to_string(),
            declared_total_trials: crate::mission_render::max_candidates_for_tests() * 4,
            rounds: vec![
                CampaignRoundRequest {
                    round_id: "r1".to_string(),
                    seed: 11,
                    mission_put_url: format!(
                        "{TEST_ROOT}/campaign-id=placeholder/round=r1/mission.json"
                    ),
                    mission_readback_url: format!(
                        "{TEST_ROOT}/campaign-id=placeholder/round=r1/mission.json?readback=1"
                    ),
                    result_put_url: format!(
                        "{TEST_ROOT}/campaign-id=placeholder/round=r1/results.zip"
                    ),
                    result_readback_url: format!(
                        "{TEST_ROOT}/campaign-id=placeholder/round=r1/results.zip?readback=1"
                    ),
                },
                CampaignRoundRequest {
                    round_id: "r2".to_string(),
                    seed: 17,
                    mission_put_url: format!(
                        "{TEST_ROOT}/campaign-id=placeholder/round=r2/mission.json"
                    ),
                    mission_readback_url: format!(
                        "{TEST_ROOT}/campaign-id=placeholder/round=r2/mission.json?readback=1"
                    ),
                    result_put_url: format!(
                        "{TEST_ROOT}/campaign-id=placeholder/round=r2/results.zip"
                    ),
                    result_readback_url: format!(
                        "{TEST_ROOT}/campaign-id=placeholder/round=r2/results.zip?readback=1"
                    ),
                },
            ],
            holdout_claim_put_url: String::new(),
            holdout_claim_readback_url: String::new(),
            campaign_result_put_url: format!(
                "{TEST_ROOT}/campaign-id=placeholder/campaign-result.json"
            ),
            campaign_result_readback_url: format!(
                "{TEST_ROOT}/campaign-id=placeholder/campaign-result.json?readback=1"
            ),
        };
        request.campaign_id = expected_campaign_id(&request).unwrap();
        for round in &mut request.rounds {
            round.mission_put_url = format!(
                "{TEST_ROOT}/campaign-id={}/round={}/mission.json",
                request.campaign_id, round.round_id
            );
            round.mission_readback_url = format!(
                "{TEST_ROOT}/campaign-id={}/round={}/mission.json?readback=1",
                request.campaign_id, round.round_id
            );
            round.result_put_url = format!(
                "{TEST_ROOT}/campaign-id={}/round={}/results.zip",
                request.campaign_id, round.round_id
            );
            round.result_readback_url = format!(
                "{TEST_ROOT}/campaign-id={}/round={}/results.zip?readback=1",
                request.campaign_id, round.round_id
            );
        }
        request.holdout_claim_put_url =
            cex_global_holdout_claim_object(&request.holdout_id).unwrap();
        request.holdout_claim_readback_url = request.holdout_claim_put_url.clone();
        request.campaign_result_put_url = format!(
            "{TEST_ROOT}/campaign-id={}/campaign-result.json",
            request.campaign_id
        );
        request.campaign_result_readback_url = format!(
            "{TEST_ROOT}/campaign-id={}/campaign-result.json?readback=1",
            request.campaign_id
        );
        request
    }

    struct CampaignE2eFixture {
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
    ) -> CampaignE2eFixture {
        let render_fixture = mission_render::tests::Fixture::new(21_608);
        let mut rows = mission_render::tests::read_feature_rows(&render_fixture.feature_path);
        if zero_labels {
            for row in &mut rows {
                row.label = 0.0;
            }
        } else {
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
                row.label = direction * 0.001;
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
        );
        let rendered = render_cex_bundle(
            &render_fixture.feature_path,
            &render_fixture.materialization_path,
            7,
            crate::mission_render::max_candidates_for_tests() * 4,
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
            _root: root,
            _replay_root: replay_root,
            _render_fixture: render_fixture,
            replay_artifact_path,
            replay_manifest_path,
            args: CampaignExecuteArgs {
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
        let feature_sha256 = crate::mission_runner::sha256_file(feature_path).unwrap();
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
        CampaignRequest {
            schema_version: CAMPAIGN_REQUEST_SCHEMA_V3.to_string(),
            campaign_id: campaign_id.clone(),
            build_source_revision: BUILD_SOURCE_REVISION.to_string(),
            image_identity: "1".repeat(64),
            campaign_inputs_sha256: "f".repeat(64),
            producer_source_revision: BUILD_SOURCE_REVISION.to_string(),
            producer_image_identity: "e".repeat(64),
            feature_url: feature_path.to_string_lossy().into_owned(),
            feature_sha256: crate::mission_runner::sha256_file(feature_path).unwrap(),
            materialization_url: materialization_path.to_string_lossy().into_owned(),
            materialization_sha256: crate::mission_runner::sha256_file(materialization_path)
                .unwrap(),
            replay_artifact_url: replay_artifact_path.to_string_lossy().into_owned(),
            replay_artifact_sha256: crate::mission_runner::sha256_file(replay_artifact_path)
                .unwrap(),
            replay_manifest_url: replay_manifest_path.to_string_lossy().into_owned(),
            replay_manifest_sha256: crate::mission_runner::sha256_file(replay_manifest_path)
                .unwrap(),
            holdout_id: holdout_id.to_string(),
            declared_total_trials: crate::mission_render::max_candidates_for_tests()
                * 2
                * seeds.len(),
            rounds: seeds
                .iter()
                .enumerate()
                .map(|(index, seed)| CampaignRoundRequest {
                    round_id: format!("r{}", index + 1),
                    seed: *seed,
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
        let levels = serde_json::json!({
            "bids": [["59999", "10"], ["59998", "10"], ["59997", "10"], ["59996", "10"], ["59995", "10"]],
            "asks": [["60001", "10"], ["60002", "10"], ["60003", "10"], ["60004", "10"], ["60005", "10"]]
        });
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
                payloads.push(serde_json::to_string(&levels).unwrap());
            }
            timestamps.push(row.feature_available_time.timestamp_micros() + 100);
            sequences.push(i64::try_from(index + 2).unwrap());
            events.push("l2_update".to_string());
            payloads.push(serde_json::to_string(&levels).unwrap());
        }
        timestamps.push(rows.last().unwrap().label_available_time.timestamp_micros() + 100);
        sequences.push(i64::try_from(timestamps.len()).unwrap());
        events.push("l2_update".to_string());
        payloads.push(serde_json::to_string(&levels).unwrap());
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
            crate::mission_runner::sha256_file(&temporary_artifact).unwrap();
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
