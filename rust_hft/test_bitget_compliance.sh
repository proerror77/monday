#!/bin/bash

# Bitget官方規範合規性測試腳本
# Bitget Official Compliance Test Script

echo "🔍 Bitget WebSocket V2 API 合規性測試"
echo "====================================="
echo ""

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 1. 檢查連接限制合規性
echo "📊 連接限制合規性檢查:"
echo "----------------------"

CONFIG_FILE="config/multi_symbol_collection.yaml"

# 檢查每個連接的訂閱數量
symbols=$(grep -A 20 "symbols:" "$CONFIG_FILE" | grep "  - " | wc -l | tr -d ' ')
channels=$(grep -A 10 "channels:" "$CONFIG_FILE" | grep "  - " | wc -l | tr -d ' ')
total_subscriptions=$((symbols * channels))

echo "- 符號數量: $symbols"
echo "- 頻道數量: $channels"
echo "- 總訂閱數: $total_subscriptions"

if [ "$total_subscriptions" -le 50 ]; then
    echo -e "- 訂閱數檢查: ${GREEN}✓ 合規${NC} (官方建議≤50)"
else
    echo -e "- 訂閱數檢查: ${RED}✗ 超標${NC} (官方建議≤50)"
    echo "  ${YELLOW}建議：減少符號或頻道數量${NC}"
fi

# 檢查心跳間隔
keepalive_interval=$(grep "keepalive_interval_sec:" "$CONFIG_FILE" | awk '{print $2}')
if [ "$keepalive_interval" = "30" ]; then
    echo -e "- 心跳間隔: ${GREEN}✓ 合規${NC} (30秒，符合官方要求)"
else
    echo -e "- 心跳間隔: ${YELLOW}⚠ 建議調整${NC} (當前${keepalive_interval}秒，官方要求30秒)"
fi

# 檢查重連策略
reconnect_strategy=$(grep "reconnect_strategy:" "$CONFIG_FILE" | awk '{print $2}' | sed 's/"//g')
if [ "$reconnect_strategy" = "conservative" ]; then
    echo -e "- 重連策略: ${GREEN}✓ 適合${NC} (保守策略，減少服務器壓力)"
else
    echo -e "- 重連策略: ${YELLOW}⚠ 注意${NC} (當前${reconnect_strategy}，建議使用conservative)"
fi

echo ""

# 2. 網絡連接測試
echo "🌐 網絡連接質量測試:"
echo "--------------------"

# 測試到Bitget的延遲
echo -n "- 網絡延遲測試: "
if command -v ping >/dev/null 2>&1; then
    # macOS ping語法
    latency=$(ping -c 3 api.bitget.com 2>/dev/null | grep 'avg' | awk -F'/' '{print $5}' | awk '{print $1}')
    if [ ! -z "$latency" ]; then
        latency_int=${latency%.*}
        if [ "$latency_int" -lt 100 ]; then
            echo -e "${GREEN}✓ 優秀${NC} (${latency}ms)"
        elif [ "$latency_int" -lt 200 ]; then
            echo -e "${YELLOW}○ 良好${NC} (${latency}ms)"
        else
            echo -e "${RED}△ 較慢${NC} (${latency}ms，可能影響連接穩定性)"
        fi
    else
        echo -e "${YELLOW}? 無法測量${NC}"
    fi
else
    echo -e "${YELLOW}? ping命令不可用${NC}"
fi

# 測試DNS解析
echo -n "- DNS解析測試: "
if nslookup ws.bitget.com >/dev/null 2>&1; then
    echo -e "${GREEN}✓ 正常${NC}"
else
    echo -e "${RED}✗ 失敗${NC}"
    echo "  ${YELLOW}建議：檢查DNS設置或使用公共DNS${NC}"
fi

echo ""

# 3. 系統資源檢查
echo "💻 系統資源合規檢查:"
echo "--------------------"

# 檢查可用內存
available_memory=$(vm_stat | awk '/Pages free/ {free=$3} /Pages inactive/ {inactive=$3} END {printf "%.0f", (free+inactive)*4096/1024/1024}')
echo "- 可用內存: ${available_memory}MB"

if [ "$available_memory" -gt 1000 ]; then
    echo -e "  ${GREEN}✓ 充足${NC} (建議≥1GB for穩定運行)"
elif [ "$available_memory" -gt 512 ]; then
    echo -e "  ${YELLOW}○ 足夠${NC} (建議監控內存使用)"
else
    echo -e "  ${RED}△ 不足${NC} (可能影響連接穩定性)"
fi

# 檢查文件描述符限制
ulimit_files=$(ulimit -n)
echo "- 文件描述符限制: $ulimit_files"

if [ "$ulimit_files" -gt 1024 ]; then
    echo -e "  ${GREEN}✓ 足夠${NC}"
else
    echo -e "  ${YELLOW}⚠ 建議增加${NC} (ulimit -n 4096)"
fi

echo ""

# 4. 配置優化建議
echo "⚙️  配置優化建議:"
echo "----------------"

echo "${BLUE}基於官方文檔的最佳實踐:${NC}"
echo "1. 單個連接訂閱數 ≤ 50個頻道"
echo "2. 每30秒發送字符串'ping'進行心跳"
echo "3. 使用保守的重連策略避免IP被限制"
echo "4. 監控連接質量，及時處理異常"

echo ""
echo "${BLUE}當前配置評估:${NC}"

# 計算配置評分
score=0
total_checks=4

if [ "$total_subscriptions" -le 50 ]; then
    score=$((score + 1))
fi

if [ "$keepalive_interval" = "30" ]; then
    score=$((score + 1))
fi

if [ "$reconnect_strategy" = "conservative" ]; then
    score=$((score + 1))
fi

if [ "$available_memory" -gt 512 ]; then
    score=$((score + 1))
fi

percentage=$((score * 100 / total_checks))

if [ "$percentage" -ge 75 ]; then
    echo -e "配置評分: ${GREEN}${percentage}%${NC} (優秀)"
elif [ "$percentage" -ge 50 ]; then
    echo -e "配置評分: ${YELLOW}${percentage}%${NC} (良好)"
else
    echo -e "配置評分: ${RED}${percentage}%${NC} (需要改進)"
fi

echo ""

# 5. 實時監控命令
echo "📊 實時監控建議:"
echo "----------------"

echo "${BLUE}連接監控命令:${NC}"
echo "1. 查看WebSocket狀態:"
echo "   tail -f rust_hft.log | grep -E 'WebSocket|ping|pong|Connected|Disconnected'"
echo ""
echo "2. 監控內存使用:"
echo "   watch -n 5 'ps aux | grep rust_hft'"
echo ""
echo "3. 網絡連接監控:"
echo "   netstat -an | grep 443"

echo ""

# 6. 快速修復腳本
echo "🛠️  快速優化腳本:"
echo "----------------"

cat << 'EOF' > /tmp/bitget_optimize.sh
#!/bin/bash
# Bitget官方規範優化腳本

echo "應用Bitget官方規範優化..."

# 設置文件描述符限制
ulimit -n 4096

# 設置環境變量
export RUST_LOG=info
export BITGET_HEARTBEAT_INTERVAL=30
export BITGET_MAX_SUBSCRIPTIONS=24
export BITGET_RECONNECT_STRATEGY=conservative

echo "✅ Bitget官方規範優化已應用"
echo "建議重新啟動應用程序以使配置生效"
EOF

chmod +x /tmp/bitget_optimize.sh
echo "生成優化腳本: /tmp/bitget_optimize.sh"

echo ""
echo "🎯 下一步行動建議:"
echo "1. 運行優化腳本: /tmp/bitget_optimize.sh"
echo "2. 使用官方規範配置重新測試"
echo "3. 監控連接穩定性和ping/pong響應"
echo "4. 確保24小時自動重連機制正常工作"

echo ""
echo -e "${GREEN}🚀 準備進行合規性測試！${NC}"