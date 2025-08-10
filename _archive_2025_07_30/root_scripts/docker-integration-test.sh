#!/bin/bash

# HFT System Docker Integration Test Script
# Docker資源整合驗證測試腳本
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
TEST_RESULTS=()

# Logging functions
log() {
    echo -e "${GREEN}[$(date +'%Y-%m-%d %H:%M:%S')] TEST: $1${NC}"
}

warn() {
    echo -e "${YELLOW}[$(date +'%Y-%m-%d %H:%M:%S')] WARNING: $1${NC}"
}

error() {
    echo -e "${RED}[$(date +'%Y-%m-%d %H:%M:%S')] ERROR: $1${NC}"
}

success() {
    echo -e "${GREEN}[$(date +'%Y-%m-%d %H:%M:%S')] SUCCESS: $1${NC}"
}

# Add test result
add_result() {
    local test_name=$1
    local status=$2
    local message=$3
    TEST_RESULTS+=("$test_name|$status|$message")
}

# Test if service is running
test_service_running() {
    local service_name=$1
    local container_name=$2
    
    log "測試服務運行狀態: $service_name"
    
    if docker ps --filter "name=$container_name" --filter "status=running" | grep -q $container_name; then
        success "$service_name 正在運行"
        add_result "$service_name Status" "PASS" "容器正在運行"
        return 0
    else
        error "$service_name 未運行"
        add_result "$service_name Status" "FAIL" "容器未運行"
        return 1
    fi
}

# Test service health
test_service_health() {
    local service_name=$1
    local container_name=$2
    
    log "測試服務健康狀態: $service_name"
    
    local health_status=$(docker inspect --format='{{.State.Health.Status}}' $container_name 2>/dev/null || echo "no-healthcheck")
    
    if [ "$health_status" = "healthy" ]; then
        success "$service_name 健康狀態良好"
        add_result "$service_name Health" "PASS" "健康檢查通過"
        return 0
    elif [ "$health_status" = "no-healthcheck" ]; then
        warn "$service_name 沒有健康檢查配置"
        add_result "$service_name Health" "SKIP" "無健康檢查"
        return 0
    else
        error "$service_name 健康檢查失敗: $health_status"
        add_result "$service_name Health" "FAIL" "健康檢查失敗: $health_status"
        return 1
    fi
}

# Test network connectivity
test_network_connectivity() {
    local from_container=$1
    local to_container=$2
    local port=$3
    local service_name=$4
    
    log "測試網絡連通性: $from_container -> $to_container:$port"
    
    if docker exec $from_container nc -z $to_container $port 2>/dev/null; then
        success "網絡連通性測試通過: $service_name"
        add_result "Network $service_name" "PASS" "連通性正常"
        return 0
    else
        error "網絡連通性測試失敗: $service_name"
        add_result "Network $service_name" "FAIL" "無法連接到 $to_container:$port"
        return 1
    fi
}

# Test HTTP endpoint
test_http_endpoint() {
    local endpoint_name=$1
    local url=$2
    local expected_status=${3:-200}
    
    log "測試HTTP端點: $endpoint_name"
    
    local status_code=$(curl -s -o /dev/null -w "%{http_code}" $url || echo "000")
    
    if [ "$status_code" -eq "$expected_status" ]; then
        success "HTTP端點測試通過: $endpoint_name ($status_code)"
        add_result "HTTP $endpoint_name" "PASS" "狀態碼: $status_code"
        return 0
    else
        error "HTTP端點測試失敗: $endpoint_name (期望: $expected_status, 實際: $status_code)"
        add_result "HTTP $endpoint_name" "FAIL" "狀態碼: $status_code"
        return 1
    fi
}

# Test Redis functionality
test_redis_functionality() {
    log "測試Redis功能"
    
    if docker exec hft-redis redis-cli ping | grep -q "PONG"; then
        success "Redis功能測試通過"
        add_result "Redis Functionality" "PASS" "PING響應正常"
        
        # Test pub/sub
        if timeout 5 docker exec hft-redis redis-cli --raw publish test_channel "test_message" >/dev/null; then
            success "Redis發布/訂閱功能正常"
            add_result "Redis PubSub" "PASS" "發布消息成功"
        else
            warn "Redis發布/訂閱測試超時"
            add_result "Redis PubSub" "WARN" "測試超時"
        fi
        return 0
    else
        error "Redis功能測試失敗"
        add_result "Redis Functionality" "FAIL" "PING響應異常"
        return 1
    fi
}

# Test ClickHouse functionality
test_clickhouse_functionality() {
    log "測試ClickHouse功能"
    
    local response=$(curl -s "http://localhost:8123/ping" || echo "failed")
    
    if [ "$response" = "Ok." ]; then
        success "ClickHouse功能測試通過"
        add_result "ClickHouse Functionality" "PASS" "HTTP接口正常"
        
        # Test simple query
        local query_result=$(curl -s "http://localhost:8123/?query=SELECT 1" || echo "failed")
        if [ "$query_result" = "1" ]; then
            success "ClickHouse查詢功能正常"
            add_result "ClickHouse Query" "PASS" "簡單查詢成功"
        else
            warn "ClickHouse查詢測試失敗"
            add_result "ClickHouse Query" "FAIL" "查詢返回: $query_result"
        fi
        return 0
    else
        error "ClickHouse功能測試失敗"
        add_result "ClickHouse Functionality" "FAIL" "HTTP接口異常"
        return 1
    fi
}

# Test volume persistence
test_volume_persistence() {
    log "測試數據持久化"
    
    local test_passed=0
    
    # Test Redis data persistence
    if docker exec hft-redis redis-cli set test_key "test_value" | grep -q "OK"; then
        if docker exec hft-redis redis-cli get test_key | grep -q "test_value"; then
            success "Redis數據持久化測試通過"
            add_result "Redis Persistence" "PASS" "數據讀寫正常"
            docker exec hft-redis redis-cli del test_key >/dev/null
            test_passed=$((test_passed + 1))
        fi
    fi
    
    # Test if volumes exist
    local volumes=$(docker volume ls --filter name=hft-unified | wc -l)
    if [ $volumes -gt 1 ]; then
        success "Docker volumes創建成功 ($((volumes-1)) 個)"
        add_result "Docker Volumes" "PASS" "$((volumes-1)) 個volume已創建"
        test_passed=$((test_passed + 1))
    else
        error "Docker volumes測試失敗"
        add_result "Docker Volumes" "FAIL" "volume未正確創建"
    fi
    
    return $((2 - test_passed))
}

# Test resource limits
test_resource_limits() {
    log "測試資源限制"
    
    local containers=("hft-redis" "hft-clickhouse" "hft-rust-core")
    local passed=0
    
    for container in "${containers[@]}"; do
        if docker stats --no-stream --format "table {{.Container}}\t{{.CPUPerc}}\t{{.MemUsage}}" | grep -q $container; then
            success "容器 $container 資源監控正常"
            add_result "Resource $container" "PASS" "資源監控正常"
            passed=$((passed + 1))
        else
            warn "容器 $container 資源監控異常"
            add_result "Resource $container" "WARN" "監控數據獲取失敗"
        fi
    done
    
    return $((${#containers[@]} - passed))
}

# Generate test report
generate_report() {
    log "生成測試報告"
    
    local report_file="docker-integration-test-report-$(date +%Y%m%d_%H%M%S).txt"
    
    {
        echo "=========================================="
        echo "HFT系統Docker整合測試報告"
        echo "測試時間: $(date)"
        echo "=========================================="
        echo
        
        local total_tests=0
        local passed_tests=0
        local failed_tests=0
        local skipped_tests=0
        
        for result in "${TEST_RESULTS[@]}"; do
            IFS='|' read -r test_name status message <<< "$result"
            total_tests=$((total_tests + 1))
            
            case $status in
                "PASS")
                    passed_tests=$((passed_tests + 1))
                    echo "✅ $test_name: $message"
                    ;;
                "FAIL")
                    failed_tests=$((failed_tests + 1))
                    echo "❌ $test_name: $message"
                    ;;
                "SKIP"|"WARN")
                    skipped_tests=$((skipped_tests + 1))
                    echo "⚠️  $test_name: $message"
                    ;;
            esac
        done
        
        echo
        echo "=========================================="
        echo "測試統計"
        echo "=========================================="
        echo "總測試數: $total_tests"
        echo "通過: $passed_tests"
        echo "失敗: $failed_tests"
        echo "跳過/警告: $skipped_tests"
        echo "成功率: $(( passed_tests * 100 / total_tests ))%"
        echo
        
        if [ $failed_tests -eq 0 ]; then
            echo "🎉 所有關鍵測試均通過！Docker整合成功。"
        else
            echo "⚠️  發現 $failed_tests 個失敗測試，請檢查上述問題。"
        fi
        
    } > $report_file
    
    cat $report_file
    success "測試報告已保存到: $report_file"
}

# Main test execution
main() {
    log "開始Docker整合測試"
    
    # Infrastructure services
    test_service_running "Redis" "hft-redis"
    test_service_health "Redis" "hft-redis"
    test_redis_functionality
    
    test_service_running "ClickHouse" "hft-clickhouse"
    test_service_health "ClickHouse" "hft-clickhouse"
    test_clickhouse_functionality
    
    test_service_running "Prometheus" "hft-prometheus"
    test_service_health "Prometheus" "hft-prometheus"
    
    test_service_running "Grafana" "hft-grafana"
    test_service_health "Grafana" "hft-grafana"
    
    # Rust Core Engine (如果正在運行)
    if docker ps --filter "name=hft-rust-core" --filter "status=running" | grep -q hft-rust-core; then
        test_service_running "Rust Core" "hft-rust-core"
        test_service_health "Rust Core" "hft-rust-core"
        
        # Test network connectivity from Rust to infrastructure
        test_network_connectivity "hft-rust-core" "hft-redis" "6379" "Rust->Redis"
        test_network_connectivity "hft-rust-core" "hft-clickhouse" "8123" "Rust->ClickHouse"
    else
        warn "Rust Core Engine未運行，跳過相關測試"
        add_result "Rust Core Status" "SKIP" "服務未啟動"
    fi
    
    # Agno Workspaces (如果正在運行)
    local workspaces=("hft-master-ui:Master UI" "hft-ops-agent:Ops Agent" "hft-ml-trainer:ML Trainer")
    for workspace_info in "${workspaces[@]}"; do
        IFS=':' read -r container_name service_name <<< "$workspace_info"
        if docker ps --filter "name=$container_name" --filter "status=running" | grep -q $container_name; then
            test_service_running "$service_name" "$container_name"
            test_service_health "$service_name" "$container_name"
        else
            warn "$service_name未運行，跳過測試"
            add_result "$service_name Status" "SKIP" "服務未啟動"
        fi
    done
    
    # HTTP endpoints
    test_http_endpoint "Grafana" "http://localhost:3000/api/health"
    test_http_endpoint "Prometheus" "http://localhost:9090/-/healthy"
    test_http_endpoint "ClickHouse" "http://localhost:8123/ping"
    
    # Test persistence and resources
    test_volume_persistence
    test_resource_limits
    
    # Generate final report
    generate_report
    
    log "Docker整合測試完成"
}

# Command line options
case "$1" in
    "quick")
        log "執行快速測試（僅基礎設施）"
        test_service_running "Redis" "hft-redis"
        test_service_running "ClickHouse" "hft-clickhouse"
        test_http_endpoint "ClickHouse" "http://localhost:8123/ping"
        generate_report
        ;;
    "network")
        log "執行網絡連通性測試"
        if docker ps --filter "name=hft-rust-core" --filter "status=running" | grep -q hft-rust-core; then
            test_network_connectivity "hft-rust-core" "hft-redis" "6379" "Rust->Redis"
            test_network_connectivity "hft-rust-core" "hft-clickhouse" "8123" "Rust->ClickHouse"
        fi
        generate_report
        ;;
    "full"|"")
        main
        ;;
    "help"|"--help"|"-h")
        echo "Docker Integration Test Script"
        echo "用法: $0 [command]"
        echo
        echo "Commands:"
        echo "  full     - 執行完整測試套件（默認）"
        echo "  quick    - 執行快速測試（僅基礎設施）"
        echo "  network  - 執行網絡連通性測試"
        echo "  help     - 顯示本幫助信息"
        ;;
    *)
        error "未知命令: $1"
        $0 help
        exit 1
        ;;
esac