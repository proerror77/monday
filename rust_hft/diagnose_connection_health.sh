#!/bin/bash

# 連接健康診斷腳本
# Connection Health Diagnostic Script

echo "🔍 Bitget WebSocket 連接健康診斷"
echo "=================================="
echo ""

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 檢查網絡連接
echo "📡 網絡連接檢查:"
echo "----------------"

# 檢查基本網絡連通性
echo -n "- 檢查到 Bitget 的網絡連通性: "
if curl -s --connect-timeout 5 https://api.bitget.com/api/spot/v1/public/time > /dev/null; then
    echo -e "${GREEN}✓ 正常${NC}"
else
    echo -e "${RED}✗ 失敗${NC}"
    echo "  ${YELLOW}建議：檢查網絡連接和防火牆設置${NC}"
fi

# 檢查 WebSocket 端點
echo -n "- 檢查 WebSocket 端點可達性: "
if timeout 5 bash -c "</dev/tcp/ws.bitget.com/443"; then
    echo -e "${GREEN}✓ 正常${NC}"
else
    echo -e "${RED}✗ 失敗${NC}"
    echo "  ${YELLOW}建議：WebSocket 端點不可達，檢查網絡或代理設置${NC}"
fi

echo ""

# 檢查系統資源
echo "💻 系統資源檢查:"
echo "----------------"

# 檢查 CPU 使用率
cpu_usage=$(top -l 1 -s 0 | grep "CPU usage" | awk '{print $3}' | sed 's/%//')
if [ "${cpu_usage%.*}" -lt 80 ]; then
    echo -e "- CPU 使用率: ${GREEN}${cpu_usage}% (正常)${NC}"
else
    echo -e "- CPU 使用率: ${RED}${cpu_usage}% (過高)${NC}"
    echo "  ${YELLOW}建議：降低系統負載或增加 CPU 資源${NC}"
fi

# 檢查內存使用率
memory_usage=$(vm_stat | awk '/Pages free/ {free=$3} /Pages active/ {active=$3} /Pages inactive/ {inactive=$3} /Pages wired/ {wired=$3} END {total=(free+active+inactive+wired)*4096/1024/1024/1024; used=(active+inactive+wired)*4096/1024/1024/1024; printf "%.1f", used/total*100}')
if [ "${memory_usage%.*}" -lt 80 ]; then
    echo -e "- 內存使用率: ${GREEN}${memory_usage}% (正常)${NC}"
else
    echo -e "- 內存使用率: ${RED}${memory_usage}% (過高)${NC}"
    echo "  ${YELLOW}建議：增加內存或優化內存使用${NC}"
fi

# 檢查文件描述符限制
ulimit_files=$(ulimit -n)
if [ "$ulimit_files" -gt 1024 ]; then
    echo -e "- 文件描述符限制: ${GREEN}${ulimit_files} (足夠)${NC}"
else
    echo -e "- 文件描述符限制: ${RED}${ulimit_files} (可能不足)${NC}"
    echo "  ${YELLOW}建議：增加文件描述符限制 (ulimit -n 4096)${NC}"
fi

echo ""

# 檢查配置設置
echo "⚙️  配置設置檢查:"
echo "----------------"

CONFIG_FILE="config/multi_symbol_collection.yaml"
if [ -f "$CONFIG_FILE" ]; then
    echo -e "- 配置文件: ${GREEN}✓ 存在${NC}"
    
    # 檢查重連策略
    reconnect_strategy=$(grep "reconnect_strategy:" "$CONFIG_FILE" | awk '{print $2}' | sed 's/"//g')
    echo "- 重連策略: $reconnect_strategy"
    
    # 檢查超時設置
    timeout_seconds=$(grep "timeout_seconds:" "$CONFIG_FILE" | awk '{print $2}')
    echo "- 連接超時: ${timeout_seconds}秒"
    
    # 檢查符號數量
    symbol_count=$(grep -A 20 "symbols:" "$CONFIG_FILE" | grep "  - " | wc -l | tr -d ' ')
    echo "- 配置的符號數量: $symbol_count"
    
    if [ "$symbol_count" -gt 15 ]; then
        echo -e "  ${YELLOW}建議：符號數量較多 ($symbol_count)，可能導致連接壓力${NC}"
    fi
    
else
    echo -e "- 配置文件: ${RED}✗ 不存在${NC}"
    echo "  ${YELLOW}建議：創建配置文件 $CONFIG_FILE${NC}"
fi

echo ""

# 檢查最近的日誌錯誤
echo "📝 最近的連接錯誤分析:"
echo "----------------------"

# 搜索最近的 WebSocket 錯誤
recent_errors=$(find . -name "*.log" -mtime -1 -exec grep -l "WebSocket.*error\|Connection.*reset" {} \; 2>/dev/null)

if [ -n "$recent_errors" ]; then
    echo -e "${YELLOW}發現最近的 WebSocket 錯誤日誌:${NC}"
    for log_file in $recent_errors; do
        echo "  - $log_file"
        echo "    最近錯誤:"
        tail -5 "$log_file" | grep -E "WebSocket|Connection|error" | tail -2 | sed 's/^/    /'
    done
else
    echo -e "${GREEN}✓ 未發現最近的 WebSocket 錯誤日誌${NC}"
fi

echo ""

# 連接測試建議
echo "🚀 連接優化建議:"
echo "----------------"

echo "1. ${BLUE}減少併發連接數量${NC}"
echo "   - 降低符號數量或增加連接分組"
echo "   - 每個連接最多訂閱 10-15 個符號"

echo ""
echo "2. ${BLUE}優化重連策略${NC}"
echo "   - 使用 'conservative' 重連策略"
echo "   - 增加重連延遲避免服務器限制"

echo ""
echo "3. ${BLUE}網絡穩定性${NC}"
echo "   - 確保穩定的網絡連接"
echo "   - 考慮使用專線或優化網絡路由"

echo ""
echo "4. ${BLUE}監控和診斷${NC}"
echo "   - 啟用詳細日誌記錄"
echo "   - 監控連接統計和錯誤率"

echo ""

# 快速修復腳本生成
echo "🛠️  快速修復腳本:"
echo "----------------"

cat << 'EOF' > /tmp/quick_fix_connections.sh
#!/bin/bash
# 快速修復 WebSocket 連接問題

echo "應用連接優化設置..."

# 增加文件描述符限制
ulimit -n 4096

# 設置更保守的重連策略
export RUST_LOG=info
export BITGET_RECONNECT_STRATEGY=conservative
export BITGET_MAX_SUBSCRIPTIONS_PER_CONNECTION=12

echo "✅ 優化設置已應用"
echo "建議重新啟動應用程序"
EOF

chmod +x /tmp/quick_fix_connections.sh
echo "生成快速修復腳本: /tmp/quick_fix_connections.sh"

echo ""
echo "🎯 下一步行動:"
echo "1. 運行快速修復腳本: /tmp/quick_fix_connections.sh"
echo "2. 重新啟動應用程序進行測試"
echo "3. 監控連接穩定性和錯誤率"
echo "4. 如問題持續，考慮進一步減少併發量"

echo ""
echo "📊 實時監控命令:"
echo "tail -f rust_hft.log | grep -E 'WebSocket|Connection|error'"