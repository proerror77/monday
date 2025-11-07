#!/bin/bash
set -e

# HFT Collector 阿里云 CLI 自动部署脚本
# 使用阿里云 CLI 部署到 ECS 实例

# 配置变量
REGION="ap-northeast-1"
INSTANCE_ID="i-6we3w3olqr8ordj4z5nk"
INSTANCE_IP="8.216.39.177"
BINARY_NAME="hft-collector"
BINARY_PATH="../../target/release/hft-collector"

# ClickHouse 配置
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="${CLICKHOUSE_USER:-default}"
CLICKHOUSE_PASSWORD="${CLICKHOUSE_PASSWORD:-}"

echo "🚀 使用阿里云 CLI 部署 HFT Collector"
echo "📍 实例: $INSTANCE_ID ($INSTANCE_IP)"
echo "🌍 区域: $REGION"

# 1. 检查实例状态
echo "🔍 检查实例状态..."
INSTANCE_INFO=$(aliyun ecs DescribeInstances --RegionId $REGION --InstanceIds "[$INSTANCE_ID]")
INSTANCE_STATUS=$(echo $INSTANCE_INFO | grep -o '"Status":"[^"]*"' | cut -d'"' -f4)
echo "实例状态: $INSTANCE_STATUS"

if [ "$INSTANCE_STATUS" != "Running" ]; then
    echo "⚠️  实例未运行，尝试启动..."
    aliyun ecs StartInstance --InstanceId $INSTANCE_ID

    echo "等待实例启动..."
    while true; do
        INSTANCE_INFO=$(aliyun ecs DescribeInstances --RegionId $REGION --InstanceIds "[$INSTANCE_ID]")
        CURRENT_STATUS=$(echo $INSTANCE_INFO | grep -o '"Status":"[^"]*"' | cut -d'"' -f4)
        if [ "$CURRENT_STATUS" = "Running" ]; then
            break
        fi
        echo "实例启动中... ($CURRENT_STATUS)"
        sleep 10
    done
    echo "✅ 实例已启动"
fi

# 2. 构建二进制文件
echo "🔨 构建 Rust 二进制文件..."
if [ ! -f "$BINARY_PATH" ]; then
    echo "二进制文件不存在，开始构建..."
    cd ../..
    cargo build --release -p hft-collector
    cd apps/collector
fi

if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ 构建失败，二进制文件不存在"
    exit 1
fi

echo "✅ 二进制文件准备就绪: $(ls -lh $BINARY_PATH)"

# 3. 创建部署脚本
echo "📝 创建远程部署脚本..."
cat > /tmp/deploy_script.sh << 'DEPLOY_EOF'
#!/bin/bash
set -e

echo "📦 配置 HFT Collector 服务..."

# 停止现有服务
systemctl stop hft-collector 2>/dev/null || echo "服务不存在，继续..."

# 设置二进制文件权限
chmod +x ~/hft-collector

# 创建 systemd 服务文件
cat > /etc/systemd/system/hft-collector.service << 'SERVICE_EOF'
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

# 环境变量
Environment=CLICKHOUSE_USER=${CLICKHOUSE_USER}
Environment=CLICKHOUSE_PASSWORD=${CLICKHOUSE_PASSWORD}
Environment=RUST_LOG=info

# 资源限制
LimitNOFILE=65536
LimitNPROC=32768

[Install]
WantedBy=multi-user.target
SERVICE_EOF

# 重载 systemd 并启动服务
systemctl daemon-reload
systemctl enable hft-collector
systemctl start hft-collector

# 等待服务启动
sleep 5

# 检查状态
echo "🔍 服务状态:"
systemctl status hft-collector --no-pager

echo "📝 最新日志:"
journalctl -u hft-collector -n 10 --no-pager

DEPLOY_EOF

# 4. 使用阿里云 CLI 执行命令
echo "🚀 开始部署..."

# 上传二进制文件
echo "📦 上传二进制文件..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "mkdir -p ~/upload" \
    --InstanceId.1 $INSTANCE_ID \
    --Name "CreateUploadDir" \
    --Timeout 60

# 由于阿里云 CLI 不直接支持文件上传，我们需要通过编码方式
echo "📂 编码并上传二进制文件..."
BINARY_BASE64=$(base64 -i "$BINARY_PATH")

# 分块上传 (阿里云 CLI 命令长度有限制)
CHUNK_SIZE=50000
TOTAL_LENGTH=${#BINARY_BASE64}
CHUNKS=$((($TOTAL_LENGTH + $CHUNK_SIZE - 1) / $CHUNK_SIZE))

echo "文件大小: $(ls -lh $BINARY_PATH | awk '{print $5}'), Base64 长度: $TOTAL_LENGTH, 分块数: $CHUNKS"

# 清空目标文件
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "echo -n > ~/hft-collector.b64" \
    --InstanceId.1 $INSTANCE_ID \
    --Name "ClearTargetFile" \
    --Timeout 60

# 分块上传
for ((i=0; i<$CHUNKS; i++)); do
    start=$((i * $CHUNK_SIZE))
    chunk=${BINARY_BASE64:$start:$CHUNK_SIZE}

    echo "上传分块 $((i+1))/$CHUNKS..."

    aliyun ecs RunCommand \
        --RegionId $REGION \
        --Type RunShellScript \
        --CommandContent "echo -n '$chunk' >> ~/hft-collector.b64" \
        --InstanceId.1 $INSTANCE_ID \
        --Name "UploadChunk$i" \
        --Timeout 120
done

# 解码二进制文件
echo "🔓 解码二进制文件..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "base64 -d ~/hft-collector.b64 > ~/hft-collector && rm ~/hft-collector.b64 && chmod +x ~/hft-collector" \
    --InstanceId.1 $INSTANCE_ID \
    --Name "DecodeBinary" \
    --Timeout 120

# 5. 执行部署脚本
echo "🚀 执行部署脚本..."
DEPLOY_SCRIPT_BASE64=$(base64 -i /tmp/deploy_script.sh)

aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "echo '$DEPLOY_SCRIPT_BASE64' | base64 -d > /tmp/deploy.sh && chmod +x /tmp/deploy.sh && /tmp/deploy.sh" \
    --InstanceId.1 $INSTANCE_ID \
    --Name "ExecuteDeployScript" \
    --Timeout 300

# 6. 检查最终状态
echo "🔍 检查服务状态..."
sleep 10

aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "systemctl status hft-collector --no-pager && echo '=== 最新日志 ===' && journalctl -u hft-collector -n 20 --no-pager" \
    --InstanceId.1 $INSTANCE_ID \
    --Name "CheckFinalStatus" \
    --Timeout 60

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 监控命令："
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'systemctl status hft-collector' --InstanceId.1 $INSTANCE_ID --Name CheckStatus --Timeout 60"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'journalctl -f -u hft-collector' --InstanceId.1 $INSTANCE_ID --Name ViewLogs --Timeout 300"
echo ""
echo "🌐 实例信息："
echo "  公网 IP: $INSTANCE_IP"
echo "  实例 ID: $INSTANCE_ID"
echo "  数据流: BitGet → 阿里云 ECS → AWS ClickHouse Cloud"
echo ""
echo "🎯 HFT Collector 现在正在收集数据！"

# 清理临时文件
rm -f /tmp/deploy_script.sh
