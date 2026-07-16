# Comprehensive Safety Hardening Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans task-by-task.

Goal: Remove every confirmed live-safety, authentication, operator-UI, Sidecar durability, and CI gap from the 2026-07-10 review without deploying or enabling live trading.

Architecture: Make ployd the single live order/risk authority and have strategy workers use the existing control-plane contract instead of owning a second venue gateway. Put validation in shared domain types, make deployment transitions fail closed, and reduce the UI/Sidecar to canonical contracts already served by the daemon.

Tech Stack: Rust, Tokio, Serde, PostgreSQL/sqlx, TypeScript, React, Node.js, GitHub Actions.

## Global Constraints

- Do not deploy, dispatch production workflows, start live trading, or use a local PostgreSQL instance.
- Keep pm5d.threelayer.live paused; validation is build/test/dry-run only.
- Preserve wire values paper and live, but reject every unknown deployment runtime mode.
- ployd is the only owner of live venue credentials, account exposure reservation, order submission, cancellation, replacement, and reconciliation.
- Security, money, persistence, and recovery failures fail closed.
- Reuse existing workspace crates, uuid, contracts, ledgers, and control client; add no framework or queue service.
- Each task is one atomic commit and stages only its owned paths.

---

### Task 1: Trading-domain invariants and collision-proof identities

Files:
- Modify crates/ploy-trading/src/runtime.rs, orders.rs, positions.rs.
- Modify crates/ploy-operator-contracts/src/trading.rs.
- Modify crates/ploy-platform-runtime/src/runtime_support.rs and trade_submit.rs.
- Modify every direct TradingRuntime::submit_intent caller found by rg.

Interfaces:
- Produce checked TradingRuntime::submit_intent returning Result.
- Add optional request idempotency_key.
- Reuse PositionLedger::net_qty, IntentPurpose, TradeSide, and workspace uuid.

- [ ] Add failing tests:
  - duplicate_intent_and_order_ids_do_not_overwrite
  - cancel_purpose_cannot_submit_order
  - exit_cannot_increase_or_flip_position
  - fill_token_side_and_numeric_invariants_are_enforced
  Each rejection must leave the ledger unchanged.
- [ ] Run rtk cargo test -p ploy-trading and verify the new tests fail.
- [ ] Add a small TradingRuntimeError. Validate non-empty unique IDs, quantity > 0, binary-option limit price in (0,1), non-Cancel purpose, and valid reduce/exit direction before mutation.
- [ ] Validate fills before mutation: quantity > 0, price > 0, fee >= 0, matching token/side, and no invalid overfill. Count Hedge as exposure.
- [ ] Replace millisecond IDs with Uuid::new_v4. For a non-empty idempotency key, return the identical existing result instead of resubmitting.
- [ ] Verify:
  - rtk cargo test -p ploy-trading -p ploy-operator-contracts -p ploy-platform-runtime
  - rtk git diff --check
- [ ] Commit fix(trading): enforce intent and fill invariants.

### Task 2: Typed deployment modes and safe lifecycle changes

Files:
- Modify crates/ploy-operator-contracts/src/deployments.rs.
- Modify deployment records in crates/ploy-platform.
- Modify crates/ploy-deployments/src/protocol.rs and runtime.rs.
- Modify crates/ploy-platform-runtime/src/deployment_control.rs, worker_tick.rs, reconcile.rs.
- Modify typed-mode callers found by rg.

Interfaces:
- Produce DeploymentRuntimeMode::Paper and DeploymentRuntimeMode::Live, serialized as paper/live.
- Reuse DesiredState, DeploymentState, WorkerLaunchSpec, and registry persistence.

- [ ] Add failing tests:
  - unknown_runtime_mode_is_rejected
  - paper_mode_typo_cannot_launch_live
  - running_deployment_rejects_execution_spec_change
  - archived_worker_is_stopped
  - archived_orders_remain_reconcilable_until_flat
- [ ] Run contract/deployment/platform-runtime tests and verify failure.
- [ ] Replace the deployment free string with a serde enum. Match exhaustively. Paper always adds --dry-run; only Live may call canonical live submission.
- [ ] Reject bundle_id, runtime_mode, or account_id changes while running. Permit identical reapply and safe cap reduction. Do not hot-restart live workers.
- [ ] Archive sets desired state stopped, stops the worker, rejects new intents, and continues reconciliation until existing orders terminate. Reject archive while active orders or open positions remain.
- [ ] Verify:
  - rtk cargo test -p ploy-operator-contracts -p ploy-platform -p ploy-deployments -p ploy-platform-runtime -p ploy-daemon-host
  - rtk git diff --check
- [ ] Commit fix(deployments): fail closed on mode and lifecycle drift.

### Task 3: Canonical live submission, durable ambiguity, and scoped restore

Files:
- Modify crates/ploy-control-client/src/lib.rs.
- Modify crates/ploy-strategy-runtime/Cargo.toml and src/live.rs, recording.rs.
- Modify crates/ploy-strategy-bundles/src/traits.rs and engine.rs.
- Modify crates/ploy-platform-runtime/src/trade_submit.rs.
- Modify crates/ploy-daemon-host/src/runtime.rs and http.rs.
- Add one forward-only migration only if existing tables cannot represent pending_submit and unknown.

Interfaces:
- Produce ControlPlaneClient::submit_intent.
- Produce explicit submit outcome Acknowledged, Rejected, or Unknown.
- Make recorder writes return Result.
- Produce deployment-scoped restore.

- [ ] Add failing tests:
  - worker_live_submit_uses_control_plane_client
  - concurrent_account_submissions_cannot_exceed_cap
  - transport_error_stays_unknown_and_is_not_retried
  - restore_is_scoped_to_deployment
  - restore_reconstructs_positions_from_filled_orders
  - restore_failure_does_not_start_empty_live_runtime
  - live_recorder_failure_stops_submission
- [ ] Run control-client, strategy-runtime, strategy-bundles, and daemon-host tests and verify failure.
- [ ] Add authenticated ControlPlaneClient::submit_intent using the existing JSON sender. Live strategy execution calls it; dry-run keeps the simulator. Workers must not read Polymarket private-key variables.
- [ ] Before venue submit, persist pending_submit. Transport ambiguity becomes unknown, retains idempotency, emits an alert, pauses/degrades the deployment, and is never auto-retried. Venue rejection remains terminal rejected.
- [ ] Make recorder writes return Result. Live stops on persistence failure; dry-run may warn.
- [ ] Restore with deployment_id, load all orders/fills for that deployment without LIMIT 500, reconstruct positions from all fills, then recover active orders. DB or ledger mismatch blocks live startup.
- [ ] Verify:
  - rtk cargo test -p ploy-connectivity -p ploy-control-client -p ploy-strategy-bundles
  - rtk cargo test -p ploy-strategy-runtime --features full --lib
  - rtk cargo test -p ploy-daemon-host
  - rtk cargo check --locked --workspace
- [ ] Commit fix(execution): centralize live submission and recovery.

### Task 4: Fail-closed API security and deployment workflows

Files:
- Modify crates/ploy-daemon-host/src/config.rs and http.rs.
- Modify release-platform.yml, reset-strategy-runtime-evidence.yml, deploy-frontend-tango-1-1.yml, market-data-gap-audit.yml, and healthcheck-tango-1-1.yml.

Interfaces:
- Reuse AuthLevel, RequiredAccess, TANGO_1_1_KNOWN_HOSTS, and the existing main-provenance gate pattern.

- [ ] Add failing tests:
  - missing_tokens_do_not_authorize_protected_routes
  - ploy_api_key_is_admin_compatibility_alias
  - same_ip_different_ports_share_rate_limit
  - different_paths_share_rate_limit
  - expired_rate_limit_buckets_are_removed
- [ ] Delete the no-token allow branch. Normalize rate keys to IP plus auth level, retain only live buckets, and return 503 on poisoned limiter mutex.
- [ ] For deploy=true or execute=true, require main dispatch, git_ref=main, and checked-out SHA equal to origin/main. Delete allow_running.
- [ ] Replace insecure SSH options with the repository-secret known-hosts pattern, HostKeyAlias tango-1-1, and StrictHostKeyChecking yes.
- [ ] Verify:
  - rtk cargo test -p ploy-daemon-host
  - actionlint -color
  - no workflow match for StrictHostKeyChecking no, UserKnownHostsFile /dev/null, or allow_running
- [ ] Commit fix(security): close API and deployment fail-open paths.

### Task 5: Canonical frontend routes, visible errors, and dependency repair

Files:
- Modify ploy-frontend/src/services/api.ts, App.tsx, components/Layout.tsx.
- Modify TradeHistory.tsx, SecurityAudit.tsx, and legacy pages calling retired routes.
- Modify ploy-frontend/package.json and package-lock.json.

Interfaces:
- Consume existing TradingStateSnapshot, AuditLogEntry, PlatformMetrics, and ActiveAlert. Add no DTO.

- [ ] Add a route-contract test that rejects retired runtime routes and mapper/component tests proving errors are not rendered as empty success.
- [ ] Fix the shared URL builder: auth paths stay at origin root; canonical API paths receive /api.
- [ ] Use /api/trading/state for orders/fills/positions, /api/audit/logs for security, and canonical deployment actions for controls. Redirect/remove pages without a canonical equivalent. Render every query/mutation error.
- [ ] Upgrade React Router within major 6 and apply the audited lodash override. Use React.lazy for page chunks; add no bundling dependency.
- [ ] Verify:
  - npm run contracts:check --prefix ploy-frontend
  - npm run lint --prefix ploy-frontend
  - npm run build --prefix ploy-frontend
  - npm audit --omit=dev --audit-level=moderate --prefix ploy-frontend
  - no JS asset larger than 500 kB
- [ ] Commit fix(frontend): align operator UI with canonical APIs.

### Task 6: Sidecar bounded execution, durable queue, and self-modification proof

Files:
- Modify ploy-sidecar/src/index.ts.
- Modify runtime/codex-cli.ts, evaluator.ts, run-requests.ts, run-recorder.ts, self-modification.ts.
- Modify ploy-sidecar/package.json.
- Modify ploy-openclaw/workspace/HEARTBEAT.md.
- Modify daemon agent-run admission validation in crates/ploy-daemon-host/src/http.rs.

Interfaces:
- Reuse AgentToolCallRecord, JSONL queue files, run records, Node crypto, child_process.execFile, and node:timers/promises.

- [ ] Add failing self-tests:
  - matching_failed_tool_does_not_satisfy_contract
  - codex_jsonl_tool_completion_is_recorded
  - poll_cycles_never_overlap
  - terminal_attempt_is_not_replayed_after_crash
  - approval_proof_cannot_be_reused_for_another_proposal
- [ ] Parse completed tool/item events from codex exec --json into existing tool-call records. Required tools pass only with called, success, or completed.
- [ ] Validate request caps in daemon and Sidecar. Consume one turn per Codex/Grok/subagent invocation. Replace setInterval with one awaited loop. Treat USD budget as admission cap while CLI usage cost is unavailable; never report fake spend.
- [ ] After each terminal record, atomically rewrite remaining in-progress JSONL. Skip a terminal run_id/attempt on restart. Append retry before checkpointing original.
- [ ] Verify HMAC-SHA256 over proposal_id, patch_hash, and verification_profile with timingSafeEqual. Replace arbitrary shell verification with fixed execFile profiles and stage only patch paths.
- [ ] Keep OpenClaw heartbeat read-only and limited to /health, /api/system/status, /api/system/alerts, /api/deployments, and /api/trading/state.
- [ ] Verify:
  - npm run contracts:check --prefix ploy-sidecar
  - npm run build --prefix ploy-sidecar
  - npm test --prefix ploy-sidecar
  - npm audit --omit=dev --audit-level=moderate --prefix ploy-sidecar
- [ ] Commit fix(sidecar): bound execution and durable approvals.

### Task 7: CI enforcement, formatting, audit cleanup, and final verification

Files:
- Modify .github/workflows/test.yml.
- Modify Cargo.lock only for audited dependency repair.
- Modify tasks/todo.md and contradictory documentation.
- Format Rust files changed by Tasks 1-4.

- [ ] Add mandatory CI checks for rustfmt, frontend contracts/lint/build/audit, Sidecar contracts/tests/build/audit, retired routes, and insecure SSH options.
- [ ] Update active rustls/rustls-webpki lockfile entries and remove production-path advisory ignores once cargo audit passes.
- [ ] Run:
  - cargo fmt --all -- --check
  - focused Rust tests for every changed crate
  - rtk cargo check --locked --workspace
  - frontend contracts, lint, build, and audit
  - Sidecar contracts, tests, build, and audit
  - cargo audit
  - actionlint -color
  - rtk git diff --check
- [ ] Record exact results in tasks/todo.md.
- [ ] Commit ci: enforce safety and contract gates.
