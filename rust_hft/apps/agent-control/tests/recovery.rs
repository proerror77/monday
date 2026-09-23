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
            ownership: OwnershipScopeV1 {
                repository_id: id("repo-monday"),
                filesystem_namespace: id("host-mac"),
                canonical_worktree: "/worktrees/one".into(),
                branch: "codex/task-one".into(),
                pull_request: Some(42),
                allowed_files: BTreeSet::from([WriteScopeV1::Subtree("src/one".into())]),
            },
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
) -> Result<UsageSettlementV1, Error> {
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
        independent.writer_id = id("writer-2");
        independent.ownership.canonical_worktree = "/worktrees/two".into();
        independent.ownership.branch = "codex/task-two".into();
        independent.ownership.pull_request = Some(43);
        independent.ownership.allowed_files =
            BTreeSet::from([WriteScopeV1::Subtree("src/two".into())]);
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
    assert_eq!(control.accounting().consumed, usage(55).into());
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
            consumed: usage(55).into(),
            reserved: usage(45),
            invocations: 2,
            overrun: false,
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
        Ok(UsageSettlementV1::AwaitingExternalReconciliation)
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
    for field in 0..5 {
        let mut changed = checkpoint.clone();
        match field {
            0 => changed.execution.invocation.task.spec_sha256 = digest("new spec"),
            1 => changed.execution.invocation.task.source_revision = "2".repeat(40),
            2 => changed.execution.invocation.task.image_sha256 = digest("different image"),
            3 => changed.execution.invocation.task.workspace_id = id("different-workspace"),
            _ => {
                changed.execution.invocation.task.ownership.branch = "codex/different-branch".into()
            }
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
fn a_foreign_usage_receipt_cannot_release_a_reservation_or_rewrite_settled_usage() {
    let (mut control, execution) = running();
    pause(&mut control, &execution);
    let before = control.accounting();
    let mut wrong = execution.clone();
    wrong.invocation.ordinal += 1;
    assert_eq!(
        settle(&mut control, &wrong, usage(1)),
        Err(Error::IdentityMismatch)
    );
    assert_eq!(control.accounting(), before);
    settle(&mut control, &execution, usage(59)).unwrap();
    assert_eq!(
        settle(&mut control, &execution, usage(1)),
        Err(Error::InvalidPhase)
    );
    assert_eq!(control.accounting().consumed, usage(59).into());
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
        Ok(UsageSettlementV1::AwaitingExternalReconciliation)
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

fn independent_binding() -> TaskBindingV1 {
    let mut binding = spec().binding;
    binding.task_id = id("task-2");
    binding.contract_id = id("contract-2");
    binding.workspace_id = id("workspace-2");
    binding.writer_id = id("writer-2");
    binding.ownership.canonical_worktree = "/worktrees/two".into();
    binding.ownership.branch = "codex/task-two".into();
    binding.ownership.pull_request = Some(43);
    binding.ownership.allowed_files = BTreeSet::from([WriteScopeV1::Subtree("src/two".into())]);
    binding
}

#[test]
fn renamed_tasks_cannot_bypass_writer_branch_pr_worktree_or_file_ownership() {
    for conflict in 0..7 {
        let mut claims = OwnershipClaimsV1::default();
        let original = spec().binding;
        claims.claim(original.clone()).unwrap();
        let mut different_task = independent_binding();
        match conflict {
            0 => different_task.writer_id = original.writer_id.clone(),
            1 => different_task.ownership.branch = original.ownership.branch.clone(),
            2 => different_task.ownership.pull_request = original.ownership.pull_request,
            3 => {
                different_task.ownership.canonical_worktree =
                    original.ownership.canonical_worktree.clone()
            }
            4 => {
                different_task.ownership.allowed_files =
                    BTreeSet::from([WriteScopeV1::File("src/one/lib.rs".into())])
            }
            5 => {
                different_task.ownership.allowed_files =
                    BTreeSet::from([WriteScopeV1::Subtree("src".into())])
            }
            _ => {
                different_task.ownership.allowed_files = BTreeSet::from([WriteScopeV1::Repository])
            }
        }
        assert_eq!(claims.claim(different_task), Err(Error::OwnershipConflict));
    }
    let mut claims = OwnershipClaimsV1::default();
    claims.claim(spec().binding).unwrap();
    let mut sibling = independent_binding();
    sibling.ownership.allowed_files =
        BTreeSet::from([WriteScopeV1::Subtree("src/one-more".into())]);
    claims.claim(sibling).unwrap(); // Prefix without a component boundary is disjoint.
}

#[test]
fn worktree_locations_include_the_filesystem_namespace_and_repository_scope() {
    let mut claims = OwnershipClaimsV1::default();
    let mut original = spec().binding;
    original.ownership.canonical_worktree = "/workspace".into();
    original.ownership.filesystem_namespace = id("actor-1");
    claims.claim(original.clone()).unwrap();
    let mut independent = independent_binding();
    independent.ownership.canonical_worktree = "/workspace".into();
    independent.ownership.filesystem_namespace = id("actor-1");
    assert_eq!(
        claims.claim(independent.clone()),
        Err(Error::OwnershipConflict)
    );
    independent.ownership.filesystem_namespace = id("actor-2");
    claims.claim(independent).unwrap();
    // Branches, PR numbers, paths and writer ownership are repository scoped.
    let mut another_repo = original;
    another_repo.task_id = id("task-3");
    another_repo.contract_id = id("contract-3");
    another_repo.workspace_id = id("workspace-3");
    another_repo.ownership.filesystem_namespace = id("actor-3");
    another_repo.ownership.repository_id = id("another-repo");
    claims.claim(another_repo).unwrap();
}

#[test]
fn unsupported_globs_and_noncanonical_paths_fail_before_admission() {
    for path in [
        "src/**",
        "src/*.rs",
        "src/../other",
        "./src",
        "/src",
        "src//one",
        "src/one/",
        "src/[ab]",
        "src/{a,b}",
        "src/?.rs",
    ] {
        let mut candidate = spec();
        candidate.binding.ownership.allowed_files =
            BTreeSet::from([WriteScopeV1::Subtree(path.into())]);
        assert!(matches!(
            TaskControlV1::admit(candidate.clone()),
            Err(Error::InvalidSpec)
        ));
        assert_eq!(
            OwnershipClaimsV1::default().claim(candidate.binding),
            Err(Error::InvalidSpec)
        );
    }
    for path in ["relative/worktree", "/worktrees/../one", "/worktrees//one"] {
        let mut candidate = spec();
        candidate.binding.ownership.canonical_worktree = path.into();
        assert!(matches!(
            TaskControlV1::admit(candidate),
            Err(Error::InvalidSpec)
        ));
    }
}

#[test]
fn proven_non_submission_is_terminal_without_refunding_an_unknown_operation() {
    for reason in [
        NotSubmittedReason::RejectedBeforeExecution,
        NotSubmittedReason::AuthoritativelyAbsent,
    ] {
        let mut control = TaskControlV1::admit(spec()).unwrap();
        let StartDecision::Submit(invocation) = control
            .start(0, id("create-1"), usage(60), 10, None)
            .unwrap()
        else {
            panic!("first invocation must submit")
        };
        assert_eq!(
            control.start(control.revision(), id("create-2"), usage(1), 15, None),
            Err(Error::InvalidPhase)
        );
        assert_eq!(control.accounting().reserved, usage(60));
        let receipt = NotSubmittedReceiptV1 {
            invocation: invocation.clone(),
            observed_at: 15,
            reason,
            evidence_sha256: digest("authenticated original-operation absence"),
        };
        for field in 0..5 {
            let mut altered = receipt.clone();
            match field {
                0 => altered.invocation.key.task.source_revision = "2".repeat(40),
                1 => altered.invocation.key.ordinal += 1,
                2 => altered.invocation.key.operation_id = id("other-create"),
                3 => altered.invocation.started_at -= 1,
                _ => altered.invocation.reservation = usage(1),
            }
            assert_eq!(
                control.observe_not_submitted(control.revision(), altered, 16),
                Err(Error::IdentityMismatch)
            );
        }
        for time in [9, 17] {
            let mut altered = receipt.clone();
            altered.observed_at = time;
            assert_eq!(
                control.observe_not_submitted(control.revision(), altered, 16),
                Err(Error::InvalidTime)
            );
        }
        assert_eq!(control.accounting().reserved, usage(60));
        control
            .observe_not_submitted(control.revision(), receipt.clone(), 101)
            .unwrap();
        let revision = control.revision();
        control
            .observe_not_submitted(revision, receipt.clone(), 102)
            .unwrap();
        assert_eq!(control.revision(), revision);
        assert_eq!(control.not_submitted(), Some(&receipt));
        assert_eq!(control.phase(), Phase::NotSubmitted);
        assert_eq!(control.accounting().reserved, Usage::default());
        assert_eq!(control.accounting().consumed, AccumulatedUsageV1::default());
        assert_eq!(control.accounting().invocations, 1);
        assert_eq!(
            control.start(control.revision(), id("create-2"), usage(1), 30, None),
            Err(Error::InvalidPhase)
        );
        assert!(matches!(
            control.start(control.revision(), id("create-1"), usage(60), 30, None),
            Ok(StartDecision::Reconcile(_))
        ));
        let execution = ExecutionRefV1 {
            invocation: invocation.key,
            provider_id: id("late-actor"),
        };
        assert_eq!(
            control.observe_running(control.revision(), execution),
            Err(Error::InvalidPhase)
        );
        assert_eq!(control.outcome(), None);
    }
}

#[test]
fn an_authenticated_overrun_is_counted_once_and_blocks_success_and_further_spend() {
    for actual in [usage(61), usage(101)] {
        let (mut control, execution) = running();
        let terminal = terminal(&execution, 0);
        control
            .observe_exit(control.revision(), terminal.clone(), 25)
            .unwrap();
        let receipt = UsageReceiptV1 {
            execution: execution.clone(),
            actual,
            evidence_sha256: digest("verified overrun"),
        };
        assert_eq!(
            control.settle_usage(control.revision(), receipt.clone()),
            Ok(UsageSettlementV1::Overrun)
        );
        let accounting = control.accounting();
        let revision = control.revision();
        assert_eq!(accounting.consumed, actual.into());
        assert!(accounting.overrun);
        assert_eq!(accounting.reserved, Usage::default());
        assert_eq!(control.usage_receipt(), Some(&receipt));
        assert_eq!(
            control.settle_usage(revision, receipt.clone()),
            Ok(UsageSettlementV1::Overrun)
        );
        assert_eq!(control.accounting(), accounting);
        assert_eq!(control.revision(), revision);
        assert_eq!(
            control.verify_outcome(
                revision,
                readback(terminal.clone(), VerifiedOutcome::Accepted)
            ),
            Err(Error::BudgetExceeded)
        );
        assert_eq!(
            control.start(revision, id("create-2"), usage(1), 30, None),
            Err(Error::BudgetExceeded)
        );
        control
            .observe_exit(revision, terminal.clone(), 101)
            .unwrap();
        assert_eq!(control.terminal(), Some(&terminal));
        assert_eq!(control.outcome(), None);
        assert_eq!(
            settle(&mut control, &execution, usage(1)),
            Err(Error::InvalidPhase)
        );
    }
    let (mut paused, execution) = running();
    let checkpoint = pause(&mut paused, &execution);
    assert_eq!(
        settle(&mut paused, &execution, usage(61)),
        Ok(UsageSettlementV1::Overrun)
    );
    assert_eq!(
        paused.start(
            paused.revision(),
            id("create-2"),
            usage(1),
            30,
            Some((&checkpoint, b"workspace"))
        ),
        Err(Error::BudgetExceeded)
    );
}

#[test]
fn cumulative_usage_above_u64_remains_exact_instead_of_discarding_the_receipt() {
    let mut task = spec();
    task.budget = Usage {
        model_tokens: u64::MAX,
        cpu_millis: u64::MAX,
    };
    let mut control = TaskControlV1::admit(task.clone()).unwrap();
    let first = control
        .start(0, id("create-1"), task.budget, 10, None)
        .unwrap();
    let execution = FakeProvider::default().apply(first);
    control
        .observe_running(control.revision(), execution.clone())
        .unwrap();
    let checkpoint = pause(&mut control, &execution);
    settle(
        &mut control,
        &execution,
        Usage {
            model_tokens: u64::MAX - 5,
            cpu_millis: u64::MAX - 5,
        },
    )
    .unwrap();
    let second = control
        .start(
            control.revision(),
            id("create-2"),
            Usage {
                model_tokens: 5,
                cpu_millis: 5,
            },
            30,
            Some((&checkpoint, b"workspace")),
        )
        .unwrap();
    let execution = FakeProvider::default().apply(second);
    control
        .observe_running(control.revision(), execution.clone())
        .unwrap();
    let mut terminal = terminal(&execution, 0);
    terminal.parent_checkpoint = Some(checkpoint.artifact_sha256);
    terminal.exited_at = 35;
    control
        .observe_exit(control.revision(), terminal, 35)
        .unwrap();
    let actual = Usage {
        model_tokens: 10,
        cpu_millis: 10,
    };
    assert_eq!(
        settle(&mut control, &execution, actual),
        Ok(UsageSettlementV1::Overrun)
    );
    let expected = u128::from(u64::MAX) + 5;
    assert_eq!(
        control.accounting().consumed,
        AccumulatedUsageV1 {
            model_tokens: expected,
            cpu_millis: expected
        }
    );
    assert_eq!(control.usage_receipt().unwrap().actual, actual);
    assert_eq!(
        settle(&mut control, &execution, actual),
        Ok(UsageSettlementV1::Overrun)
    );
    assert_eq!(control.accounting().consumed.model_tokens, expected);
}

#[test]
fn unresolved_effects_keep_the_reservation_but_never_discard_known_overrun_usage() {
    for exited in [false, true] {
        let (mut control, execution) = running();
        let operation = ExternalOperationV1 {
            invocation: execution.invocation.clone(),
            operation_id: id("external-1"),
            request_sha256: digest("external request"),
            capability: id("read-receipt"),
        };
        control
            .record_external(control.revision(), operation.clone(), 11)
            .unwrap();
        if exited {
            control
                .observe_exit(control.revision(), terminal(&execution, 0), 25)
                .unwrap();
        } else {
            pause(&mut control, &execution);
        }
        let usage = UsageReceiptV1 {
            execution: execution.clone(),
            actual: usage(101),
            evidence_sha256: digest("known usage despite missing external response"),
        };
        assert_eq!(
            control.settle_usage(control.revision(), usage.clone()),
            Ok(UsageSettlementV1::AwaitingExternalReconciliation)
        );
        assert_eq!(control.usage_receipt(), Some(&usage));
        assert_eq!(control.accounting().consumed, usage.actual.into());
        assert_eq!(control.accounting().reserved.model_tokens, 60);
        assert!(control.accounting().overrun);
        let retained = control.accounting();
        let revision = control.revision();
        assert_eq!(
            control.settle_usage(revision, usage.clone()),
            Ok(UsageSettlementV1::AwaitingExternalReconciliation)
        );
        assert_eq!(control.revision(), revision);
        assert_eq!(control.accounting(), retained);
        assert_eq!(
            control.start(revision, id("create-2"), Usage::default(), 30, None),
            Err(Error::BudgetExceeded)
        );
        let resolution = ExternalResolutionV1 {
            operation: operation.clone(),
            outcome: ExternalOutcome::Succeeded,
            evidence_sha256: digest("external outcome authenticated"),
        };
        let mut wrong = resolution.clone();
        wrong.operation.invocation.ordinal += 1;
        assert_eq!(
            control.resolve_external(revision, wrong),
            Err(Error::IdentityMismatch)
        );
        assert_eq!(control.accounting(), retained);
        control.resolve_external(revision, resolution).unwrap();
        assert_eq!(control.accounting().reserved, Usage::default());
        assert_eq!(control.accounting().consumed, retained.consumed);
        assert!(control.accounting().overrun);
        assert_eq!(
            control.settle_usage(control.revision(), usage.clone()),
            Ok(UsageSettlementV1::Overrun)
        );
        assert_eq!(control.usage_receipt(), Some(&usage));
        assert_eq!(
            control.start(
                control.revision(),
                id("create-2"),
                Usage::default(),
                30,
                None
            ),
            Err(Error::BudgetExceeded)
        );
        if exited {
            assert_eq!(
                control.verify_outcome(
                    control.revision(),
                    readback(terminal(&execution, 0), VerifiedOutcome::Accepted)
                ),
                Err(Error::BudgetExceeded)
            );
        }
        assert_eq!(control.outcome(), None);
    }
}
