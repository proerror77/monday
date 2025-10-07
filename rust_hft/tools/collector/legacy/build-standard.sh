#!/bin/bash
set -e

# HFT Collector 标准构建脚本
# 确保所有features都启用，架构正确

echo "🚀 HFT Collector 标准构建流程"
echo "================================"

# 配置
REGISTRY="${DOCKER_REGISTRY:-}"  # 可选的Docker Registry
TAG_PREFIX="hft-collector"
VERSION=$(date +"%Y%m%d-%H%M%S")
PLATFORMS="linux/amd64"  # ECS是x86_64

# 1. 创建完整功能的Dockerfile
cat > Dockerfile.standard << 'EOF'
# 多阶段构建 - 使用特定版本确保一致性
FROM rust:1.75-bullseye AS builder

WORKDIR /app

# 安装依赖
RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# 复制项目文件
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/

# 构建 - 启用所有features
RUN cargo build --release --bin hft-collector \
    --features "collector-binance,collector-binance-futures,collector-bitget,collector-bybit,collector-asterdex,collector-hyperliquid"

# 运行阶段
FROM debian:bullseye-slim

RUN apt-get update && \
    apt-get install -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/hft-collector /usr/local/bin/

ENTRYPOINT ["hft-collector"]
EOF

# 2. 构建Docker镜像
echo "📦 构建Docker镜像..."
docker build -f Dockerfile.standard \
  --platform ${PLATFORMS} \
  -t ${TAG_PREFIX}:latest \
  -t ${TAG_PREFIX}:${VERSION} \
  -t ${TAG_PREFIX}:standard \
  .

# 3. 验证镜像
echo "🔍 验证镜像功能..."
docker run --rm ${TAG_PREFIX}:standard --help

# 4. 导出镜像（用于传输到ECS）
echo "💾 导出镜像..."
docker save ${TAG_PREFIX}:standard | gzip > hft-collector-standard.tar.gz

echo "✅ 构建完成！"
echo ""
echo "📋 下一步操作："
echo "1. 上传到ECS: scp hft-collector-standard.tar.gz root@<ECS_IP>:~/"
echo "2. 在ECS加载: docker load < hft-collector-standard.tar.gz"
echo "3. 启动服务: 使用 deploy-standard.sh"