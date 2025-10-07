#!/bin/bash
set -e

# 分块上传的阿里云 ECS 部署脚本

REGION="ap-northeast-1"
INSTANCE_ID="i-6we3w3olqr8ordj4z5nk"
BINARY_PATH="../../target/release/hft-collector"

echo "🚀 分块部署 HFT Collector 到阿里云 ECS"
echo "📍 实例: $INSTANCE_ID"

# 检查二进制文件
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ 二进制文件不存在: $BINARY_PATH"
    exit 1
fi

BINARY_SIZE=$(ls -lh "$BINARY_PATH" | awk '{print $5}')
echo "✅ 二进制文件已就绪: $BINARY_SIZE"

# 1. 停止现有服务
echo "🛑 停止现有服务..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "systemctl stop hft-collector 2>/dev/null || true; rm -f ~/hft-collector ~/hft-collector.b64" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "StopAndCleanup" \
    --Timeout 60

sleep 3

# 2. 编码二进制文件并分块上传
echo "📦 编码二进制文件..."
BINARY_BASE64=$(base64 -i "$BINARY_PATH")
TOTAL_LENGTH=${#BINARY_BASE64}

# 每块最大 30KB (阿里云 CLI 命令内容限制约 32KB)
CHUNK_SIZE=30000
CHUNKS=$(( (TOTAL_LENGTH + CHUNK_SIZE - 1) / CHUNK_SIZE ))

echo "Base64 长度: $TOTAL_LENGTH 字符, 分块数: $CHUNKS"

# 清空目标文件
echo "🗂️ 初始化上传..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "echo -n > ~/hft-collector.b64" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "InitUpload" \
    --Timeout 60

sleep 2

# 分块上传
for ((i=0; i<$CHUNKS; i++)); do
    start=$((i * CHUNK_SIZE))
    chunk=${BINARY_BASE64:$start:$CHUNK_SIZE}
    progress=$((i+1))

    echo "📦 上传分块 $progress/$CHUNKS ($(echo $chunk | wc -c) bytes)..."

    aliyun ecs RunCommand \
        --RegionId $REGION \
        --Type RunShellScript \
        --CommandContent "echo -n '$chunk' >> ~/hft-collector.b64" \
        --InstanceId.1 "$INSTANCE_ID" \
        --Name "UploadChunk$i" \
        --Timeout 120

    sleep 1
done

# 3. 解码二进制文件
echo "🔓 解码二进制文件..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "base64 -d ~/hft-collector.b64 > ~/hft-collector && rm ~/hft-collector.b64 && chmod +x ~/hft-collector && ls -lh ~/hft-collector" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "DecodeBinary" \
    --Timeout 120

sleep 3

# 4. 创建 systemd 服务
echo "⚙️ 创建系统服务..."
SERVICE_SCRIPT='#!/bin/bash
cat > /etc/systemd/system/hft-collector.service << '\''EOF'\''
[Unit]
Description=HFT Data Collector - Bitget to AWS ClickHouse Cloud
After=network.target
Wants=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector --ch-url https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443 --database HTf_db_collect --top-limit 20 --batch-size 1000 --flush-ms 2000
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
WantedBy=multi-user.target
EOF

echo "Service file created successfully"'

aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "$SERVICE_SCRIPT" \
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
    --CommandContent "echo '=== 服务状态 ===' && systemctl status hft-collector --no-pager && echo && echo '=== 最新日志 ===' && journalctl -u hft-collector -n 15 --no-pager" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "CheckStatus" \
    --Timeout 60

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 后续监控命令："
echo "  # 查看服务状态"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'systemctl status hft-collector' --InstanceId.1 '$INSTANCE_ID' --Name Status"
echo ""
echo "  # 查看实时日志"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'journalctl -u hft-collector -n 20' --InstanceId.1 '$INSTANCE_ID' --Name Logs"
echo ""
echo "  # 重启服务"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'systemctl restart hft-collector' --InstanceId.1 '$INSTANCE_ID' --Name Restart"
echo ""
echo "🌐 数据流向: BitGet → 阿里云 ECS (8.216.39.177) → AWS ClickHouse Cloud"
echo "🎯 HFT Collector 正在 24/7 收集数据！"
