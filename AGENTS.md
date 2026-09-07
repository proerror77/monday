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
- Research, acquisition, training, evaluation, and runtime production code is
  Rust-only; do not add Python/PyTorch/libtorch production bindings.
- Dataset, candidate, evaluation, approval, policy, feedback, and deployment
  evidence is content-addressed or append-only. Keep private signing keys and
  LLM credentials out of DuckDB and logs.
- Preserve point-in-time availability and sealed-holdout isolation. Verify
  content hashes before consuming datasets.
- Research must not import execution adapters or broadcast transactions.
  Live-small stays disabled until every order path enforces envelope order-size
  and slippage limits.
- Follow the nearest nested `AGENTS.md`; module files contain only local differences.
  `CLAUDE.md` files are entrypoints to these shared rules, not another authority.
- Within system and developer constraints, explicit user instructions take
  precedence over repository and skill guidance. Apply skills only to the
  requested task; their workflows do not grant additional authority.

- Venues share Monday's market-data and execution seams. Existing `ploy-*` names
  are migration debt; do not create a separate product or another order/risk path.

## Working rules

- Follow the user's goal and scope. Work autonomously on clear, reversible tasks;
  preserve unrelated changes and ask before destructive, irreversible, or
  genuinely ambiguous actions.
- Complete the authorized outcome using existing conversation context and
  evidence. Reuse prior authorization; ask only for material scope or behavior
  ambiguity. Answer progress questions and continue. After interruptions, recover
  completed work and pending operations before resuming without duplication.
- Before requesting a necessary approval, complete the authorized preparation
  so the user can review the concrete action. If a rule blocks progress, cite its
  file and exact instruction, explain its applicability, and distinguish the
  requirement from your interpretation. Preserve production authorization gates.
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

## Delivery authority

- Determine the highest delivery state authorized for this task from the whole
  conversation, including an explicitly accepted plan and later corrections.
  Complete that outcome. An intermediate PR or CI result does not end a larger
  authorized task. Reuse authorization while scope and target stay the same.
- A local fix defaults to code plus focused validation. A request to publish a
  PR ends at the PR only when that is the highest authorized state. Merge requires
  explicit authorization, which may already be part of the accepted plan; do not
  request it again for each planned PR. Ambiguous "publish" does not silently
  authorize production deployment. Clarify a missing delivery boundary once,
  after preparing the concrete action, and continue independent work meanwhile.
- Before an authorized merge, verify the current PR head, required checks,
  resolved review conversations, and current base compatibility. Read the actual
  repository protection settings; CI green alone is not merge permission. Use
  the configured merge method and preserve branches/worktrees.
- The configured automatic artifact workflows are standing repository automation:
  an authorized merge can trigger publication after release admission. This is a
  known merge effect, not a separate per-run human approval. Artifact publication
  does not authorize deployment, collector changes, or runtime actuation. Read
  [publication policy](docs/agents/publication.md) when changing or operating a
  publishing workflow; manual publication must be within the task's authority.
- Collector/runtime transitions require explicit production authorization and
  the applicable contract: release -> one Gate -> cutover -> runtime -> independent
  readback. A production Gate protects that transition, not development, CI,
  merge, or artifact publication. Record one controller, exact target/candidate/
  rollback identities, stop rules, automatic cleanup, and terminal evidence.
  Repeat a failed attempt only after its cause or relevant input changed.

## Evidence and safety

- Refresh only the source of truth that can affect the next decision. Recheck
  branch and live identities before publishing or mutating runtime; local state
  and old green runs are not current truth.
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
  changes when ownership and dirty state are known; use a dedicated worktree for
  concurrent, published, or multi-session work. For that worktree, record contract,
  owner, path, branch, base SHA, allowed files, and dependencies in the private
  `git rev-parse --git-path agent-worktree.yml` location. The managed preflight
  checks this record; it is not a prerequisite for ordinary local edits.
- Re-read branch, `HEAD`, status, and PR head before publishing or merging. Pause
  only the affected writes on movement or overlap; continue independent work. Do not delete branches or worktrees without explicit
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

- Start with the smallest check that can disprove the changed behavior, normally
  within 15 minutes. Expand to the owning crate/workflow or cross-crate checks
  when the changed contracts, regression risk, failures, or requested terminal
  state require them. A crate test is not mandatory solely because a file changed.
- Use `cargo test -p <crate> --locked <filter>` and scoped Clippy from the owning
  workspace as appropriate. Reuse build caches; a clean build is only for proven
  cache corruption or explicitly requested clean-room validation.
  Run `cargo metadata --locked --no-deps` only after workspace-graph changes.
- For instruction, workflow, or shell changes, run `git diff --check` plus the
  closest contract test. Report unrelated or unavailable checks separately.
- For low-impact instruction edits, use existing contract checks and review the
  rules against concrete task scenarios; do not add tests that merely assert
  wording. After relevant checks pass, repeat or broaden only for new changes,
  failures, or an unresolved risk.

## Communication

- Lead with the result in concise Chinese unless the user requests another
  language. Use plain paragraphs; use lists or tables when they aid comparison.
- Report what changed, the checks and results, and material limitations. Separate
  verified facts from inference and unknowns; avoid boilerplate summaries and
  repeating unchanged progress.
