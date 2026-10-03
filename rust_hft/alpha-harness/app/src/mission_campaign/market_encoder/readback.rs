//! Read back every stage, independent fit witness, seed-mean forecast and IOC replay.
use super::worker::{self, FoldResult};
use super::*;
use crate::mission_campaign::sequence::readback::{cached_download, verify_artifact_inventory};
use alpha_domain::campaign_control::CampaignAttemptOutcomeV1;
use std::collections::BTreeMap;

#[allow(clippy::too_many_arguments)]
pub(crate) fn readback(
    request: &MarketRequest,
    request_hash: &str,
    root_hash: &str,
    attempt_hash: &str,
    expected_job_uid: &str,
    input_root: &Path,
    cache: &Path,
    settled_hash: Option<&str>,
) -> anyhow::Result<(CampaignAttemptOutcomeV1, String, serde_json::Value)> {
    request.validate()?;
    std::fs::create_dir_all(cache)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .redirect(Policy::none())
        .build()?;
    // The published object, never a writable local cache, is the root of
    // independent evidence. Even a settled readback re-pins this small object.
    let (result_file, hash) =
        fresh_result(&client, &request.result_readback_url, cache, settled_hash)?;
    let result: FoldResult = read_json(result_file.path())?;
    validate_result(
        request,
        request_hash,
        root_hash,
        attempt_hash,
        expected_job_uid,
        &result,
    )?;
    // Even an existing settlement is rechecked from pinned artifacts; it is not model evidence.
    let bundle = cache.join("results.zip");
    cached_download(
        &client,
        &request.bundle_readback_url,
        &bundle,
        MAX_RESULT_BUNDLE_BYTES,
    )?;
    let temporary = tempfile::tempdir_in(cache)?;
    let extracted = temporary.path().join("extracted");
    extract_bundle_with_file_limit(&bundle, &extracted, worker::archive_file_limit(request)?)?;
    verify_published_result(&extracted, &hash)?;
    verify_artifact_inventory(&bundle, &extracted, worker::DIRECTORY, &result.artifacts)?;
    verify_restored(request, input_root, &extracted, temporary.path(), &result)?;
    let outcome = outcome(&result);
    let report = serde_json::json!({"schema_version":"monday.sol_market_encoder_model_report.v1","campaign_id":request.campaign_id,"request_sha256":request_hash,"result_sha256":hash,"root_grant_sha256":root_hash,"attempt_sha256":attempt_hash,"job_uid":result.job_uid,"job_name":result.job_name,"pod_uid":result.pod_uid,"job_deadline_at":result.job_deadline_at,"study_sha256":result.study_sha256,"fold_id":result.fold_id,"stages":result.stages,"groups":result.groups,"state":result.state,"primary_fits_attempted":result.primary_fits_attempted,"verification_fits_attempted":result.verification_fits_attempted,"charged_trials":result.charged_trials,"outcome":outcome,"sealed_holdout_opened":false,"deployment_authority":false,"independent_prediction_and_replay_readback":true,"full_study_qualified":false,"paired_mse_deltas":worker::comparison_report(&result.groups)});
    Ok((outcome, hash, report))
}
pub(super) fn validate_result(
    request: &MarketRequest,
    request_hash: &str,
    root_hash: &str,
    attempt_hash: &str,
    expected_job_uid: &str,
    result: &FoldResult,
) -> anyhow::Result<()> {
    let expected = worker::empty_result(request, request_hash, root_hash, attempt_hash)?;
    if result.schema_version != expected.schema_version
        || result.campaign_id != expected.campaign_id
        || result.request_sha256 != expected.request_sha256
        || result.root_grant_sha256 != expected.root_grant_sha256
        || result.attempt_sha256 != expected.attempt_sha256
        || result.study_sha256 != expected.study_sha256
        || result.fold_id != expected.fold_id
        || result.position_policy != expected.position_policy
        || result.job_uid != expected_job_uid
        || result.job_uid.is_empty()
        || result.job_name.is_empty()
        || result.pod_uid.is_empty()
        || result.job_deadline_at.is_none()
        || result.charged_trials != expected.charged_trials
        || result.sealed_holdout_opened
        || result.deployment_authority
        || result
            .stages
            .iter()
            .map(|s| s.binding.stage.clone())
            .collect::<Vec<_>>()
            != worker::stages(request)?
        || result
            .groups
            .iter()
            .map(|g| g.model_kind)
            .collect::<Vec<_>>()
            != worker::GROUPS
        || result.state != worker::fold_state(&result.groups)
        || result.artifacts.len() + 1 > worker::archive_file_limit(request)?
    {
        bail!("market result identity, authority, stage plan or comparison changed");
    }
    use alpha_domain::market_encoder_study::MarketTrainingStagePurposeV1 as Purpose;
    let count = |purpose| {
        result
            .stages
            .iter()
            .filter(|s| s.attempted && s.binding.stage.key.purpose == purpose)
            .count() as u64
    };
    if result.primary_fits_attempted != count(Purpose::Primary)
        || result.verification_fits_attempted != count(Purpose::Verification)
    {
        bail!("market actual fit counters differ from immutable stage receipts");
    }
    for (name, hash) in &result.artifacts {
        if name.contains('/')
            || !inputs::safe_relative_path(name)
            || !hft_research_manifest::sequence::valid_sha256(hash)
        {
            bail!("invalid market artifact identity");
        }
    }
    Ok(())
}
fn verify_restored(
    request: &MarketRequest,
    root: &Path,
    extracted: &Path,
    work: &Path,
    result: &FoldResult,
) -> anyhow::Result<()> {
    request.inputs.verify_mount(root)?;
    let fold = request
        .plan
        .folds
        .iter()
        .find(|f| f.fold_id == result.fold_id)
        .context("missing market fold")?;
    request
        .inputs
        .verify_view(root, &request.inputs.train, &fold.train)?;
    request
        .inputs
        .verify_view(root, &request.inputs.validation, &fold.validation.data)?;
    request
        .inputs
        .verify_replay(root, fold.validation.data.view)?;
    let mut checked = worker::empty_result(
        request,
        &result.request_sha256,
        &result.root_grant_sha256,
        &result.attempt_sha256,
    )?;
    let mut fitted = BTreeMap::new();
    let directory = extracted.join(worker::DIRECTORY);
    for receipt in &result.stages {
        let name = format!(
            "{}.receipt.json",
            worker::stage_name(receipt.binding.stage.key)
        );
        let stored: worker::StageReceipt = read_json(&directory.join(name))?;
        if stored != *receipt {
            bail!("market receipt differs from result");
        }
        worker::accept_stage(request, &directory, receipt, &mut checked, &mut fitted)?;
    }
    let output = work.join("independent");
    std::fs::create_dir(&output)?;
    for kind in worker::GROUPS {
        let group = worker::evaluate_verified_group(
            request,
            root,
            &output,
            kind,
            &fitted,
            &checked.stages,
            &mut checked.artifacts,
        )?;
        checked.groups.push(group);
    }
    checked.state = worker::fold_state(&checked.groups).into();
    if checked != *result {
        bail!("independent market stages, counts, predictions, replay or outcomes differ");
    }
    Ok(())
}
pub(super) fn outcome(result: &FoldResult) -> CampaignAttemptOutcomeV1 {
    if result.state == "development_fold_candidate" {
        CampaignAttemptOutcomeV1::SelectedPreHoldout
    } else if result
        .stages
        .iter()
        .any(|s| s.state != worker::StageState::Completed)
        || result.groups.iter().any(|g| {
            matches!(
                g.state.as_str(),
                "fit_failed"
                    | "skipped_dependency"
                    | "replay_failed"
                    | "incomplete_decision_grid"
                    | "invalid_accounting"
                    | "unverified_holding"
                    | "execution_gate_failed"
            )
        })
    {
        CampaignAttemptOutcomeV1::Failed
    } else {
        CampaignAttemptOutcomeV1::NoCandidate
    }
}

pub(super) fn fresh_result(
    client: &Client,
    url: &str,
    cache: &Path,
    settled_hash: Option<&str>,
) -> anyhow::Result<(tempfile::NamedTempFile, String)> {
    let file = tempfile::NamedTempFile::new_in(cache)?;
    let (_, hash) = fetch_to_file(client, url, file.path(), MAX_REQUEST_BYTES)?;
    if settled_hash.is_some_and(|expected| expected != hash) {
        bail!("settled market result changed");
    }
    Ok((file, hash))
}
pub(super) fn verify_published_result(extracted: &Path, remote_hash: &str) -> anyhow::Result<()> {
    if crate::mission_runner::sha256_file(&extracted.join("result.json"))? != remote_hash {
        bail!("market archive result differs from published result");
    }
    Ok(())
}
