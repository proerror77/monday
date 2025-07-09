#!/bin/bash

# 性能優化測試腳本
# Performance Optimization Test Script

echo "🚀 性能優化測試"
echo "================"
echo ""

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 測試參數
TEST_DURATION=180  # 3分鐘測試
CONFIG_FILE="config/multi_symbol_collection.yaml"

echo "📊 測試參數:"
echo "- 測試時長: ${TEST_DURATION}秒 (3分鐘)"
echo "- 配置文件: $CONFIG_FILE"
echo "- 目標吞吐量: >50 msg/s"
echo "- 目標性能評分: >7.0/10"
echo ""

# 檢查編譯
echo "🔧 檢查編譯..."
if ! cargo check --example adaptive_collection > /dev/null 2>&1; then
    echo -e "${RED}❌ 編譯失敗，請檢查代碼${NC}"
    exit 1
fi
echo -e "${GREEN}✅ 編譯通過${NC}"

# 啟動測試
echo ""
echo "🎯 開始性能優化測試..."
echo "開始時間: $(date)"

# 創建日志文件
LOG_FILE="performance_test_$(date +%Y%m%d_%H%M%S).log"

# 啟動測試進程
cargo run --example adaptive_collection --release -- \
    --config "$CONFIG_FILE" \
    --duration "$TEST_DURATION" \
    --monitor \
    --auto-rebalance > "$LOG_FILE" 2>&1 &

TEST_PID=$!

echo "測試進程 PID: $TEST_PID"
echo "日志文件: $LOG_FILE"

# 實時監控
echo ""
echo "📈 實時監控 (每30秒更新):"
echo "----------------------------------------"

monitor_count=0
max_monitors=6  # 3分鐘 / 30秒 = 6次監控

while [ $monitor_count -lt $max_monitors ]; do
    sleep 30
    monitor_count=$((monitor_count + 1))
    
    # 檢查進程是否還在運行
    if ! kill -0 $TEST_PID 2>/dev/null; then
        echo -e "${YELLOW}⚠️ 測試進程已結束${NC}"
        break
    fi
    
    # 從日志提取最新統計
    if [ -f "$LOG_FILE" ]; then
        latest_stats=$(tail -20 "$LOG_FILE" | grep "自適應統計" | tail -1)
        if [ -n "$latest_stats" ]; then
            throughput=$(echo "$latest_stats" | grep -o '[0-9]*\.[0-9]* msg/s' | head -1)
            runtime=$(echo "$latest_stats" | grep -o '[0-9]*\.[0-9]*s' | tail -1)
            
            echo -e "${BLUE}[監控 #$monitor_count]${NC} 吞吐量: ${throughput:-N/A} | 運行時間: ${runtime:-N/A}"
        fi
        
        # 檢查錯誤
        error_count=$(grep -c "WebSocket.*error\|Connection.*reset" "$LOG_FILE")
        if [ "$error_count" -gt 0 ]; then
            echo -e "${RED}  ⚠️ 檢測到 $error_count 個WebSocket錯誤${NC}"
        fi
    fi
done

# 等待測試完成
echo ""
echo "⏰ 等待測試完成..."
wait $TEST_PID

echo ""
echo "📊 測試完成！開始分析結果..."

# 分析結果
if [ -f "$LOG_FILE" ]; then
    echo ""
    echo "🎯 === 性能優化測試結果 ==="
    echo "=========================="
    
    # 提取關鍵指標
    final_stats=$(grep "自適應動態連接收集完成統計" -A 15 "$LOG_FILE")
    
    if [ -n "$final_stats" ]; then
        throughput=$(echo "$final_stats" | grep "平均吞吐量" | grep -o '[0-9]*\.[0-9]* msg/s')
        performance_score=$(echo "$final_stats" | grep "性能評分" | grep -o '[0-9]*\.[0-9]*/10.0')
        performance_rating=$(echo "$final_stats" | grep "性能評級" | cut -d':' -f2 | xargs)
        total_messages=$(echo "$final_stats" | grep "總消息數" | grep -o '[0-9]*')
        connections=$(echo "$final_stats" | grep "總連接數" | grep -o '[0-9]*')
        
        echo "📦 總消息數: ${total_messages:-N/A}"
        echo "🚀 平均吞吐量: ${throughput:-N/A}"
        echo "🏆 性能評分: ${performance_score:-N/A}"
        echo "⭐ 性能評級: ${performance_rating:-N/A}"
        echo "🔗 連接數: ${connections:-N/A}"
        
        # 評估改善效果
        if [ -n "$throughput" ]; then
            throughput_num=$(echo "$throughput" | grep -o '[0-9]*\.[0-9]*')
            if [ "$(echo "$throughput_num > 50" | bc -l)" -eq 1 ]; then
                echo -e "${GREEN}✅ 吞吐量達標 (>50 msg/s)${NC}"
            else
                echo -e "${RED}❌ 吞吐量未達標 (<50 msg/s)${NC}"
            fi
        fi
        
        if [ -n "$performance_score" ]; then
            score_num=$(echo "$performance_score" | grep -o '[0-9]*\.[0-9]*')
            if [ "$(echo "$score_num > 7.0" | bc -l)" -eq 1 ]; then
                echo -e "${GREEN}✅ 性能評分達標 (>7.0/10)${NC}"
            else
                echo -e "${RED}❌ 性能評分未達標 (<7.0/10)${NC}"
            fi
        fi
    else
        echo -e "${RED}❌ 無法找到完整統計結果${NC}"
    fi
    
    # 檢查連接穩定性
    echo ""
    echo "🔍 連接穩定性分析:"
    websocket_errors=$(grep -c "WebSocket.*error\|Connection.*reset" "$LOG_FILE")
    pong_warnings=$(grep -c "No pong received" "$LOG_FILE")
    
    echo "- WebSocket錯誤: $websocket_errors"
    echo "- Pong警告: $pong_warnings"
    
    if [ "$websocket_errors" -eq 0 ]; then
        echo -e "${GREEN}✅ 無WebSocket錯誤${NC}"
    else
        echo -e "${RED}❌ 存在WebSocket錯誤${NC}"
    fi
    
    if [ "$pong_warnings" -le 3 ]; then
        echo -e "${GREEN}✅ Pong響應良好${NC}"
    else
        echo -e "${YELLOW}⚠️ Pong響應需要改善${NC}"
    fi
else
    echo -e "${RED}❌ 未找到日志文件${NC}"
fi

echo ""
echo "📋 優化建議:"
echo "------------"

# 基於結果給出建議
if [ -f "$LOG_FILE" ]; then
    if grep -q "需要優化" "$LOG_FILE"; then
        echo -e "${YELLOW}🔧 建議繼續優化:${NC}"
        echo "1. 檢查網絡連接質量"
        echo "2. 增加心跳頻率"
        echo "3. 優化消息處理邏輯"
        echo "4. 調整負載均衡參數"
    elif grep -q "良好" "$LOG_FILE"; then
        echo -e "${GREEN}✅ 系統性能良好${NC}"
        echo "建議進行長期穩定性測試"
    elif grep -q "優秀" "$LOG_FILE"; then
        echo -e "${GREEN}🎉 系統性能優秀${NC}"
        echo "可以投入生產環境"
    fi
fi

echo ""
echo "📄 詳細日志: $LOG_FILE"
echo "🎯 優化完成時間: $(date)"