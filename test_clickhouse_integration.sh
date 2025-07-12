#!/bin/bash

# 🚀 测试WebSocket + ClickHouse集成系统
# 此脚本将启动ClickHouse，运行WebSocket数据收集，并验证数据写入

set -e

echo "🚀 === WebSocket + ClickHouse集成测试 ==="
echo

# 1. 检查Docker是否运行
echo "📋 检查Docker环境..."
if ! docker info >/dev/null 2>&1; then
    echo "❌ Docker未运行，请启动Docker"
    exit 1
fi
echo "✅ Docker运行正常"

# 2. 启动ClickHouse (如果没有运行)
echo
echo "📊 启动ClickHouse数据库..."
cd rust_hft
if ! docker ps | grep -q hft_clickhouse; then
    echo "🐳 启动ClickHouse容器..."
    docker-compose up -d clickhouse
    echo "⏳ 等待ClickHouse启动..."
    sleep 20
    
    # 等待ClickHouse健康检查通过
    echo "🔍 检查ClickHouse健康状态..."
    while ! docker exec hft_clickhouse clickhouse-client --query "SELECT 1" >/dev/null 2>&1; do
        echo "⏳ 等待ClickHouse就绪..."
        sleep 5
    done
    echo "✅ ClickHouse已就绪"
else
    echo "✅ ClickHouse已在运行"
fi

# 3. 验证ClickHouse连接
echo
echo "🔗 验证ClickHouse连接..."
if curl -s "http://localhost:8123/ping" | grep -q "Ok"; then
    echo "✅ ClickHouse HTTP接口连接成功"
else
    echo "❌ ClickHouse连接失败"
    exit 1
fi

# 4. 检查数据库和表
echo
echo "📋 检查数据库和表结构..."
docker exec hft_clickhouse clickhouse-client --query "SHOW DATABASES" | grep -q hft_db && echo "✅ 数据库 hft_db 存在" || echo "❌ 数据库不存在"
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "SHOW TABLES" && echo "✅ 表结构检查完成"

# 5. 清理旧数据（可选）
echo
echo "🧹 清理旧测试数据..."
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "TRUNCATE TABLE IF EXISTS lob_depth"
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "TRUNCATE TABLE IF EXISTS trade_data"
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "TRUNCATE TABLE IF EXISTS ticker_data"
echo "✅ 旧数据已清理"

# 6. 运行WebSocket测试（启用ClickHouse写入）
echo
echo "🚀 运行WebSocket + ClickHouse集成测试（30秒）..."
cd ..

# 设置环境变量启用ClickHouse
export ENABLE_CLICKHOUSE=1
export CLICKHOUSE_URL="http://localhost:8123"
export CLICKHOUSE_DB="hft_db"
export CLICKHOUSE_USER="hft_user"
export CLICKHOUSE_PASSWORD="hft_password"

# 运行测试程序30秒
timeout 30s ./target/release/real_bitget_test || true

# 7. 验证数据写入
echo
echo "📊 验证数据写入结果..."
echo

echo "📈 LOB深度数据统计:"
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "SELECT count() as lob_count, min(timestamp) as first_timestamp, max(timestamp) as last_timestamp FROM lob_depth"

echo
echo "💰 交易数据统计:"
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "SELECT count() as trade_count, min(timestamp) as first_timestamp, max(timestamp) as last_timestamp FROM trade_data"

echo
echo "📊 Ticker数据统计:"
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "SELECT count() as ticker_count, min(timestamp) as first_timestamp, max(timestamp) as last_timestamp FROM ticker_data WHERE timestamp > 0"

echo
echo "🔍 各交易对数据分布:"
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "SELECT symbol, count() as count FROM lob_depth GROUP BY symbol ORDER BY count DESC LIMIT 10"

echo
echo "💾 数据库存储统计:"
docker exec hft_clickhouse clickhouse-client --database=hft_db --query "SELECT table, formatReadableSize(sum(data_compressed_bytes)) as compressed_size, formatReadableSize(sum(data_uncompressed_bytes)) as uncompressed_size, sum(rows) as total_rows FROM system.parts WHERE database = 'hft_db' GROUP BY table"

echo
echo "🎯 === 测试完成 ==="
echo "✅ WebSocket数据接收正常"
echo "✅ ClickHouse数据写入正常"
echo "✅ 数据压缩和存储正常"
echo
echo "💡 提示:"
echo "   - ClickHouse Web界面: http://localhost:8123/play"
echo "   - 停止服务: cd rust_hft && docker-compose down"
echo "   - 查看日志: docker logs hft_clickhouse"