#!/bin/bash
# 
# Agno Workspaces 自動化部署腳本 v2.0
# ===================================
# 
# 功能:
# - 自動確認部署 (無需手動輸入 Y)
# - 錯誤恢復和重試機制
# - 並行啟動優化
# - 完整的日誌記錄

set -euo pipefail

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

# 工作區列表
WORKSPACES=("ops_workspace" "ml_workspace" "master_workspace")

# 最大重試次數
MAX_RETRIES=3

# 清理函數
cleanup() {
    log_info "正在清理部署過程中的臨時文件..."
    pkill -f "ag ws up" 2>/dev/null || true
    sleep 2
}

# 信號處理
trap cleanup EXIT INT TERM

# 檢查依賴
check_dependencies() {
    log_info "檢查系統依賴..."
    
    if ! command -v ag &> /dev/null; then
        log_error "Agno CLI 未安裝，請先安裝 Agno"
        exit 1
    fi
    
    if ! command -v docker &> /dev/null; then
        log_error "Docker 未安裝，請先安裝 Docker"
        exit 1
    fi
    
    log_success "依賴檢查通過"
}

# 停止現有的工作區
stop_existing_workspaces() {
    log_info "停止現有的工作區..."
    
    for workspace in "${WORKSPACES[@]}"; do
        if [ -d "$workspace" ]; then
            log_info "停止 $workspace..."
            cd "$workspace"
            
            # 嘗試優雅停止
            timeout 30s ag ws down --env dev 2>/dev/null || {
                log_warning "$workspace 優雅停止失敗，強制停止..."
                docker stop "hft-${workspace}-ui" "hft-${workspace}-api" 2>/dev/null || true
                docker rm "hft-${workspace}-ui" "hft-${workspace}-api" 2>/dev/null || true
            }
            
            cd ..
        fi
    done
    
    log_success "現有工作區已停止"
}

# 部署單個工作區
deploy_workspace() {
    local workspace=$1
    local retry_count=0
    
    log_info "部署 $workspace..."
    
    if [ ! -d "$workspace" ]; then
        log_error "工作區目錄 $workspace 不存在"
        return 1
    fi
    
    cd "$workspace"
    
    while [ $retry_count -lt $MAX_RETRIES ]; do
        log_info "嘗試部署 $workspace (第 $((retry_count + 1)) 次)..."
        
        # 創建日誌目錄
        mkdir -p "../logs"
        
        # 使用 expect 或 timeout 自動確認
        if timeout 300s bash -c "echo 'Y' | ag ws up --env dev" > "../logs/${workspace}_deploy.log" 2>&1; then
            log_success "$workspace 部署成功"
            cd ..
            return 0
        else
            retry_count=$((retry_count + 1))
            log_warning "$workspace 部署失敗，正在準備重試..."
            
            # 清理失敗的容器
            docker stop "hft-${workspace}-ui" "hft-${workspace}-api" 2>/dev/null || true
            docker rm "hft-${workspace}-ui" "hft-${workspace}-api" 2>/dev/null || true
            
            if [ $retry_count -lt $MAX_RETRIES ]; then
                log_info "等待 10 秒後重試..."
                sleep 10
            fi
        fi
    done
    
    log_error "$workspace 部署失敗，已達到最大重試次數"
    cd ..
    return 1
}

# 驗證部署
verify_deployment() {
    local workspace=$1
    local ui_port=$2
    local api_port=$3
    
    log_info "驗證 $workspace 部署狀態..."
    
    # 等待服務啟動
    local max_wait=60
    local wait_time=0
    
    while [ $wait_time -lt $max_wait ]; do
        if curl -s -f "http://localhost:$ui_port/" > /dev/null 2>&1; then
            log_success "$workspace UI 可用 (端口 $ui_port)"
            return 0
        fi
        
        sleep 2
        wait_time=$((wait_time + 2))
    done
    
    log_warning "$workspace UI 可能還在啟動中 (端口 $ui_port)"
    return 1
}

# 主函數
main() {
    echo "🚀 Agno Workspaces 自動化部署"
    echo "============================="
    echo
    
    check_dependencies
    
    # 停止現有服務
    stop_existing_workspaces
    
    # 創建日誌目錄
    mkdir -p logs
    
    # 部署工作區
    local failed_workspaces=()
    
    for workspace in "${WORKSPACES[@]}"; do
        if ! deploy_workspace "$workspace"; then
            failed_workspaces+=("$workspace")
        fi
    done
    
    # 等待服務啟動
    log_info "等待服務啟動..."
    sleep 30
    
    # 驗證部署
    log_info "驗證部署狀態..."
    verify_deployment "ops_workspace" 8501 8000
    verify_deployment "ml_workspace" 8502 8001  
    verify_deployment "master_workspace" 8503 8002
    
    # 總結
    echo
    echo "🎉 部署完成總結"
    echo "=============="
    
    if [ ${#failed_workspaces[@]} -eq 0 ]; then
        log_success "所有工作區部署成功！"
        echo
        echo "📊 可用服務:"
        echo "• Ops Dashboard:    http://localhost:8501"
        echo "• ML Dashboard:     http://localhost:8502"
        echo "• Master Dashboard: http://localhost:8503"
        echo
        echo "📊 API 端點:"
        echo "• Ops API:          http://localhost:8000/docs"
        echo "• ML API:           http://localhost:8001/docs"
        echo "• Master API:       http://localhost:8002/docs"
    else
        log_warning "部分工作區部署失敗:"
        for workspace in "${failed_workspaces[@]}"; do
            echo "  ❌ $workspace"
        done
        echo
        echo "📝 檢查日誌: logs/<workspace>_deploy.log"
    fi
    
    echo
    echo "🛑 停止所有服務: ag ws down --env dev (在各個工作區目錄中)"
}

# 執行主函數
main "$@"