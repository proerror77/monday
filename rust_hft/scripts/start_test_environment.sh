#!/bin/bash

# 快速启动测试环境脚本
# 
# 功能：
# - 启动现有的Docker服务（ClickHouse, Redis, Prometheus, Grafana）
# - 检查服务健康状态
# - 准备数据库环境
# - 提供服务状态报告

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
WHITE='\033[1;37m'
NC='\033[0m' # No Color

# 日志函数
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

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

# 检查Docker是否运行
check_docker() {
    log_step "检查 Docker 服务状态..."
    
    if ! docker info > /dev/null 2>&1; then
        log_error "Docker 服务未运行！请先启动 Docker"
        exit 1
    fi
    
    log_success "Docker 服务运行正常 ✓"
}

# 启动Docker服务
start_services() {
    log_step "启动 HFT 测试环境服务..."
    
    log_info "正在启动服务: ClickHouse, Redis, Prometheus, Grafana"
    
    # 启动所有服务
    docker-compose up -d
    
    if [ $? -eq 0 ]; then
        log_success "所有服务启动命令执行完成 ✓"
    else
        log_error "服务启动失败！"
        exit 1
    fi
}

# 等待服务健康检查
wait_for_services() {
    log_step "等待服务完全启动并通过健康检查..."
    
    local max_wait=180  # 最大等待3分钟
    local elapsed=0
    local check_interval=5
    
    local services=("hft_clickhouse" "hft_redis" "hft_prometheus" "hft_grafana")
    
    while [ $elapsed -lt $max_wait ]; do
        local all_healthy=true
        
        echo -n "检查服务健康状态... "
        
        for service in "${services[@]}"; do
            local health_status=$(docker inspect --format='{{.State.Health.Status}}' "$service" 2>/dev/null || echo "no-health-check")
            
            if [ "$health_status" != "healthy" ] && [ "$health_status" != "no-health-check" ]; then
                all_healthy=false
                break
            fi
        done
        
        if $all_healthy; then
            echo -e "${GREEN}✓${NC}"
            log_success "所有服务健康检查通过！"
            return 0
        else
            echo -e "${YELLOW}⏳${NC} (${elapsed}s/${max_wait}s)"
            sleep $check_interval
            elapsed=$((elapsed + check_interval))
        fi
    done
    
    log_warn "部分服务可能仍在启动中，继续进行..."
}

# 检查服务端口
check_service_ports() {
    log_step "检查服务端口可用性..."
    
    local ports_and_services=(
        "8123:ClickHouse HTTP"
        "9000:ClickHouse Native"
        "6379:Redis"
        "9090:Prometheus"
        "3000:Grafana"
    )
    
    for port_service in "${ports_and_services[@]}"; do
        local port="${port_service%%:*}"
        local service="${port_service##*:}"
        
        if nc -z localhost $port 2>/dev/null; then
            log_info "✓ $service (端口 $port) 可访问"
        else
            log_warn "⚠ $service (端口 $port) 暂时不可访问"
        fi
    done
}

# 准备ClickHouse数据库
prepare_clickhouse_database() {
    log_step "准备 ClickHouse 数据库环境..."
    
    # 等待ClickHouse完全启动
    local max_attempts=12
    local attempt=1
    
    while [ $attempt -le $max_attempts ]; do
        if curl -s "http://localhost:8123/ping" > /dev/null 2>&1; then
            log_success "ClickHouse HTTP 接口响应正常 ✓"
            break
        else
            log_info "等待 ClickHouse 启动... (尝试 $attempt/$max_attempts)"
            sleep 5
            attempt=$((attempt + 1))
        fi
    done
    
    if [ $attempt -gt $max_attempts ]; then
        log_error "ClickHouse 启动超时！"
        return 1
    fi
    
    # 检查数据库是否存在
    log_info "检查 HFT 数据库..."
    
    local db_exists=$(curl -s "http://localhost:8123/" -d "SELECT name FROM system.databases WHERE name = 'hft'" | tr -d '\n')
    
    if [ "$db_exists" = "hft" ]; then
        log_success "HFT 数据库已存在 ✓"
    else
        log_info "创建 HFT 数据库..."
        curl -s "http://localhost:8123/" -d "CREATE DATABASE IF NOT EXISTS hft"
        log_success "HFT 数据库创建完成 ✓"
    fi
    
    # 显示数据库信息
    log_info "ClickHouse 信息:"
    local version=$(curl -s "http://localhost:8123/" -d "SELECT version()")
    echo "  版本: $version"
    
    local tables=$(curl -s "http://localhost:8123/" -d "SELECT count() FROM system.tables WHERE database = 'hft'")
    echo "  HFT 数据库中的表数量: $tables"
}

# 显示服务状态
show_service_status() {
    log_step "服务状态总览..."
    
    echo ""
    echo "=================================================================="
    echo "🚀 HFT 测试环境服务状态"
    echo "=================================================================="
    
    docker-compose ps
    
    echo ""
    echo "📊 服务访问地址:"
    echo "  • ClickHouse HTTP:  http://localhost:8123"
    echo "  • ClickHouse TCP:   localhost:9000"
    echo "  • Redis:            localhost:6379"
    echo "  • Prometheus:       http://localhost:9090"
    echo "  • Grafana:          http://localhost:3000 (admin/admin)"
    echo ""
    
    echo "🗄️  数据库信息:"
    echo "  • 数据库名称: hft"
    echo "  • 测试表名: market_data_15min"
    echo ""
    
    echo "🔧 管理命令:"
    echo "  • 查看日志: docker-compose logs [service_name]"
    echo "  • 停止服务: docker-compose down"
    echo "  • 重启服务: docker-compose restart [service_name]"
    echo ""
    
    log_success "测试环境准备完成！现在可以运行数据库压力测试"
    echo ""
    echo "💡 下一步："
    echo "   运行完整测试: ./scripts/run_comprehensive_db_test.sh"
    echo ""
}

# 主函数
main() {
    echo -e "${PURPLE}"
    echo "=================================================================="
    echo "🚀 HFT 测试环境启动脚本"
    echo "=================================================================="
    echo -e "${NC}"
    
    # 检查Docker
    check_docker
    
    # 启动服务
    start_services
    
    # 等待服务就绪
    wait_for_services
    
    # 检查端口
    check_service_ports
    
    # 准备数据库
    prepare_clickhouse_database
    
    # 显示状态
    show_service_status
}

# 检查是否从正确的目录运行
if [ ! -f "docker-compose.yml" ]; then
    log_error "请从包含 docker-compose.yml 的项目根目录运行此脚本"
    exit 1
fi

# 运行主函数
main "$@"