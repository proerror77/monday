#!/bin/bash

# HFT Master Workspace 編排演示腳本
# 展示 Master Workspace 如何自動協調整個 HFT 系統

set -e

echo "🎭 HFT Master Workspace 編排演示"
echo "=================================================="

# 顏色定義
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
NC='\033[0m'

echo -e "${CYAN}📋 演示流程概述:${NC}"
echo "1. 啟動 Master Workspace"
echo "2. Master 自動檢查系統狀態"
echo "3. Master 協調 Ops 和 ML Workspace"
echo "4. Master 準備 Rust HFT Core"
echo "5. Master 啟動完整交易系統"
echo

# 步驟 1: 檢查當前系統狀態
echo -e "${BLUE}步驟 1: 檢查當前系統狀態${NC}"
echo "----------------------------------------"

echo "🔍 檢查 Docker Compose Stack 狀態..."
./hft-compose.sh status | head -20

echo
echo "🌐 檢查關鍵服務可用性..."

# 檢查 Master Workspace
if curl -s -o /dev/null -w "%{http_code}" http://localhost:8504 | grep -q "200"; then
    echo -e "${GREEN}✅ Master Workspace (8504) - 可訪問${NC}"
else
    echo -e "${RED}❌ Master Workspace (8504) - 不可訪問${NC}"
fi

# 檢查 Ops Workspace
if curl -s -o /dev/null -w "%{http_code}" http://localhost:8503 | grep -q "200"; then
    echo -e "${GREEN}✅ Ops Workspace (8503) - 可訪問${NC}"
else
    echo -e "${RED}❌ Ops Workspace (8503) - 不可訪問${NC}"
fi

# 檢查 ML Workspace
if curl -s -o /dev/null -w "%{http_code}" http://localhost:8502 | grep -q "200"; then
    echo -e "${GREEN}✅ ML Workspace (8502) - 可訪問${NC}"
else
    echo -e "${RED}❌ ML Workspace (8502) - 不可訪問${NC}"
fi

# 檢查基礎設施
if redis-cli -h localhost -p 6379 ping &> /dev/null; then
    echo -e "${GREEN}✅ Redis - 可訪問${NC}"
else
    echo -e "${RED}❌ Redis - 不可訪問${NC}"
fi

if curl -s http://localhost:8123/ping | grep -q "Ok"; then
    echo -e "${GREEN}✅ ClickHouse - 可訪問${NC}"
else
    echo -e "${RED}❌ ClickHouse - 不可訪問${NC}"
fi

echo

# 步驟 2: 展示 Master Workspace 自動化功能
echo -e "${BLUE}步驟 2: Master Workspace 自動化功能展示${NC}"
echo "----------------------------------------"

echo -e "${YELLOW}💡 Master Workspace 功能:${NC}"
echo "• 🔄 自動系統初始化檢查"
echo "• 📊 實時基礎設施健康監控"
echo "• 🎯 工作區狀態協調"
echo "• ⚙️ Rust 系統準備和啟動"
echo "• 🚨 緊急故障處理"
echo

# 步驟 3: 展示系統編排邏輯
echo -e "${BLUE}步驟 3: 系統編排邏輯展示${NC}"
echo "----------------------------------------"

echo -e "${YELLOW}🎼 編排順序:${NC}"
echo "1️⃣ 基礎設施準備 (Redis, ClickHouse)"
echo "2️⃣ 數據庫初始化 (PostgreSQL schemas)"
echo "3️⃣ 工作區健康檢查 (Ops, ML)"
echo "4️⃣ Rust 系統編譯檢查"
echo "5️⃣ 市場數據連接測試"
echo "6️⃣ 系統就緒確認"
echo

# 步驟 4: 展示 Rust 系統準備
echo -e "${BLUE}步驟 4: Rust HFT Core 準備展示${NC}"
echo "----------------------------------------"

echo "🦀 檢查 Rust 項目狀態..."
cd rust_hft

echo "📦 檢查項目依賴..."
if cargo check --quiet; then
    echo -e "${GREEN}✅ Rust 項目編譯檢查通過${NC}"
else
    echo -e "${RED}❌ Rust 項目編譯檢查失敗${NC}"
fi

echo "📁 檢查關鍵文件..."
if [ -f "src/main.rs" ]; then
    echo -e "${GREEN}✅ 主程序文件存在${NC}"
fi

if [ -f "Cargo.toml" ]; then
    echo -e "${GREEN}✅ 項目配置文件存在${NC}"
fi

echo "🏗️ 檢查可用的運行目標..."
echo "• rust_hft - 主要交易引擎"
echo "• simple_hft - 簡化交易引擎"

echo "🧪 檢查可用的測試..."
echo "• performance_e2e_test - 端到端性能測試"

cd ..

# 步驟 5: 展示訪問地址
echo -e "${BLUE}步驟 5: 系統訪問地址${NC}"
echo "----------------------------------------"

echo -e "${CYAN}🌐 Web 界面訪問地址:${NC}"
echo "🎯 Master Control Center: http://localhost:8504"
echo "⚡ Operations Dashboard:  http://localhost:8503"  
echo "🧠 ML Workspace:          http://localhost:8502"
echo

echo -e "${CYAN}📊 基礎設施訪問:${NC}"
echo "📦 Redis:        localhost:6379"
echo "🏛️ ClickHouse:   localhost:8123 (HTTP)"
echo "   ClickHouse:   localhost:9000 (Native)"
echo

# 步驟 6: 展示管理命令
echo -e "${BLUE}步驟 6: 系統管理命令${NC}"
echo "----------------------------------------"

echo -e "${CYAN}🔧 系統管理:${NC}"
echo "# 查看系統狀態"
echo "./hft-compose.sh status"
echo
echo "# 查看服務日誌"
echo "./hft-compose.sh logs"
echo
echo "# 重啟服務"
echo "./hft-compose.sh restart"
echo

echo -e "${CYAN}⚡ Rust 系統控制:${NC}"
echo "# 啟動交易引擎"
echo "cd rust_hft && cargo run --release --bin rust_hft"
echo
echo "# 啟動數據收集"
echo "cd rust_hft && cargo run --release --bin simple_hft"
echo
echo "# 運行性能測試"
echo "cd rust_hft && cargo run --release --example performance_e2e_test"
echo

# 總結
echo -e "${GREEN}🎉 HFT Master Workspace 編排演示完成！${NC}"
echo "=================================================="

echo -e "${YELLOW}💡 關鍵特性總結:${NC}"
echo "✨ Master Workspace 在啟動時會自動:"
echo "   • 檢查所有基礎設施健康狀態"
echo "   • 協調 Ops 和 ML Workspace 的運行"
echo "   • 準備 Rust HFT Core 系統"
echo "   • 提供統一的系統控制界面"
echo "   • 監控整個系統的運行狀態"
echo
echo "🚀 現在您可以訪問 http://localhost:8504 開始使用 Master Control Center！"