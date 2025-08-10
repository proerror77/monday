#!/bin/bash
# 🐳 HFT系統純Docker一鍵冷啟動
# 利用現有的 docker-compose.hft.yml 完整配置

set -e

echo "🐳 HFT系統Docker冷啟動開始..."

# 確保環境變量檔案存在
if [ ! -f .env ]; then
    echo "📝 創建環境變量檔案..."
    cat > .env << EOF
OPENAI_API_KEY=your_openai_key_here
EXA_API_KEY=your_exa_key_here  
AGNO_API_KEY=your_agno_key_here
EOF
    echo "⚠️  請編輯 .env 檔案設置您的API密鑰"
fi

# =============================================================================
# 一鍵啟動所有服務
# =============================================================================
echo "🚀 啟動所有HFT服務..."
docker-compose -f docker-compose.hft.yml up -d

# =============================================================================
# 等待服務就緒
# =============================================================================
echo "⏳ 等待服務啟動完成..."

# 等待健康檢查通過
timeout 120 bash -c '
while true; do
    healthy_count=$(docker-compose -f docker-compose.hft.yml ps --format "table {{.Service}}\t{{.Status}}" | grep -c "healthy" || true)
    if [ "$healthy_count" -ge 8 ]; then  # 預期8個健康的服務
        echo "✅ 所有服務健康檢查通過"
        break
    fi
    echo "📊 等待服務就緒... ($healthy_count/8 健康)"
    sleep 5
done
'

# =============================================================================
# 顯示服務狀態
# =============================================================================
echo ""
echo "📊 服務狀態："
docker-compose -f docker-compose.hft.yml ps

echo ""
echo "🎉 HFT系統Docker啟動完成！"
echo ""
echo "📊 訪問地址："
echo "  🎯 Master控制台: http://localhost:8504"
echo "  🧠 ML工作台: http://localhost:8502"  
echo "  ⚠️  Ops監控: http://localhost:8503"
echo "  📊 ClickHouse: http://localhost:8123"
echo "  🔴 Redis: localhost:6379"
echo ""
echo "📋 管理命令："
echo "  查看日誌: docker-compose -f docker-compose.hft.yml logs -f [service_name]"
echo "  停止系統: docker-compose -f docker-compose.hft.yml down"
echo "  重啟服務: docker-compose -f docker-compose.hft.yml restart [service_name]"
echo ""
echo "⚡ 系統就緒，開始使用！"