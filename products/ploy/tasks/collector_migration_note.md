# Collector migration note: PM orderbook history -> canonical snapshots

## Context

`OrderbookHistoryCollector` now writes:

- legacy table: `clob_orderbook_history_ticks`
- canonical table: `clob_orderbook_snapshots`

Legacy writes are retained for compatibility; new readers should migrate to canonical.

## Column mapping

- `clob_orderbook_history_ticks.token_id` -> `clob_orderbook_snapshots.token_id`
- `clob_orderbook_history_ticks.condition_id` -> `clob_orderbook_snapshots.market` (current bridge mapping)
- `clob_orderbook_history_ticks.book_ts` -> `clob_orderbook_snapshots.book_timestamp`
- `clob_orderbook_history_ticks.hash` -> `clob_orderbook_snapshots.hash`
- `clob_orderbook_history_ticks.bids` -> `clob_orderbook_snapshots.bids`
- `clob_orderbook_history_ticks.asks` -> `clob_orderbook_snapshots.asks`
- source marker -> `clob_orderbook_snapshots.source = 'polymarket_orderbook_history'` (default collector config)

## Recommended read filter

Prefer reading canonical with:

- `source = 'polymarket_orderbook_history'` for history-backfilled snapshots, or
- no source filter if you intentionally want merged WS + history data.

## Backward-compatibility plan

1. Keep both tables during transition.
2. Move analytics/backtest queries to canonical table.
3. Remove legacy table reads once all consumers are migrated.

## Sync records migration (derived path)

- New derived view: `sync_records_derived`
  - Source: canonical raw tables (`binance_lob_ticks` + `clob_quote_ticks`)
  - Mapping aid: `collector_token_targets.metadata` (`symbol`, `slug`, `side`)
- `SyncCollector` now writes raw tables as primary sink.
- Legacy `sync_records` writes are compatibility-only and disabled by default.
  - Enable only if needed: `PLOY_COLLECTOR_PERSIST_SYNC_RECORDS=true`

## Legacy `ticks` pathway

- `services/data_collector` now writes canonical `clob_quote_ticks` by default.
- Legacy `ticks` table writes are deprecated and disabled by default.
  - Re-enable only for compatibility: `PLOY_LEGACY_TICKS_ENABLED=true`

## Legacy CSV pathway

- `collector/backtest_collector` no longer writes CSV by default.
- CSV output is now compatibility-only:
  - `CollectorConfig.persist_csv = true` to re-enable.
