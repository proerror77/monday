# Kalshi Full-Parity Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add Kalshi support through a native Rust adapter and exchange abstraction while preserving Polymarket defaults.

**Architecture:** Introduce `ExchangeClient` as a shared execution contract, implement it for `PolymarketClient`, add `KalshiClient` with normalized payload mapping, and wire config fields for exchange selection and credentials.

**Tech Stack:** Rust, tokio, reqwest, serde_json, hmac/sha2, existing domain/adapters.

---

### Task 1: Add exchange contract

**Files:**
- Create: `src/exchange/mod.rs`
- Create: `src/exchange/traits.rs`
- Create: `src/exchange/factory.rs`
- Modify: `src/lib.rs`

**Steps:**
1. Define `ExchangeKind` parse/display helpers.
2. Define `ExchangeClient` async trait for execution + read capabilities.
3. Add factory helpers to construct Polymarket/Kalshi clients.
4. Export module from crate root.

### Task 2: Implement Kalshi REST adapter

**Files:**
- Create: `src/adapters/kalshi_rest.rs`
- Modify: `src/adapters/mod.rs`

**Steps:**
1. Add `KalshiClient` with auth header/signing helper.
2. Implement market search, market details, orderbook fetch.
3. Implement order submit/get/cancel + account methods.
4. Implement `ExchangeClient` for `KalshiClient`.
5. Add adapter unit tests for token parsing and price conversion.

### Task 3: Connect Polymarket to exchange trait

**Files:**
- Modify: `src/adapters/polymarket_clob.rs`

**Steps:**
1. Import `ExchangeClient` and `ExchangeKind`.
2. Implement `ExchangeClient` for `PolymarketClient`.
3. Reuse existing gateway context and status/fill calculators.

### Task 4: Refactor execution to trait object

**Files:**
- Modify: `src/strategy/executor.rs`
- Modify: `src/platform/platform.rs`

**Steps:**
1. Change executor internals to `Arc<dyn ExchangeClient>`.
2. Keep compatibility constructor `new(PolymarketClient, ...)`.
3. Add `new_with_exchange(...)` constructor.
4. Replace static Polymarket status/fill calls with trait calls.

### Task 5: Add config surface for exchange selection

**Files:**
- Modify: `src/config.rs`
- Modify: `config/default.toml`

**Steps:**
1. Add `execution.exchange` (default `polymarket`).
2. Add `kalshi` section (base URL + credentials).
3. Add optional market-level exchange URL overrides.
4. Add validation + unit tests.

### Task 6: Verification

**Files:**
- N/A (command-only)

**Steps:**
1. `cargo fmt`
2. `cargo check`
3. `cargo test --lib config::tests`
4. `cargo test --lib adapters::kalshi_rest::tests`

### Task 7: Platform runtime exchange wiring

**Files:**
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/main_legacy.rs`

**Steps:**
1. Build runtime execution client from `execution.exchange` using exchange factory.
2. Use `OrderExecutor::new_with_exchange(...)` in coordinator bootstrap.
3. Load Polymarket client only for PM-bound built-in agents (crypto/politics).
4. Allow `platform start` with non-Polymarket exchange when built-in agents are disabled.
5. Keep explicit guardrails: reject non-Polymarket + built-in agent combinations.
6. Keep Kalshi runtime disabled by default behind env gate (`PLOY_ENABLE_KALSHI_EXPERIMENTAL=true`).
