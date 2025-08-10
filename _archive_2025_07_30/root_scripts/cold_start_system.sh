#!/bin/bash

# HFT 系統標準化冷啟動腳本
# ===========================
# 
# 基於 Agno 官方標準的多 workspace 冷啟動流程
# 每個 workspace 獨立運行，符合官方最佳實踐

set -e  # 任何命令失敗時退出

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 日誌函數
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

# 檢查依賴
check_dependencies() {
    log_info "檢查系統依賴..."
    
    # 檢查 Docker
    if ! command -v docker &> /dev/null; then
        log_error "Docker 未安裝"
        exit 1
    fi
    
    # 檢查 Docker Compose
    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose 未安裝"
        exit 1
    fi
    
    # 檢查 Rust
    if ! command -v cargo &> /dev/null; then
        log_error "Rust/Cargo 未安裝"
        exit 1
    fi
    
    # 檢查 Python
    if ! command -v python3 &> /dev/null; then
        log_error "Python3 未安裝"
        exit 1
    fi
    
    # 檢查 Agno
    if ! command -v ag &> /dev/null; then
        log_error "Agno 未安裝，請運行: pip install -U 'agno[aws]'"
        exit 1
    fi
    
    log_success "依賴檢查通過"
}

# 啟動基礎設施
start_infrastructure() {
    log_info "啟動基礎設施 (Redis + ClickHouse)..."
    
    # 使用 deployment/docker-compose.yml 啟動基礎設施
    if [ -f "deployment/docker-compose.yml" ]; then
        cd deployment
        docker-compose up -d redis clickhouse
        cd ..
    else
        log_warning "deployment/docker-compose.yml 不存在，嘗試其他位置..."
        
        # 檢查 rust_hft 目錄
        if [ -f "rust_hft/docker-compose.yml" ]; then
            cd rust_hft
            docker-compose up -d
            cd ..
        else
            log_error "找不到 docker-compose.yml 文件"
            exit 1
        fi
    fi
    
    # 等待服務啟動
    log_info "等待基礎設施啟動..."
    sleep 10
    
    # 檢查 Redis
    if ! redis-cli ping > /dev/null 2>&1; then
        log_error "Redis 連接失敗"
        exit 1
    fi
    
    # 檢查 ClickHouse
    if ! curl -s http://localhost:8123/ > /dev/null; then
        log_error "ClickHouse 連接失敗"
        exit 1
    fi
    
    log_success "基礎設施啟動成功"
}

# 啟動 Rust HFT 核心 (後台進程)
start_rust_core() {
    log_info "啟動 Rust HFT 核心..."
    
    cd rust_hft
    
    # 編譯
    log_info "編譯 Rust 代碼..."
    cargo build --release
    
    if [ $? -ne 0 ]; then
        log_error "Rust 編譯失敗"
        exit 1
    fi
    
    # 啟動後台進程
    log_info "啟動 Rust 市場數據收集器 (後台進程)..."
    nohup cargo run --release --bin market_data_collector > ../logs/rust_hft.log 2>&1 &
    RUST_PID=$!
    echo $RUST_PID > ../logs/rust_hft.pid
    
    cd ..
    
    # 等待啟動
    sleep 5
    
    # 檢查進程是否存活
    if ! kill -0 $RUST_PID 2>/dev/null; then
        log_error "Rust 核心啟動失敗"
        exit 1
    fi
    
    log_success "Rust HFT 核心啟動成功 (PID: $RUST_PID)"
}

# 啟動 Agno Workspaces (各自獨立運行)
start_workspaces() {
    log_info "啟動 Agno Workspaces..."
    
    # 創建日誌目錄
    mkdir -p logs
    
    # 1. 啟動 Ops Workspace (port 8501 UI, 8000 API)
    log_info "啟動 Ops Workspace..."
    cd ops_workspace
    ag set > /dev/null 2>&1
    nohup ag ws up --env dev > ../logs/ops_workspace.log 2>&1 &
    OPS_PID=$!
    echo $OPS_PID > ../logs/ops_workspace.pid
    cd ..
    
    # 等待啟動
    sleep 10
    
    # 2. 啟動 ML Workspace (port 8502 UI, 8001 API)
    log_info "啟動 ML Workspace..."
    cd ml_workspace
    ag set > /dev/null 2>&1
    nohup ag ws up --env dev > ../logs/ml_workspace.log 2>&1 &
    ML_PID=$!
    echo $ML_PID > ../logs/ml_workspace.pid
    cd ..
    
    # 等待啟動
    sleep 10
    
    # 3. 啟動 Master Workspace (port 8503 UI, 8002 API)
    log_info "啟動 Master Workspace..."
    cd master_workspace
    ag set > /dev/null 2>&1
    nohup ag ws up --env dev > ../logs/master_workspace.log 2>&1 &
    MASTER_PID=$!
    echo $MASTER_PID > ../logs/master_workspace.pid
    cd ..
    
    # 等待所有服務啟動
    sleep 15
    
    log_success "所有 Agno Workspaces 啟動完成"
}

# 驗證系統狀態
verify_system() {
    log_info "驗證系統狀態..."
    
    # 檢查基礎設施
    log_info "檢查基礎設施..."
    
    # Redis
    if redis-cli ping > /dev/null 2>&1; then
        log_success "✅ Redis: 正常"
    else
        log_error "❌ Redis: 連接失敗"
    fi
    
    # ClickHouse
    if curl -s http://localhost:8123/ping > /dev/null; then
        log_success "✅ ClickHouse: 正常"
    else
        log_error "❌ ClickHouse: 連接失敗"
    fi
    
    # 檢查 Rust 核心
    if [ -f "logs/rust_hft.pid" ]; then
        RUST_PID=$(cat logs/rust_hft.pid)
        if kill -0 $RUST_PID 2>/dev/null; then
            log_success "✅ Rust HFT 核心: 正常 (PID: $RUST_PID)"
        else
            log_error "❌ Rust HFT 核心: 進程不存在"
        fi
    else
        log_error "❌ Rust HFT 核心: PID 文件不存在"
    fi
    
    # 檢查 Agno Workspaces (通過端口檢查)
    log_info "檢查 Agno Workspaces..."
    
    # Ops Workspace (8501 UI, 8000 API)
    if curl -s http://localhost:8501/ > /dev/null; then
        log_success "✅ Ops Workspace UI: http://localhost:8501"
    else
        log_warning "⚠️ Ops Workspace UI: 不可用"
    fi
    
    if curl -s http://localhost:8000/docs > /dev/null; then
        log_success "✅ Ops Workspace API: http://localhost:8000/docs"
    else
        log_warning "⚠️ Ops Workspace API: 不可用"
    fi
    
    # ML Workspace (8502 UI, 8001 API)  
    sleep 5  # 等待 Docker 容器完全啟動
    if curl -s --max-time 5 http://localhost:8502/ > /dev/null; then
        log_success "✅ ML Workspace UI: http://localhost:8502"
    else
        log_warning "⚠️ ML Workspace UI: 啟動中或不可用"
    fi
    
    if curl -s --max-time 5 http://localhost:8001/docs > /dev/null; then
        log_success "✅ ML Workspace API: http://localhost:8001/docs"
    else
        log_warning "⚠️ ML Workspace API: 啟動中或不可用"
    fi
    
    # Master Workspace (8503 UI, 8002 API)
    if curl -s --max-time 5 http://localhost:8503/ > /dev/null; then
        log_success "✅ Master Workspace UI: http://localhost:8503"
    else
        log_warning "⚠️ Master Workspace UI: 啟動中或不可用"
    fi
    
    if curl -s --max-time 5 http://localhost:8002/docs > /dev/null; then
        log_success "✅ Master Workspace API: http://localhost:8002/docs"
    else
        log_warning "⚠️ Master Workspace API: 啟動中或不可用"
    fi
}

# 顯示系統信息
show_system_info() {
    echo ""
    echo "🎉 HFT 系統冷啟動完成！"
    echo "=========================="
    echo ""
    echo "📊 系統組件:"
    echo "• Rust HFT 核心: 市場數據收集和交易執行"
    echo "• Ops Workspace: 運維監控和告警管理"
    echo "• ML Workspace: 機器學習模型訓練"
    echo "• Master Workspace: 系統統一控制"
    echo ""
    echo "🌐 可用服務:"
    echo "• Ops Dashboard:    http://localhost:8501"
    echo "• Ops API:          http://localhost:8000/docs"
    echo "• ML Dashboard:     http://localhost:8502"
    echo "• ML API:           http://localhost:8001/docs"
    echo "• Master Dashboard: http://localhost:8503"
    echo "• Master API:       http://localhost:8002/docs"
    echo ""
    echo "🔧基礎設施:"
    echo "• Redis:            localhost:6379"
    echo "• ClickHouse:       http://localhost:8123"
    echo ""
    echo "📝 日誌位置:"
    echo "• Rust HFT:         logs/rust_hft.log"
    echo "• Ops Workspace:    logs/ops_workspace.log"
    echo "• ML Workspace:     logs/ml_workspace.log"
    echo "• Master Workspace: logs/master_workspace.log"
    echo ""
    echo "🛑 停止系統: ./stop_hft_system.sh"
    echo ""
}

# 主函數
main() {
    echo "🚀 HFT 系統標準化冷啟動"
    echo "======================="
    echo ""
    
    # 切換到腳本目錄
    cd "$(dirname "$0")"
    
    # 執行啟動流程
    check_dependencies
    echo ""
    
    start_infrastructure
    echo ""
    
    start_rust_core
    echo ""
    
    start_workspaces
    echo ""
    
    verify_system
    echo ""
    
    show_system_info
}

# 錯誤處理
trap 'log_error "啟動過程中發生錯誤，正在清理..."; exit 1' ERR

# 執行主函數
main "$@"