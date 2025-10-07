#!/bin/bash
set -e

# 统一交易所收集器部署脚本

if [ $# -lt 2 ]; then
    echo "使用方法: $0 <交易所名称> <ECS实例ID> [SYMBOLS白名单CSV]"
    echo "支持的交易所: binance, bitget, bybit, asterdex, hyperliquid"
    echo "示例: $0 binance i-6we3w3olqr8ordj4z5nk 'BTCUSDT,ETHUSDT,SOLUSDT'"
    exit 1
fi

EXCHANGE_NAME=$1
INSTANCE_ID=$2
REGION="ap-northeast-1"
BINARY_PATH="../../target/release/hft-collector"
SERVICE_NAME="${EXCHANGE_NAME}-collector"

# 可从第3个参数或环境变量注入白名单
if [ -n "$3" ]; then
  export SYMBOLS="$3"
fi

echo "🚀 部署 ${EXCHANGE_NAME} Collector 到阿里云 ECS"
echo "📍 实例: $INSTANCE_ID"

# 检查二进制文件是否存在
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ 二进制文件不存在: $BINARY_PATH"
    echo "正在构建..."
    cd ../..
    cargo build --release -p hft-collector
    cd apps/collector
fi

BINARY_SIZE=$(ls -lh "$BINARY_PATH" | awk '{print $5}')
echo "✅ 二进制文件已就绪: $BINARY_SIZE"

# 上传到文件分享服务
echo "📤 上传二进制文件到 0x0.st..."
DOWNLOAD_URL=$(curl -s -F"file=@$BINARY_PATH" https://0x0.st)

if [ -z "$DOWNLOAD_URL" ]; then
    echo "❌ 文件上传失败"
    exit 1
fi

echo "📥 下载链接: $DOWNLOAD_URL"

# 在 ECS 上下载并配置服务
echo "🔽 在 ECS 上下载并配置服务..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "
        echo '🛑 停止现有服务...'
        systemctl stop $SERVICE_NAME 2>/dev/null || true

        echo '📥 下载二进制文件...'
        wget -O ~/collector.tmp '$DOWNLOAD_URL'
        mv ~/collector.tmp ~/collector
        chmod +x ~/collector

        echo '⚙️ 创建 systemd 服务...'
        cat > /etc/systemd/system/$SERVICE_NAME.service << 'EOF'
[Unit]
Description=${EXCHANGE_NAME} Data Collector to AWS ClickHouse Cloud
After=network.target
Wants=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/collector multi --exchange ${EXCHANGE_NAME} --ch-url https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443 --database hft_db --top-limit 0 --batch-size 1000 --flush-ms 2000
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=$SERVICE_NAME

Environment=CLICKHOUSE_USER=${CLICKHOUSE_USER}
Environment=CLICKHOUSE_PASSWORD=${CLICKHOUSE_PASSWORD}
Environment=RUST_LOG=info
Environment=SYMBOLS=${SYMBOLS}
Environment=ALL_SYMBOLS=true
Environment=QUOTE_FILTER=USDT
Environment=EXCLUDE_SYMBOL_CONTAINS=UP,DOWN,BULL,BEAR

LimitNOFILE=65536
LimitNPROC=32768

[Install]
WantedBy=multi-user.target
EOF

        echo '🚀 启动服务...'
        systemctl daemon-reload
        systemctl enable $SERVICE_NAME
        systemctl start $SERVICE_NAME

        sleep 3

        echo '🔍 检查服务状态...'
        systemctl status $SERVICE_NAME --no-pager || true

        echo ''
        echo '=== 最新日志 ==='
        journalctl -u $SERVICE_NAME -n 15 --no-pager || true
    " \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "Deploy$(echo ${EXCHANGE_NAME} | sed 's/.*/\u&/')" \
    --Timeout 300

echo ""
EXCHANGE_UPPER=$(echo ${EXCHANGE_NAME} | tr '[:lower:]' '[:upper:]')
echo "✅ ${EXCHANGE_UPPER} Collector 部署完成！"
echo ""
echo "📊 监控命令："
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'systemctl status $SERVICE_NAME' --InstanceId.1 '$INSTANCE_ID' --Name Status"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'journalctl -f -u $SERVICE_NAME -n 30' --InstanceId.1 '$INSTANCE_ID' --Name Logs"
echo ""
echo "🌐 数据流向: ${EXCHANGE_UPPER} → 阿里云 ECS → AWS ClickHouse Cloud"
echo "🎯 ${EXCHANGE_UPPER} Collector 正在 24/7 收集数据！"
