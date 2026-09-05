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
- Follow the nearest nested `AGENTS.md`.

## Working rules

- Follow the user's goal and scope. Work autonomously on clear, reversible tasks;
  preserve unrelated changes and ask before destructive, irreversible, or
  genuinely ambiguous actions.
- Solve the problem directly. Use a skill, issue, specification, branch, or
  worktree only when it reduces uncertainty, coordinates durable work, or
  isolates concurrent writes; never create one merely to satisfy a workflow.
- For a defect, prove the root cause with an observable check, fix the
  shared cause, and rerun that check. Do not patch symptoms or repeat an unchanged
  experiment under a new task or issue.
- Deliver the complete authorized outcome with correct behavior, coherent
  architecture, and long-term maintainability. Choose the implementation on
  those merits, not line count or diff size. Cross-module fixes, refactors, and
  contract migrations are appropriate when the outcome requires them; preserve
  unrelated user changes and explain material tradeoffs.
- Backward compatibility is not a goal. Remove obsolete paths instead of adding
  shims or fallbacks; preserve applied migrations and audit history as read-only
  records.

## Default delivery loop

- Start from the terminal state the user requested. A request to fix, optimize,
  review, test, or merge authorizes only the corresponding development and Git
  states; it does not authorize artifact publication, a production Gate,
  cutover, deployment, collector mutation, or runtime mutation.
- The default development loop is `Code -> focused validation`. Extend it only
  through the explicitly requested Git state: publish a PR and stop when a PR is
  requested; verify exact-head CI and stop when CI is requested; merge only when
  merge is explicitly requested and exact-head required checks pass. A production
  Gate protects only a separately authorized collector or runtime transition.
  It is not a prerequisite for development, CI, merge, or artifact publication;
  a failed Gate blocks only its cutover.
- Cross a collector or runtime boundary only on explicit production authorization.
  Follow the applicable transition contract: `release -> one Gate -> cutover ->
  Runtime -> independent Readback`. Do not insert repeated Gates, ad hoc evidence
  stages, or unrelated investigations. Re-run a failed stage only after its cause
  or relevant input changed and state the new hypothesis.
- A control-plane code fix follows the same development loop. Keep Governance
  changes separate from the production transition they protect.

## Evidence and safety

- Refresh only the source of truth that can affect the next decision. Recheck
  branch and live identities before publishing or mutating runtime; local state
  and old green runs are not current truth.
- Keep Code, CI, merge, release, runtime, and readback as separate evidence labels,
  not a mandatory promotion checklist. Back claims with exact identities and
  direct evidence. For uncommitted local Code, identify the base SHA and reviewed
  working-tree changes, including relevant untracked files; do not imply that
  they are committed or published.
- Select development and control-plane checks by affected behavior and regression
  risk. Start with targeted checks, normally within 15 minutes, and broaden when
  cross-module contracts or new evidence require it. Run the full Gate once,
  immediately before a candidate crosses the collector or runtime boundary it protects.
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
- Runtime, deployment, and collector cutovers require one named controller,
  exact target and rollback identities, stop rules, automatic cleanup, and direct
  readback.

## Scope and ownership

- One active contract has one writer. Use the current checkout for isolated local
  changes when ownership and dirty state are known; use a recorded worktree for
  concurrent, published, or multi-session work.
- Re-read branch, `HEAD`, status, and PR head before publishing or merging. Stop
  on movement or overlap. Do not delete branches or worktrees without explicit
  authorization and safety checks.
- After a requested merge, synchronize the originating checkout when it can be
  done without overwriting other work. Preserve unrelated changes and report any
  checkout that could not be synchronized; do not silently update other sessions' worktrees.
- A PR contains one independently reviewable behavior; follow the PR template.
- Use one issue for one behavior or runtime outcome. Record bounded attempts,
  failures, cleanup, and evidence on that issue; create another only when the
  behavior, target, authority, or independently reviewable change differs.
- Runtime/tracking issues close from their own evidence, never from a PR.

## Focused validation

- Run checks that can disprove the changed behavior and cover affected contracts,
  followed by the owning crate or workflow check. Broaden to cross-crate or full
  workspace validation when justified by the change or failures, not by habit.
- From `rust_hft/`, use `cargo test -p <changed-crate> --locked` and scoped Clippy.
  Run `cargo metadata --locked --no-deps` only after workspace-graph changes.
- For instruction, workflow, or shell changes, run `git diff --check` plus the
  closest contract test. Report unrelated or unavailable checks separately.
