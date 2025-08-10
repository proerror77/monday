#!/bin/bash

# HFT 系統持久化啟動腳本
# ======================

echo "🚀 HFT 系統持久化啟動"
echo "===================="

# 檢查 Python 環境
if ! command -v python3 &> /dev/null; then
    echo "❌ Python3 未找到"
    exit 1
fi

# 檢查必要的依賴
echo "🔍 檢查系統依賴..."

# 檢查 Redis
if ! pgrep redis-server > /dev/null; then
    echo "❌ Redis 未運行，請先啟動 Redis"
    exit 1
fi

# 檢查 ClickHouse
if ! curl -s http://localhost:8123/ > /dev/null; then
    echo "❌ ClickHouse 未運行，請先啟動 ClickHouse"
    exit 1
fi

echo "✅ 基礎設施檢查通過"

# 設置權限
chmod +x agno_cli.py
chmod +x process_manager.py

echo ""
echo "🎯 選擇啟動模式："
echo "1. 持久化進程管理器 (推薦) - 四個獨立進程自動管理"
echo "2. Agno CLI 對話接口 - 手動控制各個工作區"
echo "3. 傳統 Master Workspace - 單一進程模式"
echo ""

read -p "請選擇模式 (1-3): " choice

case $choice in
    1)
        echo "🚀 啟動持久化進程管理器..."
        python3 process_manager.py
        ;;
    2)
        echo "🤖 啟動 Agno CLI 對話接口..."
        python3 agno_cli.py
        ;;
    3)
        echo "🎛️ 啟動傳統 Master Workspace..."
        cd master_workspace
        python3 master_workspace_app.py
        ;;
    *)
        echo "❌ 無效選擇"
        exit 1
        ;;
esac