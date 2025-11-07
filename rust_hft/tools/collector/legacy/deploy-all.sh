#!/bin/bash
set -e

# 批量部署所有交易所收集器到阿里云 ECS

INSTANCE_ID="i-6we3w3olqr8ordj4z5nk"
EXCHANGES=("binance" "bybit" "asterdex" "hyperliquid")

echo "🚀 开始批量部署多交易所收集器到 ECS: $INSTANCE_ID"
echo "📋 计划部署的交易所: ${EXCHANGES[@]}"
echo ""

# 逐个部署每个交易所
for exchange in "${EXCHANGES[@]}"; do
    echo "📦 正在部署 $exchange 收集器..."

    if ./deploy-exchange.sh "$exchange" "$INSTANCE_ID"; then
        echo "✅ $exchange 收集器部署成功"
        echo ""

        # 等待几秒钟让服务启动
        sleep 5
    else
        echo "❌ $exchange 收集器部署失败"
        echo ""

        # 可以选择继续或停止
        read -p "是否继续部署下一个交易所？(y/n): " choice
        case "$choice" in
            n|N ) echo "停止批量部署"; exit 1;;
            * ) echo "继续部署...";;
        esac
    fi
done

echo ""
echo "🎉 批量部署完成！"
echo ""
echo "📊 查看所有服务状态的命令:"
for exchange in "${EXCHANGES[@]}"; do
    echo "  # $exchange 服务"
    echo "  aliyun ecs RunCommand --RegionId ap-northeast-1 --Type RunShellScript --CommandContent 'systemctl status ${exchange}-collector' --InstanceId.1 '$INSTANCE_ID' --Name Check${exchange^}"
done

echo ""
echo "🔍 查看所有服务日志的命令:"
echo "aliyun ecs RunCommand --RegionId ap-northeast-1 --Type RunShellScript --CommandContent '"
echo "  echo \"=== 所有收集器服务状态 ===\" && \\"
echo "  systemctl list-units --type=service | grep collector && \\"
echo "  echo \"\" && \\"
echo "  echo \"=== 最新日志汇总 ===\" && \\"
for exchange in "${EXCHANGES[@]}"; do
    echo "  echo \"--- ${exchange} 收集器日志 ---\" && \\"
    echo "  journalctl -u ${exchange}-collector -n 5 --no-pager && \\"
    echo "  echo \"\" && \\"
done
echo "  echo \"完成\"' --InstanceId.1 '$INSTANCE_ID' --Name CheckAllServices"

echo ""
echo "🌐 数据流向架构："
echo "  ┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐"
echo "  │   多个交易所     │────▶│  阿里云 ECS       │────▶│ AWS ClickHouse  │"
echo "  │                │    │  (8.216.39.177)  │    │      Cloud      │"
echo "  │ • Binance      │    │                  │    │                 │"
echo "  │ • Bybit        │    │  统一收集器×N     │    │   hft_db        │"
echo "  │ • Asterdex     │    │  (每交易所独立)    │    │                 │"
echo "  │ • Hyperliquid  │    │                  │    │                 │"
echo "  └─────────────────┘    └──────────────────┘    └─────────────────┘"
echo ""
echo "🎯 所有交易所收集器现在都在 24/7 收集数据！"