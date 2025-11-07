#!/bin/bash
set -e

echo "🔧 修复Cargo.toml features配置"
echo "=============================="

# 备份原始文件
cp Cargo.toml Cargo.toml.backup

# 创建正确的Cargo.toml
cat > Cargo.toml << 'EOF'
[package]
name = "hft-collector"
version = "0.1.0"
edition = "2021"
description = "HFT数据收集器：支持现货和永续合约"

[dependencies]
# 核心异步运行时
tokio = { version = "1", default-features = false, features = ["rt-multi-thread", "macros", "time", "sync", "net"] }

# WebSocket - 使用rustls避免OpenSSL依赖
tokio-tungstenite = { version = "0.20", default-features = false, features = ["rustls-tls-webpki-roots", "connect"] }

# HTTP客户端
reqwest = { version = "0.11", default-features = false, features = ["json", "rustls-tls"] }

# ClickHouse客户端
clickhouse = { version = "0.12", default-features = false, features = ["rustls-tls"] }

# 异步
futures = "0.3"

# 序列化
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# CLI
clap = { version = "4", features = ["derive"] }

# 日志
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# 时间处理
chrono = { version = "0.4", features = ["serde"] }

# 数值处理
rust_decimal = { version = "1", features = ["serde"] }
ordered-float = "4"

# 错误处理
anyhow = "1"
async-trait = "0.1"

# URL编码
urlencoding = "2"

# 监控
prometheus = "0.13"
hyper = { version = "0.14", features = ["server", "http1"] }
once_cell = "1.19"

[features]
# 默认启用所有交易所
default = [
    "collector-binance",
    "collector-binance-futures",
    "collector-bitget",
    "collector-bybit",
    "collector-asterdex",
    "collector-hyperliquid"
]

# 交易所features
collector-binance = []           # 币安现货
collector-binance-futures = []   # 币安永续合约（USDT-M）
collector-bitget = []             # Bitget
collector-bybit = []              # Bybit
collector-hyperliquid = []        # Hyperliquid
collector-asterdex = []           # Asterdex

# 开发/测试features
dev = ["mock-exchange"]
mock-exchange = []

# 性能优化features
simd = []  # SIMD JSON解析优化

[[bin]]
name = "hft-collector"
path = "src/main.rs"
EOF

echo "✅ Cargo.toml已修复"
echo ""
echo "📋 Features配置说明："
echo "  - collector-binance: 币安现货"
echo "  - collector-binance-futures: 币安永续合约"
echo "  - collector-bitget: Bitget交易所"
echo "  - collector-bybit: Bybit交易所"
echo "  - collector-hyperliquid: Hyperliquid DEX"
echo "  - collector-asterdex: Asterdex"
echo ""
echo "🔄 下一步："
echo "  1. 运行 ./build-standard.sh 构建镜像"
echo "  2. 运行 ./deploy-standard.sh 部署到ECS"