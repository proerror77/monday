-- Backfill price_to_beat from Binance price data
-- Strategy: For each market's start_time, find closest Binance price within ±5 seconds

-- Step 1: Create temporary table with matched prices
CREATE TEMP TABLE temp_price_matches AS
SELECT DISTINCT ON (m.market_slug)
    m.market_slug,
    m.symbol,
    m.start_time,
    b.price as binance_price,
    b.trade_time,
    ABS(EXTRACT(EPOCH FROM (m.start_time - b.trade_time))) as time_diff_seconds
FROM pm_market_metadata m
JOIN binance_price_ticks b ON (
    b.symbol = m.symbol
    AND b.trade_time BETWEEN m.start_time - INTERVAL '5 seconds'
                         AND m.start_time + INTERVAL '5 seconds'
)
WHERE m.market_slug LIKE '%-updown-5m-%'
  AND m.price_to_beat IS NULL
  AND m.start_time < NOW()  -- Only backfill past markets
ORDER BY m.market_slug, time_diff_seconds ASC;

-- Step 2: Show statistics before update
SELECT
    'Before update' as stage,
    COUNT(*) as markets_to_update,
    MIN(start_time) as earliest_market,
    MAX(start_time) as latest_market
FROM temp_price_matches;

-- Step 3: Update pm_market_metadata
UPDATE pm_market_metadata m
SET price_to_beat = t.binance_price
FROM temp_price_matches t
WHERE m.market_slug = t.market_slug;

-- Step 4: Show statistics after update
SELECT
    symbol,
    COUNT(*) as total_markets,
    COUNT(price_to_beat) as has_price_to_beat,
    ROUND(100.0 * COUNT(price_to_beat) / COUNT(*), 2) as coverage_pct
FROM pm_market_metadata
WHERE market_slug LIKE '%-updown-5m-%'
  AND end_time > NOW() - INTERVAL '7 days'
GROUP BY symbol
ORDER BY symbol;

-- Step 5: Show sample of updated markets
SELECT
    market_slug,
    symbol,
    start_time,
    price_to_beat,
    'backfilled' as source
FROM pm_market_metadata
WHERE market_slug LIKE '%-updown-5m-%'
  AND price_to_beat IS NOT NULL
  AND market_slug IN (SELECT market_slug FROM temp_price_matches LIMIT 5)
ORDER BY start_time DESC;
