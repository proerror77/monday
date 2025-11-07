#!/bin/bash

echo "🔍 测试ECS连接状态"

# 默认配置
DEFAULT_IP="47.128.222.180"
SSH_KEY="hft-admin-ssh-20250926144355.pem"

# 允许用户指定IP
ECS_IP="${1:-$DEFAULT_IP}"

echo "📍 测试IP: $ECS_IP"
echo "🔑 SSH密钥: $SSH_KEY"

# 测试1: Ping连接
echo ""
echo "1️⃣ 测试网络连接..."
if ping -c 2 "$ECS_IP" >/dev/null 2>&1; then
    echo "✅ Ping成功"
else
    echo "❌ Ping失败 - 可能原因："
    echo "   • ECS实例已停止"
    echo "   • IP地址已更改"
    echo "   • 安全组阻止ICMP"
fi

# 测试2: SSH连接
echo ""
echo "2️⃣ 测试SSH连接..."
if [[ -f "$SSH_KEY" ]]; then
    if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 -i "$SSH_KEY" "root@$ECS_IP" "echo 'SSH连接成功'" 2>/dev/null; then
        echo "✅ SSH连接成功"

        # 测试3: 检查现有服务
        echo ""
        echo "3️⃣ 检查collector服务状态..."
        ssh -o StrictHostKeyChecking=no -i "$SSH_KEY" "root@$ECS_IP" "systemctl is-active hft-collector 2>/dev/null || echo 'inactive'" | \
        while read status; do
            if [[ "$status" == "active" ]]; then
                echo "✅ Collector服务正在运行"
            else
                echo "⚠️  Collector服务未运行 (状态: $status)"
            fi
        done

        # 测试4: ClickHouse连接
        echo ""
        echo "4️⃣ 测试ClickHouse连接..."
        ssh -o StrictHostKeyChecking=no -i "$SSH_KEY" "root@$ECS_IP" "clickhouse-client --host 'kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud' --port 9440 --secure --user default --password 's9wECb~NGZPOE' --database hft_db --query 'SELECT 1' 2>/dev/null && echo '✅ ClickHouse连接正常' || echo '❌ ClickHouse连接失败'"

        echo ""
        echo "🎯 ECS实例可用！可以执行部署："
        echo "   ./deploy-to-ecs.sh"
    else
        echo "❌ SSH连接失败 - 可能原因："
        echo "   • SSH密钥不正确"
        echo "   • 安全组未开放22端口"
        echo "   • 用户名不是root"
        echo "   • 实例配置问题"
    fi
else
    echo "❌ SSH密钥文件不存在: $SSH_KEY"
fi

echo ""
echo "💡 如果连接失败，请查看 ECS_DEPLOYMENT_GUIDE.md 获取详细故障排除步骤"