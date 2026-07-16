# Lessons

> **Historical standalone PLOY:** entries that mention former hosts, nested
> deployment workflows, system services, or remote recovery commands are retained
> only as incident history. They are not Monday operating instructions. Those
> hosts and workflows have no execution authority inside Monday, and live or host
> mutation remains disabled unless a separate reviewed Monday change establishes
> a new authority path.

## 2026-07-13

- Pattern: A five-minute prediction-market event was mistaken for a
  minute-level execution system, leaving primary factors behind 100ms-to-5s
  polling and a 10 Hz strategy throttle.
- Rule: Event settlement horizon and execution frequency are separate. PM5D
  live execution must remain tick-driven: ingest venue WebSocket updates
  directly and evaluate every decision-relevant tick. Delayed REST/DB ticks must
  not merge into the direct strategy hot path; recovery is a fail-closed WS
  reconnect or an explicit operator-selected `local_db` mode. Benchmark names
  must state whether they begin at raw wire parsing or at canonical tick receipt
  before describing latency as HFT-grade.

## 2026-03-02

- Pattern: Some links (including but not limited to X) fail with plain `curl` because content is JS-rendered, anti-bot protected, or behind login walls.
- Rule: If `curl` output is mostly boilerplate/error shell and no meaningful body text, switch to the `agent-browser` skill first instead of spending time on mirror/curl scraping.
- Reusable flow:
  - `agent-browser --session <name> open <target-url>`
  - `agent-browser --session <name> snapshot -i`
  - `agent-browser --session <name> get text body`
  - Extract article body from the first real article sentence onward; ignore boilerplate/error shell text.
  - Validate key numbers with a quick local calculation (Node/JS) before final analysis.
  - `agent-browser --session <name> close`
- Output standard:
  - Provide a structured analysis (core thesis, what is correct, what is inconsistent, actionable use).
  - Include relevant source URL(s) in the final answer.
  - If user asks to "save the article", default archive format should contain only two sections: `内文` and `分析` (omit capture metadata unless explicitly requested).

- Pattern: Deploying a locally-built macOS binary (`target/release/ploy` from Darwin) to a Linux server causes immediate runtime failure (`Exec format error` / binary gibberish in shell).
- Rule: All production releases must be built on Linux CI runners and validated as ELF before deploy. Never SCP a locally-built macOS binary to Linux.
- Release guardrail checklist:
  - Build on `ubuntu-latest` only for production release artifacts.
  - Use explicit production features: `claimer_daemon,api,pm_ctf,tokio/io-std`.
  - Run `file target/release/ploy` and require `ELF 64-bit LSB` in CI before deployment.
  - Deploy only the CI-built artifact, not local `target/release/ploy`.

- Pattern: On macOS, `cargo build --target x86_64-unknown-linux-gnu` can fail for crates with C build steps (for example `ring`) when `x86_64-linux-gnu-gcc` is missing.
- Rule: For local Linux artifacts from macOS, default to `cargo zigbuild --target x86_64-unknown-linux-gnu` and always verify with `file` before deploy.
- Local Linux build checklist:
  - `cargo zigbuild --release --target x86_64-unknown-linux-gnu --features "claimer_daemon,api,pm_ctf"`
  - `file target/x86_64-unknown-linux-gnu/release/ploy` must contain `ELF 64-bit LSB`.

- Pattern: 在共享工作目录直接改代码时，可能混入其他会话/任务生成的意外文件（例如未跟踪目录），导致变更污染与流程中断。
- Rule: 非 trivial 开发任务开始前，默认先创建并切换到独立 `git worktree` + 独立分支；不得在主工作目录直接实施多文件改动。
- Preflight checklist:
  - `git fetch origin`
  - `git worktree add ../<repo>-<task> -b session/<task> origin/main`
  - `cd ../<repo>-<task>`
  - `git status --short && git branch --show-current && git diff --name-only`

- Pattern: User prefers no confirmation prompts before commit/push for completed requested work.
- Rule: When requested outputs are complete and scoped changes are clear, commit and push directly without asking "要不要 commit" first.
- Commit hygiene:
  - Stage only files related to current request.
  - Keep commits atomic and avoid including unrelated local changes.
  - Report commit SHA and push result in final response.

- Historical pattern: standalone strategy operations previously ran on the former
  `tango-1-1` host rather than on a workstation.
- Retired rule: do not use the former remote-first checklist for Monday work.
  `tango-1-1` is not a current authority or deployment target, and this migration
  does not authorize SSH, startup, deployment, service recovery, or health-check
  mutations on it.

## 2026-03-05

- Pattern: User terminology correction (`ripple` -> `repo`) can change task interpretation immediately.
- Rule: When user corrects a key noun or scope term, restate the corrected scope and continue execution using that corrected term only.
- Quick check:
  - Re-anchor analysis/search to the corrected term.
  - Avoid carrying forward assumptions from the wrong term.

- Pattern: When the user asks for a large structural cleanup, repeated in-file helper extractions can still feel like local patching rather than plan-driven refactoring.
- Rule: For architecture refactors on this repo, execute the approved plan in large slices that cross file boundaries. Prefer module extraction, ownership moves, and old-path retirement over more micro-refactors inside the same oversized file.
- Execution guardrail:
  - Re-anchor on the written implementation plan before each batch.
  - Choose the highest-leverage structural slice, not the easiest local edit.
  - End each batch with compile/tests plus an atomic commit.

- Pattern: Multi-file fixes often happen in dirty worktrees.
- Rule: In dirty worktree sessions, keep progressing after explicit user approval, and isolate commits by staging only request-related files/hunks.
- Commit safety:
  - Capture preflight (`git status --short`, `git diff --name-only`) before staging.
  - Use partial staging for mixed files; do not revert unrelated edits.
  - Report remaining unstaged files after commit.

## 2026-03-09

- Pattern: Repeated confirmation prompts after a plan is already clear slow execution and create unnecessary user overhead.
- Rule: Once the user has approved or confirmed the plan, keep iterating through planned steps automatically. Only stop to ask when the next action is destructive, irreversible, production-impacting, materially changes plan scope, or is blocked by ambiguity/permissions that cannot be resolved locally.
- Execution loop:
  - Write or confirm the plan.
  - Execute the next step.
  - Verify the result.
  - Update progress.
  - Continue automatically until completion or a real stop condition appears.

- Pattern: `ploy-ci-1` is an on-demand Aliyun ECS host and may be stopped even when GitHub optimize/backtest workflows are dispatched.
- Rule: Before triggering `optimize.yml`, `backtest.yml`, or other `ploy-ci-1` workflows, verify Aliyun ECS is `Running` and the GitHub self-hosted runner is `online`. If not, start ECS first, then enable/start `actions.runner.proerror77-ploy.ploy-ci-1.service`, and only then dispatch or monitor jobs.
- Preflight checklist:
  - `aliyun ecs DescribeInstances --RegionId ap-northeast-1 --InstanceName ploy-ci-1 --PageSize 10`
  - `gh api repos/proerror77/ploy/actions/runners --jq '.runners[] | select(.name == "ploy-ci-1")'`
  - If ECS is stopped: `aliyun ecs StartInstance --RegionId ap-northeast-1 --InstanceId i-6we7z44sfbfbnosbeymz`
  - After boot, use Cloud Assistant if needed to run `systemctl enable --now actions.runner.proerror77-ploy.ploy-ci-1.service`.
  - Confirm queued workflow runs move to `in_progress` before assuming ploy-ci work has actually started.

- Pattern: A dry-run data stall on `tango-1-1` can look like a strategy,
  replay, collector, or fill-simulation bug when the real failure is public
  egress. In the 2026-05-26 incident, TCP handshakes worked but public payload
  packets from the instance were not ACKed, while Cloud Assistant/internal
  traffic still worked; Aliyun then reported `AccountInArrears` and refused
  public-IP/EIP recovery operations.
- Rule: Before changing PM5D strategy code or waiting for more dry-run records,
  verify market-data freshness and public payload flow from `tango-1-1`. If
  Cloud Assistant works but GitHub/Binance/Deribit/Polymarket HTTPS payloads
  time out, check Aliyun billing/account state and public IP/EIP status before
  debugging collectors.
- Recovery rule: After an Aliyun arrears/public-egress incident is fixed, prove
  recovery with both public payload curl probes and fresh data-plane rows/logs.
  Start a new clean dry-run evidence window after recovery; do not blend records
  collected during the outage into replay parity or promotion evidence.
- Traffic rule: Use ECS monitor counters and host logs before blaming the public
  dashboard for bandwidth consumption. In the 2026-05-27 check, nginx served
  only about `0.20MB` over 24h while ECS monitor showed hundreds of MB to GB per
  day of `InternetRX`, matching market-data collector ingress rather than page
  serving.
- Evidence checklist:
  - `curl` from `tango-1-1` to `https://api.github.com/zen`,
    Binance, Deribit, and Polymarket with timing fields.
  - `tcpdump` proving whether payload packets are ACKed, not only whether TCP
    connect succeeds.
  - `antiddos-public describe-instance-ip-address` for blackhole/defense state.
  - A paid-account/public-egress check before allocating or converting EIPs.

- Pattern: A PM5D dry-run can look profitable in the report while the active
  runner is still stuck at `max_positions`. The report may close rows by joining
  official settlement labels, but the runner's in-memory `PositionLedger` only
  frees capacity after a SELL/settlement fill.
- Rule: When dry-run stops entering after an initial batch, compare report
  closure against runtime BUY/SELL fills and strategy diagnostics. If
  `skip_max_positions` rises and runtime fills have no SELL exits, debug event
  expiry and settlement emission before changing factor logic.
- Feed rule: Local-DB PM event feeds must not mark an event expired-done when
  `resolved_up_won` is still missing. Keep the event retryable until official
  `pm_token_settlements` arrives, because observed settlement lag can exceed the
  old 120-second DB feed lookback.

- Pattern: AutoFactor / settlement-probability PRD downstream evidence does not need a live self-hosted runner once a complete sampled research snapshot artifact exists.
- Rule: Prefer GitHub-hosted `ubuntu-latest` artifact workflows for AutoFactor mining, promotion, and dry-run handoff gates. Treat `ploy-ci-1` as a legacy DB-adjacent fallback only for fresh snapshot/export work that still requires Tango private-network database access.
- Migration guardrail:
  - Do not describe `ploy-ci-1` as the default research/backtest runner for current PM5D evidence. Name the GitHub-hosted artifact path first, then call out `ploy-ci-1` only if fresh private-network DB export is the actual blocker.
  - Use `factor-walk-forward-v2-hosted-artifact.yml` or `settlement-probability-prd-gate.yml` with `snapshot_run_id`; the legacy `factor-walk-forward-v2.yml` router is removed.
  - Do not move a DB/private-endpoint workflow to GitHub-hosted runners by changing only `runs-on`; first replace the data source with a portable artifact or a hosted-safe export path.
  - When a complete sampled snapshot artifact exists, do not block strategy promotion on `ploy-ci-1` availability.

- Pattern: Runtime feedback priors can look consumed while still failing to
  affect AutoFactor selection if factor-family normalization misses generated
  selector suffixes like `_select_near_strike_ge_025` or
  `_select_entry_price_quality_ge_075`.
- Rule: When a dry-run feedback prior is supposed to avoid or penalize a losing
  runtime family, verify the artifact's `search-feedback.json`,
  `node-metrics.json`, and selected MCTS nodes show the family penalty on every
  generated selector/gate variant before concluding the search "retried"
  correctly.
- Guardrail:
  - Add normalization regressions for any new deterministic or LLM mutation
    suffix before relying on `runtime_avoid_factors`.
  - If a retry still selects a same-family variant of the losing dry-run score,
    inspect normalization first, not runtime contract plumbing.

## 2026-03-06

- Pattern: Managed strategy bootstrap can silently drift from the checked-in live strategy template and override production sizing without touching host config files.
- Rule: Managed `staggered_arb` runtime config must derive from the canonical `config/strategies/staggered_arb.toml` template and only override runtime-scoped fields like `symbols` and `series_ids`.
- Guardrail:
  - Add a regression test that asserts managed runtime config still contains `shares_per_trade = 20`.
  - Add a regression test that asserts managed runtime config does not inject `fixed_amount_usd` unless explicitly configured.

- Pattern: A release workflow can partially deploy to production and then fail late because the remote shell script is too clever.
- Rule: Keep remote deploy scripts explicit and small; prefer fixed service lists over dynamic shell pipelines for `systemctl` restarts, and syntax-check the extracted remote script before rerunning a production release.

- Pattern: In live trading flows, a partially-filled closing leg can be cancelled or fail after filling some shares; if the strategy does not accumulate those fills, the next retry can incorrectly submit the full original size again.
- Rule: For multi-step live orders, always treat partial fills as state transitions that mutate remaining exposure immediately. Retry logic must submit only residual shares and finalize the position from cumulative fills, not from the last order attempt alone.
- Verification checklist:
  - Persist cumulative filled shares/avg price/fees on every `Filled`, `Cancelled`, and `Failed` update that carries fills.
  - Clear in-flight markers after a partial close so the residual can retry cleanly.
  - Log `filled/target` progress on retries so live acceptance can prove the fix.

- Pattern: A partially-hedged cycle can expire or settle before the venue sends the final `LEG2` callback; if expiry logic ignores already-filled hedge shares, replay/live PnL can be overstated or a late callback can close the same cycle twice.
- Rule: Expiry / settlement paths must account for actual cumulative `LEG2` shares, price, and fees. After expiry settlement, retire all order tracking for that event and ignore later callbacks for positions that are no longer in `Leg1Filled`.

- Pattern: A release workflow can partially deploy the new binary and still fail availability because installed but inactive services are not explicitly started.
- Rule: Remote deploy steps must treat installed `ploy` services as start/restart targets, wait for `active`, and only then declare rollout success.

- Pattern: A live trading template can drift from the operator's intended strategy style even when the code path is unchanged, especially when timing gates are disabled and sum caps are tuned for a different regime.
- Rule: When a user describes intended entry behavior ("opening-window directional leg1" vs "strict sum-based arb"), verify both the checked-in TOML and the strategy defaults before diagnosing production inactivity.

- Pattern: Aggregate entry reject counters can be dominated by structural timing reasons and hide the actual signal path, making live strategy diagnosis look like a pricing problem when the runtime is not even reaching signal evaluation.
- Rule: For live/dry-run diagnostics, separate `entry_timing_gates` from `entry_signal_gates` and sample across a real event boundary before concluding that `sum`, `OBI`, or model thresholds are the blocker.

- Pattern: Opening-window strategies can silently miss valid entries when evaluation is only triggered by quote callbacks and the venue does not emit a fresh quote inside the narrow entry window.
- Rule: If a strategy has a short timing window, the live runtime must re-evaluate on periodic ticks using the latest cached market state, not only on event-driven quote deltas.

- Pattern: Hard-cleaning a stale live order by simply deleting its tracking entry can reopen same-event entries or duplicate hedges while the venue still has a pending or recently-filled order.
- Rule: For stale live orders, archive reconciliation metadata and keep same-event / same-position locks until a terminal callback or explicit event cleanup clears them.

- Pattern: A directional `LEG1 -> LEG2` strategy needs two different price caps: one for generic forced cleanup and another for protective stop-loss merges. Reusing one threshold for both either disables stops or allows bad timeout fills.
- Rule: Keep `force_complete_threshold` for timeout/time-safety/final-window cleanup, and use a separate `protective_close_threshold` for stop-loss / theta-driven capped-loss merges.

- Pattern: When adding volatility regime filters, old entry tests can start failing for the wrong reason because their synthetic sigma is far outside the new allowed band.
- Rule: Tests that are validating timing or quote scoping must explicitly widen `max_entry_sigma` or shrink synthetic vol so they only exercise the intended gate.

- Pattern: Protective `LEG2` stop-loss logic can cap downside, but it does not create edge if `LEG1` is opened too far from ATM or too expensively.
- Rule: For OBI-triggered long-gamma profiles, keep the entry band explicitly tight enough to preserve convexity. If replay flips from positive to negative after enabling capped-loss stops, tighten `max_leg1_price`, `max_initial_sum`, `max_trades_per_event`, and the fair-value band before touching exit logic again.

- Pattern: Once the long-gamma entry band is reasonably tight, the next failure mode is still overpaying for entries just above parity without enough directional edge.
- Rule: Treat `sum > 1.00` as premium inventory. Require stronger direction strength and stronger OBI confirmation as `sum` rises above parity, instead of only clipping with a hard `max_initial_sum`.

- Pattern: Enforcing historical Binance L2 / OBI gates in replay without checking data coverage can make a healthy strategy look broken by collapsing a window to zero trades.
- Rule: Before using a replay window as live evidence, measure `binance_lob_ticks` coverage for that window. If fresh L2 history is absent while live requires OBI, mark that window as non-parity / non-actionable instead of silently falling back to easier entry logic.

- Pattern: Validating a replay parity gate on a window without overlapping PM replay and Binance L2 history gives a false negative and wastes time.
- Rule: Use one window with current production replay behavior for regression checks, and a separate overlap window where PM replay plus `binance_lob_ticks` both exist to prove the new gate actually changes trade selection.

- Pattern: Changing a strategy profile in the checked-in TOML is not enough if the live parser defaults and replay defaults still point at the old regime.
- Rule: Whenever a strategy's intended defaults change, update all three layers together: checked-in TOML, `from_toml` parser fallbacks, and `BacktestConfig::default()`. Add a regression test for missing-field TOML parsing so old defaults cannot silently leak back in.

- Pattern: Even if parser defaults are aligned, replay can still drift from live when the CLI constructs a config directly instead of loading the canonical strategy template.
- Rule: For deployment decisions, the staggered-arb replay entrypoint must load `config/strategies/staggered_arb.toml` and only override explicit CLI-scoped inputs like symbols, capital, or one-off timing flags.

- Pattern: A green local test run can still hide an undeclared dependency on dirty worktree files; CI then fails because the pushed branch does not contain the supporting feed/schema changes.
- Rule: Before triggering a production release from a dirty worktree, inspect compile dependencies for every touched module and verify the release branch itself builds from committed files only. If a strategy change consumes a new feed enum/field, commit the feed change in the same release stack.

- Pattern: When delayed-entry logic becomes part of the core profile, legacy tests can fail for timing reasons instead of the behavior they were supposed to cover.
- Rule: Tests that are not explicitly about post-open observation delay must either set `entry_after_start_min_secs = 0` or choose timestamps safely past the minimum delay, so failures keep pointing at the intended gate.

- Pattern: Removing hard entry caps and per-event trade limits can dramatically increase turnover; a profile can stay strongly positive on long / L2-rich windows while degrading to near-flat on a noisy short window.
- Rule: For aggressive OBI long-gamma profiles, always validate at least one short production-like March window and one L2-overlap window before calling the change an improvement. Report turnover alongside PnL so overtrading is visible.

- Pattern: Polymarket submit responses can already be terminal while still omitting associated trades, which makes the raw response price look like the real fill price when it is only the submitted limit.
- Rule: When a live submit returns `Filled` / partial terminal state without trade details, query the order once before persisting `avg_fill_price`; otherwise signal history and local PnL will diverge from the venue UI.

- Pattern: The coordinator-managed strategy runtime can look healthy because it writes `signal_history`, while silently skipping `orders` persistence if it does not reuse the CLI order-store path.
- Rule: Any managed runtime that submits orders directly must normalize `client_order_id`, insert an `orders` row before execution, and update that row on both submit and poll transitions. Also treat zero-row DB updates as errors, not success.

- Pattern: A Binance depth socket can remain connected while top-of-book persistence goes stale if the collector tries to reconstruct a book from unsynchronized diff updates.
- Rule: For spot L2 features that only need top levels, prefer the combined partial-depth snapshot stream (or do full snapshot+sequence sync). Do not build a production book from raw diffs without explicit synchronization and liveness tracking.

- Pattern: Once live orders and L2 persistence are working, the next performance drag can still come from overly loose protective merge caps rather than execution bugs.
- Rule: Before changing the order state machine again, break recent live/backtest results down by exit reason. If `protective_stop_loss` dominates losses while ordinary merges are profitable, tighten `max_leg1_loss`, `force_complete_threshold`, `protective_close_threshold`, and the post-open entry window before adding more execution complexity.

- Pattern: Managed-runtime staggered-arb can diverge sharply from replay when a partial `LEG2` leaves a residual below Polymarket's venue minimum, because live will fail every submit while replay may keep assuming the remainder is fillable.
- Rule: Apply the same Polymarket minimum-order rule (`>= 5 shares` and `>= $1` notional at the attempted price) to every live and replay `LEG2` submit. Never resubmit impossible residual sizes.

- Pattern: A "smart" final-window single-leg hold can make the profile look profitable in isolated cases, but it silently changes a hedge strategy into a directional settlement bet and breaks live/replay comparability.
- Rule: For hedge-disciplined staggered-arb profiles, final-window logic should always attempt an explicit `LEG2` close when thresholds allow. Single-leg settlement should be a residual fallback, not an intentional preferred path.

- Pattern: A single overlap window with full PM + Binance L2 coverage can still be an unusually favorable regime and materially overstate staggered-arb edge.
- Rule: Do not tune staggered-arb off one strong window like `2026-02-24` alone. Require at least one recent live-like window plus adjacent independent overlap windows before treating a parameter change as robust.

- Pattern: Fixed `force_complete_threshold` / `protective_close_threshold` values can overpay for early hedges even when the same cap is appropriate near expiry.
- Rule: Treat close thresholds as final caps, not flat gates. In staggered-arb, derive stricter early-window protective/forced thresholds from time remaining, then let them widen toward the configured cap as expiry risk rises.

- Pattern: Strengthening OBI logic can be correct in isolation yet leave primary validation windows unchanged when those windows were never bottlenecked by the old OBI gate.
- Rule: After adding a new staggered-arb signal feature, compare the same recent live-like and adjacent overlap windows first. If trade count and PnL stay flat, stop loosening entry further and shift attention to exit timing or `LEG2` execution quality.

- Pattern: When staggered-arb mixes `5m` and `15m` windows under one profile, a broader timeframe can silently dilute or reverse edge even if the aggregate logic looks sensible.
- Rule: Before adding new timing complexity or loosening entry further, decompose replay by window duration. If one timeframe is consistently negative across the recent live-like and adjacent overlap windows, remove that timeframe from the canonical profile before tuning anything else.

- Pattern: A new staggered-arb control path can be logically sound and fully tested, yet still underperform a simpler parameter-only change on the actual production-like validation windows.
- Rule: After implementing new exit logic, always run a parameter sweep against the current best simple baseline before keeping the extra complexity. If a tighter close cap outperforms the new branch on the recent live-like window and independent overlap windows, disable the new branch by default and ship the simpler profile.

- Pattern: Tightening staggered-arb close caps slightly can improve both recent live-like windows and independent overlap windows, while caps that are too tight or too loose both degrade results in different ways.
- Rule: For current 5m-only staggered-arb, treat `1.06` as the new protective/forced close baseline. Re-validate `1.05`, `1.06`, and `1.07+` around any future profile change instead of assuming the old `1.08` cap is still appropriate.

- Pattern: Live staggered-arb can diverge sharply from replay when a partially filled `LEG2` leaves a residual position smaller than venue minimum order size; replay assumes any positive remainder can be completed, while live keeps retrying an impossible order.
- Rule: Before trusting live-vs-replay comparisons, inspect one concrete cycle across `orders`, `signal_history`, and venue constraints. For Polymarket-style execution, never resubmit residual `LEG2` orders below venue minimums (`5` shares and `$1` notional); clamp or settle them explicitly instead of retrying forever.

- Pattern: Managed-runtime staggered-arb currently records fills in `orders` and `signal_history`, but not in the `fills` ledger table, so relying on `fills` alone falsely suggests no live executions occurred.
- Rule: When reconciling live trading records, treat `orders.filled_shares` plus `signal_history` as the source of truth until managed-runtime fill events also persist into `fills`. Do not declare “no成交” from an empty `fills` query without checking `orders`.

- Pattern: Internal staggered-arb `split_arb_cycle_completed` totals can materially diverge from the user-visible Polymarket wallet 1D PnL, because official portfolio PnL is wallet-level and includes inventory mark-to-market while `cycle_completed` is only a strategy-emitted subset.
- Rule: For any live strategy performance review, reconcile three views in this order: (1) official Polymarket wallet 1D / profile PnL, (2) public wallet activity cashflow by market/event, (3) internal `signal_history` / `orders`. Never present `cycle_completed` totals alone as the user's真钱表现.

- Pattern: Using shell `exec_command` to invoke `apply_patch` triggers avoidable tool warnings and makes edit provenance harder to audit.
- Rule: In Codex sessions, always use the dedicated `apply_patch` tool for manual file edits. Do not run `apply_patch` through shell commands.

- Pattern: PM quote persistence gates still overstate hedgeability if a side disappears long enough to go stale and then reappears; carrying forward the old `first_seen_at` makes a fresh quote look durable when it is not.
- Rule: Whenever staggered-arb tracks quote persistence, reset persistence timing after stale quote gaps, not only when an explicit `ask=None` update arrives. Add a regression test for the stale-gap reappearance path in both live and replay code.

- Pattern: Generic `Max retries exceeded: 3` hides the real managed-order failure mode and makes live wallet-loss debugging nearly impossible.
- Rule: For managed execution retries, stop immediately on clearly non-retryable validation/auth/signing/liquidity errors, and when retries are exhausted surface the last underlying submit error verbatim in the returned error and observability path.

- Pattern: Aggregate staggered-arb gate counters can make BTC look like “not configured” when it is actually being filtered by symbol-specific entry conditions.
- Rule: Whenever diagnosing missing live trades for one symbol in a multi-symbol strategy, emit and inspect per-symbol gate counters in addition to aggregate summary counters. Do not infer symbol-level behavior from aggregate reject totals alone.

- Pattern: Managed runtime orders can silently lose gateway/idempotency guarantees if the strategy-generated `client_order_id` is not propagated into the actual `OrderRequest`.
- Rule: For coordinator-managed order submission, the strategy-generated action ID must be copied into both `client_order_id` and `idempotency_key` before execution or persistence. Add a regression test for that normalization path.

## 2026-03-09

- Pattern: During architectural cleanup, the user does not want a stream of tiny isolated refactors with frequent stop-and-report pauses; that makes structural progress look smaller than it is and slows down legacy retirement.
- Rule: For active refactor sessions, batch work into larger ownership cuts, keep multiple agents busy on non-overlapping slices, and only stop at natural atomic checkpoints (validated commit boundaries), not after every small extraction.

- Pattern: Parallel agent work can re-dirty the integration worktree with unrelated experiments, which blurs commit boundaries and slows structural refactors.
- Rule: After every parallel batch, compare the worktree against the current ownership plan and evict unrelated agent edits before validation or staging. Do not let maintenance/perf side experiments bleed into the current atomic refactor slice.

## 2026-03-11

- Pattern: Running `cargo fmt --all` on a branch that already contains many in-flight structural edits explodes the worktree and destroys atomic bugfix boundaries.
- Rule: For focused bugfix slices on this repo, format only the owned files with `rustfmt --edition 2024 <paths>` unless the branch is confirmed clean and the intent is an explicit repo-wide formatting sweep.

## 2026-04-03

- Pattern: Users treat dry run and backtest as parity checks, but the repo historically used live ingestion for `dryrun` and database reconstruction for `backtest`, so the two modes could disagree for feed-coverage reasons rather than strategy logic.
- Rule: Do not claim dry-run/backtest equivalence unless they consume the same canonical `MarketUpdate` sequence. Distinguish three modes explicitly: research backtest (historical DB reconstruction), dry run (live ingestion), and replay (recorded canonical feed).
- Parity checklist:
  - When validating dry-run behavior historically, prefer replaying a recorded canonical feed instead of reconstructing from tables.
  - If a result difference is caused by discovery/quote coverage, diagnose the ingestion path before blaming the strategy.
  - For new feed changes, add a record→replay regression proving the same `MarketUpdate` log produces the same fills and PnL.

## 2026-04-06

- Pattern: When a GitHub deployment path is slow or appears stuck, it is tempting to bypass it with manual SCP/upload to get the host unstuck quickly.
- Rule: On this repo, deployment code for trading hosts must not be uploaded manually. Use GitHub Actions artifacts and the checked-in workflow only. If the GitHub path is blocked, debug or fix the workflow, or stop and report, but do not hand-deploy binaries or code to the host.
- Deployment guardrail:
  - Treat manual upload of deployable code or binaries to trading hosts as prohibited, even for rollback or hotfix pressure.
  - If a workflow is hung in upload/restart, inspect the workflow and logs first, not the host filesystem as an alternate deploy path.
  - If recovery is urgently needed, use the last known good GitHub-built artifact and documented GitHub release path, not a locally produced binary.

## 2026-04-07

- Pattern: The user does not want new TypeScript by default when the same behavior can live in Rust.
- Rule: On this repo, use Rust for backend, control-plane, research, oversight, and the agent sidecar. TypeScript is limited to the existing frontend UI and its generated contract bindings.
- Implementation guardrail:
  - Before adding non-trivial TS logic, ask whether the behavior can instead live in `ployctl`, `ployd`, or a Rust crate.
  - If TS is temporarily unavoidable, keep it as a thin wrapper over canonical Rust surfaces rather than embedding core logic there.
  - Do not move strategy, execution, or oversight policy deeper into TS just because the sidecar currently runs on Node.

## 2026-05-11

- Pattern: Hosted artifact factor workflows become slow and brittle when they
  keep hard-coded date defaults that must exactly match a retained research
  snapshot manifest.
- Rule: Artifact-backed research loops should resolve default windows from the
  snapshot manifest and run sub-window searches against the retained artifact.
  Do not re-export a sampled snapshot or rely on hidden inclusive-date semantics
  just to test another factor window.

## 2026-05-13

- Pattern: PM5D settlement-probability dry-run rows from earlier configs can
  contaminate post-handoff profitability reads, making a rejected strategy look
  like a current candidate or making a current candidate look worse for the
  wrong reason.
- Rule: Before interpreting PM5D dry-run performance after a strategy cutover,
  prove the observation window is clean. Prefer the reset workflow artifact
  `post-reset-clean-baseline-gate.json` with `status=passed`; otherwise run
  `check_dryrun_candidate_gate.py --mode clean-baseline` or
  `dryrun-candidate-gate.yml`. Do not promote from a report that still contains
  residual runtime orders/fills.

- Pattern: GitHub-hosted artifact workflows are now the default efficient
  research/search surface once a complete sampled snapshot artifact exists, but older notes
  and habits still point agents at `ploy-ci-1`.
- Rule: For PM5D AutoFactor mining, walk-forward promotion, and dry-run handoff
  checks, use GitHub-hosted artifact workflows by default. Treat `ploy-ci-1` as
  a legacy DB-adjacent fallback only when a fresh DB export/snapshot is truly
  required.

- Pattern: A successful reset preview or green workflow does not prove runtime
  evidence was cleared; a destructive reset can be correctly blocked while the
  deployment is still running.
- Rule: Destructive runtime-evidence cleanup requires explicit operator
  approval, target deployment paused/stopped, `execute=true`,
  `allow_running=false`, `confirm=delete-strategy-runtime-evidence`, backup
  artifacts, and a passing post-reset clean-baseline gate before any fresh
  dry-run observation or strategy-quality claim.

## 2026-07-11

- Pattern: Process fixtures passed locally but failed on GitHub because they
  inferred child readiness from `spawn()` or accepted the first log row while
  asserting behavior from a later restart.
- Rule: Synchronize worker-process fixtures on an explicit child-ready signal
  and wait for the exact expected event count before assertions. Do not weaken
  production PID identity checks or add arbitrary sleeps to mask fixture races.
  Retry the runtime tick while waiting for a spawned fixture under CI load, and
  explicitly stop the final child before the test returns so parallel suites do
  not accumulate orphan workers and starve later spawns.

## 2026-07-13

- Pattern: A generic “exchange trading works” check can accidentally prove the
  Binance reference-data lane while leaving the requested Polymarket quote,
  order, settlement, and wallet lifecycle unverified.
- Rule: For Polymarket readiness, report four separate truths: live quote/data
  reachability, local order-construction/reconciliation tests, executable-depth
  replay/backtest evidence, and authenticated account/order evidence. Never use
  Binance feed health or an unauthenticated quote as proof that a Polymarket
  order can be accepted; do not place a funded test order without explicit live
  authorization.
