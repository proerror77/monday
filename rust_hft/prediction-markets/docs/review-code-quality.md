# Code Quality & Architecture Review

**Date**: 2026-02-08
**Codebase**: Ploy Polymarket Trading Bot
**Scope**: 80,533 lines of Rust across 187 source files
**Reviewer**: Code Quality Agent

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Module Organization](#1-module-organization)
3. [Rust Idioms & Patterns](#2-rust-idioms--patterns)
4. [Type Safety](#3-type-safety)
5. [Code Duplication](#4-code-duplication--critical)
6. [Configuration System](#5-configuration-system)
7. [Testing Coverage](#6-testing-coverage)
8. [Dependency Health](#7-dependency-health)
9. [Dead Code & Technical Debt](#8-dead-code--technical-debt)
10. [Recommendations Summary](#recommendations-summary)

---

## Executive Summary

The Ploy codebase is a substantial Rust trading system (~80K lines) with strong
foundational architecture: well-defined domain types, proper error handling via
`thiserror`, `Decimal` for financial math, and `zeroize` for secrets. Test
coverage is excellent (105 files with `#[cfg(test)]` out of 187 source files).

However, the codebase suffers from **significant structural duplication** caused
by an incomplete migration from a "legacy" flat module layout to a new modular
`core/crypto/sports` architecture. This is the single largest quality issue and
the root cause of most other findings.

### Severity Summary

| Severity | Count | Description |
|----------|-------|-------------|
| Critical | 3 | Type name collisions, duplicate business logic |
| High | 5 | Monolithic main.rs, incomplete migration, config sprawl |
| Medium | 8 | Dead code, TODO debt, missing integration tests |
| Low | 4 | Style inconsistencies, comment language mixing |

---

## 1. Module Organization

### 1.1 Top-Level Structure

The `src/` directory contains 18 top-level modules:

```
src/
  adapters/     - External service clients (Polymarket, Binance, Postgres, Feishu)
  ai_clients/   - LLM/data client integrations (Grok, Claude, sports data)
  api/          - REST API server (feature-gated behind "api")
  cli/          - Command-line interface definitions
  collector/    - Market data collection (Binance depth, klines)
  config.rs     - Configuration structs and loading
  coordination/ - Circuit breaker, emergency stop, lifecycle, shutdown
  domain/       - Core domain types (Order, Market, State)
  error.rs      - Error types (PloyError, OrderError, RiskError)
  lib.rs        - Public API surface
  main.rs       - Entry point and command routing (5,285 lines!)
  persistence/  - Checkpoint, DLQ, event store
  platform/     - Unified order platform with risk gates
  rl/           - Reinforcement learning (optional, feature-gated)
  signing/      - Wallet, HMAC, nonce management
  strategy/     - Trading strategies (24,971 lines across 40+ files)
  coordination/ - Circuit breaker, lifecycle, emergency stop
  tui/          - Terminal UI (ratatui)
```

**Verdict**: The top-level decomposition is logical and follows a clean layered
architecture. The `domain/` layer has no upward dependencies, `adapters/` wraps
external services, and `strategy/` contains business logic. The `platform/`
module provides a clean abstraction for order execution across domains.

### 1.2 Critical Issue: Monolithic `main.rs` (5,285 lines) [HIGH]

**File**: `src/main.rs`

The entry point is a 5,285-line monolith containing:
- Command routing (`match &cli.command`)
- ~30 `run_*` async functions (one per CLI command)
- Inline business logic for each command mode
- Signal handling, logging initialization
- A `#[allow(dead_code)]` `graceful_shutdown` function at line 2358

This violates separation of concerns. Each `run_*` function should live in its
respective module (e.g., `run_momentum_mode` belongs in `strategy/momentum.rs`
or `cli/strategy.rs`).

**Recommendation**: Extract `run_*` functions into `cli/` submodules. The `main.rs`
should be <100 lines: parse CLI, init logging, dispatch to handler.

### 1.3 Strategy Module: Incomplete Migration [HIGH]

**File**: `src/strategy/mod.rs`

The strategy module explicitly labels two architectures:

```rust
// New modular architecture
pub mod core;      // Generic split arb engine
pub mod crypto;    // Crypto-specific
pub mod sports;    // Sports-specific

// Legacy modules (to be phased out)
pub mod split_arb;     // Duplicates core/split_engine.rs
pub mod momentum;      // 2,828 lines, largest single file
pub mod engine;        // 1,449 lines
// ... 25+ more legacy modules
```

The "legacy" modules are still the primary code path. The "new" modules
(`core/`, `crypto/`, `sports/`) duplicate types and logic but are not yet
fully integrated. This creates confusion about which implementation is
canonical.

**Evidence of confusion** in `src/strategy/mod.rs:198-204`:
```rust
pub use core::{
    ArbSide as CoreArbSide,           // Aliased to avoid collision
    ArbStats as CoreArbStats,
    SplitArbConfig as CoreSplitArbConfig,
    SplitArbEngine as CoreSplitArbEngine,
    // ...
};
```

**Recommendation**: Complete the migration or remove the new modules. Having
two parallel implementations is worse than having one imperfect one.

### 1.4 Dependency Direction

The dependency graph is generally clean:

```
domain (no deps) <-- strategy <-- adapters
                 <-- platform <-- adapters
                 <-- coordination (no adapter deps)
                 <-- persistence <-- adapters
```

One concern: `strategy/core/split_engine.rs:8` imports `crate::adapters::PolymarketClient`
directly, coupling the "generic" core engine to a specific adapter. The core
engine should accept a trait (e.g., `OrderSubmitter`) instead.

---

## 2. Rust Idioms & Patterns

### 2.1 Error Handling: Well-Structured [GOOD]

**File**: `src/error.rs`

The error hierarchy is well-designed:

```rust
PloyError          // Top-level, thiserror-derived, 25+ variants
  OrderError       // Specific to order execution (Clone-able)
  RiskError        // Specific to risk management (Clone-able)
```

Key strengths:
- `thiserror` for all error types (no manual `Display` impls)
- Structured error variants with named fields (e.g., `SlippageExceeded { limit, actual }`)
- `From` impls for `OrderError -> PloyError` and `RiskError -> PloyError`
- `Result<T>` type alias at `src/error.rs:118`

One concern: the `PloyError::Other(#[from] anyhow::Error)` variant at line 114
acts as an escape hatch that bypasses the typed error system. This should be
used sparingly; grep shows it is not heavily abused.

### 2.2 Unwrap Usage: Mostly Safe [GOOD]

Total `.unwrap()` calls: **115 across 30 files**

The majority (>90%) are in `#[cfg(test)]` blocks, which is acceptable. The
production unwraps are concentrated in:

- `src/strategy/nba_state_machine.rs` (19 occurrences) -- state machine
  transitions where the developer asserts invariants
- `src/platform/position.rs` (11 occurrences) -- position aggregation math
- `src/platform/queue.rs` (22 occurrences) -- priority queue operations

**Recommendation**: Replace production `unwrap()` calls with `expect("reason")`
to document the invariant, or use `?` where the function returns `Result`.

### 2.3 Clone Usage: Heavy but Justified [MEDIUM]

Total `.clone()` calls: **334 across 30 files**

The heaviest users:
- `src/strategy/adapters.rs` (44) -- wrapping legacy types for new trait
- `src/strategy/momentum.rs` (43) -- config/state cloning across async boundaries
- `src/strategy/split_arb.rs` (25) -- position state management

Most clones are on `Arc<RwLock<T>>` handles (cheap) or `String`/`Decimal`
(small). The `adapters.rs` clones are concerning because they clone entire
position/config structs repeatedly in the adapter layer.

**Recommendation**: Consider using `Arc<T>` for shared config structs passed
across async boundaries instead of cloning the full struct each time.

### 2.4 Async Trait Usage [GOOD]

The codebase uses `async-trait` consistently for async trait definitions:

- `Strategy` trait (`src/strategy/traits.rs:20`)
- `Checkpointable` trait (`src/persistence/checkpoint.rs`)

This is the correct approach for Rust 2021 edition. When the project upgrades
to Rust 2024 edition with native async traits, these can be migrated.

### 2.5 Trait Design [GOOD]

The `Strategy` trait at `src/strategy/traits.rs:20-56` is well-designed:

```rust
#[async_trait]
pub trait Strategy: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>>;
    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>>;
    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>>;
    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>>;
    // ...
}
```

The event-driven design (market updates, order updates, ticks) is clean and
composable. The `StrategyAction` return type allows strategies to express
intent without directly executing orders.

---

## 3. Type Safety

### 3.1 Financial Calculations: Decimal Everywhere [GOOD]

The codebase consistently uses `rust_decimal::Decimal` for all financial
calculations. No `f64` is used for prices, shares, or PnL. This prevents
floating-point rounding errors that could cause incorrect trading decisions.

Examples:
- `src/config.rs:124-125`: `sum_target: Decimal`, `fee_buffer: Decimal`
- `src/strategy/slippage.rs`: All slippage calculations use `Decimal`
- `src/platform/risk.rs:26`: `max_platform_exposure: Decimal`

### 3.2 Secret Management: Zeroize [GOOD]

**File**: `src/signing/wallet.rs:30-38`

```rust
let mut secure_key = key_hex.to_string();
let wallet = secure_key.parse::<LocalWallet>()?;
secure_key.zeroize(); // Key cleared from memory
```

The `Wallet` struct uses `zeroize` to clear private keys from memory after
initialization. The `PolymarketClient` at `src/adapters/polymarket_clob.rs`
also truncates API keys in log output.

### 3.3 State Machine: Validated Transitions [GOOD]

**File**: `src/domain/state.rs:37-68`

The `StrategyState` enum implements `can_transition_to()` with an exhaustive
match, preventing invalid state transitions at runtime. This is a strong
pattern for a trading system where state corruption could cause financial loss.

### 3.4 Missing Newtypes [MEDIUM]

Token IDs, condition IDs, event IDs, and order IDs are all represented as
bare `String` types throughout the codebase. This allows accidental mixing:

```rust
// These are all String -- easy to swap by accident
pub token_id: String,
pub condition_id: String,
pub event_id: String,
pub order_id: String,
```

**Recommendation**: Introduce newtypes for domain identifiers:
```rust
pub struct TokenId(String);
pub struct ConditionId(String);
pub struct EventId(String);
pub struct OrderId(String);
```

---

## 4. Code Duplication [CRITICAL]

This is the most significant quality issue in the codebase. The incomplete
migration from legacy to new architecture has created parallel implementations
of core types and logic.

### 4.1 Duplicated Type Names [CRITICAL]

The following types exist in multiple locations with identical or near-identical
definitions:

| Type | Location 1 | Location 2 | Location 3 |
|------|-----------|-----------|-----------|
| `RiskConfig` | `src/config.rs:192` | `src/platform/risk.rs:23` | `src/strategy/core/risk.rs:17` |
| `SplitArbConfig` | `src/strategy/split_arb.rs:53` | `src/strategy/core/split_engine.rs:21` | -- |
| `PriceCache` | `src/strategy/split_arb.rs:27` | `src/strategy/core/price_cache.rs:9` | `src/adapters/binance_ws.rs:442` |
| `ExecutionConfig` | `src/config.rs:144` | `src/strategy/core/executor.rs:21` | -- |
| `OrderExecutor` | `src/strategy/executor.rs:14` | `src/strategy/core/executor.rs:110` | -- |
| `ArbSide` | `src/strategy/split_arb.rs:137` | `src/strategy/core/position.rs:11` | -- |
| `ReconciliationResult` | retired legacy module | -- | -- |

This creates real confusion. The `mod.rs` re-exports use aliases to avoid
collisions (e.g., `ArbSide as CoreArbSide`), but downstream code must know
which variant to use.

### 4.2 Client Construction Sprawl [CRITICAL]

**File**: `src/main.rs`

The most severe duplication is in `PolymarketClient` construction.
The pattern `PolymarketClient::new("https://clob.polymarket.com", true)`
appears **18 times** in `main.rs`. The authenticated variant
(`Wallet::from_env` + `PolymarketClient::new_authenticated`) appears
**10 times** with identical boilerplate:

```rust
// This exact pattern repeats 10+ times across main.rs
let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
let funder = std::env::var("POLYMARKET_FUNDER").ok();
let client = if let Some(ref funder_addr) = funder {
    PolymarketClient::new_authenticated_proxy(
        "https://clob.polymarket.com", wallet, funder_addr, false,
    ).await?
} else {
    PolymarketClient::new_authenticated(
        "https://clob.polymarket.com", wallet, false,
    ).await?
};
```

**Recommendation**: Extract into two helper functions:

```rust
fn create_readonly_client() -> Result<PolymarketClient> { ... }
async fn create_authenticated_client(neg_risk: bool) -> Result<PolymarketClient> { ... }
```

### 4.3 Strategy Adapter Boilerplate [HIGH]

**File**: `src/strategy/adapters.rs` (1,721 lines)

This file exists solely to bridge legacy strategy types to the new
`Strategy` trait interface. It contains `MomentumStrategyAdapter` and
`SplitArbStrategyAdapter` -- wrapper types that delegate to the legacy
implementations. This is 1,721 lines of glue code that would not exist
if the migration were complete.

### 4.4 Duplicate Re-exports with Aliasing [HIGH]

The `strategy/mod.rs` file has **7 aliased re-exports** to avoid name
collisions between legacy and new types:

```rust
pub use core::{ ArbSide as CoreArbSide, ... };
pub use momentum::{ MomentumDetector as LegacyMomentumDetector, ... };
pub use multi_outcome::{ RiskLevel as LegacyRiskLevel, ... };
```

This creates a confusing public API where consumers must know which
variant to use. The `Legacy` and `Core` prefixes are code smells
indicating the migration was started but not finished.

---

## 5. Configuration System

### 5.1 Architecture: Layered Loading [GOOD]

**File**: `src/config.rs`

The configuration system uses the `config` crate with a clean layered approach:

```rust
Config::builder()
    .set_default("logging.level", "info")?
    .add_source(File::from(config_dir.join("default.toml")).required(false))
    .add_source(File::from(config_dir.join(env_name)).required(false))
    .add_source(Environment::with_prefix("PLOY").separator("__").try_parsing(true))
    .build()?
    .try_deserialize()
```

This supports: defaults -> `config/default.toml` -> environment-specific
file -> `PLOY__*` environment variables. The `PLOY__` prefix with `__`
separator allows nested overrides (e.g., `PLOY__MARKET__WS_URL`).

### 5.2 Default Function Sprawl [MEDIUM]

**File**: `src/config.rs`

The config file contains **10 standalone `fn default_*()` functions** for
serde defaults:

```rust
fn default_event_edge_framework() -> String { "deterministic".to_string() }
fn default_event_edge_interval_secs() -> u64 { 30 }
fn default_event_edge_min_edge() -> Decimal { Decimal::new(8, 2) }
fn default_event_edge_max_entry() -> Decimal { Decimal::new(75, 2) }
fn default_event_edge_shares() -> u64 { 100 }
fn default_event_edge_cooldown_secs() -> u64 { 120 }
fn default_event_edge_max_daily_spend_usd() -> Decimal { Decimal::new(50, 0) }
fn default_event_edge_claude_max_turns() -> u32 { 20 }
fn default_poll_interval() -> u64 { 500 }
fn default_confirm_fill_timeout_ms() -> u64 { 2000 }
fn default_max_quote_age() -> u64 { 5 }
fn default_max_connections() -> u32 { 5 }
fn default_log_level() -> String { "info".to_string() }
fn default_max_positions_per_symbol() -> u32 { 1 }
```

This is a common pattern with serde, but the proliferation of free
functions clutters the module. Consider grouping defaults into an
`impl Default` block or a constants module.

### 5.3 Dual Default Paths [MEDIUM]

Defaults are specified in two places:
1. `Config::builder().set_default(...)` in `AppConfig::load_from()`
2. `#[serde(default = "...")]` annotations on struct fields

For example, `execution.poll_interval_ms` has a default set via both
`set_default("execution.poll_interval_ms", 500)` and
`#[serde(default = "default_poll_interval")]`. If these diverge, the
behavior depends on which source is loaded, creating subtle bugs.

**Recommendation**: Use one mechanism consistently. The `impl Default`
approach is cleaner and avoids the dual-path issue.

### 5.4 Hardcoded URLs [MEDIUM]

The Polymarket CLOB URL `"https://clob.polymarket.com"` appears as a
hardcoded string literal **18 times** in `main.rs`. This should be a
constant or pulled from configuration:

```rust
// Should be:
const POLYMARKET_CLOB_URL: &str = "https://clob.polymarket.com";
// Or better, from config:
config.market.rest_url
```

The `config.market.rest_url` field exists but is only used in the
`run_bot()` path. All other command handlers hardcode the URL.

---

## 6. Testing Coverage

### 6.1 Overview

| Metric | Value |
|--------|-------|
| Files with `#[cfg(test)]` | 105 of 187 (56%) |
| Total `#[test]` functions | 283 |
| Dedicated test files | 1 (`src/tui/tests.rs`) |
| Integration test directory | None (`tests/` does not exist) |
| Dev dependencies | `tokio-test`, `mockall` |

### 6.2 Test Distribution [HIGH CONCERN]

While 105 files contain test modules, the tests are overwhelmingly
**unit tests for pure functions** -- config parsing, math calculations,
state machine transitions, and serialization. The areas that matter
most for a trading system are under-tested:

**Well-tested areas:**
- State machine transitions (`domain/state.rs`)
- Financial calculations (`strategy/calculations.rs`)
- Slippage protection (`strategy/slippage.rs`)
- Signal detection (`strategy/signal.rs`)
- NBA win probability model (`strategy/nba_winprob.rs`)
- Circuit breaker logic (`coordination/circuit_breaker.rs`)
- Risk validation chain (`strategy/risk_mgmt/validation.rs`)

**Under-tested areas:**
- Order execution flow (no mock-based integration tests)
- Engine state machine (`strategy/engine.rs` -- 1,558 lines, no tests)
- Client authentication (`adapters/polymarket_clob.rs` -- 1,622 lines)
- Shutdown coordination (`coordination/shutdown.rs`)
- The entire `main.rs` (5,289 lines, no tests)

### 6.3 No Integration Tests [HIGH]

There is no `tests/` directory at the project root. For a trading system,
integration tests that exercise the full order lifecycle (signal detection
-> risk check -> order submission -> fill confirmation -> PnL calculation)
using mocked adapters are essential.

The `mockall` crate is listed in `dev-dependencies` but there is no
evidence of mock-based testing in the codebase. The `Strategy` trait
and execution/order-plane contracts are well-suited for mock-based testing,
but no mocks have been created.

**Recommendation**: Create integration tests in `tests/` that:
1. Mock `PolymarketClient` to simulate order fills and failures
2. Test the `StrategyEngine` state machine end-to-end
3. Test shutdown coordination under various failure scenarios
4. Test circuit breaker behavior with simulated consecutive failures

### 6.4 TUI Tests: Minimal [LOW]

**File**: `src/tui/tests.rs` (78 lines, 5 tests)

The TUI tests cover basic app state (creation, help toggle, market
switching, scrolling, quit). They do not test rendering or data
integration. This is acceptable for a TUI module but notable given
the TUI is a user-facing component.

---

## 7. Dependency Health

### 7.1 Dependency Overview

**File**: `Cargo.toml`

The project has **34 direct dependencies** (plus 3 optional for RL).
Key observations:

| Category | Crates | Assessment |
|----------|--------|------------|
| Async runtime | `tokio` (full features) | Standard, correct |
| HTTP | `reqwest` 0.11 (rustls) | One major version behind (0.12 is current) |
| WebSocket | `tokio-tungstenite` 0.21 | Current |
| Database | `sqlx` 0.7 (postgres) | Current |
| Serialization | `serde` 1, `serde_json` 1 | Current |
| Crypto/signing | `ethers` 2, `alloy` 1 | `ethers` is deprecated in favor of `alloy` |
| TUI | `ratatui` 0.29, `crossterm` 0.27 | Current |
| Error handling | `thiserror` 1, `anyhow` 1 | Current |
| Financial math | `rust_decimal` 1 | Current |
| CLI | `clap` 4 (derive) | Current |
| RL (optional) | `burn` 0.14 | Current |

### 7.2 Deprecated Dependency: `ethers` [HIGH]

The `ethers` crate (version 2) is listed alongside `alloy` (version 1).
The `ethers` crate has been officially deprecated by the Alloy project
and is no longer maintained. The codebase should migrate fully to `alloy`
for EIP-712 signing and wallet operations.

Both crates are currently used:
- `ethers` is used in `src/signing/wallet.rs` for `LocalWallet`
- `alloy` is used by the `polymarket-client-sdk` dependency

Carrying both increases compile times and binary size, and the `ethers`
crate will not receive security patches.

### 7.3 TLS Backend Conflict [LOW]

The `Cargo.toml` pulls in both `native-tls` (for `tokio-tungstenite`)
and `rustls` (for `reqwest`). This means the binary links against two
TLS implementations. While functional, it increases binary size and
attack surface. Consider standardizing on one TLS backend.

### 7.4 Release Profile: Optimized [GOOD]

```toml
[profile.release]
lto = true
codegen-units = 1
panic = "abort"
```

This is an aggressive but appropriate release profile for a trading bot
where binary size and runtime performance matter. The `panic = "abort"`
is correct for a system that should crash-and-restart rather than unwind.

### 7.5 Feature Gating [GOOD]

```toml
[features]
default = []
api = []
rl = ["burn", "burn-ndarray", "bincode"]
```

The RL module is properly feature-gated, avoiding heavy `burn` dependencies
for users who do not need reinforcement learning. The `api` feature gates
the REST API server which requires `DATABASE_URL` at compile time for
SQLx query checking.

---

## 8. Dead Code and Technical Debt

### 8.1 TODO/FIXME Inventory [HIGH]

There are **18 TODO comments** across the codebase in production code:

| File | Line | TODO |
|------|------|------|
| `src/main.rs` | 3445 | `// TODO: Implement monitoring mode` |
| `src/main.rs` | 3531 | `// TODO: Implement monitoring mode` |
| `src/main.rs` | 4770 | `// TODO: Execute real order via PolymarketClient` |
| `src/main.rs` | 4781 | `// TODO: Execute real order via PolymarketClient` |
| `src/main.rs` | 4792 | `// TODO: Execute real order` |
| `src/main.rs` | 4803 | `// TODO: Execute real order` |
| `src/adapters/postgres.rs` | 822 | `// TODO: use config` (hardcoded 500) |
| `src/platform/platform.rs` | 432 | `// TODO: get domain from report` |
| `src/cli/strategy.rs` | 821 | `// TODO: Implement actual uptime calculation` |
| `src/ai_clients/autonomous.rs` | 565 | `// TODO: Integrate with RiskManager` |
| `src/api/handlers/system.rs` | 31 | `// TODO: Get from config` |
| `src/api/handlers/stats.rs` | 271 | `// TODO: Get current price from market data` |

The most concerning TODOs are the four "Execute real order" entries in
`main.rs` (lines 4770-4803), which suggest that some command paths have
**stub order execution** that silently does nothing in production.

### 8.2 Dead Code Markers [LOW]

Only **1 instance** of `#[allow(dead_code)]` was found in the codebase
(in `main.rs`). This is a good sign -- the compiler's dead code warnings
are not being suppressed broadly. However, the absence of `#[allow(unused)]`
does not mean there is no dead code; the extensive re-export surface in
`strategy/mod.rs` (182 lines of `pub use` statements) may mask unused
items by making everything public.

### 8.3 Untracked New Files [MEDIUM]

The git status originally showed several untracked files that appeared to be
work-in-progress or experimental:

```
?? src/strategy/event_edge.rs
?? src/strategy/event_models/
```

These files are referenced by `strategy/mod.rs` (`pub mod event_edge`,
`pub mod event_models`) but are not committed to git. This means the
`main` branch may not compile without these files present locally.

### 8.4 Largest Files by Line Count [MEDIUM]

Files over 1,000 lines warrant attention for potential decomposition:

| File | Lines | Concern |
|------|-------|---------|
| `src/main.rs` | 5,289 | Should be <100 lines |
| `src/cli/legacy.rs` | 3,045 | CLI definitions; large but structured |
| `src/strategy/momentum.rs` | 2,828 | Single strategy file; could split config/engine/execution |
| `src/strategy/adapters.rs` | 1,721 | Pure glue code from incomplete migration |
| `src/adapters/polymarket_clob.rs` | 1,622 | API client; reasonable for scope |
| `src/strategy/engine.rs` | 1,558 | Core engine; reasonable but untested |
| `src/strategy/backtest.rs` | 1,239 | Backtesting; reasonable |
| `src/ai_clients/sports_data.rs` | 1,142 | Data fetching; could split by source |
| `src/adapters/polymarket_ws.rs` | 1,113 | WebSocket client; reasonable |
| `src/strategy/multi_outcome.rs` | 1,102 | Multi-outcome analysis |
| `src/ai_clients/polymarket_sports.rs` | 1,102 | Sports agent |

---

## 9. Additional Observations

### 9.1 Concurrency Patterns [GOOD]

The engine at `src/strategy/engine.rs` uses several sound concurrency
patterns:

- **Optimistic locking** via `version: u64` on `EngineState` to detect
  concurrent modifications
- **Execution mutex** (`Mutex<()>`) separate from the state `RwLock` to
  prevent concurrent order submissions without blocking state reads
- **Snapshot-then-act**: The `on_quote_update` method snapshots state
  under a read lock, drops the lock, then performs async work -- avoiding
  holding locks across `.await` points

```rust
// src/strategy/engine.rs:164-171
let (round, strategy_state, current_cycle) = {
    let state = self.state.read().await;
    let Some(round) = state.current_round.clone() else {
        return Ok(());
    };
    (round, state.strategy_state, state.current_cycle.clone())
};
// Lock dropped here, async work follows
```

### 9.2 Safety Guard in Engine [GOOD]

The engine constructor at `src/strategy/engine.rs:79-83` enforces that
`confirm_fills` must be enabled when not in dry-run mode:

```rust
if !config.dry_run.enabled && !config.execution.confirm_fills {
    return Err(PloyError::Validation(
        "execution.confirm_fills must be true when dry_run.enabled is false"
    ));
}
```

This prevents a class of bugs where submitted orders are treated as
failures because fill confirmation was not enabled.

### 9.3 Validation at Boundaries [GOOD]

Historical note: the repo previously carried a top-level
`src/validation.rs` helper module for boundary validation. That module
has since been retired in favor of the remaining canonical validation
surfaces under strategy/risk-management code.

### 9.4 Comment Language Mixing [LOW]

A few comments use non-English text, for example:

```rust
// src/platform/platform.rs:432
domain: super::types::Domain::Crypto, // TODO: 從 report 獲取
```

This is a minor consistency issue. All comments should use a single
language for maintainability by a broader team.

---

## 10. Recommendations Summary

### Priority 1: Critical (address immediately)

| # | Issue | Effort | Impact |
|---|-------|--------|--------|
| 1 | Extract client construction into helper functions (18 duplicated `PolymarketClient::new` calls, 10 duplicated auth flows in `main.rs`) | Small | Eliminates ~200 lines of duplication and reduces risk of URL/config drift |
| 2 | Resolve the legacy/new architecture split -- either complete the migration to `core/crypto/sports` or remove the new modules | Large | Eliminates 7 aliased re-exports, 1,721 lines of adapter glue, and duplicate type definitions |
| 3 | Audit the 4 "TODO: Execute real order" stubs in `main.rs` (lines 4770-4803) to confirm they are not reachable in production | Small | Prevents silent order execution failures |

### Priority 2: High (address this sprint)

| # | Issue | Effort | Impact |
|---|-------|--------|--------|
| 4 | Decompose `main.rs` (5,289 lines) -- extract `run_*` functions into `cli/` submodules, reduce `main.rs` to <100 lines | Medium | Improves maintainability, enables testing of command handlers |
| 5 | Add integration tests for `StrategyEngine` using mocked adapters (`mockall` is already a dev-dependency but unused) | Medium | Catches state machine bugs before production |
| 6 | Migrate from deprecated `ethers` crate to `alloy` for wallet/signing operations | Medium | Removes unmaintained dependency, reduces binary size |
| 7 | Replace hardcoded `"https://clob.polymarket.com"` (18 occurrences) with a constant or config value | Small | Single point of change for URL updates |

### Priority 3: Medium (address this quarter)

| # | Issue | Effort | Impact |
|---|-------|--------|--------|
| 8 | Introduce newtype wrappers for domain IDs (`TokenId`, `ConditionId`, `EventId`, `OrderId`) | Medium | Compile-time prevention of ID mixups |
| 9 | Consolidate config defaults to a single mechanism (either `impl Default` or `serde(default)`, not both) | Small | Eliminates dual-default divergence risk |
| 10 | Replace production `.unwrap()` calls with `.expect("reason")` (focus on `platform/queue.rs` with 22 occurrences and `platform/position.rs` with 11) | Small | Documents invariants, improves panic messages |
| 12 | Decouple `strategy/core/split_engine.rs` from `PolymarketClient` -- accept an `OrderSubmitter` trait instead | Medium | Makes the "generic" core engine actually generic |

### Priority 4: Low (backlog)

| # | Issue | Effort | Impact |
|---|-------|--------|--------|
| 13 | Standardize on a single TLS backend (either `native-tls` or `rustls`, not both) | Small | Reduces binary size and attack surface |
| 14 | Standardize comment language to English across the codebase | Small | Improves readability for broader team |
| 15 | Reduce `strategy/mod.rs` re-export surface (182 lines of `pub use`) -- consider re-exporting only the types that external consumers need | Medium | Cleaner public API, easier dead code detection |

---

## 11. Architecture Strengths (What to Preserve)

The following patterns are well-executed and should be preserved during any
refactoring:

1. **Domain layer isolation** -- `src/domain/` has zero upward dependencies
2. **State machine with validated transitions** -- `StrategyState::can_transition_to()`
3. **Decimal everywhere** -- no `f64` for financial math
4. **Zeroize for secrets** -- private keys cleared from memory after use
5. **Optimistic locking** -- version numbers prevent concurrent state corruption
6. **Execution mutex** -- separate from state lock, prevents order races
7. **Circuit breaker pattern** -- Closed/Open/HalfOpen with configurable thresholds
8. **Event sourcing** -- audit trail via `EventStore`
9. **Feature gating** -- RL and API modules are optional
10. **Release profile** -- LTO + single codegen unit + panic=abort

---

## 12. Conclusion

The Ploy codebase has a **strong foundation** with good Rust practices, proper
financial type safety, and comprehensive unit testing. The primary quality
concern is the **incomplete architecture migration** that has created duplicate
types, duplicate business logic, and a 1,721-line adapter layer that exists
solely as migration glue.

The recommended approach is to **pick one architecture** (legacy or new) and
commit to it fully. The legacy modules are battle-tested and should likely be
the canonical choice, with the new `core/crypto/sports` modules either
completed or removed.

The secondary concern is the **5,289-line `main.rs`** which should be
decomposed into CLI handler modules. This would also enable testing of
command handlers, which is currently impossible.

Overall quality score: **7/10** -- strong fundamentals, significant structural
debt from incomplete migration.

---

*Report generated 2026-02-08 by Code Quality Review Agent*
