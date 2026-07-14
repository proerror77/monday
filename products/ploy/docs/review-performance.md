# Performance & Reliability Analysis

**Date**: 2026-02-08
**Scope**: Async architecture, latency-critical paths, database, memory, concurrency, error recovery, monitoring, TUI
**Codebase**: ~74K lines Rust, 150+ source files

---

## Executive Summary

The Ploy trading bot has a solid async foundation built on tokio with good use of Rust's type system. However, several performance bottlenecks exist in the latency-critical arbitrage path, primarily around **lock contention in the strategy engine**, **synchronous database writes on the hot path**, and **excessive cloning of state objects**. The most impactful optimization would be restructuring the engine's state management to minimize lock hold times during order execution.

**Severity Legend**: P0 = Critical latency/reliability, P1 = Significant, P2 = Moderate, P3 = Minor

---

## 1. Async Architecture

### 1.1 Tokio Runtime Configuration

**File**: `src/main.rs:19`

```rust
#[tokio::main]
async fn main() -> Result<()> {
```

**Finding (P2)**: Uses default `#[tokio::main]` which creates a multi-threaded runtime with default thread count (num_cpus). For a latency-sensitive trading bot, this is acceptable but not optimal.

**Recommendation**: Consider explicit runtime configuration:
- Set `worker_threads` to a fixed count (e.g., 4) to avoid scheduling jitter
- Enable `thread_keep_alive` tuning for consistent latency
- Consider a dedicated single-threaded runtime for the hot path (WS -> strategy -> order)

### 1.2 Task Spawning & Cancellation Safety

**Files**: `src/main.rs`, `src/tui/runner.rs:90-103`, `src/adapters/binance_ws.rs`

Tasks are spawned with `tokio::spawn` without `JoinHandle` tracking in several places:

```rust
// src/tui/runner.rs:90
tokio::spawn(async move {
    Self::run_binance_feed(symbols, event_tx, running).await;
});
```

**Finding (P2)**: Spawned tasks are fire-and-forget. If they panic, the panic is silently swallowed. No structured concurrency pattern is used.

**Recommendation**:
- Store `JoinHandle`s and join them during shutdown
- Use `tokio::task::JoinSet` for managing groups of related tasks
- Add panic hooks or use `tokio::spawn` wrappers that log panics

### 1.3 `select!` Usage & Cancellation Safety

**Files**: Multiple (13 `select!` sites found)

Key `select!` patterns reviewed:

```rust
// src/strategy/engine.rs:138 - Engine main loop
match tokio::time::timeout(Duration::from_secs(1), updates.recv()).await {
```

**Finding (P1)**: The engine uses `timeout` + `recv()` instead of `select!`, which is actually **good** for cancellation safety since `broadcast::Receiver::recv()` is cancel-safe. However, the 1-second timeout means the engine can be up to 1 second late detecting round transitions when no quotes arrive.

```rust
// src/tui/runner.rs:117-144 - TUI event loop
tokio::select! {
    _ = tokio::time::sleep(Duration::from_millis(50)) => { ... }
    Some(event) = event_rx.recv() => { ... }
}
```

**Finding (P2)**: The TUI polls keyboard input with a 50ms sleep + `crossterm::event::poll(0ms)`. This creates a busy-wait pattern that consumes CPU unnecessarily. The `select!` biases toward the sleep branch since it completes first most of the time.

**Recommendation**: Use `crossterm::event::EventStream` (async) instead of polling, or increase the poll timeout.

---

## 2. Latency-Critical Paths

The hot path for arbitrage is: **WebSocket message -> JSON parse -> Quote cache update -> Signal detection -> Order submission**. Every millisecond matters for capturing mispricings before they close.

### 2.1 WebSocket Message Processing

**File**: `src/adapters/polymarket_ws.rs:822`

```rust
tokio::select! {
    msg = read.next() => { ... }
    _ = ping_interval.tick() => { ... }
}
```

**Finding (P1)**: The WebSocket message loop processes messages sequentially. Each message goes through:
1. JSON deserialization (serde)
2. Book top extraction with `extract_best_and_total()` (iterates all price levels)
3. `QuoteCache::update()` (DashMap insert)
4. `broadcast::send()` to strategy engine

The `extract_best_and_total()` function at `polymarket_ws.rs:349-368` iterates **all** price levels to find best bid/ask because it doesn't assume ordering from the exchange. This is correct but adds O(n) per update where n = book depth.

**Recommendation**:
- For incremental book updates, maintain a sorted book locally and update in O(log n)
- For snapshot updates, the current approach is fine since it's infrequent

### 2.2 QuoteCache: DashMap vs RwLock

**File**: `src/adapters/polymarket_ws.rs:443-444`

```rust
pub struct QuoteCache {
    quotes: Arc<dashmap::DashMap<String, Quote>>,
```

**Finding (P3 - Positive)**: Good choice. DashMap provides lock-free concurrent reads which is critical since the cache is read by the strategy engine on every tick and written by the WebSocket handler. The comment claims 2000+ ops/sec vs ~500 with RwLock, which is reasonable for this workload.

**Concern**: The `cleanup_stale()` method at line 605 calls `retain()` which briefly locks all shards. This is called from `update()` when cache is full (line 485-487), meaning a hot-path write can trigger a full cache scan.

**Recommendation**: Run `cleanup_stale()` on a background timer (e.g., every 10s) instead of inline on the write path.

### 2.3 Strategy Engine: Lock Contention on Hot Path

**File**: `src/strategy/engine.rs:16-29`

```rust
pub struct StrategyEngine {
    state: Arc<RwLock<EngineState>>,
    signal_detector: Arc<RwLock<SignalDetector>>,
    execution_mutex: Mutex<()>,
}
```

**Finding (P0 - Critical)**: The engine's `on_quote_update()` method (line 162) acquires **three separate locks** on the hot path:

1. `state.read().await` (line 165) - snapshot state
2. `signal_detector.write().await` (line 209) - update detector with new quote
3. `execution_mutex.lock().await` (line 386) - if entering a trade

The `signal_detector` requires a **write lock** on every quote update because `detector.update()` mutates internal rolling windows. This means every incoming quote blocks all other readers of the signal detector.

Additionally, `on_quote_update()` clones `Round` and `CycleContext` on every tick (lines 166, 170):

```rust
let Some(round) = state.current_round.clone() else { ... };
(round, state.strategy_state, state.current_cycle.clone())
```

`Round` contains multiple `String` fields (slug, up_token_id, down_token_id), so each clone allocates. At ~10-50 quotes/second, this is ~500-2500 allocations/second just for state snapshots.

**Recommendations**:
- Use `Arc<str>` instead of `String` for token IDs and slugs (clone is just refcount bump)
- Move `SignalDetector` behind a `Mutex` instead of `RwLock` (it's always write-locked anyway)
- Consider making `SignalDetector::update()` take immutable `&self` by using interior mutability (`Cell`/`RefCell` for the rolling windows) to avoid write-lock contention
- Pre-filter quotes by token ID **before** acquiring any locks (currently done at line 196, after the state lock)

**Estimated Impact**: 30-50% reduction in hot-path latency by eliminating unnecessary lock acquisitions and allocations.

### 2.4 Order Execution Latency

**File**: `src/strategy/executor.rs:196-261`

The `execute_with_retry()` method uses exponential backoff:

```rust
let delay = Duration::from_millis(100 * (1 << attempts));
```

**Finding (P1)**: Retry delays grow as 200ms, 400ms, 800ms. For a time-sensitive arbitrage where the window may only be seconds, even the first retry at 200ms may be too slow. The opportunity may have closed.

**Finding (P2)**: The fill confirmation polling at `executor.rs:287-314` uses `confirm_fill_timeout_ms` with a `poll_interval_ms` loop. Each poll makes an HTTP GET to the Polymarket API. At default 200ms intervals, this adds 200ms+ per poll round-trip.

**Recommendation**:
- For arbitrage strategies, use aggressive retry: 50ms, 100ms, 200ms
- Consider WebSocket-based fill notifications instead of REST polling
- Add latency metrics to track actual order-to-fill times

### 2.5 Binance WebSocket: Price History Linear Scan

**File**: `src/adapters/binance_ws.rs:262-273`

```rust
pub fn price_secs_ago(&self, secs: u64) -> Option<Decimal> {
    let target_time = self.timestamp - chrono::Duration::seconds(secs as i64);
    for (price, ts) in &self.history {
        if *ts <= target_time {
            return Some(*price);
        }
    }
    self.history.back().map(|(p, _)| *p)
}
```

**Finding (P2)**: `price_secs_ago()` does a linear scan of the VecDeque (up to 300 entries). This is called multiple times per momentum calculation: `momentum(10)`, `momentum(30)`, `momentum(60)` each scan from the front. The `weighted_momentum()` method at line 303 calls this 3 times.

**Recommendation**: Use binary search on the timestamp-sorted VecDeque, or cache the lookback results since the same lookback windows are queried repeatedly.

### 2.6 Volatility Calculation

**File**: `src/adapters/binance_ws.rs:333-383`

The `volatility()` method collects prices into a `Vec`, computes returns, then calculates variance and a Newton's method square root.

**Finding (P3)**: Allocates a new `Vec` on every call. For the momentum strategy which checks volatility on every price update, this creates allocation pressure. The Newton's method sqrt is fine for `Decimal` types where `f64::sqrt()` isn't available.

**Recommendation**: Pre-allocate the returns buffer or compute volatility incrementally using Welford's online algorithm.

---

## 3. Database Performance

### 3.1 Connection Pool Configuration

**File**: `src/adapters/postgres.rs:17-21`

```rust
pub async fn new(database_url: &str, max_connections: u32) -> Result<Self> {
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(database_url)
        .await?;
```

**Finding (P2)**: No `min_connections`, `acquire_timeout`, `idle_timeout`, or `max_lifetime` configured. The pool relies entirely on sqlx defaults.

**Recommendation**:
- Set `min_connections(2)` to avoid cold-start latency on first queries
- Set `acquire_timeout(Duration::from_secs(5))` to fail fast instead of hanging
- Set `idle_timeout(Duration::from_secs(600))` to recycle stale connections
- Set `max_lifetime(Duration::from_secs(1800))` to prevent long-lived connection issues

### 3.2 Tick Batch Insert: Sequential in Transaction

**File**: `src/adapters/postgres.rs:146-174`

```rust
pub async fn insert_ticks(&self, ticks: &[Tick]) -> Result<()> {
    let mut tx = self.pool.begin().await?;
    for tick in ticks {
        sqlx::query(...)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
```

**Finding (P1)**: Ticks are inserted one-by-one inside a transaction. For high-frequency data collection, this means N round-trips to the database per batch. With 50 ticks/second, this is 50 individual INSERT statements per commit.

**Recommendation**: Use a single multi-row INSERT with `unnest()` or build a VALUES clause:
```sql
INSERT INTO ticks (round_id, timestamp, side, best_bid, best_ask, bid_size, ask_size)
SELECT * FROM UNNEST($1::int[], $2::timestamptz[], $3::text[], ...)
```
This reduces N round-trips to 1, typically 5-10x faster for batches > 10 rows.

### 3.3 Database on the Hot Path

**File**: `src/strategy/engine.rs:334`

```rust
let round_id = self.store.upsert_round(&round).await?;
```

**Finding (P0 - Critical)**: `set_round()` calls `upsert_round()` which executes an `INSERT ... ON CONFLICT DO UPDATE` SQL statement. This is called from the main strategy loop whenever a new round is detected. While not per-tick, it blocks the engine's event processing during the database round-trip (typically 1-5ms local, 10-50ms remote).

More critically, `persist_strategy_state_best_effort()` (line 378) is called after every round change, adding another DB write to the hot path.

**Recommendation**:
- Move DB persistence to a background task via an mpsc channel
- The engine should send state snapshots to a writer task, not block on DB I/O
- Use `tokio::spawn` for best-effort persistence so the engine never waits

### 3.4 Missing Indexes (Potential)

**File**: `src/adapters/postgres.rs:96-106`

```sql
SELECT ... FROM rounds
WHERE start_time <= NOW() AND end_time > NOW()
ORDER BY start_time DESC LIMIT 1
```

**Finding (P3)**: The `get_active_round()` query filters on `start_time` and `end_time`. Without a composite index on `(start_time, end_time)`, this requires a sequential scan on the rounds table. For a small table this is fine, but worth noting.

**Finding (P3)**: The `get_ticks_for_round()` query at line 177 fetches all ticks for a round ordered by timestamp. An index on `(round_id, timestamp)` would be beneficial for backtesting queries that scan large tick histories.

---

## 4. Memory Management

### 4.1 Bounded Collections (Positive)

Several collections are properly bounded:

- `QuoteCache`: max 10,000 entries with TTL eviction (`polymarket_ws.rs:429`)
- `SpotPrice.history`: max 300 entries (`binance_ws.rs:41`)
- `TuiApp.transactions`: max 100 entries (`tui/app.rs:14`)
- `broadcast` channels: sized at 1000 (`binance_ws.rs:37`, `polymarket_ws.rs:678`)

**Finding (P3 - Positive)**: Good discipline on bounding collections. The previous review's fix for bounded TUI channels is confirmed in place.

### 4.2 Unbounded Growth Risks

**Historical file**: `src/supervisor/watchdog.rs:103` (retired in March 2026 cleanup)

```rust
struct TrackedComponent {
    health: ComponentHealth,
    restart_timestamps: Vec<DateTime<Utc>>,
}
```

**Finding (P3)**: `restart_timestamps` is cleaned up in `record_restart()` (line 272) by retaining only timestamps within the window. However, the cleanup only runs when a restart is recorded. If a component is repeatedly marked stale without restart (e.g., restart limit reached), the timestamps accumulate. In practice this is bounded by `max_restart_attempts` so the risk is minimal.

**File**: `src/strategy/momentum.rs` - `EventMatcher.active_events`

**Finding (P2)**: The `active_events` cache (`Arc<RwLock<HashMap<String, Vec<EventInfo>>>>`) is populated by `refresh_events()` but old events are only replaced when the same series is refreshed. If a series is removed from configuration, its events remain in memory indefinitely.

**Recommendation**: Add a periodic sweep that removes events past their `end_time`.

### 4.3 Broadcast Channel Lagging

**File**: `src/strategy/engine.rs:144`

```rust
Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
    warn!("Missed {} quote updates", n);
}
```

**Finding (P1)**: When the strategy engine is slow (e.g., blocked on DB or execution), the broadcast channel drops old messages. The engine logs a warning but **does not request a fresh snapshot**. This means the engine may be operating on stale quote data without knowing it.

For arbitrage, stale quotes can lead to:
- Entering trades at prices that no longer exist
- Missing exit signals
- Incorrect spread calculations

**Recommendation**:
- After a lag event, immediately fetch fresh quotes from the `QuoteCache` (which is always current)
- Consider using `mpsc` with backpressure instead of `broadcast` for the engine's feed
- Add a metric counter for lag events to monitor processing capacity

---

## 5. Concurrency Patterns

### 5.1 Lock Inventory

The codebase uses **336 lock instances** across 50 files. Key patterns:

| Component | Lock Type | Contention Risk | Hot Path? |
|-----------|-----------|----------------|-----------|
| `EngineState` | `RwLock` | Medium | Yes |
| `SignalDetector` | `RwLock` (always write) | High | Yes |
| `execution_mutex` | `Mutex` | Low (serializes orders) | Yes |
| `QuoteCache` | `DashMap` | Low | Yes |
| `FundManager` (5 locks) | `RwLock` x5 | Medium | On entry |
| `CircuitBreaker` state | `RwLock` | Low | On trade |
| `LifecycleManager` | `RwLock` | Low | No |

### 5.2 FundManager: Five Separate Locks

**File**: `src/strategy/fund_manager.rs:21-34`

```rust
pub struct FundManager {
    active_positions: Arc<RwLock<HashSet<String>>>,
    positions_per_symbol: Arc<RwLock<HashMap<String, u32>>>,
    symbol_exposure: Arc<RwLock<HashMap<String, Decimal>>>,
    cached_balance: Arc<RwLock<Option<(Decimal, tokio::time::Instant)>>>,
}
```

**Finding (P1)**: `can_open_position()` (line 100) acquires up to **four separate read locks** sequentially:
1. `active_positions.read()` (line 108)
2. `positions_per_symbol.read()` (line 128)
3. `cached_balance` via `get_balance()` (line 139)
4. `symbol_exposure` via `get_per_symbol_allocation()` (line 150)

Each lock acquisition is an await point. Under contention (e.g., multiple symbols triggering simultaneously), this creates a lock convoy where tasks queue behind each other.

**Recommendation**: Consolidate into a single `RwLock<FundState>` struct containing all fields. One lock acquisition instead of four.

### 5.3 Circuit Breaker: Mixed Atomics and RwLock

**File**: `src/coordination/circuit_breaker.rs:93-106`

The `TradingCircuitBreaker` mixes `AtomicU32` for counters with `Arc<RwLock<>>` for state and timestamps. The `should_allow()` method (line 138) acquires up to 3 read locks sequentially: `state`, `opened_at`, then potentially writes to `state` for the HalfOpen transition.

**Finding (P2)**: The TOCTOU pattern in `should_allow()` means two concurrent callers could both see `Open`, both check the timeout, and both transition to `HalfOpen`. This is benign (both get `true`) but the double transition logs duplicate messages.

**Finding (P2)**: The WebSocket-level `CircuitBreaker` at `polymarket_ws.rs:187-302` has a similar pattern but also mixes `AtomicU32` with `RwLock` for state transitions. The `record_failure()` method reads state, then writes state in separate lock acquisitions, creating a race window.

**Recommendation**: Use a single `Mutex` for the entire circuit breaker state (counters + state + timestamps). The lock is only held briefly and eliminates all TOCTOU races.

### 5.4 Deadlock Risk Assessment

**Finding (P3 - Positive)**: The previous review identified and fixed a shutdown deadlock (`shutdown.rs` at commit f55a62b). The `wait_for_completion()` method now checks the phase before waiting and has a timeout guard.

**Finding (P2)**: In `engine.rs`, the `enter_leg1()` method (line 385) acquires `execution_mutex` then `state.write()` (line 482). The `on_quote_update()` method acquires `state.read()` first (line 165). This ordering is safe because read locks don't block each other, but if any code path acquires `state.write()` then `execution_mutex`, a deadlock could occur. Current code appears safe but the lock ordering should be documented.

---

## 6. Error Recovery

### 6.1 Retry Logic

**File**: `src/strategy/executor.rs:196-261`

The executor uses exponential backoff with `100 * (1 << attempts)` milliseconds.

**Finding (P3 - Positive)**: Good pattern. The backoff is capped implicitly by `max_retries` (default 3), giving delays of 200ms, 400ms, 800ms.

**Finding (P2)**: No jitter is added to retry delays. If multiple strategies retry simultaneously (e.g., after a Polymarket API hiccup), they'll all retry at the same intervals, creating thundering herd effects.

**Recommendation**: Add random jitter: `delay * (0.75 + rand(0.5))`

### 6.2 WebSocket Reconnection

**File**: `src/adapters/polymarket_ws.rs:731-792`

**Finding (P3 - Positive)**: The WebSocket reconnection logic is well-implemented:
- Exponential backoff with jitter (lines 770-780)
- Cap at 60 seconds max delay
- Circuit breaker integration to avoid hammering a down server
- Infinite reconnection loop (appropriate for 24/7 operation)

**Finding (P2)**: The jitter implementation uses `SystemTime::now().as_nanos()` as a seed, which is not truly random but sufficient for jitter purposes. A proper `rand` crate would be better but this is a minor concern.

### 6.3 DLQ Processing

**File**: `src/persistence/dlq_processor.rs:1-100`

**Finding (P3 - Positive)**: The DLQ processor is well-designed:
- Configurable batch size (default 10)
- Exponential backoff per entry with `saturating_mul` to prevent overflow
- Max backoff cap at 1 hour
- Clear result types: `Success`, `Retry`, `PermanentFailure`, `Skip`
- Handler trait for extensibility

**Finding (P2)**: The `process_interval_secs` default of 60 seconds means failed operations wait up to 1 minute before retry. For time-sensitive order failures, this may be too slow.

**Recommendation**: Consider a two-tier DLQ: fast retry (5s) for transient failures, slow retry (60s) for persistent failures.

### 6.4 Graceful Shutdown Sequence

**File**: `src/coordination/shutdown.rs:179-284`

**Finding (P3 - Positive)**: The shutdown sequence is well-structured with 6 phases:
1. Stop new orders
2. Drain pending orders (60s timeout)
3. Checkpoint state
4. Close WebSockets (10s timeout)
5. Flush database (30s timeout)
6. Close connections

Each phase has individual timeouts plus a global 120s timeout. The `wait_for_completion()` fix (commit f55a62b) prevents the deadlock where callers would wait forever if `execute()` had already completed.

**Finding (P2)**: The shutdown phases run sequentially. Phases 4 (WebSocket close) and 5 (DB flush) could run in parallel since they're independent, saving up to 10s in shutdown time.

---

## 7. Monitoring & Observability

### 7.1 Historical Health Server

**Historical file**: `src/services/health.rs:1-200` (retired in March 2026 cleanup)

**Historical note**: The repo previously carried a standalone health/metrics server that exposed:
- liveness/readiness probes
- component-level health summaries
- quote staleness detection
- uptime tracking

That standalone server was later retired because no runtime path actually started it; the active deployment surface now relies on the API server's `/health` endpoint instead.
2. `last_ws_message.read()` again (line 167)
3. `last_db_check.read()` (line 188)
4. `strategy_state.read()` (line 210, estimated)

This is fine for a health endpoint called infrequently, but the double read of `last_ws_message` is wasteful.

### 7.2 Historical Watchdog Daemon

**Historical file**: `src/supervisor/watchdog.rs:318-399` (retired in March 2026 cleanup)

**Finding (P3 - Positive)**: The watchdog correctly implements:
- Exponential backoff for restarts (line 388-399, added in commit 558150b)
- Restart window tracking to prevent restart storms
- Event broadcasting for alerting

**Finding (P2)**: The watchdog's `start()` method spawns a background task that holds a write lock on `components` for the entire health check + restart cycle (lines 289-315, 348-399). During a restart attempt (which may take seconds), no other task can record heartbeats, potentially causing false stale detections.

**Recommendation**: Split the check and restart phases. Collect components needing restart under a read lock, release it, then perform restarts. The current code partially does this (line 348 collects under read lock) but the restart phase at line 369 re-acquires a write lock per component.

### 7.3 Missing Observability

**Finding (P1)**: Several critical metrics are not tracked:

| Metric | Why It Matters | Where to Add |
|--------|---------------|--------------|
| Quote-to-order latency | Core performance KPI | `engine.rs:on_quote_update()` |
| Lock wait time | Detect contention | `engine.rs` lock acquisitions |
| Broadcast lag count | Detect processing bottleneck | `engine.rs:144` |
| Order fill rate | Strategy effectiveness | `executor.rs` |
| WebSocket message rate | Detect feed issues | `polymarket_ws.rs` |
| DB query latency (p50/p99) | Detect slow queries | `postgres.rs` |
| Fund manager balance cache hit rate | Detect API pressure | `fund_manager.rs` |

**Recommendation**: Add `tracing::Span` instrumentation or a lightweight metrics crate (e.g., `metrics` + `metrics-exporter-prometheus`) to track these. The existing `Metrics` service provides a foundation but lacks these specific measurements.

---

## 8. TUI Performance

### 8.1 Rendering Efficiency

**File**: `src/tui/runner.rs:110-149`

The TUI main loop renders on every iteration (every 50ms = 20 FPS):

```rust
terminal.draw(|f| ui::render(f, &self.app)).map_err(...)?;
```

**Finding (P2)**: The TUI redraws the entire screen every 50ms regardless of whether data changed. For a terminal UI showing market data, this is acceptable but wasteful when the market is quiet.

**Recommendation**: Track a `dirty` flag on `TuiApp` and only redraw when data changes or a minimum interval (e.g., 250ms) has elapsed. This reduces CPU usage from ~2-5% to near zero during quiet periods.

### 8.2 Event Channel: Unbounded mpsc

**File**: `src/tui/runner.rs:82`

```rust
let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();
```

**Finding (P2)**: The TUI uses an unbounded channel for data events. If the TUI rendering falls behind (e.g., terminal is slow), events accumulate without bound. For a dashboard this is unlikely to cause issues in practice, but a bounded channel with `try_send` (dropping old events) would be safer.

**Recommendation**: Use `mpsc::channel(1000)` with `try_send()` to drop stale events rather than accumulating them.

---

## 9. Duplicate Code: Proxy Connection

### 9.1 Copy-Pasted Proxy Logic

**Files**: `src/adapters/polymarket_ws.rs:20-152`, `src/adapters/binance_ws.rs:42-185`

**Finding (P3)**: The proxy connection logic (`get_proxy_url()`, `parse_proxy_url()`, `connect_via_proxy()`, `connect_websocket_with_proxy()`) is duplicated nearly identically between the Polymarket and Binance WebSocket adapters. This is ~150 lines of duplicated code.

**Recommendation**: Extract into a shared `ws_utils` module. This is a maintainability issue rather than a performance issue, but divergent bug fixes in one copy but not the other could cause subtle connection failures.

---

## 10. Prioritized Optimization Roadmap

### Tier 1: High Impact, Moderate Effort

| # | Issue | File | Impact | Effort |
|---|-------|------|--------|--------|
| 1 | Engine state cloning on every tick | `engine.rs:166-170` | ~30% hot-path speedup | Medium |
| 2 | SignalDetector write lock on every quote | `engine.rs:209` | Eliminates contention | Medium |
| 3 | DB writes blocking hot path | `engine.rs:334,378` | Removes 1-50ms stalls | Medium |
| 4 | Broadcast lag without snapshot recovery | `engine.rs:144` | Prevents stale-data trades | Low |
| 5 | FundManager 4-lock convoy | `fund_manager.rs:100-150` | Faster position entry | Low |

### Tier 2: Moderate Impact, Low Effort

| # | Issue | File | Impact | Effort |
|---|-------|------|--------|--------|
| 6 | Tick batch insert (N round-trips) | `postgres.rs:146-174` | 5-10x faster tick writes | Low |
| 7 | Executor retry jitter missing | `executor.rs:256` | Prevents thundering herd | Trivial |
| 8 | DB pool configuration defaults | `postgres.rs:17-21` | Better connection lifecycle | Trivial |
| 9 | QuoteCache stale cleanup on write path | `polymarket_ws.rs:485` | Removes hot-path stalls | Low |
| 10 | TUI busy-wait polling | `tui/runner.rs:117-144` | Reduces idle CPU 2-5% | Low |

### Tier 3: Low Impact / Long-term

| # | Issue | File | Impact | Effort |
|---|-------|------|--------|--------|
| 11 | Price history linear scan | `binance_ws.rs:262-273` | Faster momentum calc | Low |
| 12 | Volatility Vec allocation per call | `binance_ws.rs:333-383` | Less GC pressure | Medium |
| 13 | Duplicate proxy code | `polymarket_ws.rs`, `binance_ws.rs` | Maintainability | Low |
| 14 | Structured task management | `main.rs`, `tui/runner.rs` | Better panic handling | Medium |
| 15 | Observability gaps | Multiple | Better debugging | High |

---

## 11. Strengths Summary

The codebase demonstrates several strong performance and reliability patterns:

1. **DashMap for QuoteCache** - Lock-free concurrent reads on the hottest data structure
2. **Bounded collections everywhere** - Price history, transaction lists, channels all have caps
3. **Circuit breaker pattern** - Both at WebSocket and trading levels with proper state machines
4. **Optimistic locking** - Engine state versioning prevents stale-state races
5. **Execution mutex** - Prevents concurrent order submissions (critical for CLOB)
6. **TTL-based fund manager cache** - Avoids hammering balance API on every trade check
7. **Graceful shutdown with phases** - Proper order draining before connection teardown
8. **Idempotency protection** - Prevents duplicate orders on retry
9. **WebSocket reconnection with jitter** - Proper backoff prevents reconnection storms
10. **Historical watchdog with exponential backoff** - Prevented restart storms before the supervisor layer was retired

---

## 12. Estimated Latency Budget

Current estimated hot-path latency (WebSocket message to order submission):

| Stage | Current (est.) | Optimized (est.) |
|-------|---------------|-----------------|
| WS message receive | ~0.1ms | ~0.1ms |
| JSON deserialization | ~0.2ms | ~0.2ms |
| Book top extraction | ~0.05ms | ~0.05ms |
| QuoteCache update | ~0.01ms | ~0.01ms |
| Broadcast send | ~0.01ms | ~0.01ms |
| Engine state read + clone | ~0.1ms | ~0.02ms (Arc<str>) |
| Signal detector write lock | ~0.1ms | ~0.02ms (Mutex) |
| Signal evaluation | ~0.05ms | ~0.05ms |
| Execution mutex acquire | ~0.01ms | ~0.01ms |
| DB upsert (on round change) | 1-50ms | 0ms (background) |
| Order submission (HTTP) | 50-200ms | 50-200ms |
| **Total (no DB)** | **~0.6ms** | **~0.4ms** |
| **Total (with DB)** | **1-50ms** | **~0.4ms** |

The dominant latency is the HTTP order submission to Polymarket's CLOB API (50-200ms). Internal processing overhead is small by comparison, but the DB-on-hot-path issue can add significant jitter.
