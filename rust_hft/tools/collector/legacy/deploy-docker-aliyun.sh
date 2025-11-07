#!/bin/bash
set -e

# HFT Collector Docker 阿里云 ECS 部署脚本
# 用途：将 Docker 容器部署到阿里云 ECS，连接 AWS ClickHouse Cloud

# 阿里云 ECS 配置 (2025-09-28 更新)
INSTANCE_IP="8.221.136.162"
INSTANCE_ID="i-6weeysgffr51fos2kur0"
SSH_KEY_PATH="/Users/proerror/Documents/monday/rust_hft/hft-collector-key.pem"
DOCKER_IMAGE="hft-collector:multi-exchange"

# ClickHouse Cloud (AWS) 配置
CLICKHOUSE_URL="https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
CLICKHOUSE_USER="${CLICKHOUSE_USER:-default}"
CLICKHOUSE_PASSWORD="${CLICKHOUSE_PASSWORD:-}"

# 收集器配置
EXCHANGE="binance"
TOP_LIMIT="20"
DEPTH_MODE="both"
DEPTH_LEVELS="20"
FLUSH_MS="2000"
LOB_MODE="both"

echo "🚀 开始部署 HFT 多交易所 Collector Docker 容器到阿里云 ECS..."
echo "📍 目标实例: $INSTANCE_ID ($INSTANCE_IP)"
echo "🗄️  ClickHouse: AWS Cloud"
echo "🐳 Docker 镜像: $DOCKER_IMAGE (支持 Binance + Bitget + Asterdex)"

# 检查本地Docker镜像是否存在
if ! docker image inspect "$DOCKER_IMAGE" >/dev/null 2>&1; then
    echo "❌ Docker 镜像不存在: $DOCKER_IMAGE"
    echo "请先运行: ./docker-build.sh"
    exit 1
fi

# 检查环境变量
if [ -z "$CLICKHOUSE_PASSWORD" ]; then
    echo "❌ CLICKHOUSE_PASSWORD 环境变量未设置"
    echo "请运行: export CLICKHOUSE_PASSWORD='your_password'"
    exit 1
fi

# 检查 SSH 连接
echo "🔑 测试 SSH 连接..."
if ! ssh -i "$SSH_KEY_PATH" -o ConnectTimeout=10 -o StrictHostKeyChecking=no root@$INSTANCE_IP "echo 'SSH 连接成功'" 2>/dev/null; then
    echo "❌ SSH 连接失败，请检查："
    echo "   1. SSH key 路径是否正确: $SSH_KEY_PATH"
    echo "   2. 实例 IP 是否正确: $INSTANCE_IP"
    echo "   3. 安全组是否开放 SSH (22) 端口"
    exit 1
fi

# 1. 保存Docker镜像为tar文件
echo "📦 导出 Docker 镜像..."
DOCKER_TAR="/tmp/hft-collector.tar"
docker save "$DOCKER_IMAGE" -o "$DOCKER_TAR"
echo "✅ 镜像已导出: $(ls -lh $DOCKER_TAR)"

# 2. 上传Docker镜像到ECS
echo "📤 上传 Docker 镜像到 ECS..."
scp -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no "$DOCKER_TAR" root@$INSTANCE_IP:~/hft-collector.tar

# 3. 在ECS上安装Docker并导入镜像
echo "🐳 在 ECS 上配置 Docker..."
ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP << 'EOF'
    # 更新系统
    apt-get update

    # 安装 Docker
    if ! command -v docker &> /dev/null; then
        echo "安装 Docker..."
        apt-get install -y ca-certificates curl gnupg lsb-release
        mkdir -p /etc/apt/keyrings
        curl -fsSL https://download.docker.com/linux/ubuntu/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
        echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $(lsb_release -cs) stable" | tee /etc/apt/sources.list.d/docker.list > /dev/null
        apt-get update
        apt-get install -y docker-ce docker-ce-cli containerd.io docker-compose-plugin
        systemctl enable docker
        systemctl start docker
    else
        echo "Docker 已安装"
    fi

    # 导入Docker镜像
    echo "导入 Docker 镜像..."
    docker load -i ~/hft-collector.tar

    # 清理tar文件
    rm -f ~/hft-collector.tar

    # 检查镜像
    docker images | grep hft-collector
EOF

# 4. 创建Docker运行脚本
echo "⚙️ 创建 Docker 运行脚本..."
cat << SCRIPT_EOF | ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP "tee /root/run-hft-collector.sh"
#!/bin/bash
# HFT Collector Docker 运行脚本

# 停止现有容器
docker stop hft-collector 2>/dev/null || true
docker rm hft-collector 2>/dev/null || true

# 运行新容器
docker run -d \\
  --name hft-collector \\
  --restart unless-stopped \\
  -e CLICKHOUSE_USER="$CLICKHOUSE_USER" \\
  -e CLICKHOUSE_PASSWORD="$CLICKHOUSE_PASSWORD" \\
  -e RUST_LOG=info \\
  "$DOCKER_IMAGE" \\
  multi \\
  --exchange "$EXCHANGE" \\
  --top-limit "$TOP_LIMIT" \\
  --depth-mode "$DEPTH_MODE" \\
  --depth-levels "$DEPTH_LEVELS" \\
  --flush-ms "$FLUSH_MS" \\
  --lob-mode "$LOB_MODE" \\
  --ch-url "$CLICKHOUSE_URL" \\
  --database hft_db

echo "Docker 容器启动完成"
echo "检查容器状态:"
docker ps | grep hft-collector
echo "查看日志:"
docker logs --tail 20 hft-collector
SCRIPT_EOF

# 5. 创建systemd服务文件（用于开机自启动）
echo "⚙️ 创建 systemd 服务..."
cat << SYSTEMD_EOF | ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP "tee /etc/systemd/system/hft-collector-docker.service"
[Unit]
Description=HFT Collector Docker Container
Requires=docker.service
After=docker.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/root/run-hft-collector.sh
ExecStop=docker stop hft-collector
TimeoutStartSec=0

[Install]
WantedBy=multi-user.target
SYSTEMD_EOF

# 6. 设置权限并启动服务
echo "🔧 配置和启动服务..."
ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP << 'EOF'
    # 设置脚本权限
    chmod +x /root/run-hft-collector.sh

    # 重新载入 systemd
    systemctl daemon-reload

    # 启用开机自动启动
    systemctl enable hft-collector-docker

    # 停止旧服务（如果存在）
    systemctl stop hft-collector-docker 2>/dev/null || true

    # 启动新服务
    systemctl start hft-collector-docker

    # 等待几秒钟让服务启动
    sleep 5

    # 检查服务状态
    systemctl status hft-collector-docker --no-pager

    # 检查Docker容器状态
    echo ""
    echo "Docker 容器状态:"
    docker ps | grep hft-collector
EOF

# 7. 验证部署
echo "📊 验证部署..."

# 检查容器状态
echo "🔍 检查容器运行状态..."
CONTAINER_STATUS=$(ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP "docker inspect -f '{{.State.Status}}' hft-collector 2>/dev/null || echo 'not_found'")
if [ "$CONTAINER_STATUS" = "running" ]; then
    echo "✅ Docker 容器运行正常"
else
    echo "❌ Docker 容器状态异常: $CONTAINER_STATUS"
fi

# 显示最新日志
echo "📝 最新日志 (最后 20 行):"
ssh -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no root@$INSTANCE_IP "docker logs --tail 20 hft-collector"

# 清理本地临时文件
rm -f "$DOCKER_TAR"

echo ""
echo "✅ Docker 部署完成！"
echo ""
echo "📊 监控命令："
echo "   ssh -i $SSH_KEY_PATH root@$INSTANCE_IP"
echo "   docker logs -f hft-collector               # 实时日志"
echo "   docker ps                                   # 容器状态"
echo "   docker stats hft-collector                 # 资源使用"
echo "   systemctl status hft-collector-docker      # 服务状态"
echo ""
echo "🔧 管理命令："
echo "   systemctl restart hft-collector-docker     # 重启服务"
echo "   systemctl stop hft-collector-docker        # 停止服务"
echo "   systemctl start hft-collector-docker       # 启动服务"
echo "   docker restart hft-collector               # 重启容器"
echo ""
echo "🌐 实例信息："
echo "   公网 IP: $INSTANCE_IP"
echo "   实例 ID: $INSTANCE_ID"
echo "   区域: ap-northeast-1 (东京)"
echo "   数据目标: AWS ClickHouse Cloud"
echo "   运行模式: Docker 容器"
echo ""
echo "🎯 HFT 多交易所 Collector Docker 容器现在正在从 Binance + Bitget + Asterdex 收集数据到你的 ClickHouse Cloud！"