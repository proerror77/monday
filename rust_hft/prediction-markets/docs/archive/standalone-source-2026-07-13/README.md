# Ploy

A high-performance Polymarket trading bot focused on crypto and sports prediction markets. Ships with a terminal dashboard, multi-agent coordinator, AI-assisted analysis, and optional reinforcement learning.

## Trading Platform Workspace

The workspace now also includes the platform-refactor spine:

- `new-ployd`: next-generation daemon entrypoint
- `ployctl`: operator client entrypoint
- `ploytui`: thin terminal operator console
- `new-ploy-runner`: next-generation strategy runner entrypoint
- `crates/ploy-daemon-host`: daemon host/bootstrap crate
- `crates/ploy-runner-host`: runner CLI host crate
- `crates/ploy-control-client`: shared operator client transport
- `crates/ploy-market-data`: collector/feed/scanner/discovery infrastructure
- `crates/ploy-platform-runtime`: control-plane runtime ownership boundary
- `crates/ploy-strategy-runtime`: strategy runtime ownership boundary
- `crates/ploy-platform`: control-plane core
- `crates/ploy-trading`: canonical trading lifecycle
- `crates/ploy-deployments`: worker protocol and supervisor
- `crates/ploy-operator-contracts`: shared DTO and event contracts
- `crates/ploy-strategy-bundles`: signal-to-intent runtime
- `crates/ploy-research`: replay and backtesting consumer of trading models

Current smoke path:

```bash
cargo run -p new-ployd
cargo run -p ployctl -- system status
cargo run -p ployctl -- system audit
cargo run -p ployctl -- trading status
cargo run -p ployctl -- deployments apply config/deployments/example.paper.json
cargo run -p ployctl -- deployments list
cargo run -p ployctl -- deployments inspect example.paper
cargo run -p ployctl -- trading cancel example.live <order-id>
cargo run -p ploytui
cargo run -p new-ploy-runner --features full -- run --config config/strategies/02-pm5d.unified.toml --dry-run
# realtime operator stream
curl -N http://127.0.0.1:8081/api/events/stream
rtk cargo test --test platform_smoke -- --nocapture
```

## Deployment Workflow

The default operating model is now:

1. `ployd` is the only long-running platform daemon
2. strategy configs live in `config/strategies/*.toml`
3. deployment manifests live in `config/deployments/*.json`
4. operators use `ployctl deployments apply|inspect|pause|resume|stop`
5. `ployd` starts and supervises `ploy-runner --config ...` workers for each deployment

Do not create new per-strategy systemd units for normal operation.
Use deployment manifests instead.

### Bundle Resolution

Deployment manifests use `bundle_id` to select a strategy config:

- if `bundle_id` ends with `.toml` or contains a path separator, it is treated as a config path
- otherwise it resolves to `config/strategies/<bundle_id>.toml`

Example:

```json
{
  "deployment_id": "pm5d.v3.dryrun",
  "bundle_id": "02-pm5d.v3-dryrun",
  "account_id": "acct-pm5d-dryrun",
  "max_gross_exposure": "5.00",
  "runtime_mode": "paper",
  "desired_state": "running"
}
```

This resolves to:

```text
config/strategies/02-pm5d.v3-dryrun.toml
```

### PM5D Dry-Run Deployments

The repo now includes three PM5D deployment manifests:

- `config/deployments/pm5d.v2.dryrun.json`
- `config/deployments/pm5d.v3.dryrun.json`
- `config/deployments/pm5d.v4.dryrun.json`

Each one targets:

- `02-pm5d.v2-dryrun.toml`
- `02-pm5d.v3-dryrun.toml`
- `02-pm5d.v4-dryrun.toml`

Apply them with:

```bash
cargo run -p ployctl -- deployments apply config/deployments/pm5d.v2.dryrun.json
cargo run -p ployctl -- deployments apply config/deployments/pm5d.v3.dryrun.json
cargo run -p ployctl -- deployments apply config/deployments/pm5d.v4.dryrun.json
```

Inspect and control them with:

```bash
cargo run -p ployctl -- deployments list
cargo run -p ployctl -- deployments inspect pm5d.v3.dryrun
cargo run -p ployctl -- deployments pause pm5d.v3.dryrun
cargo run -p ployctl -- deployments resume pm5d.v3.dryrun
cargo run -p ployctl -- deployments stop pm5d.v3.dryrun
```

Remote host equivalent:

```bash
/opt/ploy/bin/ployctl deployments list
/opt/ploy/bin/ployctl deployments inspect pm5d.v3.dryrun
/opt/ploy/bin/ployctl deployments pause pm5d.v3.dryrun
/opt/ploy/bin/ployctl deployments resume pm5d.v3.dryrun
/opt/ploy/bin/ployctl deployments stop pm5d.v3.dryrun
```

### Direct Runner Usage

Use `new-ploy-runner -- run --config ...` for:

- local backtests
- local dry-run debugging
- full live/dry-run mode requires the explicit Cargo feature:
  `cargo run -p new-ploy-runner --features full -- run --config ...`
- one-off manual strategy inspection

Example:

```bash
cargo run -p new-ploy-runner --features full -- run --config config/strategies/02-pm5d.v4-dryrun.toml --dry-run
```

Use deployment manifests when you want the platform daemon to own lifecycle and supervision.

Required control-plane auth:

- Set `PLOY_ADMIN_TOKEN`, `PLOY_API_ADMIN_TOKEN`, or the compatibility alias `PLOY_API_KEY` before booting `ployd`. Protected API routes fail closed when no credential is configured; only public health and auth-session routes remain available.
- Set `PLOY_OPERATOR_TOKEN` or `PLOY_API_OPERATOR_TOKEN` if you want a write-capable operator credential for deployment controls, intent ingress, and order cancel/replace without granting full admin access.
- Set `PLOY_SIDECAR_AUTH_TOKEN` if you want a read-only agent/sidecar credential for system status, deployment snapshots, trading snapshots, and the SSE event stream.
- Set `PLOY_API_AUTH_COOKIE_SECRET` as well if you want browser auth cookies to stay valid across daemon restarts or multiple instances.
- Set `PLOY_REQUEST_RATE_LIMIT_PER_MINUTE` if you need to tighten or relax the daemon HTTP request throttle. `0` disables the limiter.
- Set `PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS` and `PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS` if you want to tune venue outage retry backoff for live fill reconciliation.
- `ployctl` will automatically reuse `PLOY_ADMIN_TOKEN`, `PLOY_API_ADMIN_TOKEN`, `PLOY_API_KEY`, `PLOY_OPERATOR_TOKEN`, `PLOY_API_OPERATOR_TOKEN`, or `PLOY_SIDECAR_AUTH_TOKEN` for authenticated requests, in that order.
- Browser operator surfaces authenticate through `/auth/login`, which sets an `HttpOnly` same-site signed session cookie so the frontend event stream can stay authenticated without storing the raw admin token in browser storage. Login returns `admin_auth_not_configured` until an admin credential is configured.
- `ployd` appends authenticated control-plane activity to `run/platform/audit-log.jsonl`, and `ployctl system audit` reads that stream back through the daemon API.
- `ployctl system status` now surfaces live reconcile failure count, next retry time, and the last reconcile error so venue outages are visible without digging through logs.
- Current access bands are `public`, `read_only`, `operator`, and `admin`. `sidecar` credentials stop at `read_only`; `operator` credentials can mutate deployments and trading controls; only `admin` can access audit logs and browser-login flows.

Runbooks:

- [`docs/runbooks/platform-startup.md`](docs/runbooks/platform-startup.md)
- [`docs/runbooks/platform-deploy.md`](docs/runbooks/platform-deploy.md)
- [`docs/runbooks/live-deployment-checklist.md`](docs/runbooks/live-deployment-checklist.md)
- [`docs/runbooks/live-dry-run-drill.md`](docs/runbooks/live-dry-run-drill.md)

Default release workflow:

- `.github/workflows/release-platform.yml`
- Build-only verification: dispatch `release-platform.yml` with `deploy=false`
  to build, package, checksum, and upload the bundle artifact without restarting
  the remote service.

Remote live-host acceptance path:

- deploy with [`docs/runbooks/platform-deploy.md`](docs/runbooks/platform-deploy.md)
- validate the host with [`docs/runbooks/live-deployment-checklist.md`](docs/runbooks/live-deployment-checklist.md)
- run the dry-run acceptance drill from [`docs/runbooks/live-dry-run-drill.md`](docs/runbooks/live-dry-run-drill.md)
- only then move to manual live go/no-go review

Compatibility note:

- `new-ployd`, `new-ploy-runner`, `ployctl`, and `ploytui` are the workspace entrypoints for the trading platform spine.
- The old root runtime tree has been retired from the compiled workspace.
- Remaining `ploy ...` examples below are historical reference only and are not runnable entrypoints in this branch.
- Current sports scope in this branch is `discovery + live-state data capture + replay/backtest support`.
- Sports live execution and market making remain out of scope until lifecycle safeguards are added for game-start orderbook resets.

## Features

- **Two runtime domains** -- Crypto (BTC/ETH/SOL UP/DOWN), Sports (NBA/NFL live odds)
- **Multiple strategies** -- Momentum, Split-Arb, Event-Edge mispricing scanner, NBA Q3-Q4 comeback, market making
- **Coordinator-managed runtime** -- Canonical strategy runtime with central order queue, governance gate, risk gate, and position aggregation
- **Event registry** -- Automated DISCOVER -> RESEARCH -> MONITOR -> TRADE pipeline for new markets
- **TUI dashboard** -- Ratatui-based terminal UI with live positions, quotes, Binance price feed, and trade log
- **Claude AI agent** -- Advisory, autonomous, and chat modes for market analysis and trade execution
- **Reinforcement learning** -- PPO training, lead-lag strategies, ONNX inference (optional `rl` / `onnx` feature flags)
- **Persistence** -- PostgreSQL event store, checkpoints, dead-letter queue, and crash recovery
- **Risk management** -- Position limits, circuit breaker, daily loss limit, slippage protection, emergency stop

## Architecture (Layered Live Runtime)

Production runtime now uses a 4-plane model:

- **Strategy Plane**: canonical `Strategy` implementations decide direction, timing, sizing, and state transitions.
- **Capital Governance Plane**: OpenClaw-style governance agents manage budget, pause/resume, throttle, and deployment-scoped policy.
- **Execution Plane**: the coordinator is the only live order ingress (`StrategyIntent -> Governance/Risk Gate -> Queue -> Executor`), plus audit trail and recovery.
- **Control Plane**: deployment/config projection, lifecycle control, health, observability, and rollout/shutdown wiring.

Key rule: OpenClaw does not sit in the synchronous per-order decision path for HFT. It governs boundaries; strategies decide entries/exits inside those boundaries.

Live strategies now start only through the canonical managed `Strategy` runtime.

Collector / backfill command routing is documented in [docs/COLLECTOR_RUNBOOK.md](docs/COLLECTOR_RUNBOOK.md).

New live strategies should implement the canonical `Strategy` contract.
`TradingAgent` / `DomainAgent` are retired and only remain in historical design docs.
The default workspace control plane is:
- `GET /api/system/status`
- `GET /api/audit/logs`
- `GET /api/deployments`
- `GET /api/deployments/:id`
- `POST /api/deployments/:id/control`
- `GET /api/trading/state`
- `GET /reports/strategy`
- `POST /api/deployments/:id/intents`
- `POST /api/deployments/:id/orders/:order_id/cancel`
- `POST /api/deployments/:id/orders/:order_id/replace`
- `GET /api/events/stream`

The older `strategies/control`, `strategy-evaluations`, `/api/sidecar/*`, and
`ploy rpc` surfaces are historical reference only in this branch and are no
longer part of the default operator path.

Governance agents live under `crate::agents`; canonical live strategy runtime ownership lives under `crate::strategy`, `crate::coordinator`, and `crate::plugins`.

## Prerequisites

- **Rust** 1.75+ (2021 edition)
- **PostgreSQL** 15+ with an active database for event store, checkpoints, and strategy state
- **Polymarket account** with API credentials and a funded wallet on Polygon
- (Optional) `ANTHROPIC_API_KEY` for Claude AI agent commands
- (Optional) `GROK_API_KEY` for Grok-based sports analysis

## Installation

```bash
# Clone and build
git clone https://github.com/proerror77/ploy.git
cd ploy
cargo build --release

# Build with optional feature flags
cargo build --release --features rl        # Reinforcement learning (burn + ndarray)
cargo build --release --features onnx      # ONNX model inference (tract)
cargo build --release --features analysis  # DuckDB parquet analysis
```

Run database migrations before first use:

```bash
export DATABASE_URL="postgres://localhost/ploy"
sqlx migrate run
```

## Configuration

### Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `POLYMARKET_PRIVATE_KEY` | Yes | Ethereum private key for order signing |
| `POLYMARKET_API_KEY` | Yes | Polymarket CLOB API key |
| `POLYMARKET_API_SECRET` | Yes | Polymarket CLOB API secret |
| `POLYMARKET_PASSPHRASE` | Yes | Polymarket CLOB passphrase |
| `POLYMARKET_FUNDER` | No | Proxy/Magic wallet address |
| `RELAYER_API_KEY` | No | Polymarket Relayer API key for gasless account operations |
| `RELAYER_API_KEY_ADDRESS` | No | Address that owns the relayer key |
| `DATABASE_URL` | Yes | PostgreSQL connection string (overrides config) |
| `ANTHROPIC_API_KEY` | No | Required for `agent` and AI-powered commands |
| `ANTHROPIC_BASE_URL` | No | Optional Anthropic-compatible base URL (examples: MiniMax `https://api.minimaxi.com/anthropic` or `https://api.minimax.io/anthropic`) |
| `ANTHROPIC_DEFAULT_OPUS_MODEL` | No | Optional alias override (example: map `opus` → `MiniMax-M2.5`) |
| `ANTHROPIC_CUSTOM_HEADERS` | No | Optional custom headers in newline-separated `Header: Value` format (example: `Authorization: Bearer <key>`) |
| `GROK_API_KEY` | No | Required for Grok-based sports analysis |
| `PLOY_RISK__CRYPTO_ALLOCATION_PCT` | No | Domain capital split (0..1), used to derive crypto exposure cap |
| `PLOY_RISK__SPORTS_ALLOCATION_PCT` | No | Domain capital split (0..1), used to derive sports exposure cap |
| `PLOY_RISK__CRYPTO_MAX_EXPOSURE_USD` | No | Hard crypto domain exposure cap (overrides pct-derived cap) |
| `PLOY_RISK__SPORTS_MAX_EXPOSURE_USD` | No | Hard sports domain exposure cap (overrides pct-derived cap) |
| `PLOY_RISK__CRYPTO_DAILY_LOSS_LIMIT_USD` | No | Hard crypto domain daily loss stop |
| `PLOY_RISK__SPORTS_DAILY_LOSS_LIMIT_USD` | No | Hard sports domain daily loss stop |
| `PLOY_RISK__MAX_DRAWDOWN_USD` | No | Hard drawdown stop (runtime cumulative realized curve) |
| `PLOY_ACCOUNT_ID` | No | Runtime account scope identifier (default `default`) |
| `PLOY_API_PORT` | No | API listen port for `new-ployd` and operator clients (default `8081`) |
| `PLOY_API_ADMIN_TOKEN` | No | Admin token for protected API routes, including operator terminal actions |
| `PLOY_ADMIN_TOKEN` | No | Dashboard-side fallback token name for operator polling/actions if `PLOY_API_ADMIN_TOKEN` is unset |
| `PLOY_DRY_RUN__ENABLED` | No | Force runtime dry-run mode (`true`/`false`) |
| `PLOY_DEPLOYMENTS_REQUIRE_EVIDENCE` | No | Require strategy evidence before enabling deployments (`true`/`false`) |
| `PLOY_DEPLOYMENTS_REQUIRED_STAGES` | No | Required evidence stages CSV (default `backtest,paper`) |
| `PLOY_DEPLOYMENTS_MAX_EVIDENCE_AGE_HOURS` | No | Max evidence staleness window in hours (default `168`) |
| `PLOY_ALLOW_DIRECT_LIVE` | No | Allow direct (non-Coordinator) live order paths. Not recommended. |
| `PLOY_ALLOW_DIRECT_STRATEGY_LIVE` | No | Archived compatibility flag for the retired single-binary runtime; not part of the workspace default path. |

### Config File

The default configuration lives in `config/default.toml`. Override the path with `--config` / `-c`.

| Section | Key examples |
|---------|-------------|
| `[market]` | `ws_url`, `rest_url`, `market_slug` |
| `[strategy]` | `shares`, `window_min`, `move_pct`, `sum_target`, `fee_buffer`, `slippage_buffer`, `profit_buffer` |
| `[execution]` | `order_timeout_ms`, `max_retries`, `max_spread_bps`, `poll_interval_ms` |
| `[risk]` | `max_single_exposure_usd`, `min_remaining_seconds`, `max_consecutive_failures`, `daily_loss_limit_usd`, `leg2_force_close_seconds` |
| `[database]` | `url`, `max_connections` |
| `[dry_run]` | `enabled` (defaults to `true`) |
| `[logging]` | `level`, `json` |
| `[event_edge_agent]` | `enabled`, `trade`, `interval_secs`, `min_edge`, `max_entry`, `shares`, `cooldown_secs`, `max_daily_spend_usd`, `titles` |
| `[nba_comeback]` | `enabled`, `min_edge`, `max_entry_price`, `shares`, `min_deficit`, `max_deficit`, `target_quarter`, `espn_poll_interval_secs` |

See the inline comments in `config/default.toml` for a full explanation of every field.

### Strategy Runtime Backtests

`new-ploy-runner --config config/strategies/*.toml` uses the unified strategy-runtime config in
`crates/ploy-strategy-bundles`.

- `[reference_data]`
  - `pyth_symbols = [...]` configures additive non-crypto reference capture.
  - `capture_sports_state = true|false` toggles live sports-state recording.
- `[backtest_data]`
  - `include_reference_prices = true|false` opts historical DB backtests into `reference_price_ticks`.
  - `include_sports_state = true|false` opts historical DB backtests into `sports_state_events`.
  - `reference_symbols = [...]` overrides the symbols loaded from `reference_price_ticks`; when omitted, the loader falls back to `[reference_data].pyth_symbols`.

Historical trust remains conservative in this phase:

- PM quotes stay pinned to the existing trusted source list in `clob_quote_ticks`.
- Official settlement truth remains `pm_token_settlements`.
- Reference prices and sports-state are additive observational inputs until a later trust-cutover phase blesses broader historical defaults.

## Usage

### Live Trading (Recommended)

Ploy uses a **Coordinator-only** live execution plane. For live orders, use the platform entry point:

Historical reference only: the old `ploy platform start ...` path has been retired from this branch. Use the `ployd` / `ployctl` workspace runbook instead.

Legacy commands that can place orders (example: `ploy run`, `ploy momentum`, `ploy split-arb`, `ploy crypto split-arb`, `ploy sports split-arb`, `ploy event-edge --trade`, `ploy agent --enable-trading`) are **blocked for live execution by default**.

If you need an explicit override (not recommended), set:

```bash
export PLOY_ALLOW_DIRECT_LIVE=true
```

### Global Flags

```
--dry-run  / -d    Override dry-run mode (no real orders)
--market   / -m    Override market slug from config
--config   / -c    Config file path (default: config/default.toml)
```

### Current Operator Commands

Use the workspace control-plane binaries as the active operator surface:

```bash
cargo run -p new-ployd
cargo run -p ployctl -- system status
cargo run -p ployctl -- deployments list
cargo run -p ployctl -- trading status
cargo run -p new-ploy-runner --features full -- run --config config/strategies/02-pm5d.unified.toml --dry-run
```

Older `ploy run`, `ploy serve`, `ploy dashboard`, `ploy search`, `ploy book`,
`ploy current`, `ploy watch`, `ploy account`, `ploy claim`, `ploy history`, and
`ploy ev` examples are archived compatibility references. Do not use them as
the default runtime, research, or deployment path for new work.

### Collector And Backfill

Current data work is split between CI-managed collectors and artifact-backed
research workflows. Treat older `ploy collect`, `ploy orderbook-history`, and
`ploy strategy backfill-*` references as archived compatibility paths.

- `.github/workflows/deploy-tango-1-1.yml` deploys the CI-built
  `/opt/ploy/bin/ploy-runner` and collector systemd units on `tango-1-1`.
- `/opt/ploy/bin/ploy-runner collect-*` commands are foreground operator
  diagnostics on the data host, not promotion evidence.
- `.github/workflows/research-snapshot.yml` creates retained sampled research
  artifacts.
- `factor-review-v2-hosted-artifact.yml`,
  `factor-walk-forward-v2-hosted-artifact.yml`, and
  `runtime-candidate-replay.yml` produce attribution, walk-forward, and runtime
  replay evidence from retained artifacts.

See [docs/COLLECTOR_RUNBOOK.md](docs/COLLECTOR_RUNBOOK.md) for examples and workflow guidance.

### Strategies

Strategy work should flow through reviewed strategy configs,
`new-ploy-runner`, and the artifact-backed research workflows. Treat older
direct strategy commands such as `ploy trade`, `ploy momentum`,
`ploy split-arb`, `ploy market-make`, `ploy scan`, `ploy analyze`, and
`ploy paper` as archived compatibility examples, not deployable evidence.

Post-settlement claiming is not handled by an in-process Ploy claimer daemon on
the current workspace path. Use Polymarket account auto-claim for normal
settlement, and keep only the official Relayer API credentials in the runtime
secret store for gasless account operations:

```bash
export RELAYER_API_KEY=xxx
export RELAYER_API_KEY_ADDRESS=0x...
```

Do not reintroduce `CLAIMER_*` or `POLY_BUILDER_*` env vars unless a new
account-ops capability is added and verified. The retired `ploy-claimer` crate
and live-runner daemon path must remain out of the default runtime.

Example: split 100u capital into crypto/sports 50/50 and hard-stop each domain at 45u daily loss:

```bash
export PLOY_RISK__CRYPTO_ALLOCATION_PCT=0.5
export PLOY_RISK__SPORTS_ALLOCATION_PCT=0.5
export PLOY_RISK__CRYPTO_DAILY_LOSS_LIMIT_USD=45
export PLOY_RISK__SPORTS_DAILY_LOSS_LIMIT_USD=45
```

### Archived Event-Edge And Agent CLI

The old `ploy event-edge ...` and `ploy agent ...` commands are archived
compatibility examples. Do not use them as promotion evidence, dry-run handoff,
or live execution paths. New strategy research should create artifact-backed
evidence through the workflows in
[docs/runbooks/strategy-research-cicd.md](docs/runbooks/strategy-research-cicd.md).

### Domain: Crypto

```bash
# Archived compatibility examples only:
# ploy crypto split-arb --coins SOL,ETH,BTC --dry-run
# ploy crypto monitor --coins SOL,ETH
```

### Domain: Sports

```bash
# Archived compatibility examples only:
# ploy sports split-arb --leagues NBA --dry-run
# ploy sports monitor --leagues NBA
# ploy sports draftkings --sport nba --min-edge 5
# ploy sports analyze --team1 LAL --team2 BOS
# ploy sports polymarket --league nba --live
# ploy sports chain --team1 LAL --team2 BOS
# ploy sports live-scan --sport nba --min-edge 3
```

### Strategy Management

```bash
cargo run -p ployctl -- deployments list
cargo run -p ployctl -- deployments inspect <deployment-id>
cargo run -p ployctl -- deployments pause <deployment-id>
cargo run -p ployctl -- deployments resume <deployment-id>
cargo run -p ployctl -- deployments stop <deployment-id>
```

### Archived Single-Binary Platform CLI

The old `ploy platform start ...` commands are no longer runnable in this branch.
Use [`docs/runbooks/platform-startup.md`](docs/runbooks/platform-startup.md) for the workspace daemon/client flow.

### Operator Terminal

Use `ployctl` for scripted operator actions and `ploytui` for the terminal
console. Both are clients of the `new-ployd` control plane and do not introduce
a direct live order path.

Start the control plane and terminal console:

```bash
export PLOY_API_ADMIN_TOKEN=change-me
cargo run -p new-ployd
cargo run -p ploytui
```

Behavior:

- `ployctl` and `ploytui` connect to `http://127.0.0.1:${PLOY_API_PORT:-8081}`.
- operator requests use `x-ploy-admin-token`.
- all operator actions flow through the workspace control plane.

If the admin token is missing, protected operator requests fail closed.
See [docs/runbooks/operator-terminal.md](docs/runbooks/operator-terminal.md) for the minimal operator flow.

Deployment matrix entries support runtime scope controls:

```json
{
  "id": "crypto-momentum-5m",
  "strategy": "momentum",
  "domain": "Crypto",
  "enabled": true,
  "account_ids": ["acct-main", "acct-paper"],
  "execution_mode": "any"
}
```

- `account_ids`: optional allow-list. Empty means all accounts.
- `execution_mode`: `any` | `dry_run_only` | `live_only`.

The older `ploy rpc` and sidecar evidence endpoints are archived compatibility
references only. They are not part of the default workspace operator surface on
this branch.

### RL Commands (requires `--features rl`)

```bash
ploy rl train --episodes 1000 --series 10423        # Train RL model
ploy rl run --model ./models/best --series 10423     # Live trading with RL
ploy rl eval --model ./models/best --data test.csv   # Evaluate model
ploy rl info --model ./models/best                   # Inspect model stats
ploy rl export --model ./models/best -o model.onnx   # Export for deployment
ploy rl backtest --episodes 100                      # Backtest on sample data
ploy rl lead-lag --episodes 1000 --symbol BTCUSDT    # Train lead-lag RL
ploy rl lead-lag-live --symbol BTCUSDT --market btc-price-series-15m  # Live lead-lag
ploy rl agent --symbol BTCUSDT --market btc-price-series-15m \
    --up-token <id> --down-token <id>                # Full RL agent integration
```

### Data Collection

```bash
gh workflow run research-snapshot.yml \
  -f git_ref=main \
  -f start_date=2026-05-16 \
  -f end_date=2026-05-18 \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT

/opt/ploy/bin/ploy-runner check-db --db-url "$PLOY_DATABASE__URL"
```

## Architecture

Ploy is organized around a multi-domain platform where each prediction market category (currently crypto and sports/NBA) has a dedicated trading agent. The agents submit orders through a central coordinator that applies risk checks, queues orders, and dispatches them to the Polymarket CLOB via authenticated API calls.

Strategies run independently and can be managed as daemons (start/stop/status). The event registry continuously discovers new markets, scores them for edge, and promotes them through a funnel from discovery to active trading. Persistence is handled by PostgreSQL with an event store for auditability, a checkpoint system for crash recovery, and a dead-letter queue for failed operations.

```
apps/
  new-ployd/     Next-generation daemon entrypoint
  new-ploy-runner/ Next-generation runner entrypoint
  ployctl/       Operator client entrypoint
  ploytui/       Thin terminal operator console
crates/
  ploy-daemon-host/ Daemon host/bootstrap crate
  ploy-runner-host/ Runner CLI host crate
  ploy-control-client/ Shared operator client transport
  ploy-market-data/ Collector/feed/scanner/discovery
  ploy-platform/ Control-plane core
  ploy-platform-runtime/ Runtime orchestration ownership
  ploy-trading/  Canonical trading lifecycle
  ploy-deployments/ Deployment worker protocol + supervisor
  ploy-operator-contracts/ Shared API and event contracts
  ploy-strategy-runtime/ Strategy runtime ownership
  ploy-strategy-bundles/ Signal-to-intent runtime
  ploy-research/ Replay and backtest consumers
ploy-frontend/   Web operator console
ploy-sidecar/    Agent-facing sidecar client
config/          Platform and deployment configuration
docs/            Design docs, runbooks, and migration notes
```

## Development

```bash
cargo run -p new-ployd               # Boot the platform daemon
cargo run -p ployctl -- system status
cargo run -p ployctl -- trading status
cargo run -p ployctl -- deployments apply config/deployments/example.paper.json
cargo run -p ployctl -- deployments list
cargo run -p ployctl -- deployments inspect example.paper
cargo run -p ploytui
cargo run -p new-ploy-runner --features full -- run --config config/strategies/02-pm5d.unified.toml --dry-run
curl -N http://127.0.0.1:8081/api/events/stream
rtk cargo check -p new-ployd         # Fast daemon type-check loop
rtk cargo check -p new-ploy-runner   # Fast runner type-check loop
rtk cargo check -p ployctl           # Fast client type-check loop
rtk cargo check -p ploytui           # Fast terminal console type-check loop
rtk cargo test --test platform_smoke platform_smoke_registers_and_starts_one_deployment -- --nocapture
cargo fmt --check                    # Check formatting
cargo clippy -- -D warnings          # Lint
rtk cargo build -p new-ployd         # Build the daemon binary
rtk cargo build -p new-ploy-runner --features full   # Build the full runner binary
rtk cargo build -p ployctl           # Build the operator client binary
rtk cargo build -p ploytui           # Build the terminal console binary
```

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for the contributor guide.

## License

MIT

## Disclaimer

This software is for educational and research purposes only. Trading on prediction markets carries substantial risk of financial loss. Always start with `dry_run.enabled = true` and verify behavior before committing real funds. Use at your own risk.
