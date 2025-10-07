#!/bin/bash
set -e

# HFT Collector 阿里云 ECS 部署脚本
# 用途：将 hft-collector 部署到阿里云 ECS，连接 AWS ClickHouse Cloud

# 阿里云 ECS 配置
INSTANCE_IP="8.216.39.177"
INSTANCE_ID="i-6we3w3olqr8ordj4z5nk"
SSH_KEY_PATH="$HOME/.ssh/id_rsa"  # 请确认你的 SSH key 路径
BINARY_PATH="./target/release/hft-collector"

# ClickHouse Cloud (AWS) 配置
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="${CLICKHOUSE_USER:-default}"
CLICKHOUSE_PASSWORD="${CLICKHOUSE_PASSWORD:-}"

echo "🚀 开始部署 HFT Collector 到阿里云 ECS..."
echo "📍 目标实例: $INSTANCE_ID ($INSTANCE_IP)"
echo "🗄️  ClickHouse: AWS Cloud"

# 检查二进制文件是否存在
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ 二进制文件不存在: $BINARY_PATH"
    echo "请先运行: cargo build --release -p hft-collector"
    exit 1
fi

# 检查 SSH 连接
echo "🔑 测试 SSH 连接..."
if ! ssh -i "$SSH_KEY_PATH" -o ConnectTimeout=10 -o StrictHostKeyChecking=no root@$INSTANCE_IP "echo 'SSH 连接成功'" 2>/dev/null; then
    echo "❌ SSH 连接失败，请检查："
    echo "   1. SSH key 路径是否正确: $SSH_KEY_PATH"
    echo "   2. 实例 IP 是否正确: $INSTANCE_IP"
    echo "   3. 安全组是否开放 SSH (22) 端口"
    exit 1
fi

# 1. 上传二进制文件到 ECS
echo "📦 上传二进制文件..."
scp -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no "$BINARY_PATH" root@$INSTANCE_IP:~/hft-collector

# 2. 安装必要依赖
echo "📋 安装系统依赖..."
ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP << 'EOF'
    # 更新系统
    apt-get update

    # 安装必要的依赖
    apt-get install -y ca-certificates curl htop systemctl

    # 设置二进制文件权限
    chmod +x ~/hft-collector

    # 创建日志目录
    mkdir -p /var/log/hft-collector
EOF

# 3. 创建 systemd 服务文件
echo "⚙️ 创建 systemd 服务..."
cat << 'SYSTEMD_EOF' | ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP "tee /etc/systemd/system/hft-collector.service"
[Unit]
Description=HFT Data Collector - Bitget USDT-Futures to AWS ClickHouse
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

# ClickHouse 连接环境变量
Environment=CLICKHOUSE_USER=${CLICKHOUSE_USER}
Environment=CLICKHOUSE_PASSWORD=${CLICKHOUSE_PASSWORD}
Environment=RUST_LOG=info

# 资源限制
LimitNOFILE=65536
LimitNPROC=32768

# 网络优化
Nice=-5

[Install]
WantedBy=multi-user.target
SYSTEMD_EOF

# 4. 配置和启动服务
echo "🔧 配置和启动服务..."
ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP << 'EOF'
    # 重新载入 systemd
    systemctl daemon-reload

    # 启用开机自动启动
    systemctl enable hft-collector

    # 停止旧服务（如果存在）
    systemctl stop hft-collector 2>/dev/null || true

    # 启动新服务
    systemctl start hft-collector

    # 等待几秒钟让服务启动
    sleep 3

    # 检查服务状态
    systemctl status hft-collector --no-pager
EOF

# 5. 验证部署
echo "📊 验证部署..."

# 检查服务状态
echo "🔍 检查服务运行状态..."
SERVICE_STATUS=$(ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP "systemctl is-active hft-collector")
if [ "$SERVICE_STATUS" = "active" ]; then
    echo "✅ 服务运行正常"
else
    echo "❌ 服务状态异常: $SERVICE_STATUS"
fi

# 显示最新日志
echo "📝 最新日志 (最后 10 行):"
ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP "journalctl -u hft-collector -n 10 --no-pager"

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 监控命令："
echo "   ssh -i $SSH_KEY_PATH root@$INSTANCE_IP"
echo "   journalctl -f -u hft-collector          # 实时日志"
echo "   systemctl status hft-collector          # 服务状态"
echo "   htop                                     # 系统资源"
echo ""
echo "🔧 管理命令："
echo "   systemctl restart hft-collector         # 重启服务"
echo "   systemctl stop hft-collector            # 停止服务"
echo "   systemctl start hft-collector           # 启动服务"
echo ""
echo "🌐 实例信息："
echo "   公网 IP: $INSTANCE_IP"
echo "   实例 ID: $INSTANCE_ID"
echo "   区域: ap-northeast-1 (东京)"
echo "   数据目标: AWS ClickHouse Cloud"
echo ""
echo "🎯 HFT Collector 现在正在从 BitGet 收集数据到你的 ClickHouse Cloud！"
