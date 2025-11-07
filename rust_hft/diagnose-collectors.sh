#!/bin/bash
# 快速诊断ClickHouse数据收集状态

set -euo pipefail

ECS_IP="${ECS_IP:-8.216.64.241}"
SSH_KEY="hft-collector-key.pem"

echo "🔍 HFT Collector 诊断工具"
echo "="*80
echo ""

# 检查SSH连接
if ! ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no -o ConnectTimeout=5 root@$ECS_IP "echo ''" 2>/dev/null; then
    echo "❌ 无法连接到ECS: $ECS_IP"
    echo "请检查:"
    echo "  1. IP地址是否正确"
    echo "  2. SSH密钥是否有效"
    echo "  3. 安全组是否允许SSH(22)"
    exit 1
fi

echo "✅ ECS连接正常"
echo ""

# 检查systemd服务
echo "📊 Systemd服务状态"
echo "-"*80

ssh -i "$SSH_KEY" root@$ECS_IP 'bash -s' << 'REMOTE_SCRIPT'
for service in hft-collector hft-collector-binance hft-collector-binance-futures hft-collector-bitget hft-collector-bybit hft-collector-hyperliquid; do
    if systemctl list-units --all | grep -q "$service.service"; then
        status=$(systemctl is-active $service 2>/dev/null || echo "inactive")
        if [[ "$status" == "active" ]]; then
            echo "✅ $service: 运行中"
        else
            echo "❌ $service: $status"
        fi
    fi
done
REMOTE_SCRIPT

echo ""
echo "📝 最近日志 (每个服务最后3行)"
echo "-"*80

ssh -i "$SSH_KEY" root@$ECS_IP 'bash -s' << 'REMOTE_SCRIPT'
for service in hft-collector hft-collector-binance hft-collector-binance-futures hft-collector-bitget hft-collector-bybit hft-collector-hyperliquid; do
    if systemctl list-units --all | grep -q "$service.service"; then
        echo ""
        echo "=== $service ==="
        journalctl -u $service -n 3 --no-pager 2>/dev/null | tail -3 || echo "无日志"
    fi
done
REMOTE_SCRIPT

echo ""
echo "💾 进程检查"
echo "-"*80
ssh -i "$SSH_KEY" root@$ECS_IP "ps aux | grep '[h]ft-collector' | awk '{print \$2, \$11, \$12, \$13, \$14, \$15}'"

echo ""
echo "📊 ClickHouse 数据检查 (需要等待数据收集)"
echo "-"*80
echo "请在5分钟后运行以下命令检查数据："
echo ""
echo "python3 << 'EOF'
import clickhouse_connect
from datetime import datetime

client = clickhouse_connect.get_client(
    host='kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud',
    port=8443,
    database='hft_db',
    secure=True,
    username='default',
    password='s9wECb~NGZPOE'
)

tables = {
    'Binance Spot': ['binance_trades', 'binance_l1', 'binance_orderbook'],
    'Binance Futures': ['binance_futures_trades', 'binance_futures_orderbook'],
    'Bitget': ['bitget_trades', 'bitget_futures_trades'],
    'Bybit': ['bybit_trades', 'bybit_orderbook'],
    'Hyperliquid': ['hyperliquid_trades']
}

print(f\"\n{'='*80}\")
print(f\"ClickHouse 数据健康检查 - {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\")
print(f\"{'='*80}\n\")

for section, table_list in tables.items():
    print(f\"### {section} ###\")
    for table in table_list:
        try:
            result = client.query(f'''
                SELECT
                    count(*) as cnt,
                    max(ts) as latest,
                    dateDiff('second', max(ts), now()) as delay_sec
                FROM {table}
                WHERE ts >= now() - INTERVAL 5 MINUTE
            ''').result_rows

            if result:
                cnt, latest, delay = result[0]
                status = '✅' if delay < 60 else '⚠️' if delay < 300 else '❌'
                print(f\"{status} {table:30s} | 5min: {cnt:>8,} 条 | 延迟: {delay}s\")
        except Exception as e:
            print(f\"❌ {table:30s} | 错误: {str(e)[:40]}\")
    print()

print(f\"{'='*80}\")
print(\"\n图例: ✅ 正常(<60s) | ⚠️ 轻微延迟 | ❌ 严重延迟/停滞\")
EOF"
