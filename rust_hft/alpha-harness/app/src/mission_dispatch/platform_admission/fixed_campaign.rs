//! Source-derived fixed scientific inputs. Every input is an opaque observation
//! or the actual finalized request inspected by the canonical dispatcher.
use super::{
    released_build::ReadbackBuildRelease, worker_configuration::VerifiedWorkerConfiguration,
};
use crate::mission_campaign::prepared_inputs::VerifiedNativeCampaignPreparedInputs;
use alpha_store::campaign_ledger::VerifiedCampaignPlatformBudget;
use anyhow::{ensure, Context};
use hft_research_platform::{
    execution::{Backend, Profile},
    orchestrator::{TaskKind, TaskSpec},
    research::{Experiment, Run},
};

pub(super) struct FixedCampaign {
    pub(super) experiment: Experiment,
    pub(super) run: Run,
    pub(super) spec: TaskSpec,
}

pub(super) struct Inputs<'a> {
    pub budget: &'a VerifiedCampaignPlatformBudget,
    pub data: &'a VerifiedNativeCampaignPreparedInputs,
    pub build: &'a ReadbackBuildRelease,
    pub configuration: &'a VerifiedWorkerConfiguration,
    pub profile: Profile,
    pub validated: &'a super::super::ValidatedSubmission,
    pub manifest: &'a serde_json::Value,
    pub context: &'a str,
    pub namespace: &'a str,
}

pub(super) fn construct(inputs: Inputs<'_>) -> anyhow::Result<FixedCampaign> {
    let Inputs {
        budget,
        data,
        build,
        configuration,
        profile,
        validated,
        manifest,
        context,
        namespace,
    } = inputs;
    let reservation = budget.reservation();
    let execution = &reservation.execution;
    let artifact = build.release().artifact();
    let request = &validated.submission.request;
    ensure!(
        data.request_sha256() == reservation.request_sha256
            && data.campaign_inputs_sha256() == execution.campaign_inputs_sha256
            && data.evaluation_protocol_sha256() == execution.evaluation_protocol_sha256
            && data.source_revision() == execution.source_revision
            && u64::try_from(data.declared_trials())? == reservation.declared_trials,
        "verified development inputs differ from original charged Campaign"
    );
    ensure!(
        artifact.build.code_commit == data.source_revision()
            && artifact.image == execution.runner_image
            && super::super::image_digest(&artifact.image)? == data.runner_image_identity(),
        "Campaign source or released runner differs from native execution authority"
    );
    let architecture = match artifact.build.target.as_str() {
        "x86_64-unknown-linux-gnu" => "amd64",
        "aarch64-unknown-linux-gnu" => "arm64",
        _ => anyhow::bail!("unsupported released Campaign ABI"),
    };
    ensure!(
        matches!(profile.backend, Backend::KubernetesJob | Backend::AcsJob)
            && profile.cluster == context
            && profile.namespace == namespace
            && profile.cpu_millis == execution.job_cpu_millis
            && profile.memory_mib == execution.job_memory_mib
            && profile.architecture == architecture
            && profile.gpu == 0
            && profile.scratch_mib == 20 * 1024
            && profile.prepared_pvc.is_none()
            && profile.worker_secret.as_deref() == Some(configuration.name()),
        "platform Job target/resources/configuration differ from inspected native execution"
    );
    profile.validate()?;
    let worker = artifact
        .executables
        .iter()
        .find(|v| v.name == "monday-cex-worker")
        .context("released scientific Campaign worker is absent")?;
    let container = &manifest["items"][1]["spec"]["template"]["spec"]["containers"][0];
    let mut command = container["command"]
        .as_array()
        .context("inspected Campaign executable is absent")?
        .iter()
        .chain(
            container["args"]
                .as_array()
                .context("inspected Campaign arguments are absent")?,
        )
        .map(|value| {
            value
                .as_str()
                .map(String::from)
                .context("inspected Campaign argv is not text")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        command.first().map(String::as_str) == Some("/usr/local/bin/monday-cex-worker"),
        "native Job does not use the released scientific worker"
    );
    let request_paths = command
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0] == "--request")
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    ensure!(
        request_paths.len() == 1 && command[request_paths[0]] == "/inputs/campaign.json",
        "native Job request path differs from its inspected immutable input"
    );
    command[request_paths[0]] = "/config/campaign.json".into();
    artifact.admits_command(&command)?;
    let experiment = Experiment {
        schema: 1,
        hypothesis: format!(
            "Canonical pre-holdout Campaign family {}",
            reservation.family_id
        ),
        parent_experiment_sha256: None,
        variant: [
            ("native_family_id".into(), reservation.family_id.clone()),
            (
                "native_root_grant_sha256".into(),
                budget.root().content_sha256().into(),
            ),
        ]
        .into(),
    };
    let run = Run {
        schema: 1,
        experiment_sha256: experiment.id()?,
        kind: TaskKind::CexCampaign,
        build_artifact_sha256: artifact.id()?,
        configuration_sha256: reservation.request_sha256.clone(),
        command: command.clone(),
        code_commit: artifact.build.code_commit.clone(),
        source_manifest_sha256: artifact.build.source_manifest_sha256.clone(),
        image: artifact.image.clone(),
        data_manifest_sha256: data.collection_id().into(),
        seed: request
            .rounds
            .first()
            .context("finalized Campaign has no bounded rounds")?
            .seed,
        evaluator_sha256: worker.blob.sha256.clone(),
        evaluation_protocol_sha256: execution.evaluation_protocol_sha256.clone(),
        fit_identity_sha256: None,
    };
    let spec = TaskSpec {
        schema: 1,
        kind: TaskKind::CexCampaign,
        run_manifest_sha256: run.id()?,
        view_manifest_sha256: data.collection_id().into(),
        source_sha256: run.source_manifest_sha256.clone(),
        image: run.image.clone(),
        command,
        profile,
        timeout_ms: i64::try_from(
            reservation
                .reserved_job_seconds
                .checked_mul(1000)
                .context("native Job duration overflow")?,
        )?,
        max_attempts: 1,
        output_prefix: format!("research/native-campaigns/{}", budget.operation_sha256()?),
        fit_identity_sha256: None,
        worker_configuration: Some(configuration.reference().clone()),
    };
    spec.validate()?;
    run.admit_build(artifact)?;
    run.admit(&spec)?;
    Ok(FixedCampaign {
        experiment,
        run,
        spec,
    })
}
