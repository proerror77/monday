#!/bin/bash
# 🔄 智能重啟HFT系統
# 檢查現有服務狀態，只重啟需要的服務

set -e

echo "🔄 HFT系統智能重啟..."

# =============================================================================
# 檢查當前狀態
# =============================================================================
echo "📊 檢查當前服務狀態..."

# 檢查服務健康狀態
check_service_health() {
    local service_name=$1
    local container_name=$2
    
    if docker ps --filter "name=$container_name" --filter "status=running" | grep -q "$container_name"; then
        health_status=$(docker inspect --format='{{.State.Health.Status}}' "$container_name" 2>/dev/null || echo "no-health-check")
        if [ "$health_status" = "healthy" ] || [ "$health_status" = "no-health-check" ]; then
            echo "  ✅ $service_name: 健康運行中"
            return 0
        else
            echo "  ⚠️  $service_name: 不健康 ($health_status)"
            return 1
        fi
    else
        echo "  ❌ $service_name: 未運行"
        return 1
    fi
}

# 檢查所有服務
services_to_restart=""

if ! check_service_health "Redis" "hft-redis"; then
    services_to_restart="$services_to_restart hft-redis"
fi

if ! check_service_health "ClickHouse" "hft-clickhouse"; then
    services_to_restart="$services_to_restart hft-clickhouse"
fi

if ! check_service_health "Master DB" "hft-master-db"; then
    services_to_restart="$services_to_restart hft-master-db"
fi

if ! check_service_health "ML DB" "hft-ml-db"; then
    services_to_restart="$services_to_restart hft-ml-db"
fi

if ! check_service_health "Ops DB" "hft-ops-db"; then
    services_to_restart="$services_to_restart hft-ops-db"
fi

if ! check_service_health "Master UI" "hft-master-ui"; then
    services_to_restart="$services_to_restart hft-master-ui"
fi

if ! check_service_health "ML UI" "hft-ml-ui"; then
    services_to_restart="$services_to_restart hft-ml-ui"
fi

if ! check_service_health "Ops UI" "hft-ops-ui"; then
    services_to_restart="$services_to_restart hft-ops-ui"
fi

if ! check_service_health "Master API" "hft-master-api"; then
    services_to_restart="$services_to_restart hft-master-api"
fi

if ! check_service_health "ML API" "hft-ml-api"; then
    services_to_restart="$services_to_restart hft-ml-api"
fi

if ! check_service_health "Ops API" "hft-ops-api"; then
    services_to_restart="$services_to_restart hft-ops-api"
fi

# =============================================================================
# 決定操作
# =============================================================================
if [ -z "$services_to_restart" ]; then
    echo ""
    echo "🎉 所有服務都在健康運行中！"
    echo ""
    echo "📊 訪問地址："
    echo "  🎯 Master控制台: http://localhost:8504"
    echo "  🧠 ML工作台: http://localhost:8502"  
    echo "  ⚠️  Ops監控: http://localhost:8503"
    echo ""
    echo "如需強制重啟，請使用: docker-compose -f docker-compose.hft.yml restart"
    exit 0
fi

echo ""
echo "🔧 需要重啟的服務: $services_to_restart"
echo ""

# 詢問用戶選擇
read -p "選擇操作: [1] 只重啟問題服務, [2] 重啟所有服務, [3] 取消 (1/2/3): " choice

case $choice in
    1)
        echo "🔄 重啟問題服務..."
        docker-compose -f docker-compose.hft.yml restart $services_to_restart
        ;;
    2)
        echo "🔄 重啟所有服務..."
        docker-compose -f docker-compose.hft.yml down
        sleep 2
        docker-compose -f docker-compose.hft.yml up -d
        ;;
    3)
        echo "❌ 操作已取消"
        exit 0
        ;;
    *)
        echo "❌ 無效選擇，操作已取消"
        exit 1
        ;;
esac

# =============================================================================
# 等待服務就緒
# =============================================================================
echo "⏳ 等待服務就緒..."
sleep 10

# 顯示最終狀態
echo ""
echo "📊 最終服務狀態："
docker-compose -f docker-compose.hft.yml ps

echo ""
echo "🎉 系統重啟完成！"
echo ""
echo "📊 訪問地址："
echo "  🎯 Master控制台: http://localhost:8504"
echo "  🧠 ML工作台: http://localhost:8502"  
echo "  ⚠️  Ops監控: http://localhost:8503"