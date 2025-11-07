#!/bin/bash
set -euo pipefail

echo "🚀 HFT Collector ECS 完整部署脚本"

# 配置参数
ECS_IP="${ECS_IP:-8.221.136.162}"  # 当前IP地址
SSH_KEY="hft-collector-key.pem"
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="default"
CLICKHOUSE_PASSWORD="s9wECb~NGZPOE"
DATABASE="hft_db"

echo "📋 配置信息："
echo "  ECS IP: $ECS_IP"
echo "  SSH Key: $SSH_KEY"
echo "  ClickHouse: $CLICKHOUSE_URL"
echo "  Database: $DATABASE"

# 检查必要文件
if [[ ! -f "$SSH_KEY" ]]; then
    echo "❌ SSH密钥文件不存在: $SSH_KEY"
    exit 1
fi

if [[ ! -f "apps/collector/Cargo.toml" ]]; then
    echo "❌ 请在rust_hft根目录运行此脚本"
    exit 1
fi

# 步骤1：编译最新版本
echo "📦 编译HFT Collector..."
cd apps/collector
cargo build --release --features collector-binance
cd ../..
BINARY_PATH="$(pwd)/target/release/hft-collector"

if [[ ! -f "$BINARY_PATH" ]]; then
    echo "❌ 编译失败，找不到二进制文件"
    exit 1
fi

echo "✅ 编译完成: $BINARY_PATH ($(ls -lh "$BINARY_PATH" | awk '{print $5}'))"

# 辅助函数
run_remote() {
    ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i "$SSH_KEY" "root@$ECS_IP" "$1"
}

upload_file() {
    scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i "$SSH_KEY" "$1" "root@$ECS_IP:$2"
}

# 步骤2：测试连接
echo "🔗 测试ECS连接..."
if ! ping -c 2 "$ECS_IP" >/dev/null 2>&1; then
    echo "❌ 无法ping通ECS: $ECS_IP"
    echo "💡 请检查："
    echo "   1. ECS实例是否正在运行"
    echo "   2. 安全组是否允许SSH(22)和ICMP"
    echo "   3. IP地址是否已更改"
    exit 1
fi

if ! run_remote "echo 'SSH连接成功'" 2>/dev/null; then
    echo "❌ SSH连接失败"
    echo "💡 请检查："
    echo "   1. SSH密钥是否正确"
    echo "   2. 安全组是否允许SSH(22)"
    echo "   3. 实例用户是否为root"
    exit 1
fi

echo "✅ ECS连接正常"

# 步骤3：上传二进制文件
echo "📤 上传collector二进制文件..."
upload_file "$BINARY_PATH" "/root/hft-collector-new"
run_remote "chmod +x /root/hft-collector-new"

# 步骤4：创建配置和服务
echo "⚙️  创建systemd服务配置..."
run_remote "cat > /etc/systemd/system/hft-collector.service << 'EOF'
[Unit]
Description=HFT Data Collector - Binance Spot & Futures
After=network.target
Wants=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector-new multi --exchange binance --ch-url $CLICKHOUSE_URL --database $DATABASE --top-limit 5 --batch-size 1000 --flush-ms 2000
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-collector

# 环境变量
Environment=RUST_LOG=info
Environment=CLICKHOUSE_USER=$CLICKHOUSE_USER
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD

# 资源限制
LimitNOFILE=65536
LimitNPROC=32768

[Install]
WantedBy=multi-user.target
EOF"

# 步骤5：部署和启动
echo "🔄 停止旧服务并部署新版本..."
run_remote "systemctl stop hft-collector 2>/dev/null || true"
run_remote "mv /root/hft-collector-real /root/hft-collector-backup-\$(date +%s) 2>/dev/null || true"
run_remote "mv /root/hft-collector-new /root/hft-collector-real"

echo "▶️  启动服务..."
run_remote "systemctl daemon-reload"
run_remote "systemctl enable hft-collector"
run_remote "systemctl start hft-collector"

# 步骤6：验证部署
echo "📊 验证部署状态..."
sleep 3

echo "=== 服务状态 ==="
run_remote "systemctl status hft-collector --no-pager -l" || true

echo ""
echo "=== 最新日志 ==="
run_remote "journalctl -u hft-collector -n 10 --no-pager" || true

echo ""
echo "=== ClickHouse连接测试 ==="
run_remote "clickhouse-client --host 'kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud' --port 9440 --secure --user $CLICKHOUSE_USER --password '$CLICKHOUSE_PASSWORD' --database $DATABASE --query 'SELECT 1 as test' 2>/dev/null && echo '✅ ClickHouse连接正常' || echo '❌ ClickHouse连接失败'"

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 监控命令："
echo "  查看实时日志: ssh -i $SSH_KEY root@$ECS_IP 'journalctl -u hft-collector -f'"
echo "  查看服务状态: ssh -i $SSH_KEY root@$ECS_IP 'systemctl status hft-collector'"
echo "  重启服务:     ssh -i $SSH_KEY root@$ECS_IP 'systemctl restart hft-collector'"
echo ""
echo "🔍 数据验证（5分钟后执行）："
echo "  ssh -i $SSH_KEY root@$ECS_IP 'clickhouse-client --host \"kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud\" --port 9440 --secure --user $CLICKHOUSE_USER --password \"$CLICKHOUSE_PASSWORD\" --database $DATABASE --query \"SELECT count(*) FROM binance_orderbook\"'"