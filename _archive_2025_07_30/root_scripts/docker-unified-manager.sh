#!/bin/bash

# HFT System Unified Docker Manager
# 統一的Docker資源管理腳本
# Author: Docker Integration Expert
# Version: 1.0

set -e

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
COMPOSE_FILE="docker-compose.unified.yml"
ENV_FILE=".env.unified"
PROJECT_NAME="hft-unified"

# Logging function
log() {
    echo -e "${GREEN}[$(date +'%Y-%m-%d %H:%M:%S')] $1${NC}"
}

warn() {
    echo -e "${YELLOW}[$(date +'%Y-%m-%d %H:%M:%S')] WARNING: $1${NC}"
}

error() {
    echo -e "${RED}[$(date +'%Y-%m-%d %H:%M:%S')] ERROR: $1${NC}"
}

# Check prerequisites
check_prerequisites() {
    log "檢查系統先決條件..."
    
    if ! command -v docker &> /dev/null; then
        error "Docker未安裝，請先安裝Docker"
        exit 1
    fi
    
    if ! command -v docker-compose &> /dev/null; then
        error "Docker Compose未安裝，請先安裝Docker Compose"
        exit 1
    fi
    
    # Check if required directories exist
    local required_dirs=("data" "logs" "config")
    for dir in "${required_dirs[@]}"; do
        if [ ! -d "$dir" ]; then
            log "創建目錄: $dir"
            mkdir -p "$dir"/{redis,clickhouse,prometheus,grafana}
        fi
    done
    
    # Create required config directories
    mkdir -p config/{prometheus,grafana}
    mkdir -p logs/clickhouse
    
    log "先決條件檢查完成"
}

# Create necessary config files
create_configs() {
    log "創建必要的配置文件..."
    
    # Prometheus配置
    if [ ! -f "config/prometheus/prometheus.yml" ]; then
        cat > config/prometheus/prometheus.yml << 'EOF'
global:
  scrape_interval: 15s
  evaluation_interval: 15s

rule_files:
  # - "first_rules.yml"
  # - "second_rules.yml"

scrape_configs:
  - job_name: 'hft-rust-core'
    static_configs:
      - targets: ['hft-rust-core:8080']
    scrape_interval: 5s
    metrics_path: /metrics

  - job_name: 'hft-ops-agent'
    static_configs:
      - targets: ['hft-ops-agent:8003']
    scrape_interval: 10s

  - job_name: 'redis'
    static_configs:
      - targets: ['hft-redis:6379']
    scrape_interval: 30s

  - job_name: 'clickhouse'
    static_configs:
      - targets: ['hft-clickhouse:8123']
    scrape_interval: 30s
EOF
        log "Prometheus配置文件已創建"
    fi
    
    # Grafana數據源配置
    mkdir -p config/grafana/datasources
    if [ ! -f "config/grafana/datasources/prometheus.yml" ]; then
        cat > config/grafana/datasources/prometheus.yml << 'EOF'
apiVersion: 1

datasources:
  - name: Prometheus
    type: prometheus
    access: proxy
    url: http://hft-prometheus:9090
    isDefault: true
    editable: true
    
  - name: ClickHouse
    type: grafana-clickhouse-datasource
    access: proxy
    url: http://hft-clickhouse:8123
    database: hft
    basicAuth: false
    editable: true
EOF
        log "Grafana數據源配置已創建"
    fi
}

# Start infrastructure services first
start_infrastructure() {
    log "啟動基礎設施服務..."
    
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE up -d \
        hft-redis \
        hft-clickhouse \
        hft-prometheus \
        hft-grafana
    
    log "等待基礎設施服務健康檢查..."
    sleep 30
    
    # Check service health
    check_service_health "hft-redis" "Redis"
    check_service_health "hft-clickhouse" "ClickHouse"
    check_service_health "hft-prometheus" "Prometheus"
    check_service_health "hft-grafana" "Grafana"
    
    log "基礎設施服務啟動完成"
}

# Start Rust core engine
start_rust_core() {
    log "啟動Rust核心引擎..."
    
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE up -d hft-rust-core
    
    log "等待Rust核心引擎啟動..."
    sleep 20
    
    check_service_health "hft-rust-core" "Rust Core Engine"
    
    log "Rust核心引擎啟動完成"
}

# Start Agno workspaces
start_workspaces() {
    log "啟動Agno工作區..."
    
    # Start workspace databases first
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE up -d \
        hft-master-db \
        hft-ops-db \
        hft-ml-db
    
    log "等待數據庫啟動..."
    sleep 20
    
    # Start workspace applications
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE up -d \
        hft-master-ui \
        hft-master-api \
        hft-ops-agent
    
    log "Agno工作區啟動完成"
}

# Check service health
check_service_health() {
    local container_name=$1
    local service_name=$2
    local max_retries=10
    local retry_count=0
    
    while [ $retry_count -lt $max_retries ]; do
        if docker ps --filter "name=$container_name" --filter "status=running" | grep -q $container_name; then
            local health_status=$(docker inspect --format='{{.State.Health.Status}}' $container_name 2>/dev/null || echo "no-healthcheck")
            
            if [ "$health_status" = "healthy" ] || [ "$health_status" = "no-healthcheck" ]; then
                log "$service_name 服務健康"
                return 0
            fi
        fi
        
        retry_count=$((retry_count + 1))
        log "等待 $service_name 服務啟動... ($retry_count/$max_retries)"
        sleep 5
    done
    
    warn "$service_name 服務可能未正常啟動，請檢查日志"
    return 1
}

# Show service status
show_status() {
    log "服務狀態概覽:"
    echo
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE ps
    echo
    log "服務端點:"
    echo -e "${BLUE}Grafana Dashboard:${NC} http://localhost:3000 (admin/hft123)"
    echo -e "${BLUE}Prometheus:${NC} http://localhost:9090"
    echo -e "${BLUE}ClickHouse:${NC} http://localhost:8123"
    echo -e "${BLUE}Master Workspace UI:${NC} http://localhost:8504"
    echo -e "${BLUE}Master Workspace API:${NC} http://localhost:8002"
    echo -e "${BLUE}Ops Workspace UI:${NC} http://localhost:8503"
    echo -e "${BLUE}Ops Workspace API:${NC} http://localhost:8003"
    echo -e "${BLUE}ML Workspace UI:${NC} http://localhost:8502"
    echo -e "${BLUE}ML Workspace API:${NC} http://localhost:8001"
    echo
}

# Show logs for specific service
show_logs() {
    local service_name=$1
    if [ -z "$service_name" ]; then
        docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE logs -f
    else
        docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE logs -f $service_name
    fi
}

# Stop all services
stop_services() {
    log "停止所有服務..."
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE down
    log "服務已停止"
}

# Clean up resources
cleanup() {
    log "清理資源..."
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE down -v --remove-orphans
    docker system prune -f
    log "資源清理完成"
}

# Start ML training job
start_ml_training() {
    log "啟動ML訓練任務..."
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE run --rm hft-ml-trainer \
        python3 -m workspace.workflows.training_workflow --symbol BTCUSDT --hours 24
}

# Backup volumes
backup_data() {
    log "備份數據..."
    local backup_dir="backup_$(date +%Y%m%d_%H%M%S)"
    mkdir -p $backup_dir
    
    # Stop services first
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE stop
    
    # Backup volume data
    cp -r data/ $backup_dir/
    
    # Restart services
    docker-compose --env-file $ENV_FILE -f $COMPOSE_FILE start
    
    log "數據備份完成: $backup_dir"
}

# Main command handler
case "$1" in
    "start")
        check_prerequisites
        create_configs
        start_infrastructure
        start_rust_core
        start_workspaces
        show_status
        ;;
    "start-infra")
        check_prerequisites
        create_configs
        start_infrastructure
        show_status
        ;;
    "start-rust")
        start_rust_core
        show_status
        ;;
    "start-workspaces")
        start_workspaces
        show_status
        ;;
    "status")
        show_status
        ;;
    "logs")
        show_logs $2
        ;;
    "stop")
        stop_services
        ;;
    "restart")
        stop_services
        sleep 5
        $0 start
        ;;
    "cleanup")
        cleanup
        ;;
    "ml-train")
        start_ml_training
        ;;
    "backup")
        backup_data
        ;;
    "help"|"--help"|"-h"|"")
        echo "HFT System Unified Docker Manager"
        echo "用法: $0 <command> [options]"
        echo
        echo "Commands:"
        echo "  start          - 啟動完整系統（基礎設施 + Rust核心 + 工作區）"
        echo "  start-infra    - 僅啟動基礎設施服務"
        echo "  start-rust     - 啟動Rust核心引擎"
        echo "  start-workspaces - 啟動Agno工作區"
        echo "  status         - 顯示服務狀態"
        echo "  logs [service] - 顯示日志（可選指定服務名）"
        echo "  stop           - 停止所有服務"
        echo "  restart        - 重啟所有服務"
        echo "  cleanup        - 清理所有資源（包括數據卷）"
        echo "  ml-train       - 啟動ML訓練任務"
        echo "  backup         - 備份數據"
        echo "  help           - 顯示此幫助信息"
        echo
        echo "Examples:"
        echo "  $0 start                     # 啟動完整系統"
        echo "  $0 logs hft-rust-core       # 查看Rust核心日志"
        echo "  $0 logs                      # 查看所有服務日志"
        echo
        ;;
    *)
        error "未知命令: $1"
        $0 help
        exit 1
        ;;
esac