-- Database Data Completeness Check for pm_5m_directional backtest

\echo '=== Table Existence Check ==='
SELECT
    table_name,
    CASE WHEN EXISTS (SELECT FROM information_schema.tables WHERE table_name = t.table_name)
         THEN 'EXISTS'
         ELSE 'MISSING'
    END as status
FROM (VALUES
    ('sync_records'),
    ('binance_price_ticks'),
    ('clob_quote_ticks'),
    ('pm_market_metadata'),
    ('binance_lob_ticks')
) AS t(table_name);

\echo ''
\echo '=== binance_price_ticks - Per Symbol Coverage ==='
SELECT
    symbol,
    COUNT(*) as row_count,
    MIN(trade_time) as earliest,
    MAX(trade_time) as latest,
    MAX(trade_time) - MIN(trade_time) as duration
FROM binance_price_ticks
WHERE symbol IN ('BTCUSDT', 'ETHUSDT', 'SOLUSDT', 'XRPUSDT', 'DOGEUSDT', 'HYPEUSDT', 'BNBUSDT')
GROUP BY symbol
ORDER BY symbol;

\echo ''
\echo '=== clob_quote_ticks - Overall Coverage ==='
SELECT
    COUNT(*) as total_rows,
    MIN(received_at) as earliest,
    MAX(received_at) as latest,
    MAX(received_at) - MIN(received_at) as duration,
    COUNT(DISTINCT token_id) as unique_tokens
FROM clob_quote_ticks;

\echo ''
\echo '=== pm_market_metadata - Market Windows ==='
SELECT
    COUNT(*) as total_markets,
    MIN(start_time) as earliest_start,
    MAX(end_time) as latest_end,
    COUNT(CASE WHEN end_time > NOW() THEN 1 END) as active_markets
FROM pm_market_metadata;

\echo ''
\echo '=== binance_lob_ticks - L2 Orderbook Data (Optional) ==='
SELECT
    symbol,
    COUNT(*) as row_count,
    MIN(event_time) as earliest,
    MAX(event_time) as latest
FROM binance_lob_ticks
WHERE symbol IN ('BTCUSDT', 'ETHUSDT', 'SOLUSDT', 'XRPUSDT', 'DOGEUSDT', 'HYPEUSDT', 'BNBUSDT')
GROUP BY symbol
ORDER BY symbol;

\echo ''
\echo '=== Data Quality - Check for Gaps ==='
\echo 'Checking for 1-hour gaps in binance_price_ticks...'
WITH time_gaps AS (
    SELECT
        symbol,
        trade_time,
        LAG(trade_time) OVER (PARTITION BY symbol ORDER BY trade_time) as prev_timestamp,
        trade_time - LAG(trade_time) OVER (PARTITION BY symbol ORDER BY trade_time) as gap
    FROM binance_price_ticks
    WHERE symbol IN ('BTCUSDT', 'ETHUSDT', 'SOLUSDT')
)
SELECT
    symbol,
    COUNT(*) as gaps_over_1hour,
    MAX(gap) as max_gap
FROM time_gaps
WHERE gap > INTERVAL '1 hour'
GROUP BY symbol
ORDER BY symbol;

\echo ''
\echo '=== Recommended Backtest Date Range ==='
\echo 'Based on overlapping data availability across all sources'
SELECT
    GREATEST(
        (SELECT MIN(trade_time) FROM binance_price_ticks WHERE symbol = 'BTCUSDT'),
        (SELECT MIN(received_at) FROM clob_quote_ticks),
        (SELECT MIN(start_time) FROM pm_market_metadata)
    ) as recommended_start,
    LEAST(
        (SELECT MAX(trade_time) FROM binance_price_ticks WHERE symbol = 'BTCUSDT'),
        (SELECT MAX(received_at) FROM clob_quote_ticks),
        (SELECT MAX(end_time) FROM pm_market_metadata WHERE end_time < NOW())
    ) as recommended_end;
