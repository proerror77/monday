#!/bin/bash
set -e

# HFT Collector 标准部署脚本
# 同时部署现货和永续合约收集器

# ECS配置
INSTANCE_IP="${ECS_IP:-8.221.136.162}"
SSH_KEY_PATH="${SSH_KEY:-/Users/proerror/Documents/monday/rust_hft/hft-collector-key.pem}"

# ClickHouse配置
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="default"
CLICKHOUSE_PASSWORD="s9wECb~NGZPOE"
DATABASE="hft_db"

echo "🚀 HFT Collector 标准部署"
echo "=========================="
echo "目标: ${INSTANCE_IP}"

# 1. 上传镜像到ECS
echo "📤 上传Docker镜像..."
scp -i "${SSH_KEY_PATH}" hft-collector-standard.tar.gz root@${INSTANCE_IP}:~/

# 2. 在ECS上执行部署
ssh -i "${SSH_KEY_PATH}" root@${INSTANCE_IP} << 'DEPLOY_SCRIPT'
set -e

echo "📦 加载Docker镜像..."
docker load < ~/hft-collector-standard.tar.gz

echo "🛑 停止旧版本..."
docker stop $(docker ps -q --filter 'name=hft-') 2>/dev/null || true
docker rm $(docker ps -aq --filter 'name=hft-') 2>/dev/null || true

echo "🔧 创建docker-compose配置..."
cat > docker-compose.yml << 'EOF'
version: '3.8'

services:
  # 币安现货收集器
  binance-spot:
    image: hft-collector:standard
    container_name: hft-binance-spot
    restart: unless-stopped
    environment:
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - RUST_LOG=info
    command: >
      multi
      --exchange binance
      --top-limit 20
      --depth-mode both
      --depth-levels 20
      --flush-ms 2000
      --lob-mode both
      --ch-url https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      --database hft_db
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"

  # 币安永续合约收集器
  binance-futures:
    image: hft-collector:standard
    container_name: hft-binance-futures
    restart: unless-stopped
    environment:
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - RUST_LOG=info
    command: >
      multi
      --exchange binance_futures
      --top-limit 15
      --depth-mode both
      --depth-levels 20
      --flush-ms 2000
      --lob-mode both
      --ch-url https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      --database hft_db
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"

  # Bitget收集器（可选）
  bitget:
    image: hft-collector:standard
    container_name: hft-bitget
    restart: unless-stopped
    environment:
      - CLICKHOUSE_USER=default
      - CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
      - RUST_LOG=info
    command: >
      multi
      --exchange bitget
      --top-limit 10
      --depth-mode limited
      --depth-levels 20
      --flush-ms 3000
      --lob-mode snapshot
      --ch-url https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
      --database hft_db
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"
EOF

echo "🚀 启动所有服务..."
docker-compose up -d

echo "⏳ 等待服务启动..."
sleep 10

echo "📊 服务状态:"
docker-compose ps

echo "📋 实时日志:"
docker-compose logs --tail=5

echo "✅ 部署完成！"
DEPLOY_SCRIPT

echo ""
echo "✅ 标准部署完成！"
echo ""
echo "📊 监控命令："
echo "  ssh -i ${SSH_KEY_PATH} root@${INSTANCE_IP} 'docker-compose ps'"
echo "  ssh -i ${SSH_KEY_PATH} root@${INSTANCE_IP} 'docker-compose logs -f binance-spot'"
echo "  ssh -i ${SSH_KEY_PATH} root@${INSTANCE_IP} 'docker-compose logs -f binance-futures'"
echo ""
echo "🔧 管理命令："
echo "  重启: ssh -i ${SSH_KEY_PATH} root@${INSTANCE_IP} 'docker-compose restart'"
echo "  停止: ssh -i ${SSH_KEY_PATH} root@${INSTANCE_IP} 'docker-compose stop'"
echo "  更新: 重新运行此脚本"