#!/bin/bash
set -euo pipefail

# 基本配置（可用環境變數覆寫）
INSTANCE_IP="${INSTANCE_IP:-47.245.9.164}"
INSTANCE_ID="${INSTANCE_ID:-i-6we8ptdjxqxifkvd8ni1}"
REMOTE_USER="${REMOTE_USER:-ubuntu}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/hft-collector-key.pem}"
BINARY_PATH="${BINARY_PATH:-./target/release/hft-collector}"
WHITELIST_LOCAL="${WHITELIST_LOCAL:-config/symbol_whitelist.json}"
CLICKHOUSE_URL="${CLICKHOUSE_URL:-https://n8xmym7qxq.ap-northeast-1.aws.clickhouse.cloud:8443}"

if [[ "$REMOTE_USER" == "root" ]]; then
  SUDO=""
  HOME_DIR="/root"
else
  SUDO="sudo"
  HOME_DIR="/home/$REMOTE_USER"
fi

# 讀取 ClickHouse 憑證（優先使用新憑證文件）
if [ -f "$HOME/Downloads/clickhouse_credentials (1).txt" ]; then
    CLICKHOUSE_PASSWORD=$(grep "Password:" "$HOME/Downloads/clickhouse_credentials (1).txt" | cut -d' ' -f2 | tr -d '\n\r')
elif [ -f "clickhouse_credentials.txt" ]; then
    CLICKHOUSE_PASSWORD=$(grep "Password:" clickhouse_credentials.txt | cut -d' ' -f2 | tr -d '\n\r')
else
    echo "❌ 找不到 ClickHouse 憑證文件"
    exit 1
fi

echo "🚀 開始部署 HFT Collector 到阿里雲 ECS (Ubuntu) ..."
echo "📍 目標實例: $INSTANCE_ID ($INSTANCE_IP), 使用者: $REMOTE_USER"
echo "🗄️  ClickHouse: $CLICKHOUSE_URL"

# 檢查二進制文件
if [ ! -f "$BINARY_PATH" ]; then
    echo "🛠️  未找到二進制，開始本地編譯..."
    cargo build -p hft-collector --release
fi

# 等待 SSH 就緒
echo "⏳ 等待 SSH 服務就緒..."
for i in {1..30}; do
    if ssh -i "$SSH_KEY" -o ConnectTimeout=5 -o StrictHostKeyChecking=no "$REMOTE_USER"@$INSTANCE_IP "echo 'SSH OK'" 2>/dev/null; then
        echo "✅ SSH 連接成功"
        break
    fi
    echo "   嘗試 $i/30..."
    sleep 5
done

# 1. 上傳二進制
echo "📦 上傳二進制文件與白名單..."
scp -i "$SSH_KEY" -o StrictHostKeyChecking=no "$BINARY_PATH" "$REMOTE_USER"@$INSTANCE_IP:/tmp/hft-collector
if [[ -f "$WHITELIST_LOCAL" ]]; then
  scp -i "$SSH_KEY" -o StrictHostKeyChecking=no "$WHITELIST_LOCAL" "$REMOTE_USER"@$INSTANCE_IP:/tmp/symbol_whitelist.json || true
fi

# 2. 安裝依賴
echo "📋 安裝系統依賴..."
ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@$INSTANCE_IP bash -s << EOF
set -e
$SUDO apt-get update -qq
$SUDO apt-get install -y ca-certificates curl htop
$SUDO mkdir -p "$HOME_DIR"/config
$SUDO mv -f /tmp/hft-collector "$HOME_DIR"/hft-collector
$SUDO chmod +x "$HOME_DIR"/hft-collector
if [[ -f /tmp/symbol_whitelist.json ]]; then
  $SUDO mv -f /tmp/symbol_whitelist.json "$HOME_DIR"/config/symbol_whitelist.json
fi
$SUDO mkdir -p /var/log/hft-collector
EOF

# 3. 創建 systemd 服務
echo "⚙️ 創建 systemd 服務 (binance + bitget)..."

# Binance 服務
cat << SYSTEMD_EOF | ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@\$INSTANCE_IP "$SUDO tee /etc/systemd/system/hft-collector-binance.service > /dev/null"
[Unit]
Description=HFT Data Collector (Binance) - WS Splitted Per Symbol
After=network.target
Wants=network.target

[Service]
Type=simple
User=$REMOTE_USER
WorkingDirectory=$HOME_DIR
ExecStart=$HOME_DIR/hft-collector \\
  --exchanges binance \\
  --symbols BTCUSDT,ETHUSDT,SOLUSDT \\
  --ch-url $CLICKHOUSE_URL \\
  --database hft_db \\
  --batch-size 1000 \\
  --flush-ms 1500
Restart=always
RestartSec=8
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-collector-binance

Environment=CLICKHOUSE_USER=default
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
Environment=RUST_LOG=info
Environment=COLLECTOR_SPLIT_PROFILES=1
Environment=BINANCE_WS_SYMBOLS_PER_CONN=1
Environment=BINANCE_STREAM_PROFILE=all
Environment=SYMBOLS_FILE=$HOME_DIR/config/symbol_whitelist.json

LimitNOFILE=65536
LimitNPROC=32768
Nice=-5

[Install]
WantedBy=multi-user.target
SYSTEMD_EOF

# Bitget 服務
cat << SYSTEMD2_EOF | ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@\$INSTANCE_IP "$SUDO tee /etc/systemd/system/hft-collector-bitget.service > /dev/null"
[Unit]
Description=HFT Data Collector (Bitget) - WS Splitted Per Symbol
After=network.target
Wants=network.target

[Service]
Type=simple
User=$REMOTE_USER
WorkingDirectory=$HOME_DIR
ExecStart=$HOME_DIR/hft-collector \\
  --exchanges bitget \\
  --symbols BTCUSDT,ETHUSDT,SOLUSDT \\
  --ch-url $CLICKHOUSE_URL \\
  --database hft_db \\
  --batch-size 1000 \\
  --flush-ms 1500
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-collector-bitget

Environment=CLICKHOUSE_USER=default
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
Environment=RUST_LOG=info
Environment=COLLECTOR_SPLIT_PROFILES=1
Environment=BITGET_WS_SYMBOLS_PER_CONN=1
Environment=BITGET_STREAM_PROFILE=all
Environment=BITGET_MARKET=USDT-FUTURES
Environment=SYMBOLS_FILE=$HOME_DIR/config/symbol_whitelist.json

LimitNOFILE=65536
LimitNPROC=32768

Nice=-5

[Install]
WantedBy=multi-user.target
SYSTEMD2_EOF

# 4. 啟動服務
echo "🔧 啟動服務..."
ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@$INSTANCE_IP bash -s << EOF
set -e
$SUDO systemctl daemon-reload
$SUDO systemctl enable hft-collector-binance
$SUDO systemctl enable hft-collector-bitget
$SUDO systemctl restart hft-collector-binance || $SUDO systemctl start hft-collector-binance
$SUDO systemctl restart hft-collector-bitget || $SUDO systemctl start hft-collector-bitget
$SUDO systemctl enable hft-collector-binance
$SUDO systemctl enable hft-collector-bitget
$SUDO systemctl restart hft-collector-binance || $SUDO systemctl start hft-collector-binance
$SUDO systemctl restart hft-collector-bitget || $SUDO systemctl start hft-collector-bitget
sleep 2
$SUDO systemctl status hft-collector-binance --no-pager || true
$SUDO systemctl status hft-collector-bitget --no-pager || true
EOF

# Bybit 服務
echo "⚙️ 創建 systemd 服務 (bybit + asterdex)..."
cat << SYSTEMD3_EOF | ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@\$INSTANCE_IP "$SUDO tee /etc/systemd/system/hft-collector-bybit.service > /dev/null"
[Unit]
Description=HFT Data Collector (Bybit) - WS Splitted Per Symbol
After=network.target
Wants=network.target

[Service]
Type=simple
User=$REMOTE_USER
WorkingDirectory=$HOME_DIR
ExecStart=$HOME_DIR/hft-collector \\
  --exchanges bybit \\
  --symbols BTCUSDT,ETHUSDT,SOLUSDT \\
  --ch-url $CLICKHOUSE_URL \\
  --database hft_db \\
  --batch-size 1000 \\
  --flush-ms 1500
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-collector-bybit

Environment=CLICKHOUSE_USER=default
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
Environment=RUST_LOG=info
Environment=COLLECTOR_SPLIT_PROFILES=1
Environment=BYBIT_WS_SYMBOLS_PER_CONN=1
Environment=BYBIT_STREAM_PROFILE=all
Environment=SYMBOLS_FILE=$HOME_DIR/config/symbol_whitelist.json

LimitNOFILE=65536
LimitNPROC=32768
Nice=-5

[Install]
WantedBy=multi-user.target
SYSTEMD3_EOF

# Asterdex 服務
cat << SYSTEMD4_EOF | ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@\$INSTANCE_IP "$SUDO tee /etc/systemd/system/hft-collector-asterdex.service > /dev/null"
[Unit]
Description=HFT Data Collector (Asterdex) - WS Splitted Per Symbol
After=network.target
Wants=network.target

[Service]
Type=simple
User=$REMOTE_USER
WorkingDirectory=$HOME_DIR
ExecStart=$HOME_DIR/hft-collector \\
  --exchanges asterdex \\
  --symbols BTCUSDT,ETHUSDT,SOLUSDT \\
  --ch-url $CLICKHOUSE_URL \\
  --database hft_db \\
  --batch-size 1000 \\
  --flush-ms 1500
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-collector-asterdex

Environment=CLICKHOUSE_USER=default
Environment=CLICKHOUSE_PASSWORD=$CLICKHOUSE_PASSWORD
Environment=RUST_LOG=info
Environment=COLLECTOR_SPLIT_PROFILES=1
Environment=ASTERDEX_WS_SYMBOLS_PER_CONN=1
Environment=ASTERDEX_STREAM_PROFILE=all
Environment=SYMBOLS_FILE=$HOME_DIR/config/symbol_whitelist.json

LimitNOFILE=65536
LimitNPROC=32768
Nice=-5

[Install]
WantedBy=multi-user.target
SYSTEMD4_EOF

echo "🔧 啟動 Bybit & Asterdex 服務..."
ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@$INSTANCE_IP bash -s << EOF
set -e
$SUDO systemctl daemon-reload
$SUDO systemctl enable hft-collector-bybit
$SUDO systemctl enable hft-collector-asterdex
$SUDO systemctl restart hft-collector-bybit || $SUDO systemctl start hft-collector-bybit
$SUDO systemctl restart hft-collector-asterdex || $SUDO systemctl start hft-collector-asterdex
sleep 3
$SUDO systemctl status hft-collector-bybit --no-pager || true
$SUDO systemctl status hft-collector-asterdex --no-pager || true
EOF

# 5. 驗證
echo ""
echo "📊 服務狀態:"
ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@$INSTANCE_IP "$SUDO systemctl is-active hft-collector-binance || true; $SUDO systemctl is-active hft-collector-bitget || true"

echo ""
echo "📝 最新日誌:"
ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$REMOTE_USER"@$INSTANCE_IP "$SUDO journalctl -u hft-collector-binance -n 30 --no-pager; echo; $SUDO journalctl -u hft-collector-bitget -n 30 --no-pager; echo; $SUDO journalctl -u hft-collector-bybit -n 30 --no-pager; echo; $SUDO journalctl -u hft-collector-asterdex -n 30 --no-pager"

echo ""
echo "✅ 部署完成！"
echo ""
echo "📊 監控命令:"
echo "   ssh $REMOTE_USER@$INSTANCE_IP"
echo "   sudo journalctl -f -u hft-collector-binance   # Binance 實時日誌"
echo "   sudo journalctl -f -u hft-collector-bitget    # Bitget  實時日誌"
echo "   sudo systemctl status hft-collector-binance"
echo "   sudo systemctl status hft-collector-bitget"
echo "   sudo systemctl status hft-collector-bybit"
echo "   sudo systemctl status hft-collector-asterdex"
echo ""
echo "🌐 實例信息:"
echo "   公網 IP: $INSTANCE_IP"
echo "   實例 ID: $INSTANCE_ID"
echo "   規格: 2核2G (按量付費 ¥0.2432/小時)"
echo "   數據目標: AWS ClickHouse Cloud"
