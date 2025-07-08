#!/bin/bash

# 快速數據檢查腳本
echo "🔍 快速數據檢查報告"
echo "===================="

# 檢查ClickHouse連接
if ! docker exec hft_clickhouse clickhouse-client --query "SELECT 1" > /dev/null 2>&1; then
    echo "❌ ClickHouse未運行"
    exit 1
fi

echo "✅ ClickHouse連接正常"
echo ""

# 總記錄數
total_records=$(docker exec hft_clickhouse clickhouse-client --query "SELECT count(*) FROM hft_db.enhanced_market_data" 2>/dev/null)
echo "📊 總記錄數: ${total_records}"

# 各商品統計
echo ""
echo "📋 各商品數據統計:"
docker exec hft_clickhouse clickhouse-client --query "
SELECT 
    symbol,
    count(*) as records,
    round(count(*) * 100.0 / ${total_records}, 2) as percentage
FROM hft_db.enhanced_market_data 
GROUP BY symbol 
ORDER BY records DESC
" --format PrettyCompact

# 時間範圍
echo ""
echo "⏰ 數據時間範圍:"
docker exec hft_clickhouse clickhouse-client --query "
SELECT 
    min(timestamp) as earliest_time,
    max(timestamp) as latest_time,
    max(timestamp) - min(timestamp) as duration_microseconds
FROM hft_db.enhanced_market_data
" --format PrettyCompact

# 最近5分鐘數據
echo ""
echo "🔥 最近5分鐘活動:"
docker exec hft_clickhouse clickhouse-client --query "
SELECT 
    symbol,
    count(*) as recent_records
FROM hft_db.enhanced_market_data 
WHERE timestamp > (SELECT max(timestamp) - 300000000 FROM hft_db.enhanced_market_data)
GROUP BY symbol 
ORDER BY recent_records DESC
" --format PrettyCompact

echo ""
echo "✅ 數據檢查完成"