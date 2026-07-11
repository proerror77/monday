#!/bin/bash
set -euo pipefail

echo "🔧 HFT Collector 修復部署腳本"

# 檢查當前目錄
if [[ ! -f "apps/collector/Cargo.toml" ]]; then
    echo "❌ 請在 rust_hft 根目錄運行此腳本"
    exit 1
fi

# 設置變數
ECS_HOST="47.128.222.180"  # 可能需要更新
SSH_KEY="${SSH_KEY:-$HOME/.ssh/hft-admin-ssh.pem}"
BINARY_NAME="hft-collector"
SERVICE_NAME="hft-collector"
CLICKHOUSE_HOST="${CLICKHOUSE_HOST:-kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud}"
CLICKHOUSE_PORT="${CLICKHOUSE_PORT:-9440}"
CLICKHOUSE_USER="${CLICKHOUSE_USER:-default}"
CLICKHOUSE_PASSWORD="${CLICKHOUSE_PASSWORD:-}"

echo "📦 編譯修復版本..."
pushd apps/collector >/dev/null
cargo build --release
BIN_PATH="$(pwd)/target/release/$BINARY_NAME"
popd >/dev/null

echo "🚀 部署到 ECS..."

# 函數：執行遠程命令
run_remote() {
    ssh -o StrictHostKeyChecking=no -i "$SSH_KEY" "root@$ECS_HOST" "$1"
}

# 上傳二進制文件
echo "📤 上傳二進制文件: $BIN_PATH"
scp -o StrictHostKeyChecking=no -i "$SSH_KEY" \
    "$BIN_PATH" \
    "root@$ECS_HOST:/root/hft-collector-fixed"

# 停止舊服務
echo "⏹️  停止舊服務..."
run_remote "systemctl stop $SERVICE_NAME || true"

# 備份舊二進制文件
echo "💾 備份舊版本..."
run_remote "cp /root/hft-collector-real /root/hft-collector-backup-$(date +%s) || true"

# 替換二進制文件
echo "🔄 替換二進制文件..."
run_remote "mv /root/hft-collector-fixed /root/hft-collector-real"
run_remote "chmod +x /root/hft-collector-real"

# 啟動服務
echo "▶️  啟動修復版本服務..."
run_remote "systemctl start $SERVICE_NAME"

# 檢查狀態
echo "📊 檢查服務狀態..."
run_remote "systemctl status $SERVICE_NAME --no-pager -l"

echo "📊 最新日誌:"
run_remote "journalctl -u $SERVICE_NAME -n 10 --no-pager"

echo "✅ 修復版本部署完成!"
echo "💡 使用以下命令監控日誌:"
echo "   ssh -i $SSH_KEY root@$ECS_HOST 'journalctl -u $SERVICE_NAME -f'"

# 檢查 ClickHouse 數據
echo "🔍 檢查 ClickHouse 表..."
if [[ -n "$CLICKHOUSE_PASSWORD" ]]; then
    run_remote "clickhouse-client --host '$CLICKHOUSE_HOST' --port '$CLICKHOUSE_PORT' --secure --user '$CLICKHOUSE_USER' --password '$CLICKHOUSE_PASSWORD' --database hft_db --query 'SELECT count(*) as binance_orderbook_count FROM binance_orderbook' || echo '表檢查失敗'"
else
    echo "⚠️  跳過 ClickHouse 表檢查；請設置 CLICKHOUSE_PASSWORD"
fi
