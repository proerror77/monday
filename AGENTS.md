# Monday Agent Instructions

## Authority

- Monday is one Rust-first, multi-venue system. Research lives in
  `rust_hft/alpha-harness`, acquisition in `rust_hft/tools/collector`, prediction
  markets in `rust_hft/prediction-markets`, and runtime/risk/execution in
  `rust_hft/apps/live`, `rust_hft/risk-control`, and `rust_hft/execution-gateway`.
- Research may emit typed candidates and signed deployment envelopes. It may not
  submit orders, change risk limits, or resume a paused runtime. Live stays
  disabled until a separately reviewed runtime contract proves every gate.
- The production CEX research seam is `mission campaign-freeze` -> `mission
  campaign-finalize` -> `mission dispatch submit` -> generated `mission
  campaign-execute`. Direct `mission execute`, `mission run`, and `loop run`
  are diagnostic implementation surfaces, never alternate completion paths.
- The user grants standing authorization to complete assigned Monday tasks,
  including necessary code changes, validation, PR/CI, merge, artifact and image
  publication, bounded research runs, deployment/cutover, task-owned cleanup,
  and independent readback. Reuse this authorization across turns and sessions;
  do not request confirmation again merely because work reaches another stage.
  Apply it within the assigned goal, existing budgets, and explicit exclusions.
  It does not authorize unrelated actions or bypass signed grants, approval
  revocations, holdout isolation, trading gates, or other technical controls.
- Follow the nearest nested `AGENTS.md`.
- Within system and developer constraints, explicit user instructions take
  precedence over repository and skill guidance. Apply skills only to the
  requested task; their workflows do not grant additional authority.

## Working rules

- Follow the user's goal and scope autonomously under the standing authorization.
  Preserve unrelated changes. Ask only when a material scope choice is unresolved,
  an action exceeds that authorization, or a required external authority is absent;
  continue independent work while awaiting the answer.
- Use conversation context to resolve routine choices and state material
  assumptions. Treat follow-up questions as steering the active task; answer and
  continue unless the user changes the goal. After interruption, resume from
  completed work and pending operations rather than restarting.
- If a concrete blocker requires user input, first complete independent
  preparation and present one reviewable decision. Cite any blocking instruction
  and explain why existing authorization does not cover it; distinguish the
  written requirement from your interpretation.
- Solve the problem directly. Use a skill, issue, specification, branch, or
  worktree only when it reduces uncertainty, coordinates durable work, or
  isolates concurrent writes; never create one merely to satisfy a workflow.
- For a defect, reproduce the cause with an observable check, fix the shared
  cause, and verify the repaired behavior using the focused validation rules.
- Deliver the complete authorized outcome with correct behavior, coherent
  architecture, and long-term maintainability. Choose the implementation on
  those merits, not line count or diff size. Cross-module fixes, refactors, and
  contract migrations are appropriate when the outcome requires them; preserve
  unrelated user changes and explain material tradeoffs.
- Backward compatibility is not a goal. Remove obsolete paths instead of adding
  shims or fallbacks; preserve applied migrations and audit history as read-only
  records.

## Default delivery loop

- Infer the required terminal state from the assigned outcome and conversation.
  For implementation and completion requests, carry the work through the delivery
  stages needed to achieve that outcome under the standing authorization. A local
  commit or green test is not a stopping point when merge, publication, a bounded
  research run, or readback is still needed. Honor explicit analysis-only,
  local-only, draft-only, or other narrower stopping instructions.
- Validate the changed behavior and review the actual diff. Before merging, require
  the current PR head's required checks and repository protection to pass. Before
  publishing, bind the artifact to the verified source and read back its identity.
  These are execution checks, not additional requests for user permission.
- When the task requires a collector or runtime transition, follow its applicable
  contract: `release -> one Gate -> cutover -> Runtime -> independent Readback`.
  A failed Gate blocks its cutover, not independent development or publication.
  Re-run a failed stage only after its cause or relevant input changes, and state
  the new hypothesis. Keep one controller, exact target/rollback identities,
  stop rules, and task-owned cleanup for each transition.
- Keep Governance changes separate from the production transitions they protect.
  Use the canonical Campaign seam for research, preserve the granted resource and
  trial limits, and verify terminal results even when the outcome is negative.

## Evidence and safety

- Keep Code, CI, merge, release, runtime, and readback as separate evidence labels,
  not a mandatory promotion checklist. Back claims with exact identities and
  direct evidence. For uncommitted local Code, identify the base SHA and reviewed
  working-tree changes, including relevant untracked files; do not imply that
  they are committed or published.
- Keep each change independently testable and rollbackable. Do not mix Research,
  Governance, and Runtime.
- Never replace missing real data with fixtures, fabricate completeness, weaken a
  fail-closed gate, or call a successful preparation step terminal evidence.
- For a requested issue, PR, or artifact, completion requires reading back its
  identity and the properties or relationships required by that request. Do not
  require later delivery states unless requested. For an asynchronous job whose
  result is requested, verify its terminal result and output, not just submission.
- Remote build or validation tasks must use `monday-remote-build`; never place a
  workspace, toolchain, Cargo cache, or target directory on an `ack-system` node.

## Scope and ownership

- One active contract has one writer. Use the current checkout for isolated local
  changes when ownership and dirty state are known; use a recorded worktree for
  concurrent, published, or multi-session work.
- Before publishing or merging, refresh branch, `HEAD`, status, PR head, and
  relevant live identities. Reconcile task-owned movement; pause only work that
  overlaps another writer. Preserve dirty, active, or unique branches/worktrees;
  retire task-owned ones only after ownership and recovery checks.
- After a requested merge, synchronize the originating checkout when it can be
  done without overwriting other work. Preserve unrelated changes and report any
  checkout that could not be synchronized; do not silently update other sessions' worktrees.
- A PR contains one independently reviewable behavior; follow the PR template.
- Use one issue for one behavior or runtime outcome. Record bounded attempts,
  failures, cleanup, and evidence on that issue; create another only when the
  behavior, target, authority, or independently reviewable change differs.
- Runtime/tracking issues close from their own evidence, never from a PR.

## Focused validation

- Start with checks that can disprove the changed behavior, normally within
  15 minutes, then the owning crate or workflow check. Broaden only for affected
  cross-module contracts, new failures, or an identified unresolved risk.
- From `rust_hft/`, use `cargo test -p <changed-crate> --locked` and scoped Clippy.
  Run `cargo metadata --locked --no-deps` only after workspace-graph changes.
- For instruction, workflow, or shell changes, run `git diff --check` plus the
  closest contract test. Report unrelated or unavailable checks separately.
- For low-impact instruction edits, use existing contract checks and review
  concrete task scenarios. Add tests for behavior or credible regressions, not
  assertions that mirror implementation or wording.

## Progress and retry limits

- Keep a compact task checkpoint: required outcome, current stage, input/source
  identity, completed checks, pending operation IDs, and the next unresolved step.
  Reuse passing evidence while its relevant inputs are unchanged. A repeat check
  needs a changed input, an observed failure, a specific risk, or a required CI gate.
- For asynchronous work, prefer the service's wait primitive or returned cursor;
  otherwise use bounded backoff. Wait for the required terminal state or deadline
  using the same operation ID. A queued or running job is not a failed attempt.
- After three identical failures with unchanged inputs, stop that retry path,
  identify its cause or missing prerequisite, and continue independent work.
  Resume only when the cause or a relevant input changes. A new task, issue,
  worktree, or attempt name does not reset an unchanged failure.
- Once the required outcome and readback pass, finish. Avoid another review,
  cleanup, planning, or validation cycle without a newly identified requirement.

## Communication

- Lead with the result in concise Chinese unless the user requests another
  language. Use plain paragraphs; use lists or tables when they aid comparison.
- Report what changed, the checks and results, and material limitations. Separate
  verified facts from inference and unknowns; avoid boilerplate summaries and
  repeating unchanged progress.
