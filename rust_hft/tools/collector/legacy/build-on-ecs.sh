#!/bin/bash
set -e

# ECS 服务器上直接构建的脚本

ECS_IP="8.221.136.162"
KEY_PATH="$HOME/.ssh/hft-admin-ssh"

echo "🚀 在ECS上直接构建HFT Collector"
echo "================================="
echo ""

# 1. 创建构建目录
echo "📁 创建构建目录..."
ssh -i ${KEY_PATH} root@${ECS_IP} << 'EOF'
mkdir -p /root/hft-build
cd /root/hft-build
EOF

# 2. 复制必要文件
echo "📤 上传源代码..."
scp -i ${KEY_PATH} Cargo.toml root@${ECS_IP}:/root/hft-build/
scp -i ${KEY_PATH} -r src root@${ECS_IP}:/root/hft-build/

# 3. 复制Dockerfile
echo "📋 上传Dockerfile..."
scp -i ${KEY_PATH} Dockerfile.standard root@${ECS_IP}:/root/hft-build/Dockerfile

# 4. 在ECS上构建
echo "🔨 在ECS上构建镜像（这需要一些时间）..."
ssh -i ${KEY_PATH} root@${ECS_IP} << 'EOF'
cd /root/hft-build

# 构建Docker镜像
docker build -t hft-collector:standard -f Dockerfile --no-cache .

# 清理旧容器
docker ps -a | grep hft- | awk '{print $1}' | xargs -r docker rm -f

echo "✅ 镜像构建完成"
docker images | grep hft-collector
EOF

# 5. 创建docker-compose.yml
echo "📝 创建docker-compose配置..."
ssh -i ${KEY_PATH} root@${ECS_IP} << 'COMPOSE'
cat > /root/docker-compose.yml << 'EOF'
version: '3.8'

services:
  # 币安现货收集器 - 连接到 stream.binance.com
  binance-spot:
    image: hft-collector:standard
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
      --symbols BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,ADAUSDT,AVAXUSDT,SHIBUSDT,DOTUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
      --rate-limit 50
    networks:
      - hft-network
    volumes:
      - /var/log/hft:/var/log/hft

  # 币安永续合约收集器 - 连接到 fstream.binance.com
  binance-futures:
    image: hft-collector:standard
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
      --symbols BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,ADAUSDT,AVAXUSDT,SHIBUSDT,DOTUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
      --rate-limit 50
    networks:
      - hft-network
    volumes:
      - /var/log/hft:/var/log/hft

  # Bitget收集器
  bitget:
    image: hft-collector:standard
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
      --symbols BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,ADAUSDT,AVAXUSDT,SHIBUSDT,DOTUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
      --rate-limit 50
    networks:
      - hft-network
    volumes:
      - /var/log/hft:/var/log/hft

  # Bybit收集器
  bybit:
    image: hft-collector:standard
    container_name: hft-bybit
    restart: unless-stopped
    environment:
      - RUST_LOG=info
      - CLICKHOUSE_URL=https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - CLICKHOUSE_DATABASE=hft_db
    command: >
      multi
      --exchange bybit
      --top-limit 20
      --depth 5
    networks:
      - hft-network
    volumes:
      - /var/log/hft:/var/log/hft

  # Hyperliquid收集器
  hyperliquid:
    image: hft-collector:standard
    container_name: hft-hyperliquid
    restart: unless-stopped
    environment:
      - RUST_LOG=info
      - CLICKHOUSE_URL=https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - CLICKHOUSE_DATABASE=hft_db
    command: >
      multi
      --exchange hyperliquid
      --top-limit 20
      --depth 5
    networks:
      - hft-network
    volumes:
      - /var/log/hft:/var/log/hft

  # AsterDEX 收集器
  asterdex:
    image: hft-collector:standard
    container_name: hft-asterdex
    restart: unless-stopped
    environment:
      - RUST_LOG=info
      - CLICKHOUSE_URL=https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - CLICKHOUSE_DATABASE=hft_db
    command: >
      multi
      --exchange asterdex
      --top-limit 20
      --depth 20
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
EOF

echo "✅ docker-compose.yml 已创建"
COMPOSE

# 6. 启动服务
echo "🚀 启动所有收集器..."
ssh -i ${KEY_PATH} root@${ECS_IP} << 'EOF'
cd /root
docker-compose up -d

echo ""
echo "✅ 收集器已启动"
echo ""
echo "📊 服务状态："
docker-compose ps

echo ""
echo "📈 容器资源使用："
docker stats --no-stream
EOF

echo ""
echo "✅ 部署完成！"
echo ""
echo "🔍 查看日志："
echo "  - 现货: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs -f hft-binance-spot'"
echo "  - 期货: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs -f hft-binance-futures'"
echo "  - Bitget: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs -f hft-bitget'"
echo ""
echo "📊 验证数据："
echo "  运行: ./verify-collection.sh"
