#!/bin/bash
# Hyperliquid 完整演示啟動腳本
#
# 使用方法:
#   ./scripts/run_hyperliquid_demo.sh paper       # Paper 模式
#   ./scripts/run_hyperliquid_demo.sh live        # Live 模式（需要私鑰）
#   ./scripts/run_hyperliquid_demo.sh test        # Paper 模式 + 測試下單

set -e

# 顏色輸出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 顯示幫助信息
show_help() {
    echo -e "${BLUE}Hyperliquid 完整演示啟動腳本${NC}"
    echo ""
    echo "使用方法:"
    echo "  $0 paper                  # Paper 模式（安全模擬）"
    echo "  $0 live                   # Live 模式（真實交易）"
    echo "  $0 test                   # Paper 模式 + 測試下單功能"
    echo ""
    echo "環境變量:"
    echo "  HYPERLIQUID_PRIVATE_KEY   # Live 模式所需的私鑰"
    echo "  RUST_LOG                  # 日誌級別（可選）"
    echo ""
    echo "示例:"
    echo "  HYPERLIQUID_PRIVATE_KEY=abc123... $0 live"
    echo "  RUST_LOG=debug $0 paper"
}

# 檢查參數
MODE="${1:-}"
if [[ -z "$MODE" ]]; then
    show_help
    exit 1
fi

# 切換到項目根目錄
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

echo -e "${BLUE}🚀 Hyperliquid 完整演示啟動器${NC}"
echo ""

# 檢查 Rust 和 Cargo
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}❌ 錯誤: 未找到 cargo，請安裝 Rust${NC}"
    exit 1
fi

# 根據模式設置參數
case "$MODE" in
    "paper")
        echo -e "${GREEN}📝 Paper 模式：安全的模擬交易${NC}"
        DEMO_ARGS="--mode paper --duration 30"
        ;;
    "live")
        echo -e "${YELLOW}⚠️  Live 模式：真實交易模式！${NC}"
        if [[ -z "${HYPERLIQUID_PRIVATE_KEY:-}" ]]; then
            echo -e "${RED}❌ 錯誤: Live 模式需要設置 HYPERLIQUID_PRIVATE_KEY 環境變量${NC}"
            echo ""
            echo "獲取私鑰的步驟："
            echo "1. 登錄 https://app.hyperliquid.xyz/"
            echo "2. 從 MetaMask 導出私鑰（64 位十六進制，不含 0x）"
            echo "3. 設置環境變量: export HYPERLIQUID_PRIVATE_KEY=your_key"
            exit 1
        fi
        DEMO_ARGS="--mode live --duration 30"
        ;;
    "test")
        echo -e "${GREEN}🧪 Paper 模式 + 測試下單功能${NC}"
        DEMO_ARGS="--mode paper --duration 30 --test-orders"
        ;;
    *)
        echo -e "${RED}❌ 錯誤: 未知模式 '$MODE'${NC}"
        echo ""
        show_help
        exit 1
        ;;
esac

echo ""
echo -e "${BLUE}配置檢查...${NC}"

# 檢查項目結構
if [[ ! -f "apps/hyperliquid-demo/Cargo.toml" ]]; then
    echo -e "${RED}❌ 錯誤: 未找到 hyperliquid-demo 應用${NC}"
    exit 1
fi

# 編譯檢查
echo -e "${YELLOW}🔨 編譯檢查...${NC}"
if ! cargo check -p hyperliquid-demo --quiet; then
    echo -e "${RED}❌ 錯誤: 編譯失敗${NC}"
    exit 1
fi

echo -e "${GREEN}✅ 編譯檢查通過${NC}"
echo ""

# 設置日誌級別（如果未設置）
if [[ -z "${RUST_LOG:-}" ]]; then
    export RUST_LOG="info,adapter_hyperliquid_data=debug,adapter_hyperliquid_execution=debug"
    echo -e "${BLUE}📝 日誌級別: $RUST_LOG${NC}"
fi

echo ""
echo -e "${BLUE}🎯 啟動演示...${NC}"
echo -e "${YELLOW}參數: $DEMO_ARGS${NC}"
echo ""

# 運行演示
cargo run -p hyperliquid-demo --bin complete_demo -- $DEMO_ARGS

echo ""
echo -e "${GREEN}✨ 演示完成！${NC}"