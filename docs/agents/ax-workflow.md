# Agent research and delivery toward AX

This workflow applies when planning or migrating Monday's agent execution layer.
It also improves today's research-to-implementation handoff without requiring an
AX installation. [AGENTS.md](../../AGENTS.md) remains the authority source;
[the adoption ADR](../architecture/ADR-0003-ax-agent-execution.md) defines the
target boundary. These are operator procedures and acceptance requirements,
not claims that the current shell helper enforces a sandbox or scheduler.

## Start from one observable outcome

Keep a research question, an implementation change, and an admitted experiment
as distinct outcomes when their owners or authority differ. A source review can
produce a recommendation without creating an experiment. A normal negative
experiment can complete without producing a deployable candidate.

For a task that spans sessions, record the following in its existing issue or
handoff evidence document. A small local investigation needs no new issue.

| Record | Purpose |
| --- | --- |
| Question and decision | State the uncertainty to resolve and what finding would change the decision. |
| Source identities | Pin Monday base/head, upstream revision, relevant input references and their availability boundaries. Separate observed facts, inference and unknowns. |
| Scope and owner | Name one writer, allowed files, branch/worktree, required capabilities, and the assigned delivery endpoint. Check existing leases before starting. |
| Budget and stop | Use the actual time/resource/model-spend bounds. Reference an existing signed Campaign grant when applicable; unspecified experiment resources remain unadmitted. |
| Acceptance evidence | Define a smallest disproof, the output to inspect independently, and the stopping condition, including valid negative outcomes. |
| Continuation | Retain logical task ID, active operation/run IDs, completed evidence, unresolved effects, original deadline and next action. |

Use existing [packet fields](cursor-codex-handoff.md#packet). Put the brief's
path/hash in `Evidence paths` / `evidence_paths`, bounds in `Constraints` /
`constraints`, and observable checks in `Done criteria` / `done_criteria`.
This table does not introduce a second executable YAML schema or replace the
machine packet. Never put credentials in packets, research notes or logs.

## Research loop

1. Read primary sources at a recorded revision and the owning Monday code. Test
   the proposed mapping against failure and recovery behavior, not just API names.
2. List material gaps and alternatives. A benchmark or scale claim from upstream
   remains an upstream claim until measured on the chosen workload and platform.
3. Delegate independent questions with file ownership. The integrating writer
   reviews the actual artifacts and resolves conflicting evidence.
4. Preserve the findings in `docs/reports/`; put a durable architecture decision
   in `docs/architecture/`, and bounded implementation slices in `docs/plans/`.
   Update the current task map rather than creating another repository index.
5. Turn the next supported change into one implementation packet, with source
   identity, exact allowed files and a falsifiable acceptance case. An unresolved
   platform prerequisite stays an explicit dependency, not a fabricated result.

For quantitative research, use the existing
[Campaign preparation and recovery](../research/CAMPAIGN_WORKFLOW.md). AX agents
may propose plans and inspect authorized compact receipts. The ACK controller
owns freeze/finalize/dispatch/generated execute, grants, ledger accounting and
settlement. Keep research data, models, archives and independent verification in
ACK/OSS; AX migration does not permit downloading them to the operator laptop.
Do not create another trial family, replace a failed Job, open holdout, or reset a
deadline because an agent session restarted. Zero trades or no candidate is a
terminal finding when the native evidence proves it.

## Implementation and verification loop

1. Resolve the named base, writer and existing worktree before editing. An agent
   task must not occupy a contract already owned by a lease or another task.
2. Use the nearest existing regression that can disprove the change. Reuse
   passing evidence while its inputs remain unchanged; broaden for affected
   contracts or observed failures. A documentation change uses link/reference
   checks and concrete scenario review, not wording-only tests.
3. Review the diff and receipts independently of the executing agent's final
   message. Follow the established PR/CI/merge endpoint and publication policy.
4. When installation or runtime is in scope, bind the installed artifact to the
   validated source and inspect its actual behavior. An old installed helper,
   a new repository head, and a successful fixture run are different evidence.

For AX adoption, the first useful workload is a bounded development task against
a fixed disposable repository: make a change, run its existing check, suspend and
resume, and read back the final diff and receipt. Grant only the capabilities
needed for that task. Do not use an actual Campaign or trading service as the
first sandbox acceptance test.

## Durable execution and recovery requirements

The following are acceptance requirements for the future AX adapter, not new
commands supported by `monday-agent` today:

- Bind a logical task to its admitted spec hash, one writer/workspace, provider
  task identity, execution attempt, source/image identity and original bounds.
  A changed spec is a reviewed revision; it is not an in-place edit of history.
- Persist the operation identity before a side effect. If submission succeeds
  but the response is lost, reconcile the recorded identity before retrying.
  If absence cannot be established, preserve the occupation as unresolved.
- Distinguish a pause request, confirmed process quiescence, a durable checkpoint
  and successful restoration. A file copy does not prove a process is paused.
  Document whether recovery restores an actor, a workspace, application state,
  or merely reattaches to an already-running external Job.
- Tie checkpoints to immutable provenance. Restoring an actor does not restore
  old credentials, renew a grant or repeat a completed external side effect.
  Revalidate revocations, deadlines and writer ownership before further effects.
- Keep cumulative consumption and unresolved reservations outside the restorable
  workspace. A lost usage receipt is unknown consumption, not a refund; reconcile
  it before further spending. Reuse the existing Campaign ledger for research.
- Preserve stop/cancel requests across controller restarts. Releasing a lease or
  deleting a workspace requires observed termination of its owned execution;
  an expired deadline or missing controller PID is insufficient.
- Store terminal receipts and output digests separately from mutable provider
  phase. Process exit zero and provider `Running` are not task completion.
  Independent readback decides whether the requested outcome was achieved.
- Wait on the provider operation or recorded Job UID. Do not create a new task
  to work around an unchanged failure or unknown consumption. Retain the existing
  three-identical-failure stop rule and continue only independent work.

For an existing Campaign, restore the observer and reconcile the original
Campaign, request and Job identities. Do not snapshot or copy the active
authority-ledger/WAL into an AX workspace, and do not run two ledger owners.

## Promotion evidence for each migration slice

| Claim | Readback |
| --- | --- |
| Code | Exact source plus relevant checks, including the actual uncommitted diff when applicable. |
| CI / merge | Checks for the exact PR head, resolved blocking review, compatible base and merged identity. |
| Platform readiness | Named host/cluster and pinned AX/Substrate/images; observed isolation, recovery, secret handling and limits. Documentation alone is insufficient. |
| Task acceptance | Bounded workload, persisted identity, interruption/recovery trace, terminal receipt and independently inspected result. |
| Research integration | Existing Campaign IDs, unchanged consumption/deadline, terminal settlement and ACK-side independent readback. A successful agent task alone is insufficient. |

Only require the rows within the assigned outcome. Roll back a failed adoption
slice at its adapter or deployment boundary while retaining immutable evidence;
do not silently send a failed isolated task through the unrestricted local shell.

See the [migration slices](../plans/2026-09-23-ax-adoption.md) for the next
implementation contract and the [source research](../reports/2026-09-23-ax-adoption-research.md)
for upstream facts and unresolved platform questions.
