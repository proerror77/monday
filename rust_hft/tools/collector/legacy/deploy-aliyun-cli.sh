#!/bin/bash
set -e

echo "🚀 使用阿里云CLI部署HFT Collector"
echo "=================================="
echo ""

# ECS实例ID
INSTANCE_ID="i-j6c90tfflso26qj38vun"
REGION="cn-hongkong"

# 检查阿里云CLI
if ! command -v aliyun &> /dev/null; then
    echo "❌ 未安装阿里云CLI"
    echo "请先安装: brew install aliyun-cli"
    echo "然后配置: aliyun configure"
    exit 1
fi

# 步骤1：检查ECS状态
echo "📊 步骤1：检查ECS状态..."
aliyun ecs DescribeInstanceStatus \
    --RegionId ${REGION} \
    --InstanceId.1 ${INSTANCE_ID} \
    --output json | jq -r '.InstanceStatuses.InstanceStatus[0].Status'

echo ""
echo "📤 步骤2：使用云助手执行部署..."

# 创建部署命令
DEPLOY_CMD='
# 清理旧容器
docker ps -a | grep hft- | awk "{print \$1}" | xargs -r docker rm -f

# 拉取或使用本地镜像
if ! docker images | grep -q "hft-collector:standard"; then
    echo "构建本地镜像..."
    # 这里假设镜像已经存在或通过其他方式传输
fi

# 创建docker-compose
cat > /root/docker-compose.yml << EOF
version: "3.8"

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
      multi
      --exchange binance_futures
      --symbols BTCUSDT,ETHUSDT,BNBUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
    networks:
      - hft-network

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
      --symbols BTCUSDT,ETHUSDT,BNBUSDT
      --streams orderbook,trades,l1,ticker
      --depth 5
    networks:
      - hft-network

networks:
  hft-network:
    driver: bridge
EOF

# 启动服务
cd /root && docker-compose up -d
docker-compose ps
'

# Base64编码
DEPLOY_CMD_BASE64=$(echo "${DEPLOY_CMD}" | base64)

# 执行云助手命令
echo "执行部署命令..."
COMMAND_ID=$(aliyun ecs RunCommand \
    --RegionId ${REGION} \
    --Type RunShellScript \
    --CommandContent "${DEPLOY_CMD_BASE64}" \
    --InstanceId.1 ${INSTANCE_ID} \
    --ContentEncoding Base64 \
    --Timeout 600 \
    --output json | jq -r '.CommandId')

echo "命令ID: ${COMMAND_ID}"
echo "等待执行..."
sleep 15

# 获取结果
echo ""
echo "📊 执行结果："
aliyun ecs DescribeInvocationResults \
    --RegionId ${REGION} \
    --CommandId ${COMMAND_ID} \
    --output json | jq -r '.Invocation.InvocationResults.InvocationResult[0].Output' | base64 -d

echo ""
echo "✅ 部署请求已发送"
echo ""
echo "验证：运行 ./verify-collection.sh"