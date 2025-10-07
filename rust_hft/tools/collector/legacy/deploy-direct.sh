#!/bin/bash
set -e

echo "🚀 直接部署HFT Collector到ECS"
echo "=============================="
echo ""

# ECS配置
ECS_IP="8.221.136.162"
KEY_PATH="$HOME/Documents/monday/rust_hft/hft-admin-ssh-20250926144355.pem"
IMAGE_FILE="hft-collector-standard.tar.gz"

# 步骤1：压缩镜像（如果还没压缩）
if [ ! -f "${IMAGE_FILE}" ]; then
    echo "📦 压缩Docker镜像..."
    docker save hft-collector:standard | gzip > ${IMAGE_FILE}
    echo "✅ 镜像已压缩: $(ls -lh ${IMAGE_FILE} | awk '{print $5}')"
fi

# 步骤2：分片传输到ECS
echo ""
echo "📤 传输镜像到ECS（使用分片传输避免超时）..."
echo "镜像大小: $(ls -lh ${IMAGE_FILE} | awk '{print $5}')"

# 分割文件为5MB的片段
split -b 5m ${IMAGE_FILE} ${IMAGE_FILE}.part.

# 传输所有片段
for part in ${IMAGE_FILE}.part.*; do
    echo "传输: $part"
    scp -i ${KEY_PATH} -o ConnectTimeout=10 -o ServerAliveInterval=10 $part root@${ECS_IP}:/tmp/ &
done

# 等待所有传输完成
wait
echo "✅ 所有片段已传输"

# 步骤3：在ECS上合并和部署
echo ""
echo "🔧 在ECS上合并镜像并部署..."
ssh -i ${KEY_PATH} root@${ECS_IP} << 'DEPLOY'
# 合并片段
cd /tmp
cat hft-collector-standard.tar.gz.part.* > hft-collector-standard.tar.gz
rm -f hft-collector-standard.tar.gz.part.*

# 加载镜像
echo "加载Docker镜像..."
docker load < hft-collector-standard.tar.gz
rm -f hft-collector-standard.tar.gz

# 清理旧容器
echo "清理旧容器..."
docker ps -a | grep hft- | awk '{print $1}' | xargs -r docker rm -f

# 创建docker-compose配置
cat > /root/docker-compose.yml << 'COMPOSE'
version: '3.8'

services:
  # 币安永续合约收集器 - 连接 fstream.binance.com
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
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"

  # 币安现货收集器 - 连接 stream.binance.com
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
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"

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
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"

networks:
  hft-network:
    driver: bridge
COMPOSE

# 启动服务
echo "启动所有收集器..."
docker-compose up -d

# 等待服务启动
sleep 10

# 显示状态
echo ""
echo "📊 服务状态："
docker-compose ps

echo ""
echo "📈 永续合约收集器日志："
docker logs --tail 30 hft-binance-futures 2>&1 | grep -E "(Connected|futures|fstream|BTCUSDT)" || echo "启动中..."

echo ""
echo "📈 现货收集器日志："
docker logs --tail 30 hft-binance-spot 2>&1 | grep -E "(Connected|spot|stream|BTCUSDT)" || echo "启动中..."

echo ""
echo "📈 Bitget收集器日志："
docker logs --tail 30 hft-bitget 2>&1 | grep -E "(Connected|bitget|BTCUSDT)" || echo "启动中..."

DEPLOY

# 清理本地分片文件
echo ""
echo "🧹 清理本地临时文件..."
rm -f ${IMAGE_FILE}.part.*

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 关键信息："
echo "  - 永续合约收集器: 连接到 fstream.binance.com"
echo "  - 现货收集器: 连接到 stream.binance.com"
echo "  - Bitget收集器: 连接到 ws.bitget.com"
echo ""
echo "🔍 验证命令："
echo "  1. 查看服务状态: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker-compose ps'"
echo "  2. 查看永续合约日志: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs -f hft-binance-futures'"
echo "  3. 查看现货日志: ssh -i ${KEY_PATH} root@${ECS_IP} 'docker logs -f hft-binance-spot'"
echo "  4. 验证数据收集: ./verify-collection.sh"