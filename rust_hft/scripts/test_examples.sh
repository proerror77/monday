#!/bin/bash

# Rust HFT Examples 測試腳本
# 根據三層架構驗證所有examples的編譯和基本功能

echo "🚀 Rust HFT Examples 測試開始"
echo "==============================="

# 設置顏色
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# 計數器
TOTAL=0
PASSED=0
FAILED=0

# 測試函數
test_example() {
    local example_path=$1
    local example_name=$2
    local test_args=$3
    
    TOTAL=$((TOTAL + 1))
    
    echo -e "${YELLOW}測試 $example_name${NC}"
    echo "   路徑: $example_path"
    
    # 檢查文件是否存在
    if [ ! -f "$example_path" ]; then
        echo -e "   ${RED}❌ 檔案不存在${NC}"
        FAILED=$((FAILED + 1))
        return 1
    fi
    
    # 編譯檢查
    if cargo check --example $(basename "$example_path" .rs) 2>/dev/null; then
        echo -e "   ${GREEN}✅ 編譯成功${NC}"
        PASSED=$((PASSED + 1))
    else
        echo -e "   ${RED}❌ 編譯失敗${NC}"
        FAILED=$((FAILED + 1))
        return 1
    fi
}

echo "📁 快速入門示例"
echo "──────────────"
test_example "examples/00_quickstart.rs" "快速入門"
test_example "examples/00_quickstart_lockfree.rs" "無鎖快速入門"
test_example "examples/00_zero_copy_websocket_test.rs" "零拷貝WebSocket測試"

echo ""
echo "📁 數據收集層"
echo "─────────────"
test_example "examples/01_data_collection/ws_performance_test.rs" "WebSocket 性能測試"
test_example "examples/01_data_collection/clickhouse_writer.rs" "ClickHouse 寫入測試"
test_example "examples/01_data_collection/multi_symbol_collector.rs" "多標的收集器"
test_example "examples/01_data_collection/data_quality_monitor.rs" "數據品質監控"

echo ""
echo "📁 統一系統"
echo "───────────"
test_example "examples/02_simplified_unified_system.rs" "簡化統一系統"

echo ""
echo "📁 模型推理層"
echo "─────────────"
test_example "examples/03_model_inference/inference_benchmark.rs" "推理性能測試"
test_example "examples/03_model_inference/strategy_executor.rs" "策略執行器"

echo ""
echo "📁 系統整合層"
echo "─────────────"
test_example "examples/05_system_integration/end_to_end_test.rs" "端到端測試"
test_example "examples/05_system_integration/latency_benchmark.rs" "延遲基準測試"
test_example "examples/05_system_integration/system_monitoring.rs" "系統監控"
test_example "examples/05_system_integration/stress_test.rs" "壓力測試"

echo ""
echo "📁 其他測試"
echo "───────────"
test_example "examples/test_real_ws_connection.rs" "真實WebSocket連接測試"

echo ""
echo "==============================="
echo "🎉 測試完成"
echo "   總計: $TOTAL"
echo -e "   ${GREEN}通過: $PASSED${NC}"
echo -e "   ${RED}失敗: $FAILED${NC}"

if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}🏆 所有測試通過！${NC}"
    exit 0
else
    echo -e "${RED}❌ 有 $FAILED 個測試失敗${NC}"
    exit 1
fi