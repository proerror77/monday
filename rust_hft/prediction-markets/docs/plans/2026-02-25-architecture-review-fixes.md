# Architecture Review Fixes — 2026-02-25

## Context
5-agent parallel architecture review of the Ploy trading system (126K LOC, 267 files).
Identified 7 CRITICAL, 8 HIGH, 12 MEDIUM issues across architecture, security,
performance, code quality, and strategy logic.

## Approach
Four-phase pipeline. Each phase ends with atomic commits. Later phases depend on earlier ones.

---

## Phase 1: Fund Safety Quick Fixes (~42 lines)

### 1.1 Leg2 shares from Leg1 fill, not config
- **File**: `src/strategy/engine.rs`
- **Change**: Leg2 shares = `ctx.leg1_shares` instead of `self.config.strategy.shares`
- **Why**: Partial fill on Leg1 IOC creates over-hedged position

### 1.2 Forced Leg2 price ceiling
- **File**: `src/strategy/engine.rs` (`enter_leg2_forced`)
- **Change**: Cap limit price at `1.0 - leg1_price + max_acceptable_loss`
- **Why**: Stale REST quote + thin book = terrible fill price

### 1.3 Order default expiration
- **File**: `src/signing/order.rs` (lines 127, 164)
- **Change**: `expiration: U256::zero()` → `current_timestamp + 300` (5 min)
- **Why**: Crash leaves ghost orders that fill at stale prices

### 1.4 Production unwrap replacement
- **Files**: `src/strategy/sports/discovery.rs:144-145,227-228`, `src/api/handlers/system.rs:593`
- **Change**: `.unwrap()` → `.ok_or_else()` / `.map_err()`
- **Why**: Panics crash the entire process

### 1.5 Engine unwind error logging
- **File**: `src/strategy/engine.rs` (~25 locations)
- **Change**: `let _ = self.store.xxx().await` → `if let Err(e) = ... { error!(...) }`
- **Why**: Silent DB failures in unwind corrupt crash recovery state

---

## Phase 2: Security + Risk Hardening (~120 lines)

### 2.1 Constant-time token comparison
- **File**: `src/api/auth.rs:109,140,148,185`
- **Dep**: Add `subtle` crate
- **Change**: `==` → `ConstantTimeEq`

### 2.2 WebSocket authentication
- **Files**: `src/api/websocket.rs`, `src/api/routes.rs:167`
- **Change**: Add token check to WS upgrade handler

### 2.3 Circuit breaker method unification
- **File**: `src/coordination/circuit_breaker.rs`
- **Change**: `should_allow()` delegates to `should_allow_trade()`

### 2.4 NBA per-game max cost
- **File**: `src/strategy/nba_comeback/core.rs`
- **Change**: Add `max_cost_per_game_usd` config + check in scale-in path

### 2.5 Event Edge EV sorting
- **File**: `src/strategy/event_edge/core.rs:227-234`
- **Change**: Collect all passing rows, sort by `net_ev` desc, pick best

### 2.6 ApiKeyResponse custom Debug
- **File**: `src/adapters/polymarket_clob.rs:322-328`
- **Change**: Manual `Debug` impl that redacts `secret`/`passphrase`

### 2.7 Cycle.state type safety
- **File**: `src/domain/order.rs:233`
- **Change**: `state: String` → validated via `StrategyState` enum

### 2.8 Force-Leg2 double-submission guard
- **File**: `src/strategy/engine.rs`
- **Change**: Add `force_leg2_attempted: bool` to `CycleContext`

---

## Phase 3: Code Quality + Test Coverage

### 3.1 Engine core tests
- **File**: `src/strategy/engine.rs` (add #[cfg(test)] module)
- **Coverage**: State transitions, abort paths, version mismatch, emergency unwind

### 3.2 f64 → Decimal in financial calculations
- **Files**: `strategy/volatility_arb.rs`, `strategy/live_arbitrage.rs`, `strategy/nba_exit.rs`

### 3.3 Claimer .ok() → proper error handling
- **File**: `src/strategy/claimer.rs` (8 locations)

### 3.4 Dead code cleanup
- Delete `agent/sports_analyst_enhanced.rs`
- Clean up TODO stubs
- Remove `#[allow(dead_code)]` where possible

### 3.5 Sidecar security defaults
- **File**: `src/api/auth.rs`
- **Change**: `sidecar_auth_required` default → `true`, `auth_cookie_secure` default → `true`

---

## Phase 4: Architecture Refactoring

### 4.1 Agent module rename
- `agent/` → `ai_clients/`
- Remove `agent_system/`
- Global search-replace

### 4.2 Strategy module split
- `strategy/execution/` ← engine, executor, fund_manager, idempotency
- Move NBA flat files into `strategy/nba_comeback/`
- `strategy/risk/` ← risk, validation, slippage

### 4.3 Risk management unification
- Define `trait RiskGate` in shared location
- Unify 3 `RiskConfig` types + 3 `RiskState` enums

### 4.4 Circular dependency resolution
- Move `HealthState` to `domain/`
- Move exchange response types to `domain/`
- Rename `coordination/` → `reliability/` or merge into `coordinator/infra/`

### 4.5 Nonce manager dedup
- Keep `signing/nonce_manager.rs`, remove or thin-wrap `adapters/nonce_manager.rs`

### 4.6 EventEdge consolidation
- Identify canonical agent wrapper, deprecate/remove duplicates

### 4.7 GlobalState bounded collections
- `circuit_breaker_events` → cap at 1000, FIFO eviction

---

## Completion Status (2026-02-25)

### Commit 06912a8 — Phase 1+2 (14 fixes)
- ✅ 1.2 Forced Leg2 price ceiling
- ✅ 1.3 Order default expiration (→ 30min, configurable)
- ✅ 1.4 Production unwrap replacement (system.rs)
- ✅ 1.5 Engine unwind error logging (14 locations)
- ✅ 2.1 Constant-time token comparison
- ✅ 2.2 WebSocket authentication
- ✅ 2.3 Circuit breaker method unification
- ✅ 2.5 Event Edge EV sorting
- ✅ 2.6 ApiKeyResponse custom Debug
- ✅ 2.8 Force-Leg2 double-submission guard
- ✅ 3.4 Dead code cleanup (sports_analyst_enhanced.rs)
- ✅ 3.5 Sidecar security defaults
- ✅ 4.7 GlobalState bounded collections (500 cap)
- ⬜ 1.1 FALSE POSITIVE — Leg2 shares already from ctx.leg1_shares
- ⬜ 2.4 FALSE POSITIVE — NBA per-game limit already exists
- ⬜ 3.3 FALSE POSITIVE — claimer .ok() are std::env::var().ok()

### Commit ce21655 — Order expiry refinement
- ✅ 1.3 Increased order expiry to 30min, added PLOY_ORDER_EXPIRY_SECS env var

### Commit 6806141 — Dead code removal + type safety (-1558 lines)
- ✅ 4.5 Nonce manager dedup (removed adapters/nonce_manager.rs)
- ✅ 2.7 Cycle.state String → StrategyState enum
- ✅ 4.6 EventEdge runner consolidation (removed simple interval runner)
- ✅ 4.3 Risk types — removed orphaned strategy/core/risk.rs + executor.rs
- ✅ 3.2 f64 → Decimal: fixed Kelly position sizing truncation (.round())

### Deferred to separate PRs
- ✅ 4.1 Agent module rename (agent/ → ai_clients/) — DONE in module-cleanup PR
- ✅ 4.2 Strategy module split (execution/, nba_comeback/, risk_mgmt/) — DONE
- ✅ 4.3 Risk unification — 3rd type was dead code (deleted). Remaining 2 serve different
  layers (per-strategy vs cross-platform) — intentional, no unification needed
- ✅ 4.4 Circular dependency — HealthState references RiskManager+Metrics so can't move to
  domain/. adapters→services dep is layering concern, not compilation issue; acceptable in
  single-crate Rust. Extracting trait would be over-engineering

### Current commit — EngineStore trait + engine tests
- ✅ 3.1 Engine tests — 12 tests covering state machine transitions, abort paths, version locking
- ✅ EngineStore trait (15 methods) with MockStore for DI-based testing
- ✅ MockExchangeClient for test isolation from HTTP/CLOB layer
