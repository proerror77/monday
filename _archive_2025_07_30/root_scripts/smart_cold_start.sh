#!/bin/bash
# 🚀 HFT系統智能冷啟動腳本
# 結合了一鍵啟動的便利性和分執行緒的穩定性

set -e

echo "🚀 HFT系統智能冷啟動開始..."

# =============================================================================
# 階段一：基礎設施 (同步啟動，確保穩定)
# =============================================================================
echo "📊 啟動基礎設施..."
docker-compose -f docker-compose.hft.yml up -d hft-redis hft-clickhouse

# 等待基礎設施就緒
echo "⏳ 等待基礎設施就緒..."
timeout 30 bash -c 'until docker-compose -f docker-compose.hft.yml ps | grep -q "healthy"; do sleep 1; done'

# =============================================================================
# 階段二：Rust核心 (後台啟動)
# =============================================================================
echo "🦀 啟動Rust HFT核心..."
cd rust_hft
cargo run --release --bin rust_hft -- --config config/quick_test.yaml > ../logs/rust_hft.log 2>&1 &
RUST_PID=$!
echo $RUST_PID > ../logs/rust_hft.pid
cd ..

# 給Rust核心3秒初始化時間
sleep 3

# =============================================================================
# 階段三：Python Workspaces (並行啟動)
# =============================================================================
echo "🐍 並行啟動Python Workspaces..."

# Master Workspace (端口 8504)
cd hft-master-workspace
streamlit run ui/Home.py --server.port 8504 --server.headless true > ../logs/master_workspace.log 2>&1 &
MASTER_PID=$!
echo $MASTER_PID > ../logs/master_workspace.pid
cd ..

# ML Workspace (端口 8502) 
cd hft-ml-workspace
streamlit run ui/Home.py --server.port 8502 --server.headless true > ../logs/ml_workspace.log 2>&1 &
ML_PID=$!  
echo $ML_PID > ../logs/ml_workspace.pid
cd ..

# Ops Workspace (端口 8503)
cd hft-ops-workspace  
streamlit run ui/Home.py --server.port 8503 --server.headless true > ../logs/ops_workspace.log 2>&1 &
OPS_PID=$!
echo $OPS_PID > ../logs/ops_workspace.pid
cd ..

# =============================================================================
# 階段四：健康檢查
# =============================================================================
echo "🔍 執行健康檢查..."
sleep 10

# 檢查服務狀態
check_service() {
    local name=$1
    local url=$2
    local max_attempts=30
    local attempt=1
    
    while [ $attempt -le $max_attempts ]; do
        if curl -sf "$url" >/dev/null 2>&1; then
            echo "  ✅ $name: 運行正常"
            return 0
        fi
        sleep 2
        attempt=$((attempt + 1))
    done
    echo "  ❌ $name: 啟動失敗"
    return 1
}

echo "📊 服務狀態檢查:"
check_service "Redis" "redis://localhost:6379"
check_service "ClickHouse" "http://localhost:8123/ping"  
check_service "Master UI" "http://localhost:8504/_stcore/health"
check_service "ML UI" "http://localhost:8502/_stcore/health"
check_service "Ops UI" "http://localhost:8503/_stcore/health"

# 檢查Rust進程
if kill -0 $RUST_PID 2>/dev/null; then
    echo "  ✅ Rust HFT核心: 運行正常 (PID: $RUST_PID)"
else
    echo "  ❌ Rust HFT核心: 啟動失敗"
fi

# =============================================================================
# 完成
# =============================================================================
echo ""
echo "🎉 HFT系統啟動完成！"
echo ""
echo "📊 訪問地址:"
echo "  🎯 Master控制台: http://localhost:8504"  
echo "  🧠 ML工作台: http://localhost:8502"
echo "  ⚠️  Ops監控: http://localhost:8503"
echo ""
echo "📋 進程管理:"
echo "  停止系統: ./stop_hft_system.sh"
echo "  查看日誌: tail -f logs/*.log"
echo "  系統狀態: ./verify_system.sh"
echo ""
echo "⚡ 系統就緒，開始交易！"