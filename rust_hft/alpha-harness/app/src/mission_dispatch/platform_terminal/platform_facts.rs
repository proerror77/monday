//! Source bindings authenticate the actual readonly observer output. The raw
//! snapshot is not a proof; this type has no deserializer or public constructor.
use super::snapshot_transport::{self, ReadonlySnapshotBytes};
use alpha_store::campaign_ledger::VerifiedCampaignPlatformTerminalSource;
use anyhow::{ensure, Context};
use hft_research_platform::{
    admission::NativeAdmissionTrust,
    execution::ExecutionHandle,
    orchestrator::{AttemptContext, Lease, TaskKind},
    release::VerifiedBuildRelease,
    research::NativeTerminalSnapshot,
};
use std::path::Path;

pub(super) struct VerifiedPlatformFacts {
    pub(super) snapshot: NativeTerminalSnapshot,
    pub(super) snapshot_sha256: String,
    pub(super) observer_release_sha256: String,
    pub(super) lease: Lease,
    pub(super) handle: ExecutionHandle,
}

pub(super) fn read(
    source: &VerifiedCampaignPlatformTerminalSource,
    binary: &Path,
    release: &VerifiedBuildRelease,
    native_trust: &NativeAdmissionTrust,
) -> anyhow::Result<VerifiedPlatformFacts> {
    let bytes = snapshot_transport::read(
        binary,
        release,
        &source.transfer().tenant,
        &source.transfer().request_sha256,
    )?;
    bind(source, bytes, native_trust)
}

fn bind(
    source: &VerifiedCampaignPlatformTerminalSource,
    bytes: ReadonlySnapshotBytes,
    native_trust: &NativeAdmissionTrust,
) -> anyhow::Result<VerifiedPlatformFacts> {
    let snapshot: NativeTerminalSnapshot = serde_json::from_slice(bytes.bytes())
        .context("released observer returned invalid readonly facts")?;
    let observed = verify_snapshot(source, &snapshot, native_trust)?;
    Ok(VerifiedPlatformFacts {
        snapshot,
        snapshot_sha256: hft_research_platform::sha256(bytes.bytes()),
        observer_release_sha256: bytes.observer_release_sha256().into(),
        lease: observed.0,
        handle: observed.1,
    })
}

fn verify_snapshot(
    source: &VerifiedCampaignPlatformTerminalSource,
    snapshot: &NativeTerminalSnapshot,
    native_trust: &NativeAdmissionTrust,
) -> anyhow::Result<(Lease, ExecutionHandle)> {
    // Trust comes from the host's independently approved public trust file. The
    // database cannot nominate a new issuer by returning another trust document.
    ensure!(
        hft_research_platform::identity(native_trust)?
            == hft_research_platform::identity(&snapshot.native_trust)?,
        "readonly snapshot changed independently expected native witness trust"
    );
    let native = native_trust.verify(&snapshot.native_admission)?;
    let evidence = native.evidence();
    let transfer = source.transfer();
    let reservation = source.reservation();
    let execution = &reservation.execution;
    let task = &snapshot.task;
    let spec = &task.spec;
    snapshot.run.admit(spec)?;
    ensure!(
        snapshot.schema == "monday.native_platform_terminal_snapshot.v1"
            && snapshot.tenant == transfer.tenant
            && evidence.tenant == transfer.tenant
            && task.id == transfer.request_sha256
            && spec.id()? == transfer.request_sha256
            && snapshot.run.id()? == transfer.run_sha256
            && evidence.run == snapshot.run
            && evidence.admission.task_spec == *spec
            && evidence.operation_sha256 == source.operation_sha256()?
            && evidence.native_request_sha256 == reservation.request_sha256
            && evidence.family_id == reservation.family_id
            && evidence.root_grant_sha256 == source.root().content_sha256()
            && evidence.transfer_receipt_sha256 == source.transfer_receipt().object_sha256()?
            && evidence.admission.resource_reservation_receipt_sha256
                == source.reservation_receipt().object_sha256()?
            && evidence.admission.scientific_grant_receipt_sha256
                == source.root_receipt().object_sha256()?
            && evidence.declared_trials == reservation.declared_trials
            && evidence.reserved_job_seconds == reservation.reserved_job_seconds
            && evidence.reserved_llm_tokens == reservation.reserved_llm_tokens
            && evidence.issued_ms
                == source
                    .transfer_receipt()
                    .receipt
                    .recorded_at
                    .timestamp_millis()
            && evidence.expires_ms <= source.root().grant().expires_at.timestamp_millis()
            && spec.kind == TaskKind::CexCampaign
            && spec.max_attempts == 1
            && spec.profile.cpu_millis == execution.job_cpu_millis
            && spec.profile.memory_mib == execution.job_memory_mib
            && spec.profile.gpu == 0
            && spec.image == execution.runner_image
            && snapshot.run.code_commit == execution.source_revision
            && snapshot.run.configuration_sha256 == reservation.request_sha256
            && snapshot.run.evaluation_protocol_sha256 == execution.evaluation_protocol_sha256,
        "readonly terminal snapshot changed original native transfer, fixed Run or full debit"
    );
    ensure!(
        task.state.terminal()
            && task.attempt == 1
            && task.execution.is_none()
            && task.lease.is_none()
            && !task.retry_after_stop
            && snapshot.terminal_revision > 0
            && snapshot.terminal_event.revision == snapshot.terminal_revision
            && snapshot.terminal_event.event == "stop_reconciled"
            && snapshot.terminal_event.document == *task,
        "readonly facts lack the exact reconciled terminal event"
    );
    let event = snapshot
        .execution_event
        .as_ref()
        .context("terminal snapshot lost original execution event")?;
    let executed = &event.document;
    let lease = executed
        .lease
        .as_ref()
        .context("recorded execution event lacks original lease")?;
    let handle = executed
        .execution
        .as_ref()
        .context("recorded execution event lacks original Job UID")?;
    ensure!(
        event.revision > 0
            && event.revision < snapshot.terminal_revision
            && executed.id == task.id
            && executed.spec == *spec
            && executed.attempt == task.attempt
            && executed.fence == task.fence,
        "historical execution event changed original request or attempt fence"
    );
    AttemptContext {
        spec: spec.clone(),
        lease: lease.clone(),
    }
    .validate()?;
    handle.validate(lease, spec)?;
    ensure!(
        snapshot.result == task.receipt,
        "readonly snapshot changed immutable terminal result"
    );
    if let Some(result) = &snapshot.result {
        result.validate(spec, lease)?;
    }
    // Historical expiration/revocation is intentionally not an execution gate.
    // This reader grants no execution and cannot release the original charge.
    Ok((lease.clone(), handle.clone()))
}
