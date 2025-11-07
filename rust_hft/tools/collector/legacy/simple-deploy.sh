#!/bin/bash
set -e

# 简化的阿里云 ECS 部署脚本

REGION="ap-northeast-1"
INSTANCE_ID="i-6we3w3olqr8ordj4z5nk"
BINARY_PATH="../../target/release/hft-collector"

echo "🚀 部署 HFT Collector 到阿里云 ECS"
echo "📍 实例: $INSTANCE_ID"

# 检查二进制文件
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ 二进制文件不存在: $BINARY_PATH"
    exit 1
fi

echo "✅ 二进制文件已就绪: $(ls -lh $BINARY_PATH | awk '{print $5}')"

# 1. 编码二进制文件
echo "📦 编码二进制文件..."
BINARY_BASE64=$(base64 -i "$BINARY_PATH")

# 2. 停止现有服务
echo "🛑 停止现有服务..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "systemctl stop hft-collector 2>/dev/null || true" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "StopService" \
    --Timeout 60

sleep 5

# 3. 上传并解码二进制文件
echo "📂 上传二进制文件..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "echo '$BINARY_BASE64' | base64 -d > ~/hft-collector && chmod +x ~/hft-collector" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "UploadBinary" \
    --Timeout 120

sleep 3

# 4. 创建 systemd 服务
echo "⚙️ 创建系统服务..."
SERVICE_CONTENT='[Unit]
Description=HFT Data Collector - Bitget to AWS ClickHouse Cloud
After=network.target
Wants=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector --ch-url https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443 --database hft_db --top-limit 20 --batch-size 1000 --flush-ms 2000
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-collector

Environment=CLICKHOUSE_USER=default
Environment=CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
Environment=RUST_LOG=info

LimitNOFILE=65536
LimitNPROC=32768

[Install]
WantedBy=multi-user.target'

SERVICE_BASE64=$(echo "$SERVICE_CONTENT" | base64 -w 0)

aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "echo '$SERVICE_BASE64' | base64 -d > /etc/systemd/system/hft-collector.service" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "CreateService" \
    --Timeout 60

sleep 3

# 5. 启动服务
echo "🚀 启动数据收集服务..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "systemctl daemon-reload && systemctl enable hft-collector && systemctl start hft-collector" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "StartService" \
    --Timeout 60

sleep 5

# 6. 检查服务状态
echo "🔍 检查服务状态..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "systemctl status hft-collector --no-pager && echo '=== 最新日志 ===' && journalctl -u hft-collector -n 10 --no-pager" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "CheckStatus" \
    --Timeout 60

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 监控命令："
echo "  # 查看服务状态"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'systemctl status hft-collector' --InstanceIds '[\"$INSTANCE_ID\"]' --Name CheckStatus"
echo ""
echo "  # 查看实时日志"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'journalctl -f -u hft-collector' --InstanceIds '[\"$INSTANCE_ID\"]' --Name ViewLogs"
echo ""
echo "🌐 数据流: BitGet → 阿里云 ECS (8.216.39.177) → AWS ClickHouse Cloud"
echo "🎯 HFT Collector 正在 24/7 收集数据到 ClickHouse！"