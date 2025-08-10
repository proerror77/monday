#!/bin/bash

# HFT 系統 Agno 標準啟動腳本
# ===========================

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
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

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

echo "🚀 HFT 系統 Agno 標準啟動"
echo "========================"
echo ""

# 切換到腳本目錄
cd "$(dirname "$0")"

# 檢查基礎依賴
check_dependencies() {
    log_info "檢查依賴..."
    
    if ! command -v docker &> /dev/null; then
        log_error "Docker 未安裝"
        exit 1
    fi
    
    if ! command -v ag &> /dev/null; then
        log_error "Agno 未安裝"
        exit 1
    fi
    
    if ! command -v cargo &> /dev/null; then
        log_error "Rust/Cargo 未安裝"
        exit 1
    fi
    
    log_success "依賴檢查通過"
}

# 啟動基礎設施 (簡化版)
start_infrastructure() {
    log_info "啟動基礎設施..."
    
    # 檢查 Redis 是否已運行
    if redis-cli ping > /dev/null 2>&1; then
        log_success "✅ Redis 已運行"
    else
        log_info "啟動 Redis..."
        if command -v brew &> /dev/null; then
            brew services start redis
        else
            log_warning "請手動啟動 Redis"
        fi
    fi
    
    # 檢查 ClickHouse 是否已運行
    if curl -s http://localhost:8123/ping > /dev/null 2>&1; then
        log_success "✅ ClickHouse 已運行"
    else
        log_warning "請確保 ClickHouse 正在運行"
        log_info "可以使用: docker run -d --name clickhouse-server -p 8123:8123 clickhouse/clickhouse-server"
    fi
}

# 啟動 Rust HFT 核心
start_rust_core() {
    log_info "啟動 Rust HFT 核心..."
    
    cd rust_hft
    
    # 創建日誌目錄
    mkdir -p ../logs
    
    # 編譯
    log_info "編譯 Rust 代碼..."
    if cargo build --release; then
        log_success "✅ Rust 編譯成功"
    else
        log_error "❌ Rust 編譯失敗"
        cd ..
        return 1
    fi
    
    # 啟動後台進程
    log_info "啟動市場數據收集器..."
    nohup cargo run --release --bin market_data_collector > ../logs/rust_hft.log 2>&1 &
    RUST_PID=$!
    echo $RUST_PID > ../logs/rust_hft.pid
    
    cd ..
    
    # 等待啟動
    sleep 3
    
    if kill -0 $RUST_PID 2>/dev/null; then
        log_success "✅ Rust HFT 核心啟動成功 (PID: $RUST_PID)"
        return 0
    else
        log_error "❌ Rust HFT 核心啟動失敗"
        return 1
    fi
}

# 啟動單個 Agno Workspace
start_workspace() {
    local ws_name=$1
    local display_name=$2
    
    log_info "啟動 $display_name..."
    
    if [ ! -d "$ws_name" ]; then
        log_error "❌ $ws_name 目錄不存在"
        return 1
    fi
    
    cd "$ws_name"
    
    # 設置為活動工作區
    ag set > /dev/null 2>&1
    
    # 創建日誌目錄
    mkdir -p ../logs
    
    # 啟動 workspace (後台)
    log_info "啟動 $ws_name (ag ws up)..."
    # 使用更強大的自動確認機制
    (echo "Y"; sleep 1; echo "Y"; sleep 1; echo "Y") | timeout 300s ag ws up --env dev > "../logs/${ws_name}.log" 2>&1 &
    local WS_PID=$!
    echo $WS_PID > "../logs/${ws_name}.pid"
    
    cd ..
    
    log_success "✅ $display_name 啟動命令已執行 (PID: $WS_PID)"
    
    return 0
}

# 主啟動流程
main() {
    # 檢查依賴
    check_dependencies
    echo ""
    
    # 啟動基礎設施
    start_infrastructure
    echo ""
    
    # 啟動 Rust 核心
    if start_rust_core; then
        echo ""
    else
        log_warning "Rust 核心啟動失敗，但繼續啟動其他服務"
        echo ""
    fi
    
    # 啟動 Agno Workspaces
    log_info "啟動 Agno Workspaces..."
    
    start_workspace "ops_workspace" "Ops 運維工作區"
    sleep 2
    
    start_workspace "ml_workspace" "ML 機器學習工作區"
    sleep 2
    
    start_workspace "master_workspace" "Master 主控制器"
    sleep 2
    
    echo ""
    log_info "等待所有服務啟動..."
    sleep 15
    
    # 顯示服務狀態
    show_services_status
}

# 顯示服務狀態
show_services_status() {
    echo ""
    log_info "檢查服務狀態..."
    echo ""
    
    # 檢查基礎設施
    if redis-cli ping > /dev/null 2>&1; then
        log_success "✅ Redis: 正常"
    else
        log_error "❌ Redis: 不可用"
    fi
    
    if curl -s --max-time 3 http://localhost:8123/ping > /dev/null 2>&1; then
        log_success "✅ ClickHouse: 正常"
    else
        log_error "❌ ClickHouse: 不可用"
    fi
    
    # 檢查 Rust 進程
    if [ -f "logs/rust_hft.pid" ]; then
        RUST_PID=$(cat logs/rust_hft.pid)
        if kill -0 $RUST_PID 2>/dev/null; then
            log_success "✅ Rust HFT 核心: 運行中 (PID: $RUST_PID)"
        else
            log_error "❌ Rust HFT 核心: 進程退出"
        fi
    else
        log_warning "⚠️ Rust HFT 核心: 未啟動"
    fi
    
    # 檢查 Agno Workspaces (通過端口)
    echo ""
    log_info "檢查 Agno Workspaces..."
    
    # 定義期望的端口
    declare -A workspace_ports=(
        ["ops_workspace"]="8501"
        ["ml_workspace"]="8502" 
        ["master_workspace"]="8503"
    )
    
    declare -A api_ports=(
        ["ops_workspace"]="8000"
        ["ml_workspace"]="8001"
        ["master_workspace"]="8002"
    )
    
    for ws in ops_workspace ml_workspace master_workspace; do
        ui_port=${workspace_ports[$ws]}
        api_port=${api_ports[$ws]}
        
        # 檢查 UI 端口
        if curl -s --max-time 3 http://localhost:$ui_port/ > /dev/null 2>&1; then
            log_success "✅ $ws UI: http://localhost:$ui_port"
        else
            log_warning "⚠️ $ws UI: 啟動中或不可用 (port $ui_port)"
        fi
        
        # 檢查 API 端口
        if curl -s --max-time 3 http://localhost:$api_port/docs > /dev/null 2>&1; then
            log_success "✅ $ws API: http://localhost:$api_port/docs"
        else
            log_warning "⚠️ $ws API: 啟動中或不可用 (port $api_port)"
        fi
    done
    
    echo ""
    echo "🎉 HFT 系統啟動完成！"
    echo "===================="
    echo ""
    echo "🌐 可用服務:"
    echo "• Master Dashboard:  http://localhost:8503"
    echo "• Ops Dashboard:     http://localhost:8501"
    echo "• ML Dashboard:      http://localhost:8502"
    echo ""
    echo "📝 日誌位置:"
    echo "• logs/rust_hft.log"
    echo "• logs/ops_workspace.log"
    echo "• logs/ml_workspace.log"
    echo "• logs/master_workspace.log"
    echo ""
    echo "🛑 停止系統: pkill -f 'cargo run' && pkill -f 'ag ws'"
    echo ""
}

# 執行主函數
main "$@"