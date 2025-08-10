#!/bin/bash

# HFT System Coordinated Startup Script
# 使用workspace coordinator进行协调启动
# 解决依赖关系和启动顺序问题

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
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

log_debug() {
    echo -e "${BLUE}[DEBUG]${NC} $1"
}

# 检查必要的命令
check_prerequisites() {
    log_info "检查系统前置条件..."
    
    if ! command -v docker &> /dev/null; then
        log_error "Docker 未安装或不在PATH中"
        exit 1
    fi
    
    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose 未安装或不在PATH中"
        exit 1
    fi
    
    if ! command -v python3 &> /dev/null; then
        log_error "Python3 未安装或不在PATH中"
        exit 1
    fi
    
    log_info "前置条件检查通过"
}

# 检查环境变量
check_environment() {
    log_info "检查环境变量..."
    
    if [[ -f .env ]]; then
        log_info "发现 .env 文件，加载环境变量"
        export $(cat .env | grep -v '^#' | xargs)
    else
        log_warn "未发现 .env 文件，使用默认配置"
    fi
    
    # 检查必要的环境变量
    REQUIRED_VARS=("OPENAI_API_KEY")
    for var in "${REQUIRED_VARS[@]}"; do
        if [[ -z "${!var}" ]]; then
            log_warn "环境变量 $var 未设置"
        fi
    done
}

# 清理旧的容器和网络
cleanup_old_resources() {
    log_info "清理旧的容器和网络..."
    
    # 停止并删除相关容器
    CONTAINERS=("hft-redis" "hft-clickhouse" "hft-master-db" "hft-ops-db" "hft-ml-db" 
                "rust-hft" "hft-ops-api" "hft-ops-ui" "hft-ml-api" "hft-ml-ui" 
                "hft-master-api" "hft-master-ui" "workspace-coordinator")
    
    for container in "${CONTAINERS[@]}"; do
        if docker ps -a --format 'table {{.Names}}' | grep -q "^${container}$"; then
            log_debug "删除容器: $container"
            docker rm -f "$container" 2>/dev/null || true
        fi
    done
    
    # 清理悬空的网络
    log_debug "清理悬空的网络"
    docker network prune -f 2>/dev/null || true
}

# 创建必要的目录和配置
setup_directories() {
    log_info "创建必要的目录..."
    
    DIRS=("logs" "models" "outputs" "shared_config")
    for dir in "${DIRS[@]}"; do
        if [[ ! -d "$dir" ]]; then
            mkdir -p "$dir"
            log_debug "创建目录: $dir"
        fi
    done
    
    # 确保配置文件存在
    if [[ ! -f "shared_config/redis_channels.yaml" ]]; then
        log_error "Redis通道配置文件不存在: shared_config/redis_channels.yaml"
        exit 1
    fi
    
    if [[ ! -f "shared_config/grpc_services.yaml" ]]; then
        log_error "gRPC服务配置文件不存在: shared_config/grpc_services.yaml"
        exit 1
    fi
    
    if [[ ! -f "shared_config/startup_sequence.yaml" ]]; then
        log_error "启动序列配置文件不存在: shared_config/startup_sequence.yaml"
        exit 1
    fi
}

# 构建Docker镜像
build_images() {
    log_info "构建Docker镜像..."
    
    # 构建workspace镜像
    WORKSPACES=("hft-master-workspace" "hft-ops-workspace" "hft-ml-workspace")
    for workspace in "${WORKSPACES[@]}"; do
        if [[ -d "$workspace" && -f "$workspace/Dockerfile" ]]; then
            log_debug "构建镜像: $workspace"
            docker build -t "local/${workspace##hft-}:dev" "$workspace/" || {
                log_error "构建 $workspace 镜像失败"
                exit 1
            }
        else
            log_warn "跳过 $workspace (目录或Dockerfile不存在)"
        fi
    done
    
    # 构建Rust HFT镜像
    if [[ -d "rust_hft" && -f "rust_hft/Dockerfile" ]]; then
        log_debug "构建Rust HFT镜像"
        docker build -t "local/rust-hft:dev" "rust_hft/" || {
            log_error "构建Rust HFT镜像失败"
            exit 1
        }
    fi
    
    # 构建Coordinator镜像
    if [[ -f "Dockerfile.coordinator" ]]; then
        log_debug "构建Workspace Coordinator镜像"
        docker build -f Dockerfile.coordinator -t "local/workspace-coordinator:dev" . || {
            log_error "构建Workspace Coordinator镜像失败"
            exit 1
        }
    fi
}

# 阶段性启动服务
start_services_by_phase() {
    log_info "开始阶段性启动服务..."
    
    # Phase 0: 基础设施
    log_info "Phase 0: 启动基础设施服务 (Redis, ClickHouse, PostgreSQL)"
    docker-compose -f docker-compose.fixed.yml up -d \
        hft-redis hft-clickhouse hft-master-db hft-ops-db hft-ml-db
    
    # 等待基础设施就绪
    log_info "等待基础设施服务就绪..."
    wait_for_service "hft-redis" "redis-cli -h hft-redis ping" 60
    wait_for_service "hft-clickhouse" "wget --quiet --tries=1 --spider http://hft-clickhouse:8123/ping" 120
    wait_for_service "hft-master-db" "pg_isready -h hft-master-db -U ai" 60
    wait_for_service "hft-ops-db" "pg_isready -h hft-ops-db -U ai" 60
    wait_for_service "hft-ml-db" "pg_isready -h hft-ml-db -U ai" 60
    
    # Phase 1: Rust HFT 核心
    log_info "Phase 1: 启动Rust HFT核心引擎"
    docker-compose -f docker-compose.fixed.yml up -d rust-hft
    wait_for_service "rust-hft" "grpc_health_probe -addr=rust-hft:50051" 120
    
    # Phase 2: Ops Workspace
    log_info "Phase 2: 启动运维工作区"
    docker-compose -f docker-compose.fixed.yml up -d hft-ops-api hft-ops-ui
    wait_for_service "hft-ops-api" "curl -f http://hft-ops-api:8003/health" 90
    wait_for_service "hft-ops-ui" "curl -f http://hft-ops-ui:8503/_stcore/health" 60
    
    # Phase 3: ML Workspace
    log_info "Phase 3: 启动机器学习工作区"
    docker-compose -f docker-compose.fixed.yml up -d hft-ml-api hft-ml-ui
    wait_for_service "hft-ml-api" "curl -f http://hft-ml-api:8001/health" 120
    wait_for_service "hft-ml-ui" "curl -f http://hft-ml-ui:8502/_stcore/health" 60
    
    # Phase 4: Master Workspace
    log_info "Phase 4: 启动主控工作区"
    docker-compose -f docker-compose.fixed.yml up -d hft-master-api hft-master-ui
    wait_for_service "hft-master-api" "curl -f http://hft-master-api:8002/health" 90
    wait_for_service "hft-master-ui" "curl -f http://hft-master-ui:8504/_stcore/health" 60
}

# 启动使用Python coordinator
start_with_coordinator() {
    log_info "使用Python Workspace Coordinator启动..."
    
    # 首先启动基础设施
    docker-compose -f docker-compose.fixed.yml up -d \
        hft-redis hft-clickhouse hft-master-db hft-ops-db hft-ml-db
    
    # 等待基础设施就绪
    sleep 30
    
    # 启动coordinator
    docker-compose -f docker-compose.fixed.yml --profile monitoring up -d workspace-coordinator
    
    # coordinator将负责启动其他服务
    log_info "Workspace Coordinator 已启动，它将负责协调其他服务的启动"
    
    # 监控coordinator日志
    log_info "监控coordinator启动日志..."
    timeout 300 docker logs -f workspace-coordinator || {
        log_warn "监控超时，请手动检查日志"
    }
}

# 等待服务就绪
wait_for_service() {
    local service_name="$1"
    local health_check="$2"
    local timeout="$3"
    local count=0
    
    log_info "等待服务就绪: $service_name"
    
    while [ $count -lt $timeout ]; do
        if docker exec "$service_name" sh -c "$health_check" &>/dev/null; then
            log_info "服务 $service_name 已就绪"
            return 0
        fi
        
        sleep 2
        count=$((count + 2))
        
        if [ $((count % 20)) -eq 0 ]; then
            log_debug "等待 $service_name ($count/${timeout}s)"
        fi
    done
    
    log_error "服务 $service_name 启动超时"
    return 1
}

# 验证系统状态
verify_system() {
    log_info "验证系统状态..."
    
    # 检查所有容器状态
    EXPECTED_CONTAINERS=("hft-redis" "hft-clickhouse" "hft-master-db" "hft-ops-db" "hft-ml-db"
                        "rust-hft" "hft-ops-api" "hft-ops-ui" "hft-ml-api" "hft-ml-ui"
                        "hft-master-api" "hft-master-ui")
    
    local healthy_count=0
    local total_count=${#EXPECTED_CONTAINERS[@]}
    
    for container in "${EXPECTED_CONTAINERS[@]}"; do
        if docker ps --format 'table {{.Names}}\t{{.Status}}' | grep "$container" | grep -q "healthy\|Up"; then
            log_debug "✓ $container: 运行中"
            ((healthy_count++))
        else
            log_warn "✗ $container: 未运行或不健康"
        fi
    done
    
    log_info "系统状态: $healthy_count/$total_count 服务正常运行"
    
    if [ $healthy_count -eq $total_count ]; then
        log_info "🎉 HFT系统启动成功！"
        display_service_urls
        return 0
    else
        log_warn "部分服务未正常启动，请检查日志"
        return 1
    fi
}

# 显示服务URL
display_service_urls() {
    log_info "服务访问地址:"
    echo -e "${BLUE}================================${NC}"
    echo -e "${GREEN}Master Workspace UI:${NC} http://localhost:8504"
    echo -e "${GREEN}Master Workspace API:${NC} http://localhost:8002"
    echo -e "${GREEN}Ops Workspace UI:${NC} http://localhost:8503"
    echo -e "${GREEN}Ops Workspace API:${NC} http://localhost:8003"
    echo -e "${GREEN}ML Workspace UI:${NC} http://localhost:8502"
    echo -e "${GREEN}ML Workspace API:${NC} http://localhost:8001"
    echo -e "${GREEN}Redis:${NC} localhost:6379"
    echo -e "${GREEN}ClickHouse:${NC} http://localhost:8123"
    echo -e "${GREEN}Rust HFT gRPC:${NC} localhost:50051"
    echo -e "${BLUE}================================${NC}"
}

# 主函数
main() {
    local start_method="${1:-manual}"
    
    log_info "🚀 启动HFT系统 - 协调启动模式"
    log_info "启动方法: $start_method"
    
    check_prerequisites
    check_environment
    cleanup_old_resources
    setup_directories
    build_images
    
    case "$start_method" in
        "coordinator")
            start_with_coordinator
            ;;
        "manual"|*)
            start_services_by_phase
            ;;
    esac
    
    verify_system
    
    log_info "🎯 HFT系统启动完成"
    log_info "使用 'docker-compose -f docker-compose.fixed.yml logs -f' 查看所有日志"
    log_info "使用 'docker-compose -f docker-compose.fixed.yml down' 停止系统"
}

# 信号处理
cleanup_on_exit() {
    log_info "收到停止信号，清理资源..."
    docker-compose -f docker-compose.fixed.yml down
    exit 0
}

trap cleanup_on_exit SIGINT SIGTERM

# 执行主函数
main "$@"