//! Synthetic PG facts bind a genuine finalized-input/source-ledger fixture.
//! This tests readonly binding, not a live platform snapshot or scientific Run.
use alpha_store::campaign_ledger::VerifiedCampaignPlatformTerminalSource;
use hft_research_platform::{
    admission::{NativeAdmission, NativeAdmissionTrust},
    execution::{resource_name, AttemptIdentityRef, ExecutionHandle},
    orchestrator::{Admission, Lease, State, Task, TaskSpec},
    research::{NativeTerminalSnapshot, Run, TerminalLedgerEvent},
};

#[test]
fn retained_terminal_signature_rechecks_scientific_and_release_roles_without_a_key() {
    use hft_research_platform::terminal_audit::{
        sign_terminal_audit, NativeTerminalAuditWitness, SignedNativeTerminalAuditWitness,
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let public = key.verifying_key().to_bytes();
    let trust = NativeAdmissionTrust {
        schema: "monday.native_reservation_trust.v1".into(),
        native_reservation_keys: [("host-witness".into(), hex::encode(public))].into(),
    };
    let evidence = NativeTerminalAuditWitness {
        schema: "monday.native_terminal_audit_witness.v1".into(),
        tenant: "terminal-role-fixture".into(),
        operation_sha256: "a".repeat(64),
        request_sha256: "b".repeat(64),
        run_sha256: "c".repeat(64),
        native_evidence_sha256: "d".repeat(64),
        audit_receipt_sha256: "e".repeat(64),
        retained_manifest_sha256: "f".repeat(64),
        task_id: "b".repeat(64),
        attempt: 1,
        fence: 1,
        job_uid: "job-fixture".into(),
        pod_uid: "pod-fixture".into(),
        terminal_revision: 3,
        issued_ms: 1000,
    };
    let signed = sign_terminal_audit(evidence.clone(), "host-witness".into(), &key).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), serde_json::to_vec_pretty(&signed).unwrap()).unwrap();
    let restored: SignedNativeTerminalAuditWitness =
        crate::mission_dispatch::platform_admission::read_metadata(
            &file.path().canonicalize().unwrap(),
        )
        .unwrap();
    trust.verify_terminal_audit(&restored).unwrap();
    let verify = |authority: &[[u8; 32]], release: &[[u8; 32]]| {
        super::observer::verify_retained_witness(
            &restored,
            &evidence,
            "host-witness",
            &trust,
            authority,
            release,
        )
    };
    assert!(verify(&[public], &[]).is_err());
    assert!(verify(&[], &[public]).is_err());
    verify(&[], &[]).unwrap();
    assert_eq!(restored.evidence.issued_ms, 1000);
    assert_eq!(
        std::fs::read(file.path()).unwrap(),
        serde_json::to_vec_pretty(&signed).unwrap()
    );
}

pub(in crate::mission_dispatch) fn assert_original_snapshot_bindings(
    source: &VerifiedCampaignPlatformTerminalSource,
    run: &Run,
    spec: &TaskSpec,
    trust: &NativeAdmissionTrust,
    key: &ed25519_dalek::SigningKey,
) {
    let reservation = source.reservation();
    let issued = source
        .transfer_receipt()
        .receipt
        .recorded_at
        .timestamp_millis();
    let admission = NativeAdmission {
        schema: "monday.native_scientific_admission.v1".into(),
        tenant: source.transfer().tenant.clone(),
        run: run.clone(),
        admission: Admission {
            schema: 1,
            request_sha256: spec.id().unwrap(),
            task_spec: spec.clone(),
            resource_reservation_receipt_sha256: source
                .reservation_receipt()
                .object_sha256()
                .unwrap(),
            scientific_grant_receipt_sha256: source.root_receipt().object_sha256().unwrap(),
            release_admission_receipt_sha256: "a".repeat(64),
            max_attempts: 1,
        },
        operation_sha256: source.operation_sha256().unwrap(),
        native_request_sha256: reservation.request_sha256.clone(),
        family_id: reservation.family_id.clone(),
        root_grant_sha256: source.root().content_sha256().into(),
        approval_sha256: "b".repeat(64),
        transfer_receipt_sha256: source.transfer_receipt().object_sha256().unwrap(),
        declared_trials: reservation.declared_trials,
        reserved_job_seconds: reservation.reserved_job_seconds,
        reserved_llm_tokens: reservation.reserved_llm_tokens,
        issued_ms: issued,
        expires_ms: source.root().grant().expires_at.timestamp_millis(),
    };
    let signed =
        hft_research_platform::admission::sign(admission, "host-witness".into(), key).unwrap();
    let mut task = Task::new(spec.clone()).unwrap();
    let initial = Lease {
        task_id: task.id.clone(),
        attempt: 1,
        fence: 7,
        owner: "fixed-observer-fixture".into(),
        expires_ms: issued + 30_000,
    };
    let handle = ExecutionHandle {
        backend: spec.profile.backend,
        cluster: spec.profile.cluster.clone(),
        namespace: spec.profile.namespace.clone(),
        name: resource_name(&initial),
        uid: "actual-fixture-job-uid".into(),
        attempt: 1,
        fence: 7,
        task_id: task.id.clone(),
        request_sha256: task.id.clone(),
    };
    task.state = State::Running;
    task.attempt = 1;
    task.fence = 7;
    task.lease = Some(initial.clone());
    task.execution = Some(handle.clone());
    task.deadline_ms = Some(issued + spec.timeout_ms);
    task.attempt_identity = Some(AttemptIdentityRef {
        secret_name: format!("{}-identity", resource_name(&initial)),
        secret_uid: "original-late-secret-uid".into(),
        scope_sha256: "c".repeat(64),
        native_evidence_sha256: signed.evidence_sha256.clone(),
        data_sha256: "d".repeat(64),
        attempt: 1,
        fence: 7,
        deadline_ms: issued + spec.timeout_ms,
        launch_lease: initial.clone(),
    });
    // The platform renews its lease. The provider still owns the original bytes.
    task.lease.as_mut().unwrap().expires_ms += 10_000;
    let execution = task.clone();
    task.state = State::Failed;
    task.lease = None;
    task.execution = None;
    let snapshot = NativeTerminalSnapshot {
        schema: "monday.native_platform_terminal_snapshot.v1".into(),
        tenant: source.transfer().tenant.clone(),
        task: task.clone(),
        run: run.clone(),
        native_admission: signed,
        native_trust: trust.clone(),
        terminal_revision: 4,
        terminal_event: TerminalLedgerEvent {
            revision: 4,
            event: "stop_reconciled".into(),
            document: task,
        },
        execution_event: Some(TerminalLedgerEvent {
            revision: 2,
            event: "execution_started".into(),
            document: execution,
        }),
        result: None,
    };
    let actual = super::platform_facts::verify_snapshot(source, &snapshot, trust).unwrap();
    assert_eq!(actual, (initial, handle));
    for mutate in [
        |s: &mut NativeTerminalSnapshot| s.tenant = "foreign".into(),
        |s: &mut NativeTerminalSnapshot| s.terminal_event.event = "worker_claim".into(),
        |s: &mut NativeTerminalSnapshot| s.terminal_event.revision = 3,
        |s: &mut NativeTerminalSnapshot| s.execution_event.as_mut().unwrap().document.fence = 8,
        |s: &mut NativeTerminalSnapshot| {
            s.execution_event
                .as_mut()
                .unwrap()
                .document
                .attempt_identity = None
        },
        |s: &mut NativeTerminalSnapshot| {
            s.task
                .attempt_identity
                .as_mut()
                .unwrap()
                .native_evidence_sha256 = "e".repeat(64)
        },
        |s: &mut NativeTerminalSnapshot| {
            s.execution_event
                .as_mut()
                .unwrap()
                .document
                .attempt_identity
                .as_mut()
                .unwrap()
                .launch_lease
                .owner = "another-host".into()
        },
    ] {
        let mut changed = snapshot.clone();
        mutate(&mut changed);
        assert!(super::platform_facts::verify_snapshot(source, &changed, trust).is_err());
    }
    let mut trust = trust.clone();
    trust
        .native_reservation_keys
        .insert("another-host".into(), "f".repeat(64));
    assert!(super::platform_facts::verify_snapshot(source, &snapshot, &trust).is_err());
}
