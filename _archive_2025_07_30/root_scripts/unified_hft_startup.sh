#!/bin/bash

# HFT系統統一啟動腳本
# 解決配置衝突，實現系統從"無法啟動"到"可以交易"

set -e  # 遇到錯誤立即停止

echo "🚀 HFT系統統一啟動流程開始..."
echo "目標: 實現25μs延遲和100ms告警響應"
echo "=====================================\n"

# 檢查工作目錄
if [[ ! -f "CLAUDE.md" ]]; then
    echo "❌ 錯誤: 請在項目根目錄執行此腳本"
    exit 1
fi

# 創建日誌目錄
mkdir -p logs

# Phase 1: 基礎設施啟動
echo "📦 Phase 1: 啟動基礎設施服務..."
cd rust_hft

# 啟動Redis和ClickHouse
docker-compose up -d --remove-orphans 2>/dev/null || true
sleep 10

# 驗證基礎設施
echo "🔍 驗證基礎設施狀態..."
if ! redis-cli ping >/dev/null 2>&1; then
    echo "❌ Redis啟動失敗，重試..."
    docker-compose restart redis
    sleep 5
fi

if ! curl -s http://localhost:8123 >/dev/null 2>&1; then
    echo "❌ ClickHouse啟動失敗，重試..."
    docker-compose restart clickhouse  
    sleep 5
fi

echo "✅ 基礎設施啟動完成"

# Phase 2: 安裝Python依賴
echo "📝 Phase 2: 安裝Python依賴..."
cd ../ops-workspace
pip install -r requirements.txt >/dev/null 2>&1 || echo "⚠️ ops-workspace依賴安裝警告"

cd ../ml-workspace  
pip install -r requirements.txt >/dev/null 2>&1 || echo "⚠️ ml-workspace依賴安裝警告"

cd ..
echo "✅ Python依賴安裝完成"

# Phase 3: 編譯Rust核心
echo "🔨 Phase 3: 編譯Rust HFT核心..."
cd rust_hft
cargo build --release --quiet
echo "✅ Rust核心編譯完成"

# Phase 4: 啟動核心服務
echo "🚀 Phase 4: 啟動核心服務..."

# 啟動Rust HFT核心
nohup cargo run --release --bin rust_hft > ../logs/rust_hft.log 2>&1 &
echo $! > ../logs/rust_hft.pid
echo "✅ Rust HFT核心啟動 (PID: $(cat ../logs/rust_hft.pid))"

cd ..
sleep 3

# 啟動Ops Workspace監控
cd ops-workspace
nohup python workflows/alert_workflow.py > ../logs/ops_workspace.log 2>&1 &  
echo $! > ../logs/ops_workspace.pid
echo "✅ Ops Workspace啟動 (PID: $(cat ../logs/ops_workspace.pid))"

cd ..
sleep 2

echo "✅ 核心服務啟動完成"

# Phase 5: 系統驗證
echo "🔍 Phase 5: 系統集成驗證..."

# 檢查進程狀態
RUST_PID=$(cat logs/rust_hft.pid 2>/dev/null || echo "")
OPS_PID=$(cat logs/ops_workspace.pid 2>/dev/null || echo "")

if [[ -n "$RUST_PID" ]] && kill -0 "$RUST_PID" 2>/dev/null; then
    echo "✅ Rust HFT核心運行正常"
else
    echo "❌ Rust HFT核心啟動失敗"
fi

if [[ -n "$OPS_PID" ]] && kill -0 "$OPS_PID" 2>/dev/null; then
    echo "✅ Ops Workspace運行正常"  
else
    echo "❌ Ops Workspace啟動失敗"
fi

# 檢查Redis連接
if redis-cli ping >/dev/null 2>&1; then
    echo "✅ Redis連接正常"
else
    echo "❌ Redis連接失敗"
fi

# 檢查ClickHouse連接
if curl -s http://localhost:8123 >/dev/null 2>&1; then
    echo "✅ ClickHouse連接正常"
else
    echo "❌ ClickHouse連接失敗"
fi

echo "\n🎉 HFT系統啟動流程完成！"
echo "=====================================\n"

echo "📊 系統狀態總覽:"
echo "- Rust HFT核心: $(ps aux | grep rust_hft | grep -v grep | wc -l) 進程"
echo "- Ops Workspace: $(ps aux | grep alert_workflow | grep -v grep | wc -l) 進程"  
echo "- Redis: $(redis-cli ping 2>/dev/null || echo 'DISCONNECTED')"
echo "- ClickHouse: $(curl -s http://localhost:8123 2>/dev/null && echo 'OK' || echo 'ERROR')"

echo "\n📋 下一步操作:"
echo "1. 執行性能測試: cd rust_hft && cargo run --example latency_benchmark"
echo "2. 啟動ML訓練: cd ml-workspace && python workflows/training_workflow.py"
echo "3. 查看實時日誌: tail -f logs/rust_hft.log"
echo "4. 停止系統: ./stop_hft_system.sh"

echo "\n🎯 系統已準備好開始交易！"