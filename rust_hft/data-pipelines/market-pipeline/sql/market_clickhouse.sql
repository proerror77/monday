-- Offline migration; this pilot uses one immutable batch per partition.
-- PG limits the admitted writer; operators must set a bounded partition budget
-- before widening scope. This is not a full-bucket partitioning design.
CREATE DATABASE market_data;
CREATE TABLE market_data.events_v1 (
  batch_id FixedString(64),
  venue LowCardinality(String),
  market LowCardinality(String),
  instrument String,
  session String,
  normalizer_sha256 FixedString(64),
  source_sha256 FixedString(64),
  event_kind LowCardinality(String),
  ordinal UInt64,
  source_row UInt64,
  source_received_ns UInt64,
  available_ns UInt64,
  exchange_event_ns Nullable(UInt64),
  transaction_ns Nullable(UInt64),
  first_update_id Nullable(UInt64),
  final_update_id Nullable(UInt64),
  previous_update_id Nullable(UInt64),
  aggregate_trade_id Nullable(UInt64),
  first_trade_id Nullable(UInt64),
  last_trade_id Nullable(UInt64),
  price Nullable(Decimal(18,8)),
  quantity Nullable(Decimal(18,8)),
  is_buyer_maker Nullable(Bool),
  bid_price Array(Decimal(18,8)),
  bid_quantity Array(Decimal(18,8)),
  ask_price Array(Decimal(18,8)),
  ask_quantity Array(Decimal(18,8))
) ENGINE=MergeTree
PARTITION BY batch_id
ORDER BY (venue,market,instrument,available_ns,ordinal)
SETTINGS non_replicated_deduplication_window=0;
