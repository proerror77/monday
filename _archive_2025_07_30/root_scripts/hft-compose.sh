#!/bin/bash

# HFT Docker Compose Stack 管理腳本
# 符合 Agno 官方規範的多 workspace 統一管理工具
# 基於 Docker Compose 2025 最佳實踐

set -e  # Exit on any error

# =============================================================================
# Configuration
# =============================================================================
COMPOSE_FILE="docker-compose.hft.yml"
ENV_FILE=".env.hft"
PROJECT_NAME="hft-system"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Emoji for better UX
ROCKET="🚀"
STOP="⏹️"
RESTART="🔄"
STATUS="📊"
LOGS="📋"
BUILD="🔨"
CLEAN="🧹"
CHECK="✅"
ERROR="❌"
WARNING="⚠️"

# =============================================================================
# Helper Functions
# =============================================================================

print_header() {
    echo -e "${CYAN}================================${NC}"
    echo -e "${CYAN}  HFT Docker Compose Manager${NC}"
    echo -e "${CYAN}  符合 Agno 官方規範${NC}"
    echo -e "${CYAN}================================${NC}"
    echo ""
}

print_success() {
    echo -e "${GREEN}${CHECK} $1${NC}"
}

print_error() {
    echo -e "${RED}${ERROR} $1${NC}"
}

print_warning() {
    echo -e "${YELLOW}${WARNING} $1${NC}"
}

print_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

check_prerequisites() {
    print_info "檢查系統前置條件..."
    
    # Check if docker is installed
    if ! command -v docker &> /dev/null; then
        print_error "Docker 未安裝。請先安裝 Docker。"
        exit 1
    fi
    
    # Check if docker compose is available
    if ! docker compose version &> /dev/null; then
        print_error "Docker Compose 未安裝或版本太舊。請更新到最新版本。"
        exit 1
    fi
    
    # Check if compose file exists
    if [[ ! -f "$COMPOSE_FILE" ]]; then
        print_error "Docker Compose 配置文件 '$COMPOSE_FILE' 不存在。"
        exit 1
    fi
    
    # Check if env file exists
    if [[ ! -f "$ENV_FILE" ]]; then
        print_warning "環境變數文件 '$ENV_FILE' 不存在。使用默認配置。"
    fi
    
    print_success "系統前置條件檢查完成"
}

show_status() {
    print_info "顯示 HFT 系統狀態..."
    echo ""
    
    # Show running containers
    echo -e "${PURPLE}${STATUS} 容器狀態:${NC}"
    docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" ps --format table
    echo ""
    
    # Show resource usage
    echo -e "${PURPLE}📈 資源使用情況:${NC}"
    docker stats --no-stream --format "table {{.Container}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.NetIO}}\t{{.BlockIO}}" $(docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" ps -q) 2>/dev/null || echo "無運行容器"
    echo ""
    
    # Show network info
    echo -e "${PURPLE}🌐 網絡狀態:${NC}"
    docker network ls --filter name=hft --format "table {{.Name}}\t{{.Driver}}\t{{.Scope}}"
    echo ""
}

start_services() {
    local services="$1"
    print_info "啟動 HFT 系統服務${services:+: $services}..."
    
    # Start infrastructure first
    if [[ -z "$services" ]] || [[ "$services" == "infra" ]] || [[ "$services" == "all" ]]; then
        print_info "啟動基礎設施服務 (Redis, ClickHouse)..."
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" up -d hft-redis hft-clickhouse
        
        # Wait for infrastructure to be healthy
        print_info "等待基礎設施服務就緒..."
        sleep 10
    fi
    
    # Start all services or specific workspace
    if [[ -z "$services" ]] || [[ "$services" == "all" ]]; then
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" up -d
    elif [[ "$services" == "master" ]]; then
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" up -d hft-master-db hft-master-ui hft-master-api
    elif [[ "$services" == "ops" ]]; then
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" up -d hft-ops-db hft-ops-ui hft-ops-api
    elif [[ "$services" == "ml" ]]; then
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" up -d hft-ml-db hft-ml-ui hft-ml-api
    elif [[ "$services" == "infra" ]]; then
        # Already started above
        print_success "基礎設施服務已啟動"
    else
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" up -d "$services"
    fi
    
    print_success "服務啟動完成"
    
    # Show access URLs
    if [[ -z "$services" ]] || [[ "$services" == "all" ]] || [[ "$services" == "master" ]]; then
        echo -e "${GREEN}🎯 HFT Master Control: http://localhost:8504${NC}"
    fi
    if [[ -z "$services" ]] || [[ "$services" == "all" ]] || [[ "$services" == "ops" ]]; then
        echo -e "${GREEN}⚡ HFT Operations: http://localhost:8503${NC}"
    fi
    if [[ -z "$services" ]] || [[ "$services" == "all" ]] || [[ "$services" == "ml" ]]; then
        echo -e "${GREEN}🧠 HFT ML Workspace: http://localhost:8502${NC}"
    fi
}

stop_services() {
    local services="$1"
    print_info "停止 HFT 系統服務${services:+: $services}..."
    
    if [[ -z "$services" ]] || [[ "$services" == "all" ]]; then
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" down
    else
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" stop "$services"
    fi
    
    print_success "服務停止完成"
}

restart_services() {
    local services="$1"
    print_info "重啟 HFT 系統服務${services:+: $services}..."
    
    if [[ -z "$services" ]] || [[ "$services" == "all" ]]; then
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" restart
    else
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" restart "$services"
    fi
    
    print_success "服務重啟完成"
}

show_logs() {
    local services="$1"
    local lines="${2:-100}"
    
    print_info "顯示服務日誌${services:+: $services} (最後 $lines 行)..."
    
    if [[ -z "$services" ]]; then
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" logs --tail="$lines" -f
    else
        docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" logs --tail="$lines" -f "$services"
    fi
}

build_images() {
    print_info "構建 Docker 鏡像..."
    
    # Build all workspace images
    docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" build --no-cache
    
    print_success "鏡像構建完成"
}

cleanup() {
    print_info "清理 Docker 資源..."
    
    # Stop and remove containers
    docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" down -v --remove-orphans
    
    # Remove unused images
    docker image prune -f
    
    # Remove unused volumes
    docker volume prune -f
    
    # Remove unused networks
    docker network prune -f
    
    print_success "清理完成"
}

health_check() {
    print_info "執行健康檢查..."
    
    local all_healthy=true
    
    # Check each service
    services=("hft-redis" "hft-clickhouse" "hft-master-db" "hft-master-ui" "hft-master-api" 
              "hft-ops-db" "hft-ops-ui" "hft-ops-api" "hft-ml-db" "hft-ml-ui" "hft-ml-api")
    
    for service in "${services[@]}"; do
        if docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" ps "$service" | grep -q "running"; then
            health=$(docker inspect "$service" --format='{{.State.Health.Status}}' 2>/dev/null || echo "no-healthcheck")
            if [[ "$health" == "healthy" ]] || [[ "$health" == "no-healthcheck" ]]; then
                print_success "$service: 健康"
            else
                print_error "$service: 不健康 ($health)"
                all_healthy=false
            fi
        else
            print_error "$service: 未運行"
            all_healthy=false
        fi
    done
    
    if $all_healthy; then
        print_success "所有服務都正常運行"
    else
        print_error "部分服務存在問題"
        exit 1
    fi
}

show_help() {
    echo "HFT Docker Compose Stack 管理工具"
    echo ""
    echo "用法: $0 <command> [options]"
    echo ""
    echo "Commands:"
    echo "  up [service]     啟動服務 (可選: all, master, ops, ml, infra)"
    echo "  down [service]   停止服務"
    echo "  restart [service] 重啟服務"
    echo "  status           顯示系統狀態"
    echo "  logs [service] [lines] 顯示日誌"
    echo "  build            構建鏡像"
    echo "  cleanup          清理資源"
    echo "  health           健康檢查"
    echo "  help             顯示幫助"
    echo ""
    echo "Examples:"
    echo "  $0 up                    # 啟動所有服務"
    echo "  $0 up master             # 只啟動 Master 工作區"
    echo "  $0 logs hft-master-ui 50 # 顯示 Master UI 最後 50 行日誌"
    echo "  $0 health                # 執行健康檢查"
}

# =============================================================================
# Main Script
# =============================================================================

main() {
    print_header
    
    local command="${1:-help}"
    
    case "$command" in
        "up"|"start")
            check_prerequisites
            start_services "$2"
            ;;
        "down"|"stop")
            check_prerequisites
            stop_services "$2"
            ;;
        "restart")
            check_prerequisites
            restart_services "$2"
            ;;
        "status"|"ps")
            check_prerequisites
            show_status
            ;;
        "logs")
            check_prerequisites
            show_logs "$2" "$3"
            ;;
        "build")
            check_prerequisites
            build_images
            ;;
        "cleanup"|"clean")
            cleanup
            ;;
        "health"|"check")
            check_prerequisites
            health_check
            ;;
        "help"|"-h"|"--help")
            show_help
            ;;
        *)
            print_error "未知命令: $command"
            show_help
            exit 1
            ;;
    esac
}

# Execute main function with all arguments
main "$@"