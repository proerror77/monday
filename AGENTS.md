# Monday Agent Instructions

## Working Defaults

- Work autonomously on clear, reversible tasks. Ask only before a destructive,
  irreversible, or genuinely ambiguous action.
- Verify claims from repository, runtime, or deployed-state evidence. Preserve
  unrelated user changes.
- Use the narrowest applicable engineering workflow: triage for intake,
  diagnosing-bugs for root cause, tdd for behavior changes, implement for an
  approved change, and code-review before merge.

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
