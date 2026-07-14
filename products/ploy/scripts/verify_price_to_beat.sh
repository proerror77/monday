#!/bin/bash
# 验证 price_to_beat 采集器是否正常工作

: "${PGPASSWORD:?Set PGPASSWORD before running this check}"
psql -U postgres -d ploy << 'EOF'
-- 1. 检查最近更新的 price_to_beat
SELECT
    market_slug,
    symbol,
    start_time,
    price_to_beat,
    NOW() - start_time as time_since_start
FROM pm_market_metadata
WHERE market_slug LIKE '%-updown-5m-%'
  AND price_to_beat IS NOT NULL
  AND start_time > NOW() - INTERVAL '1 hour'
ORDER BY start_time DESC
LIMIT 10;

-- 2. 统计 price_to_beat 覆盖率
SELECT
    symbol,
    COUNT(*) as total_markets,
    COUNT(price_to_beat) as has_price_to_beat,
    ROUND(100.0 * COUNT(price_to_beat) / COUNT(*), 2) as coverage_pct
FROM pm_market_metadata
WHERE market_slug LIKE '%-updown-5m-%'
  AND end_time > NOW() - INTERVAL '24 hours'
GROUP BY symbol
ORDER BY symbol;

-- 3. 检查即将开始的市场（未来 5 分钟）
SELECT
    market_slug,
    symbol,
    start_time,
    price_to_beat,
    start_time - NOW() as time_until_start
FROM pm_market_metadata
WHERE market_slug LIKE '%-updown-5m-%'
  AND start_time > NOW()
  AND start_time < NOW() + INTERVAL '5 minutes'
ORDER BY start_time;
EOF
