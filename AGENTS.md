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
- Research, acquisition, training, evaluation, and runtime production code is
  Rust-only. Venues share Monday's market-data, order, and risk seams.
- Research cannot import execution adapters or broadcast transactions. Live-small
  requires enforced envelope order-size and slippage limits on every order path.
- Follow the nearest nested `AGENTS.md` for local technical differences;
  `CLAUDE.md` points to these shared rules rather than defining another authority.
- Within system and developer constraints, explicit user instructions take
  precedence over repository and skill guidance. Apply skills only to the
  requested task; their workflows do not grant additional authority.

## Working context

- Resolve routine choices from the task and current evidence. Ask only when a
  material scope choice or required authority remains unresolved; continue
  independent work. Preserve unrelated changes and resume from existing task
  evidence after interruption.
- For architecture or module ownership changes, read `README.md`,
  `rust_hft/ARCHITECTURE.md`, and `docs/architecture/REPOSITORY_LAYOUT.md`, then the
  relevant implementation.
  For tracked work, use `docs/agents/issue-tracker.md` and
  `docs/agents/triage-labels.md`; use `docs/agents/domain.md` for domain docs.
- Backward compatibility is not a goal. Remove obsolete paths instead of adding
  shims or fallbacks; preserve applied migrations and audit history as read-only
  records.

## Default delivery loop

- Without a different delivery context, an implementation request defaults to
  `Code -> focused validation -> review -> PR/CI -> merge`. An assigned publication,
  research, or production outcome continues through the corresponding publication,
  run/cutover, and readback stages under the standing authorization. Use the target
  and budget established by that task; clarify an unresolved target or scope once.
  Honor explicit analysis-only, local-only, draft-only, or narrower stopping points.
- Review the actual diff. Before merging, require current-head checks, resolved
  blocking review conversations, verified base compatibility, and the repository's
  configured protection and merge method. Before publishing, bind the artifact to
  the verified source and read back its identity. These are execution checks, not
  additional requests for user permission.
- Automatic artifact publication is an expected effect of an authorized merge.
  When operating or changing publishing workflows, use
  [publication policy](docs/agents/publication.md) and reuse the task's authority.
- When the task requires a collector or runtime transition, follow its applicable
  contract: `release -> one Gate -> cutover -> Runtime -> independent Readback`.
  A failed Gate blocks its cutover, not independent development or publication.
  Re-run a failed stage only after its cause or relevant input changes, and state
  the new hypothesis. Keep one controller, exact target/rollback identities and
  stop rules. Re-read the live target and rollback identities immediately before
  cutover; drift pauses that write until reconciled. Arm automatic failure/exit
  cleanup before the first mutation, scoped to resources owned by the transition.
- Keep Governance changes separate from the production transitions they protect.
  Use the canonical Campaign seam for research, preserve the granted resource and
  trial limits, and verify terminal results even when the outcome is negative.

## Evidence and safety

- Keep dataset, candidate, evaluation, approval, policy, feedback, and deployment
  evidence content-addressed or append-only. Verify input hashes and preserve
  point-in-time availability. Keep private signing keys and LLM credentials out
  of DuckDB and logs.
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
- Keep CEX raw input preparation, freeze, training, bulk artifact verification,
  settlement and metric computation in ACK/OSS. Reuse the authenticated ACK
  result cache; a workstation may sign/inspect control metadata and format
  bounded reports, but must not gate cloud progress on bulk downloads or a local
  verifier. Keep the active ledger on its single-writer cloud volume. Follow
  `deployment/aliyun/research/README.md` for cloud reporting and recovery.
- Bound research/controller lifetime in cloud Jobs and retain the original task
  deadline on resume. Do not rely solely on a desktop PID or sticky node-retention
  annotation for cleanup; verify release of task-owned resources independently.

## Scope and ownership

- One active contract has one writer. Use the current checkout for isolated local
  changes when ownership and dirty state are known; use a recorded worktree for
  concurrent, published, or multi-session work. Record contract, owner, path,
  branch, base SHA, allowed files, and dependencies in the private
  `git rev-parse --git-path agent-worktree.yml` location used by managed preflight.
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

- Start with a check that can disprove the changed behavior, normally within
  15 minutes. Expand to owning-crate/workflow or cross-module checks when the
  affected contract, regression risk, failure, or requested outcome requires it.
  A changed file alone does not require a full crate or workspace test run.
- From the owning workspace, use `cargo test -p <crate> --locked <filter>` and
  scoped Clippy as appropriate. Reuse build caches; reserve clean builds for
  proven cache corruption or requested clean-room validation. Run
  `cargo metadata --locked --no-deps` only after workspace-graph changes.
- For instruction, workflow, or shell changes, run `git diff --check` plus the
  closest contract test. Report unrelated or unavailable checks separately.
- For low-impact instruction edits, use existing contract checks and review
  concrete task scenarios. Add tests for behavior or credible regressions, not
  assertions that mirror implementation or wording.

## Progress and completion

- For multi-session or asynchronous work, retain the outcome, source identity,
  completed checks, pending operation IDs, and next unresolved step. Reuse passing
  evidence while its inputs remain unchanged; repeat for changed inputs, a
  specific regression risk, an observed failure, or a required CI gate.
- Follow asynchronous work by its operation ID and service wait primitive or
  bounded backoff. A queued or running job is not a failed attempt. After three
  identical failures with unchanged inputs, stop that retry path until its cause
  changes; a new task or attempt name does not reset the failure count.
- Finish once the requested outcome and readback pass. Report exact identities,
  checks, results, and material limitations, separating verified facts from
  inference and unknowns.

## Communication

- Lead with the result in concise Chinese unless the user requests another
  language. Use plain paragraphs; use lists or tables when they aid comparison.
