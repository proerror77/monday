use hft_agent_control::*;
use std::collections::{BTreeMap, BTreeSet};

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}
fn digest(value: &str) -> Digest {
    Digest::of(value.as_bytes())
}
fn usage(tokens: u64) -> Usage {
    Usage {
        model_tokens: tokens,
        cpu_millis: tokens * 10,
    }
}

fn spec() -> TaskSpecV1 {
    TaskSpecV1 {
        binding: TaskBindingV1 {
            task_id: id("task-1"),
            contract_id: id("contract-1"),
            workspace_id: id("workspace-1"),
            writer_id: id("writer-1"),
            spec_sha256: digest("admitted packet"),
            source_revision: "1".repeat(40),
            image_sha256: digest("runner image"),
        },
        deadline: 100,
        max_invocations: 3,
        budget: usage(100),
        cpu_millicores: 500,
        memory_bytes: 256 * 1024 * 1024,
        capabilities: BTreeSet::from([id("read-receipt")]),
    }
}

/// No process or network: the count detects whether recovery would emit another
/// provider submission after the first response was lost.
#[derive(Default)]
struct FakeProvider {
    submissions: BTreeMap<Id, usize>,
}

impl FakeProvider {
    fn apply(&mut self, decision: StartDecision) -> ExecutionRefV1 {
        let invocation = match decision {
            StartDecision::Submit(invocation) => {
                *self
                    .submissions
                    .entry(invocation.key.operation_id.clone())
                    .or_default() += 1;
                invocation
            }
            StartDecision::Reconcile(invocation) => {
                assert_eq!(self.submissions.get(&invocation.key.operation_id), Some(&1));
                invocation
            }
        };
        ExecutionRefV1 {
            invocation: invocation.key,
            provider_id: id("provider-actor-1"),
        }
    }
}

fn running() -> (TaskControlV1, ExecutionRefV1) {
    let mut control = TaskControlV1::admit(spec()).unwrap();
    let decision = control
        .start(0, id("create-1"), usage(60), 10, None)
        .unwrap();
    let execution = FakeProvider::default().apply(decision);
    control
        .observe_running(control.revision(), execution.clone())
        .unwrap();
    (control, execution)
}

fn checkpoint(
    execution: &ExecutionRefV1,
    parent: Option<Digest>,
    bytes: &[u8],
) -> (QuiescenceReceiptV1, CheckpointReceiptV1) {
    let quiet = QuiescenceReceiptV1 {
        execution: execution.clone(),
        observed_at: 20,
        evidence_sha256: digest("worker stopped"),
    };
    let checkpoint = CheckpointReceiptV1 {
        execution: execution.clone(),
        quiescence_sha256: quiet.evidence_sha256,
        parent_checkpoint: parent,
        artifact_sha256: Digest::of(bytes),
        committed_at: 21,
    };
    (quiet, checkpoint)
}

fn pause(control: &mut TaskControlV1, execution: &ExecutionRefV1) -> CheckpointReceiptV1 {
    control.request_pause(control.revision()).unwrap();
    let (quiet, checkpoint) = checkpoint(execution, None, b"workspace");
    control
        .confirm_pause(
            control.revision(),
            &quiet,
            checkpoint.clone(),
            b"workspace",
            22,
        )
        .unwrap();
    checkpoint
}

fn settle(
    control: &mut TaskControlV1,
    execution: &ExecutionRefV1,
    actual: Usage,
) -> Result<(), Error> {
    control.settle_usage(
        control.revision(),
        UsageReceiptV1 {
            execution: execution.clone(),
            actual,
            evidence_sha256: digest("usage verified"),
        },
    )
}

fn terminal(execution: &ExecutionRefV1, exit_code: i32) -> TerminalReceiptV1 {
    TerminalReceiptV1 {
        execution: execution.clone(),
        parent_checkpoint: None,
        exited_at: 25,
        exit_code,
        output_sha256: digest("result"),
        stdout_sha256: digest("stdout"),
        stderr_sha256: digest("stderr"),
    }
}

fn readback(terminal: TerminalReceiptV1, outcome: VerifiedOutcome) -> OutcomeReadbackV1 {
    OutcomeReadbackV1 {
        observed_output_sha256: terminal.output_sha256,
        terminal,
        verifier_evidence_sha256: digest("independent checker"),
        outcome,
    }
}

#[test]
fn two_observers_of_one_revision_and_a_lost_create_response_submit_once() {
    let mut control = TaskControlV1::admit(spec()).unwrap();
    let mut provider = FakeProvider::default();
    let observed_revision = control.revision();
    let first = control
        .start(observed_revision, id("create-1"), usage(60), 10, None)
        .unwrap();
    let execution = provider.apply(first); // Response lost; no running receipt committed.
    assert_eq!(
        control.start(observed_revision, id("create-2"), usage(60), 11, None),
        Err(Error::StaleRevision)
    );
    assert_eq!(
        control.start(control.revision(), id("create-2"), usage(60), 11, None),
        Err(Error::InvalidPhase)
    );
    assert_eq!(control.phase(), Phase::Submitting);
    assert_eq!(control.accounting().reserved, usage(60));
    // Reconciliation is allowed after expiry, but emits no new submission.
    let replay = control
        .start(control.revision(), id("create-1"), usage(60), 101, None)
        .unwrap();
    assert!(matches!(replay, StartDecision::Reconcile(_)));
    assert_eq!(provider.apply(replay), execution);
    control
        .observe_running(control.revision(), execution.clone())
        .unwrap();
    let mut replacement = execution;
    replacement.provider_id = id("replacement-actor");
    assert_eq!(
        control.observe_running(control.revision(), replacement),
        Err(Error::IdentityMismatch)
    );
    assert_eq!(provider.submissions.values().sum::<usize>(), 1);
}

#[test]
fn task_and_legacy_claims_compete_for_the_same_contract_and_workspace() {
    for legacy_first in [false, true] {
        let mut claims = OwnershipClaimsV1::default();
        let task = spec().binding;
        let mut legacy = task.clone();
        legacy.task_id = id("legacy-lease-42");
        legacy.writer_id = id("legacy-writer");
        let (first, second) = if legacy_first {
            (legacy, task)
        } else {
            (task, legacy)
        };
        claims.claim(first.clone()).unwrap();
        claims.claim(first.clone()).unwrap(); // Same immutable claim is idempotent.
        assert_eq!(claims.claim(second.clone()), Err(Error::OwnershipConflict));
        let mut different_workspace = second.clone();
        different_workspace.workspace_id = id("other-workspace");
        assert_eq!(
            claims.claim(different_workspace),
            Err(Error::OwnershipConflict)
        );
        let mut different_contract = second;
        different_contract.contract_id = id("other-contract");
        assert_eq!(
            claims.claim(different_contract),
            Err(Error::OwnershipConflict)
        );
        let mut independent = first;
        independent.task_id = id("task-2");
        independent.contract_id = id("contract-2");
        independent.workspace_id = id("workspace-2");
        claims.claim(independent).unwrap();
    }
}

#[test]
fn pause_requires_matching_quiescence_and_a_committed_checkpoint() {
    let (mut control, execution) = running();
    let (quiet, checkpoint) = checkpoint(&execution, None, b"workspace");
    assert_eq!(
        control.confirm_pause(
            control.revision(),
            &quiet,
            checkpoint.clone(),
            b"workspace",
            22
        ),
        Err(Error::InvalidPhase)
    );
    control.request_pause(control.revision()).unwrap();
    let revision = control.revision();
    let mut wrong_worker = quiet.clone();
    wrong_worker.execution.provider_id = id("another-worker");
    assert_eq!(
        control.confirm_pause(
            revision,
            &wrong_worker,
            checkpoint.clone(),
            b"workspace",
            22
        ),
        Err(Error::IdentityMismatch)
    );
    let mut premature = checkpoint.clone();
    premature.committed_at = 19;
    assert_eq!(
        control.confirm_pause(revision, &quiet, premature, b"workspace", 22),
        Err(Error::CheckpointMismatch)
    );
    assert_eq!(
        control.confirm_pause(revision, &quiet, checkpoint.clone(), b"partial write", 22),
        Err(Error::CheckpointMismatch)
    );
    assert_eq!(control.phase(), Phase::PauseRequested);
    assert!(control.checkpoint().is_none());
    assert_eq!(control.revision(), revision);
    control
        .confirm_pause(revision, &quiet, checkpoint, b"workspace", 22)
        .unwrap();
    assert_eq!(control.phase(), Phase::Paused);
    assert_eq!(control.accounting().reserved, usage(60));
}

#[test]
fn restoring_files_does_not_refund_unknown_or_settled_consumption() {
    let (mut control, execution) = running();
    let checkpoint = pause(&mut control, &execution);
    let accounting = control.accounting();
    assert_eq!(
        control.start(
            control.revision(),
            id("create-2"),
            usage(50),
            30,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::UsageUnresolved)
    );
    assert_eq!(control.accounting(), accounting);
    settle(&mut control, &execution, usage(55)).unwrap();
    assert_eq!(
        control.start(
            control.revision(),
            id("create-2"),
            usage(50),
            30,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::BudgetExceeded)
    );
    assert_eq!(control.accounting().consumed, usage(55));
    let second = control
        .start(
            control.revision(),
            id("create-2"),
            usage(45),
            30,
            Some((&checkpoint, b"workspace")),
        )
        .unwrap();
    assert!(matches!(second, StartDecision::Submit(_)));
    assert_eq!(
        control.accounting(),
        AccountingV1 {
            consumed: usage(55),
            reserved: usage(45),
            invocations: 2
        }
    );
    // Replaying the old file checkpoint cannot undo a live reservation.
    assert_eq!(
        control.start(
            control.revision(),
            id("create-3"),
            usage(1),
            31,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::InvalidPhase)
    );
    assert_eq!(control.accounting().reserved, usage(45));
}

#[test]
fn unresolved_external_effect_survives_pause_and_blocks_spend_until_reconciled() {
    let (mut control, execution) = running();
    let operation = ExternalOperationV1 {
        invocation: execution.invocation.clone(),
        operation_id: id("external-1"),
        request_sha256: digest("request"),
        capability: id("read-receipt"),
    };
    let mut refused = operation.clone();
    refused.capability = id("dispatch-research");
    assert_eq!(
        control.record_external(control.revision(), refused, 11),
        Err(Error::CapabilityDenied)
    );
    assert_eq!(
        control.record_external(control.revision(), operation.clone(), 11),
        Ok(OperationDecision::Submit)
    );
    assert_eq!(
        control.record_external(control.revision(), operation.clone(), 12),
        Ok(OperationDecision::Reconcile)
    );
    let mut another = operation.clone();
    another.operation_id = id("external-2");
    assert_eq!(
        control.record_external(control.revision(), another, 12),
        Err(Error::ExternalOperationUnresolved)
    );
    let checkpoint = pause(&mut control, &execution);
    assert_eq!(
        settle(&mut control, &execution, usage(20)),
        Err(Error::ExternalOperationUnresolved)
    );
    assert_eq!(
        control.start(
            control.revision(),
            id("create-2"),
            usage(40),
            30,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::ExternalOperationUnresolved)
    );
    assert_eq!(control.accounting().reserved, usage(60));
    let resolution = ExternalResolutionV1 {
        operation: operation.clone(),
        outcome: ExternalOutcome::Succeeded,
        evidence_sha256: digest("same operation readback"),
    };
    control
        .resolve_external(control.revision(), resolution.clone())
        .unwrap();
    control
        .resolve_external(control.revision(), resolution)
        .unwrap();
    settle(&mut control, &execution, usage(20)).unwrap();
    let next = control
        .start(
            control.revision(),
            id("create-2"),
            usage(40),
            30,
            Some((&checkpoint, b"workspace")),
        )
        .unwrap();
    let next_execution = FakeProvider::default().apply(next);
    control
        .observe_running(control.revision(), next_execution.clone())
        .unwrap();
    assert_eq!(
        control.record_external(control.revision(), operation.clone(), 31),
        Ok(OperationDecision::AlreadyResolved)
    );
    let mut rebound = operation.clone();
    rebound.invocation = next_execution.invocation;
    assert_eq!(
        control.record_external(control.revision(), rebound, 31),
        Err(Error::IdentityMismatch)
    );
    let mut altered = operation;
    altered.request_sha256 = digest("changed request");
    assert_eq!(
        control.record_external(control.revision(), altered, 31),
        Err(Error::IdentityMismatch)
    );
}

#[test]
fn changed_identity_or_corrupt_bytes_cannot_restore_a_checkpoint() {
    let (mut control, execution) = running();
    let checkpoint = pause(&mut control, &execution);
    settle(&mut control, &execution, usage(20)).unwrap();
    let before = control.accounting();
    for field in 0..4 {
        let mut changed = checkpoint.clone();
        match field {
            0 => changed.execution.invocation.task.spec_sha256 = digest("new spec"),
            1 => changed.execution.invocation.task.source_revision = "2".repeat(40),
            2 => changed.execution.invocation.task.image_sha256 = digest("different image"),
            _ => changed.execution.invocation.task.workspace_id = id("different-workspace"),
        }
        assert_eq!(
            control.start(
                control.revision(),
                id("create-2"),
                usage(40),
                30,
                Some((&changed, b"workspace"))
            ),
            Err(Error::CheckpointMismatch)
        );
    }
    assert_eq!(
        control.start(
            control.revision(),
            id("create-2"),
            usage(40),
            30,
            Some((&checkpoint, b"corrupt"))
        ),
        Err(Error::CheckpointMismatch)
    );
    assert_eq!(control.accounting(), before);
    assert_eq!(control.checkpoint(), Some(&checkpoint));
}

#[test]
fn zero_exit_needs_exact_independent_readback_and_negative_is_not_failure() {
    for outcome in [VerifiedOutcome::Accepted, VerifiedOutcome::Negative] {
        let (mut control, execution) = running();
        let receipt = terminal(&execution, 0);
        control
            .observe_exit(control.revision(), receipt.clone(), 25)
            .unwrap();
        assert_eq!(control.phase(), Phase::Exited);
        assert_eq!(control.outcome(), None);
        let evidence = readback(receipt.clone(), outcome);
        assert_eq!(
            control.verify_outcome(control.revision(), evidence.clone()),
            Err(Error::UsageUnresolved)
        );
        settle(&mut control, &execution, usage(20)).unwrap();
        let mut wrong_output = evidence.clone();
        wrong_output.observed_output_sha256 = digest("another artifact");
        assert_eq!(
            control.verify_outcome(control.revision(), wrong_output),
            Err(Error::IdentityMismatch)
        );
        let mut wrong_receipt = evidence.clone();
        wrong_receipt.terminal.stdout_sha256 = digest("rewritten logs");
        assert_eq!(
            control.verify_outcome(control.revision(), wrong_receipt),
            Err(Error::IdentityMismatch)
        );
        control
            .verify_outcome(control.revision(), evidence.clone())
            .unwrap();
        control
            .verify_outcome(control.revision(), evidence)
            .unwrap();
        assert_eq!(control.outcome(), Some(outcome));
        assert_eq!(control.phase(), Phase::Verified);
    }
    let (mut failed, execution) = running();
    let failure = terminal(&execution, 17);
    failed
        .observe_exit(failed.revision(), failure.clone(), 25)
        .unwrap();
    settle(&mut failed, &execution, usage(20)).unwrap();
    assert_eq!(
        failed.verify_outcome(
            failed.revision(),
            readback(failure, VerifiedOutcome::Negative)
        ),
        Err(Error::ProcessFailed)
    );
}

#[test]
fn deadline_revocation_and_invocation_bound_survive_recovery() {
    let (mut expired, execution) = running();
    let checkpoint = pause(&mut expired, &execution);
    settle(&mut expired, &execution, usage(20)).unwrap();
    assert_eq!(
        expired.start(
            expired.revision(),
            id("create-2"),
            usage(10),
            100,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::DeadlineExpired)
    );
    assert_eq!(expired.spec().deadline, 100);
    let (mut revoked, execution) = running();
    revoked.revoke(revoked.revision()).unwrap();
    let operation = ExternalOperationV1 {
        invocation: execution.invocation.clone(),
        operation_id: id("external-1"),
        request_sha256: digest("request"),
        capability: id("read-receipt"),
    };
    assert_eq!(
        revoked.record_external(revoked.revision(), operation, 12),
        Err(Error::Revoked)
    );
    let checkpoint = pause(&mut revoked, &execution); // Cleanup and readback remain possible.
    settle(&mut revoked, &execution, usage(20)).unwrap();
    assert_eq!(
        revoked.start(
            revoked.revision(),
            id("create-2"),
            usage(10),
            30,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::Revoked)
    );
    let mut bounded_spec = spec();
    bounded_spec.max_invocations = 1;
    let mut bounded = TaskControlV1::admit(bounded_spec).unwrap();
    let first = bounded
        .start(0, id("create-1"), usage(1), 10, None)
        .unwrap();
    let execution = FakeProvider::default().apply(first);
    bounded
        .observe_running(bounded.revision(), execution.clone())
        .unwrap();
    let checkpoint = pause(&mut bounded, &execution);
    settle(&mut bounded, &execution, usage(0)).unwrap();
    assert_eq!(
        bounded.start(
            bounded.revision(),
            id("create-2"),
            usage(1),
            30,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::BudgetExceeded)
    );
}

#[test]
fn a_forged_usage_receipt_cannot_release_a_reservation() {
    let (mut control, execution) = running();
    pause(&mut control, &execution);
    let before = control.accounting();
    let mut wrong = execution.clone();
    wrong.invocation.ordinal += 1;
    assert_eq!(
        settle(&mut control, &wrong, usage(1)),
        Err(Error::IdentityMismatch)
    );
    assert_eq!(
        settle(&mut control, &execution, usage(61)),
        Err(Error::BudgetExceeded)
    );
    assert_eq!(control.accounting(), before);
    settle(&mut control, &execution, usage(59)).unwrap();
    assert_eq!(
        settle(&mut control, &execution, usage(1)),
        Err(Error::InvalidPhase)
    );
    assert_eq!(control.accounting().consumed, usage(59));
}

#[test]
fn another_tasks_external_receipt_cannot_resolve_this_tasks_unknown_effect() {
    let (mut first, execution_a) = running();
    let mut other_spec = spec();
    other_spec.binding.task_id = id("task-2");
    let mut second = TaskControlV1::admit(other_spec).unwrap();
    let start = second
        .start(0, id("create-1"), usage(60), 10, None)
        .unwrap();
    let execution_b = FakeProvider::default().apply(start);
    second
        .observe_running(second.revision(), execution_b.clone())
        .unwrap();
    let operation_a = ExternalOperationV1 {
        invocation: execution_a.invocation,
        operation_id: id("shared-operation"),
        request_sha256: digest("same request"),
        capability: id("read-receipt"),
    };
    let operation_b = ExternalOperationV1 {
        invocation: execution_b.invocation.clone(),
        ..operation_a.clone()
    };
    first
        .record_external(first.revision(), operation_a.clone(), 12)
        .unwrap();
    second
        .record_external(second.revision(), operation_b.clone(), 12)
        .unwrap();
    let foreign = ExternalResolutionV1 {
        operation: operation_a,
        outcome: ExternalOutcome::Succeeded,
        evidence_sha256: digest("task-a-receipt"),
    };
    assert_eq!(
        second.resolve_external(second.revision(), foreign),
        Err(Error::IdentityMismatch)
    );
    pause(&mut second, &execution_b);
    assert_eq!(
        settle(&mut second, &execution_b, usage(20)),
        Err(Error::ExternalOperationUnresolved)
    );
    assert_eq!(second.accounting().reserved, usage(60));
    second
        .resolve_external(
            second.revision(),
            ExternalResolutionV1 {
                operation: operation_b,
                outcome: ExternalOutcome::Succeeded,
                evidence_sha256: digest("task-b-receipt"),
            },
        )
        .unwrap();
    settle(&mut second, &execution_b, usage(20)).unwrap();
}

#[test]
fn resume_cannot_predate_its_parent_checkpoint() {
    let (mut control, execution) = running();
    let checkpoint = pause(&mut control, &execution);
    settle(&mut control, &execution, usage(20)).unwrap();
    let before = control.accounting();
    assert_eq!(
        control.start(
            control.revision(),
            id("create-2"),
            usage(10),
            20,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::InvalidTime)
    );
    assert_eq!(control.accounting(), before);
    assert_eq!(control.phase(), Phase::Paused);
}
