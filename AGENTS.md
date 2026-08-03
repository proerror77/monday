# Monday Agent Instructions

## Mission and authority

- Monday is one Rust-first, multi-venue system. Research lives in
  `rust_hft/alpha-harness`, acquisition in `rust_hft/tools/collector`, prediction
  markets in `rust_hft/prediction-markets`, and runtime/risk/execution in
  `rust_hft/apps/live`, `rust_hft/risk-control`, and `rust_hft/execution-gateway`.
- Research may emit typed candidates and signed deployment envelopes. It may not
  submit orders, change risk limits, or resume a paused runtime. Live stays
  disabled until a separately reviewed runtime contract proves every gate.
- Follow the nearest nested `AGENTS.md`; prediction-market work also follows
  `rust_hft/prediction-markets/AGENTS.md`.

## Fast workflow

- Work autonomously on clear, reversible tasks. Preserve unrelated user changes;
  ask only before destructive, irreversible, or genuinely ambiguous actions.
- Use the lightest workflow that preserves evidence:
  - Read-only/status: inspect and answer directly; no PRD, issue, branch, or worktree.
  - Small specified change: no PRD or issue; focused failing check, minimum patch,
    focused validation, then diff review.
  - Defect or runtime drift: use `diagnosing-bugs` to prove the cause first.
  - Multi-step or multi-session outcome: use `to-prd`, then `to-issues`.
  - Approved issue: use `implement`, which drives `tdd`, then `code-review`.
- Prefer the narrowest applicable Matt Pocock skill. Use another skill only when
  it fits better or the user asks for it.

## Engineering decisions

- Backward compatibility is not a goal. Remove deprecated paths and their
  callers, aliases, configuration, tests, documentation, and deployment
  assets. Do not add shims, dual implementations, silent fallbacks, or migration
  branches merely to keep obsolete behavior alive.
- Ship the smallest end-to-end implementation that satisfies the current
  contract. Add capability only after that path works and has focused evidence.
- Reuse existing modules, standard-library or platform features, and installed
  dependencies before writing new infrastructure or adding a dependency. If
  they do not satisfy the requirement, prefer a mature, maintained library when
  it reduces total complexity or reliability risk. Read the relevant
  documentation and types before deciding existing code cannot support the need.
- Add a module, abstraction, configuration option, or indirect layer only when a
  current requirement creates a distinct responsibility, authority boundary, or
  lifecycle.
- Keep changes on the canonical long-term architecture. Do not introduce a
  production path known to require replacement; transitional cutover code must
  have an owner, removal condition, and removal issue.
- Before designing a non-trivial solution, inspect how mature products solve the
  same class of problem and prefer proven patterns and conventions. Localized
  changes do not require external product research.
- Applied database migrations and immutable historical evidence are records, not
  active compatibility surfaces. Preserve required audit history, but keep
  historical decoding read-only and unable to write, resume, promote, deploy, or
  execute.

## Durable guards from repeated failures

- Refresh `origin/main` and live GitHub/runtime state before claims or new work.
  A local checkout, old green run, or screen view is not current truth.
- Keep Code, CI, merge, release, runtime, and readback as separate states. Claim
  only the latest state backed by an exact SHA/digest and direct readback.
- One change is one independently testable and rollbackable behavior. Do not mix
  Research, Governance, and Runtime. Keep collector deployment, cohort/snapshot,
  evaluator/MCTS, and result publication as separate rollout units.
- Never replace missing real data with fixtures, fabricate completeness, weaken a
  fail-closed gate, or call a successful preparation step terminal evidence.
- Publishing an issue, PR, artifact, image, or job is not completion. Read back
  its relationships, checks, immutable identity, terminal result, and output.

## Scope and ownership

- One active contract has one writer and one writable branch/worktree; when
  published, it has one PR. Reuse a clean, owned worktree only for the same
  contract; otherwise create a recorded `codex/<slug>` worktree from the base SHA.
- Record `agent-worktree.yml`. Before edits, commits, rebases, pushes, or merges,
  re-read branch, `HEAD`, status, and PR head; stop on movement or overlapping ownership.
- Do not delete branches or worktrees without explicit authorization and exact
  checks for dirty files, unpushed commits, PR state, and active ownership.
- A PR is one behavior and rollback unit. Use the PR template. At 25 changed files
  or 750 non-generated lines, split unless a named reviewer approves an atomic exception.
- GitHub metadata is authoritative. Follow `docs/agents/issue-tracker.md`, issue
  templates, lifecycle checks, and `/pm:issue-close`. Runtime/tracking issues
  close from their own evidence, never from a PR.
- Runtime, deployment, and collector cutovers require one named controller,
  exact target/candidate/configuration/rollback identities, stop rules, and readback.

## Focused validation

- Run the smallest check that can disprove the change, then the owning crate or
  workflow check. Do not compile the full workspace for ordinary changes.
- From `rust_hft/`, use `cargo test -p <changed-crate> --locked` and scoped Clippy.
  Run `cargo metadata --locked --no-deps` only after workspace-graph changes.
- For instruction, workflow, or shell changes, run `git diff --check` plus the
  closest contract test. Run `.github/scripts/agent-worktree-preflight.sh` in a
  managed worktree. Report unrelated or unavailable checks separately.

## Repeated workflows become skills

- Keep this file as a router. After the same multi-step workflow succeeds twice,
  package it as `.agents/skills/<name>/SKILL.md` instead of adding its procedure here.
- One skill owns one job and states triggers, inputs, steps, stop conditions,
  verification, and output. Reuse repository scripts/docs; do not copy them.
- Validate skills manually before scheduling or write access. Runtime skills stay
  fail-closed and never broaden user authority.
