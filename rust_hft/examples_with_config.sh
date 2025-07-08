#!/bin/bash

# 🚀 HFT 歷史資料下載器 - YAML 配置示例腳本
# 
# 展示如何使用 YAML 配置文件簡化複雜的下載任務

set -e

# 顏色設定
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
NC='\033[0m'

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_example() {
    echo -e "${PURPLE}[EXAMPLE]${NC} $1"
}

echo "🎯 HFT 歷史資料下載器 - YAML 配置示例"
echo "=========================================="
echo

# 確保 ClickHouse 正在運行
log_info "檢查 ClickHouse 服務..."
if curl -s http://localhost:8123/ping > /dev/null; then
    log_success "ClickHouse 服務正常運行"
else
    log_warning "ClickHouse 未運行，啟動服務..."
    docker-compose up -d clickhouse
    sleep 10
fi

# 1. 創建默認配置文件
log_example "1. 創建默認配置文件"
cargo run --example download_historical_lob -- --create-config my_config.yaml
log_success "配置文件已創建: my_config.yaml"
echo

# 2. 列出所有可用的配置 profiles
log_example "2. 列出配置中的所有 profiles"
cargo run --example download_historical_lob -- --config config/historical_data_config.yaml --list-profiles
echo

# 3. 使用快速測試配置
log_example "3. 快速測試 (單日 BTCUSDT 深度資料)"
cargo run --example download_historical_lob -- \
    --config config/historical_data_config.yaml \
    --profile quick_test
log_success "快速測試完成"
echo

# 4. 使用開發環境配置
log_example "4. 開發環境配置 (小批量測試)"
cargo run --example download_historical_lob -- \
    --config config/historical_data_config.yaml \
    --profile development
log_success "開發環境測試完成"
echo

# 5. 命令行參數覆蓋配置文件
log_example "5. 命令行參數覆蓋 (使用 test profile 但改為 SOLUSDT)"
cargo run --example download_historical_lob -- \
    --config config/historical_data_config.yaml \
    --profile test \
    --symbol SOLUSDT \
    --max-concurrent 2
log_success "參數覆蓋測試完成"
echo

# 6. 批量下載多個交易對
log_example "6. 批量下載多個交易對 (使用 production profile)"
log_warning "這個任務會下載大量數據，確認要繼續嗎？ (y/N)"
read -r response
if [[ "$response" =~ ^[Yy]$ ]]; then
    cargo run --example download_historical_lob -- \
        --config config/historical_data_config.yaml \
        --profile production \
        --max-concurrent 3 \
        --start-date 2024-07-01 \
        --end-date 2024-07-02
    log_success "批量下載完成"
else
    log_info "跳過批量下載"
fi
echo

# 7. 顯示數據庫統計
log_example "7. 查看 ClickHouse 資料庫統計"
echo "=== 資料庫統計 ==="
curl -s 'http://localhost:8123/' --data "
SELECT 
    symbol,
    count() as record_count,
    min(timestamp) as earliest_data,
    max(timestamp) as latest_data
FROM hft_db.lob_depth 
GROUP BY symbol 
ORDER BY record_count DESC
FORMAT PrettyCompact
"
echo

# 8. 性能測試
log_example "8. 性能對比測試"
echo "測試不同批次大小的性能..."

# 小批次
log_info "測試小批次 (batch_size: 1000)"
time cargo run --example download_historical_lob -- \
    --config config/historical_data_config.yaml \
    --profile quick_test \
    --batch-size 1000 \
    --symbol ETHUSDT

# 大批次
log_info "測試大批次 (batch_size: 5000)"
time cargo run --example download_historical_lob -- \
    --config config/historical_data_config.yaml \
    --profile quick_test \
    --batch-size 5000 \
    --symbol ETHUSDT

log_success "性能測試完成"
echo

# 9. 自定義配置示例
log_example "9. 創建自定義配置文件"
cat > custom_config.yaml << 'EOF'
# 自定義 HFT 配置
my_custom_profile:
  symbol: "ADAUSDT"
  data_type: "depth"
  date_range:
    start_date: "2024-07-01"
    end_date: "2024-07-01"
  performance:
    max_concurrent: 3
    batch_size: 2000
  clickhouse:
    enabled: true
    url: "http://localhost:8123"
  output:
    directory: "./my_custom_data"
    cleanup_zip: true
EOF

cargo run --example download_historical_lob -- \
    --config custom_config.yaml \
    --profile my_custom_profile

log_success "自定義配置測試完成"
rm custom_config.yaml
echo

# 10. 顯示最終統計
log_example "10. 最終資料庫統計摘要"
echo "=== 完整統計摘要 ==="
curl -s 'http://localhost:8123/' --data "
SELECT 
    'LOB Depth Records' as table_type,
    count() as total_records,
    uniq(symbol) as unique_symbols,
    formatReadableSize(sum(length(toString(bid_prices)) + length(toString(ask_prices)))) as estimated_size
FROM hft_db.lob_depth
UNION ALL
SELECT 
    'Trade Records' as table_type,
    count() as total_records,
    uniq(symbol) as unique_symbols,
    formatReadableSize(sum(length(trade_id) + length(toString(price)))) as estimated_size
FROM hft_db.trade_data
FORMAT PrettyCompact
"

echo
log_success "🎉 所有示例完成！"
echo
echo "💡 總結："
echo "   - YAML 配置讓複雜的下載任務變得簡單"
echo "   - 可以預定義多個環境配置"
echo "   - 命令行參數可以靈活覆蓋配置"
echo "   - 支援批量下載多個交易對"
echo "   - 一鍵完成下載→入庫→統計全流程"
echo
echo "📚 下一步："
echo "   - 編輯 config/historical_data_config.yaml 創建您的配置"
echo "   - 使用 --profile 參數選擇不同環境"
echo "   - 運行訓練流程：cargo run --example train_with_historical"
echo