#!/bin/bash
# Feature gates 快速檢查腳本
# 用途：在本地快速驗證常用 feature 組合是否能編譯通過

set -e

# 顏色定義
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "🔍 HFT Feature Gates 快速檢查"
echo ""

# 核心 crate 列表
CORE_CRATES=(
    "hft-core"
    "hft-snapshot"
    "hft-engine"
    "hft-ports"
)

# 要測試的 feature 組合
FEATURES=(
    ""                                    # 默認配置
    "json-simd"                           # SIMD JSON
    "metrics"                             # Prometheus 監控
    "clickhouse"                          # ClickHouse 數據庫
    "redis"                               # Redis 緩存
    "metrics,clickhouse,redis"            # 完整基礎設施
)

# 統計
TOTAL_CHECKS=0
PASSED_CHECKS=0
FAILED_CHECKS=0

# 檢查單個 crate + feature 組合
check_crate_feature() {
    local crate=$1
    local feature=$2

    TOTAL_CHECKS=$((TOTAL_CHECKS + 1))

    echo -n "  ├─ 檢查 $crate"
    if [ -n "$feature" ]; then
        echo -n " (features: $feature)"
    fi
    echo -n "... "

    # 執行檢查，捕獲輸出並檢查是否有錯誤
    local output
    if [ -n "$feature" ]; then
        output=$(cargo check -p "$crate" --features "$feature" 2>&1)
    else
        output=$(cargo check -p "$crate" 2>&1)
    fi

    # 檢查是否編譯成功（查找 "Finished" 而不是 "error"）
    if echo "$output" | grep -q "Finished"; then
        echo -e "${GREEN}✅ 通過${NC}"
        PASSED_CHECKS=$((PASSED_CHECKS + 1))
        return 0
    else
        echo -e "${RED}❌ 失敗${NC}"
        FAILED_CHECKS=$((FAILED_CHECKS + 1))
        # 輸出錯誤信息
        echo "$output" | grep -A 3 "error" || true
        return 1
    fi
}

# 主循環
for feature in "${FEATURES[@]}"; do
    echo ""
    if [ -z "$feature" ]; then
        echo "📦 測試默認配置"
    else
        echo "📦 測試 feature: $feature"
    fi

    for crate in "${CORE_CRATES[@]}"; do
        check_crate_feature "$crate" "$feature" || true
    done
done

# 總結
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "📊 檢查總結"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  總檢查數: $TOTAL_CHECKS"
echo -e "  ${GREEN}通過: $PASSED_CHECKS${NC}"
if [ $FAILED_CHECKS -gt 0 ]; then
    echo -e "  ${RED}失敗: $FAILED_CHECKS${NC}"
    echo ""
    echo -e "${RED}⚠️  存在編譯錯誤，請檢查並修復${NC}"
    exit 1
else
    echo ""
    echo -e "${GREEN}✅ 所有 feature 組合檢查通過！${NC}"
    exit 0
fi
