#!/bin/bash
set -e

echo "🚀 最终部署方案 - 阿里云OSS传输"
echo "================================"
echo ""

# 配置
IMAGE_FILE="hft-collector-standard.tar.gz"
OSS_BUCKET="hft-collector"  # 您需要在阿里云创建这个bucket
OSS_REGION="oss-cn-hongkong"
ECS_IP="8.221.136.162"

# 检查镜像文件
if [ ! -f "${IMAGE_FILE}" ]; then
    echo "❌ 镜像文件不存在"
    exit 1
fi

echo "📦 镜像大小: $(ls -lh ${IMAGE_FILE} | awk '{print $5}')"
echo ""

# 步骤1：上传到阿里云OSS
echo "📤 上传到阿里云OSS..."
echo "请执行以下命令："
echo ""
echo "1. 安装ossutil: brew install ossutil"
echo "2. 配置: ossutil config"
echo "3. 上传: ossutil cp ${IMAGE_FILE} oss://${OSS_BUCKET}/"
echo "4. 获取URL: ossutil sign oss://${OSS_BUCKET}/${IMAGE_FILE}"
echo ""
echo "或使用阿里云控制台手动上传到OSS"
echo ""

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "📋 然后在ECS上执行："
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "ssh root@${ECS_IP}"
echo ""
cat << 'DEPLOY'
# 下载镜像（替换OSS_URL为实际的URL）
cd /tmp
wget -O hft-collector.tar.gz "OSS_URL"

# 加载镜像
docker load < hft-collector.tar.gz

# 清理
docker ps -a | grep hft- | awk '{print $1}' | xargs -r docker rm -f

# 部署
cat > /root/docker-compose.yml << 'COMPOSE'
version: '3.8'
services:
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
      multi --exchange binance_futures
      --symbols BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,ADAUSDT,AVAXUSDT
      --streams orderbook,trades,l1,ticker --depth 5 --rate-limit 50

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
      multi --exchange binance
      --symbols BTCUSDT,ETHUSDT,BNBUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,ADAUSDT,AVAXUSDT
      --streams orderbook,trades,l1,ticker --depth 5 --rate-limit 50
  
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
      multi --exchange bybit --top-limit 20 --depth 5

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
      multi --exchange hyperliquid --top-limit 20 --depth 5

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
      multi --exchange asterdex --top-limit 20 --depth 20
COMPOSE

cd /root
docker-compose up -d
docker-compose ps

# 验证
sleep 5
echo "永续合约状态："
docker logs --tail 50 hft-binance-futures | grep -E "(Connected|futures|fstream|BTCUSDT)"
echo ""
echo "现货状态："
docker logs --tail 50 hft-binance-spot | grep -E "(Connected|spot|stream|BTCUSDT)"
DEPLOY
