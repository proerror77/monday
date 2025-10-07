#!/bin/bash
set -e

# HFT Collector 裸機部署腳本
# 用途：在 EC2 上部署和管理 hft-collector 服務

INSTANCE_IP="43.206.212.216"
KEY_PATH="$HOME/.ssh/aws_tango_1.pem"  # 請確認你的 key path
BINARY_PATH="./target/release/hft-collector"

echo "🚀 開始部署 HFT Collector 到裸機 EC2..."

# 1. 複製二進制文件到 EC2
echo "📦 上傳二進制文件..."
scp -i "$KEY_PATH" -o StrictHostKeyChecking=no "$BINARY_PATH" ec2-user@$INSTANCE_IP:~/hft-collector

# 2. 創建 systemd 服務文件
echo "⚙️ 創建 systemd 服務..."
cat << 'EOF' | ssh -i "$KEY_PATH" -o StrictHostKeyChecking=no ec2-user@$INSTANCE_IP "sudo tee /etc/systemd/system/hft-collector.service"
[Unit]
Description=HFT Data Collector - Bitget USDT-Futures
After=network.target
Wants=network.target

[Service]
Type=simple
User=ec2-user
WorkingDirectory=/home/ec2-user
ExecStart=/home/ec2-user/hft-collector --top-limit 10 --batch-size 5000 --flush-ms 1000
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal
SyslogIdentifier=hft-collector

# Environment variables
Environment=CLICKHOUSE_URL=https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443
Environment=CLICKHOUSE_USER=default
Environment=CLICKHOUSE_PASSWORD=s9wECb~NGZPOE
Environment=RUST_LOG=info

# Resource limits
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

# 3. 設置服務權限和啟動
echo "🔧 配置和啟動服務..."
ssh -i "$KEY_PATH" -o StrictHostKeyChecking=no ec2-user@$INSTANCE_IP << 'EOF'
    # 設置二進制文件權限
    chmod +x ~/hft-collector
    
    # 重新載入 systemd
    sudo systemctl daemon-reload
    
    # 啟用開機自動啟動
    sudo systemctl enable hft-collector
    
    # 啟動服務
    sudo systemctl start hft-collector
    
    # 檢查服務狀態
    sudo systemctl status hft-collector --no-pager
EOF

echo "✅ 部署完成！"
echo ""
echo "📊 使用以下命令監控服務："
echo "   ssh -i $KEY_PATH ec2-user@$INSTANCE_IP"
echo "   sudo journalctl -f -u hft-collector  # 查看實時日誌"
echo "   sudo systemctl status hft-collector   # 查看服務狀態"
echo "   htop                                  # 查看系統資源"
echo ""
echo "🔗 EC2 實例信息："
echo "   公共 IP: $INSTANCE_IP"
echo "   實例 ID: i-07a721b068e882f94"
echo "   區域: ap-northeast-1"