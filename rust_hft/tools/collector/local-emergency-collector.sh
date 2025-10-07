#!/bin/bash
# 紧急本地数据收集器 - 在 ECS 故障时使用

# ClickHouse Cloud 配置
export CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
export CLICKHOUSE_USER="default"
export CLICKHOUSE_PASSWORD="s9wECb~NGZPOE"

echo "🚨 紧急启动本地数据收集器..."
echo "📍 目标: ClickHouse Cloud"
echo "⚠️  注意: 这是临时方案，ECS 恢复后应切换回"

# 检查是否已安装 Docker
if ! command -v docker &> /dev/null; then
    echo "❌ 需要安装 Docker"
    echo "请运行: brew install --cask docker"
    exit 1
fi

# 构建本地镜像
echo "🔨 构建本地 Docker 镜像..."
cd /Users/proerror/Documents/monday/rust_hft/apps/collector

# 创建简化的 Dockerfile
cat > Dockerfile.local << 'EOF'
FROM rust:1.75-bullseye AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release --bin hft-collector

FROM debian:bullseye-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/hft-collector /usr/local/bin/
ENTRYPOINT ["hft-collector"]
EOF

# 构建镜像
docker build -f Dockerfile.local -t hft-collector:local .

# 停止任何现有的本地容器
docker stop local-hft-collector 2>/dev/null || true
docker rm local-hft-collector 2>/dev/null || true

# 启动本地收集器
echo "🚀 启动 Binance 现货数据收集..."
docker run -d \
  --name local-hft-collector \
  --restart unless-stopped \
  -e CLICKHOUSE_USER="$CLICKHOUSE_USER" \
  -e CLICKHOUSE_PASSWORD="$CLICKHOUSE_PASSWORD" \
  -e RUST_LOG=info \
  hft-collector:local \
  multi \
  --exchange binance \
  --top-limit 10 \
  --depth-mode limited \
  --depth-levels 10 \
  --flush-ms 3000 \
  --lob-mode snapshot \
  --ch-url "$CLICKHOUSE_URL" \
  --database hft_db

echo ""
echo "✅ 本地紧急收集器已启动"
echo ""
echo "📊 监控命令:"
echo "   docker logs -f local-hft-collector"
echo "   docker stats local-hft-collector"
echo ""
echo "⚠️  注意事项:"
echo "   1. 本地网络延迟会高于 ECS"
echo "   2. 减少了符号数量 (top-limit 10) 以降低负载"
echo "   3. ECS 恢复后请立即切换回"