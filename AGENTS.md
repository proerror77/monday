# Monday Agent Instructions

## Working Defaults

- Work autonomously on clear, reversible tasks. Ask only before a destructive,
  irreversible, or genuinely ambiguous action.
- Verify claims from repository, runtime, or deployed-state evidence. Preserve
  unrelated user changes.
- Prefer the installed Matt Pocock engineering skills. Use the narrowest match:
  `to-prd` for product definition, `triage` for intake, `diagnosing-bugs` for
  root cause, `tdd` for behavior changes, `implement` for an approved change,
  and `code-review` before merge.
- Do not default to Superpowers or require the user to name a Matt skill. Use
  another installed skill only when it more directly matches the request or the
  user explicitly asks for that workflow.

## Karpathy-Inspired Coding Principles

- **Think Before Coding.** State material assumptions explicitly. When ambiguity
  would change behavior or scope, present the plausible interpretations and ask
  rather than choosing silently. Surface tradeoffs and push back when a simpler
  approach is sufficient.
- **Simplicity First.** Prefer the minimum code that solves the requested
  problem. Do not add speculative features, single-use abstractions,
  unrequested flexibility, or handling for scenarios excluded by proven
  invariants. Never simplify away validation, security, data-loss prevention,
  or other trust-boundary checks.
- **Surgical Changes.** Every changed line must trace to the request. Match the
  existing style; do not refactor, reformat, or remove unrelated code. Remove
  only the imports, variables, or functions made obsolete by the current change.
- **Goal-Driven Execution.** Define success criteria before non-trivial work;
  reproduce bugs with a focused test, preserve before/after checks for
  refactors, and loop until focused validation passes. For multi-step work, use
  a brief `step -> verification` plan.

## Pull Request Scope Guardrail

A pull request is one behavior contract and one rollout/rollback unit. Do not
combine independently reviewable work merely because it shares a broad goal.

Before opening a PR, record in its description a one-sentence change contract,
its acceptance evidence, and explicit out-of-scope work. Split the work when
any of these is true:

- it crosses trust domains (Research, Governance, Runtime) without one
  inseparable end-to-end contract;
- a collector/data contract, execution/risk policy, readiness/deployment, or
  promotion rule can be merged, reverted, or tested independently;
- it needs different reviewers or has more than one separately testable
  failure mode;
- it would exceed an available review-tool limit. Confirm that limit before
  opening the PR and keep below it.

At 25 changed files or 750 non-generated lines, stop and assess a split. Split
unless an inseparable end-to-end safety contract requires atomic delivery. An
atomic exception needs an explicitly named reviewer approval and must explain
why each part cannot be safely merged and rolled back on its own.

Use a stacked PR only for a real dependency. State its base PR and merge order.
After each layer's base is merged, that layer must compile and remain
fail-closed on its own; otherwise keep the inseparable safety contract atomic.
Put review fixes in the focused PR that owns the behavior; do not append them
to an unrelated umbrella branch. Keep lockfiles, generated artifacts, and
mechanical renames with the PR that requires them.

Every PR description must include: change contract, out of scope, dependency
or merge order, focused validation, and rollout/rollback impact; write `None`
where a field does not apply. A safety boundary needs a targeted counterexample
test, not only workspace compilation.

## Concurrent Work Control

- Each branch, worktree, and PR has exactly one write owner at a time. Record
  the owner and current head before delegating; reviewers and researchers are
  read-only unless ownership is explicitly transferred.
- Before every edit, commit, push, rebase, or merge, re-read the branch name,
  `HEAD`, worktree status, and PR head. Stop if any value moved unexpectedly;
  never absorb an unexplained concurrent change into the current PR.
- Do not run writable agents on overlapping files or the same dependency lane.
  A predecessor PR must merge before its dependent branch is rewritten or
  promoted, unless the stack and merge order were declared in advance.
- A behavior fix belongs to one PR only. Close or archive stale experiments
  before another implementation of the same contract is promoted.
- Runtime, deployment, and collector cutover commands require one named
  controller. Other tasks may inspect them read-only until control is handed
  over explicitly.
