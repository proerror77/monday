#!/bin/bash

# HFT 完整項目啟動腳本
# 符合 Agno 官方規範的統一啟動流程

set -e

echo "🚀 啟動 HFT 高頻交易系統"
echo "================================"

# 顏色定義
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

# 檢查必要條件
echo -e "${BLUE}📋 檢查系統前置條件...${NC}"

if ! command -v docker &> /dev/null; then
    echo -e "${RED}❌ Docker 未安裝${NC}"
    exit 1
fi

if ! command -v docker-compose &> /dev/null; then
    echo -e "${RED}❌ Docker Compose 未安裝${NC}"
    exit 1
fi

if ! command -v cargo &> /dev/null; then
    echo -e "${RED}❌ Rust/Cargo 未安裝${NC}"
    exit 1
fi

echo -e "${GREEN}✅ 系統前置條件檢查完成${NC}"

# 啟動 Docker Compose Stack
echo -e "${BLUE}🐳 啟動 Docker Compose Stack...${NC}"
./hft-compose.sh up

# 等待服務完全就緒
echo -e "${BLUE}⏳ 等待服務完全就緒...${NC}"
sleep 10

# 檢查服務健康狀態
echo -e "${BLUE}🏥 檢查服務健康狀態...${NC}"

services=(
    "http://localhost:8504:Master UI"
    "http://localhost:8503:Ops UI" 
    "http://localhost:8502:ML UI"
    "http://localhost:8123/ping:ClickHouse"
)

all_healthy=true

for service_info in "${services[@]}"; do
    IFS=':' read -r url name <<< "$service_info"
    
    if curl -s -o /dev/null -w "%{http_code}" "$url" | grep -q "200"; then
        echo -e "${GREEN}✅ $name 健康${NC}"
    else
        echo -e "${RED}❌ $name 不可訪問${NC}"
        all_healthy=false
    fi
done

# 檢查 Redis
if redis-cli -h localhost -p 6379 ping &> /dev/null; then
    echo -e "${GREEN}✅ Redis 健康${NC}"
else
    echo -e "${RED}❌ Redis 不可訪問${NC}"
    all_healthy=false
fi

if [ "$all_healthy" = false ]; then
    echo -e "${RED}❌ 部分服務未就緒，請檢查日誌${NC}"
    echo "使用命令查看日誌: ./hft-compose.sh logs"
    exit 1
fi

# 編譯 Rust 核心組件
echo -e "${BLUE}⚙️ 編譯 Rust 核心組件...${NC}"
cd rust_hft
cargo build --release
cd ..

echo -e "${GREEN}🎉 HFT 系統啟動完成！${NC}"
echo
echo "================================"
echo -e "${YELLOW}🌐 系統訪問地址:${NC}"
echo
echo -e "🎯 ${GREEN}HFT Master Control:${NC} http://localhost:8504"
echo -e "⚡ ${GREEN}HFT Operations:${NC}     http://localhost:8503"  
echo -e "🧠 ${GREEN}HFT ML Workspace:${NC}   http://localhost:8502"
echo
echo "================================"
echo -e "${YELLOW}🔧 管理命令:${NC}"
echo
echo "查看狀態:   ./hft-compose.sh status"
echo "查看日誌:   ./hft-compose.sh logs"
echo "重啟服務:   ./hft-compose.sh restart"
echo "停止服務:   ./hft-compose.sh down"
echo
echo "啟動 Rust 核心引擎:"
echo "cd rust_hft && cargo run --release --bin rust_hft"
echo
echo "================================"
echo -e "${GREEN}✨ 系統已準備就緒，開始您的 HFT 交易之旅！${NC}"