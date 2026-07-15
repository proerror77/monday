# Local Trading Readiness Implementation Plan Index

> For agentic workers: REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans task-by-task.

Goal: Turn the approved local-readiness design into five independently reviewable programs that make Ploy safe to research, dry-run, package, and later deploy without creating cloud resources or enabling live trading in this phase.

Architecture: Keep `ployd` as the only wallet/order authority, keep research event-rooted and horizon-specific, keep the Sidecar research-only, and prepare mutually exclusive research/trade bundles whose future deployment is SHA-pinned and fail-closed.

Tech Stack: Rust 1.91, Tokio, Serde, rust_decimal, SQLx/PostgreSQL, Polymarket official Rust SDK V2, Polars/Parquet, TypeScript/Node.js, Python standard library, Bash, systemd, and GitHub Actions.

## Global Constraints

- Do not create or change ECS, RDS, OSS, KMS, RAM, VPC, security groups, DNS, GitHub environments, repository secrets, remote services, wallets, allowances, orders, redemptions, or live deployments.
- Do not run a command that assumes local PostgreSQL. Database-backed evidence runs later through GitHub Actions against the research environment.
- Keep every live deployment paused. No task may fund a wallet, resume a live deployment, or dispatch a deploy workflow.
- Treat `docs/PROJECT_SEMANTICS.md` as the research and promotion source of truth.
- Preserve one-event-one-decision accounting. Diagnostic entry grids are not deployable trades.
- Use event-held-out train/validation/test splits, train-only normalization, at least three distinct walk-forward windows, executable prices, fees/slippage/latency, PnL, ROI, average entry, and maximum drawdown.
- Keep DL and RL blocked until their explicit foundation gates pass.
- Keep strategy/domain crates independent of Polymarket SDK types. SDK-specific code stays in connectivity or market-data integration modules.
- Keep the retired claimer retired. A future manual redeem capability is a separate, operator-approved account-ops slice and is not part of the default runtime.
- Stage explicit paths only. Each task ends with its focused tests, `rtk git diff --check`, and one atomic commit.
- Implement the nine approved slices below as nine branches/PRs. Each dependent slice branches from updated `main` after its prerequisite merges; do not combine adjacent slices just because they share one plan document.

## Accepted Design

Source specification:

- `docs/superpowers/specs/2026-07-11-local-trading-readiness-design.md`

Evidence stage for this planning slice: `implementation hardening design`.

The five implementation programs are:

| Order | Plan | Produces | Does not produce |
| --- | --- | --- | --- |
| 1 | `2026-07-11-live-execution-safety.md` | fail-closed admission, wallet/cap guards, FAK, venue truth, emergency quiesce | V2 SDK migration, settlement, cloud deploy |
| 2 | `2026-07-11-polymarket-v2-settlement.md` | official V2 adapter and confirmed redemption accounting | manual redeem broadcast, live enablement |
| 3 | `2026-07-11-horizon-safe-research.md` | PM5D/PM15D contracts, PostgreSQL portable export, shared phase APIs, rolling fixture evidence | profitable-strategy claim, DL/RL |
| 4 | `2026-07-11-research-orchestrator.md` | research-only Sidecar orchestration and structural mutation denial | wallet/order/redeem/deploy tools |
| 5 | `2026-07-11-dual-host-packaging.md` | mutually exclusive bundles, research Sidecar service, immutable activation, Aliyun workflow guards | cloud resource creation or remote installation |

## Nine Atomic PR Slices

| Slice | Branch | Owned plan tasks | Merge gate |
| --- | --- | --- | --- |
| 1 | `fix/live-admission-account-guards` | Live safety Tasks 1-2 | admission source/exposure envelope, wallet/cap tests green |
| 2 | `fix/live-order-quiesce` | Live safety Tasks 3-7 | FAK/partial fills, venue truth, quiesce/API/signal tests green; direct live resume removed |
| 3 | `feat/polymarket-v2-adapter` | V2 Task 1 | exact SDK 0.6.0, V2/Gamma identity, list-position tests green |
| 4 | `feat/polymarket-settlement-lifecycle` | V2 Tasks 2-5 | settlement/restore/generated-contract/reconcile tests green |
| 5 | `feat/canonical-live-promotion` | V2 Task 6 | generic resume blocked; signed single-use promotion/recovery tests green |
| 6 | `feat/horizon-safe-event-research` | Horizon-safe research Tasks 1-6 | full portable fixture evidence chain and workflow gates green |
| 7 | `feat/polymarket-research-orchestrator` | Research Orchestrator Tasks 1-5 | isolated no-tool child, PostgreSQL queue/action journal tests green |
| 8 | `ci/dual-host-release-bundles` | Dual-host packaging Tasks 1-9 | mutually exclusive archive, rollback, recovery/promotion workflow tests green |
| 9 | `chore/local-trading-readiness-review` | this index's final review only | whole-repository matrix and evidence record green |

## Dependency Graph

```text
live execution safety
  -> Polymarket V2 adapter
  -> confirmed settlement/redeem reconciliation
  -> trade bundle and live checklist

horizon-safe research
  -> Research Orchestrator profile
  -> research bundle and hosted artifact workflow

live promotion contract + canonical V2 store + recorded parity
  -> protected human canary workflow

all four implementation programs
  -> dual-host packaging
  -> final local readiness evidence
```

Land slices in numeric order. Live and horizon plans both touch strategy config contracts, so they are not declared disjoint: Slice 6 rebases after Slices 1-5 and treats the live plan as owner of the committed live TOML/manifests. Slice 7 starts after the canonical horizon/profile contract. Slice 8 packages the merged binaries/configs. Slice 9 starts from updated `main` after all eight implementation PRs merge.

## Cross-Program Contracts

### Live state

- Risk-increasing intent requires live desired `running`, observed `running`, and a fresh authenticated venue probe.
- Paused/degraded/draining/recovering live state permits only risk reduction, cancellation, and reconciliation.
- A missing venue order ID remains unknown; no local-only cancel may fabricate a terminal state.
- Emergency stop succeeds only when venue/canonical orders and unexplained positions are all zero.

### Account state

- Paper account IDs use `paper:`.
- Live account IDs are normalized non-zero EVM funder/proxy wallet addresses.
- One daemon controls one live wallet and one positive shared account cap.
- The first later canary cap is USD 5; a strategy stake above that cap is invalid.

### Settlement state

- Resolution and redemption are distinct.
- Resolution alone never releases position quantity or exposure.
- A confirmed redeem transaction/activity releases quantity exactly once.
- Payout values `0`, `0.5`, and `1` are represented without synthetic SELL fills.

### Dataset state

- One event-root artifact carries one horizon contract and one market window.
- The research snapshot path is the only PostgreSQL-to-portable exporter; hosted dataset/training workflows consume hashed artifacts without database access.
- PM5D uses `market_window_secs=300`; PM15D uses `900`; PM1H (`3600`) fails closed.
- PM5D and PM15D have separate configs, manifests, recordings, models/scorers, replay artifacts, and research trace identity.

### Agent state

- Sidecar queue JSONL is transport, not promotion truth.
- Production trade-to-research requests use the PostgreSQL lease queue and daemon-sanitized paper context; file JSONL is local-only.
- Research OS, workflow run URLs, immutable artifacts, and typed handoffs are the evidence source; the sanitized bounded snapshot array is embedded directly in the no-tool child's stdin.
- Any order, cancel, redeem, deployment mutation, live-state mutation, deploy workflow, or unrestricted file mutation receipt blocks the Agent run.

### Release state

- Research and trade bundles are allowlist-built and mutually exclusive.
- Trade installation uses `/opt/ploy/releases/<sha>` and an atomic `/opt/ploy/current` symlink.
- Research installation includes the compiled no-tool Sidecar under its own Agent env; trade installation is owned only by `release-aliyun.yml` and consumes mandatory `platform-live.env` with dotenv disabled.
- A deployable SHA must equal `origin/main` and have a successful required Test run for the same SHA.
- Remote install scripts contain no `cargo` or `rustc` invocation and no long-lived secret material.
- Research-host recording export, trade-host release-pinned replay/dry-run snapshot, and hosted parity are joined only through exact retained artifact IDs and recomputed config/recording/report/release/runner hashes; the replay/parity workflows contain no Tango execution path.
- Dry-run parity computes the only allowed live-config hash; a protected review-only PR materializes it, and live canary additionally requires a fresh successful RDS PITR restore artifact.

## Specification Coverage Matrix

| Approved specification requirement | Owning executable tasks |
| --- | --- |
| Live order policy, partial fills, FAK, canonical cancel | Live Tasks 3-5 |
| Emergency quiesce, deadlines, direct-resume removal, canonical signed parity/human gate, deterministic live config and RDS proof | Live Tasks 3 and 5-7; V2 Task 6; Agent Task 2; Packaging Tasks 4, 8, and 9 |
| Risk-effect admission, worker/operator source, stale health | Live Task 1; Live Task 4 resume gate |
| Normalized wallet, paper namespace, one wallet/cap, USD 5 template | Live Task 2 |
| Unknown submit/cancel/order/position and reconcile-before-retry | Live Tasks 3-4; V2 Task 1 position loader |
| Official SDK V2, geoblock, protocol/market identity | V2 Task 1 |
| Official resolution payout, failed/retryable partial redemption, confirmed payout 0/0.5/1, restore | V2 Tasks 2-5 |
| PostgreSQL canonical production deployment/trading/audit state, atomic versioning, JSON cache demotion | V2 Task 3; Packaging Tasks 6 and 8 |
| PM5D/PM15D contract, PM1H rejection, no future leakage | Horizon Tasks 1 and 3-4 |
| PostgreSQL portable exporter, hosted producer, and one-fixture coverage-to-walk-forward chain | Horizon Tasks 2-5; Agent Task 2 export action |
| Feature governance, fixed baseline, library-owned model-family/search, executable replay/parity metrics | Horizon Tasks 3-6 |
| Research-only Agent, stdin evidence, no child tools/subagents/plugins/live mutation, deterministic horizon actions | Agent Tasks 1-4 |
| Cross-host PostgreSQL request lease, crash-safe workflow/issue dispatch, immutable decision/prior artifacts, exact recording/replay/dry-run artifact chain, deterministic config-PR boundary | Agent Tasks 2-5; Horizon Task 5; Packaging Tasks 2, 4, 6, and 9 |
| Research/trade separation, Sidecar unit, exact env/units/watchdog, immutable code and unit rollback | Packaging Tasks 1-6 |
| Main/same-SHA provenance, host identity, migrations/RDS restore proof, no host Rust build | Packaging Tasks 4 and 7-9 |
| Nine atomic branches/PRs and whole-repository readiness evidence | This index, Slices 1-9 |

Fail-closed matrix mapping:

- unhealthy/stale venue, degraded live, or missing identity/cap: Live Tasks 1-4;
- unknown submit/cancel, partial venue error, or shutdown deadline: Live Tasks 3-6;
- resolution without confirmation, failed/reverted/missing receipt, external quantity: V2 Tasks 2-5;
- canonical database unavailable/version-conflicted/audit-failed or stale JSON cache: V2 Task 3;
- mixed/unsupported horizon, missing governed artifact, future label leakage: Horizon Tasks 1-5;
- non-allowlisted Agent receipt, action ambiguity, mismatched horizon: Agent Tasks 1-3;
- role/secret/provenance/migration/rollback failure: Packaging Tasks 1-9.

## Integration Sequence

### Integration Tasks 1-2: Land the two live-safety slices

Implement Live Tasks 1-2 on Slice 1, merge, rebase Slice 2, then implement Live Tasks 3-7. After each slice run its focused tests; after Slice 2 run:

```bash
rtk cargo test --locked \
  -p ploy-connectivity \
  -p ploy-platform \
  -p ploy-platform-runtime \
  -p ploy-daemon-host \
  -p ploy-operator-contracts \
  -p ploy-control-client \
  -p ploy-strategy-runtime \
  -p ployctl \
  -p new-ployd
rtk pytest tests/test_live_promotion_gate.py tests/test_strategy_config_contracts.py -q
```

Expected result: admission source and worst-case exposure are enforced; FAK partial fills are persisted; unknown venue state blocks resume; emergency quiesce is audited; live remains paused.

### Integration Tasks 3-5: Land V2, settlement, then canonical live promotion

Implement only V2 Task 1 on Slice 3. After it merges, implement V2 Tasks 2-5 on Slice 4. Rebase Slice 5 from updated `main` and implement only V2 Task 6 so live promotion remains one reviewable security feature. Final evidence:

```bash
rtk cargo test --locked \
  -p ploy-trading \
  -p ploy-connectivity \
  -p ploy-operator-contracts \
  -p ploy-platform-runtime \
  -p ploy-daemon-host \
  -p ployctl
rtk cargo check --locked -p new-ployd
npm run contracts:check --prefix ploy-frontend
npm run contracts:check --prefix ploy-sidecar
scripts/check_v2_claim_redeem_gate.sh
```

Expected result: CLOB V2 is active; CLOB/Gamma identity and collateral are corroborated; official resolution proves payout independently from redemption; partial confirmations are idempotent; PostgreSQL is canonical while JSON is cache-only; generic live resume is disabled and the signed single-use promotion API is unreachable without the later protected workflow.

### Integration Task 6: Land horizon-safe research

Follow the horizon plan from updated `main`:

```bash
rtk cargo test --locked -p ploy-research --features polars-export dataset --lib
rtk cargo test --locked -p ploy-research --features polars-export --test event_root_portable_input
rtk pytest tests/test_event_ml_rolling_workflow.py \
  tests/test_build_runtime_candidate_strategy_replay.py \
  tests/test_strategy_config_contracts.py
```

Expected result: PostgreSQL has one explicit hashed-artifact export path; one 450-event portable fixture reaches coverage, attribution, feature governance, fixed baseline, model-family, bounded search, and at least three genuine library-built walk-forward windows; PM5D/PM15D cannot cross and PM1H is rejected.

### Integration Task 7: Land Research Orchestrator

Follow the Agent plan from updated `main`:

```bash
npm run contracts:check --prefix ploy-sidecar
npm run test --prefix ploy-sidecar
npm run build --prefix ploy-sidecar
npm run contracts:check --prefix ploy-frontend
npm run build --prefix ploy-frontend
```

Expected result: the model child has an empty home and no shell/filesystem/subagent/plugin/browser/MCP/search surface; the research host claims daemon-sanitized requests through a least-privilege PostgreSQL lease queue without shared files or a daemon token; the deterministic parent embeds validated typed evidence snapshots in stdin; actions from PostgreSQL export through a reviewable dry-run config PR are explicit, horizon-bound, and journaled before dispatch; ambiguous outcomes never redispatch.

### Integration Task 8: Land dual-host packaging

Follow the packaging plan after Slices 1-7:

```bash
bash scripts/tests/test_package_deploy_bundle.sh
bash scripts/tests/test_activate_release.sh
bash scripts/tests/test_verify_deploy_role.sh
rtk cargo test --locked -p ploy --test workflow_security
rtk cargo test --locked -p ploy --test platform_release_workflow
```

Expected result: research has the ops-only collector/research plane plus isolated PostgreSQL-queue Sidecar service, trade has the execution plane and exact service/timers/live gate, the protected canary workflow requires exact main parity/HMAC/human review, rollback restores both release files and unit definitions, and no workflow is dispatched.

## Final Local Readiness Review

After Slices 1-8 are merged, create Slice 9 directly from updated `main`; do not cherry-pick commits already present on main. Run:

```bash
rtk cargo fmt --all -- --check
rtk cargo test --locked --workspace
rtk cargo clippy --locked --workspace --all-targets -- -D warnings

npm run contracts:check --prefix ploy-sidecar
npm run test --prefix ploy-sidecar
npm run build --prefix ploy-sidecar
npm audit --omit=dev --audit-level=moderate --prefix ploy-sidecar

npm run contracts:check --prefix ploy-frontend
npm run lint --prefix ploy-frontend
npm run build --prefix ploy-frontend
npm audit --omit=dev --audit-level=moderate --prefix ploy-frontend

rtk pytest \
  tests/test_event_ml_rolling_workflow.py \
  tests/test_build_runtime_candidate_strategy_replay.py \
  tests/test_live_promotion_gate.py \
  tests/test_runtime_market_data_boundary.py \
  tests/test_persist_research_trace_contract.py \
  tests/test_canonical_runtime_store_contracts.py \
  tests/test_polymarket_v2_indexer_contracts.py

bash scripts/tests/test_package_deploy_bundle.sh
bash scripts/tests/test_activate_release.sh
bash scripts/tests/test_verify_deploy_role.sh
bash scripts/tests/test_ploy_platform_watchdog.sh
python3 -m py_compile scripts/validate_live_promotion_gate.py
if command -v actionlint >/dev/null 2>&1; then
  actionlint
else
  ruby -e 'require "yaml"; Dir[".github/workflows/*.yml"].sort.each { |path| YAML.load_file(path) }'
fi
rtk git diff --check
rtk git status --short
```

Expected result:

- every command exits zero;
- no local PostgreSQL process or remote host is required;
- the only known warnings are documented pre-existing warnings, not new clippy failures;
- `rtk git status --short` is empty after the final evidence commit;
- live trading is still paused and unapproved.

## Deferred Runtime Gates

The following are intentionally not satisfied locally and therefore continue to block live trading:

- real research/data host and trade host inventory;
- protected environment variables, known-host fingerprints, RAM roles, and secret materialization;
- RDS TLS/PITR/restore evidence and OSS retention evidence;
- retained real PM5D/PM15D event-root data;
- fresh factor attribution, three or more genuine walk-forward windows, executable replay, paper observation, and recorded parity;
- wallet balance, allowance, funder/proxy identity, geoblock, V2 authenticated probe, cancel-all drill, service restart, backup/restore, and confirmed settlement/redeem evidence;
- explicit human approval for the first USD 5 canary.

Passing all local plans means the repository is ready to enter those gates. It is not a profitability claim and not authorization to trade.
