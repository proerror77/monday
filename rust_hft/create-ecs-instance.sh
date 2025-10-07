#!/bin/bash
set -euo pipefail

echo "🚀 创建阿里云抢占式ECS实例部署HFT Collector"

# ========== 配置参数 ==========
REGION="cn-hongkong"                    # 区域：香港
ZONE="cn-hongkong-b"                    # 可用区
INSTANCE_TYPE="ecs.t6-c1m2.large"       # 实例规格：2核2G（最低成本）
IMAGE_ID="ubuntu_22_04_x64_20G_alibase_20230907.vhd"  # Ubuntu 22.04镜像
SECURITY_GROUP_NAME="hft-collector-sg"
INSTANCE_NAME="hft-collector-spot"
KEY_PAIR_NAME="hft-collector-key"
KEY_FILE="hft-collector-key-new.pem"

# 价格策略：抢占式实例
SPOT_STRATEGY="SpotWithPriceLimit"      # 设置价格上限的抢占式实例
SPOT_PRICE_LIMIT="0.05"                 # 价格上限：0.05元/小时（约为按量的1/3）

# 网络计费：按流量计费
INTERNET_CHARGE_TYPE="PayByTraffic"     # 按使用流量计费
INTERNET_MAX_BANDWIDTH_OUT="100"        # 峰值带宽100Mbps（按流量计费时这是上限）

# 系统盘
SYSTEM_DISK_CATEGORY="cloud_efficiency" # 高效云盘（最低成本）
SYSTEM_DISK_SIZE="40"                   # 40GB系统盘

# ========== 辅助函数 ==========
run_aliyun() {
    aliyun ecs "$@" --region "$REGION" 2>&1
}

check_aliyun_cli() {
    if ! command -v aliyun &> /dev/null; then
        echo "❌ 阿里云CLI未安装"
        echo "💡 安装方法："
        echo "   brew install aliyun-cli  # macOS"
        echo "   或访问: https://github.com/aliyun/aliyun-cli"
        exit 1
    fi

    # 检查配置
    if ! aliyun configure list &> /dev/null; then
        echo "❌ 阿里云CLI未配置"
        echo "💡 请先配置："
        echo "   aliyun configure"
        exit 1
    fi

    echo "✅ 阿里云CLI配置正常"
}

# ========== 主流程 ==========

# 1. 检查CLI
check_aliyun_cli

# 2. 创建或获取安全组
echo "🔒 配置安全组..."
SECURITY_GROUP_ID=$(run_aliyun DescribeSecurityGroups \
    --SecurityGroupName "$SECURITY_GROUP_NAME" \
    | jq -r '.SecurityGroups.SecurityGroup[0].SecurityGroupId // empty')

if [[ -z "$SECURITY_GROUP_ID" ]]; then
    echo "📝 创建新安全组: $SECURITY_GROUP_NAME"
    SECURITY_GROUP_ID=$(run_aliyun CreateSecurityGroup \
        --SecurityGroupName "$SECURITY_GROUP_NAME" \
        --Description "HFT Collector Security Group" \
        | jq -r '.SecurityGroupId')
    echo "✅ 安全组创建成功: $SECURITY_GROUP_ID"

    # 添加SSH规则（允许所有IP访问22端口）
    echo "🔓 添加SSH访问规则..."
    run_aliyun AuthorizeSecurityGroup \
        --SecurityGroupId "$SECURITY_GROUP_ID" \
        --IpProtocol "tcp" \
        --PortRange "22/22" \
        --SourceCidrIp "0.0.0.0/0" \
        --Priority "1" > /dev/null
    echo "✅ SSH规则添加成功"
else
    echo "✅ 使用现有安全组: $SECURITY_GROUP_ID"
fi

# 3. 创建或导入密钥对
echo "🔑 配置SSH密钥对..."
EXISTING_KEY=$(run_aliyun DescribeKeyPairs \
    --KeyPairName "$KEY_PAIR_NAME" \
    | jq -r '.KeyPairs.KeyPair[0].KeyPairName // empty')

if [[ -z "$EXISTING_KEY" ]]; then
    echo "📝 创建新密钥对: $KEY_PAIR_NAME"
    PRIVATE_KEY=$(run_aliyun CreateKeyPair \
        --KeyPairName "$KEY_PAIR_NAME" \
        | jq -r '.PrivateKeyBody')

    echo "$PRIVATE_KEY" > "$KEY_FILE"
    chmod 600 "$KEY_FILE"
    echo "✅ 密钥对创建成功，保存到: $KEY_FILE"
else
    echo "✅ 使用现有密钥对: $KEY_PAIR_NAME"
    if [[ ! -f "$KEY_FILE" ]]; then
        echo "⚠️  警告: 密钥文件 $KEY_FILE 不存在"
        echo "💡 请确保你有对应的私钥文件"
    fi
fi

# 4. 获取VPC和交换机信息
echo "🌐 获取网络配置..."
VPC_ID=$(run_aliyun DescribeVpcs \
    | jq -r '.Vpcs.Vpc[0].VpcId // empty')

if [[ -z "$VPC_ID" ]]; then
    echo "❌ 未找到VPC，请先创建VPC"
    exit 1
fi

VSWITCH_ID=$(run_aliyun DescribeVSwitches \
    --VpcId "$VPC_ID" \
    --ZoneId "$ZONE" \
    | jq -r '.VSwitches.VSwitch[0].VSwitchId // empty')

if [[ -z "$VSWITCH_ID" ]]; then
    echo "❌ 未找到交换机，请先在可用区 $ZONE 创建交换机"
    exit 1
fi

echo "✅ VPC: $VPC_ID, VSwitch: $VSWITCH_ID"

# 5. 创建抢占式实例
echo "💰 创建抢占式ECS实例..."
echo "   规格: $INSTANCE_TYPE (2核2G)"
echo "   计费: 抢占式实例，价格上限 $SPOT_PRICE_LIMIT 元/小时"
echo "   带宽: 按流量计费，峰值 ${INTERNET_MAX_BANDWIDTH_OUT}Mbps"

INSTANCE_ID=$(run_aliyun CreateInstance \
    --InstanceType "$INSTANCE_TYPE" \
    --ImageId "$IMAGE_ID" \
    --SecurityGroupId "$SECURITY_GROUP_ID" \
    --VSwitchId "$VSWITCH_ID" \
    --InstanceName "$INSTANCE_NAME" \
    --KeyPairName "$KEY_PAIR_NAME" \
    --SpotStrategy "$SPOT_STRATEGY" \
    --SpotPriceLimit "$SPOT_PRICE_LIMIT" \
    --InternetChargeType "$INTERNET_CHARGE_TYPE" \
    --InternetMaxBandwidthOut "$INTERNET_MAX_BANDWIDTH_OUT" \
    --SystemDisk.Category "$SYSTEM_DISK_CATEGORY" \
    --SystemDisk.Size "$SYSTEM_DISK_SIZE" \
    | jq -r '.InstanceId')

if [[ -z "$INSTANCE_ID" ]]; then
    echo "❌ 实例创建失败"
    exit 1
fi

echo "✅ 实例创建成功: $INSTANCE_ID"

# 6. 启动实例
echo "▶️  启动实例..."
run_aliyun StartInstance --InstanceId "$INSTANCE_ID" > /dev/null
echo "✅ 实例启动中..."

# 7. 等待实例运行并获取公网IP
echo "⏳ 等待实例运行并分配公网IP..."
for i in {1..30}; do
    STATUS=$(run_aliyun DescribeInstances \
        --InstanceIds "[$INSTANCE_ID]" \
        | jq -r '.Instances.Instance[0].Status')

    PUBLIC_IP=$(run_aliyun DescribeInstances \
        --InstanceIds "[$INSTANCE_ID]" \
        | jq -r '.Instances.Instance[0].PublicIpAddress.IpAddress[0] // empty')

    if [[ "$STATUS" == "Running" ]] && [[ -n "$PUBLIC_IP" ]]; then
        echo "✅ 实例运行中，公网IP: $PUBLIC_IP"
        break
    fi

    echo "   状态: $STATUS，等待中... ($i/30)"
    sleep 10
done

if [[ -z "$PUBLIC_IP" ]]; then
    echo "❌ 获取公网IP超时"
    exit 1
fi

# 8. 等待SSH可用
echo "🔗 等待SSH服务就绪..."
for i in {1..30}; do
    if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 -i "$KEY_FILE" "root@$PUBLIC_IP" "echo 'SSH连接成功'" 2>/dev/null; then
        echo "✅ SSH连接成功"
        break
    fi
    echo "   SSH尝试 $i/30..."
    sleep 10
done

# 9. 输出实例信息
echo ""
echo "========================================="
echo "✅ ECS实例创建成功！"
echo "========================================="
echo ""
echo "📊 实例信息："
echo "  实例ID:   $INSTANCE_ID"
echo "  实例名称: $INSTANCE_NAME"
echo "  公网IP:   $PUBLIC_IP"
echo "  规格:     $INSTANCE_TYPE (2核2G)"
echo "  区域:     $REGION / $ZONE"
echo ""
echo "💰 计费信息："
echo "  实例计费: 抢占式，价格上限 $SPOT_PRICE_LIMIT 元/小时"
echo "  带宽计费: 按流量付费，峰值 ${INTERNET_MAX_BANDWIDTH_OUT}Mbps"
echo "  预估成本: ~0.05元/小时 + 流量费"
echo ""
echo "🔑 SSH连接："
echo "  ssh -i $KEY_FILE root@$PUBLIC_IP"
echo ""
echo "📝 下一步："
echo "  1. 更新 deploy-to-ecs.sh 中的 ECS_IP 为: $PUBLIC_IP"
echo "  2. 更新 SSH_KEY 为: $KEY_FILE"
echo "  3. 运行: ./deploy-to-ecs.sh"
echo ""
echo "🛠️  管理命令："
echo "  查看实例: aliyun ecs DescribeInstances --InstanceIds [$INSTANCE_ID] --region $REGION"
echo "  停止实例: aliyun ecs StopInstance --InstanceId $INSTANCE_ID --region $REGION"
echo "  启动实例: aliyun ecs StartInstance --InstanceId $INSTANCE_ID --region $REGION"
echo "  删除实例: aliyun ecs DeleteInstance --InstanceId $INSTANCE_ID --Force true --region $REGION"
echo ""

# 10. 创建环境配置文件
cat > ".ecs-info" << EOF
INSTANCE_ID=$INSTANCE_ID
PUBLIC_IP=$PUBLIC_IP
KEY_FILE=$KEY_FILE
REGION=$REGION
EOF

echo "💾 实例信息已保存到 .ecs-info"
echo ""
