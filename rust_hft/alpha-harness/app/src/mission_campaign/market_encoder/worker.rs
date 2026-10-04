//! One reserved development fold; stage receipts are immutable and attempt-bound.
use super::stage_permit::{self, StageAuthorization};
use super::*;
use crate::mission_campaign::sequence::{readback::verify_artifact_inventory, worker as shared};
use alpha_domain::{
    campaign_control::{
        verify_campaign_root_grant, CampaignAttemptReservationV1, SignedCampaignRootGrantV1,
        VerifiedCampaignRootGrant,
    },
    market_encoder_study::{
        MarketDataViewV1, MarketTrainingStageKeyV1 as Key, MarketTrainingStageKindV1 as Kind,
        MarketTrainingStagePurposeV1 as Purpose, MarketTrainingStageV1 as Stage,
    },
};
use alpha_engine::{
    market_encoder_study::{
        fit_market_stage, predict_market_validation, verify_market_stage_pair, FittedMarketStage,
        MarketPredictionCoverageV1, MarketStageInput, MarketStudyEnsemble, VerifiedMarketStage,
    },
    sequence_study::SOL_SEQUENCE_POSITION_POLICY,
};
use chrono::{DateTime, Utc};
use hft_backtest::engine::{TargetPositionDecision, TargetPositionReplayMetrics};
use hft_research_manifest::{
    market_encoder::MarketDataReadRequestV1,
    model::{HorizonHoldingPolicyV1, HorizonPositionState},
};
use std::collections::BTreeMap;

pub(crate) const DIRECTORY: &str = "market-encoder-results";
pub(super) const RESULT_SCHEMA: &str = "monday.sol_market_encoder_fold_result.v1";
pub(super) const GROUPS: [Kind; 5] = [
    Kind::Scratch,
    Kind::LinearProbe,
    Kind::FineTune,
    Kind::ScratchCompute,
    Kind::Ridge,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StageBinding {
    pub schema_version: String,
    pub stage: Stage,
    pub campaign_id: String,
    pub request_sha256: String,
    pub root_grant_sha256: String,
    pub attempt_sha256: String,
    pub study_sha256: String,
    pub source_revision: String,
    pub image_identity: String,
    pub dependencies: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StageState {
    Completed,
    FitFailed,
    VerificationFailed,
    SkippedDependency,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StageModel {
    pub manifest_sha256: String,
    pub weights_sha256: String,
    pub fitted_values_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StageReceipt {
    pub binding: StageBinding,
    pub authorization_sha256: String,
    pub state: StageState,
    pub attempted: bool,
    pub model: Option<StageModel>,
    pub diagnostic: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StageStart {
    pub binding: StageBinding,
    pub authorization_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeedDiagnostic {
    pub seed: u64,
    pub prediction: shared::SequenceHorizonDiagnosticV1,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupResult {
    pub model_kind: Kind,
    pub state: String,
    pub verified_members: usize,
    pub coverage: Option<MarketPredictionCoverageV1>,
    pub replay: Option<TargetPositionReplayMetrics>,
    pub report: Option<shared::SequenceGroupReportV1>,
    pub seed_diagnostics: Vec<SeedDiagnostic>,
    pub diagnostic: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FoldResult {
    pub schema_version: String,
    pub campaign_id: String,
    pub request_sha256: String,
    pub root_grant_sha256: String,
    pub attempt_sha256: String,
    pub study_sha256: String,
    pub fold_id: u8,
    pub position_policy: String,
    pub job_name: String,
    pub pod_uid: String,
    pub job_uid: String,
    pub job_deadline_at: Option<DateTime<Utc>>,
    pub primary_fits_attempted: u64,
    pub verification_fits_attempted: u64,
    pub charged_trials: u64,
    pub stages: Vec<StageReceipt>,
    pub groups: Vec<GroupResult>,
    pub state: String,
    pub artifacts: BTreeMap<String, String>,
    pub sealed_holdout_opened: bool,
    pub deployment_authority: bool,
}

pub(crate) fn execute(args: CampaignExecuteArgs) -> anyhow::Result<()> {
    let request: MarketRequest = read_json(&args.request)?;
    request.validate()?;
    if !args.pre_holdout
        || request.build_source_revision != BUILD_SOURCE_REVISION
        || request.campaign_id != args.campaign_id
        || request.image_identity != args.image_identity
        || hft_research_artifacts::sha256_file(&args.request)? != args.request_sha256
    {
        bail!("market worker source, mode or request identity changed");
    }
    let input_dir = args
        .request
        .parent()
        .context("market request has no directory")?;
    let signed: SignedCampaignRootGrantV1 = read_json(&input_dir.join("sequence-root-grant.json"))?;
    let keys: BTreeMap<String, String> = read_json(&input_dir.join("sequence-trusted-keys.json"))?;
    let trusted = keys
        .into_iter()
        .map(|(name, key)| {
            let bytes: [u8; 32] = hex::decode(key)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid market public key"))?;
            Ok((name, ed25519_dalek::VerifyingKey::from_bytes(&bytes)?))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let grant = verify_campaign_root_grant(&signed, &trusted, Utc::now())?;
    let attempt: CampaignAttemptReservationV1 =
        read_json(&input_dir.join("sequence-attempt.json"))?;
    grant.validate_attempt_scope(&attempt, Utc::now())?;
    if attempt.request_sha256 != args.request_sha256
        || attempt.campaign_id != request.campaign_id
        || attempt.declared_trials != request.declared_trials()?
        || attempt.policy_revision_id != request.policy_id()?
        || attempt.execution
            != crate::mission_dispatch::sequence_admission::execution_binding(
                &request,
                &grant.grant().execution.controller_image,
            )?
    {
        bail!("market worker differs from reserved authority");
    }
    let result = run_fold(
        &request,
        &args.request_sha256,
        &attempt.content_hash()?,
        &grant,
        Path::new("/sequence-inputs"),
        &args.work_dir,
    )?;
    let result_path = args.work_dir.join("result.json");
    persist_exact(&result_path, &serde_json::to_vec_pretty(&result)?)?;
    let bundle = args.work_dir.join("results.zip");
    if bundle.try_exists()? {
        let temporary = tempfile::tempdir_in(&args.work_dir)?;
        let extracted = temporary.path().join("extracted");
        extract_bundle_with_file_limit(&bundle, &extracted, archive_file_limit(&request)?)?;
        verify_artifact_inventory(&bundle, &extracted, DIRECTORY, &result.artifacts)?;
        if hft_research_artifacts::sha256_file(&extracted.join("result.json"))?
            != hft_research_artifacts::sha256_file(&result_path)?
        {
            bail!("existing market archive result differs");
        }
    } else {
        shared::pack_result_artifacts(&args.work_dir, &bundle, DIRECTORY, &result.artifacts)?;
    }
    grant.validate_active_at(Utc::now())?;
    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .redirect(Policy::none())
        .build()?;
    publish_immutable_file(&client, &request.bundle_put_url, &bundle, "application/zip")?;
    publish_immutable_file(
        &client,
        &request.result_put_url,
        &result_path,
        "application/json",
    )?;
    for (url, path, limit) in [
        (
            &request.bundle_readback_url,
            &bundle,
            MAX_RESULT_BUNDLE_BYTES,
        ),
        (
            &request.result_readback_url,
            &result_path,
            MAX_REQUEST_BYTES,
        ),
    ] {
        let readback = tempfile::NamedTempFile::new_in(&args.work_dir)?;
        fetch_to_file(&client, url, readback.path(), limit)?;
        if hft_research_artifacts::sha256_file(path)?
            != hft_research_artifacts::sha256_file(readback.path())?
        {
            bail!("market publication readback differs");
        }
    }
    print_json(
        &serde_json::json!({"campaign_id":request.campaign_id,"result_sha256":hft_research_artifacts::sha256_file(&result_path)?,"state":result.state,"primary_fits_attempted":result.primary_fits_attempted,"verification_fits_attempted":result.verification_fits_attempted,"charged_trials":result.charged_trials,"sealed_holdout_opened":false,"deployment_authority":false}),
    )
}

pub(crate) fn stages(request: &MarketRequest) -> anyhow::Result<Vec<Stage>> {
    Ok(request
        .plan
        .development_stages()
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .filter(|s| s.key.fold_id == request.inputs.fold_id)
        .collect())
}
pub(crate) fn stage_name(key: Key) -> String {
    format!(
        "fold-{}-{}-{}-{}",
        key.fold_id,
        kind_name(key.kind),
        key.seed,
        if key.purpose == Purpose::Primary {
            "primary"
        } else {
            "verification"
        }
    )
}
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Pretrain => "pretrain",
        Kind::Scratch => "scratch",
        Kind::LinearProbe => "linear_probe",
        Kind::FineTune => "fine_tune",
        Kind::ScratchCompute => "scratch_compute",
        Kind::Ridge => "ridge",
    }
}
pub(super) fn reader_request(
    request: &MarketRequest,
    data: &MarketDataViewV1,
) -> MarketDataReadRequestV1 {
    MarketDataReadRequestV1 {
        feature_dataset_sha256: data.features_sha256.clone(),
        qualified_anchors_sha256: data.qualified_anchors_sha256.clone(),
        input: request.plan.input.clone(),
        view: data.view,
        anchor_end_ms: data.view.end_ms - 30000,
    }
}
pub(crate) fn binding(
    request: &MarketRequest,
    result: &FoldResult,
    stage: Stage,
) -> anyhow::Result<StageBinding> {
    let dependencies = stage
        .prerequisites()
        .into_iter()
        .map(|key| {
            let name = format!("{}.receipt.json", stage_name(key));
            let hash = result
                .artifacts
                .get(&name)
                .context("missing prerequisite receipt")?
                .clone();
            Ok((name, hash))
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(StageBinding {
        schema_version: "monday.market_stage_binding.v1".into(),
        stage,
        campaign_id: request.campaign_id.clone(),
        request_sha256: result.request_sha256.clone(),
        root_grant_sha256: result.root_grant_sha256.clone(),
        attempt_sha256: result.attempt_sha256.clone(),
        study_sha256: result.study_sha256.clone(),
        source_revision: request.build_source_revision.clone(),
        image_identity: request.image_identity.clone(),
        dependencies,
    })
}
pub(super) fn ready(stage: &Stage, receipts: &[StageReceipt]) -> bool {
    stage.prerequisites().iter().all(|key| {
        receipts
            .iter()
            .any(|r| r.binding.stage.key == *key && r.state == StageState::Completed)
    })
}
pub(super) fn parent<'a>(
    stage: &Stage,
    fitted: &'a BTreeMap<Key, FittedMarketStage>,
) -> anyhow::Result<Option<VerifiedMarketStage<'a>>> {
    stage
        .parent
        .map(|key| {
            verify_market_stage_pair(
                fitted.get(&key).context("missing primary parent")?,
                fitted
                    .get(&Key {
                        purpose: Purpose::Verification,
                        ..key
                    })
                    .context("missing verification parent")?,
            )
            .map_err(anyhow::Error::msg)
        })
        .transpose()
}
pub(crate) fn empty_result(
    request: &MarketRequest,
    request_hash: &str,
    root_hash: &str,
    attempt_hash: &str,
) -> anyhow::Result<FoldResult> {
    Ok(FoldResult {
        schema_version: RESULT_SCHEMA.into(),
        campaign_id: request.campaign_id.clone(),
        request_sha256: request_hash.into(),
        root_grant_sha256: root_hash.into(),
        attempt_sha256: attempt_hash.into(),
        study_sha256: request.plan.content_hash().map_err(anyhow::Error::msg)?,
        fold_id: request.inputs.fold_id,
        position_policy: SOL_SEQUENCE_POSITION_POLICY.into(),
        job_name: String::new(),
        pod_uid: String::new(),
        job_uid: String::new(),
        job_deadline_at: None,
        primary_fits_attempted: 0,
        verification_fits_attempted: 0,
        charged_trials: request.declared_trials()?,
        stages: Vec::new(),
        groups: Vec::new(),
        state: "pending".into(),
        artifacts: BTreeMap::new(),
        sealed_holdout_opened: false,
        deployment_authority: false,
    })
}
pub(super) fn run_fold(
    request: &MarketRequest,
    request_hash: &str,
    attempt_hash: &str,
    grant: &VerifiedCampaignRootGrant,
    root: &Path,
    output: &Path,
) -> anyhow::Result<FoldResult> {
    let results_dir = output.join(DIRECTORY);
    std::fs::create_dir_all(&results_dir)?;
    request.inputs.verify_mount(root)?;
    let fold = request
        .plan
        .folds
        .iter()
        .find(|f| f.fold_id == request.inputs.fold_id)
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
    let mut result = empty_result(request, request_hash, grant.content_sha256(), attempt_hash)?;
    let mut fitted = BTreeMap::new();
    let authority = stage_permit::FileStageAuthority::from_environment(
        request,
        request_hash,
        attempt_hash,
        grant.content_sha256(),
        grant.grant().expires_at,
        output,
    )?;
    for stage in stages(request)? {
        grant.validate_active_at(Utc::now())?;
        execute_stage(
            request,
            &results_dir,
            stage,
            &mut result,
            &mut fitted,
            |stage| authority.authorize(stage),
            |stage, parent| {
                if stage.key.kind == Kind::Pretrain {
                    let mut reader = inputs::open_feature_reader(
                        root,
                        &request.inputs.train,
                        &reader_request(request, &fold.train),
                    )
                    .map_err(|e| e.to_string())?;
                    fit_market_stage(
                        &request.plan,
                        stage.key,
                        MarketStageInput::Features(&mut reader),
                        parent,
                    )
                } else {
                    let mut reader = inputs::open_task_reader(
                        root,
                        &request.inputs.train,
                        &reader_request(request, &fold.train),
                    )
                    .map_err(|e| e.to_string())?;
                    fit_market_stage(
                        &request.plan,
                        stage.key,
                        MarketStageInput::Targets(&mut reader),
                        parent,
                    )
                }
            },
        )?;
        authority.validate_runtime_binding(&result.job_name, &result.pod_uid)?;
    }
    let temporary = tempfile::tempdir_in(output)?;
    for kind in GROUPS {
        grant.validate_active_at(Utc::now())?;
        let mut artifacts = BTreeMap::new();
        let group = evaluate_verified_group(
            request,
            root,
            temporary.path(),
            kind,
            &fitted,
            &result.stages,
            &mut artifacts,
        )?;
        for (name, hash) in artifacts {
            persist_exact(
                &results_dir.join(&name),
                &std::fs::read(temporary.path().join(&name))?,
            )?;
            result.artifacts.insert(name, hash);
        }
        result.groups.push(group);
    }
    result.state = fold_state(&result.groups).into();
    let actual = std::fs::read_dir(&results_dir)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<anyhow::Result<std::collections::BTreeSet<_>>>()?;
    if actual != result.artifacts.keys().cloned().collect() {
        bail!("market output contains unaccounted or partial artifacts");
    }
    Ok(result)
}
pub(crate) fn validate_receipt(
    receipt: &StageReceipt,
    expected: &StageBinding,
    ready: bool,
) -> anyhow::Result<()> {
    let failed = matches!(
        receipt.state,
        StageState::FitFailed | StageState::VerificationFailed | StageState::SkippedDependency
    );
    if receipt.binding != *expected
        || !hft_research_manifest::sequence::valid_sha256(&receipt.authorization_sha256)
        || receipt.attempted == (receipt.state == StageState::SkippedDependency)
        || (receipt.state == StageState::SkippedDependency) == ready
        || receipt.model.is_some()
            != matches!(
                receipt.state,
                StageState::Completed | StageState::VerificationFailed
            )
        || (receipt.state == StageState::VerificationFailed
            && expected.stage.key.purpose != Purpose::Verification)
        || failed != receipt.diagnostic.is_some()
        || receipt
            .diagnostic
            .as_ref()
            .is_some_and(|d| d.is_empty() || d.len() > 8192)
    {
        bail!("market stage completion receipt changed its identity, dependency or accounting");
    }
    if let Some(model) = &receipt.model {
        for hash in [
            &model.manifest_sha256,
            &model.weights_sha256,
            &model.fitted_values_sha256,
        ] {
            if !hft_research_manifest::sequence::valid_sha256(hash) {
                bail!("invalid market model identity");
            }
        }
    }
    Ok(())
}
pub(super) fn accept_stage(
    request: &MarketRequest,
    directory: &Path,
    receipt: &StageReceipt,
    result: &mut FoldResult,
    fitted: &mut BTreeMap<Key, FittedMarketStage>,
) -> anyhow::Result<()> {
    verify_stage_record(request, directory, receipt, result)?;
    let stage = &receipt.binding.stage;
    let name = stage_name(stage.key);
    if let Some(model) = &receipt.model {
        let manifest = inputs::Artifact {
            file: format!("{name}.json"),
            sha256: model.manifest_sha256.clone(),
        }
        .read(directory, 4 * 1024 * 1024)?;
        let weights = inputs::Artifact {
            file: format!("{name}.weights"),
            sha256: model.weights_sha256.clone(),
        }
        .read(directory, 16 * 1024 * 1024)?;
        let parent = parent(stage, fitted)?;
        let restored = FittedMarketStage::restore(
            &request.plan,
            stage.key,
            &manifest,
            &model.manifest_sha256,
            weights,
            parent.as_ref(),
        )
        .map_err(anyhow::Error::msg)?;
        if restored
            .fitted_values_digest()
            .map_err(anyhow::Error::msg)?
            != model.fitted_values_sha256
        {
            bail!("market stage fitted value identity differs");
        }
        if let Some(primary) = stage.verification_of {
            let verified = verify_market_stage_pair(
                fitted.get(&primary).context("missing stage primary")?,
                &restored,
            )
            .is_ok();
            if verified != (receipt.state == StageState::Completed) {
                bail!("independent market fit comparison differs from its receipt");
            }
        }
        fitted.insert(stage.key, restored);
    }
    Ok(())
}

/// The native controller can check the completed prefix without loading models.
/// All files and historical permits are rechecked; model semantics are checked
/// independently by accept_stage/readback.
pub(crate) fn verify_stage_record(
    request: &MarketRequest,
    directory: &Path,
    receipt: &StageReceipt,
    result: &mut FoldResult,
) -> anyhow::Result<()> {
    let stage = &receipt.binding.stage;
    let expected_stage = stages(request)?
        .get(result.stages.len())
        .cloned()
        .context("stage receipt exceeds the fixed plan")?;
    if *stage != expected_stage {
        bail!("market stage completion is not the next declared stage");
    }
    validate_receipt(
        receipt,
        &binding(request, result, stage.clone())?,
        ready(stage, &result.stages),
    )?;
    let name = stage_name(stage.key);
    let stored: StageReceipt = read_json(&directory.join(format!("{name}.receipt.json")))?;
    if stored != *receipt {
        bail!("market receipt bytes differ from declared completion");
    }
    let bytes = inputs::Artifact {
        file: format!("{name}.permit.json"),
        sha256: receipt.authorization_sha256.clone(),
    }
    .read(directory, 64 * 1024)?;
    let authorization: StageAuthorization = serde_json::from_slice(&bytes)?;
    validate_stage_authorization(request, result, stage, &authorization)?;
    let mut next = result.clone();
    pin_job(&mut next, &authorization);
    next.artifacts.insert(
        format!("{name}.permit.json"),
        receipt.authorization_sha256.clone(),
    );
    if receipt.attempted {
        let start: StageStart = read_json(&directory.join(format!("{name}.start.json")))?;
        if start.binding != receipt.binding
            || start.authorization_sha256 != receipt.authorization_sha256
        {
            bail!("market stage start record differs from its permission");
        }
        track(&mut next.artifacts, directory, format!("{name}.start.json"))?;
        match stage.key.purpose {
            Purpose::Primary => next.primary_fits_attempted += 1,
            Purpose::Verification => next.verification_fits_attempted += 1,
        }
    } else if directory.join(format!("{name}.start.json")).try_exists()? {
        bail!("skipped market stage has a fit start marker");
    }
    if let Some(model) = &receipt.model {
        for (suffix, hash, limit) in [
            ("json", &model.manifest_sha256, 4 * 1024 * 1024),
            ("weights", &model.weights_sha256, 16 * 1024 * 1024),
        ] {
            inputs::Artifact {
                file: format!("{name}.{suffix}"),
                sha256: hash.clone(),
            }
            .read(directory, limit)?;
            next.artifacts
                .insert(format!("{name}.{suffix}"), hash.clone());
        }
    }
    if receipt.model.is_none() {
        for suffix in ["json", "weights"] {
            if directory.join(format!("{name}.{suffix}")).try_exists()? {
                bail!("failed or skipped market stage has unaccounted model artifacts");
            }
        }
    }
    track(
        &mut next.artifacts,
        directory,
        format!("{name}.receipt.json"),
    )?;
    next.stages.push(receipt.clone());
    *result = next;
    Ok(())
}

pub(crate) fn validate_stage_authorization(
    request: &MarketRequest,
    result: &FoldResult,
    stage: &Stage,
    authorization: &StageAuthorization,
) -> anyhow::Result<()> {
    let permit = &authorization.permit;
    let challenge = &permit.request;
    alpha_domain::campaign_stage::verify_stage_permit(
        &request.stage_authority,
        challenge,
        permit,
        authorization.accepted_at,
    )
    .map_err(anyhow::Error::msg)?;
    if authorization.schema_version != stage_permit::AUTHORIZATION_SCHEMA
        || challenge.request_sha256 != result.request_sha256
        || challenge.attempt_sha256 != result.attempt_sha256
        || challenge.root_grant_sha256 != result.root_grant_sha256
        || challenge.stage != stage.key
        || (!result.job_uid.is_empty()
            && (result.job_uid != permit.job_uid
                || result.job_name != challenge.job_name
                || result.pod_uid != challenge.pod_uid
                || result.job_deadline_at != Some(permit.job_deadline_at)))
    {
        bail!("market stage permission changed its attempt, stage or admitted Job");
    }
    Ok(())
}
fn pin_job(result: &mut FoldResult, authorization: &StageAuthorization) {
    result.job_uid = authorization.permit.job_uid.clone();
    result.job_name = authorization.permit.request.job_name.clone();
    result.pod_uid = authorization.permit.request.pod_uid.clone();
    result.job_deadline_at = Some(authorization.permit.job_deadline_at);
}

fn track(
    artifacts: &mut BTreeMap<String, String>,
    directory: &Path,
    name: String,
) -> anyhow::Result<()> {
    artifacts.insert(
        name.clone(),
        hft_research_artifacts::sha256_file(&directory.join(name))?,
    );
    Ok(())
}
fn persist_exact(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if path.try_exists()? {
        if !path.is_file()
            || path.metadata()?.len() != bytes.len() as u64
            || hft_research_artifacts::sha256_file(path)? != format!("{:x}", Sha256::digest(bytes))
        {
            bail!(
                "existing immutable market artifact differs: {}",
                path.display()
            );
        }
        Ok(())
    } else {
        stage_permit::write_atomic_new(path, bytes)
    }
}

pub(super) fn evaluate_verified_group(
    request: &MarketRequest,
    root: &Path,
    output: &Path,
    kind: Kind,
    fitted: &BTreeMap<Key, FittedMarketStage>,
    receipts: &[StageReceipt],
    artifacts: &mut BTreeMap<String, String>,
) -> anyhow::Result<GroupResult> {
    let seeds = if kind == Kind::Ridge {
        vec![0]
    } else {
        request.plan.seeds.clone()
    };
    let mut verified = Vec::new();
    for seed in &seeds {
        let key = Key {
            fold_id: request.inputs.fold_id,
            kind,
            seed: *seed,
            purpose: Purpose::Primary,
        };
        let verification = Key {
            purpose: Purpose::Verification,
            ..key
        };
        if [key, verification].iter().all(|key| {
            receipts
                .iter()
                .any(|r| r.binding.stage.key == *key && r.state == StageState::Completed)
        }) {
            verified.push(
                verify_market_stage_pair(
                    fitted.get(&key).context("missing group primary")?,
                    fitted
                        .get(&verification)
                        .context("missing group verification")?,
                )
                .map_err(anyhow::Error::msg)?,
            );
        }
    }
    if verified.len() != seeds.len() {
        let skipped = receipts
            .iter()
            .filter(|r| {
                r.binding.stage.key.kind == kind && r.binding.stage.key.purpose == Purpose::Primary
            })
            .all(|r| r.state == StageState::SkippedDependency);
        return Ok(GroupResult {
            model_kind: kind,
            state: if skipped {
                "skipped_dependency"
            } else {
                "fit_failed"
            }
            .into(),
            verified_members: verified.len(),
            coverage: None,
            replay: None,
            report: None,
            seed_diagnostics: Vec::new(),
            diagnostic: Some("the complete fixed seed set did not independently verify".into()),
        });
    }
    let ensemble = MarketStudyEnsemble::new(&request.plan, verified.iter().collect())
        .map_err(anyhow::Error::msg)?;
    evaluate_group(
        request,
        root,
        output,
        &ensemble,
        kind,
        seeds.len(),
        artifacts,
    )
}
fn evaluate_group(
    request: &MarketRequest,
    root: &Path,
    output: &Path,
    ensemble: &MarketStudyEnsemble<'_>,
    kind: Kind,
    members: usize,
    artifacts: &mut BTreeMap<String, String>,
) -> anyhow::Result<GroupResult> {
    let fold = request
        .plan
        .folds
        .iter()
        .find(|f| f.fold_id == request.inputs.fold_id)
        .context("missing market evaluation fold")?;
    // run_fold/readback admitted the native sources once; each opened reader
    // still pins and checks its manifests and all streamed shard bytes.
    let name = kind_name(kind);
    let predictions_name = format!("{name}-predictions.jsonl");
    let mut predictions = std::io::BufWriter::new(
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(output.join(&predictions_name))?,
    );
    let mut reader = inputs::open_task_reader(
        root,
        &request.inputs.validation,
        &reader_request(request, &fold.validation.data),
    )?;
    let mut moments = shared::HorizonMoments::default();
    let seeds = if kind == Kind::Ridge {
        vec![0]
    } else {
        request.plan.seeds.clone()
    };
    let mut per_seed = seeds
        .iter()
        .map(|seed| (*seed, shared::HorizonMoments::default()))
        .collect::<BTreeMap<_, _>>();
    let holding = HorizonHoldingPolicyV1 {
        horizon_millis: 30000,
    };
    let mut position = HorizonPositionState::default();
    let mut decisions = Vec::new();
    let coverage = predict_market_validation(&request.plan, ensemble, &mut reader, |row| {
        moments.observe(row.predicted_return, f64::from(row.observed_return))?;
        if row
            .member_returns
            .iter()
            .map(|(seed, _)| *seed)
            .collect::<Vec<_>>()
            != seeds
        {
            return Err("market prediction diagnostic changed the fixed seed set".into());
        }
        for (seed, predicted) in &row.member_returns {
            per_seed
                .get_mut(seed)
                .ok_or("missing market diagnostic seed")?
                .observe(*predicted, f64::from(row.observed_return))?;
        }
        let opening = ensemble.entry_decision(&request.plan, &row)?;
        let entry = opening.entry_target.unwrap_or(0.0);
        let action = position.advance(
            &holding,
            opening
                .timestamp_us
                .try_into()
                .map_err(|_| "negative market clock")?,
            true,
            Some(entry),
        )?;
        decisions.push(TargetPositionDecision {
            timestamp_us: opening.timestamp_us,
            entry_target: Some(entry),
            target_position: action.target(),
        });
        serde_json::to_writer(&mut predictions, &row).map_err(|e| e.to_string())?;
        predictions.write_all(b"\n").map_err(|e| e.to_string())
    })
    .map_err(anyhow::Error::msg)?;
    predictions.flush()?;
    track(artifacts, output, predictions_name)?;
    let mut group = GroupResult {
        model_kind: kind,
        state: "incomplete_decision_grid".into(),
        verified_members: members,
        coverage: Some(coverage.clone()),
        replay: None,
        report: None,
        seed_diagnostics: Vec::new(),
        diagnostic: None,
    };
    if coverage.emitted > 0 {
        group.seed_diagnostics = per_seed
            .into_iter()
            .map(|(seed, moments)| {
                Ok(SeedDiagnostic {
                    seed,
                    prediction: moments.finish(30000).map_err(anyhow::Error::msg)?,
                })
            })
            .collect::<anyhow::Result<_>>()?;
    }
    if !coverage.complete {
        return Ok(group);
    }
    let report = shared::SequenceGroupReportV1 {
        schema_version: "monday.sol_market_encoder_group_report.v1".into(),
        horizons: vec![moments.finish(30000).map_err(anyhow::Error::msg)?],
        blocks: Vec::new(),
        fees: None,
        stresses: Vec::new(),
        largest_block_abs_net_return_share: None,
        uncertainty_note: shared::REPORT_NOTE.into(),
    };
    group.report = Some(report);
    let mut next = decisions
        .last()
        .context("complete market grid has no decisions")?
        .timestamp_us
        .checked_add(1_000_000)
        .context("market tail clock overflow")?;
    let end_us = fold
        .validation
        .data
        .view
        .end_ms
        .checked_mul(1000)
        .context("market end clock overflow")?;
    while next < end_us {
        let action = position
            .advance(&holding, next.try_into()?, false, None)
            .map_err(anyhow::Error::msg)?;
        decisions.push(TargetPositionDecision {
            timestamp_us: next,
            entry_target: Some(0.0),
            target_position: action.target(),
        });
        next = next
            .checked_add(1_000_000)
            .context("market tail clock overflow")?;
    }
    let (state, replay, diagnostic) = shared::replay_group(
        root,
        &request.inputs.replay_artifact,
        &request.inputs.replay_manifest,
        &request.plan.costs,
        end_us,
        &decisions,
        name,
        output,
        artifacts,
        group.report.as_mut().context("missing market report")?,
    )?;
    group.state = state;
    group.replay = replay;
    group.diagnostic = diagnostic;
    Ok(group)
}
/// Only the fixed C route can qualify this fold. This is never full-study or final authorization.
pub(super) fn fold_state(groups: &[GroupResult]) -> &'static str {
    let find = |kind| groups.iter().find(|g| g.model_kind == kind);
    let Some(c) = find(Kind::FineTune) else {
        return "incomplete_comparison";
    };
    if c.state != "development_candidate" {
        return "no_development_candidate";
    }
    let rmse = |g: &GroupResult| {
        g.coverage
            .as_ref()
            .filter(|c| c.complete)
            .and(g.report.as_ref())
            .and_then(|r| r.horizons.iter().find(|h| h.horizon_ms == 30000))
            .map(|h| h.rmse)
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    let Some(c_mse) = rmse(c) else {
        return "incomplete_comparison";
    };
    let controls = [find(Kind::Scratch), find(Kind::ScratchCompute)];
    if controls.iter().any(|g| g.and_then(rmse).is_none()) {
        return "incomplete_comparison";
    }
    // Squaring nonnegative RMSE is monotonic; compare without introducing overflow.
    if controls
        .iter()
        .all(|g| g.and_then(rmse).is_some_and(|mse| c_mse < mse))
    {
        "development_fold_candidate"
    } else {
        "no_predictive_increment"
    }
}
/// Each stage has permit/start/receipt/manifest/weights; each group has prediction/config/trace.
pub(super) fn archive_file_limit(request: &MarketRequest) -> anyhow::Result<usize> {
    Ok(stages(request)?.len() * 5 + GROUPS.len() * 3 + 1)
}

/// Load a complete stage or execute it once. The start marker survives an interrupted fit.
pub(super) fn execute_stage(
    request: &MarketRequest,
    results_dir: &Path,
    stage: Stage,
    result: &mut FoldResult,
    fitted: &mut BTreeMap<Key, FittedMarketStage>,
    authorize: impl FnOnce(&Stage) -> anyhow::Result<StageAuthorization>,
    fit: impl FnOnce(&Stage, Option<&VerifiedMarketStage<'_>>) -> Result<FittedMarketStage, String>,
) -> anyhow::Result<()> {
    if stages(request)?.get(result.stages.len()) != Some(&stage) {
        bail!("market stage is not the next stage in its fixed plan");
    }
    let expected = binding(request, result, stage.clone())?;
    let name = stage_name(stage.key);
    let receipt_path = results_dir.join(format!("{name}.receipt.json"));
    let receipt = if receipt_path.try_exists()? {
        let receipt: StageReceipt = read_json(&receipt_path)?;
        validate_receipt(&receipt, &expected, ready(&stage, &result.stages))?;
        receipt
    } else {
        // A start without a complete receipt is an interrupted fit, not permission to rerun.
        ensure_unstarted(results_dir, stage.key)?;
        let authorization = authorize(&stage)?;
        validate_stage_authorization(request, result, &stage, &authorization)?;
        // A valid historical permit is evidence, but cannot start a new fit now.
        alpha_domain::campaign_stage::verify_stage_permit(
            &request.stage_authority,
            &authorization.permit.request,
            &authorization.permit,
            Utc::now(),
        )
        .map_err(anyhow::Error::msg)?;
        let authorization_bytes = serde_json::to_vec_pretty(&authorization)?;
        let authorization_sha256 = format!("{:x}", Sha256::digest(&authorization_bytes));
        stage_permit::write_atomic_new(
            &results_dir.join(format!("{name}.permit.json")),
            &authorization_bytes,
        )?;
        if !ready(&stage, &result.stages) {
            StageReceipt {
                binding: expected,
                authorization_sha256,
                state: StageState::SkippedDependency,
                attempted: false,
                model: None,
                diagnostic: Some("required stage did not complete and verify".into()),
            }
        } else {
            stage_permit::write_atomic_new(
                &results_dir.join(format!("{name}.start.json")),
                &serde_json::to_vec_pretty(&StageStart {
                    binding: expected.clone(),
                    authorization_sha256: authorization_sha256.clone(),
                })?,
            )?;
            research_event(
                "alpha-harness",
                "market_encoder_stage_started",
                serde_json::json!({"campaign_id":request.campaign_id,"stage":stage,"attempt_sha256":result.attempt_sha256}),
            );
            let parent = parent(&stage, fitted)?;
            alpha_domain::campaign_stage::verify_stage_permit(
                &request.stage_authority,
                &authorization.permit.request,
                &authorization.permit,
                Utc::now(),
            )
            .map_err(anyhow::Error::msg)?;
            let fit = fit(&stage, parent.as_ref());
            match fit {
                Err(error) => StageReceipt {
                    binding: expected,
                    authorization_sha256,
                    state: StageState::FitFailed,
                    attempted: true,
                    model: None,
                    diagnostic: Some(error.chars().take(2048).collect()),
                },
                Ok(model) => {
                    let (metadata, weights) = model.bundle().map_err(anyhow::Error::msg)?;
                    stage_permit::write_atomic_new(
                        &results_dir.join(format!("{name}.json")),
                        &metadata,
                    )?;
                    stage_permit::write_atomic_new(
                        &results_dir.join(format!("{name}.weights")),
                        &weights,
                    )?;
                    let check = stage
                        .verification_of
                        .map(|key| {
                            verify_market_stage_pair(&fitted[&key], &model)
                                .map(|_| ())
                                .map_err(anyhow::Error::msg)
                        })
                        .transpose();
                    let diagnostic = check
                        .err()
                        .map(|e| format!("{e:#}").chars().take(2048).collect());
                    StageReceipt {
                        binding: expected,
                        authorization_sha256,
                        state: if diagnostic.is_some() {
                            StageState::VerificationFailed
                        } else {
                            StageState::Completed
                        },
                        attempted: true,
                        model: Some(StageModel {
                            manifest_sha256: format!("{:x}", Sha256::digest(&metadata)),
                            weights_sha256: format!("{:x}", Sha256::digest(&weights)),
                            fitted_values_sha256: model
                                .fitted_values_digest()
                                .map_err(anyhow::Error::msg)?,
                        }),
                        diagnostic,
                    }
                }
            }
        }
    };
    persist_exact(&receipt_path, &serde_json::to_vec_pretty(&receipt)?)?;
    // Use restored, externally pinned bundles for descendants and predictions, even in the first run.
    accept_stage(request, results_dir, &receipt, result, fitted)?;
    Ok(())
}

pub(super) fn ensure_unstarted(directory: &Path, key: Key) -> anyhow::Result<()> {
    let name = stage_name(key);
    for suffix in ["start.json", "json", "weights", "permit.json"] {
        if directory.join(format!("{name}.{suffix}")).try_exists()? {
            bail!("incomplete market stage {name}; an explicit bounded recovery is required");
        }
    }
    Ok(())
}

/// Exploratory paired differences only. They never select a seed or loosen a gate.
pub(super) fn comparison_report(groups: &[GroupResult]) -> serde_json::Value {
    let Some(c) = groups.iter().find(|g| g.model_kind == Kind::FineTune) else {
        return serde_json::json!([]);
    };
    let mse = |g: &GroupResult| {
        g.coverage
            .as_ref()
            .filter(|c| c.complete)
            .and(g.report.as_ref())
            .and_then(|r| r.horizons.first())
            .map(|h| h.rmse * h.rmse)
            .filter(|v| v.is_finite())
    };
    let Some(c_mse) = mse(c) else {
        return serde_json::json!([]);
    };
    let comparisons = [Kind::Scratch, Kind::LinearProbe, Kind::ScratchCompute].into_iter().filter_map(|kind| {
        let control = groups.iter().find(|g| g.model_kind == kind)?;
        let control_mse = mse(control)?;
        let seeds = c.seed_diagnostics.iter().filter_map(|s| {
            let other = control.seed_diagnostics.iter().find(|o| o.seed == s.seed)?;
            let delta = s.prediction.rmse * s.prediction.rmse - other.prediction.rmse * other.prediction.rmse;
            delta.is_finite().then(|| serde_json::json!({"seed":s.seed,"mse_delta":delta}))
        }).collect::<Vec<_>>();
        Some(serde_json::json!({"route":"fine_tune","control":kind,"seed_mean_mse_delta":c_mse-control_mse,"paired_seed_mse_deltas":seeds}))
    }).collect::<Vec<_>>();
    serde_json::json!(comparisons)
}
