#!/bin/bash

# WebSocket連接實時監控腳本
# Real-time WebSocket Connection Monitor

echo "🔍 WebSocket 連接實時監控"
echo "========================"
echo "監控 Rust HFT 系統的 WebSocket 連接狀態..."
echo "按 Ctrl+C 停止監控"
echo ""

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 計數器
error_count=0
success_count=0
ping_pong_count=0
connection_count=0

# 監控函數
monitor_logs() {
    tail -f target/release/examples/adaptive_collection.log 2>/dev/null | while read -r line; do
        timestamp=$(echo "$line" | grep -o '^[0-9-]*T[0-9:]*\.[0-9]*Z')
        
        # 檢查WebSocket錯誤
        if echo "$line" | grep -q -i "websocket.*error\|connection.*reset\|protocol.*error"; then
            error_count=$((error_count + 1))
            echo -e "${RED}[$(date '+%H:%M:%S')] ❌ WebSocket錯誤: ${NC}"
            echo "   $line"
            echo ""
        fi
        
        # 檢查成功連接
        if echo "$line" | grep -q "WebSocket connected successfully\|Bitget V2 WebSocket connected"; then
            connection_count=$((connection_count + 1))
            echo -e "${GREEN}[$(date '+%H:%M:%S')] ✅ 連接成功 #${connection_count}${NC}"
        fi
        
        # 檢查ping/pong
        if echo "$line" | grep -q -i "ping\|pong"; then
            ping_pong_count=$((ping_pong_count + 1))
            echo -e "${BLUE}[$(date '+%H:%M:%S')] 💓 心跳 #${ping_pong_count}${NC}"
        fi
        
        # 檢查訂閱
        if echo "$line" | grep -q "Subscribed to"; then
            success_count=$((success_count + 1))
            symbol=$(echo "$line" | grep -o '[A-Z]*USDT')
            echo -e "${GREEN}[$(date '+%H:%M:%S')] 📊 訂閱成功: ${symbol}${NC}"
        fi
        
        # 檢查統計信息
        if echo "$line" | grep -q "自適應統計\|吞吐量"; then
            throughput=$(echo "$line" | grep -o '[0-9]*\.[0-9]* msg/s')
            echo -e "${YELLOW}[$(date '+%H:%M:%S')] 📈 性能: ${throughput}${NC}"
        fi
    done
}

# 啟動監控
echo "開始監控..."
monitor_logs &
MONITOR_PID=$!

# 等待用戶中斷
trap "kill $MONITOR_PID 2>/dev/null; echo; echo '監控已停止'; exit 0" INT

# 定期統計報告
while true; do
    sleep 30
    echo ""
    echo -e "${BLUE}=== 30秒統計報告 ===${NC}"
    echo -e "✅ 成功連接: ${GREEN}${connection_count}${NC}"
    echo -e "📊 成功訂閱: ${GREEN}${success_count}${NC}"  
    echo -e "💓 心跳檢查: ${BLUE}${ping_pong_count}${NC}"
    echo -e "❌ 錯誤計數: ${RED}${error_count}${NC}"
    echo ""
done