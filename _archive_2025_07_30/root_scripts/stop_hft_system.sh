#!/bin/bash

# HFT 系統停止腳本
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

# 停止 Ops Workspace
stop_ops_workspace() {
    log_step "停止 Ops Workspace..."
    
    if [[ -f "logs/ops-workspace.pid" ]]; then
        OPS_PID=$(cat logs/ops-workspace.pid)
        if kill -0 $OPS_PID 2>/dev/null; then
            log_info "正在停止 Ops Workspace (PID: $OPS_PID)..."
            kill $OPS_PID
            
            # 等待進程結束
            for i in {1..10}; do
                if ! kill -0 $OPS_PID 2>/dev/null; then
                    break
                fi
                sleep 1
            done
            
            # 如果進程仍在運行，強制終止
            if kill -0 $OPS_PID 2>/dev/null; then
                log_warn "強制終止 Ops Workspace..."
                kill -9 $OPS_PID
            fi
            
            log_info "✅ Ops Workspace 已停止"
        else
            log_warn "Ops Workspace 進程不存在 (PID: $OPS_PID)"
        fi
        
        rm logs/ops-workspace.pid
    else
        log_info "未找到 Ops Workspace PID 文件"
    fi
    
    # 停止相關的 Python 進程
    if pgrep -f "real_latency_guard" > /dev/null; then
        log_info "停止相關的監控代理進程..."
        pkill -f "real_latency_guard"
    fi
}

# 停止 Rust HFT 核心
stop_rust_core() {
    log_step "停止 Rust HFT 核心..."
    
    if [[ -f "logs/hft-core.pid" ]]; then
        HFT_PID=$(cat logs/hft-core.pid)
        if kill -0 $HFT_PID 2>/dev/null; then
            log_info "正在停止 HFT 核心引擎 (PID: $HFT_PID)..."
            kill $HFT_PID
            
            # 等待進程結束
            for i in {1..10}; do
                if ! kill -0 $HFT_PID 2>/dev/null; then
                    break
                fi
                sleep 1
            done
            
            # 如果進程仍在運行，強制終止
            if kill -0 $HFT_PID 2>/dev/null; then
                log_warn "強制終止 HFT 核心引擎..."
                kill -9 $HFT_PID
            fi
            
            log_info "✅ HFT 核心引擎已停止"
        else
            log_warn "HFT 核心引擎進程不存在 (PID: $HFT_PID)"
        fi
        
        rm logs/hft-core.pid
    else
        log_info "未找到 HFT 核心引擎 PID 文件"
    fi
}

# 停止基礎設施服務
stop_infrastructure() {
    log_step "停止基礎設施服務..."
    
    cd rust_hft
    
    # 檢查是否有 Docker 服務在運行
    if docker-compose ps | grep -q "Up"; then
        log_info "正在停止 Docker 服務..."
        docker-compose down
        log_info "✅ Docker 服務已停止"
    else
        log_info "沒有運行中的 Docker 服務"
    fi
    
    cd ..
}

# 清理臨時文件
cleanup_temp_files() {
    log_step "清理臨時文件..."
    
    # 清理 PID 文件
    find logs -name "*.pid" -delete 2>/dev/null || true
    
    # 清理臨時日誌
    find logs -name "*.tmp" -delete 2>/dev/null || true
    
    log_info "✅ 臨時文件清理完成"
}

# 顯示停止後的狀態
show_stop_status() {
    log_step "系統停止狀態"
    
    echo ""
    echo "🛑 HFT 系統已停止"
    echo ""
    echo "📊 服務狀態:"
    
    # 檢查 Redis 狀態
    if redis-cli ping &> /dev/null; then
        echo -e "  ⚠️ Redis: ${YELLOW}仍在運行${NC} (可能被其他應用使用)"
    else
        echo -e "  ✅ Redis: ${GREEN}已停止${NC}"
    fi
    
    # 檢查 ClickHouse 狀態
    if curl -s http://localhost:8123 &> /dev/null; then
        echo -e "  ⚠️ ClickHouse: ${YELLOW}仍在運行${NC} (可能被其他應用使用)"
    else
        echo -e "  ✅ ClickHouse: ${GREEN}已停止${NC}"
    fi
    
    # 檢查 HFT 核心狀態
    if pgrep -f "hft-core" > /dev/null; then
        echo -e "  ⚠️ HFT 核心: ${YELLOW}仍有相關進程${NC}"
    else
        echo -e "  ✅ HFT 核心: ${GREEN}已停止${NC}"
    fi
    
    # 檢查 Ops 監控狀態
    if pgrep -f "real_latency_guard" > /dev/null; then
        echo -e "  ⚠️ Ops 監控: ${YELLOW}仍有相關進程${NC}"
    else
        echo -e "  ✅ Ops 監控: ${GREEN}已停止${NC}"
    fi
    
    echo ""
    echo "📁 日誌文件保留在 logs/ 目錄中"
    echo "🔧 重新啟動: ./start_hft_system.sh"
    echo ""
}

# 強制停止所有相關進程
force_stop_all() {
    log_step "強制停止所有相關進程..."
    
    # 停止所有可能的 HFT 相關進程
    pkill -f "hft-core" 2>/dev/null || true
    pkill -f "real_latency_guard" 2>/dev/null || true
    pkill -f "ml_trainer" 2>/dev/null || true
    
    # 等待進程終止
    sleep 2
    
    # 強制終止仍在運行的進程
    pkill -9 -f "hft-core" 2>/dev/null || true
    pkill -9 -f "real_latency_guard" 2>/dev/null || true
    pkill -9 -f "ml_trainer" 2>/dev/null || true
    
    log_info "✅ 強制停止完成"
}

# 主函數
main() {
    echo "🛑 HFT 系統停止腳本 v4.0"
    echo "==============================="
    echo ""
    
    # 檢查是否在正確的目錄
    if [[ ! -f "CLAUDE.md" ]] || [[ ! -d "rust_hft" ]] || [[ ! -d "ops_workspace" ]]; then
        log_error "請在 HFT 項目根目錄執行此腳本"
        exit 1
    fi
    
    # 檢查是否需要強制停止
    if [[ "$1" == "--force" ]]; then
        log_warn "執行強制停止模式"
        force_stop_all
    fi
    
    # 正常停止流程
    stop_ops_workspace
    stop_rust_core
    stop_infrastructure
    cleanup_temp_files
    show_stop_status
    
    log_info "🎯 HFT 系統停止完成！"
}

# 顯示幫助信息
show_help() {
    echo "HFT 系統停止腳本"
    echo ""
    echo "用法:"
    echo "  $0            正常停止所有服務"
    echo "  $0 --force    強制停止所有相關進程"
    echo "  $0 --help     顯示此幫助信息"
    echo ""
}

# 處理命令行參數
case "$1" in
    --help|-h)
        show_help
        exit 0
        ;;
    *)
        main "$@"
        ;;
esac