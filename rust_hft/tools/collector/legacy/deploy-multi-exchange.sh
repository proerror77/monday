#!/bin/bash
set -e

# 全面多交易所 HFT 数据收集器部署脚本
# 支持: Binance 现货+期货, Bitget, Asterdex, Bybit, Hyperliquid

# ECS 配置
INSTANCE_IP="8.221.136.162"
SSH_KEY_PATH="/Users/proerror/Documents/monday/rust_hft/hft-collector-key.pem"

# ClickHouse Cloud 配置
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="default"
CLICKHOUSE_PASSWORD="${CLICKHOUSE_PASSWORD:-s9wECb~NGZPOE}"

# 数据收集配置
TOP_LIMIT="20"
DEPTH_MODE="both"
DEPTH_LEVELS="20"
FLUSH_MS="2000"
LOB_MODE="both"

echo "🚀 开始部署全面多交易所 HFT 数据收集系统..."
echo "📍 目标实例: $INSTANCE_IP"
echo "🏗️ 支持交易所: Binance(现货+期货), Bitget, Asterdex, Bybit, Hyperliquid"

# 1. 停止现有服务
echo "⏹️ 停止现有服务..."
ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP "
    systemctl stop hft-collector-docker 2>/dev/null || true
    docker stop \$(docker ps -q --filter 'name=hft-') 2>/dev/null || true
    docker rm \$(docker ps -aq --filter 'name=hft-') 2>/dev/null || true
"

# 2. 上传修复后的源码
echo "📤 上传全功能源码..."
tar czf hft-collector-multi-exchange.tar.gz src/ Cargo.toml Cargo.lock Dockerfile.ecs
scp -i "$SSH_KEY_PATH" hft-collector-multi-exchange.tar.gz root@$INSTANCE_IP:~/

# 3. 在 ECS 上构建全功能 Docker 镜像
echo "🐳 构建全功能 Docker 镜像..."
ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP << 'EOF'
    # 清理并准备构建环境
    rm -rf ~/hft-multi-build
    mkdir -p ~/hft-multi-build
    cd ~/hft-multi-build

    # 解压源码
    tar xzf ~/hft-collector-multi-exchange.tar.gz

    # 删除旧镜像
    docker rmi hft-collector:multi-full 2>/dev/null || true

    # 构建全功能镜像（支持所有交易所）
    echo "🔨 构建支持全交易所的 Docker 镜像..."
    docker build -f Dockerfile.ecs -t hft-collector:multi-full .

    echo "✅ 全功能 Docker 镜像构建完成"
    docker images | grep hft-collector
EOF

# 4. 创建多交易所运行脚本
echo "⚙️ 创建多交易所运行脚本..."
cat << 'SCRIPT_EOF' | ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP "tee /root/run-multi-exchange.sh"
#!/bin/bash
# 多交易所 HFT 数据收集器运行脚本

# 基础配置
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="default"
CLICKHOUSE_PASSWORD="s9wECb~NGZPOE"
DATABASE="hft_db"
TOP_LIMIT="20"

# 停止所有现有容器
echo "🛑 停止现有容器..."
docker stop $(docker ps -q --filter 'name=hft-') 2>/dev/null || true
docker rm $(docker ps -aq --filter 'name=hft-') 2>/dev/null || true

# 定义要启动的交易所收集器
declare -a EXCHANGES=(
    "binance"
    "binance_futures"
    "bitget"
    "asterdex"
    "bybit"
    "hyperliquid"
)

# 启动每个交易所的收集器
for exchange in "${EXCHANGES[@]}"; do
    echo "🚀 启动 $exchange 数据收集器..."

    docker run -d \
        --name "hft-${exchange}" \
        --restart unless-stopped \
        -e CLICKHOUSE_USER="$CLICKHOUSE_USER" \
        -e CLICKHOUSE_PASSWORD="$CLICKHOUSE_PASSWORD" \
        -e RUST_LOG=info \
        hft-collector:multi-full \
        multi \
        --exchange "$exchange" \
        --top-limit "$TOP_LIMIT" \
        --depth-mode "both" \
        --depth-levels "20" \
        --flush-ms "2000" \
        --lob-mode "both" \
        --ch-url "$CLICKHOUSE_URL" \
        --database "$DATABASE"

    # 等待容器启动
    sleep 2
done

echo ""
echo "✅ 多交易所数据收集器启动完成"
echo ""
echo "📊 运行状态:"
docker ps --filter 'name=hft-' --format 'table {{.Names}}\t{{.Status}}\t{{.Ports}}'

echo ""
echo "📝 检查各交易所日志:"
for exchange in "${EXCHANGES[@]}"; do
    echo "--- $exchange 最新日志 ---"
    docker logs --tail 5 "hft-${exchange}" 2>/dev/null || echo "容器未运行"
    echo ""
done

SCRIPT_EOF

# 5. 创建 systemd 服务（管理多交易所容器）
echo "⚙️ 创建多交易所 systemd 服务..."
cat << 'SYSTEMD_EOF' | ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP "tee /etc/systemd/system/hft-multi-exchange.service"
[Unit]
Description=HFT Multi-Exchange Data Collector
Requires=docker.service
After=docker.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/root/run-multi-exchange.sh
ExecStop=/bin/bash -c 'docker stop $(docker ps -q --filter "name=hft-") || true'
TimeoutStartSec=120

[Install]
WantedBy=multi-user.target
SYSTEMD_EOF

# 6. 启动多交易所服务
echo "🔧 启动多交易所数据收集服务..."
ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP << 'EOF'
    # 设置脚本权限
    chmod +x /root/run-multi-exchange.sh

    # 重新载入 systemd
    systemctl daemon-reload

    # 启用开机自启动
    systemctl enable hft-multi-exchange

    # 启动服务
    systemctl start hft-multi-exchange

    # 等待服务启动
    sleep 15

    # 检查服务状态
    systemctl status hft-multi-exchange --no-pager

    echo ""
    echo "🐳 Docker 容器状态:"
    docker ps --filter 'name=hft-'
EOF

# 7. 验证部署
echo ""
echo "📊 验证多交易所部署..."

# 检查所有容器状态
echo "🔍 检查容器运行状态..."
ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP "
    echo '=== 容器状态 ==='
    docker ps --filter 'name=hft-' --format 'table {{.Names}}\t{{.Status}}\t{{.CreatedAt}}'

    echo ''
    echo '=== 各交易所最新日志 ==='
    for container in \$(docker ps --filter 'name=hft-' --format '{{.Names}}'); do
        echo \"--- \$container ---\"
        docker logs --tail 3 \"\$container\" 2>/dev/null || echo \"无日志\"
        echo \"\"
    done
"

# 8. 清理本地临时文件
rm -f hft-collector-multi-exchange.tar.gz

echo ""
echo "✅ 多交易所 HFT 数据收集系统部署完成！"
echo ""
echo "🎯 支持的交易所："
echo "   • Binance 现货 (全量订单簿 + 交易 + Ticker)"
echo "   • Binance 期货 (全量订单簿 + 交易 + Ticker)"
echo "   • Bitget (全量数据)"
echo "   • Asterdex (全量数据)"
echo "   • Bybit (全量数据)"
echo "   • Hyperliquid (全量数据)"
echo ""
echo "📊 监控命令："
echo "   ssh -i $SSH_KEY_PATH root@$INSTANCE_IP"
echo "   docker ps --filter 'name=hft-'              # 容器状态"
echo "   docker logs -f hft-binance                   # Binance 实时日志"
echo "   docker logs -f hft-bitget                    # Bitget 实时日志"
echo "   systemctl status hft-multi-exchange          # 服务状态"
echo ""
echo "🔧 管理命令："
echo "   systemctl restart hft-multi-exchange         # 重启所有收集器"
echo "   systemctl stop hft-multi-exchange            # 停止所有收集器"
echo "   docker restart hft-binance                   # 重启单个收集器"
echo ""
echo "🎯 现在开始从 6 个交易所全面收集数据到 ClickHouse Cloud！"