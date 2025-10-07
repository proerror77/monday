#!/bin/bash
set -e

# 优化的阿里云 ECS 部署脚本
# 改进: 更小的分块、更好的错误处理、进度显示

REGION="ap-northeast-1"
INSTANCE_ID="i-6we3w3olqr8ordj4z5nk"
BINARY_PATH="../../target/release/hft-collector"

echo "🚀 优化部署 HFT Collector 到阿里云 ECS"
echo "📍 实例: $INSTANCE_ID"

# 检查二进制文件
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ 二进制文件不存在: $BINARY_PATH"
    exit 1
fi

BINARY_SIZE=$(ls -lh "$BINARY_PATH" | awk '{print $5}')
echo "✅ 二进制文件已就绪: $BINARY_SIZE"

# 1. 停止现有服务并清理
echo "🛑 停止现有服务并清理..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "systemctl stop hft-collector 2>/dev/null || true; rm -f ~/hft-collector ~/hft-collector.b64" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "StopAndCleanup" \
    --Timeout 60

sleep 2

# 2. 编码二进制文件并使用更小的分块
echo "📦 编码二进制文件..."
BINARY_BASE64=$(base64 -i "$BINARY_PATH")
TOTAL_LENGTH=${#BINARY_BASE64}

# 使用更小的分块以提高成功率 (20KB)
CHUNK_SIZE=20000
CHUNKS=$(( (TOTAL_LENGTH + CHUNK_SIZE - 1) / CHUNK_SIZE ))

echo "📊 上传参数:"
echo "  - Base64 长度: $TOTAL_LENGTH 字符"
echo "  - 分块大小: $CHUNK_SIZE 字符"
echo "  - 分块数量: $CHUNKS"

# 清空目标文件
echo "🗂️ 初始化上传..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "echo -n > ~/hft-collector.b64" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "InitUpload" \
    --Timeout 60

sleep 1

# 分块上传，增加重试机制
echo "📦 开始分块上传..."
for ((i=0; i<$CHUNKS; i++)); do
    start=$((i * CHUNK_SIZE))
    chunk=${BINARY_BASE64:$start:$CHUNK_SIZE}
    progress=$((i+1))
    percent=$(( progress * 100 / CHUNKS ))

    echo -n "📦 上传分块 $progress/$CHUNKS ($percent%) ..."

    # 重试机制
    retry_count=0
    max_retries=3

    while [ $retry_count -lt $max_retries ]; do
        if aliyun ecs RunCommand \
            --RegionId $REGION \
            --Type RunShellScript \
            --CommandContent "echo -n '$chunk' >> ~/hft-collector.b64" \
            --InstanceId.1 "$INSTANCE_ID" \
            --Name "UploadChunk$i" \
            --Timeout 120 > /dev/null 2>&1; then
            echo " ✅"
            break
        else
            retry_count=$((retry_count + 1))
            echo -n " 🔄重试($retry_count/$max_retries)"
            sleep 1
        fi
    done

    if [ $retry_count -eq $max_retries ]; then
        echo " ❌ 分块 $i 上传失败"
        exit 1
    fi

    # 每10个分块后暂停一下，避免API限流
    if [ $((i % 10)) -eq 9 ]; then
        echo "⏸️  暂停 2 秒避免限流..."
        sleep 2
    fi
done

echo "✅ 所有分块上传完成"

# 3. 验证并解码二进制文件
echo "🔍 验证上传完整性..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "wc -c ~/hft-collector.b64 | awk '{print \$1}'" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "CheckUploadSize" \
    --Timeout 60

echo "🔓 解码二进制文件..."
aliyun ecs RunCommand \
    --RegionId $REGION \
    --Type RunShellScript \
    --CommandContent "base64 -d ~/hft-collector.b64 > ~/hft-collector && rm ~/hft-collector.b64 && chmod +x ~/hft-collector && ls -lh ~/hft-collector" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "DecodeBinary" \
    --Timeout 120

sleep 2

# 4. 启动服务（systemd 服务文件应该已经存在）
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
    --CommandContent "echo '=== 服务状态 ===' && systemctl status hft-collector --no-pager && echo && echo '=== 最新日志 ===' && journalctl -u hft-collector -n 20 --no-pager" \
    --InstanceId.1 "$INSTANCE_ID" \
    --Name "CheckStatus" \
    --Timeout 60

echo ""
echo "✅ 优化部署完成！"
echo ""
echo "📊 监控命令："
echo "  # 查看服务状态"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'systemctl status hft-collector' --InstanceId.1 '$INSTANCE_ID' --Name Status"
echo ""
echo "  # 查看实时日志"
echo "  aliyun ecs RunCommand --RegionId $REGION --Type RunShellScript --CommandContent 'journalctl -f -u hft-collector -n 30' --InstanceId.1 '$INSTANCE_ID' --Name Logs"
echo ""
echo "🌐 数据流向: BitGet → 阿里云 ECS (8.216.39.177) → AWS ClickHouse Cloud"
echo "🎯 HFT Collector 正在 24/7 收集数据！"