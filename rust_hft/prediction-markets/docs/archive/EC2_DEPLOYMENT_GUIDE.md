# 🚀 Ploy Trading System - EC2 部署指南

**目標 EC2**：tango_1_1
- **實例 ID**：i-0b29ca671375dad53
- **IP 地址**：13.113.155.16
- **密鑰對**：bn-watcher-key
- **狀態**：✅ 運行中

---

## 📋 部署方式選擇

### 方式 1：使用 S3 傳輸（推薦）✅

**優點**：不需要 SSH 密鑰，速度快，可靠
**步驟**：

#### 步驟 1：上傳文件到 S3

```bash
# 1. 創建 S3 bucket（如果還沒有）
aws s3 mb s3://ploy-deployment-$(date +%s)

# 2. 上傳前端構建文件
cd /Users/proerror/Documents/ploy/ploy-frontend
aws s3 cp dist/ s3://ploy-deployment-XXXXX/frontend/ --recursive

# 3. 打包並上傳後端代碼
cd /Users/proerror/Documents/ploy
tar czf ploy-backend.tar.gz \
    --exclude='target' \
    --exclude='node_modules' \
    --exclude='ploy-frontend' \
    --exclude='.git' \
    --exclude='data' \
    --exclude='results' \
    Cargo.toml Cargo.lock src/ examples/ migrations/

aws s3 cp ploy-backend.tar.gz s3://ploy-deployment-XXXXX/
```

#### 步驟 2：在 EC2 上下載並部署

使用 AWS Systems Manager Session Manager 連接到 EC2：

```bash
# 連接到 EC2
aws ssm start-session --target i-0b29ca671375dad53
```

然後在 EC2 上執行：

```bash
# 1. 創建目錄
mkdir -p ~/ploy/{frontend,backend}

# 2. 從 S3 下載文件
aws s3 cp s3://ploy-deployment-XXXXX/frontend/ ~/ploy/frontend/ --recursive
aws s3 cp s3://ploy-deployment-XXXXX/ploy-backend.tar.gz ~/ploy/backend/

# 3. 解壓後端代碼
cd ~/ploy/backend
tar xzf ploy-backend.tar.gz
rm ploy-backend.tar.gz

# 4. 安裝依賴
sudo apt-get update
sudo apt-get install -y nginx build-essential pkg-config libssl-dev

# 5. 安裝 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# 6. 配置 Nginx
sudo tee /etc/nginx/sites-available/ploy > /dev/null << 'EOF'
server {
    listen 80;
    server_name _;

    # 前端
    location / {
        root /home/ubuntu/ploy/frontend;
        try_files $uri $uri/ /index.html;
    }

    # 後端 API
    location /api {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_cache_bypass $http_upgrade;
    }

    # WebSocket
    location /ws {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "Upgrade";
        proxy_set_header Host $host;
    }
}
EOF

# 7. 啟用 Nginx 配置
sudo ln -sf /etc/nginx/sites-available/ploy /etc/nginx/sites-enabled/
sudo rm -f /etc/nginx/sites-enabled/default
sudo nginx -t
sudo systemctl restart nginx
sudo systemctl enable nginx

# 8. 構建後端
cd ~/ploy/backend
cargo build --release

# 9. 創建 systemd 服務
sudo tee /etc/systemd/system/ploy-backend.service > /dev/null << 'EOF'
[Unit]
Description=Ploy Trading Backend
After=network.target

[Service]
Type=simple
User=ubuntu
WorkingDirectory=/home/ubuntu/ploy/backend
ExecStart=/home/ubuntu/ploy/backend/target/release/ploy
Restart=always
RestartSec=10
Environment="RUST_LOG=info"

[Install]
WantedBy=multi-user.target
EOF

# 10. 啟動服務
sudo systemctl daemon-reload
sudo systemctl start ploy-backend
sudo systemctl enable ploy-backend

# 11. 檢查狀態
sudo systemctl status ploy-backend
sudo systemctl status nginx
```

---

### 方式 2：使用 EC2 Instance Connect

#### 步驟 1：在 AWS Console 中連接

1. 打開 AWS Console
2. 進入 EC2 → Instances
3. 選擇 tango_1_1 (i-0b29ca671375dad53)
4. 點擊 "Connect" → "EC2 Instance Connect"
5. 點擊 "Connect"

#### 步驟 2：執行部署命令

在瀏覽器終端中執行上面「方式 1 - 步驟 2」中的所有命令。

---

### 方式 3：使用 SSH 密鑰（如果有）

如果你有 bn-watcher-key.pem 文件：

```bash
# 1. 設置密鑰權限
chmod 400 ~/.ssh/bn-watcher-key.pem

# 2. 上傳前端文件
scp -i ~/.ssh/bn-watcher-key.pem -r dist/* ubuntu@13.113.155.16:~/ploy/frontend/

# 3. 上傳後端代碼
tar czf /tmp/ploy-backend.tar.gz \
    --exclude='target' \
    --exclude='node_modules' \
    --exclude='ploy-frontend' \
    --exclude='.git' \
    Cargo.toml Cargo.lock src/ examples/ migrations/

scp -i ~/.ssh/bn-watcher-key.pem /tmp/ploy-backend.tar.gz ubuntu@13.113.155.16:~/ploy/backend/

# 4. SSH 連接並部署
ssh -i ~/.ssh/bn-watcher-key.pem ubuntu@13.113.155.16

# 然後執行「方式 1 - 步驟 2」中的命令
```

---

## 🔍 驗證部署

### 檢查服務狀態

```bash
# 檢查 Nginx
sudo systemctl status nginx

# 檢查後端服務
sudo systemctl status ploy-backend

# 查看後端日誌
sudo journalctl -u ploy-backend -f

# 查看 Nginx 日誌
sudo tail -f /var/log/nginx/access.log
sudo tail -f /var/log/nginx/error.log
```

### 測試訪問

```bash
# 測試前端
curl http://13.113.155.16

# 測試後端 API（如果有健康檢查端點）
curl http://13.113.155.16/api/health
```

### 在瀏覽器中訪問

- **前端主頁**：http://13.113.155.16
- **NBA Swing**：http://13.113.155.16/nba-swing
- **策略監控**：http://13.113.155.16/monitor-strategy
- **交易歷史**：http://13.113.155.16/trades

---

## 🛠️ 管理命令

### 重啟服務

```bash
# 重啟後端
sudo systemctl restart ploy-backend

# 重啟 Nginx
sudo systemctl restart nginx

# 重啟所有服務
sudo systemctl restart ploy-backend nginx
```

### 查看日誌

```bash
# 後端日誌（實時）
sudo journalctl -u ploy-backend -f

# 後端日誌（最近 100 行）
sudo journalctl -u ploy-backend -n 100

# Nginx 訪問日誌
sudo tail -f /var/log/nginx/access.log

# Nginx 錯誤日誌
sudo tail -f /var/log/nginx/error.log
```

### 更新代碼

```bash
# 1. 停止服務
sudo systemctl stop ploy-backend

# 2. 更新代碼（使用 S3 或 git pull）
cd ~/ploy/backend
# ... 更新代碼 ...

# 3. 重新構建
cargo build --release

# 4. 啟動服務
sudo systemctl start ploy-backend
```

---

## 🔒 安全配置

### 配置防火牆

```bash
# 允許 HTTP
sudo ufw allow 80/tcp

# 允許 HTTPS（如果需要）
sudo ufw allow 443/tcp

# 啟用防火牆
sudo ufw enable
```

### 配置 HTTPS（可選）

```bash
# 安裝 Certbot
sudo apt-get install -y certbot python3-certbot-nginx

# 獲取證書（需要域名）
sudo certbot --nginx -d your-domain.com

# 自動續期
sudo certbot renew --dry-run
```

---

## 📊 監控

### 系統資源

```bash
# CPU 和內存使用
htop

# 磁盤使用
df -h

# 網絡連接
sudo netstat -tulpn | grep LISTEN
```

### 應用監控

```bash
# 檢查進程
ps aux | grep ploy

# 檢查端口
sudo lsof -i :8080
sudo lsof -i :80
```

---

## 🆘 故障排除

### 前端無法訪問

```bash
# 檢查 Nginx 狀態
sudo systemctl status nginx

# 檢查 Nginx 配置
sudo nginx -t

# 檢查文件權限
ls -la ~/ploy/frontend/

# 重啟 Nginx
sudo systemctl restart nginx
```

### 後端無法啟動

```bash
# 查看詳細日誌
sudo journalctl -u ploy-backend -n 100 --no-pager

# 檢查二進制文件
ls -la ~/ploy/backend/target/release/ploy

# 手動運行測試
cd ~/ploy/backend
./target/release/ploy

# 檢查端口占用
sudo lsof -i :8080
```

### 構建失敗

```bash
# 檢查 Rust 版本
rustc --version
cargo --version

# 更新 Rust
rustup update

# 清理並重新構建
cd ~/ploy/backend
cargo clean
cargo build --release
```

---

## 📝 快速命令參考

```bash
# 連接到 EC2（SSM）
aws ssm start-session --target i-0b29ca671375dad53

# 連接到 EC2（SSH，如果有密鑰）
ssh -i ~/.ssh/bn-watcher-key.pem ubuntu@13.113.155.16

# 查看所有服務狀態
sudo systemctl status ploy-backend nginx

# 重啟所有服務
sudo systemctl restart ploy-backend nginx

# 查看實時日誌
sudo journalctl -u ploy-backend -f

# 檢查 EC2 狀態
aws ec2 describe-instances --instance-ids i-0b29ca671375dad53 --query 'Reservations[0].Instances[0].[State.Name,PublicIpAddress]' --output text
```

---

## 🎯 推薦部署流程

**最簡單的方式**：

1. ✅ 使用 S3 上傳文件
2. ✅ 使用 SSM Session Manager 連接到 EC2
3. ✅ 執行部署命令
4. ✅ 驗證訪問

**命令總結**：

```bash
# 本地執行
aws s3 mb s3://ploy-deployment-$(date +%s)
aws s3 cp dist/ s3://ploy-deployment-XXXXX/frontend/ --recursive
tar czf ploy-backend.tar.gz Cargo.toml Cargo.lock src/ examples/ migrations/
aws s3 cp ploy-backend.tar.gz s3://ploy-deployment-XXXXX/

# 連接到 EC2
aws ssm start-session --target i-0b29ca671375dad53

# 在 EC2 上執行（複製上面「方式 1 - 步驟 2」中的所有命令）
```

---

**版本**：v1.0.0
**日期**：2026-01-13
**狀態**：✅ 部署指南已就緒
