#!/bin/bash
set -e

# HTTP 下载方式部署脚本

REGION="ap-northeast-1"
INSTANCE_ID="i-6we3w3olqr8ordj4z5nk"
BINARY_PATH="../../target/release/hft-collector"

echo "🚀 HTTP 方式部署 HFT Collector 到阿里云 ECS"
echo "📍 实例: $INSTANCE_ID"

# 检查二进制文件
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ 二进制文件不存在: $BINARY_PATH"
    exit 1
fi

BINARY_SIZE=$(ls -lh "$BINARY_PATH" | awk '{print $5}')
echo "✅ 二进制文件已就绪: $BINARY_SIZE"

# 1. 启动本地 HTTP 服务器
echo "🌐 启动本地 HTTP 服务器..."
cd ../../target/release

# 在后台启动 Python HTTP 服务器
python3 -m http.server 8000 > /dev/null 2>&1 &
HTTP_PID=$!
echo "📡 HTTP 服务器已启动 (PID: $HTTP_PID)"

# 等待服务器启动
sleep 2

# 获取本地 IP 地址
LOCAL_IP=$(curl -s ifconfig.me 2>/dev/null || echo "获取IP失败")
echo "🌍 本地访问地址: http://$LOCAL_IP:8000/hft-collector"

# 2. 在 ECS 上下载二进制文件
echo "📥 在 ECS 上下载二进制文件..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "
        # 停止现有服务
        systemctl stop hft-collector 2>/dev/null || true

        # 下载二进制文件
        curl -f -o ~/hft-collector http://$LOCAL_IP:8000/hft-collector

        # 设置权限
        chmod +x ~/hft-collector

        # 验证文件
        ls -lh ~/hft-collector

        echo '二进制文件下载完成'
    " \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "DownloadBinary" \
    --Timeout 180

# 3. 停止 HTTP 服务器
echo "🛑 停止本地 HTTP 服务器..."
kill $HTTP_PID 2>/dev/null || true
cd ../../apps/collector

# 4. 启动服务
echo "🚀 启动数据收集服务..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "systemctl daemon-reload && systemctl start hft-collector" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "StartService" \
    --Timeout 60

sleep 3

# 5. 检查服务状态
echo "🔍 检查服务状态..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "echo '=== 服务状态 ===' && systemctl status hft-collector --no-pager && echo && echo '=== 最新日志 ===' && journalctl -u hft-collector -n 15 --no-pager" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "CheckStatus" \
    --Timeout 60

echo ""
echo "✅ HTTP 部署完成！"
echo ""
echo "📊 监控命令："
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'systemctl status hft-collector' --InstanceId.1 '$INSTANCE_ID' --Name Status"
echo ""
echo "🌐 数据流向: BitGet → 阿里云 ECS (8.216.39.177) → AWS ClickHouse Cloud"
echo "🎯 HFT Collector 正在 24/7 收集数据！"