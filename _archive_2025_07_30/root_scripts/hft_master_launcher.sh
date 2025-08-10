#!/bin/bash

# HFT系統Master Orchestrator啟動腳本
# 基於L0+L1+L2+L3分層架構的統一管理

set -euo pipefail

# =================== 全局配置 ===================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG_DIR="${SCRIPT_DIR}/logs"
COMPOSE_FILE="${SCRIPT_DIR}/docker-compose.master.yml"
HEALTH_CHECK_TIMEOUT=300  # 5分鐘
STARTUP_LOG="${LOG_DIR}/hft_startup_$(date +%Y%m%d_%H%M%S).log"

# 顏色輸出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# =================== 日誌函數 ===================

setup_logging() {
    mkdir -p "${LOG_DIR}"
    exec 1> >(tee -a "${STARTUP_LOG}")
    exec 2> >(tee -a "${STARTUP_LOG}" >&2)
}

log_info() {
    echo -e "${GREEN}[INFO]${NC} $(date '+%Y-%m-%d %H:%M:%S') - $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $(date '+%Y-%m-%d %H:%M:%S') - $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $(date '+%Y-%m-%d %H:%M:%S') - $1"
}

log_step() {
    echo -e "${BLUE}[STEP]${NC} $(date '+%Y-%m-%d %H:%M:%S') - $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $(date '+%Y-%m-%d %H:%M:%S') - $1"
}

# =================== 健康檢查函數 ===================

wait_for_service() {
    local service=$1
    local url=$2
    local timeout=${3:-60}
    local count=0
    
    log_step "等待服務 ${service} 就緒..."
    
    while [ $count -lt $timeout ]; do
        if curl -sf "$url" >/dev/null 2>&1; then
            log_success "${service} 服務就緒"
            return 0
        fi
        count=$((count + 1))
        sleep 1
        if [ $((count % 10)) -eq 0 ]; then
            log_info "${service} 等待中... (${count}/${timeout}s)"
        fi
    done
    
    log_error "${service} 服務啟動超時"
    return 1
}

wait_for_grpc_service() {
    local service=$1
    local address=$2
    local timeout=${3:-60}
    local count=0
    
    log_step "等待gRPC服務 ${service} 就緒..."
    
    while [ $count -lt $timeout ]; do
        if grpcurl -plaintext "$address" list >/dev/null 2>&1; then
            log_success "${service} gRPC服務就緒"
            return 0
        fi
        count=$((count + 1))
        sleep 1
        if [ $((count % 10)) -eq 0 ]; then
            log_info "${service} gRPC等待中... (${count}/${timeout}s)"
        fi
    done
    
    log_error "${service} gRPC服務啟動超時"
    return 1
}

check_prerequisites() {
    log_step "檢查系統先決條件..."
    
    # 檢查Docker
    if ! command -v docker &> /dev/null; then
        log_error "Docker未安裝"
        exit 1
    fi
    
    # 檢查Docker Compose
    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose未安裝"
        exit 1
    fi
    
    # 檢查grpcurl
    if ! command -v grpcurl &> /dev/null; then
        log_warn "grpcurl未安裝，將跳過gRPC健康檢查"
    fi
    
    # 檢查端口是否被占用
    local ports=(6379 8123 9000 9090 3000 50051 8080 8502 8503 8504)
    local occupied_ports=()
    
    for port in "${ports[@]}"; do
        if lsof -Pi :$port -sTCP:LISTEN -t >/dev/null 2>&1; then
            occupied_ports+=($port)
        fi
    done
    
    if [ ${#occupied_ports[@]} -ne 0 ]; then
        log_warn "以下端口被占用: ${occupied_ports[*]}"
        read -p "是否繼續？占用端口的服務將被停止 (y/N): " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            log_info "用戶取消啟動"
            exit 0
        fi
    fi
    
    log_success "先決條件檢查完成"
}

# =================== 系統管理函數 ===================

cleanup_existing_containers() {
    log_step "清理現有容器..."
    
    # 停止並移除現有容器
    docker-compose -f "${COMPOSE_FILE}" down --remove-orphans --volumes 2>/dev/null || true
    
    # 清理孤立容器
    docker ps -a --filter "name=hft-" --format "{{.ID}}" | xargs -r docker rm -f
    
    log_success "容器清理完成"
}

start_infrastructure() {
    log_step "🚀 啟動L0基礎設施層..."
    
    # 首先啟動基礎設施服務
    docker-compose -f "${COMPOSE_FILE}" up -d \
        redis clickhouse prometheus grafana
    
    # 等待基礎設施服務就緒
    wait_for_service "Redis" "http://localhost:6379" 30
    wait_for_service "ClickHouse" "http://localhost:8123" 60  
    wait_for_service "Prometheus" "http://localhost:9090" 45
    wait_for_service "Grafana" "http://localhost:3000" 45
    
    log_success "✅ L0基礎設施層啟動完成"
}

start_rust_core() {
    log_step "⚡ 啟動L1 Rust HFT核心引擎..."
    
    docker-compose -f "${COMPOSE_FILE}" up -d rust-hft-core
    
    # 等待Rust核心服務就緒
    wait_for_service "Rust HTTP" "http://localhost:8080/metrics" 90
    
    if command -v grpcurl &> /dev/null; then
        wait_for_grpc_service "Rust gRPC" "localhost:50051" 60
    fi
    
    log_success "✅ L1 Rust核心引擎啟動完成"
}

start_ops_workspace() {
    log_step "🛡️ 啟動L2運維工作區..."
    
    docker-compose -f "${COMPOSE_FILE}" up -d ops-workspace
    
    # 等待運維工作區就緒
    wait_for_service "Ops Streamlit" "http://localhost:8503" 60
    wait_for_service "Ops API" "http://localhost:8003" 30
    
    log_success "✅ L2運維工作區啟動完成"
}

start_master_orchestrator() {
    log_step "🎯 啟動Master Orchestrator..."
    
    docker-compose -f "${COMPOSE_FILE}" up -d master-orchestrator
    
    # 等待Master控制台就緒
    wait_for_service "Master Console" "http://localhost:8504" 90
    wait_for_service "Master API" "http://localhost:8004" 30
    
    log_success "✅ Master Orchestrator啟動完成"
}

start_ml_workspace() {
    log_step "🧠 準備L3機器學習工作區 (按需啟動)..."
    
    # ML workspace設為按需啟動，不在常駐服務中
    docker-compose -f "${COMPOSE_FILE}" create ml-workspace
    
    log_success "✅ L3機器學習工作區已準備就緒（使用 start_ml 命令啟動）"
}

# =================== 驗證函數 ===================

perform_system_validation() {
    log_step "🔍 執行系統集成驗證..."
    
    local validation_passed=true
    
    # 檢查基礎設施連通性
    log_info "驗證Redis連接..."
    if ! docker exec hft-redis redis-cli ping | grep -q "PONG"; then
        log_error "Redis連接失敗"
        validation_passed=false
    fi
    
    log_info "驗證ClickHouse連接..."
    if ! curl -s "http://localhost:8123/?query=SELECT%201" | grep -q "1"; then
        log_error "ClickHouse連接失敗"
        validation_passed=false
    fi
    
    # 檢查Rust核心指標
    log_info "驗證Rust核心指標..."
    if ! curl -s "http://localhost:8080/metrics" | grep -q "hft_"; then
        log_error "Rust核心指標未暴露"
        validation_passed=false
    fi
    
    # 檢查工作區API
    log_info "驗證運維工作區API..."
    if ! curl -s "http://localhost:8003/docs" | grep -q "FastAPI"; then
        log_error "運維工作區API未響應"
        validation_passed=false
    fi
    
    # 檢查Master控制台
    log_info "驗證Master控制台..."  
    if ! curl -s "http://localhost:8504" | grep -q "Streamlit"; then
        log_error "Master控制台未響應"
        validation_passed=false
    fi
    
    if [ "$validation_passed" = true ]; then
        log_success "✅ 系統集成驗證通過"
        return 0
    else
        log_error "❌ 系統集成驗證失敗"
        return 1
    fi
}

display_system_info() {
    echo ""
    echo -e "${PURPLE}=================================${NC}"
    echo -e "${PURPLE}  🎯 HFT系統啟動完成！${NC}"
    echo -e "${PURPLE}=================================${NC}"
    echo ""
    echo -e "${CYAN}📊 系統控制面板：${NC}"
    echo -e "  🎛️  Master控制台:     ${GREEN}http://localhost:8504${NC}"
    echo -e "  🛡️  運維監控台:       ${GREEN}http://localhost:8503${NC}"
    echo -e "  📈  Grafana儀表板:    ${GREEN}http://localhost:3000${NC} (admin/hft_grafana_2025)"
    echo -e "  🔍  Prometheus:       ${GREEN}http://localhost:9090${NC}"
    echo ""
    echo -e "${CYAN}🔌 API端點：${NC}"
    echo -e "  ⚡  Rust gRPC:        ${GREEN}localhost:50051${NC}"
    echo -e "  📊  Rust Metrics:     ${GREEN}http://localhost:8080/metrics${NC}"
    echo -e "  🛡️  Ops API:          ${GREEN}http://localhost:8003/docs${NC}"
    echo -e "  🎯  Master API:       ${GREEN}http://localhost:8004/docs${NC}"
    echo ""
    echo -e "${CYAN}🚀 常用命令：${NC}"
    echo -e "  啟動ML訓練:           ${YELLOW}./hft_master_launcher.sh start_ml${NC}"
    echo -e "  檢查系統狀態:         ${YELLOW}./hft_master_launcher.sh status${NC}"
    echo -e "  查看日誌:             ${YELLOW}./hft_master_launcher.sh logs${NC}"
    echo -e "  停止系統:             ${YELLOW}./hft_master_launcher.sh stop${NC}"
    echo ""
    echo -e "${GREEN}🎉 系統已準備就緒，可以開始交易！${NC}"
    echo ""
}

# =================== 主要命令函數 ===================

cmd_start() {
    log_info "🚀 開始啟動HFT系統..."
    
    setup_logging
    check_prerequisites
    cleanup_existing_containers
    
    # 分階段啟動系統
    start_infrastructure
    start_rust_core  
    start_ops_workspace
    start_master_orchestrator
    start_ml_workspace
    
    # 執行系統驗證
    if perform_system_validation; then
        display_system_info
        log_success "HFT系統啟動成功！"
        exit 0
    else
        log_error "系統驗證失敗，請檢查日誌"
        exit 1
    fi
}

cmd_stop() {
    log_info "🛑 停止HFT系統..."
    
    docker-compose -f "${COMPOSE_FILE}" down --remove-orphans
    
    log_success "HFT系統已停止"
}

cmd_restart() {
    log_info "🔄 重啟HFT系統..."
    
    cmd_stop
    sleep 5
    cmd_start
}

cmd_status() {
    echo -e "${BLUE}📊 HFT系統狀態：${NC}"
    echo ""
    
    docker-compose -f "${COMPOSE_FILE}" ps
    
    echo ""
    echo -e "${BLUE}🔍 服務健康檢查：${NC}"
    
    # 檢查關鍵服務
    local services=(
        "Redis:http://localhost:6379"
        "ClickHouse:http://localhost:8123"
        "Prometheus:http://localhost:9090"
        "Grafana:http://localhost:3000"
        "Rust-Metrics:http://localhost:8080/metrics"
        "Ops-API:http://localhost:8003"
        "Master-Console:http://localhost:8504"
    )
    
    for service_info in "${services[@]}"; do
        local name="${service_info%%:*}"
        local url="${service_info##*:}"
        
        if curl -sf "$url" >/dev/null 2>&1; then
            echo -e "  ✅ ${name}: ${GREEN}運行中${NC}"
        else
            echo -e "  ❌ ${name}: ${RED}不可用${NC}"
        fi
    done
}

cmd_logs() {
    local service=${1:-""}
    
    if [ -n "$service" ]; then
        docker-compose -f "${COMPOSE_FILE}" logs -f "$service"
    else
        docker-compose -f "${COMPOSE_FILE}" logs -f
    fi
}

cmd_start_ml() {
    log_info "🧠 啟動ML訓練工作區..."
    
    docker-compose -f "${COMPOSE_FILE}" up -d ml-workspace
    
    wait_for_service "ML Streamlit" "http://localhost:8502" 60
    wait_for_service "ML API" "http://localhost:8002" 30
    
    log_success "✅ ML工作區已啟動"
    echo -e "  🧠 ML控制台: ${GREEN}http://localhost:8502${NC}"
    echo -e "  🔬 ML API: ${GREEN}http://localhost:8002/docs${NC}"
}

cmd_stop_ml() {
    log_info "🛑 停止ML工作區..."
    
    docker-compose -f "${COMPOSE_FILE}" stop ml-workspace
    
    log_success "ML工作區已停止"
}

# =================== 主函數 ===================

show_usage() {
    echo "HFT Master Orchestrator - 統一系統管理工具"
    echo ""
    echo "用法: $0 <command> [options]"
    echo ""
    echo "命令:"
    echo "  start       啟動完整HFT系統"
    echo "  stop        停止HFT系統"
    echo "  restart     重啟HFT系統"
    echo "  status      顯示系統狀態"
    echo "  logs [svc]  查看日誌 (可選指定服務)"
    echo "  start_ml    啟動ML訓練工作區"
    echo "  stop_ml     停止ML工作區"
    echo ""
    echo "示例:"
    echo "  $0 start                    # 啟動完整系統"
    echo "  $0 logs rust-hft-core      # 查看Rust核心日誌"
    echo "  $0 start_ml                # 啟動ML訓練"
    echo ""
}

main() {
    case "${1:-}" in
        start)
            cmd_start
            ;;
        stop)
            cmd_stop
            ;;
        restart)
            cmd_restart  
            ;;
        status)
            cmd_status
            ;;
        logs)
            cmd_logs "${2:-}"
            ;;
        start_ml)
            cmd_start_ml
            ;;
        stop_ml)
            cmd_stop_ml
            ;;
        *)
            show_usage
            exit 1
            ;;
    esac
}

# 執行主函數
main "$@"