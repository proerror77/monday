# Polymarket API Audit (2026-02-19)

Scope: verify that this project is using Polymarket APIs according to official docs and SDK behavior.

Local static check command:

- `scripts/check_polymarket_api_usage.sh`

## Official Sources Checked

- Market data fetching: <https://docs.polymarket.com/market-data/fetching-markets>
- Gamma markets/events pagination guide: <https://docs.polymarket.com/developers/gamma-markets-api/fetch-markets-guide>
- Quickstart: <https://docs.polymarket.com/quickstart>
- CLOB L2 client: <https://docs.polymarket.com/trading/clients/l2>
- CLOB active/open orders: <https://docs.polymarket.com/developers/CLOB/orders/get-active-order>
- CLOB trades: <https://docs.polymarket.com/developers/CLOB/trades/trades>
- CTF redeem: <https://docs.polymarket.com/trading/ctf/redeem>
- Data API positions: <https://docs.polymarket.com/developers/misc-endpoints/data-api-get-positions>
- Data API positions endpoint (SDK API reference): <https://docs.polymarket.com/developers/data-api/fetch-all-positions>

## Conformance Matrix

- Gamma discovery (`events`, `markets`, `series`): typed SDK calls are primary in `src/adapters/polymarket_clob.rs` (`GammaClient::{markets,event_by_id,series_by_id,search}`); status: `PASS`.
- Gamma pagination (`limit` / `offset`): Data/agent paths now use standard `limit`/`offset` form (removed `_limit` usage in trading-facing modules); status: `PASS`.
- CLOB L2 orders/trades pagination (`next_cursor`): implemented in `fetch_orders_paginated` and `fetch_trades_paginated`; status: `PASS`.
- CLOB cancel-market-orders scoping (`asset_id`/market scoping): `cancel_all_orders(token_id)` now uses `CancelMarketOrderRequest.asset_id`; status: `PASS`.
- Data API positions (`user`, `limit`, `offset`, optional flags): implemented with typed `PositionsRequest` and pagination; status: `PASS`.
- CTF redeem (`redeemPositions(collateral, 0x0, conditionId, [1,2])`): implemented in `src/strategy/claimer.rs`; status: `PASS`.

## Code-to-API Mapping

- Gamma market/event discovery
  - `src/adapters/polymarket_clob.rs`
  - Uses `GammaClient::markets`, `GammaClient::event_by_id`, `GammaClient::series_by_id`, `GammaClient::search`
- CLOB L2 trading
  - `src/adapters/polymarket_clob.rs`
  - Uses authenticated SDK flow for `post_order`, `order`, `orders`, `trades`, `cancel_order`, `cancel_market_orders`, `balance_allowance`
- Data API positions
  - `src/adapters/polymarket_clob.rs`
  - Uses typed SDK `DataClient::positions` with official query params and pagination
- CTF redeem
  - `src/strategy/claimer.rs`
  - Uses `ConditionalTokens.redeemPositions(collateral, 0x0 parent, conditionId, [1,2])` as documented

## Gaps Found And Fixed

1. Positions retrieval used ad-hoc HTTP call with only `user` query.
   - Fixed to use typed official Data API SDK request (`PositionsRequest`) and page through `limit/offset`.

2. Order/trade history only read the first CLOB page.
   - Fixed by following `next_cursor` pagination until terminal cursor.

3. `cancel_all_orders(token_id)` canceled account-wide orders.
   - Fixed to call token-scoped `cancel_market_orders` with `asset_id=token_id`.

4. Auto-claim execution stability.
   - Already switched to ConditionalTokens redeem path (not exchange call).
   - Added wallet gas preflight and safer redeemable filtering logic in `src/strategy/claimer.rs`.
   - Wired live momentum runtime to attach `AutoClaimer` automatically (default on in live mode, env-toggleable).

5. Duplicate event entry risk.
   - Added entry mutex and condition-level duplicate checks in `src/strategy/momentum.rs`.
6. Position risk tracking drift in momentum engine.
   - Fixed `FundManager` integration to record actual notional exposure on entry/exit.
   - Fixed resolution/exit paths to release tracked notional exposure.
   - Added confidence + Kelly-based share scaling (`MomentumConfig.use_kelly_sizing`, `kelly_fraction_cap`) to align live sizing with edge quality.
7. Drawdown reporting format dependency.
   - `scripts/report_drawdown.py` now supports SQLite output (`reports/drawdown.sqlite`) in addition to JSON.
   - This avoids CSV dependency and supports queryable, per-strategy/per-product/per-timeframe analysis.
8. SDK/type safety for series event discovery.
   - Replaced manual `reqwest` `/series/{id}` parsing with typed `GammaClient::series_by_id` in `src/adapters/polymarket_clob.rs`.
9. Legacy Gamma query parameter style in strategy/agent paths.
   - Replaced `_limit` with `limit` (plus `offset`) in `src/strategy/event_edge/mod.rs` and `src/ai_clients/sports_analyst.rs`.
10. Trade collector using raw Data API HTTP endpoint.
   - Replaced raw `GET https://data-api.polymarket.com/trades` calls in `src/coordinator/bootstrap.rs` with typed SDK `DataClient::trades(TradesRequest)`.
   - Kept pagination/high-water-mark/overlap behavior, and enforced official SDK offset bound (`<= 10000`).
11. Sports/Event-edge raw Gamma discovery paths.
   - Replaced raw Gamma `/events` and `/markets` HTTP calls in `src/ai_clients/sports_analyst.rs` and `src/strategy/event_edge/mod.rs` with typed SDK search/event flows.
   - Added static audit guard in `scripts/check_polymarket_api_usage.sh` to prevent regression in these SDK-migrated modules.
12. Domain-level portfolio risk split and hard-stop hooks.
   - Added domain exposure/day-loss controls in `src/platform/risk.rs`:
     - per-domain exposure caps (`crypto_max_exposure`, `sports_max_exposure`, ...)
     - per-domain daily loss caps (`crypto_daily_loss_limit`, `sports_daily_loss_limit`, ...)
   - Coordinator now registers agent domain into risk gate, enabling domain-scoped checks.
   - Bootstrap exposes env-driven split controls (e.g. `PLOY_RISK__CRYPTO_ALLOCATION_PCT`, `PLOY_RISK__SPORTS_ALLOCATION_PCT`).
13. Historical raw Gamma reads in helper/CLI paths.
   - Earlier cleanup migrated helper paths such as `sports_analyst_enhanced.rs`, `live_arbitrage.rs`, and `main.rs::fetch_market_snapshot` from raw HTTP Gamma URLs to typed `GammaClient` requests (`SearchRequest`, `SeriesByIdRequest`, `EventByIdRequest`).
   - The regression guard work remains relevant even though some of those helper modules were later retired.
14. Historical sports/politics integration modules previously used raw Gamma URLs.
   - The old migration covered the sports/politics market helpers now living under `src/ai_clients/`; the politics helper was later retired during the March 2026 cleanup.
   - The remaining sports-side integrations stay under the static audit regression guard.

## Remaining TODO For “Production-Complete” State

- Add integration tests against sandbox keys for:
  - multi-page orders/trades
  - token-scoped cancel behavior
  - redeem path with resolved test market
- Consolidate remaining raw-HTTP Polymarket calls in non-core helper modules to typed SDK wrappers where available.
- Add periodic API health checks with alerting for:
  - `clob`, `gamma`, `data-api` latency/error rates
- Add structured reconciliation job:
  - exchange open orders vs DB intents vs local strategy state
