# Ploy Polymarket Trading Bot -- Security Audit Report

**Date:** 2026-02-08
**Auditor:** Claude Opus 4.6 (Security Engineering)
**Scope:** Key management, API authentication, order safety, coordination, LLM integration, crash recovery, fund management
**Codebase:** ~74K lines Rust, 150+ source files

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Key Management and Wallet Security](#2-key-management-and-wallet-security)
3. [API Authentication and HMAC Signing](#3-api-authentication-and-hmac-signing)
4. [Order Signing and Submission Safety](#4-order-signing-and-submission-safety)
5. [Strategy Engine Concurrency and State Safety](#5-strategy-engine-concurrency-and-state-safety)
6. [Circuit Breaker and Emergency Stop](#6-circuit-breaker-and-emergency-stop)
7. [LLM Agent Security](#7-llm-agent-security)
8. [Shutdown and Crash Recovery](#8-shutdown-and-crash-recovery)
9. [Fund Management and Reconciliation](#9-fund-management-and-reconciliation)
10. [Summary of Findings](#10-summary-of-findings)

---

## 1. Executive Summary

This audit examined the Ploy Polymarket trading bot across seven security-critical areas. The codebase demonstrates strong security awareness in several areas -- private key zeroization, optimistic locking in the strategy engine, EIP-712 typed data signing, and prompt injection sanitization for LLM inputs. However, several findings of varying severity were identified.

**Finding Summary:**
- **Critical:** 1
- **High:** 4
- **Medium:** 6
- **Low:** 5

The single critical finding relates to the `ApiCredentials` struct deriving `Debug` and `Clone`, which can leak secrets into logs. The high-severity findings involve HMAC message logging, the `PrivateKeySigner` remaining in memory after construction, orders defaulting to no expiration, and the `LocalWallet` inner field retaining the private key despite the zeroization of the hex string.

---

## 2. Key Management and Wallet Security

**Files reviewed:**
- `/Users/proerror/Documents/ploy/src/signing/wallet.rs`
- `/Users/proerror/Documents/ploy/src/signing/order.rs`
- `/Users/proerror/Documents/ploy/src/adapters/polymarket_clob.rs`

### 2.1 Private Key Zeroization (Good Practice)

The `Wallet::from_private_key` method correctly zeroizes the hex string copy after parsing:

```rust
// src/signing/wallet.rs:30-38
let mut secure_key = key_hex.to_string();
let wallet = secure_key
    .parse::<LocalWallet>()
    .map_err(|e| PloyError::Wallet(format!("Invalid private key: {}", e)))?
    .with_chain_id(chain_id);
secure_key.zeroize();
```

Similarly, `Wallet::from_env` zeroizes the environment variable string after use. The `PolymarketClient::new_authenticated` also zeroizes the private key hex string in a scoped block.

### Finding SEC-01: LocalWallet Retains Private Key in Memory

| Attribute | Value |
|-----------|-------|
| **Severity** | High |
| **File** | `src/signing/wallet.rs:14-17` |
| **Status** | Open |

The `Wallet` struct wraps `ethers::signers::LocalWallet`, which internally stores the private key as a `k256::ecdsa::SigningKey`. While the hex string is zeroized, the actual cryptographic key material persists in the `LocalWallet` for the lifetime of the `Wallet` struct. The `Wallet` also derives `Clone`, meaning copies of the key material can proliferate.

Additionally, the `inner()` method on line 118-120 exposes the underlying `LocalWallet` directly, allowing callers to extract the signing key.

**Remediation:**
- Document that the `LocalWallet` retains key material by design (required for signing).
- Remove or restrict the `pub fn inner()` method to `pub(crate)` to limit exposure surface.
- Consider implementing `Drop` with zeroization for the `Wallet` struct if the `LocalWallet` supports it.
- Avoid implementing `Clone` on `Wallet` unless strictly necessary; use `Arc<Wallet>` for shared ownership instead.

### Finding SEC-02: PrivateKeySigner Persists in PolymarketClient

| Attribute | Value |
|-----------|-------|
| **Severity** | High |
| **File** | `src/adapters/polymarket_clob.rs:43` |
| **Status** | Open |

The `PolymarketClient` stores `signer: Option<PrivateKeySigner>` as a field. The `PrivateKeySigner` (from the `alloy` crate) holds the private key for the lifetime of the client. The client also implements `Clone`, which clones the signer and its key material.

```rust
// src/adapters/polymarket_clob.rs:43
signer: Option<PrivateKeySigner>,
```

**Remediation:**
- Wrap the signer in `Arc` rather than cloning it directly.
- Audit all clone sites to ensure key material is not unnecessarily duplicated.

### Finding SEC-03: Deprecated private_key_hex() Returns Empty String

| Attribute | Value |
|-----------|-------|
| **Severity** | Low |
| **File** | `src/signing/wallet.rs:88-97` |
| **Status** | Informational |

The `private_key_hex()` method is correctly deprecated and returns an empty string. However, it still exists in the public API. Any caller relying on it will silently get an empty string, which could cause subtle bugs rather than a clear error.

**Remediation:**
- Remove the method entirely or change it to return `Result<!, PloyError>` (i.e., always error).

---

## 3. API Authentication and HMAC Signing

**Files reviewed:**
- `/Users/proerror/Documents/ploy/src/signing/hmac.rs`
- `/Users/proerror/Documents/ploy/src/signing/auth.rs`

### 3.1 HMAC-SHA256 Implementation (Good Practice)

The HMAC signing implementation in `hmac.rs` correctly uses the `hmac` and `sha2` crates with constant-time comparison (via `mac.finalize()`). The message construction follows the Polymarket CLOB API specification: `{timestamp}{METHOD}{path}{body}`.

### 3.2 EIP-712 Authentication (Good Practice)

The `auth.rs` file implements EIP-712 typed data signing for CLOB authentication correctly, with proper domain separator computation and struct hashing. The `\x19\x01` prefix is correctly applied.

### Finding SEC-04: ApiCredentials Derives Debug -- Secrets Leak to Logs

| Attribute | Value |
|-----------|-------|
| **Severity** | Critical |
| **File** | `src/signing/hmac.rs:11-16` |
| **Status** | Open |

The `ApiCredentials` struct derives `Debug` and `Clone`:

```rust
#[derive(Debug, Clone)]
pub struct ApiCredentials {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}
```

If this struct is ever passed to `tracing::debug!`, `format!("{:?}", creds)`, or appears in an error chain, the API secret and passphrase will be written to logs in plaintext. The `secret` field contains the base64-encoded HMAC secret, which is equivalent to a private key for API authentication.

**Remediation:**
- Implement a custom `Debug` that redacts sensitive fields:
  ```rust
  impl std::fmt::Debug for ApiCredentials {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
          f.debug_struct("ApiCredentials")
              .field("api_key", &format!("{}...", &self.api_key[..8.min(self.api_key.len())]))
              .field("secret", &"[REDACTED]")
              .field("passphrase", &"[REDACTED]")
              .finish()
      }
  }
  ```
- Apply `zeroize::Zeroize` to the `secret` and `passphrase` fields.
- Make fields private and provide accessor methods.

### Finding SEC-05: HMAC Debug Log Includes Full Signed Message

| Attribute | Value |
|-----------|-------|
| **Severity** | High |
| **File** | `src/signing/hmac.rs:106-113` |
| **Status** | Open |

The `build_headers` method logs the full HMAC message being signed at `debug` level:

```rust
tracing::debug!(
    "HMAC signing - timestamp: {}, method: {}, path: {}, message: '{}', address: {}",
    timestamp, method, path, message, self.address
);
```

The `message` variable contains the full request body (which may include order details, token IDs, and amounts). While this does not directly leak the secret, it leaks the exact message that was signed, which combined with the signature (sent in headers) could aid replay attacks if logs are compromised. More importantly, if the body contains sensitive data, it will appear in logs.

**Remediation:**
- Remove the `message` field from the debug log, or truncate it.
- Log only the path and method at debug level; log the full message only at trace level.

### Finding SEC-06: No Timestamp Validation on HMAC Signatures

| Attribute | Value |
|-----------|-------|
| **Severity** | Low |
| **File** | `src/signing/hmac.rs:61-66` |
| **Status** | Informational |

The HMAC timestamp is generated from `SystemTime::now()` with no clock skew protection. If the system clock is significantly wrong, signatures will be rejected by the server. The `expect()` on line 64 will panic if the system clock is before the UNIX epoch, though this is an extremely unlikely scenario.

**Remediation:**
- Consider adding a clock skew check (e.g., compare with an NTP source or the server's timestamp header on responses).

---

## 4. Order Signing and Submission Safety

**Files reviewed:**
- `/Users/proerror/Documents/ploy/src/signing/order.rs`
- `/Users/proerror/Documents/ploy/src/signing/nonce_manager.rs`
- `/Users/proerror/Documents/ploy/src/strategy/idempotency.rs`

### 4.1 EIP-712 Order Signing (Good Practice)

The order signing in `order.rs` correctly implements the Polymarket CTF Exchange EIP-712 typed data signing. The domain separator includes the verifying contract address, chain ID, name, and version. The struct hash includes all 12 order fields. Random salts are generated using `rand::thread_rng()`.

### 4.2 Nonce Management (Good Practice)

The `NonceManager` uses database-backed atomic nonce allocation via `SELECT get_next_nonce($1)`, preventing nonce collisions across restarts. Nonces can be marked as used or released, supporting crash recovery.

### 4.3 Idempotency (Good Practice)

The `IdempotencyManager` uses database-level `INSERT ... ON CONFLICT DO NOTHING` for atomic duplicate detection. Keys have a 1-hour TTL with periodic cleanup.

### Finding SEC-07: Orders Default to No Expiration

| Attribute | Value |
|-----------|-------|
| **Severity** | High |
| **File** | `src/signing/order.rs:127, 164` |
| **Status** | Open |

Both `OrderData::new_buy` and `OrderData::new_sell` set `expiration: U256::zero()`, which means orders never expire on-chain:

```rust
expiration: U256::zero(), // No expiration
```

If the bot crashes after submitting an order but before tracking it, the order will remain live on the exchange indefinitely. This is especially dangerous for limit orders at stale prices.

**Remediation:**
- Set a default expiration (e.g., 5-15 minutes from submission time).
- Make expiration configurable via `AppConfig`.
- At minimum, use the round end time as the expiration for arbitrage orders.

### Finding SEC-08: Idempotency Hash Does Not Include Time Component

| Attribute | Value |
|-----------|-------|
| **Severity** | Medium |
| **File** | `src/strategy/idempotency.rs:75-83` |
| **Status** | Open |

The `hash_request` fallback hashes only `token_id`, `shares`, `limit_price`, `market_side`, and `order_side`. It does not include a timestamp or nonce. Two legitimate orders with identical parameters submitted at different times would produce the same hash, potentially causing the second to be rejected as a duplicate.

```rust
fn hash_request(request: &OrderRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(request.token_id.as_bytes());
    hasher.update(&request.shares.to_le_bytes());
    hasher.update(request.limit_price.to_string().as_bytes());
    hasher.update(request.market_side.to_string().as_bytes());
    hasher.update(request.order_side.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}
```

Note: In practice, the primary key path uses `client_order_id` (which is unique per order), so this fallback is only triggered when both `idempotency_key` and `client_order_id` are empty. The risk is low but the design is fragile.

**Remediation:**
- Include a timestamp or the `client_order_id` in the hash.
- Alternatively, always require a non-empty `client_order_id`.

---

## 5. Strategy Engine Concurrency and State Safety

**Files reviewed:**
- `/Users/proerror/Documents/ploy/src/strategy/engine.rs`

### 5.1 Optimistic Locking (Good Practice)

The `StrategyEngine` uses a version counter in `EngineState` to detect concurrent modifications. Before and after each order execution, the engine checks that the version has not been modified by another task. If a mismatch is detected, the cycle is aborted and the circuit breaker is triggered. This is a strong defense against race conditions.

### 5.2 Execution Mutex (Good Practice)

A dedicated `execution_mutex: Mutex<()>` prevents concurrent order submissions, separate from the state `RwLock`. This ensures that only one order can be in-flight at a time, preventing double-spend scenarios.

### 5.3 State Re-validation (Good Practice)

Both `enter_leg1` and `enter_leg2` re-validate the strategy state under the write lock after acquiring the execution mutex and performing async checks. This prevents TOCTOU (time-of-check-time-of-use) bugs where the state could change between the initial check and the actual submission.

### Finding SEC-09: Best-Effort DB Persistence Could Lose Order Records

| Attribute | Value |
|-----------|-------|
| **Severity** | Medium |
| **File** | `src/strategy/engine.rs:576-601` |
| **Status** | Open |

After order execution, the engine persists order status updates with `let _ = self.store.update_order_status(...)`. The `let _ =` pattern silently discards database errors. If the database is temporarily unavailable, the order record will not reflect the actual exchange state.

While the engine does halt trading on critical DB failures (e.g., failing to persist the Leg1 order intent), the post-execution status updates are best-effort. This means the local database could show an order as "pending" when it has actually been filled on the exchange.

**Remediation:**
- Log database errors even when using best-effort persistence.
- Consider a retry mechanism for post-execution status updates.
- The reconciliation service partially mitigates this, but it only runs every 30 seconds.

### Finding SEC-10: confirm_fills Guard Only at Engine Construction

| Attribute | Value |
|-----------|-------|
| **Severity** | Medium |
| **File** | `src/strategy/engine.rs:79-83` |
| **Status** | Open |

The engine validates that `confirm_fills` is true when `dry_run` is false:

```rust
if !config.dry_run.enabled && !config.execution.confirm_fills {
    return Err(PloyError::Validation(
        "execution.confirm_fills must be true when dry_run.enabled is false".to_string(),
    ));
}
```

This is a good safety check, but it only runs at construction time. If the configuration is hot-reloaded (which the `config` crate supports), this invariant could be violated at runtime.

**Remediation:**
- Re-check this invariant at the start of each trading cycle, or make the config immutable after engine construction.

---

## 6. Circuit Breaker and Emergency Stop

**Files reviewed:**
- `/Users/proerror/Documents/ploy/src/coordination/circuit_breaker.rs`
- `/Users/proerror/Documents/ploy/src/coordination/emergency_stop.rs`

### 6.1 Circuit Breaker Pattern (Good Practice)

The `TradingCircuitBreaker` implements a proper three-state circuit breaker (Closed/Open/HalfOpen) with:
- Consecutive failure threshold triggering
- Daily loss limit enforcement
- Quote staleness detection
- WebSocket disconnect detection
- HalfOpen trade count and exposure limits
- Automatic recovery timeout from Open to HalfOpen

The half-open counters are correctly reset on both `trip()` and `transition_to_half_open()`, preventing stale counter values from a previous half-open period.

### 6.2 Emergency Stop (Good Practice)

The `EmergencyStopManager` uses an `AtomicBool` for fast lock-free checking (`is_stopped()`), with parallel order cancellation and configurable timeouts. Emergency state is persisted to the database and loaded on startup, preventing the bot from resuming trading after a crash during an emergency.

### Finding SEC-11: Emergency Stop Relaxed Ordering on is_stopped Check

| Attribute | Value |
|-----------|-------|
| **Severity** | Low |
| **File** | `src/coordination/emergency_stop.rs:133` |
| **Status** | Open |

The `is_stopped()` method uses `Ordering::Relaxed`:

```rust
pub fn is_stopped(&self) -> bool {
    self.is_stopped.load(Ordering::Relaxed)
}
```

While `trigger()` uses `Ordering::SeqCst` to set the flag, the read side uses `Relaxed`, which means a thread could theoretically observe a stale `false` value for a brief period after the flag is set. In practice, this is unlikely to cause issues on x86 architectures, but on ARM (e.g., Apple Silicon where this bot runs on macOS), the window could be slightly larger.

**Remediation:**
- Change to `Ordering::Acquire` for the load to pair with the `SeqCst` store, ensuring the stop flag is visible promptly across all threads.

---

## 7. LLM Agent Security

**Files reviewed:**
- `/Users/proerror/Documents/ploy/src/ai_clients/autonomous.rs`
- `/Users/proerror/Documents/ploy/src/ai_clients/grok.rs`
- `/Users/proerror/Documents/ploy/src/ai_clients/client.rs`

### 7.1 Prompt Injection Sanitization (Good Practice)

The `AutonomousAgent::sanitize_for_prompt` method strips control characters and truncates external input to 500 characters before embedding it in LLM prompts:

```rust
fn sanitize_for_prompt(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(500)
        .collect()
}
```

This is applied to Grok search results (sentiment and key points) before they are included in the analysis prompt. This mitigates prompt injection from attacker-controlled market data or social media content.

### 7.2 Rate Limiting (Good Practice)

Both the `GrokClient` and `AutonomousAgent` implement rate limiting:
- `GrokClient`: 6-second minimum interval between API calls (~10 req/min)
- `AutonomousAgent`: 60-second cooldown on Grok searches
- `GrokClient`: 30-second HTTP timeout on all requests

### 7.3 Autonomy Levels (Good Practice)

The agent defaults to `AdvisoryOnly` mode with trading disabled. The `validate_actions` method enforces trade size limits, total exposure limits, confidence thresholds, and only allows risk increases (not decreases) from the LLM.

### Finding SEC-12: Grok API Key Stored in Plaintext Config Struct

| Attribute | Value |
|-----------|-------|
| **Severity** | Medium |
| **File** | `src/ai_clients/grok.rs:18-28` |
| **Status** | Open |

The `GrokConfig` struct stores the API key as a plain `String` with no zeroization or redaction:

```rust
pub struct GrokConfig {
    pub api_key: String,
    pub base_url: String,
    pub timeout_secs: u64,
    pub model: String,
}
```

The struct does derive `Debug`, which means the API key could appear in logs if the config is ever debug-printed. The key is also sent in the `Authorization` header on every request.

**Remediation:**
- Implement a custom `Debug` that redacts `api_key`.
- Consider wrapping `api_key` in a `secrecy::Secret<String>` type.

### Finding SEC-13: Autonomous Agent Exposure Tracking Not Synchronized with Exchange

| Attribute | Value |
|-----------|-------|
| **Severity** | Medium |
| **File** | `src/ai_clients/autonomous.rs:565` |
| **Status** | Open |

The `execute_action` method for `EnterPosition` updates an in-memory exposure counter but does not actually submit orders to the exchange (marked with `// TODO: Integrate with actual order executor`):

```rust
// TODO: Integrate with actual order executor
// For now, just track the exposure
let trade_value = Decimal::from(*shares) * *max_price;
*self.current_exposure.write().await += trade_value;
```

Similarly, `ExitPosition` reduces exposure by a fixed 20% rather than the actual position value. This means the exposure tracking is fictional and does not reflect real positions. If the TODO is completed without updating the exposure logic, the agent could exceed its configured limits.

**Remediation:**
- When integrating with the real order executor, ensure exposure tracking is based on actual fill confirmations, not order intent.
- Add integration tests that verify exposure limits are enforced end-to-end.

---

## 8. Shutdown and Crash Recovery

**Files reviewed:**
- `/Users/proerror/Documents/ploy/src/coordination/shutdown.rs`
- `/Users/proerror/Documents/ploy/src/persistence/checkpoint.rs`

### 8.1 Graceful Shutdown Sequencing (Good Practice)

The `GracefulShutdown` implements a well-structured six-phase shutdown sequence:
1. Stop new order acceptance
2. Drain pending orders (with configurable timeout)
3. Create final checkpoint
4. Close WebSocket connections
5. Flush database
6. Close connections

Each phase has its own timeout, and the total shutdown has a 120-second hard timeout. The `wait_for_completion` method includes a deadlock prevention check -- it returns immediately if the phase is already `Complete`.

### 8.2 Checkpoint Service (Good Practice)

The `CheckpointService` provides periodic state snapshots via the `Checkpointable` trait. Components implement `to_checkpoint()` and `from_checkpoint()` for serialization/deserialization. Checkpoints include a version number for optimistic locking during restore.

### Finding SEC-14: ShutdownToken Captures Stale Snapshot

| Attribute | Value |
|-----------|-------|
| **Severity** | Low |
| **File** | `src/coordination/shutdown.rs:306-312` |
| **Status** | Open |

The `ShutdownToken::is_shutdown_requested()` method returns a boolean that was captured at token creation time, not the current value:

```rust
pub struct ShutdownToken {
    shutdown_requested: bool,  // Snapshot, not live reference
    signal_rx: broadcast::Receiver<ShutdownSignal>,
    phase_rx: watch::Receiver<ShutdownPhase>,
}
```

If a token is created before shutdown is requested, `is_shutdown_requested()` will always return `false` even after shutdown begins. Callers must use `wait_for_shutdown()` or check `current_phase()` for live status.

**Remediation:**
- Store an `Arc<AtomicBool>` reference instead of a snapshot, or document this limitation clearly.

### Finding SEC-15: Checkpoint Periodic Task Detached Without Join Handle

| Attribute | Value |
|-----------|-------|
| **Severity** | Low |
| **File** | `src/persistence/checkpoint.rs:173` |
| **Status** | Open |

The `CheckpointService::start` method spawns a background task with `tokio::spawn` but does not return or store the `JoinHandle`. If the task panics, the panic will be silently swallowed and checkpointing will stop without any notification.

**Remediation:**
- Store the `JoinHandle` and check it periodically, or use a supervised task pattern that restarts on panic.

---

## 9. Fund Management and Reconciliation

**Files reviewed:**
- `/Users/proerror/Documents/ploy/src/strategy/fund_manager.rs`

### 9.1 Fund Manager Balance Caching (Good Practice)

The `FundManager` implements a TTL-based balance cache with a 10-second expiry. This prevents excessive API calls while ensuring reasonably fresh balance data. The cache is invalidated on position open/close events.

### 9.2 Multi-Layer Position Limits (Good Practice)

The `can_open_position` method enforces multiple safety checks in sequence:
1. Maximum total positions limit
2. Duplicate event detection
3. Per-symbol position limit
4. Minimum balance requirement
5. Per-symbol allocation (dynamic fund distribution)
6. Maximum single exposure limit
7. Minimum order size (5 shares, $1 value)

### Finding SEC-16: Legacy Reconciliation Path Relied on Unimplemented get_positions()

| Attribute | Value |
|-----------|-------|
| **Severity** | Medium |
| **File** | `src/adapters/polymarket_clob.rs:1271-1280` |
| **Status** | Open |

The retired reconciliation path called `self.client.get_positions()` to fetch exchange balances. However, the `PolymarketClient::get_positions()` method is a stub that always returns an empty vector:

```rust
pub async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
    if self.dry_run {
        return Ok(vec![]);
    }
    // The SDK doesn't have a direct positions endpoint
    warn!("get_positions not fully implemented with SDK");
    Ok(vec![])
}
```

This means the reconciliation service will always see zero exchange positions, causing every local position to be flagged as a critical discrepancy (local > 0, exchange = 0). In practice, this likely means the reconciliation service is either not running or its results are being ignored.

**Remediation:**
- Implement `get_positions()` using the Polymarket REST API or derive positions from trade history.
- Until implemented, disable the reconciliation service or gate it behind a feature flag to avoid false-positive critical alerts flooding the logs.

---

## 10. Summary of Findings

### Findings by Severity

| ID | Severity | Title | File |
|----|----------|-------|------|
| SEC-04 | **Critical** | ApiCredentials derives Debug -- secrets leak to logs | `src/signing/hmac.rs:11` |
| SEC-01 | High | LocalWallet retains private key in memory | `src/signing/wallet.rs:14` |
| SEC-02 | High | PrivateKeySigner persists in PolymarketClient | `src/adapters/polymarket_clob.rs:43` |
| SEC-05 | High | HMAC debug log includes full signed message | `src/signing/hmac.rs:106` |
| SEC-07 | High | Orders default to no expiration | `src/signing/order.rs:127` |
| SEC-08 | Medium | Idempotency hash does not include time component | `src/strategy/idempotency.rs:75` |
| SEC-09 | Medium | Best-effort DB persistence could lose order records | `src/strategy/engine.rs:576` |
| SEC-10 | Medium | confirm_fills guard only at engine construction | `src/strategy/engine.rs:79` |
| SEC-12 | Medium | Grok API key stored in plaintext config struct | `src/ai_clients/grok.rs:18` |
| SEC-13 | Medium | Autonomous agent exposure tracking not synchronized | `src/ai_clients/autonomous.rs:565` |
| SEC-16 | Medium | Reconciliation relies on unimplemented get_positions() | `src/adapters/polymarket_clob.rs:1271` |
| SEC-03 | Low | Deprecated private_key_hex() returns empty string | `src/signing/wallet.rs:88` |
| SEC-06 | Low | No timestamp validation on HMAC signatures | `src/signing/hmac.rs:61` |
| SEC-11 | Low | Emergency stop Relaxed ordering on is_stopped check | `src/coordination/emergency_stop.rs:133` |
| SEC-14 | Low | ShutdownToken captures stale snapshot | `src/coordination/shutdown.rs:306` |
| SEC-15 | Low | Checkpoint periodic task detached without join handle | `src/persistence/checkpoint.rs:173` |

### Priority Remediation Roadmap

**Immediate (before next production deployment):**

1. **SEC-04 (Critical):** Implement custom `Debug` for `ApiCredentials` that redacts `secret` and `passphrase`. This is a one-line change that prevents credential leakage to any log sink.

2. **SEC-07 (High):** Add a default order expiration of 5-15 minutes. Stale orders on the exchange after a crash represent unbounded financial risk.

3. **SEC-05 (High):** Remove or redact the `message` field from the HMAC debug log in `build_headers()`.

**Short-term (within 1-2 weeks):**

4. **SEC-01/SEC-02 (High):** Restrict `Wallet::inner()` to `pub(crate)`, wrap `PrivateKeySigner` in `Arc` to prevent key material duplication on clone.

5. **SEC-16 (Medium):** Implement `get_positions()` or disable the reconciliation service. A reconciliation service that always reports false positives is worse than no reconciliation at all.

6. **SEC-12 (Medium):** Add custom `Debug` for `GrokConfig` to redact the API key.

**Medium-term (within 1 month):**

7. **SEC-09 (Medium):** Add retry logic or at minimum error logging for post-execution DB status updates.

8. **SEC-13 (Medium):** When integrating the autonomous agent with the real order executor, ensure exposure tracking is based on confirmed fills.

9. **SEC-08 (Medium):** Include a timestamp in the idempotency hash fallback path.

10. **SEC-10 (Medium):** Re-validate the `confirm_fills` invariant at the start of each trading cycle.

### Positive Security Patterns Observed

The codebase demonstrates mature security engineering in several areas that deserve recognition:

- **Private key zeroization** (`wallet.rs`, `polymarket_clob.rs`): Hex string copies are zeroized immediately after parsing.
- **Custom Debug for Wallet**: The `Wallet` struct implements a custom `Debug` that only shows the address and chain ID, never the key material.
- **Optimistic locking in the strategy engine**: Version counters detect concurrent state modifications and trigger circuit breakers.
- **Execution mutex**: Prevents concurrent order submissions, eliminating double-spend race conditions.
- **EIP-712 typed data signing**: Proper domain separation for both authentication and order signing.
- **Prompt injection sanitization**: External data is stripped of control characters and truncated before LLM prompt embedding.
- **Default-safe autonomy**: The autonomous agent defaults to `AdvisoryOnly` with trading disabled.
- **Circuit breaker with HalfOpen limits**: Trade count and exposure caps during recovery prevent cascading failures.
- **Emergency stop persistence**: Emergency state survives process restarts via database persistence.
- **Graceful shutdown with deadlock prevention**: Phase-based shutdown with timeout guards at every stage.

---

*End of Security Audit Report*
