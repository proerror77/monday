# Monday AX adoption: bounded migration slices

Stopped on 2026-09-25. Monday will not adopt AX. Parallel agent and CLI work
uses `task-batch` on the existing lease/task admission path. The local kind
cluster `monday-ax-lab` was deleted. Do not deploy AX or Substrate onto ACK.
The unused library `hft-agent-control` at `rust_hft/apps/agent-control` was
removed. Live admission remains `task-batch` in
`.github/scripts/agent-worktree-preflight.sh`. The slice text below is the
2026-09-23 plan, not a remaining instruction.

- Date: 2026-09-23.
- Baseline: `16a8bd252b7a7d8bda15e7ec2222ee874f1dcb0b`.
- Direction: [ADR-0003](../architecture/ADR-0003-ax-agent-execution.md).
- Sources: [pinned AX/Substrate research](../reports/2026-09-23-ax-adoption-research.md).
- Procedure: [agent research and delivery workflow](../agents/ax-workflow.md).
- Tracking outcome: [#1216](https://github.com/proerror77/monday/issues/1216).

This plan changes the execution environment around engineering and research
assistants while preserving Monday's native Campaign, Governance and trading
Runtime. It is not evidence that AX has been installed, that a sandbox has passed
acceptance, or that a research grant exists. Use the assigned goal's standing
authority for implementation; resolve the actual platform target and budget
before consuming new cloud or model resources.

## Current evidence and concurrent work

PRs [#1213](https://github.com/proerror77/monday/pull/1213),
[#1214](https://github.com/proerror77/monday/pull/1214) and
[#1215](https://github.com/proerror77/monday/pull/1215) merged the local task
prototype and repairs. At the baseline, its task path is a shell process with
workspace-copy recovery. It does not provide a distributed controller.

The separate worktree `monday/.worktrees/cursor/agent-ax-invoke`, branch
`cursor/agent-ax-enforce`, held uncommitted changes to the preflight script and
its test during this investigation. Preserve that writer's files. Re-read its
status and obtain a single-writer handoff before absorbing or replacing those
changes; the path's existence does not prove an agent is still running.

The operator's installed helper was still the `bd2184f` release without `task-*`
commands. Source, installation and real execution acceptance are separate
deliverables. Do not install the unreviewed dirty script as the migration.

## Slices and exit criteria

| Slice | Result | Proof required before the next dependent slice |
| --- | --- | --- |
| 0. Research and workflow | Source-backed ADR, present/future boundary, task handoff and continuation procedure | Source pins and links verified; concrete interruption, negative-result and ownership scenarios reviewed. This is the present change. |
| 1a. Task contract model | Small Rust library for task/attempt/checkpoint/receipt validation and transitions | Deterministic model and fake-provider tests; no executable, daemon, new database, vendor calls or takeover of existing ownership. |
| 1b. Shared execution admission | Wire the accepted contract into one ownership admission path across leases/tasks | Resolve the existing writer handoff first; actual concurrent-start, interruption and lease/task exclusion checks. A model test alone does not prove this slice. |
| 2. AX platform feasibility | Named isolated lab, pinned AX/Substrate/runtime images and a verified execution capability profile | ACK/API/runtime compatibility, authentication/authorization, durable control state, egress, resources, worker loss and cleanup all tested within explicit bounds. Failure is a valid result. |
| 3. One real engineering task | Adapter and Rust runner execute a bounded code change using the verified AX backend | Actual allowed/denied operations, pause/restart, retained files, no duplicate effects, terminal receipt and independently verified diff/checks. Install only the source-bound validated client. |
| 4. Research observer | AX assistant reads authorized compact Campaign status/receipts | Read-only capability; same Campaign/request/Job identities and evidence hashes; negative result preserved; no dispatch, ledger copy or holdout access. |
| 5. Governed research delegation | Typed bridge requests admitted work from the existing ACK controller | Lost-response/restart tests prove no duplicate Job, no extra trials, unchanged deadline, native settlement and ACK-side independent readback. Any real run uses its existing signed grant. |
| 6. Retirement and scale | Superseded shell task execution removed; validated concurrency expanded | Replacement acceptance, caller migration and rollback evidence; real fencing before multiple authoritative writers; no hidden local-shell fallback. |

Slice 1a and the read-only investigation portion of 2 can proceed independently.
A cluster deployment requires the resolved target and bounds; it is not a
prerequisite for writing the task contract. Do not expand all slices into new
services or issues upfront. Use one issue per next independently reviewable
behavior and a tracking issue for the migration outcome.

## First implementation contract: task model, then shared admission

**Slice 1a outcome:** model the admission, recovery and evidence constraints for
an engineering task in a small independently testable Rust library. Its checks
prove state transitions and validation only; they do not enable execution or
establish exclusion against the current shell helper.

**Placement and allowed files:** `rust_hft/apps/agent-control` as a non-default
workspace member `hft-agent-control`, plus the necessary workspace manifest/lock
changes. Start with a library and tests only, not a binary or service. The
existing `apps` root owns operator composition; engineering task control must not
become a responsibility of `alpha-harness` domain/store/engine. Do not introduce
an AX SDK, database, provider network access, research ledger access, execution
adapter dependency or production spawn in this slice. Validate the workspace
graph when adding the member. The ADR records the interface and owner.

**Slice 1b outcome:** wire this model into one shared atomic admission path so an
admitted task has exactly one active execution across lease/task entrypoints.
This requires a single-writer handoff for the existing scripts. A separate Rust
ownership store that legacy `apply` cannot see does not satisfy the contract.

**Inputs:** immutable packet/spec reference, logical task ID, writer/workspace
identity, source/image, capability references, original deadline and actual
resource limits. Model-spend limits and research-trial limits are distinct.

**Outputs:** in 1a, versioned types, validated transitions and receipt/checkpoint
verification, with a fake provider for deterministic tests. In 1b, actual occupied
ownership records, immutable invocation-start/terminal receipts and checkpoint
publication, plus independently verified outcome or explicit unresolved state.
Keep existing audit records readable; do not add compatibility execution fallbacks.

**Acceptance cases:** 1a models these cases without spawning; 1b must demonstrate
their actual effects through both entrypoints before claiming runtime safety.

1. Concurrent starts for one task launch one worker. Different tasks sharing a
   contract/workspace also launch one worker. An active legacy lease blocks the
   task and vice versa.
2. A crash between submission and receipt retains an uncertain operation. A
   repeated request reconciles that identity and does not launch another worker.
3. A pause requested during execution confirms termination/quiescence before
   committing a checkpoint. Failure to prove it retains occupation. Killing a
   controller is not proof that its descendants stopped.
4. Publish a checkpoint to a new immutable identity atomically. An interrupted
   write leaves the previous committed checkpoint intact. Changed spec/source
   or corrupt checkpoint is rejected rather than silently resumed.
5. Recovery retains task ID, original deadline, consumed bounds and external
   operation references. File recovery starts a new recorded invocation; it does
   not promise memory or model-session restoration.
   Accumulated consumption and unresolved reservations live outside restorable
   workspace state. Unknown spending cannot be refunded by restoring an older
   checkpoint; reconcile it before allowing further spend. 1a validates the
   accounting references/transition rule; 1b and platform acceptance prove the
   selected enforcement, without replacing the existing Campaign ledger.
6. Exit zero produces an exit receipt with an unverified outcome. Only a separate
   check of the requested artifact advances it to verified completion. A valid
   research negative must remain distinct from execution failure.
7. Secret bytes never enter spec, command, log or checkpoint. Revoked authority
   prevents further effects after restoration. Cover allowed and refused
   capabilities, not just refusal cases.

Use deterministic local fake providers to test races and lost responses, clearly
labelled as fixtures. They prove the adapter contract only. Platform isolation
and real-agent acceptance require the later slices' actual evidence.

## Platform feasibility contract

Before a provisioning request, name the target host/cluster, owners, pinned
components, maximum resources/spend, absolute deadline and cleanup/rollback
identities. Reuse the established remote-build process for remote validation;
never build on an `ack-system` node.

Investigate and then measure:

- Required Kubernetes APIs, certificate identity and selected substrate runtime;
  distinguish the gVisor and microVM paths rather than assuming KVM requirements.
- AX control API authentication and caller authorization; isolation between tasks
  and workspace/secret scopes. A private IP alone is not caller authorization.
- Durable Redis/control state and reconciliation after controller restart or lost
  events; single-owner admission/fencing across restarts.
- Exact mapping and enforcement of CPU and memory fields. Refuse a requested
  bound if the selected backend cannot demonstrate it.
- Denied-by-default gateway behavior, host/port rules, redirects and actual model,
  Git and MCP transport compatibility. A declared Gateway is not proof of policy
  installation; do not start if required policy is missing or failed.
- Worker loss, committed checkpoints and restore behavior/RPO. The selected AX
  controller's recovery path must be tested separately from Substrate APIs.
- Worker capacity and interruption policy separately from Monday's CPU Spot
  research Jobs. Do not place the authority ledger in a disposable actor snapshot.

The source research records upstream gaps. Where current AX cannot meet a
mandatory capability, explicitly choose a pinned upstream fix, a narrowly owned
integration change, or a blocked adoption slice. Do not silently treat a field
in a manifest as an enforced limit or replace AX with an unrestricted shell.

## Handoff for the first slice

Use the existing handoff/machine packet. Resolve values from current state at
dispatch time; this human-readable packet is not an executable grant.

```text
From: Codex
To: Cursor
Routed by: Monk
Goal: Implement slice 1a: the engineering task contract library and deterministic transition/receipt tests.
Evidence paths: ADR-0003, AX source research, this plan, exact current base and current writer/ownership snapshot; handoff is required only for slice 1b.
Constraints: Rust; allowed files rust_hft/apps/agent-control and required workspace manifest/lock changes; one writer; library and fake-provider tests only; no bin/daemon/new database/network/spawn or edits to existing AX-enforce scripts; no cloud, model spend, research dispatch, holdout or trading effects.
Done criteria: Reviewed types/transitions and diff; concurrency-model/interruption/receipt checks; workspace graph validation; exact PR head and CI; merge/readback. No claim that runtime admission is enabled.
CONFIRM / trading gates: none; this slice cannot reach research or trading execution.
Branch: Resolve a fresh implementation branch after checking ownership.
Writer: Resolve one implementation owner and recorded worktree before editing.
```

The user may assign Codex or another implementation writer directly; the packet
does not override that assignment. Never start a second writer on the existing
dirty branch solely because a migration plan was approved.

## Completion and status reporting

Report each slice with source identity, verified evidence, unresolved dependency
and next bounded action. This migration completes only when its selected backend
is accepted, the assigned engineering/research paths run through it with native
evidence, and superseded execution paths are retired. Publishing this plan or a
green documentation PR does not complete that outcome.
