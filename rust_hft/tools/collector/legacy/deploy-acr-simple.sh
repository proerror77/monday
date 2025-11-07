#!/bin/bash
set -e

echo "🚀 使用阿里云ACR部署HFT Collector"
echo "==================================="
echo ""

# 阿里云ACR配置（香港区域）
ACR_REGISTRY="registry.cn-hongkong.aliyuncs.com"
ACR_NAMESPACE="hft-proerror"  # 您需要在阿里云控制台创建这个命名空间
IMAGE_NAME="hft-collector"
VERSION=$(date +%Y%m%d-%H%M%S)
ACR_IMAGE="${ACR_REGISTRY}/${ACR_NAMESPACE}/${IMAGE_NAME}:${VERSION}"
ACR_LATEST="${ACR_REGISTRY}/${ACR_NAMESPACE}/${IMAGE_NAME}:latest"

# ECS配置
ECS_IP="8.221.136.162"
KEY_PATH="$HOME/.ssh/hft-admin-ssh-20250926144355.pem"

# 步骤1：标记本地镜像
echo "📦 步骤1：标记本地镜像..."
docker tag hft-collector:standard ${ACR_IMAGE}
docker tag hft-collector:standard ${ACR_LATEST}
echo "✅ 镜像已标记"

# 步骤2：登录ACR（需要您的阿里云账号）
echo ""
echo "📝 步骤2：登录阿里云ACR..."
echo "请在阿里云控制台获取登录命令："
echo "1. 访问: https://cr.console.aliyun.com/"
echo "2. 选择地域: 香港"
echo "3. 点击: 访问凭证"
echo "4. 复制登录命令"
echo ""
echo "登录命令格式类似："
echo "docker login --username=您的阿里云账号 ${ACR_REGISTRY}"
echo ""
read -p "请输入您的阿里云账号（例如：123456@qq.com）: " ACR_USERNAME
docker login --username=${ACR_USERNAME} ${ACR_REGISTRY}

# 步骤3：推送到ACR
echo ""
echo "📤 步骤3：推送镜像到ACR..."
docker push ${ACR_IMAGE}
docker push ${ACR_LATEST}
echo "✅ 镜像已推送到ACR"

# 步骤4：在ECS上部署
echo ""
echo "🚀 步骤4：在ECS上部署..."
ssh -i ${KEY_PATH} root@${ECS_IP} << EOF
# 登录ACR（ECS上）
docker login --username=${ACR_USERNAME} ${ACR_REGISTRY}

# 清理旧容器
echo "清理旧容器..."
docker ps -a | grep hft- | awk '{print \$1}' | xargs -r docker rm -f

# 拉取最新镜像
echo "拉取最新镜像..."
docker pull ${ACR_LATEST}

# 创建docker-compose.yml
cat > /root/docker-compose.yml << 'COMPOSE'
version: '3.8'

services:
  # 币安永续合约收集器（优先级最高）
  binance-futures:
    image: ${ACR_LATEST}
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
    volumes:
      - /var/log/hft:/var/log/hft

  # 币安现货收集器
  binance-spot:
    image: ${ACR_LATEST}
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
    volumes:
      - /var/log/hft:/var/log/hft

  # Bitget收集器
  bitget:
    image: ${ACR_LATEST}
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
    volumes:
      - /var/log/hft:/var/log/hft

networks:
  hft-network:
    driver: bridge

volumes:
  hft-logs:
    driver: local
COMPOSE

# 替换镜像地址
sed -i "s|\${ACR_LATEST}|${ACR_LATEST}|g" /root/docker-compose.yml

# 启动服务
echo "启动所有服务..."
docker-compose up -d

# 显示状态
echo ""
echo "📊 服务状态："
docker-compose ps

echo ""
echo "📈 容器日志（前10行）："
sleep 5
docker logs --tail 10 hft-binance-futures
docker logs --tail 10 hft-binance-spot
docker logs --tail 10 hft-bitget

EOF

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 关键信息："
echo "  - ACR镜像: ${ACR_LATEST}"
echo "  - 版本号: ${VERSION}"
echo "  - ECS IP: ${ECS_IP}"
echo ""
echo "🔍 实用命令："
echo "  查看永续合约日志: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs -f hft-binance-futures'"
echo "  查看现货日志: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs -f hft-binance-spot'"
echo "  查看服务状态: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker-compose ps'"
echo "  重启服务: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker-compose restart'"
echo ""
echo "下一步: 运行 ./verify-collection.sh 验证数据收集"