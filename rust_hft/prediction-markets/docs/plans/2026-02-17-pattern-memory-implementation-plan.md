# Pattern Memory Strategy Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a new `pattern_memory` strategy that trades Polymarket crypto `up-or-down-5m` markets using Binance closed 5m/15m klines, similarity matching, and a weighted Bayesian posterior for “finish above price_to_beat”.

**Architecture:** A `PatternMemoryStrategy` (implements `Strategy`) consumes `MarketUpdate::BinanceKline` (5m/15m), `MarketUpdate::EventDiscovered` (with `price_to_beat`), and `MarketUpdate::PolymarketQuote`. It maintains per-symbol pattern memories and emits `StrategyAction::SubmitOrder` when posterior probability implies positive EV under entry constraints.

**Tech Stack:** Rust, tokio, tokio-tungstenite, serde_json, chrono, rust_decimal, Polymarket Gamma + CLOB WS.

---

### Task 1: Core Pattern Memory Engine (Pure, Unit-Tested)

**Files:**
- Create: `src/strategy/pattern_memory/engine.rs`
- Create: `src/strategy/pattern_memory/mod.rs`
- Test: `src/strategy/pattern_memory/engine.rs`

**Step 1: Write failing tests**
- Pearson correlation: identical vectors => ~1, inverse => ~-1, constant variance => 0
- Weight function clamp behavior for `thr=0.7`
- Beta posterior math for weighted counts
- Strict threshold rule: `next_return == r_req` counts as DOWN

**Step 2: Run tests to verify failure**

Run: `cargo test -q pattern_memory::engine`
Expected: FAIL due to missing module/functions.

**Step 3: Minimal implementation**
- Implement:
  - `pearson_corr(x, y) -> f64`
  - `corr_weight(corr, thr) -> f64`
  - `beta_posterior(alpha, beta, up_w, down_w) -> f64`
  - `classify(next_return, r_req) -> bool` (true=UP)
- Implement `PatternMemory`:
  - Store samples: `pattern: [f64; N]`, `next_return: f64`
  - `ingest_closed_return(r: f64)`:
    - Update rolling returns buffer
    - Convert prior pending pattern into a stored sample when next return arrives
  - `posterior_for_required_return(r_req, thr, alpha, beta) -> Posterior`:
    - Iterate matches, apply weights, compute posterior and `n_eff`

**Step 4: Run tests to verify pass**

Run: `cargo test -q pattern_memory::engine`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/strategy/pattern_memory/mod.rs src/strategy/pattern_memory/engine.rs
git commit -m "feat(pattern_memory): add core engine with bayesian posterior"
```

---

### Task 2: Add Binance Kline Feed Types to Strategy Traits

**Files:**
- Modify: `src/strategy/traits.rs`
- Modify: `src/strategy/feeds.rs`
- Test: `src/strategy/traits.rs`

**Step 1: Write failing tests**
- A serde roundtrip for `DataFeed::BinanceKlines { ... }` (ensures enum is serializable)

**Step 2: Run test to verify failure**

Run: `cargo test -q strategy::traits`
Expected: FAIL (variant missing).

**Step 3: Minimal implementation**
- Add `DataFeed::BinanceKlines { symbols: Vec<String>, intervals: Vec<String>, closed_only: bool }`
- Add `MarketUpdate::BinanceKline { symbol: String, interval: String, kline: KlineBar, timestamp: DateTime<Utc> }`
- Add `KlineBar` struct in `src/strategy/traits.rs` (avoid adapter coupling)

**Step 4: Verify tests pass**

Run: `cargo test -q strategy::traits`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/strategy/traits.rs
git commit -m "feat(strategy): add binance kline feed + market update"
```

---

### Task 3: Binance Kline WebSocket Adapter (Parse + Reconnect)

**Files:**
- Create: `src/adapters/binance_kline_ws.rs`
- Modify: `src/adapters/mod.rs`
- Test: `src/adapters/binance_kline_ws.rs`

**Step 1: Write failing tests**
- JSON parsing test for combined-stream kline event with:
  - interval `5m`
  - `x=true` (closed)
  - open/close prices parsed as Decimal
  - symbol normalized to `BTCUSDT`

**Step 2: Run tests to verify failure**

Run: `cargo test -q binance_kline_ws`
Expected: FAIL (module missing).

**Step 3: Minimal implementation**
- Implement:
  - `BinanceKlineWebSocket::new(symbols, intervals, closed_only)`
  - Broadcast channel of `KlineUpdate`
  - `run()` with reconnect + ping interval
  - Proxy support (reuse pattern from existing WS adapters)
  - Parse combined stream wrapper `{stream,data}` and inner kline payload

**Step 4: Verify tests pass**

Run: `cargo test -q binance_kline_ws`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/adapters/binance_kline_ws.rs src/adapters/mod.rs
git commit -m "feat(adapters): add binance kline websocket adapter"
```

---

### Task 4: DataFeedManager Support for BinanceKlines + Polymarket Event Enrichment

**Files:**
- Modify: `src/strategy/feeds.rs`
- Modify: `src/strategy/traits.rs`
- Modify: `src/adapters/polymarket_clob.rs` (if needed for gamma fields)
- Test: `src/strategy/feeds.rs`

**Step 1: Write failing tests**
- Feed manager translates a `KlineUpdate` into `MarketUpdate::BinanceKline`
- Event discovery emits `MarketUpdate::EventDiscovered` with `price_to_beat` populated when title contains a price

**Step 2: Run tests to verify failure**

Run: `cargo test -q strategy::feeds`
Expected: FAIL (no kline handling / no price_to_beat).

**Step 3: Minimal implementation**
- Add optional `binance_kline_ws` to `DataFeedManager`
- Start kline WS in `start()` similar to spot WS forwarding
- Update Polymarket event discovery to:
  - Extract token IDs from Gamma event markets (`clobTokenIds` first)
  - Parse `price_to_beat` from `title`/`question`
  - Include `title` and `price_to_beat` in `MarketUpdate::EventDiscovered`
- Add periodic refresh loop:
  - Re-discover events per series every `refresh_secs`
  - Reconcile desired token set using `PolymarketWebSocket::reconcile_token_sides`
  - Call `request_resubscribe()` when token set changes

**Step 4: Verify tests pass**

Run: `cargo test -q strategy::feeds`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/strategy/feeds.rs src/strategy/traits.rs
git commit -m "feat(feeds): add binance klines + enriched polymarket discovery"
```

---

### Task 5: Implement `pattern_memory` Strategy (Dry-Run First)

**Files:**
- Create: `src/strategy/pattern_memory/strategy.rs`
- Modify: `src/strategy/mod.rs`
- Modify: `src/strategy/manager.rs`
- Create: `config/strategies/pattern_memory_default.toml`
- Test: `src/strategy/pattern_memory/strategy.rs`

**Step 1: Write failing tests**
- Given:
  - Stored 5m samples
  - An event with `price_to_beat`
  - Quotes for UP/DOWN tokens
  - A closed 5m bar at boundary
  - A 15m direction filter that agrees
  - Config gates satisfied
- Expect:
  - One `StrategyAction::SubmitOrder` for the chosen token

**Step 2: Run tests to verify failure**

Run: `cargo test -q pattern_memory::strategy`
Expected: FAIL.

**Step 3: Minimal implementation**
- Implement `PatternMemoryStrategy` that:
  - Requires feeds:
    - `BinanceKlines` for 5m+15m
    - `PolymarketEvents` for crypto 5m series
    - `Tick` for housekeeping
  - Maintains per-symbol engines for 5m and 15m
  - Stores discovered events and current quotes
  - On 5m closed bar:
    - Select event with time remaining within window
    - Compute `r_req` and posterior `p_up`
    - Apply MTF gate and EV gates
    - Emit `SubmitOrder`
  - De-dups per `event_id`

**Step 4: Verify tests pass**

Run: `cargo test -q pattern_memory::strategy`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/strategy/pattern_memory src/strategy/mod.rs src/strategy/manager.rs config/strategies/pattern_memory_default.toml
git commit -m "feat(strategy): add pattern_memory strategy"
```

---

### Task 6: Wire CLI Foreground Runner for BinanceKlines + Neg-Risk Auth

**Files:**
- Modify: `src/cli/strategy.rs`
- Test: `src/cli/strategy.rs` (smoke-level compile test only)

**Step 1: Write failing test**
- (Optional) Minimal compile-only test is usually enough here; focus on `cargo test`.

**Step 2: Implement**
- Extract kline feed requirements from `required_feeds`
- Configure `DataFeedManager` with BinanceKline WS when needed
- For crypto UP/DOWN strategies (`momentum`, `pattern_memory`), set authenticated `PolymarketClient::new_authenticated(..., neg_risk=true)`

**Step 3: Verify**

Run: `cargo test -q`
Expected: PASS.

**Step 4: Commit**

```bash
git add src/cli/strategy.rs
git commit -m "feat(cli): support binance klines + neg-risk auth for crypto strategies"
```

---

### Task 7: Manual Dry-Run Validation

**Run:**

```bash
cargo run -- strategy start pattern_memory --config config/strategies/pattern_memory_default.toml --foreground --dry-run
```

**Expected:**
- Logs show:
  - Binance kline closed bars for 5m and 15m
  - Polymarket event discovery + token resubscriptions
  - Posterior `p_up`, `n_eff`, `r_req`, and EV gate decisions
  - Dry-run order submissions when gates pass
