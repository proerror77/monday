#!/bin/bash
set -e

# 阿里云容器镜像服务配置
# 需要先登录：docker login --username=your_username registry.cn-hongkong.aliyuncs.com
ACR_REGISTRY="registry.cn-hongkong.aliyuncs.com"
ACR_NAMESPACE="hft-system"  # 需要在阿里云控制台创建
ACR_REPO="hft-collector"
VERSION=$(date +%Y%m%d-%H%M%S)
IMAGE_TAG="${ACR_REGISTRY}/${ACR_NAMESPACE}/${ACR_REPO}:${VERSION}"
IMAGE_LATEST="${ACR_REGISTRY}/${ACR_NAMESPACE}/${ACR_REPO}:latest"

echo "🚀 HFT Collector 智能部署方案"
echo "=============================="
echo "使用阿里云容器镜像服务(ACR)"
echo ""

# 1. 构建优化镜像
echo "🔨 构建优化的Docker镜像..."
docker build -t ${IMAGE_TAG} -f Dockerfile.optimized .
docker tag ${IMAGE_TAG} ${IMAGE_LATEST}

# 2. 推送到ACR
echo "📤 推送到阿里云容器镜像服务..."
echo "请确保已执行: docker login --username=<your-username> ${ACR_REGISTRY}"
docker push ${IMAGE_TAG}
docker push ${IMAGE_LATEST}

# 3. 在ECS上拉取并部署
echo "🚀 在ECS上部署..."
ECS_IP="8.221.136.162"
KEY_PATH="$HOME/.ssh/hft-admin-ssh"

ssh -i ${KEY_PATH} root@${ECS_IP} << EOF
# 登录ACR（如果需要）
# docker login --username=<username> --password=<password> ${ACR_REGISTRY}

# 拉取最新镜像
docker pull ${IMAGE_LATEST}

# 创建docker-compose配置
cat > /root/docker-compose.yml << 'COMPOSE'
version: '3.8'

services:
  # 币安现货收集器
  binance-spot:
    image: ${IMAGE_LATEST}
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
      --symbols BTCUSDT,ETHUSDT,BNBUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
    networks:
      - hft-network

  # 币安永续合约收集器
  binance-futures:
    image: ${IMAGE_LATEST}
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
      --symbols BTCUSDT,ETHUSDT,BNBUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
    networks:
      - hft-network

networks:
  hft-network:
    driver: bridge
COMPOSE

# 重启服务
docker-compose down
docker-compose up -d

echo "✅ 部署完成"
docker-compose ps
EOF

echo ""
echo "✅ 智能部署完成！"
echo ""
echo "优势："
echo "  1. 使用ACR避免上传超时"
echo "  2. cargo-chef缓存依赖，加速后续构建"
echo "  3. 多阶段构建，最小化镜像体积"
echo "  4. 版本管理，支持回滚"