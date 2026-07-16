# Kalshi Full-Parity Design

## Context

The codebase currently couples order execution and market data to `PolymarketClient`.
Kalshi support requires an exchange-agnostic contract while preserving current Polymarket behavior.

## Goals

1. Add a unified exchange abstraction for order execution and account/market queries.
2. Add a native Rust Kalshi adapter compatible with existing order-domain models.
3. Keep Polymarket as the default runtime and maintain backward compatibility.

## Non-Goals (This PR Series)

1. Full migration of every strategy to Kalshi-specific market identifiers.
2. Replacing all Polymarket-specific agent logic in one change.
3. Cross-exchange portfolio netting or universal event discovery semantics.

## Architecture

1. Introduce `src/exchange/traits.rs` with `ExchangeClient` + `ExchangeKind`.
2. Implement `ExchangeClient` for `PolymarketClient`.
3. Add `KalshiClient` (`src/adapters/kalshi_rest.rs`) with normalized response mapping.
4. Refactor `OrderExecutor` and `OrderPlatform` to accept `Arc<dyn ExchangeClient>`
   while preserving existing `new(PolymarketClient, ...)` constructors.
5. Extend config with:
   - `execution.exchange`
   - `kalshi.base_url/api_key/api_secret`
   - optional `market.exchange_rest_url` / `market.exchange_ws_url`

## Risks and Mitigations

1. API schema drift on Kalshi endpoints.
   - Mitigation: tolerant JSON parsing and default fallbacks.
2. Authentication mismatch for signing scheme.
   - Mitigation: auth logic isolated in `KalshiClient::auth_headers`.
3. Runtime regressions in existing Polymarket flow.
   - Mitigation: preserve default constructors and dry-run behavior.
