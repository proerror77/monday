-- db_retention.sql — Tick table retention policy
-- clob_trade_ticks: 7 days, clob_quote_ticks: 7 days, deribit_iv_ticks: 7 days
-- Uses batched deletes (10k rows per batch) to avoid long locks.
-- Run daily via cron at 04:00.

\set ON_ERROR_STOP on
\timing on

-- ============================================================
-- 1. clob_trade_ticks — retain 7 days (batched)
-- ============================================================
DO $retention$
DECLARE
  batch_size  CONSTANT int := 10000;
  deleted     int;
  total       bigint := 0;
BEGIN
  RAISE NOTICE '=== clob_trade_ticks: deleting rows older than 7 days ===';
  LOOP
    DELETE FROM clob_trade_ticks
    WHERE id IN (
      SELECT id FROM clob_trade_ticks
      WHERE trade_ts < now() - interval '7 days'
      LIMIT batch_size
    );
    GET DIAGNOSTICS deleted = ROW_COUNT;
    total := total + deleted;
    EXIT WHEN deleted < batch_size;
  END LOOP;
  RAISE NOTICE 'clob_trade_ticks: deleted % rows', total;
END
$retention$;

VACUUM VERBOSE clob_trade_ticks;

-- ============================================================
-- 2. clob_quote_ticks — retain 7 days (batched)
-- ============================================================
DO $retention$
DECLARE
  batch_size  CONSTANT int := 10000;
  deleted     int;
  total       bigint := 0;
BEGIN
  RAISE NOTICE '=== clob_quote_ticks: deleting rows older than 7 days ===';
  LOOP
    DELETE FROM clob_quote_ticks
    WHERE id IN (
      SELECT id FROM clob_quote_ticks
      WHERE received_at < now() - interval '7 days'
      LIMIT batch_size
    );
    GET DIAGNOSTICS deleted = ROW_COUNT;
    total := total + deleted;
    EXIT WHEN deleted < batch_size;
  END LOOP;
  RAISE NOTICE 'clob_quote_ticks: deleted % rows', total;
END
$retention$;

VACUUM VERBOSE clob_quote_ticks;

-- ============================================================
-- 3. deribit_iv_ticks — retain 7 days (batched)
-- ============================================================
DO $retention$
DECLARE
  batch_size  CONSTANT int := 10000;
  deleted     int;
  total       bigint := 0;
BEGIN
  RAISE NOTICE '=== deribit_iv_ticks: deleting rows older than 7 days ===';
  LOOP
    DELETE FROM deribit_iv_ticks
    WHERE id IN (
      SELECT id FROM deribit_iv_ticks
      WHERE fetched_at < now() - interval '7 days'
      LIMIT batch_size
    );
    GET DIAGNOSTICS deleted = ROW_COUNT;
    total := total + deleted;
    EXIT WHEN deleted < batch_size;
  END LOOP;
  RAISE NOTICE 'deribit_iv_ticks: deleted % rows', total;
END
$retention$;

VACUUM VERBOSE deribit_iv_ticks;
