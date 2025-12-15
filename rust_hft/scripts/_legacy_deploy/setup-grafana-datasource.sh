#!/bin/bash

# 配置 Grafana ClickHouse 數據源和 Dashboard

set -euo pipefail

GRAFANA_URL="${GRAFANA_URL:-http://localhost:3000}"
GRAFANA_USER="${GRAFANA_USER:-admin}"
GRAFANA_PASSWORD="${GRAFANA_PASSWORD:-sonic0923}"
CLICKHOUSE_URL="${CLICKHOUSE_URL:-http://localhost:8123}"

GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m'

log() {
    echo -e "${GREEN}[$(date '+%H:%M:%S')]${NC} $1"
}

error() {
    echo -e "${RED}[$(date '+%H:%M:%S')] ERROR:${NC} $1"
}

# 檢查 Grafana 連接
check_grafana() {
    log "檢查 Grafana 連接..."
    if curl -s -u "$GRAFANA_USER:$GRAFANA_PASSWORD" "$GRAFANA_URL/api/health" > /dev/null; then
        log "Grafana 連接成功"
        return 0
    else
        error "Grafana 連接失敗，請檢查 URL 和憑證"
        return 1
    fi
}

# 安裝 ClickHouse 數據源插件
install_clickhouse_plugin() {
    log "安裝 ClickHouse 數據源插件..."
    
    # 檢查插件是否已安裝
    PLUGINS=$(curl -s -u "$GRAFANA_USER:$GRAFANA_PASSWORD" "$GRAFANA_URL/api/plugins")
    
    if echo "$PLUGINS" | grep -q "vertamedia-clickhouse-datasource"; then
        log "ClickHouse 插件已安裝"
    else
        log "需要手動安裝 ClickHouse 插件"
        echo "請在 Grafana 中執行以下步驟："
        echo "1. 進入 Configuration > Plugins"
        echo "2. 搜索 'ClickHouse'"
        echo "3. 安裝 'ClickHouse' 數據源插件"
        echo "4. 重新運行此腳本"
        return 1
    fi
}

# 創建 ClickHouse 數據源
create_datasource() {
    log "創建 ClickHouse 數據源..."
    
    DATASOURCE_CONFIG=$(cat << EOF
{
  "name": "ClickHouse-HFT",
  "type": "vertamedia-clickhouse-datasource",
  "access": "proxy",
  "url": "$CLICKHOUSE_URL",
  "database": "hft",
  "basicAuth": false,
  "isDefault": true,
  "jsonData": {
    "timeout": 10,
    "defaultDatabase": "hft",
    "usePOST": false
  }
}
EOF
)

    RESPONSE=$(curl -s -w "%{http_code}" -u "$GRAFANA_USER:$GRAFANA_PASSWORD" \
        -X POST "$GRAFANA_URL/api/datasources" \
        -H "Content-Type: application/json" \
        -d "$DATASOURCE_CONFIG")
    
    HTTP_CODE="${RESPONSE: -3}"
    BODY="${RESPONSE%???}"
    
    if [[ "$HTTP_CODE" == "200" ]]; then
        log "ClickHouse 數據源創建成功"
    elif [[ "$HTTP_CODE" == "409" ]]; then
        log "ClickHouse 數據源已存在"
    else
        error "創建數據源失敗: HTTP $HTTP_CODE"
        echo "$BODY"
        return 1
    fi
}

# 導入 Dashboard
import_dashboard() {
    log "導入 HFT Dashboard..."
    
    if [[ ! -f "grafana-dashboard.json" ]]; then
        error "找不到 grafana-dashboard.json 文件"
        return 1
    fi
    
    DASHBOARD_CONFIG=$(cat << EOF
{
  "dashboard": $(cat grafana-dashboard.json | jq '.dashboard'),
  "overwrite": true,
  "inputs": [
    {
      "name": "DS_CLICKHOUSE",
      "type": "datasource",
      "pluginId": "vertamedia-clickhouse-datasource",
      "value": "ClickHouse-HFT"
    }
  ]
}
EOF
)

    RESPONSE=$(curl -s -w "%{http_code}" -u "$GRAFANA_USER:$GRAFANA_PASSWORD" \
        -X POST "$GRAFANA_URL/api/dashboards/import" \
        -H "Content-Type: application/json" \
        -d "$DASHBOARD_CONFIG")
    
    HTTP_CODE="${RESPONSE: -3}"
    BODY="${RESPONSE%???}"
    
    if [[ "$HTTP_CODE" == "200" ]]; then
        log "Dashboard 導入成功"
        DASHBOARD_URL=$(echo "$BODY" | jq -r '.importedUrl // empty')
        if [[ -n "$DASHBOARD_URL" ]]; then
            log "Dashboard URL: $GRAFANA_URL$DASHBOARD_URL"
        fi
    else
        error "導入 Dashboard 失敗: HTTP $HTTP_CODE"
        echo "$BODY"
        return 1
    fi
}

# 創建簡化版本的 Dashboard（手動配置）
create_simple_dashboard() {
    log "創建簡化版 Dashboard..."
    
cat << 'EOF'
==== 手動創建 Dashboard 步驟 ====

1. 登錄 Grafana (http://localhost:3000)
   用戶名: admin
   密碼: sonic0923

2. 創建 ClickHouse 數據源:
   - 進入 Configuration > Data Sources
   - 點擊 "Add data source"
   - 選擇 "ClickHouse"
   - 設置:
     * Name: ClickHouse-HFT
     * URL: http://localhost:8123
     * Database: hft
   - 點擊 "Save & Test"

3. 創建 Dashboard:
   - 進入 "+" > Dashboard
   - 添加 Panel，使用以下查詢:

Panel 1 - 總事件數:
SELECT COUNT(*) FROM hft.raw_ws_events WHERE timestamp >= now() - INTERVAL 1 HOUR

Panel 2 - 按交易所統計:
SELECT venue, COUNT(*) as events FROM hft.raw_ws_events 
WHERE timestamp >= now() - INTERVAL 1 HOUR 
GROUP BY venue

Panel 3 - 交易量前10:
SELECT symbol, venue, COUNT(*) as trades FROM hft.raw_ws_events 
WHERE timestamp >= now() - INTERVAL 1 HOUR AND channel = 'trade' 
GROUP BY symbol, venue ORDER BY trades DESC LIMIT 10

Panel 4 - 數據流速率:
SELECT 
  toStartOfMinute(toDateTime(intDiv(timestamp, 1000000))) as time,
  venue,
  COUNT(*) as events_per_minute
FROM hft.raw_ws_events 
WHERE timestamp >= now() - INTERVAL 1 HOUR 
GROUP BY time, venue 
ORDER BY time

====================================
EOF
}

# 主函數
main() {
    log "開始配置 Grafana HFT Dashboard..."
    
    if ! check_grafana; then
        exit 1
    fi
    
    # 嘗試自動配置，如果失敗則提供手動步驟
    if ! create_datasource; then
        log "自動配置失敗，提供手動配置步驟..."
        create_simple_dashboard
        exit 1
    fi
    
    log "✅ Grafana 配置完成!"
    log "🎯 訪問 Dashboard: $GRAFANA_URL"
    log "👤 用戶名: $GRAFANA_USER"
    log "🔑 密碼: $GRAFANA_PASSWORD"
    
    # 顯示一些數據統計
    log ""
    log "📊 當前數據統計:"
    TOTAL_EVENTS=$(curl -s "$CLICKHOUSE_URL/?query=SELECT%20COUNT(*)%20FROM%20hft.raw_ws_events" || echo "N/A")
    RECENT_EVENTS=$(curl -s "$CLICKHOUSE_URL/?query=SELECT%20COUNT(*)%20FROM%20hft.raw_ws_events%20WHERE%20timestamp%20%3E%3D%20now()%20-%20INTERVAL%201%20HOUR" || echo "N/A")
    
    echo "   總事件數: $TOTAL_EVENTS"
    echo "   最近1小時: $RECENT_EVENTS"
}

# 檢查依賴
if ! command -v curl &> /dev/null; then
    error "需要安裝 curl"
    exit 1
fi

if ! command -v jq &> /dev/null; then
    log "建議安裝 jq 來更好地處理 JSON"
fi

main "$@"