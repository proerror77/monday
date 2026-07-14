-- Migration: 034_clob_quote_ticks_full_resolution
-- Description: Drop per-second dedup index on clob_quote_ticks so every tick is
--              stored at full resolution.  Backtest replay needs sub-second data
--              to faithfully reproduce the live feed.

DROP INDEX IF EXISTS uq_clob_quote_ticks_token_second;
