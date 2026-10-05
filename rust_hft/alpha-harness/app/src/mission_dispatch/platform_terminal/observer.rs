use super::{platform_facts, scientific_results, stopped_execution, PlatformTerminalArgs};
use crate::mission_dispatch::{
    admission, load_submission, platform_admission, render_controlled_manifest, validate_submission,
};
use alpha_domain::campaign_control::SignedCampaignRootGrantV1;
use alpha_store::{
    campaign_ledger::{
        CampaignPlatformScientificStatusV1, CampaignPlatformTerminalAuditV1,
        CampaignPlatformTerminalStateV1, VerifiedCampaignPlatformTerminalSource,
    },
    AlphaStore,
};
use anyhow::{ensure, Context};
use hft_research_platform::{
    admission::NativeAdmissionTrust,
    build::BuildArtifact,
    orchestrator::State,
    release::{BuildReleaseTrust, SignedBuildRelease},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    schema: String,
    observer_binary: PathBuf,
    observer_build: PathBuf,
    observer_release: PathBuf,
    observer_trust: PathBuf,
    native_trust: PathBuf,
    /// URLs must name each exact PG receipt artifact. Bytes remain independently
    /// bound to its task/attempt/fence, digest and size, including ZIP contents.
    artifact_readback: BTreeMap<String, String>,
    #[serde(default)]
    artifact_tls: platform_admission::HostTls,
}

/// Neither caller JSON nor a deserializer can construct this value. Its only
/// producer joins the original source operation and independent observations.
struct VerifiedPlatformTerminalEvidence {
    audit: CampaignPlatformTerminalAuditV1,
    retained: PathBuf,
}

pub(super) fn audit(args: PlatformTerminalArgs) -> anyhow::Result<()> {
    crate::cli::require_cloud_data_host(std::env::consts::OS)?;
    ensure!(
        std::env::var("MONDAY_EXECUTION_HOST").as_deref() == Ok("ack"),
        "terminal audit requires the controlled ACK source observer"
    );
    hft_research_dispatch_io::validate_cluster_target(&args.context, &args.namespace)?;
    ensure!(
        !crate::mission_dispatch::sequence_admission::is_study_submission(&args.submission)?
            && !crate::mission_dispatch::final_admission::is_final_submission(&args.submission)?,
        "terminal audit accepts canonical pre-holdout Campaign operations"
    );
    let validated = validate_submission(load_submission(&args.submission)?)?;
    let control = admission::read_control(&args.control)?;
    let manifest = render_controlled_manifest(&validated, &args.namespace, &control)?;
    let inspection = admission::reconstruct_binding(
        &validated,
        &manifest,
        &control.materialization_path,
        &control.controller_image,
        control.attempt_ordinal,
    )?;
    let signed: SignedCampaignRootGrantV1 =
        platform_admission::read_metadata(&control.signed_root_grant_path)?;
    let expected = inspection
        .historical_reservation_for(&signed.content_sha256, &signed.grant.family.family_id);
    let mut store = AlphaStore::open(&control.ledger_path)?;
    let source =
        store.campaign_platform_terminal_source(&expected.family_id, &expected.operation_id()?)?;
    ensure!(
        source.reservation() == &expected && source.root().signed_grant() == &signed,
        "terminal observer changed original finalized request or historical Root"
    );
    let path = args.observation.canonicalize()?;
    let mut observation: Observation =
        serde_json::from_slice(&platform_admission::file_bytes(&path, 1024 * 1024, true)?)?;
    ensure!(
        observation.schema == "monday.native_campaign_terminal_observer.v1"
            && observation.artifact_readback.len() <= 258,
        "invalid bounded terminal observer configuration"
    );
    let base = path
        .parent()
        .context("observer configuration parent absent")?;
    for path in [
        &mut observation.observer_binary,
        &mut observation.observer_build,
        &mut observation.observer_release,
        &mut observation.observer_trust,
        &mut observation.native_trust,
    ] {
        if !path.is_absolute() {
            *path = base.join(&*path);
        }
    }
    let build: BuildArtifact = platform_admission::read_metadata(&observation.observer_build)?;
    let release: SignedBuildRelease =
        platform_admission::read_metadata(&observation.observer_release)?;
    let release_trust: BuildReleaseTrust =
        platform_admission::read_metadata(&observation.observer_trust)?;
    let verified_release = release_trust.verify(&build, &release)?;
    let native_trust: NativeAdmissionTrust =
        platform_admission::read_metadata(&observation.native_trust)?;
    let output = args.output.canonicalize()?;
    ensure_private_directory(&output)?;
    let evidence = if let Some(audit) = store.campaign_platform_terminal_audit(&source)? {
        restore(&source, audit, &output, &native_trust, &verified_release)?
    } else {
        let facts = platform_facts::read(
            &source,
            &observation.observer_binary,
            &verified_release,
            &native_trust,
        )?;
        let stopped = stopped_execution::read(
            &args.context,
            &facts.snapshot.task.spec,
            &facts.lease,
            &facts.handle,
        )?;
        stopped_execution::verify_controlled_identity(
            &stopped,
            facts
                .snapshot
                .task
                .attempt_identity
                .as_ref()
                .context("controlled identity missing")?,
        )?;
        construct(
            &source,
            &validated.submission.request,
            facts,
            stopped,
            &observation,
            &output,
        )?
    };
    let receipt = store.record_campaign_platform_terminal_audit(&source, &evidence.audit)?;
    // A durable append precedes publication. Retries recover this exact audit
    // and its complete retained content instead of replacing observation time.
    let origin = reqwest::Url::parse(
        &hft_research_dispatch_io::canonical_tokyo_oss_internal_object(
            "Campaign result",
            &validated.submission.request.campaign_result_readback_url,
        )?,
    )?
    .origin()
    .ascii_serialization();
    let client = platform_admission::host_client(&platform_admission::HostTls::default())?;
    admission::publish_family_receipts_with(
        &mut store,
        &source.reservation().family_id,
        &origin,
        &control.receipt_access,
        |access, bytes| admission::publish_and_readback(&client, access, bytes),
    )?;
    if let Some(study) = store.campaign_study_id_for_family(&source.reservation().family_id)? {
        admission::publish_study_receipts_with(
            &mut store,
            &study,
            &origin,
            &control.receipt_access,
            |access, bytes| admission::publish_and_readback(&client, access, bytes),
        )?;
    }
    crate::cli::print_json(
        &serde_json::json!({"schema":"monday.native_campaign_terminal_audit_result.v1", "operation_sha256":source.operation_sha256()?, "task_id":evidence.audit.task_id, "attempt":evidence.audit.attempt, "fence":evidence.audit.fence, "terminal_revision":evidence.audit.terminal_revision, "job_uid":evidence.audit.job_uid, "pod_uid":evidence.audit.pod_uid, "audit_receipt_sha256":receipt.object_sha256()?, "retained_observation":evidence.retained, "charging_trials":evidence.audit.charging_trials, "known_scientific_consumption":evidence.audit.known_scientific_consumption, "cleanup_authority":"not_issued", "budget_released":false}),
    )
}

fn construct(
    source: &VerifiedCampaignPlatformTerminalSource,
    request: &crate::mission_campaign::CampaignRequest,
    facts: platform_facts::VerifiedPlatformFacts,
    stopped: stopped_execution::VerifiedStoppedExecution,
    observation: &Observation,
    output: &Path,
) -> anyhow::Result<VerifiedPlatformTerminalEvidence> {
    let temporary = tempfile::tempdir_in(output)?;
    let root = temporary.path().canonicalize()?;
    let client = platform_admission::host_client(&observation.artifact_tls)?;
    let input = tempfile::tempdir_in(output)?;
    let source_client = platform_admission::host_client(&platform_admission::HostTls::default())?;
    let data = crate::mission_campaign::prepared_inputs::acquire_native_prepared(
        request,
        &source.reservation().request_sha256,
        &source_client,
        input.path(),
    )?;
    let execution = &source.reservation().execution;
    ensure!(
        data.campaign_inputs_sha256() == execution.campaign_inputs_sha256
            && data.evaluation_protocol_sha256() == execution.evaluation_protocol_sha256
            && data.source_revision() == execution.source_revision
            && data.runner_image_identity()
                == crate::mission_dispatch::image_digest(&execution.runner_image)?
            && data.declared_trials() as u64 == source.reservation().declared_trials
            && data.collection_id() == facts.snapshot.task.spec.view_manifest_sha256
            && data.collection_id() == facts.snapshot.run.data_manifest_sha256,
        "native decoded inputs changed original reserved source or fixed Run"
    );
    let state = match facts.snapshot.task.state {
        State::Succeeded => CampaignPlatformTerminalStateV1::Succeeded,
        State::Failed => CampaignPlatformTerminalStateV1::Failed,
        State::Cancelled => CampaignPlatformTerminalStateV1::Cancelled,
        State::TimedOut => CampaignPlatformTerminalStateV1::TimedOut,
        _ => anyhow::bail!("nonterminal compute cannot produce a source audit"),
    };
    let (science, consumed, result) = if state == CampaignPlatformTerminalStateV1::Succeeded {
        ensure!(
            stopped.worker_exit_code == 0,
            "successful compute has a failed original process"
        );
        scientific_results::read(
            &facts,
            data.finalized_request(),
            &source.reservation().request_sha256,
            &client,
            &source_client,
            &observation.artifact_readback,
            &root,
        )?
    } else {
        (CampaignPlatformScientificStatusV1::Unknown, None, None)
    };
    for (name, bytes) in [
        ("platform-snapshot.json", facts.snapshot_bytes.clone()),
        ("job.json", serde_json::to_vec_pretty(&stopped.job)?),
        ("pod.json", serde_json::to_vec_pretty(&stopped.pod)?),
        (
            "prepared-inputs.json",
            serde_json::to_vec_pretty(data.prepared().manifest())?,
        ),
        (
            "source-transfer.json",
            source.transfer_receipt().publication_bytes()?,
        ),
    ] {
        platform_admission::retain(&root.join(name), &bytes)?;
    }
    let retained_manifest_sha256 = super::retained_files::retain(&root)?;
    let audit = CampaignPlatformTerminalAuditV1 {
        schema_version: "monday.campaign_platform_terminal_audit.v1".into(),
        transfer: source.transfer().clone(),
        platform_state: state,
        scientific_status: science,
        charging_trials: source.reservation().declared_trials,
        known_scientific_consumption: consumed,
        retained_manifest_sha256,
        platform_snapshot_sha256: facts.snapshot_sha256,
        observer_release_sha256: facts.observer_release_sha256,
        native_admission_sha256: hft_research_platform::identity(&facts.snapshot.native_admission)?,
        native_trust_sha256: hft_research_platform::identity(&facts.snapshot.native_trust)?,
        collection_sha256: facts.snapshot.task.spec.view_manifest_sha256.clone(),
        task_id: facts.snapshot.task.id.clone(),
        attempt: facts.lease.attempt,
        fence: facts.lease.fence,
        terminal_revision: facts.snapshot.terminal_revision,
        terminal_event_sha256: hft_research_platform::identity(&facts.snapshot.terminal_event)?,
        execution_event_sha256: hft_research_platform::identity(&facts.snapshot.execution_event)?,
        job_uid: stopped.handle.uid,
        pod_uid: stopped.pod_uid,
        job_sha256: stopped.job_sha256,
        pod_sha256: stopped.pod_sha256,
        native_result_sha256: result,
        observed_at: stopped.observed_at,
    };
    for (name, bytes) in [
        ("platform-snapshot.json", facts.snapshot_bytes),
        ("job.json", serde_json::to_vec_pretty(&stopped.job)?),
        ("pod.json", serde_json::to_vec_pretty(&stopped.pod)?),
        ("terminal-audit.json", serde_json::to_vec_pretty(&audit)?),
    ] {
        platform_admission::retain(&root.join(name), &bytes)?;
    }
    let retained = output.join(alpha_domain::canonical_json_hash(&audit)?);
    ensure!(
        !retained.exists(),
        "uncommitted terminal observation already occupies immutable output"
    );
    std::fs::rename(temporary.keep(), &retained)?;
    std::fs::File::open(output)?.sync_all()?;
    Ok(VerifiedPlatformTerminalEvidence { audit, retained })
}

fn restore(
    source: &VerifiedCampaignPlatformTerminalSource,
    audit: CampaignPlatformTerminalAuditV1,
    output: &Path,
    trust: &NativeAdmissionTrust,
    release: &hft_research_platform::release::VerifiedBuildRelease,
) -> anyhow::Result<VerifiedPlatformTerminalEvidence> {
    let retained = output.join(alpha_domain::canonical_json_hash(&audit)?);
    ensure_private_directory(&retained)?;
    super::retained_files::verify(&retained, &audit.retained_manifest_sha256)?;
    let bytes =
        platform_admission::file_bytes(&retained.join("terminal-audit.json"), 1024 * 1024, true)?;
    ensure!(
        serde_json::from_slice::<CampaignPlatformTerminalAuditV1>(&bytes)? == audit
            && audit.transfer == *source.transfer()
            && audit.observer_release_sha256 == release.artifact().id()?,
        "retained audit changed the authenticated source event or observer release"
    );
    let snapshot = platform_admission::file_bytes(
        &retained.join("platform-snapshot.json"),
        1024 * 1024,
        true,
    )?;
    ensure!(
        hft_research_platform::sha256(&snapshot) == audit.platform_snapshot_sha256,
        "retained readonly facts changed"
    );
    let snapshot: hft_research_platform::research::NativeTerminalSnapshot =
        serde_json::from_slice(&snapshot)?;
    trust.verify(&snapshot.native_admission)?;
    ensure!(
        hft_research_platform::identity(&snapshot.native_admission)?
            == audit.native_admission_sha256
            && hft_research_platform::identity(trust)? == audit.native_trust_sha256
            && snapshot.task.id == audit.task_id
            && snapshot.terminal_revision == audit.terminal_revision,
        "retained audit changed native identity or terminal revision"
    );
    for (name, digest) in [
        ("job.json", &audit.job_sha256),
        ("pod.json", &audit.pod_sha256),
    ] {
        let value: serde_json::Value = serde_json::from_slice(&platform_admission::file_bytes(
            &retained.join(name),
            1024 * 1024,
            true,
        )?)?;
        ensure!(
            alpha_domain::canonical_json_hash(&value)? == *digest,
            "retained provider bytes changed"
        );
    }
    // The original audit already owns this mechanical record. This retry does
    // not execute science, nominate new stopped resources or create GC authority.
    Ok(VerifiedPlatformTerminalEvidence { audit, retained })
}

fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        path.is_absolute()
            && path.canonicalize()? == path
            && path.is_dir()
            && path.metadata()?.permissions().mode() & 0o077 == 0,
        "terminal output requires canonical private storage"
    );
    Ok(())
}
