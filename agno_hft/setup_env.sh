#!/bin/bash

# HFT Agno Agent 環境設置腳本

echo "🔧 設置HFT Agno Agent環境..."

# 檢查是否在正確的Python版本
PYTHON_VERSION=$(python --version 2>&1 | cut -d' ' -f2 | cut -d'.' -f1,2)
echo "當前Python版本: $PYTHON_VERSION"

# 安裝必要的依賴
echo "📦 安裝必要依賴..."
pip install ollama agno numpy asyncio typing-extensions

# 檢查ollama服務
echo "🔍 檢查Ollama服務..."
if curl -s http://localhost:11434/api/version > /dev/null; then
    echo "✅ Ollama服務運行正常"
else
    echo "❌ Ollama服務未運行，請先啟動ollama"
    echo "提示: 運行 'ollama serve' 或重啟ollama服務"
fi

# 檢查qwen3模型
echo "🔍 檢查Qwen3模型..."
if ollama list | grep -q "qwen3"; then
    echo "✅ Qwen3模型已安裝"
else
    echo "⚠️ Qwen3模型未安裝，正在下載..."
    ollama pull qwen3:8b
fi

echo "🎉 環境設置完成！"
echo ""
echo "🚀 啟動HFT Agent系統:"
echo "   python start_hft.py"