#!/bin/bash

# HFT 系統一鍵啟動腳本
# 作者: HFT System Team
# 版本: 4.0
# 日期: 2025-07-25

set -e  # 遇到錯誤立即退出

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 日誌函數
log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_step() {
    echo -e "${BLUE}[STEP]${NC} $1"
}

# 檢查是否在正確的目錄
check_directory() {
    if [[ ! -f "CLAUDE.md" ]] || [[ ! -d "rust_hft" ]] || [[ ! -d "ops_workspace" ]]; then
        log_error "請在 HFT 項目根目錄執行此腳本"
        exit 1
    fi
    log_info "✅ 目錄檢查通過"
}

# 創建必要的目錄
create_directories() {
    log_step "創建日誌目錄..."
    mkdir -p logs
    mkdir -p rust_hft/logs
    mkdir -p ops_workspace/logs
    mkdir -p ml_workspace/logs
    log_info "✅ 目錄創建完成"
}

# 檢查依賴
check_dependencies() {
    log_step "檢查系統依賴..."
    
    # 檢查 Docker
    if ! command -v docker &> /dev/null; then
        log_error "Docker 未安裝，請先安裝 Docker"
        exit 1
    fi
    
    # 檢查 Docker Compose
    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose 未安裝，請先安裝 Docker Compose"
        exit 1
    fi
    
    # 檢查 Rust
    if ! command -v cargo &> /dev/null; then
        log_error "Rust 未安裝，請先安裝 Rust 工具鏈"
        exit 1
    fi
    
    # 檢查 Python
    if ! command -v python3 &> /dev/null; then
        log_error "Python3 未安裝，請先安裝 Python3"
        exit 1
    fi
    
    log_info "✅ 依賴檢查通過"
}

# 檢查系統資源
check_resources() {
    log_step "檢查系統資源..."
    
    # 檢查可用內存 (至少需要 4GB)
    available_mem=$(free -m | awk 'NR==2{printf "%.0f", $7}')
    if [[ $available_mem -lt 4096 ]]; then
        log_warn "可用內存較低 (${available_mem}MB)，建議至少 4GB"
    fi
    
    # 檢查磁盤空間 (至少需要 10GB)
    available_disk=$(df . | awk 'NR==2{print $4}')
    if [[ $available_disk -lt 10485760 ]]; then
        log_warn "磁盤空間較低，建議至少 10GB 可用空間"
    fi
    
    log_info "✅ 資源檢查完成"
}

# 啟動基礎設施
start_infrastructure() {
    log_step "啟動基礎設施 (Redis + ClickHouse)..."
    
    cd rust_hft
    
    # 檢查是否已有服務在運行
    if docker-compose ps | grep -q "Up"; then
        log_warn "發現已運行的服務，正在重啟..."
        docker-compose down
        sleep 2
    fi
    
    # 啟動服務
    docker-compose up -d redis clickhouse
    
    # 等待服務啟動
    log_info "等待服務啟動..."
    sleep 10
    
    # 驗證 Redis
    if redis-cli ping &> /dev/null; then
        log_info "✅ Redis 啟動成功"
    else
        log_error "❌ Redis 啟動失敗"
        exit 1
    fi
    
    # 驗證 ClickHouse
    if curl -s http://localhost:8123 | grep -q "Ok"; then
        log_info "✅ ClickHouse 啟動成功"
    else
        log_error "❌ ClickHouse 啟動失敗"
        exit 1
    fi
    
    cd ..
}

# 編譯並啟動 Rust HFT 核心
start_rust_core() {
    log_step "編譯並啟動 Rust HFT 核心..."
    
    cd rust_hft
    
    # 檢查是否已有進程在運行
    if [[ -f "../logs/hft-core.pid" ]] && kill -0 $(cat ../logs/hft-core.pid) 2>/dev/null; then
        log_warn "HFT 核心已在運行，正在重啟..."
        kill $(cat ../logs/hft-core.pid)
        rm ../logs/hft-core.pid
        sleep 2
    fi
    
    # 編譯
    log_info "正在編譯 Rust 代碼..."
    if cargo build --release > ../logs/rust-build.log 2>&1; then
        log_info "✅ Rust 編譯成功"
    else
        log_error "❌ Rust 編譯失敗，請檢查日誌: logs/rust-build.log"
        exit 1
    fi
    
    # 啟動核心引擎
    log_info "啟動 HFT 核心引擎..."
    cargo run --release --bin main > ../logs/hft-core.log 2>&1 &
    HFT_PID=$!
    echo $HFT_PID > ../logs/hft-core.pid
    
    # 等待啟動
    sleep 5
    
    # 驗證進程
    if kill -0 $HFT_PID 2>/dev/null; then
        log_info "✅ HFT 核心引擎啟動成功 (PID: $HFT_PID)"
    else
        log_error "❌ HFT 核心引擎啟動失敗"
        exit 1
    fi
    
    cd ..
}

# 啟動 Ops Workspace 監控
start_ops_monitoring() {
    log_step "啟動 Ops Workspace 監控..."
    
    cd ops_workspace
    
    # 檢查是否已有進程在運行
    if pgrep -f "ag ws" > /dev/null; then
        log_warn "Ops Workspace 已在運行，正在重啟..."
        pkill -f "ag ws"
        sleep 2
    fi
    
    # 檢查 Agno 和依賴
    if ! python3 -c "import agno" &> /dev/null; then
        log_warn "正在安裝 Agno 和依賴..."
        pip3 install -r requirements.txt > ../logs/pip-install.log 2>&1
    fi
    
    # 啟動 Ops Workspace 監控代理
    log_info "啟動 Ops Workspace 監控代理..."
    python3 agents/real_latency_guard.py > ../logs/ops-workspace.log 2>&1 &
    OPS_PID=$!
    echo $OPS_PID > ../logs/ops-workspace.pid
    
    # 等待啟動
    sleep 5
    
    # 驗證進程
    if kill -0 $OPS_PID 2>/dev/null; then
        log_info "✅ Ops Workspace 啟動成功 (PID: $OPS_PID)"
    else
        log_error "❌ Ops Workspace 啟動失敗，檢查日誌: logs/ops-workspace.log"
        exit 1
    fi
    
    cd ..
}

# 執行系統驗證
verify_system() {
    log_step "執行系統驗證測試..."
    
    # 執行快速集成測試
    if python3 test_cross_workspace_integration.py > logs/integration-test.log 2>&1; then
        log_info "✅ 系統集成測試通過"
    else
        log_warn "⚠️ 系統集成測試失敗，請檢查日誌: logs/integration-test.log"
    fi
    
    # 執行性能測試
    if python3 test_quick_performance.py > logs/performance-test.log 2>&1; then
        log_info "✅ 性能測試通過"
    else
        log_warn "⚠️ 性能測試失敗，請檢查日誌: logs/performance-test.log"
    fi
}

# 顯示系統狀態
show_system_status() {
    log_step "系統狀態總覽"
    
    echo ""
    echo "🎉 HFT 系統啟動完成！"
    echo ""
    echo "📊 服務狀態:"
    
    # Redis 狀態
    if redis-cli ping &> /dev/null; then
        echo -e "  ✅ Redis: ${GREEN}運行中${NC} (端口 6379)"
    else
        echo -e "  ❌ Redis: ${RED}未運行${NC}"
    fi
    
    # ClickHouse 狀態
    if curl -s http://localhost:8123 &> /dev/null; then
        echo -e "  ✅ ClickHouse: ${GREEN}運行中${NC} (端口 8123)"
    else
        echo -e "  ❌ ClickHouse: ${RED}未運行${NC}"
    fi
    
    # HFT 核心狀態
    if [[ -f "logs/hft-core.pid" ]] && kill -0 $(cat logs/hft-core.pid) 2>/dev/null; then
        echo -e "  ✅ HFT 核心: ${GREEN}運行中${NC} (PID: $(cat logs/hft-core.pid))"
    else
        echo -e "  ❌ HFT 核心: ${RED}未運行${NC}"
    fi
    
    # Ops 監控狀態
    if [[ -f "logs/ops-agent.pid" ]] && kill -0 $(cat logs/ops-agent.pid) 2>/dev/null; then
        echo -e "  ✅ Ops 監控: ${GREEN}運行中${NC} (PID: $(cat logs/ops-agent.pid))"
    else
        echo -e "  ❌ Ops 監控: ${RED}未運行${NC}"
    fi
    
    echo ""
    echo "📈 系統信息:"
    echo "  - 內存使用: $(free -h | awk 'NR==2{printf "%.1f%%", $3/$2*100}')"
    echo "  - 磁盤使用: $(df . | awk 'NR==2{printf "%.1f%%", $3/$2*100}')"
    echo "  - 負載均衡: $(uptime | awk -F'load average:' '{print $2}')"
    
    echo ""
    echo "🔧 管理命令:"
    echo "  - 查看 HFT 日誌: tail -f logs/hft-core.log"
    echo "  - 查看 Ops 日誌: tail -f logs/ops-agent.log"  
    echo "  - 停止系統: ./stop_hft_system.sh"
    echo "  - 系統監控: watch -n 2 'ps aux | grep -E \"(hft-core|real_latency_guard)\" | grep -v grep'"
    
    echo ""
    echo "🎯 系統已準備好處理交易！"
}

# 清理函數 (錯誤時調用)
cleanup_on_error() {
    log_error "啟動過程中發生錯誤，正在清理..."
    
    # 停止可能已啟動的進程
    if [[ -f "logs/hft-core.pid" ]]; then
        kill $(cat logs/hft-core.pid) 2>/dev/null || true
        rm logs/hft-core.pid 2>/dev/null || true
    fi
    
    if [[ -f "logs/ops-agent.pid" ]]; then
        kill $(cat logs/ops-agent.pid) 2>/dev/null || true
        rm logs/ops-agent.pid 2>/dev/null || true
    fi
    
    # 停止 Docker 服務
    cd rust_hft 2>/dev/null && docker-compose down 2>/dev/null || true
}

# 主函數
main() {
    echo "🚀 HFT 系統啟動腳本 v4.0"
    echo "================================"
    echo ""
    
    # 設置錯誤處理
    trap cleanup_on_error ERR
    
    # 執行啟動步驟
    check_directory
    create_directories
    check_dependencies
    check_resources
    start_infrastructure
    start_rust_core
    start_ops_monitoring
    verify_system
    show_system_status
    
    log_info "🎉 HFT 系統啟動完成！"
}

# 執行主函數
main "$@"