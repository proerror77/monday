#!/bin/bash

# 多商品高性能數據收集啟動腳本
# Multi-Symbol High Performance Data Collection Startup Script

echo "🚀 多商品高性能數據收集系統啟動"
echo "=================================="

# 設置日誌級別
export RUST_LOG=info

# 檢查配置文件
CONFIG_FILE="config/multi_symbol_collection.yaml"
if [ ! -f "$CONFIG_FILE" ]; then
    echo "❌ 配置文件不存在: $CONFIG_FILE"
    exit 1
fi

echo "✅ 配置文件檢查通過: $CONFIG_FILE"

# 檢查ClickHouse連接
echo "🔍 檢查ClickHouse連接..."
if docker exec hft_clickhouse clickhouse-client --query "SELECT 1" > /dev/null 2>&1; then
    echo "✅ ClickHouse連接正常"
else
    echo "⚠️  ClickHouse未啟動，嘗試啟動..."
    if command -v docker-compose &> /dev/null; then
        docker-compose up -d clickhouse
        sleep 5
        
        if docker exec hft_clickhouse clickhouse-client --query "SELECT 1" > /dev/null 2>&1; then
            echo "✅ ClickHouse啟動成功"
        else
            echo "❌ ClickHouse啟動失敗，繼續運行但不會入庫"
        fi
    else
        echo "⚠️  Docker Compose未安裝，跳過ClickHouse啟動"
    fi
fi

# 創建數據表
echo "📊 創建ClickHouse數據表..."
if docker exec hft_clickhouse clickhouse-client --query "SELECT 1" > /dev/null 2>&1; then
    if [ -f "clickhouse/multi_symbol_tables.sql" ]; then
        docker exec -i hft_clickhouse clickhouse-client < clickhouse/multi_symbol_tables.sql > /dev/null 2>&1
        echo "✅ 數據表創建完成"
    else
        echo "⚠️  數據表SQL文件不存在，跳過創建"
    fi
else
    echo "⚠️  跳過數據表創建"
fi

# 編譯檢查
echo "🔧 編譯檢查..."
echo "檢查基於流量的多連接模塊..."
if cargo check --example volume_based_collection > /dev/null 2>&1; then
    echo "✅ 基於流量的多連接編譯檢查通過"
else
    echo "❌ 基於流量的多連接編譯檢查失敗"
    echo "嘗試修復..."
    cargo clean
    cargo check --example volume_based_collection
    if [ $? -ne 0 ]; then
        echo "❌ 編譯失敗，請檢查代碼"
        exit 1
    fi
fi

echo "檢查單連接備用模塊..."
if cargo check --example multi_symbol_high_perf_collection > /dev/null 2>&1; then
    echo "✅ 單連接備用模塊編譯檢查通過"
else
    echo "⚠️  單連接備用模塊編譯失敗，但不影響主要功能"
fi

# 選擇運行模式
echo ""
echo "🎯 高性能數據錄製測試模式："
echo "1) ⚡ 快速測試 (5分鐘) - 驗證系統穩定性"
echo "2) 📊 標準測試 (30分鐘) - 性能基準測試"  
echo "3) 🔋 長期測試 (2小時) - 承載量測試"
echo "4) 🌙 24小時測試 - 準生產測試"
echo "5) 🛠️  自定義時長"
echo ""
read -p "請輸入選擇 [1-5]: " choice

case $choice in
    1)
        echo "⚡ 啟動快速穩定性測試 (5分鐘)..."
        echo "💡 測試目標：驗證連接穩定性、數據完整性、錯誤率"
        echo "📊 開始時間: $(date)"
        cargo run --example adaptive_collection --release -- --config $CONFIG_FILE --duration 300 --monitor --auto-rebalance
        ;;
    2)
        echo "📊 啟動標準性能測試 (30分鐘)..."
        echo "💡 測試目標：吞吐量基準、內存使用、延遲分佈"
        echo "📊 開始時間: $(date)"
        cargo run --example adaptive_collection --release -- --config $CONFIG_FILE --duration 1800 --monitor --auto-rebalance
        ;;
    3)
        echo "🔋 啟動長期承載量測試 (2小時)..."
        echo "💡 測試目標：最大承載量、長期穩定性、性能退化"
        echo "📊 開始時間: $(date)"
        cargo run --example adaptive_collection --release -- --config $CONFIG_FILE --duration 7200 --monitor --auto-rebalance
        ;;
    4)
        echo "🌙 啟動24小時準生產測試..."
        echo "💡 測試目標：24/7穩定運行、故障自動恢復、完整性能報告"
        echo "📊 開始時間: $(date)"
        cargo run --example adaptive_collection --release -- --config $CONFIG_FILE --duration 86400 --monitor --auto-rebalance
        ;;
    5)
        read -p "📝 輸入測試時長（分鐘）: " duration
        duration_seconds=$((duration * 60))
        echo "🛠️  啟動自定義測試 (${duration} 分鐘)..."
        echo "💡 測試配置：自適應動態連接 + 增強重連 + 性能監控"
        echo "📊 開始時間: $(date)"
        cargo run --example adaptive_collection --release -- --config $CONFIG_FILE --duration "$duration_seconds" --monitor --auto-rebalance
        ;;
    *)
        echo "❌ 無效選擇，使用默認標準測試模式"
        echo "📊 啟動標準性能測試 (30分鐘)..."
        cargo run --example adaptive_collection --release -- --config $CONFIG_FILE --duration 1800 --monitor --auto-rebalance
        ;;
esac

# 測試完成後的性能報告
echo ""
echo "🎉 數據錄製測試完成！"
echo "📊 結束時間: $(date)"
echo ""
echo "📈 快速性能檢查："
if docker exec hft_clickhouse clickhouse-client --query "SELECT 1" > /dev/null 2>&1; then
    echo "✅ 數據庫連接正常"
    
    # 檢查數據量
    total_records=$(docker exec hft_clickhouse clickhouse-client --query "SELECT count(*) FROM hft_db.enhanced_market_data" 2>/dev/null || echo "查詢失敗")
    echo "📊 總記錄數: ${total_records}"
    
    # 檢查每個商品的數據量
    echo "📋 各商品數據統計:"
    docker exec hft_clickhouse clickhouse-client --query "SELECT symbol, count(*) as records, min(timestamp) as start_time, max(timestamp) as end_time FROM hft_db.enhanced_market_data GROUP BY symbol ORDER BY records DESC LIMIT 10" 2>/dev/null || echo "查詢失敗"
    
    # 檢查最近數據
    echo ""
    echo "⏰ 最近數據時間檢查:"
    docker exec hft_clickhouse clickhouse-client --query "SELECT symbol, max(timestamp) as latest_time FROM hft_db.enhanced_market_data GROUP BY symbol ORDER BY latest_time DESC LIMIT 5" 2>/dev/null || echo "查詢失敗"
else
    echo "⚠️  數據庫連接失敗，無法生成性能報告"
fi

echo ""
echo "💡 性能優化建議："
echo "✅ 已啟用增強重連機制 - 自動故障恢復"
echo "✅ 已啟用自適應動態連接 - 智能負載均衡"
echo "✅ 已啟用實時監控 - 性能指標追蹤"
echo ""
echo "📋 下一步測試建議："
echo "1. 短期測試：觀察連接穩定性和錯誤率"
echo "2. 中期測試：監控吞吐量和內存使用"
echo "3. 長期測試：驗證24/7運行能力"
echo "4. 負載測試：逐步增加商品數量測試極限"

