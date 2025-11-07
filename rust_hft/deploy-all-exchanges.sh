#!/bin/bash
set -euo pipefail

echo "🚀 HFT Collector - 部署所有交易所"

# 配置参数
ECS_IP="${ECS_IP:-8.216.64.241}"  # 更新为正确的IP
SSH_KEY="hft-collector-key.pem"
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="default"
CLICKHOUSE_PASSWORD="s9wECb~NGZPOE"
DATABASE="hft_db"

echo "📋 配置信息："
echo "  ECS IP: $ECS_IP"
echo "  部署交易所: Binance(Spot+Futures), Bitget(Spot+Futures), Bybit, Hyperliquid"

# 编译所有交易所collector
echo "📦 编译所有交易所collector..."
cd apps/collector
cargo build --release \
    --features "collector-binance,collector-binance-futures,collector-bitget,collector-bybit,collector-hyperliquid"
cd ../..

BINARY_PATH="$(pwd)/target/release/hft-collector"
echo "✅ 编译完成: $(ls -lh "$BINARY_PATH" | awk '{print $5}')"

# SSH连接函数
run_remote() {
    ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i "$SSH_KEY" "root@$ECS_IP" "$1"
}

upload_file() {
    scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i "$SSH_KEY" "$1" "root@$ECS_IP:$2"
}

# 测试连接
echo "🔗 测试ECS连接..."
if ! run_remote "echo 'SSH连接成功'" 2>/dev/null; then
    echo "❌ SSH连接失败，IP可能不正确: $ECS_IP"
    exit 1
fi

# 上传二进制
echo "📤 上传collector..."
upload_file "$BINARY_PATH" "/root/hft-collector-new"
run_remote "chmod +x /root/hft-collector-new"

# 创建多个systemd服务
echo "⚙️  创建systemd服务配置..."

# 1. Binance Spot
run_remote "cat > /etc/systemd/system/hft-collector-binance.service << 'EOF'
[Unit]
Description=HFT Collector - Binance Spot
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector-new multi --exchange binance --ch-url $CLICKHOUSE_URL --database $DATABASE --top-limit 20 --batch-size 2000 --flush-ms 1000
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-binance

Environment=RUST_LOG=info
Environment=CLICKHOUSE_USER=$CLICKHOUSE_USER
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF"

# 2. Binance Futures
run_remote "cat > /etc/systemd/system/hft-collector-binance-futures.service << 'EOF'
[Unit]
Description=HFT Collector - Binance Futures
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector-new multi --exchange binance_futures --ch-url $CLICKHOUSE_URL --database $DATABASE --top-limit 20 --batch-size 2000 --flush-ms 1000
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-binance-futures

Environment=RUST_LOG=info
Environment=CLICKHOUSE_USER=$CLICKHOUSE_USER
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF"

# 3. Bitget Spot
run_remote "cat > /etc/systemd/system/hft-collector-bitget.service << 'EOF'
[Unit]
Description=HFT Collector - Bitget Spot
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector-new multi --exchange bitget --ch-url $CLICKHOUSE_URL --database $DATABASE --top-limit 20 --batch-size 2000 --flush-ms 1000
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-bitget

Environment=RUST_LOG=info
Environment=CLICKHOUSE_USER=$CLICKHOUSE_USER
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF"

# 4. Bybit
run_remote "cat > /etc/systemd/system/hft-collector-bybit.service << 'EOF'
[Unit]
Description=HFT Collector - Bybit
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector-new multi --exchange bybit --ch-url $CLICKHOUSE_URL --database $DATABASE --top-limit 20 --batch-size 2000 --flush-ms 1000
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-bybit

Environment=RUST_LOG=info
Environment=CLICKHOUSE_USER=$CLICKHOUSE_USER
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF"

# 5. Hyperliquid
run_remote "cat > /etc/systemd/system/hft-collector-hyperliquid.service << 'EOF'
[Unit]
Description=HFT Collector - Hyperliquid
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root
ExecStart=/root/hft-collector-new multi --exchange hyperliquid --ch-url $CLICKHOUSE_URL --database $DATABASE --top-limit 20 --batch-size 2000 --flush-ms 1000
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-hyperliquid

Environment=RUST_LOG=info
Environment=CLICKHOUSE_USER=$CLICKHOUSE_USER
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF"

# 部署
echo "🔄 停止旧服务..."
run_remote "systemctl stop hft-collector-binance 2>/dev/null || true"
run_remote "systemctl stop hft-collector-binance-futures 2>/dev/null || true"
run_remote "systemctl stop hft-collector-bitget 2>/dev/null || true"
run_remote "systemctl stop hft-collector-bybit 2>/dev/null || true"
run_remote "systemctl stop hft-collector-hyperliquid 2>/dev/null || true"
run_remote "systemctl stop hft-collector 2>/dev/null || true"  # 停止旧的单一服务

echo "📥 部署新版本..."
run_remote "mv /root/hft-collector-new /root/hft-collector-new-$(date +%s).backup 2>/dev/null || true"
run_remote "mv /root/hft-collector-new /root/hft-collector"

echo "▶️  启动所有服务..."
run_remote "systemctl daemon-reload"

# 启用并启动所有服务
for service in binance binance-futures bitget bybit hyperliquid; do
    echo "  启动 $service..."
    run_remote "systemctl enable hft-collector-$service"
    run_remote "systemctl start hft-collector-$service"
done

# 等待服务启动
echo "⏳ 等待服务启动..."
sleep 5

# 验证所有服务状态
echo ""
echo "="*80
echo "📊 服务状态检查"
echo "="*80

for service in binance binance-futures bitget bybit hyperliquid; do
    echo ""
    echo "=== hft-collector-$service ==="
    run_remote "systemctl is-active hft-collector-$service && echo '✅ 运行中' || echo '❌ 未运行'"
    run_remote "journalctl -u hft-collector-$service -n 3 --no-pager | tail -3"
done

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 监控命令："
echo "  查看所有服务: ssh -i $SSH_KEY root@$ECS_IP 'systemctl status hft-collector-*'"
echo "  查看Binance日志: ssh -i $SSH_KEY root@$ECS_IP 'journalctl -u hft-collector-binance -f'"
echo "  重启所有服务: ssh -i $SSH_KEY root@$ECS_IP 'systemctl restart hft-collector-*'"
echo ""
echo "🔍 5分钟后验证数据："
echo "  检查ClickHouse数据入库情况"
