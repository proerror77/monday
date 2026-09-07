---
name: monday-delivery-status
description: Verify requested Monday Code, PR/CI, release, or deployment states with direct evidence. Excludes brief implementation progress and evidence inside research runs.
---

# Monday Delivery Status

Produce a read-only report of the states the user asked to verify.
The states are evidence labels, not required implementation steps. Never infer a
later state from an earlier one.

## Workflow

1. Identify the requested states and the relevant branch, PR, release, service, or environment. Inspect another state only when its evidence is necessary to prove a requested claim; do not audit every earlier state automatically.
2. Read local branch, `HEAD`, worktree status, and upstream when local Code is in scope or needed to identify the target. A named PR's CI can be verified from its current GitHub head and required checks without a local worktree audit.
3. Refresh GitHub only for the requested claim or its necessary identity dependencies.
4. Read deployment and runtime only when Runtime or Readback is requested and the target and access path are known.
5. Read the external artifact or service independently only for requested Readback. Verify that its immutable identity and configuration match Runtime.
6. Mark requested states as `passed`, `failed`, `pending`, `blocked`, or `unknown`. Cite necessary dependency evidence without turning unrequested states into additional completion requirements.

## State contract

- **Code**: the implementation is identified and relevant local validation passed. For committed work, record the exact SHA; for uncommitted work, record the base SHA and reviewed diff, including relevant untracked file contents. Never label working-tree changes as committed or published. A failed relevant check makes this state `failed`; unavailable required validation makes it `unknown`.
- **CI**: required checks for that exact PR head finished successfully.
- **Merge**: GitHub reports the exact head merged into the intended base.
- **Release**: an artifact or image exists and its immutable identity matches the merged commit.
- **Runtime**: the intended environment runs that immutable identity with the intended configuration and passing health checks.
- **Readback**: an independent query observes the expected terminal output or service behavior from the same deployed immutable identity and configuration.

Do not collapse runner outages, code failures, mergeability, approval, deployment, or runtime health into one "CI failed" or "done" result.

A production Gate failure blocks cutover only. It never blocks or downgrades Code, CI, Merge, or Release when those states have their own passing evidence.

## Stop conditions

- Audit actions are read-only; never mutate merely to improve the reported state.
- If implementation is already active, answer a brief progress question from current task evidence and continue the implementation without invoking this audit.
- Do not carry this audit workflow into a later implementation turn.
- Stop and report `blocked` if the audited branch, PR head, or in-scope working-tree contents move during the audit.
- Report authentication, network, or permission gaps as `unknown`; do not reuse stale screenshots or old green runs.

## Output

Return one row per requested state with: `State | Result | Exact identity | Direct evidence | Blocker/next check`.
End with the earliest incomplete state within the requested scope, or explicitly state that all requested states passed. Unrequested later states are not blockers.
