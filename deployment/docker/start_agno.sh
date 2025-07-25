#!/bin/bash
# Agno HFT 控制平面啟動腳本
# 管理 HFT Operations Agent 和 ML Workflow Agent 的生命週期

set -euo pipefail

# 設置環境變量
export PYTHONPATH="/app:${PYTHONPATH:-}"
export AGNO_LOG_LEVEL="${LOG_LEVEL:-INFO}"
export AGNO_WORK_DIR="/app"

# 日誌函數
log() {
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] $*" >&2
}

# 清理函數
cleanup() {
    log "🛑 收到終止信號，開始優雅關閉..."
    
    # 終止後台進程
    if [[ -n "${HFT_OPS_PID:-}" ]]; then
        log "⏹️  停止 HFT Operations Agent (PID: $HFT_OPS_PID)"
        kill -TERM "$HFT_OPS_PID" 2>/dev/null || true
    fi
    
    if [[ -n "${ML_WORKFLOW_PID:-}" ]]; then
        log "⏹️  停止 ML Workflow Agent (PID: $ML_WORKFLOW_PID)"
        kill -TERM "$ML_WORKFLOW_PID" 2>/dev/null || true
    fi
    
    # 等待進程結束
    wait 2>/dev/null || true
    
    log "✅ 優雅關閉完成"
    exit 0
}

# 設置信號處理
trap cleanup SIGTERM SIGINT

# 初始化函數
initialize() {
    log "🚀 初始化 Agno HFT 控制平面..."
    
    # 創建必要目錄
    mkdir -p /app/logs/{hft_ops,ml_workflow,system}
    mkdir -p /app/models/{staging,production}
    mkdir -p /app/outputs/{training,evaluation}
    
    # 設置日誌權限
    chmod 755 /app/logs
    chmod 755 /app/models
    chmod 755 /app/outputs
    
    log "📁 目錄結構創建完成"
}

# 等待依賴服務
wait_for_services() {
    log "⏳ 等待依賴服務啟動..."
    
    # 等待 Redis
    local redis_timeout=30
    local redis_count=0
    
    while ! python3 -c "
import redis
try:
    r = redis.Redis(host='redis', port=6379, db=0, socket_timeout=3)
    r.ping()
    print('Redis connected')
except Exception as e:
    print(f'Redis failed: {e}')
    exit(1)
" 2>/dev/null; do
        redis_count=$((redis_count + 1))
        if [[ $redis_count -gt $redis_timeout ]]; then
            log "❌ Redis 連接超時"
            exit 1
        fi
        sleep 1
    done
    
    log "✅ Redis 連接成功"
    
    # 等待 ClickHouse
    local ch_timeout=60
    local ch_count=0
    
    while ! curl -sf "http://clickhouse:8123/ping" >/dev/null 2>&1; do
        ch_count=$((ch_count + 1))
        if [[ $ch_count -gt $ch_timeout ]]; then
            log "❌ ClickHouse 連接超時"
            exit 1
        fi
        sleep 1
    done
    
    log "✅ ClickHouse 連接成功"
    
    # 等待 Rust 引擎（可選）
    if curl -sf "http://rust-engine:8080/health" >/dev/null 2>&1; then
        log "✅ Rust 引擎連接成功"
    else
        log "⚠️  Rust 引擎未連接，但繼續啟動"
    fi
}

# 啟動 HFT Operations Agent
start_hft_operations() {
    log "🏭 啟動 HFT Operations Agent..."
    
    cd /app
    python3 -u hft_operations_agent.py > /app/logs/hft_ops/hft_ops.log 2>&1 &
    HFT_OPS_PID=$!
    
    log "🏭 HFT Operations Agent 已啟動 (PID: $HFT_OPS_PID)"
    
    # 檢查進程是否正常運行
    sleep 3
    if ! kill -0 "$HFT_OPS_PID" 2>/dev/null; then
        log "❌ HFT Operations Agent 啟動失敗"
        exit 1
    fi
}

# 啟動 ML Workflow Agent
start_ml_workflow() {
    log "🧠 啟動 ML Workflow Agent..."
    
    cd /app
    python3 -u ml_workflow_agent.py > /app/logs/ml_workflow/ml_workflow.log 2>&1 &
    ML_WORKFLOW_PID=$!
    
    log "🧠 ML Workflow Agent 已啟動 (PID: $ML_WORKFLOW_PID)"
    
    # 檢查進程是否正常運行
    sleep 3
    if ! kill -0 "$ML_WORKFLOW_PID" 2>/dev/null; then
        log "❌ ML Workflow Agent 啟動失敗"
        exit 1
    fi
}

# 健康監控循環
health_monitor() {
    log "🏥 啟動健康監控循環..."
    
    while true; do
        sleep 30
        
        # 檢查 HFT Operations Agent
        if [[ -n "${HFT_OPS_PID:-}" ]] && ! kill -0 "$HFT_OPS_PID" 2>/dev/null; then
            log "❌ HFT Operations Agent 進程異常，嘗試重啟..."
            start_hft_operations
        fi
        
        # 檢查 ML Workflow Agent
        if [[ -n "${ML_WORKFLOW_PID:-}" ]] && ! kill -0 "$ML_WORKFLOW_PID" 2>/dev/null; then
            log "❌ ML Workflow Agent 進程異常，嘗試重啟..."
            start_ml_workflow
        fi
        
        # 記錄系統狀態
        if [[ $(($(date +%s) % 300)) -eq 0 ]]; then  # 每5分鐘記錄一次
            log "📊 系統狀態: HFT_OPS(${HFT_OPS_PID:-停止}), ML_WORKFLOW(${ML_WORKFLOW_PID:-停止})"
        fi
    done
}

# 主函數
main() {
    log "🎯 Agno HFT 控制平面啟動中..."
    log "🏗️  架構: 雙平面 HFT (Rust執行 + Python控制)"
    log "📋 模式: ${AGNO_MODE:-development}"
    
    # 初始化
    initialize
    
    # 等待依賴服務
    wait_for_services
    
    # 啟動核心代理
    start_hft_operations
    start_ml_workflow
    
    log "🎉 所有代理啟動完成！"
    log "📊 監控端口: HFT Operations 和 ML Workflow 代理已就緒"
    
    # 進入健康監控循環
    health_monitor
}

# 檢查運行模式
if [[ "${1:-}" == "debug" ]]; then
    log "🐛 調試模式啟動"
    set -x
fi

# 啟動主函數
main "$@"