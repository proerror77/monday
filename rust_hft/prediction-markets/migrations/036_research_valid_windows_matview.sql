-- Migration 036: Materialized view for factor research valid windows
--
-- Pre-computes the expensive 3-way join (pm_market_metadata × pm_token_settlements
-- × binance_lob_ticks) so factor_research --discover-valid-5m-windows runs in
-- milliseconds instead of minutes.
--
-- Refresh: ploy_maintenance.sh runs REFRESH MATERIALIZED VIEW CONCURRENTLY every 6h.

CREATE MATERIALIZED VIEW IF NOT EXISTS research_valid_windows AS
SELECT
    m.symbol,
    m.start_time,
    m.end_time,
    m.market_slug
FROM pm_market_metadata m
WHERE m.symbol IS NOT NULL
  AND m.start_time IS NOT NULL
  AND m.end_time IS NOT NULL
  AND EXTRACT(EPOCH FROM (m.end_time - m.start_time)) = 300
  AND EXISTS (
      SELECT 1
      FROM pm_token_settlements s
      WHERE s.market_slug = m.market_slug
        AND s.resolved = true
  )
  AND EXISTS (
      SELECT 1
      FROM binance_lob_ticks l
      WHERE l.symbol = m.symbol
        AND l.event_time >= m.start_time
        AND l.event_time <= m.end_time
  );

-- UNIQUE index required for REFRESH MATERIALIZED VIEW CONCURRENTLY.
CREATE UNIQUE INDEX IF NOT EXISTS idx_research_valid_windows_pk
    ON research_valid_windows(symbol, start_time, end_time);

CREATE INDEX IF NOT EXISTS idx_research_valid_windows_start
    ON research_valid_windows(start_time);
