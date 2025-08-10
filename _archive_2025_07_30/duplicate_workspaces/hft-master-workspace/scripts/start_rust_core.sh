#!/bin/bash

# Rust HFT Core 啟動腳本
# 由 Master Controller 調用以啟動 Rust 交易引擎

set -e

echo "🚀 Starting Rust HFT Core System"
echo "=================================="

# 顏色定義
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

# 檢查 Rust 項目路徑
RUST_PATH="/app/../rust_hft"
if [ ! -d "$RUST_PATH" ]; then
    echo -e "${RED}❌ Rust HFT 項目路徑不存在: $RUST_PATH${NC}"
    exit 1
fi

cd "$RUST_PATH"

echo -e "${BLUE}📋 檢查 Rust 環境...${NC}"

# 檢查 Cargo
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}❌ Cargo 未安裝${NC}"
    exit 1
fi

echo -e "${GREEN}✅ Cargo 可用${NC}"

# 檢查項目是否可以編譯
echo -e "${BLUE}🔧 檢查項目編譯狀態...${NC}"
if cargo check --quiet; then
    echo -e "${GREEN}✅ 項目編譯檢查通過${NC}"
else
    echo -e "${RED}❌ 項目編譯檢查失敗${NC}"
    exit 1
fi

# 檢查基礎設施連接
echo -e "${BLUE}🌐 檢查基礎設施連接...${NC}"

# 檢查 Redis
if redis-cli -h hft-redis -p 6379 ping &> /dev/null; then
    echo -e "${GREEN}✅ Redis 連接正常${NC}"
else
    echo -e "${RED}❌ Redis 連接失敗${NC}"
    exit 1
fi

# 檢查 ClickHouse
if curl -s http://hft-clickhouse:8123/ping | grep -q "Ok"; then
    echo -e "${GREEN}✅ ClickHouse 連接正常${NC}"
else
    echo -e "${RED}❌ ClickHouse 連接失敗${NC}"
    exit 1
fi

# 選擇啟動模式
echo -e "${BLUE}🎯 選擇啟動模式...${NC}"

case "${1:-trading}" in
    "test")
        echo -e "${YELLOW}🧪 啟動測試模式${NC}"
        echo "執行性能端到端測試..."
        cargo run --release --example performance_e2e_test
        ;;
    "collect")
        echo -e "${YELLOW}📊 啟動數據收集模式${NC}"
        echo "啟動市場數據收集器..."
        cargo run --release --bin simple_hft
        ;;
    "trading")
        echo -e "${YELLOW}⚡ 啟動完整交易模式${NC}"
        echo "啟動 HFT 交易引擎..."
        
        # 設置環境變數
        export RUST_LOG=info
        export RUST_BACKTRACE=1
        
        # 啟動主要交易引擎
        cargo run --release --bin rust_hft
        ;;
    *)
        echo -e "${RED}❌ 未知的啟動模式: $1${NC}"
        echo "可用模式: test, collect, trading"
        exit 1
        ;;
esac

echo -e "${GREEN}🎉 Rust HFT Core 啟動完成！${NC}"