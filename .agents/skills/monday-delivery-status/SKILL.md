---
name: monday-delivery-status
description: Report Monday source-delivery truth as separate Code, CI, merge, release, runtime, and readback states with exact live identities. Use for "现在卡在哪里", "是否完成", "能否上线", PR/CI, release, deployment, or shipped claims. For evidence inside a research run, use monday-research-evidence-audit instead.
---

# Monday Delivery Status

Produce a read-only status report. Never infer a later state from an earlier one.

## Workflow

1. Identify the requested branch, PR, release, service, and environment. If the user names only a local change, start from the current branch and `HEAD`.
2. Read local branch, `HEAD`, worktree status, and upstream without changing files.
3. Refresh live GitHub evidence. Read the PR head SHA, merge state, required checks, reviews, target branch, and latest relevant workflow or release artifact.
4. Read deployment and runtime state only when the target and access path are known. Record the deployed image/SHA, configuration identity, health, and named controller if present.
5. Read the final external artifact or service independently. Verify that its immutable identity and configuration match `Runtime`; otherwise mark `Runtime` or `Readback` `unknown`. A publish command or healthy process is not readback.
6. Mark each state `passed`, `failed`, `pending`, `blocked`, or `unknown`. Use `unknown` when direct evidence is unavailable.

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

- Stop mutation entirely; this Skill never reruns CI, merges, deploys, restarts, or changes configuration.
- Stop and report `blocked` if branch or PR head moves during the audit.
- Report authentication, network, or permission gaps as `unknown`; do not reuse stale screenshots or old green runs.

## Output

Return one row per state with: `State | Result | Exact identity | Direct evidence | Blocker/next check`.
End with one sentence naming the earliest incomplete state; that is the current blocker.
