#!/bin/bash
set -e

# 验证数据收集状态的脚本

# ClickHouse配置
CH_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CH_USER="default"
CH_PASSWORD="s9wECb~NGZPOE"

echo "📊 HFT数据收集状态验证"
echo "======================="
echo ""

# 函数：执行ClickHouse查询
run_query() {
    local query=$1
    curl -s -X POST "${CH_URL}" \
        --user "${CH_USER}:${CH_PASSWORD}" \
        --data-binary "${query}" \
        --header "Content-Type: text/plain" 2>/dev/null
}

# 1. 检查各表的数据量
echo "📈 数据表状态（最近5分钟）："
echo "----------------------------"

query="
SELECT
    CASE
        WHEN table LIKE '%futures%' THEN '🔴 ' || table
        WHEN table LIKE '%spot%' OR table LIKE 'binance_%' AND table NOT LIKE '%futures%' THEN '🟢 ' || table
        ELSE '⚪ ' || table
    END as table_type,
    COUNT(*) as records,
    MAX(ts) as latest_time,
    COUNT(DISTINCT symbol) as symbols
FROM (
    SELECT 'binance_orderbook' as table, ts, symbol FROM hft_db.binance_orderbook WHERE ts > now() - INTERVAL 5 MINUTE
    UNION ALL
    SELECT 'binance_trades' as table, ts, symbol FROM hft_db.binance_trades WHERE ts > now() - INTERVAL 5 MINUTE
    UNION ALL
    SELECT 'binance_l1' as table, ts, symbol FROM hft_db.binance_l1 WHERE ts > now() - INTERVAL 5 MINUTE
    UNION ALL
    SELECT 'binance_ticker' as table, ts, symbol FROM hft_db.binance_ticker WHERE ts > now() - INTERVAL 5 MINUTE
    UNION ALL
    SELECT 'binance_futures_orderbook' as table, ts, symbol FROM hft_db.binance_futures_orderbook WHERE ts > now() - INTERVAL 5 MINUTE
    UNION ALL
    SELECT 'binance_futures_trades' as table, ts, symbol FROM hft_db.binance_futures_trades WHERE ts > now() - INTERVAL 5 MINUTE
    UNION ALL
    SELECT 'binance_futures_l1' as table, ts, symbol FROM hft_db.binance_futures_l1 WHERE ts > now() - INTERVAL 5 MINUTE
    UNION ALL
    SELECT 'binance_futures_ticker' as table, ts, symbol FROM hft_db.binance_futures_ticker WHERE ts > now() - INTERVAL 5 MINUTE
)
GROUP BY table
ORDER BY table
FORMAT PrettyCompactMonoBlock"

run_query "$query"

echo ""
echo "📊 数据质量分析："
echo "-----------------"

# 2. 检查数据连续性
query="
WITH time_gaps AS (
    SELECT
        'binance_orderbook' as table_name,
        toStartOfMinute(ts) as minute,
        COUNT(*) as records_per_minute
    FROM hft_db.binance_orderbook
    WHERE ts > now() - INTERVAL 30 MINUTE
    GROUP BY minute
)
SELECT
    COUNT(*) as total_minutes,
    COUNT(DISTINCT minute) as active_minutes,
    ROUND(COUNT(DISTINCT minute) * 100.0 / COUNT(*), 2) as coverage_percent,
    MIN(records_per_minute) as min_records,
    MAX(records_per_minute) as max_records,
    AVG(records_per_minute) as avg_records
FROM time_gaps
FORMAT PrettyCompactMonoBlock"

echo "现货订单簿连续性："
run_query "$query"

echo ""
echo "🔍 符号分类检查："
echo "-----------------"

# 3. 检查哪些符号在哪个表
query="
SELECT
    symbol,
    SUM(spot_count) as spot_records,
    SUM(futures_count) as futures_records,
    CASE
        WHEN SUM(futures_count) > 0 AND SUM(spot_count) = 0 THEN '✅ 正确分类为永续'
        WHEN SUM(spot_count) > 0 AND SUM(futures_count) = 0 THEN '✅ 正确分类为现货'
        WHEN SUM(spot_count) > 0 AND SUM(futures_count) > 0 THEN '⚠️  同时存在两表'
        ELSE '❌ 无数据'
    END as status
FROM (
    SELECT symbol, COUNT(*) as spot_count, 0 as futures_count
    FROM hft_db.binance_orderbook
    WHERE ts > now() - INTERVAL 5 MINUTE
    GROUP BY symbol
    UNION ALL
    SELECT symbol, 0 as spot_count, COUNT(*) as futures_count
    FROM hft_db.binance_futures_orderbook
    WHERE ts > now() - INTERVAL 5 MINUTE
    GROUP BY symbol
)
GROUP BY symbol
ORDER BY symbol
LIMIT 20
FORMAT PrettyCompactMonoBlock"

run_query "$query"

echo ""
echo "💡 诊断建议："
echo "------------"
echo "🟢 现货表: binance_orderbook, binance_trades, binance_l1, binance_ticker"
echo "🔴 期货表: binance_futures_orderbook, binance_futures_trades, binance_futures_l1, binance_futures_ticker"
echo ""
echo "如果期货表为空，说明："
echo "1. Docker镜像缺少binance-futures feature"
echo "2. 或者collector没有连接到正确的永续合约端点(fstream.binance.com)"
echo ""
echo "解决方案："
echo "1. 运行 ./fix-cargo-features.sh 修复配置"
echo "2. 运行 ./build-standard.sh 构建完整镜像"
echo "3. 运行 ./deploy-standard.sh 部署到ECS"