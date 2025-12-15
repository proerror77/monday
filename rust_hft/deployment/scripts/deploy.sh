#!/bin/bash
# HFT 系統部署腳本
# 用於構建和部署所有服務

set -e

# 顏色輸出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# 配置
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DOCKER_DIR="$PROJECT_ROOT/deployment/docker"
K8S_DIR="$PROJECT_ROOT/deployment/k8s"

# 默認值
REGISTRY="${ECR_REGISTRY:-localhost:5000}"
IMAGE_TAG="${IMAGE_TAG:-latest}"
NAMESPACE="hft-trading"

# 函數: 打印帶顏色的消息
log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# 函數: 構建 Docker 鏡像
build_images() {
    log_info "構建 Docker 鏡像..."

    cd "$PROJECT_ROOT"

    # Trading Engine
    log_info "構建 trading-engine 鏡像..."
    docker build -t "$REGISTRY/hft-trading:$IMAGE_TAG" \
        -f "$DOCKER_DIR/Dockerfile.trading" .

    # Market Data (Bitget)
    log_info "構建 market-data-bitget 鏡像..."
    docker build -t "$REGISTRY/hft-market-data-bitget:$IMAGE_TAG" \
        -f "$DOCKER_DIR/Dockerfile.market-data" \
        --build-arg EXCHANGE=bitget .

    # Market Data (Binance)
    log_info "構建 market-data-binance 鏡像..."
    docker build -t "$REGISTRY/hft-market-data-binance:$IMAGE_TAG" \
        -f "$DOCKER_DIR/Dockerfile.market-data" \
        --build-arg EXCHANGE=binance .

    # Sentinel
    log_info "構建 sentinel 鏡像..."
    docker build -t "$REGISTRY/hft-sentinel:$IMAGE_TAG" \
        -f "$DOCKER_DIR/Dockerfile.sentinel" .

    log_info "所有鏡像構建完成!"
}

# 函數: 推送鏡像到 Registry
push_images() {
    log_info "推送鏡像到 Registry: $REGISTRY..."

    docker push "$REGISTRY/hft-trading:$IMAGE_TAG"
    docker push "$REGISTRY/hft-market-data-bitget:$IMAGE_TAG"
    docker push "$REGISTRY/hft-market-data-binance:$IMAGE_TAG"
    docker push "$REGISTRY/hft-sentinel:$IMAGE_TAG"

    log_info "所有鏡像推送完成!"
}

# 函數: 部署到 Kubernetes
deploy_k8s() {
    log_info "部署到 Kubernetes..."

    # 檢查 kubectl
    if ! command -v kubectl &> /dev/null; then
        log_error "kubectl 未安裝"
        exit 1
    fi

    # 替換環境變數
    export ECR_REGISTRY="$REGISTRY"
    export IMAGE_TAG="$IMAGE_TAG"

    # 部署順序很重要
    log_info "創建 Namespace..."
    envsubst < "$K8S_DIR/namespace.yaml" | kubectl apply -f -

    log_info "創建 ConfigMaps..."
    envsubst < "$K8S_DIR/configmaps.yaml" | kubectl apply -f -

    log_info "創建 Secrets..."
    envsubst < "$K8S_DIR/secrets.yaml" | kubectl apply -f -

    log_info "部署基礎設施..."
    envsubst < "$K8S_DIR/infrastructure.yaml" | kubectl apply -f -

    # 等待基礎設施就緒
    log_info "等待 Redis 就緒..."
    kubectl -n $NAMESPACE rollout status statefulset/redis --timeout=120s

    log_info "等待 ClickHouse 就緒..."
    kubectl -n $NAMESPACE rollout status statefulset/clickhouse --timeout=180s

    log_info "部署 Sentinel..."
    envsubst < "$K8S_DIR/sentinel.yaml" | kubectl apply -f -

    log_info "部署 Market Data 服務..."
    envsubst < "$K8S_DIR/market-data.yaml" | kubectl apply -f -

    log_info "部署 Trading Engine..."
    envsubst < "$K8S_DIR/trading-engine.yaml" | kubectl apply -f -

    log_info "Kubernetes 部署完成!"
}

# 函數: 啟動本地開發環境
dev_up() {
    log_info "啟動本地開發環境..."
    cd "$DOCKER_DIR"
    docker compose up -d redis clickhouse prometheus grafana
    log_info "本地開發環境已啟動"
    log_info "  Redis: localhost:6379"
    log_info "  ClickHouse: localhost:8123"
    log_info "  Prometheus: localhost:9090"
    log_info "  Grafana: localhost:3000 (admin/admin)"
}

# 函數: 停止本地開發環境
dev_down() {
    log_info "停止本地開發環境..."
    cd "$DOCKER_DIR"
    docker compose down
    log_info "本地開發環境已停止"
}

# 函數: 顯示狀態
status() {
    log_info "檢查 Kubernetes 狀態..."

    if ! command -v kubectl &> /dev/null; then
        log_warn "kubectl 未安裝，僅顯示 Docker 狀態"
        docker ps --filter "name=hft-"
        return
    fi

    echo ""
    echo "=== Pods ==="
    kubectl -n $NAMESPACE get pods -o wide 2>/dev/null || log_warn "無法獲取 Pods"

    echo ""
    echo "=== Services ==="
    kubectl -n $NAMESPACE get svc 2>/dev/null || log_warn "無法獲取 Services"

    echo ""
    echo "=== Deployments ==="
    kubectl -n $NAMESPACE get deployments 2>/dev/null || log_warn "無法獲取 Deployments"
}

# 函數: 顯示日誌
logs() {
    local service=${1:-trading-engine}
    log_info "顯示 $service 日誌..."

    if command -v kubectl &> /dev/null; then
        kubectl -n $NAMESPACE logs -f -l app=$service --tail=100
    else
        docker logs -f "hft-$service" 2>/dev/null || log_error "容器不存在"
    fi
}

# 函數: 顯示幫助
usage() {
    echo "HFT 系統部署腳本"
    echo ""
    echo "用法: $0 <command> [options]"
    echo ""
    echo "Commands:"
    echo "  build       構建所有 Docker 鏡像"
    echo "  push        推送鏡像到 Registry"
    echo "  deploy      部署到 Kubernetes"
    echo "  dev-up      啟動本地開發環境"
    echo "  dev-down    停止本地開發環境"
    echo "  status      顯示部署狀態"
    echo "  logs [svc]  顯示服務日誌"
    echo "  all         構建 + 推送 + 部署"
    echo ""
    echo "環境變數:"
    echo "  ECR_REGISTRY  容器 Registry 地址 (默認: localhost:5000)"
    echo "  IMAGE_TAG     鏡像標籤 (默認: latest)"
    echo ""
    echo "範例:"
    echo "  $0 build"
    echo "  $0 deploy"
    echo "  ECR_REGISTRY=xxx.dkr.ecr.ap-southeast-1.amazonaws.com IMAGE_TAG=v1.0.0 $0 all"
}

# 主入口
case "${1:-}" in
    build)
        build_images
        ;;
    push)
        push_images
        ;;
    deploy)
        deploy_k8s
        ;;
    dev-up)
        dev_up
        ;;
    dev-down)
        dev_down
        ;;
    status)
        status
        ;;
    logs)
        logs "${2:-}"
        ;;
    all)
        build_images
        push_images
        deploy_k8s
        ;;
    *)
        usage
        ;;
esac
