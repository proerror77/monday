#!/bin/bash
set -euo pipefail

echo "🚀 HFT Collector 自动部署到新创建的ECS"

# 读取ECS信息
if [[ ! -f ".ecs-info" ]]; then
    echo "❌ 未找到 .ecs-info 文件"
    echo "💡 请先运行 ./create-ecs-instance.sh 创建ECS实例"
    exit 1
fi

source .ecs-info

echo "📋 目标ECS信息："
echo "  实例ID: $INSTANCE_ID"
echo "  公网IP: $PUBLIC_IP"
echo "  密钥:   $KEY_FILE"
echo ""

# ClickHouse配置
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="default"
CLICKHOUSE_PASSWORD="s9wECb~NGZPOE"
DATABASE="hft_db"

# 检查必要文件
if [[ ! -f "$KEY_FILE" ]]; then
    echo "❌ SSH密钥文件不存在: $KEY_FILE"
    exit 1
fi

if [[ ! -f "Cargo.toml" ]]; then
    echo "❌ 请在rust_hft根目录运行此脚本"
    exit 1
fi

# 步骤1：编译collector
echo "📦 编译HFT Collector..."
cargo build --release -p hft-live --features live-bitget,live-binance
BINARY_PATH="$(pwd)/target/release/hft-live"

if [[ ! -f "$BINARY_PATH" ]]; then
    echo "❌ 编译失败，找不到二进制文件"
    exit 1
fi

echo "✅ 编译完成: $BINARY_PATH ($(ls -lh "$BINARY_PATH" | awk '{print $5}'))"

# 辅助函数
run_remote() {
    ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i "$KEY_FILE" "root@$PUBLIC_IP" "$1"
}

upload_file() {
    scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i "$KEY_FILE" "$1" "root@$PUBLIC_IP:$2"
}

# 步骤2：测试连接
echo "🔗 测试ECS连接..."
if ! run_remote "echo 'SSH连接成功'" 2>/dev/null; then
    echo "❌ SSH连接失败"
    exit 1
fi
echo "✅ ECS连接正常"

# 步骤3：安装依赖
echo "📥 安装系统依赖..."
run_remote "apt-get update && apt-get install -y ca-certificates curl jq"

# 步骤4：上传二进制文件
echo "📤 上传collector二进制文件..."
upload_file "$BINARY_PATH" "/root/hft-collector"
run_remote "chmod +x /root/hft-collector"

# 步骤5：创建配置文件
echo "⚙️  创建系统配置..."
upload_file "config/dev/system.yaml" "/root/system.yaml"

# 步骤6：创建systemd服务
echo "🔧 创建systemd服务..."
run_remote "cat > /etc/systemd/system/hft-collector.service << 'EOFSERVICE'
[Unit]
Description=HFT Data Collector - Multi Exchange
After=network.target
Wants=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector --config /root/system.yaml
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-collector

# 环境变量
Environment=RUST_LOG=info
Environment=CLICKHOUSE_URL=$CLICKHOUSE_URL
Environment=CLICKHOUSE_USER=$CLICKHOUSE_USER
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
Environment=CLICKHOUSE_DATABASE=$DATABASE

# 资源限制
LimitNOFILE=65536
LimitNPROC=32768

[Install]
WantedBy=multi-user.target
EOFSERVICE"

# 步骤7：启动服务
echo "▶️  启动服务..."
run_remote "systemctl daemon-reload"
run_remote "systemctl enable hft-collector"
run_remote "systemctl start hft-collector"

# 步骤8：验证部署
echo "📊 验证部署状态..."
sleep 3

echo ""
echo "=== 服务状态 ==="
run_remote "systemctl status hft-collector --no-pager -l" || true

echo ""
echo "=== 最新日志 ==="
run_remote "journalctl -u hft-collector -n 20 --no-pager" || true

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 监控命令："
echo "  查看实时日志: ssh -i $KEY_FILE root@$PUBLIC_IP 'journalctl -u hft-collector -f'"
echo "  查看服务状态: ssh -i $KEY_FILE root@$PUBLIC_IP 'systemctl status hft-collector'"
echo "  重启服务:     ssh -i $KEY_FILE root@$PUBLIC_IP 'systemctl restart hft-collector'"
echo ""
echo "🔍 SSH登录："
echo "  ssh -i $KEY_FILE root@$PUBLIC_IP"
echo ""
