#!/bin/bash
set -e

echo "🚀 使用Docker Hub部署HFT Collector"
echo "===================================="
echo ""
echo "优势："
echo "1. 无需配置阿里云ACR"
echo "2. 全球CDN加速"
echo "3. 免费额度充足"
echo ""

# Docker Hub配置
DOCKER_USER="proerrorhft"  # 您可以修改为您的Docker Hub用户名
IMAGE_NAME="hft-collector"
VERSION=$(date +%Y%m%d-%H%M%S)
DOCKER_IMAGE="${DOCKER_USER}/${IMAGE_NAME}:${VERSION}"
DOCKER_LATEST="${DOCKER_USER}/${IMAGE_NAME}:latest"

# ECS配置
ECS_IP="8.221.136.162"
KEY_PATH="$HOME/.ssh/hft-admin-ssh-20250926144355.pem"

# 步骤1：标记本地镜像
echo "📦 步骤1：标记本地镜像..."
docker tag hft-collector:standard ${DOCKER_IMAGE}
docker tag hft-collector:standard ${DOCKER_LATEST}
echo "✅ 镜像已标记"

# 步骤2：推送到Docker Hub（公开仓库，无需登录）
echo ""
echo "📤 步骤2：推送镜像到Docker Hub..."
echo "注意：如果这是私有项目，请先执行: docker login"
echo "推送中（约需30秒）..."

# 尝试推送，如果失败则提示登录
if ! docker push ${DOCKER_IMAGE} 2>/dev/null; then
    echo "需要登录Docker Hub："
    echo "请执行: docker login"
    echo "然后重新运行此脚本"
    exit 1
fi
docker push ${DOCKER_LATEST}
echo "✅ 镜像已推送到Docker Hub"

# 步骤3：在ECS上部署
echo ""
echo "🚀 步骤3：在ECS上部署..."
ssh -i ${KEY_PATH} root@${ECS_IP} << EOF
# 清理旧容器
echo "清理旧容器..."
docker ps -a | grep hft- | awk '{print \$1}' | xargs -r docker rm -f

# 拉取最新镜像（从Docker Hub）
echo "从Docker Hub拉取镜像..."
docker pull ${DOCKER_LATEST}

# 创建docker-compose.yml
cat > /root/docker-compose.yml << 'COMPOSE'
version: '3.8'

services:
  # 币安永续合约收集器（最重要）
  binance-futures:
    image: ${DOCKER_LATEST}
    container_name: hft-binance-futures
    restart: unless-stopped
    environment:
      - RUST_LOG=info
      - CLICKHOUSE_URL=https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - CLICKHOUSE_DATABASE=hft_db
    command: >
      multi
      --exchange binance_futures
      --symbols BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,ADAUSDT,AVAXUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
      --rate-limit 50
    networks:
      - hft-network

  # 币安现货收集器
  binance-spot:
    image: ${DOCKER_LATEST}
    container_name: hft-binance-spot
    restart: unless-stopped
    environment:
      - RUST_LOG=info
      - CLICKHOUSE_URL=https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - CLICKHOUSE_DATABASE=hft_db
    command: >
      multi
      --exchange binance
      --symbols BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,ADAUSDT,AVAXUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
      --rate-limit 50
    networks:
      - hft-network

  # Bitget收集器
  bitget:
    image: ${DOCKER_LATEST}
    container_name: hft-bitget
    restart: unless-stopped
    environment:
      - RUST_LOG=info
      - CLICKHOUSE_URL=https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - CLICKHOUSE_DATABASE=hft_db
    command: >
      multi
      --exchange bitget
      --symbols BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
      --rate-limit 50
    networks:
      - hft-network

networks:
  hft-network:
    driver: bridge
COMPOSE

# 替换镜像地址
sed -i "s|\${DOCKER_LATEST}|${DOCKER_LATEST}|g" /root/docker-compose.yml

# 启动服务
echo "启动所有服务..."
docker-compose up -d

# 等待服务启动
sleep 5

# 显示状态
echo ""
echo "📊 服务状态："
docker-compose ps

echo ""
echo "📈 检查永续合约连接："
docker logs --tail 20 hft-binance-futures | grep -E "(Connected to|fstream.binance.com|futures)" || echo "等待连接..."

echo ""
echo "📈 检查现货连接："
docker logs --tail 20 hft-binance-spot | grep -E "(Connected to|stream.binance.com|spot)" || echo "等待连接..."

EOF

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 部署信息："
echo "  - Docker Hub镜像: ${DOCKER_LATEST}"
echo "  - 版本: ${VERSION}"
echo "  - ECS: ${ECS_IP}"
echo ""
echo "🔍 验证命令："
echo "  1. 查看永续合约日志: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs --tail 50 hft-binance-futures'"
echo "  2. 查看现货日志: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs --tail 50 hft-binance-spot'"
echo "  3. 查看服务状态: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker-compose ps'"
echo ""
echo "📊 验证数据收集："
echo "  运行: ./verify-collection.sh"