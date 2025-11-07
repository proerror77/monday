#!/bin/bash
set -e

# ECS Docker 構建和部署腳本
# 在 ECS 上直接構建並運行修復版本的 HFT Collector

# ECS 配置
INSTANCE_IP="8.221.136.162"
SSH_KEY_PATH="/Users/proerror/Documents/monday/rust_hft/hft-collector-key.pem"

# ClickHouse Cloud 配置
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="default"
CLICKHOUSE_PASSWORD="${CLICKHOUSE_PASSWORD:-s9wECb~NGZPOE}"

# 收集器配置
EXCHANGE="binance"
TOP_LIMIT="20"
DEPTH_MODE="both"
DEPTH_LEVELS="20"
FLUSH_MS="2000"
LOB_MODE="both"

echo "🚀 開始在 ECS 上構建和部署修復版本的 HFT Collector..."
echo "📍 目標實例: $INSTANCE_IP"
echo "🔧 構建方式: ECS 直接構建"

# 1. 停止現有服務
echo "⏹️  停止現有服務..."
ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP "systemctl stop hft-collector-docker 2>/dev/null || true"

# 2. 上傳源碼和 Dockerfile
echo "📤 上傳修復版本源碼..."
tar czf hft-collector-fixed-src.tar.gz src/ Cargo.toml Cargo.lock Dockerfile.ecs
scp -i "$SSH_KEY_PATH" hft-collector-fixed-src.tar.gz root@$INSTANCE_IP:~/

# 3. 在 ECS 上構建 Docker 鏡像
echo "🐳 在 ECS 上構建 Docker 鏡像..."
ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP << 'EOF'
    # 清理舊文件
    rm -rf ~/hft-collector-build
    mkdir -p ~/hft-collector-build
    cd ~/hft-collector-build

    # 解壓源碼
    tar xzf ~/hft-collector-fixed-src.tar.gz

    # 停止並移除舊容器和鏡像
    docker stop hft-collector 2>/dev/null || true
    docker rm hft-collector 2>/dev/null || true
    docker rmi hft-collector:fixed-v2 2>/dev/null || true

    # 構建新鏡像
    echo "🔨 構建修復版本 Docker 鏡像..."
    docker build -f Dockerfile.ecs -t hft-collector:fixed-v2 .

    echo "✅ Docker 鏡像構建完成"
    docker images | grep hft-collector
EOF

# 4. 創建新的運行腳本
echo "⚙️ 創建新的運行腳本..."
cat << SCRIPT_EOF | ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP "tee /root/run-hft-collector-fixed.sh"
#!/bin/bash
# HFT Collector 修復版本運行腳本

# 停止現有容器
docker stop hft-collector 2>/dev/null || true
docker rm hft-collector 2>/dev/null || true

# 運行修復版本容器
docker run -d \\
  --name hft-collector \\
  --restart unless-stopped \\
  -e CLICKHOUSE_USER="$CLICKHOUSE_USER" \\
  -e CLICKHOUSE_PASSWORD="$CLICKHOUSE_PASSWORD" \\
  -e RUST_LOG=info \\
  hft-collector:fixed-v2 \\
  multi \\
  --exchange "$EXCHANGE" \\
  --top-limit "$TOP_LIMIT" \\
  --depth-mode "$DEPTH_MODE" \\
  --depth-levels "$DEPTH_LEVELS" \\
  --flush-ms "$FLUSH_MS" \\
  --lob-mode "$LOB_MODE" \\
  --ch-url "$CLICKHOUSE_URL" \\
  --database hft_db

echo "🚀 修復版本容器啟動完成"
echo "檢查容器狀態:"
docker ps | grep hft-collector
echo "查看日志:"
docker logs --tail 20 hft-collector
SCRIPT_EOF

# 5. 更新 systemd 服務
echo "⚙️ 更新 systemd 服務..."
cat << SYSTEMD_EOF | ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP "tee /etc/systemd/system/hft-collector-docker.service"
[Unit]
Description=HFT Collector Fixed Version Docker Container
Requires=docker.service
After=docker.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/root/run-hft-collector-fixed.sh
ExecStop=docker stop hft-collector
TimeoutStartSec=0

[Install]
WantedBy=multi-user.target
SYSTEMD_EOF

# 6. 啟動修復版本服務
echo "🔧 啟動修復版本服務..."
ssh -i "$SSH_KEY_PATH" root@$INSTANCE_IP << 'EOF'
    # 設置腳本權限
    chmod +x /root/run-hft-collector-fixed.sh

    # 重新載入 systemd
    systemctl daemon-reload

    # 啟動服務
    systemctl start hft-collector-docker

    # 等待服務啟動
    sleep 10

    # 檢查服務狀態
    systemctl status hft-collector-docker --no-pager

    # 檢查容器狀態
    echo ""
    echo "Docker 容器狀態:"
    docker ps | grep hft-collector

    # 檢查日志
    echo ""
    echo "最新日志:"
    docker logs --tail 30 hft-collector
EOF

# 7. 清理本地臨時文件
rm -f hft-collector-fixed-src.tar.gz

echo ""
echo "✅ 修復版本構建和部署完成！"
echo ""
echo "🔍 期貨符號分類已修復："
echo "   - 只有明確的期貨格式 (如 BTCUSDT_241227) 才會被歸類為期貨"
echo "   - 使用白名單方式識別重要現貨交易對"
echo "   - 所有通過現貨端點的數據都正確歸類為現貨"
echo ""
echo "📊 監控命令："
echo "   ssh -i $SSH_KEY_PATH root@$INSTANCE_IP"
echo "   docker logs -f hft-collector               # 實時日志"
echo "   docker ps                                   # 容器狀態"
echo "   systemctl status hft-collector-docker      # 服務狀態"
echo ""
echo "🎯 修復版本現在正在運行，期貨數據分類問題已解決！"