---
name: monday-delivery-status
description: Audit explicitly requested Monday source-delivery state across Code, CI, merge, release, runtime, and readback. Use only for PR/CI, release, deployment, go-live, or shipped-state verification; do not use for ETA, percentage-complete, or brief progress during active implementation. For evidence inside a research run, use monday-research-evidence-audit instead.
---

# Monday Delivery Status

Produce a read-only report through the terminal state the user asked to verify.
The states are evidence labels, not required implementation steps. Never infer a
later state from an earlier one.

## Workflow

1. Identify the highest state the user explicitly asked to verify and the relevant branch, PR, release, service, or environment.
2. Read local branch, `HEAD`, worktree status, and upstream only when Code or a later state is in scope.
3. Refresh GitHub only when CI, merge, release, or evidence needed for one of those requested states is in scope.
4. Read deployment and runtime only when Runtime or Readback is requested and the target and access path are known.
5. Read the external artifact or service independently only for requested Readback. Verify that its immutable identity and configuration match Runtime.
6. Mark only the states through the requested terminal state as `passed`, `failed`, `pending`, `blocked`, or `unknown`.

## State contract

- **Code**: exact commit exists and focused local validation passed; any failed focused check makes this state `failed`.
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
- Stop and report `blocked` if branch or PR head moves during the audit.
- Report authentication, network, or permission gaps as `unknown`; do not reuse stale screenshots or old green runs.

## Output

Return one row per requested state with: `State | Result | Exact identity | Direct evidence | Blocker/next check`.
End with one sentence naming the earliest incomplete state; that is the current blocker.
