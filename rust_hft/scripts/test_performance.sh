#!/bin/bash
# Rust HFT 性能測試腳本

set -e

echo "🚀 Rust HFT Performance Test Script"
echo "=================================="

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 檢查 Docker
check_docker() {
    if ! command -v docker &> /dev/null; then
        echo -e "${RED}❌ Docker is not installed${NC}"
        exit 1
    fi
    
    if ! docker ps &> /dev/null; then
        echo -e "${RED}❌ Docker daemon is not running${NC}"
        exit 1
    fi
    
    echo -e "${GREEN}✅ Docker is ready${NC}"
}

# 啟動服務
start_services() {
    echo -e "\n${BLUE}📦 Starting services...${NC}"
    
    # 只啟動 ClickHouse 和 Redis
    docker-compose up -d clickhouse redis
    
    # 等待服務就緒
    echo -e "${YELLOW}⏳ Waiting for services to be ready...${NC}"
    
    # 等待 ClickHouse
    for i in {1..30}; do
        if docker-compose exec -T clickhouse clickhouse-client --query "SELECT 1" &> /dev/null; then
            echo -e "${GREEN}✅ ClickHouse is ready${NC}"
            break
        fi
        if [ $i -eq 30 ]; then
            echo -e "${RED}❌ ClickHouse failed to start${NC}"
            exit 1
        fi
        sleep 1
    done
    
    # 等待 Redis
    for i in {1..10}; do
        if docker-compose exec -T redis redis-cli ping &> /dev/null; then
            echo -e "${GREEN}✅ Redis is ready${NC}"
            break
        fi
        if [ $i -eq 10 ]; then
            echo -e "${RED}❌ Redis failed to start${NC}"
            exit 1
        fi
        sleep 1
    done
}

# 初始化數據庫
init_database() {
    echo -e "\n${BLUE}🗄️  Initializing database...${NC}"
    
    # 創建數據庫
    docker-compose exec -T clickhouse clickhouse-client --query "CREATE DATABASE IF NOT EXISTS hft"
    
    # 創建表
    docker-compose exec -T clickhouse clickhouse-client --database hft --query "
    CREATE TABLE IF NOT EXISTS orderbook_snapshots (
        timestamp DateTime64(3),
        symbol String,
        bid_price Float64,
        bid_quantity Float64,
        ask_price Float64,
        ask_quantity Float64,
        spread Float64,
        mid_price Float64
    ) ENGINE = MergeTree()
    ORDER BY (symbol, timestamp)
    TTL timestamp + INTERVAL 7 DAY"
    
    docker-compose exec -T clickhouse clickhouse-client --database hft --query "
    CREATE TABLE IF NOT EXISTS trades (
        timestamp DateTime64(3),
        symbol String,
        price Float64,
        quantity Float64,
        side String,
        trade_id String
    ) ENGINE = MergeTree()
    ORDER BY (symbol, timestamp)
    TTL timestamp + INTERVAL 30 DAY"
    
    docker-compose exec -T clickhouse clickhouse-client --database hft --query "
    CREATE TABLE IF NOT EXISTS performance_metrics (
        timestamp DateTime64(3),
        metric_name String,
        value Float64,
        tags Map(String, String)
    ) ENGINE = MergeTree()
    ORDER BY (metric_name, timestamp)
    TTL timestamp + INTERVAL 90 DAY"
    
    echo -e "${GREEN}✅ Database initialized${NC}"
}

# 構建項目
build_project() {
    echo -e "\n${BLUE}🔨 Building project...${NC}"
    
    # 使用 release 模式構建性能測試相關的 examples
    cargo build --release --example ws_performance_test
    
    if [ $? -eq 0 ]; then
        echo -e "${GREEN}✅ Build successful${NC}"
    else
        echo -e "${RED}❌ Build failed${NC}"
        exit 1
    fi
}

# 運行測試
run_test() {
    local test_type=$1
    local config_file=${2:-"config/performance_test.yaml"}
    
    echo -e "\n${BLUE}🧪 Running $test_type test...${NC}"
    
    case $test_type in
        "ws")
            echo "Testing WebSocket throughput only..."
            cargo run --release --example ws_performance_test -- --config $config_file --verbose
            ;;
        "ch")
            echo "Testing ClickHouse write performance (using ws_performance_test with CH output)..."
            cargo run --release --example ws_performance_test -- --config $config_file --clickhouse --verbose
            ;;
        "full")
            echo "Running full performance test..."
            cargo run --release --example ws_performance_test -- --config $config_file --verbose
            ;;
        *)
            echo -e "${RED}Unknown test type: $test_type${NC}"
            echo "Usage: $0 [ws|ch|full] [config_file]"
            exit 1
            ;;
    esac
}

# 顯示結果
show_results() {
    echo -e "\n${BLUE}📊 Test Results:${NC}"
    
    # 查找最新的結果文件
    latest_result=$(ls -t performance_results_*.json 2>/dev/null | head -1)
    
    if [ -n "$latest_result" ]; then
        echo -e "${GREEN}Latest result file: $latest_result${NC}"
        
        # 使用 jq 格式化輸出（如果可用）
        if command -v jq &> /dev/null; then
            jq . "$latest_result"
        else
            cat "$latest_result"
        fi
        
        # 從 ClickHouse 查詢一些統計
        echo -e "\n${BLUE}📈 Database Statistics:${NC}"
        
        echo -e "\nOrderBook snapshots:"
        docker-compose exec -T clickhouse clickhouse-client --database hft --query \
            "SELECT count() as total_rows, 
                    max(timestamp) as latest_timestamp,
                    uniqExact(symbol) as unique_symbols
             FROM orderbook_snapshots" 2>/dev/null || echo "No data yet"
        
        echo -e "\nTrades:"
        docker-compose exec -T clickhouse clickhouse-client --database hft --query \
            "SELECT count() as total_rows,
                    max(timestamp) as latest_timestamp,
                    uniqExact(symbol) as unique_symbols
             FROM trades" 2>/dev/null || echo "No data yet"
    else
        echo -e "${YELLOW}No result files found${NC}"
    fi
}

# 清理
cleanup() {
    echo -e "\n${BLUE}🧹 Cleaning up...${NC}"
    
    read -p "Stop services? (y/n) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        docker-compose down
        echo -e "${GREEN}✅ Services stopped${NC}"
    fi
}

# 主函數
main() {
    local test_type=${1:-"full"}
    local config_file=${2:-"config/performance_test.yaml"}
    
    echo "Test type: $test_type"
    echo "Config file: $config_file"
    
    # 檢查配置文件
    if [ ! -f "$config_file" ]; then
        echo -e "${RED}❌ Config file not found: $config_file${NC}"
        exit 1
    fi
    
    # 執行步驟
    check_docker
    start_services
    init_database
    build_project
    run_test "$test_type" "$config_file"
    show_results
    
    # 詢問是否清理
    cleanup
}

# 捕獲 Ctrl+C
trap cleanup INT

# 執行主函數
main "$@"