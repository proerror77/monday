#!/usr/bin/env bash
set -euo pipefail

# Deploy tools/collector to an Aliyun ECS and launch multiple systemd services
# covering binance (spot+futures), bitget (spot+futures), and asterdex (spot-like).
#
# Requirements:
# - SSH access to ECS as root with a PEM key
# - ClickHouse Cloud URL/user/password
#
# Usage:
#   ECS_IP=1.2.3.4 SSH_KEY=hft-collector-key.pem \
#   CH_URL=https://...:8443 CH_USER=default CH_PASSWORD='***' CH_DB=hft_db \
#   ./scripts/deploy-ecs-tools-collector.sh

ECS_IP=${ECS_IP:-}
SSH_KEY=${SSH_KEY:-hft-collector-key.pem}
CH_URL=${CH_URL:-"https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"}
CH_USER=${CH_USER:-default}
CH_PASSWORD=${CH_PASSWORD:-}
CH_DB=${CH_DB:-hft_db}

[ -n "$ECS_IP" ] || { echo "ECS_IP is required" >&2; exit 1; }
[ -f "$SSH_KEY" ] || { echo "SSH key not found: $SSH_KEY" >&2; exit 1; }

run_remote() { ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i "$SSH_KEY" "root@$ECS_IP" "$1"; }
upload_file() { scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 -i "$SSH_KEY" "$1" "root@$ECS_IP:$2"; }

echo "== Building collector (cross-compile for Linux) =="
if command -v docker &> /dev/null; then
  # Use Docker cross-compilation for Linux target
  docker run --rm -v "$(pwd):/workspace" -w /workspace rust:1.75 bash -c "
    cd tools/collector &&
    cargo build --release --target x86_64-unknown-linux-gnu
  " || {
    echo "Docker build failed, trying local build..." >&2
    pushd tools/collector >/dev/null
    cargo build --release
    popd >/dev/null
  }
  BIN=tools/collector/target/x86_64-unknown-linux-gnu/release/hft-collector
  [ -f "$BIN" ] || BIN=tools/collector/target/release/hft-collector
else
  # Fallback to local build (may not work on ECS if architecture mismatch)
  pushd tools/collector >/dev/null
  cargo build --release
  popd >/dev/null
  BIN=tools/collector/target/release/hft-collector
fi
[ -f "$BIN" ] || { echo "Build failed: $BIN not found" >&2; exit 1; }

# Whitelist base tokens (prefer config/symbol_whitelist.json if present)
BASE=()
if [ -f config/symbol_whitelist.json ]; then
  while IFS= read -r line; do
    line=$(echo "$line" | sed 's/^\s*//; s/\s*$//' | tr 'a-z' 'A-Z')
    [ -n "$line" ] && BASE+=("$line")
  done < <(jq -r '.base_tokens[]? // empty' config/symbol_whitelist.json | sed 's/^\s*//; s/\s*$//')
fi
if [ ${#BASE[@]} -eq 0 ]; then
  BASE=(BTC ETH SOL SUI XRP XLM HYPE ASTER PUMP DOGE AVAX WLFI ADA ENA ONDO SEI MERL B BNB BGB OKB)
fi

# Compose symbol lists per exchange
join_by() { local IFS=","; echo "$*"; }

BINANCE_SPOT=()
BINANCE_FUT=()
BITGET_SPOT=()
BITGET_FUT=()
ASTERDEX=()
for t in "${BASE[@]}"; do
  BINANCE_SPOT+=("${t}USDT")
  BINANCE_FUT+=("${t}USDT")
  BITGET_SPOT+=("${t}USDT:SPOT")
  BITGET_FUT+=("${t}USDT:USDT-FUTURES")
  ASTERDEX+=("${t}USDT")
done

SYMS_BINANCE_SPOT=$(join_by "${BINANCE_SPOT[@]}")
SYMS_BINANCE_FUT=$(join_by "${BINANCE_FUT[@]}")
SYMS_BITGET_ALL=$(join_by "${BITGET_SPOT[@]}" "${BITGET_FUT[@]}")
SYMS_ASTERDEX=$(join_by "${ASTERDEX[@]}")

echo "== Installing runtime deps & uploading binary =="
run_remote "apt-get update && apt-get install -y ca-certificates >/dev/null 2>&1 || true"
upload_file "$BIN" "/usr/local/bin/hft-collector"
run_remote "chmod +x /usr/local/bin/hft-collector"

echo "== Writing common env file =="
run_remote "bash -lc 'cat > /etc/hft-collector.env << EOF
CH_URL=$CH_URL
CH_DATABASE=$CH_DB
CLICKHOUSE_USER=$CH_USER
CLICKHOUSE_PASSWORD=$CH_PASSWORD
BATCH_SIZE=1000
FLUSH_MS=2000
# Spool (keep small)
COLLECTOR_SPOOL_DIR=/var/lib/hft-collector/spool
COLLECTOR_SPOOL_MAX_BYTES=67108864
COLLECTOR_SPOOL_DRAIN_LIMIT=5
COLLECTOR_SPOOL_TTL_SECS=21600
EOF'
mkdir -p /var/lib/hft-collector/spool
chmod 600 /etc/hft-collector.env"

make_service() {
  local name=$1; shift
  local cmdline=$*
  run_remote "bash -lc 'cat > /etc/systemd/system/${name}.service << EOF
[Unit]
Description=${name}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=/etc/hft-collector.env
ExecStart=/usr/local/bin/hft-collector ${cmdline}
Restart=always
RestartSec=5
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload'"
}

echo "== Creating services =="
# Binance Spot
make_service hft-collector-binance-spot \
  --exchanges binance \
  --symbols "$SYMS_BINANCE_SPOT" \
  --ch-url "$CH_URL" \
  --database "$CH_DB" \
  --batch-size 1000 \
  --flush-ms 2000

# Binance Futures
make_service hft-collector-binance-futures \
  --exchanges binance_futures \
  --symbols "$SYMS_BINANCE_FUT" \
  --ch-url "$CH_URL" \
  --database "$CH_DB" \
  --batch-size 1000 \
  --flush-ms 2000

# Bitget (spot + futures in one process via instType suffix)
make_service hft-collector-bitget \
  --exchanges bitget \
  --symbols "$SYMS_BITGET_ALL" \
  --ch-url "$CH_URL" \
  --database "$CH_DB" \
  --batch-size 1000 \
  --flush-ms 2000

# Asterdex (spot-like)
make_service hft-collector-asterdex \
  --exchanges asterdex \
  --symbols "$SYMS_ASTERDEX" \
  --ch-url "$CH_URL" \
  --database "$CH_DB" \
  --batch-size 1000 \
  --flush-ms 2000

echo "== Enabling & starting services =="
run_remote "systemctl enable hft-collector-binance-spot hft-collector-binance-futures hft-collector-bitget hft-collector-asterdex"
run_remote "systemctl restart hft-collector-binance-spot hft-collector-binance-futures hft-collector-bitget hft-collector-asterdex"

echo "== Status tail =="
run_remote "for s in binance-spot binance-futures bitget asterdex; do echo ==== \$s ====; systemctl is-active hft-collector-\$s || true; journalctl -u hft-collector-\$s -n 20 --no-pager || true; done"

echo "Done. Validate ClickHouse after a few minutes."
