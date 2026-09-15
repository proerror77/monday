//! Reusable, declarative preparation for existing native Campaigns. No signer,
//! ledger, Kubernetes client or training entrypoint is called from this module.
use super::*;
use crate::cli::CampaignPrepareArgs;
use crate::mission_render::PreparedCexInputMetadata;
use std::collections::BTreeSet;

const PLAN_SCHEMA: &str = "monday.cex_campaign_preparation_plan.v1";
const INDEX_SCHEMA: &str = "monday.cex_campaign_preparation.v1";
const INPUT_SCHEMA: &str = "monday.cex_prepared_inputs.v1";
const MAX_MEMBERS: usize = 32;
const MAX_METADATA_BYTES: u64 = 8 * crate::mission_runner::MAX_MATERIALIZATION_BYTES + 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileRef {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MlpTreatment {
    updates: usize,
    learning_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    id: String,
    /// Full native plan only when this member differs beyond an MLP treatment.
    #[serde(default)]
    research_plan: Option<CexCampaignResearchPlanV1>,
    #[serde(default)]
    mlp: Option<MlpTreatment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparationPlan {
    schema_version: String,
    source_revision: String,
    image: String,
    campaign_root: String,
    campaign_inputs: FileRef,
    input_root: PathBuf,
    /// Optional independently retained input receipt from an earlier preparation.
    #[serde(default)]
    prepared_inputs: Option<FileRef>,
    seeds: Vec<u64>,
    base_research_plan: CexCampaignResearchPlanV1,
    members: Vec<Member>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedInputs {
    preparation_authentication_tag: Option<String>,
    schema_version: String,
    source_revision: String,
    image_identity: String,
    campaign_inputs_sha256: String,
    /// Only a metadata summary. Its digest must be pinned by the caller before reuse.
    render_metadata: PreparedCexInputMetadata,
}

fn authentication_payload<T: Serialize>(value: &T) -> anyhow::Result<String> {
    let mut value = serde_json::to_value(value)?;
    value
        .as_object_mut()
        .context("preparation attestation object")?
        .remove("preparation_authentication_tag");
    Ok(canonical_json_hash(&value)?)
}

pub(super) fn authenticate<T: Serialize>(
    ledger: &alpha_store::AlphaStore,
    value: &T,
) -> anyhow::Result<String> {
    Ok(ledger.attest_research_preparation(&authentication_payload(value)?)?)
}

pub(super) fn verify_authentication<T: Serialize>(
    ledger: &alpha_store::AlphaStore,
    value: &T,
    tag: Option<&str>,
) -> anyhow::Result<()> {
    let tag = tag.context("prepared evidence lacks a trusted preparation attestation")?;
    ledger
        .verify_research_preparation(&authentication_payload(value)?, tag)
        .context("preparation attestation does not match the trusted ledger")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedMember {
    pub id: String,
    pub research_plan: FileRef,
    pub freeze: FileRef,
    pub campaign_id: String,
    pub declared_trials: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparationIndex {
    pub ledger: PathBuf,
    pub schema_version: String,
    pub plan_sha256: String,
    pub source_revision: String,
    pub image: String,
    pub campaign_root: String,
    pub campaign_inputs: FileRef,
    pub input_root: PathBuf,
    pub prepared_inputs: FileRef,
    // Strings are intentional: orchestration JSON tools must not round u64 seeds.
    pub seeds: Vec<String>,
    pub members: Vec<PreparedMember>,
}

fn bytes(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut output)?;
    if output.len() as u64 > limit {
        bail!("preparation metadata exceeds its byte limit");
    }
    Ok(output)
}

pub(crate) fn verified_bytes(
    reference: &FileRef,
    base: &Path,
    limit: u64,
) -> anyhow::Result<Vec<u8>> {
    let expected = normalized_sha256("preparation metadata", &reference.sha256)?;
    let path = base.join(&reference.path);
    if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
        bail!("preparation metadata cannot be a symbolic link");
    }
    let value = bytes(&path, limit)?;
    if hex::encode(Sha256::digest(&value)) != expected {
        bail!("preparation metadata SHA256 mismatch");
    }
    Ok(value)
}

fn publish_bytes(root: &Path, relative: &Path, value: &[u8]) -> anyhow::Result<FileRef> {
    let path = root.join(relative);
    data_mission::ensure_real_directory(
        path.parent()
            .context("preparation artifact has no parent")?,
        "preparation artifact",
    )?;
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || bytes(&path, MAX_METADATA_BYTES)? != value
        {
            bail!("existing preparation artifact differs; refusing to replace evidence");
        }
    } else {
        let mut staged = data_mission::temporary_output_file(&path, ".prepared-")?;
        staged.write_all(value)?;
        staged.as_file().sync_all()?;
        staged.persist_noclobber(&path).map_err(|e| e.error)?;
    }
    Ok(FileRef {
        path: relative.into(),
        sha256: hex::encode(Sha256::digest(value)),
    })
}

fn publish<T: Serialize>(root: &Path, relative: &Path, value: &T) -> anyhow::Result<FileRef> {
    // Match native freeze/finalize bytes, including the absence of an extra newline.
    let value = serde_json::to_vec_pretty(value)?;
    if value.len() as u64 > MAX_METADATA_BYTES {
        bail!("prepared metadata exceeds its byte limit");
    }
    publish_bytes(root, relative, &value)
}

fn member_plans(plan: &PreparationPlan) -> anyhow::Result<Vec<CexCampaignResearchPlanV1>> {
    if plan.schema_version != PLAN_SCHEMA
        || plan.members.is_empty()
        || plan.members.len() > MAX_MEMBERS
        || plan.seeds.len() < 2
        || plan.seeds.len() > 16
        || plan.seeds.iter().collect::<BTreeSet<_>>().len() != plan.seeds.len()
    {
        bail!("invalid preparation plan schema, member count or seeds");
    }
    plan.base_research_plan.validate()?;
    let mut ids = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut members: Vec<CexCampaignResearchPlanV1> = plan
        .members
        .iter()
        .map(|member| {
            validate_dns_label("preparation member", &member.id)?;
            if !ids.insert(&member.id) {
                bail!("duplicate preparation member id");
            }
            let mut selected = member
                .research_plan
                .as_ref()
                .unwrap_or(&plan.base_research_plan)
                .clone();
            if let Some(treatment) = &member.mlp {
                let mlp = selected
                    .mlp_training
                    .as_mut()
                    .context("MLP treatment requires a declared base training plan")?;
                mlp.updates = treatment.updates;
                mlp.optimization
                    .as_mut()
                    .context("MLP treatment requires explicit stability controls")?
                    .learning_rate = treatment.learning_rate;
            }
            selected.validate()?;
            let mut treatment_identity = selected.clone();
            treatment_identity.comparison_family_trials = None;
            if !identities.insert(canonical_json_hash(&treatment_identity)?) {
                bail!("duplicate preparation member configuration");
            }
            if selected.generation != 0 {
                bail!("preparation members must be initial plans");
            }
            if let Some(mlp) = &selected.mlp_training {
                mlp.validate_requested_seeds(&plan.seeds)
                    .map_err(anyhow::Error::msg)?;
            }
            selected.effective_multiple_testing_trials(declared_total_trials_for_rounds(
                &selected,
                plan.seeds.len(),
            )?)?;
            Ok(selected)
        })
        .collect::<anyhow::Result<_>>()?;
    if members.len() > 1 {
        let total = members.iter().try_fold(0usize, |sum, member| {
            sum.checked_add(declared_total_trials_for_rounds(member, plan.seeds.len())?)
                .context("comparison family trial bound overflowed")
        })?;
        let bound = members
            .iter()
            .filter_map(|member| member.comparison_family_trials)
            .max()
            .unwrap_or(0)
            .max(total);
        for member in &mut members {
            member.comparison_family_trials = Some(bound);
        }
    }
    Ok(members)
}

fn freeze_args(plan: &PreparationPlan, receipt: &Path) -> CampaignFreezeArgs {
    CampaignFreezeArgs {
        preparation_ledger: None,
        reuse: None,
        reuse_sha256: None,
        final_evaluation_control: None,
        campaign_inputs: receipt.into(),
        input_root: plan.input_root.clone(),
        source_revision: plan.source_revision.clone(),
        image: plan.image.clone(),
        campaign_root: plan.campaign_root.clone(),
        seeds: plan.seeds.clone(),
        research_plan: None,
        study_proposal: None,
        output: PathBuf::new(),
    }
}

fn restore_inputs(
    plan: &PreparationPlan,
    receipt: CampaignInputsReceipt,
    shared: SharedInputs,
    ledger: &alpha_store::AlphaStore,
) -> anyhow::Result<ValidatedCampaignInputSet> {
    verify_authentication(
        ledger,
        &shared,
        shared.preparation_authentication_tag.as_deref(),
    )?;
    let image_identity = mission_dispatch::image_digest(&plan.image)?;
    if shared.schema_version != INPUT_SCHEMA
        || shared.source_revision != plan.source_revision
        || shared.source_revision != BUILD_SOURCE_REVISION
        || shared.image_identity != image_identity
        || shared.campaign_inputs_sha256 != plan.campaign_inputs.sha256
    {
        bail!("prepared input receipt source, image or dataset identity differs");
    }
    let render_inputs = PreparedCexInputs::restore_metadata(
        shared.render_metadata,
        &receipt.feature.sha256,
        &receipt.materialization.sha256,
    )?;
    Ok(ValidatedCampaignInputSet {
        feature_url: receipt.feature.object_url.clone(),
        feature_sha256: receipt.feature.sha256.clone(),
        materialization_url: receipt.materialization.object_url.clone(),
        materialization_sha256: receipt.materialization.sha256.clone(),
        replay_artifact_url: receipt.replay_artifact.object_url.clone(),
        replay_artifact_sha256: receipt.replay_artifact.sha256.clone(),
        replay_manifest_url: receipt.replay_manifest.object_url.clone(),
        replay_manifest_sha256: receipt.replay_manifest.sha256.clone(),
        producer_image_identity: mission_dispatch::image_digest(&receipt.image_ref)?,
        receipt,
        render_inputs,
        campaign_inputs_sha256: plan.campaign_inputs.sha256.clone(),
        build_source_revision: plan.source_revision.clone(),
        image_identity,
        campaign_root: canonical_tokyo_oss_internal_object("campaign root", &plan.campaign_root)?,
    })
}

pub fn prepare(args: CampaignPrepareArgs) -> anyhow::Result<()> {
    print_json(&prepare_report(
        args,
        None,
        PreparationPurpose::ArtifactsOnly,
    )?)
}

pub(super) enum PreparationPurpose {
    ArtifactsOnly,
    ExecuteWorkflow { comparison_trials: usize },
}

fn validate_workflow_training(plans: &[CexCampaignResearchPlanV1]) -> anyhow::Result<()> {
    for plan in plans {
        if !plan.supervised_model_scope.is_default() {
            plan.validate()?;
            continue;
        }
        let training = plan.mlp_training.as_ref().context("canonical supervised workflow requires an explicit MLP training plan; diagnostic defaults cannot execute")?;
        let optimization = training
            .optimization
            .as_ref()
            .context("workflow requires frozen gradient, loss and convergence controls")?;
        let convergence = &optimization.controls.convergence;
        let tail = convergence
            .comparisons
            .checked_add(1)
            .and_then(|windows| convergence.window_updates.checked_mul(windows))
            .context("workflow convergence window overflowed")?;
        if !optimization.controls.stop_on_convergence
            || training.updates < convergence.minimum_updates
            || training.updates < tail
        {
            bail!("workflow requires convergence stopping and enough update budget for its minimum and tail windows");
        }
    }
    Ok(())
}

/// Plan all input groups before admitting bulk data. The workflow uses the
/// sum for statistical correction while native grants charge each member only.
pub(super) fn workflow_plan_bound(
    reference: &FileRef,
    base: &Path,
) -> anyhow::Result<(usize, usize, Vec<String>)> {
    let value = verified_bytes(reference, base, MAX_REQUEST_BYTES)?;
    let plan: PreparationPlan = serde_json::from_slice(&value)?;
    let members = member_plans(&plan)?;
    validate_workflow_training(&members)?;
    if normalized_source_revision("preparation source", &plan.source_revision)?
        != BUILD_SOURCE_REVISION
    {
        bail!("workflow preparation source differs from the executable");
    }
    mission_dispatch::image_digest(&plan.image)?;
    canonical_tokyo_oss_internal_object("campaign root", &plan.campaign_root)?;
    let mut sum = 0usize;
    let mut bound = 0usize;
    for member in members {
        sum = sum
            .checked_add(declared_total_trials_for_rounds(&member, plan.seeds.len())?)
            .context("workflow trial sum overflow")?;
        bound = bound.max(member.comparison_family_trials.unwrap_or(0));
    }
    Ok((
        sum,
        bound,
        plan.members.into_iter().map(|member| member.id).collect(),
    ))
}

pub(super) fn prepare_report(
    args: CampaignPrepareArgs,
    expected_sha: Option<&str>,
    purpose: PreparationPurpose,
) -> anyhow::Result<serde_json::Value> {
    // Bulk data preparation is colocated with ACK inputs. Software tests use
    // local synthetic fixtures and do not establish cloud runtime evidence.
    #[cfg(not(test))]
    if std::env::consts::OS != "linux"
        || std::env::var("MONDAY_EXECUTION_HOST").as_deref() != Ok("ack")
    {
        bail!("Campaign matrix preparation belongs in ACK; export only bounded control metadata");
    }
    let plan_path = std::fs::canonicalize(&args.plan)?;
    let ledger_path = std::fs::canonicalize(&args.ledger)?;
    let ledger = alpha_store::AlphaStore::open_read_only(&ledger_path)?;
    let base = plan_path.parent().unwrap();
    let plan_bytes = bytes(&plan_path, MAX_REQUEST_BYTES)?;
    if let Some(expected) = expected_sha {
        if hex::encode(Sha256::digest(&plan_bytes))
            != normalized_sha256("workflow preparation plan", expected)?
        {
            bail!("workflow preparation plan SHA256 mismatch");
        }
    }
    let mut plan: PreparationPlan = serde_json::from_slice(&plan_bytes)?;
    let mut plans = member_plans(&plan)?;
    if let PreparationPurpose::ExecuteWorkflow { comparison_trials } = purpose {
        validate_workflow_training(&plans)?;
        for member in &mut plans {
            member.comparison_family_trials = Some(
                member
                    .comparison_family_trials
                    .unwrap_or(0)
                    .max(comparison_trials),
            );
            member.effective_multiple_testing_trials(declared_total_trials_for_rounds(
                member,
                plan.seeds.len(),
            )?)?;
        }
    }
    if normalized_source_revision("preparation source", &plan.source_revision)?
        != BUILD_SOURCE_REVISION
    {
        bail!("preparation source does not match this build");
    }
    mission_dispatch::image_digest(&plan.image)?;
    canonical_tokyo_oss_internal_object("campaign root", &plan.campaign_root)?;
    let receipt_bytes = verified_bytes(&plan.campaign_inputs, base, MAX_REQUEST_BYTES)?;
    let receipt: CampaignInputsReceipt = serde_json::from_slice(&receipt_bytes)?;
    validate_campaign_inputs_receipt(&receipt)?;
    let key = canonical_json_hash(&serde_json::json!({"schema":PLAN_SCHEMA,
        "source":plan.source_revision,"image":plan.image,"campaign_root":plan.campaign_root,
        "campaign_inputs_sha256":plan.campaign_inputs.sha256,"seeds":plan.seeds,
        "members":plan.members.iter().zip(&plans).map(|(m,p)|serde_json::json!({"id":m.id,"plan":p})).collect::<Vec<_>>() }))?;
    data_mission::ensure_real_directory(&args.output_root, "preparation output")?;
    let output = std::fs::canonicalize(&args.output_root)?.join(&key);
    data_mission::ensure_real_directory(&output, "preparation state")?;
    data_mission::ensure_output_path_is_not_symlink(
        &output.join(".prepare.lock"),
        "preparation lock",
    )?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(output.join(".prepare.lock"))?;
    lock.try_lock()
        .context("this preparation already has an active writer")?;
    let index_path = output.join("preparation.json");
    data_mission::ensure_output_path_is_not_symlink(&index_path, "preparation index")?;
    let local_receipt = publish_bytes(&output, Path::new("campaign-inputs.json"), &receipt_bytes)?;
    plan.input_root = base.join(&plan.input_root);
    let mut args_for_freeze = freeze_args(&plan, &output.join(&local_receipt.path));
    args_for_freeze.preparation_ledger = Some(ledger_path.clone());
    if index_path.exists() {
        let index: PreparationIndex =
            serde_json::from_slice(&bytes(&index_path, MAX_METADATA_BYTES)?)?;
        if index.schema_version != INDEX_SCHEMA
            || index.ledger != ledger_path
            || index.plan_sha256 != key
            || index.members.len() != plan.members.len()
        {
            bail!("retained preparation index differs from its plan");
        }
        if index.source_revision != plan.source_revision
            || index.image != plan.image
            || index.campaign_root != plan.campaign_root
            || index.campaign_inputs.sha256 != plan.campaign_inputs.sha256
            || index.campaign_inputs.path != Path::new("campaign-inputs.json")
            || index.seeds
                != plan
                    .seeds
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            || index.prepared_inputs.path
                != Path::new("inputs").join(format!("{}.json", index.prepared_inputs.sha256))
        {
            bail!("retained preparation header differs from the plan");
        }
        let shared: SharedInputs = serde_json::from_slice(&verified_bytes(
            &index.prepared_inputs,
            &output,
            MAX_METADATA_BYTES,
        )?)?;
        restore_inputs(&plan, receipt, shared, &ledger)?;
        for ((expected, research), member) in plan.members.iter().zip(&plans).zip(&index.members) {
            if member.id != expected.id
                || member.freeze.path != Path::new("members").join(&member.id).join("freeze.json")
                || member.research_plan.path
                    != Path::new("members")
                        .join(&member.id)
                        .join("research-plan.json")
            {
                bail!("retained preparation member differs");
            }
            let prepared_plan: CexCampaignResearchPlanV1 = serde_json::from_slice(
                &verified_bytes(&member.research_plan, &output, MAX_REQUEST_BYTES)?,
            )?;
            if prepared_plan != *research {
                bail!("retained research plan differs");
            }
            args_for_freeze.reuse = Some(output.join(&member.freeze.path));
            args_for_freeze.reuse_sha256 = Some(member.freeze.sha256.clone());
            let (request, _) = reuse_frozen_request(&args_for_freeze, research, None)?;
            if member.campaign_id != request.campaign_id
                || member.declared_trials != request.declared_total_trials
            {
                bail!("retained preparation member report differs from its request");
            }
        }
        return Ok(
            serde_json::json!({"status":"ready","preparation":index_path,"sha256":hex::encode(Sha256::digest(bytes(&index_path,MAX_METADATA_BYTES)?)),"reused":true,"bulk_input_reads":0}),
        );
    }
    let ready_path = output.join("input-ready.json");
    data_mission::ensure_output_path_is_not_symlink(&ready_path, "input preparation checkpoint")?;
    let (inputs, shared_ref, reused_inputs) = if ready_path.exists() {
        let reference: FileRef = serde_json::from_slice(&bytes(&ready_path, MAX_REQUEST_BYTES)?)?;
        if reference.path != Path::new("inputs").join(format!("{}.json", reference.sha256)) {
            bail!("partial input checkpoint has an invalid content address");
        }
        let shared: SharedInputs =
            serde_json::from_slice(&verified_bytes(&reference, &output, MAX_METADATA_BYTES)?)?;
        (
            restore_inputs(&plan, receipt, shared, &ledger)?,
            reference,
            true,
        )
    } else {
        let (inputs, data, reused) = if let Some(previous) = &plan.prepared_inputs {
            let data = verified_bytes(previous, base, MAX_METADATA_BYTES)?;
            let shared: SharedInputs = serde_json::from_slice(&data)?;
            (restore_inputs(&plan, receipt, shared, &ledger)?, data, true)
        } else {
            let inputs = validated_campaign_inputs(
                &args_for_freeze,
                plans.iter().any(|plan| plan.calendar.is_some()),
            )?;
            for research in &plans {
                inputs.render_inputs.verify_development_precheck(research)?;
            }
            let mut shared = SharedInputs {
                preparation_authentication_tag: None,
                schema_version: INPUT_SCHEMA.into(),
                source_revision: plan.source_revision.clone(),
                image_identity: inputs.image_identity.clone(),
                campaign_inputs_sha256: inputs.campaign_inputs_sha256.clone(),
                render_metadata: inputs.render_inputs.metadata()?,
            };
            shared.preparation_authentication_tag = Some(authenticate(&ledger, &shared)?);
            let mut data = serde_json::to_vec_pretty(&shared)?;
            data.push(b'\n');
            (inputs, data, false)
        };
        if data.len() as u64 > MAX_METADATA_BYTES {
            bail!("shared input metadata exceeds its byte limit");
        }
        let sha = hex::encode(Sha256::digest(&data));
        let reference = publish_bytes(
            &output,
            &Path::new("inputs").join(format!("{sha}.json")),
            &data,
        )?;
        publish(&output, Path::new("input-ready.json"), &reference)?;
        (inputs, reference, reused)
    };
    let mut members = Vec::new();
    for (member, research) in plan.members.iter().zip(&plans) {
        let (request, campaign_inputs_sha256) =
            freeze_prepared_request(&inputs, research, &plan.seeds, None)?;
        let mut frozen = FrozenCampaignPlan {
            preparation_authentication_tag: None,
            schema_version: CAMPAIGN_FREEZE_SCHEMA_V1.into(),
            campaign_inputs_sha256,
            signing_plan: signing_plan(&request)?,
            canonical_request: request.clone(),
        };
        frozen.preparation_authentication_tag = Some(authenticate(&ledger, &frozen)?);
        let directory = Path::new("members").join(&member.id);
        members.push(PreparedMember {
            id: member.id.clone(),
            research_plan: publish(&output, &directory.join("research-plan.json"), research)?,
            freeze: publish(&output, &directory.join("freeze.json"), &frozen)?,
            campaign_id: request.campaign_id,
            declared_trials: request.declared_total_trials,
        });
    }
    let index = PreparationIndex {
        ledger: ledger_path,
        schema_version: INDEX_SCHEMA.into(),
        plan_sha256: key,
        source_revision: plan.source_revision,
        image: plan.image,
        campaign_root: plan.campaign_root,
        campaign_inputs: local_receipt,
        input_root: plan.input_root,
        prepared_inputs: shared_ref,
        seeds: plan.seeds.iter().map(ToString::to_string).collect(),
        members,
    };
    let reference = publish(&output, Path::new("preparation.json"), &index)?;
    Ok(
        serde_json::json!({"status":"ready","preparation":index_path,"sha256":reference.sha256,
        "reused":false,"input_validation_reused":reused_inputs,"members":index.members.len()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> PreparationPlan {
        PreparationPlan {
            schema_version: PLAN_SCHEMA.into(),
            source_revision: BUILD_SOURCE_REVISION.into(),
            image: format!("registry/research@sha256:{}", "a".repeat(64)),
            campaign_root: "https://bucket.oss-ap-northeast-1-internal.aliyuncs.com/research/test"
                .into(),
            campaign_inputs: FileRef {
                path: "missing.json".into(),
                sha256: "b".repeat(64),
            },
            input_root: "missing".into(),
            prepared_inputs: None,
            seeds: vec![7, u64::MAX],
            base_research_plan: CexCampaignResearchPlanV1::canonical(),
            members: vec![Member {
                id: "first".into(),
                research_plan: None,
                mlp: None,
            }],
        }
    }
    #[test]
    fn ridge_workflow_plans_its_budget_without_requiring_mlp_or_reading_bulk_inputs() {
        let root = tempfile::tempdir().unwrap();
        let mut plan = plan();
        plan.base_research_plan.supervised_model_scope =
            alpha_domain::CexSupervisedModelScopeV1::RidgeOnly;
        plan.base_research_plan.holding =
            Some(hft_research_manifest::model::HorizonHoldingPolicyV1 {
                horizon_millis: 5000,
            });
        plan.base_research_plan.comparison_family_trials = Some(138);
        let bytes = serde_json::to_vec(&plan).unwrap();
        let path = root.path().join("plan.json");
        std::fs::write(&path, &bytes).unwrap();
        let reference = FileRef {
            path: path.clone(),
            sha256: hex::encode(Sha256::digest(&bytes)),
        };
        let (trials, bound, ids) = workflow_plan_bound(&reference, root.path()).unwrap();
        assert_eq!(
            trials,
            declared_total_trials_for_rounds(&plan.base_research_plan, 2).unwrap()
        );
        assert_eq!(bound, 138);
        assert_eq!(ids, vec!["first"]);
        assert!(!root.path().join("missing.json").exists());
        let changed = FileRef {
            path,
            sha256: "a".repeat(64),
        };
        assert!(workflow_plan_bound(&changed, root.path()).is_err());
    }

    #[test]
    fn executable_workflow_rejects_diagnostic_defaults_and_inadequate_training_budgets() {
        let mut research = CexCampaignResearchPlanV1::canonical();
        assert!(validate_workflow_training(&[research.clone()]).is_err());
        let mut training = super::super::tests::paired_mlp_plan_for_tests();
        training.updates = 4096;
        training.optimization = Some(alpha_domain::mlp_training::CexMlpOptimizationV1 {
            learning_rate: 0.0003,
            controls: hft_research_manifest::mlp_training::MlpOptimizationControlsV1::default(),
        });
        research.mlp_training = Some(training);
        validate_workflow_training(&[research.clone()]).unwrap();
        research.mlp_training.as_mut().unwrap().updates = 8;
        assert!(validate_workflow_training(&[research.clone()]).is_err());
        research.mlp_training.as_mut().unwrap().updates = 4096;
        research
            .mlp_training
            .as_mut()
            .unwrap()
            .optimization
            .as_mut()
            .unwrap()
            .controls
            .stop_on_convergence = false;
        assert!(validate_workflow_training(&[research]).is_err());
    }

    #[test]
    fn preparation_authentication_rejects_self_consistent_edits_and_other_ledgers() {
        let ledger = alpha_store::AlphaStore::open_in_memory().unwrap();
        let other = alpha_store::AlphaStore::open_in_memory().unwrap();
        let mut payload =
            serde_json::json!({"holdout_id":"original","preparation_authentication_tag":null});
        let tag = authenticate(&ledger, &payload).unwrap();
        payload["preparation_authentication_tag"] = tag.clone().into();
        verify_authentication(&ledger, &payload, Some(&tag)).unwrap();
        assert!(verify_authentication(&other, &payload, Some(&tag)).is_err());
        payload["holdout_id"] = "caller-edited-cohort".into();
        assert!(verify_authentication(&ledger, &payload, Some(&tag)).is_err());
        assert!(verify_authentication(&ledger, &payload, None).is_err());
    }

    #[test]
    fn metadata_envelope_supports_json_expansion_at_the_materialization_limit() {
        let materialization =
            "\"".repeat(crate::mission_runner::MAX_MATERIALIZATION_BYTES as usize);
        let encoded =
            serde_json::to_vec(&serde_json::json!({"materialization_json":materialization}))
                .unwrap();
        assert!(encoded.len() as u64 > crate::mission_runner::MAX_MATERIALIZATION_BYTES);
        assert!(encoded.len() as u64 <= MAX_METADATA_BYTES);
    }

    #[test]
    fn matrix_validation_rejects_duplicates_paths_and_unsupported_recipes() {
        let mut value = plan();
        member_plans(&value).unwrap();
        value.members.push(Member {
            id: "second".into(),
            research_plan: None,
            mlp: None,
        });
        assert!(member_plans(&value)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
        value.members.pop();
        value.members[0].id = "../escape".into();
        assert!(member_plans(&value).is_err());
        value.members[0].id = "first".into();
        value.members[0].mlp = Some(MlpTreatment {
            updates: 8192,
            learning_rate: 0.0003,
        });
        assert!(member_plans(&value).is_err());
        value.members[0].mlp = None;
        value.seeds = vec![7, 7];
        assert!(member_plans(&value).is_err());
    }
    #[test]
    fn preparation_artifacts_reject_symlinks_and_conflicting_bytes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let reference = publish_bytes(root.path(), Path::new("first.json"), b"{} ").unwrap();
        assert_eq!(
            verified_bytes(&reference, root.path(), 100).unwrap(),
            b"{} "
        );
        assert!(publish_bytes(root.path(), Path::new("first.json"), b"{}").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("members")).unwrap();
            assert!(publish_bytes(root.path(), Path::new("members/file.json"), b"{}").is_err());
            assert!(!outside.path().join("file.json").exists());
        }
        let lock_path = root.path().join("lock");
        let first = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .unwrap();
        first.try_lock().unwrap();
        let second = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        assert!(second.try_lock().is_err());
    }
}
