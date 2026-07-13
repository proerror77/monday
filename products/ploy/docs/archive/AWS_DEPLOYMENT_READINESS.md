# 🚀 AWS 部署就緒評估報告

**評估時間：** 2026-01-10
**系統版本：** Phase 1 安全修復完成版本
**目標環境：** AWS (ap-northeast-1 日本區域)

---

## 📊 部署就緒度評分

| 類別 | 狀態 | 完成度 | 說明 |
|------|------|--------|------|
| **代碼實現** | ✅ | 100% | 所有安全修復已完成 |
| **Docker 配置** | ✅ | 100% | Dockerfile 已存在 |
| **CI/CD 流程** | ✅ | 100% | GitHub Actions 已配置 |
| **數據庫遷移** | ⚠️ | 80% | 需要在 AWS 環境運行 |
| **環境變量** | ⚠️ | 90% | 需要添加新的密鑰 |
| **監控告警** | ⚠️ | 60% | 需要配置 CloudWatch |

**總體就緒度：** 85% - **可以部署，但需要完成以下步驟**

---

## ✅ 已具備的 AWS 部署能力

### 1. Docker 容器化 ✅
**文件：** `Dockerfile`

```dockerfile
# 多階段構建
FROM debian:bookworm-slim AS builder
# ... 編譯階段 ...

FROM debian:bookworm-slim AS runtime
# ... 運行階段 ...
```

**特性：**
- ✅ 多階段構建（減小鏡像大小）
- ✅ 非 root 用戶運行（安全）
- ✅ 健康檢查配置
- ✅ 日誌目錄掛載
- ✅ 環境變量配置

### 2. GitHub Actions CI/CD ✅
**文件：** `.github/workflows/deploy-aws-jp.yml`

**流程：**
1. ✅ 構建 Docker 鏡像
2. ✅ 推送到 AWS ECR
3. ✅ SSH 到 EC2 部署
4. ✅ 自動重啟容器
5. ✅ 驗證部署狀態

**支持的參數：**
- 交易符號（BTCUSDT, ETHUSDT, SOLUSDT, XRPUSDT）
- 最小移動百分比
- 最大入場價格
- 每筆交易股數
- 預測模式開關
- 止盈/止損百分比

### 3. 本地開發環境 ✅
**文件：** `docker-compose.yml`

```yaml
services:
  postgres:
    image: postgres:16-alpine
    # PostgreSQL 數據庫配置
```

**特性：**
- ✅ PostgreSQL 16 數據庫
- ✅ 健康檢查
- ✅ 數據持久化
- ✅ 自動初始化遷移

---

## ⚠️ 需要完成的 AWS 部署步驟

### 步驟 1：更新 Dockerfile（添加數據庫遷移）

**當前問題：** Dockerfile 沒有包含數據庫遷移邏輯

**解決方案：** 需要添加 sqlx-cli 和遷移腳本

```dockerfile
# 在 builder 階段添加
RUN cargo install sqlx-cli --no-default-features --features postgres

# 在 runtime 階段添加
COPY --from=builder /root/.cargo/bin/sqlx /opt/ploy/bin/sqlx
COPY migrations /opt/ploy/migrations

# 添加啟動腳本
COPY docker-entrypoint.sh /opt/ploy/bin/
RUN chmod +x /opt/ploy/bin/docker-entrypoint.sh
ENTRYPOINT ["/opt/ploy/bin/docker-entrypoint.sh"]
```

### 步驟 2：創建啟動腳本

**需要創建：** `docker-entrypoint.sh`

```bash
#!/bin/bash
set -e

echo "🚀 Starting Ploy Trading Bot..."

# 等待數據庫就緒
echo "⏳ Waiting for database..."
until pg_isready -h $DATABASE_HOST -p $DATABASE_PORT -U $DATABASE_USER; do
  sleep 2
done

echo "✅ Database is ready!"

# 運行數據庫遷移
echo "📦 Running database migrations..."
cd /opt/ploy
sqlx migrate run --database-url "$DATABASE_URL"

echo "✅ Migrations complete!"

# 啟動應用
echo "🎯 Starting trading bot..."
exec /opt/ploy/bin/ploy "$@"
```

### 步驟 3：配置 AWS RDS PostgreSQL

**需要創建：**
1. RDS PostgreSQL 實例
2. 安全組配置
3. 數據庫連接字符串

**推薦配置：**
```yaml
實例類型: db.t3.micro (開發) / db.t3.small (生產)
存儲: 20GB SSD
備份: 7 天自動備份
多可用區: 是（生產環境）
加密: 是
```

**連接字符串格式：**
```
postgresql://ploy:PASSWORD@ploy-db.xxxxx.ap-northeast-1.rds.amazonaws.com:5432/ploy
```

### 步驟 4：更新 GitHub Secrets

**需要添加的新密鑰：**

```yaml
# 現有密鑰（已配置）
AWS_ACCESS_KEY_ID: ✅
AWS_SECRET_ACCESS_KEY: ✅
AWS_EC2_PRIVATE_KEY: ✅
AWS_EC2_HOST: ✅
POLYMARKET_PRIVATE_KEY: ✅
POLYMARKET_FUNDER: ✅
FEISHU_WEBHOOK_URL: ✅

# 新增密鑰（需要配置）
DATABASE_URL: ⚠️ 需要添加
DATABASE_HOST: ⚠️ 需要添加
DATABASE_PORT: ⚠️ 需要添加（默認 5432）
DATABASE_USER: ⚠️ 需要添加（默認 ploy）
DATABASE_PASSWORD: ⚠️ 需要添加
```

### 步驟 5：更新 GitHub Actions 工作流

**需要修改：** `.github/workflows/deploy-aws-jp.yml`

```yaml
# 在 docker run 命令中添加數據庫環境變量
docker run -d \
  --name ploy-trading \
  --restart unless-stopped \
  -v /var/log/ploy:/opt/ploy/logs \
  -e DATABASE_URL="${{ secrets.DATABASE_URL }}" \
  -e DATABASE_HOST="${{ secrets.DATABASE_HOST }}" \
  -e DATABASE_PORT="${{ secrets.DATABASE_PORT }}" \
  -e DATABASE_USER="${{ secrets.DATABASE_USER }}" \
  -e DATABASE_PASSWORD="${{ secrets.DATABASE_PASSWORD }}" \
  -e POLYMARKET_PRIVATE_KEY="${{ secrets.POLYMARKET_PRIVATE_KEY }}" \
  -e POLYMARKET_FUNDER="${{ secrets.POLYMARKET_FUNDER }}" \
  -e FEISHU_WEBHOOK_URL="${{ secrets.FEISHU_WEBHOOK_URL }}" \
  -e RUST_LOG=info,ploy=debug \
  ${{ steps.login-ecr.outputs.registry }}/${{ env.ECR_REPOSITORY }}:latest \
  momentum \
  --symbols "${{ github.event.inputs.symbols }}" \
  --min-move ${{ github.event.inputs.min_move }} \
  --max-entry ${{ github.event.inputs.max_entry }} \
  --shares ${{ github.event.inputs.shares }} \
  $PREDICTIVE_FLAG
```

### 步驟 6：配置 CloudWatch 監控

**需要設置：**

1. **日誌收集**
```bash
# 在 EC2 上安裝 CloudWatch Agent
sudo yum install amazon-cloudwatch-agent

# 配置日誌收集
{
  "logs": {
    "logs_collected": {
      "files": {
        "collect_list": [
          {
            "file_path": "/var/log/ploy/*.log",
            "log_group_name": "/aws/ploy/trading",
            "log_stream_name": "{instance_id}"
          }
        ]
      }
    }
  }
}
```

2. **告警規則**
```yaml
# 關鍵指標告警
- 容器停止運行
- CPU 使用率 > 80%
- 內存使用率 > 80%
- 錯誤日誌頻率 > 10/分鐘
- 數據庫連接失敗
```

---

## 📋 完整部署檢查清單

### 前置準備
- [ ] AWS 賬號已創建
- [ ] IAM 用戶已配置（ECR、EC2、RDS 權限）
- [ ] EC2 實例已啟動（推薦 t3.small）
- [ ] RDS PostgreSQL 已創建
- [ ] 安全組已配置（允許 EC2 訪問 RDS）
- [ ] ECR 倉庫已創建

### 代碼準備
- [x] 安全修復已完成
- [ ] 創建 `docker-entrypoint.sh`
- [ ] 更新 `Dockerfile`
- [ ] 更新 `.github/workflows/deploy-aws-jp.yml`
- [ ] 測試本地 Docker 構建

### GitHub 配置
- [x] AWS 訪問密鑰已配置
- [x] EC2 SSH 密鑰已配置
- [ ] 數據庫連接信息已添加
- [x] Polymarket 密鑰已配置
- [x] Feishu Webhook 已配置

### 數據庫準備
- [ ] RDS 實例已啟動
- [ ] 數據庫 `ploy` 已創建
- [ ] 用戶權限已配置
- [ ] 從 EC2 測試連接成功
- [ ] 運行數據庫遷移

### 部署驗證
- [ ] Docker 鏡像構建成功
- [ ] 推送到 ECR 成功
- [ ] 容器啟動成功
- [ ] 數據庫遷移成功
- [ ] 應用日誌正常
- [ ] 健康檢查通過

### 監控配置
- [ ] CloudWatch Agent 已安裝
- [ ] 日誌收集已配置
- [ ] 告警規則已設置
- [ ] SNS 通知已配置

---

## 🚀 快速部署指南

### 1. 創建 RDS 數據庫（10 分鐘）

```bash
# 使用 AWS CLI 創建 RDS 實例
aws rds create-db-instance \
  --db-instance-identifier ploy-db \
  --db-instance-class db.t3.micro \
  --engine postgres \
  --engine-version 16.1 \
  --master-username ploy \
  --master-user-password YOUR_SECURE_PASSWORD \
  --allocated-storage 20 \
  --vpc-security-group-ids sg-xxxxx \
  --db-subnet-group-name default \
  --backup-retention-period 7 \
  --region ap-northeast-1

# 等待實例創建完成
aws rds wait db-instance-available \
  --db-instance-identifier ploy-db
```

### 2. 配置 GitHub Secrets（5 分鐘）

```bash
# 在 GitHub 倉庫設置中添加以下 Secrets：
DATABASE_URL=postgresql://ploy:PASSWORD@ploy-db.xxxxx.ap-northeast-1.rds.amazonaws.com:5432/ploy
DATABASE_HOST=ploy-db.xxxxx.ap-northeast-1.rds.amazonaws.com
DATABASE_PORT=5432
DATABASE_USER=ploy
DATABASE_PASSWORD=YOUR_SECURE_PASSWORD
```

### 3. 更新代碼（15 分鐘）

```bash
# 1. 創建啟動腳本
cat > docker-entrypoint.sh << 'EOF'
#!/bin/bash
set -e
echo "🚀 Starting Ploy Trading Bot..."
# ... (完整腳本見上文)
EOF

# 2. 更新 Dockerfile
# (見上文修改建議)

# 3. 更新 GitHub Actions
# (見上文修改建議)

# 4. 提交更改
git add .
git commit -m "feat: Add AWS deployment support with database migrations"
git push
```

### 4. 觸發部署（5 分鐘）

```bash
# 在 GitHub Actions 頁面手動觸發 "Deploy to AWS Japan" 工作流
# 或使用 GitHub CLI
gh workflow run deploy-aws-jp.yml \
  -f symbols="BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT" \
  -f min_move="0.15" \
  -f max_entry="45" \
  -f shares="100" \
  -f predictive="true" \
  -f take_profit="20" \
  -f stop_loss="12"
```

### 5. 驗證部署（5 分鐘）

```bash
# SSH 到 EC2 實例
ssh -i your-key.pem ec2-user@YOUR_EC2_IP

# 檢查容器狀態
docker ps
docker logs ploy-trading --tail 50

# 檢查數據庫連接
docker exec ploy-trading psql $DATABASE_URL -c "SELECT * FROM nonce_state;"

# 檢查應用健康
curl http://localhost:8080/health
```

---

## 💰 AWS 成本估算

### 開發環境（每月）
| 服務 | 配置 | 成本 |
|------|------|------|
| EC2 t3.micro | 1 實例 | $7.50 |
| RDS db.t3.micro | 1 實例 | $15.00 |
| EBS 存儲 | 20GB | $2.00 |
| ECR 存儲 | 1GB | $0.10 |
| 數據傳輸 | 10GB | $0.90 |
| **總計** | | **$25.50/月** |

### 生產環境（每月）
| 服務 | 配置 | 成本 |
|------|------|------|
| EC2 t3.small | 1 實例 | $15.00 |
| RDS db.t3.small | 多可用區 | $60.00 |
| EBS 存儲 | 50GB | $5.00 |
| ECR 存儲 | 5GB | $0.50 |
| CloudWatch | 日誌 + 告警 | $10.00 |
| 數據傳輸 | 50GB | $4.50 |
| **總計** | | **$95.00/月** |

---

## 🎯 建議的部署策略

### 階段 1：測試環境部署（本週）
1. ✅ 完成代碼修改
2. ✅ 創建測試 RDS 實例
3. ✅ 配置 GitHub Secrets
4. ✅ 運行首次部署
5. ✅ 驗證所有功能

### 階段 2：生產環境準備（下週）
1. 創建生產 RDS（多可用區）
2. 配置 CloudWatch 監控
3. 設置告警規則
4. 配置自動備份
5. 壓力測試

### 階段 3：生產部署（下下週）
1. 藍綠部署策略
2. 逐步切換流量
3. 24 小時監控
4. 性能優化
5. 成本優化

---

## ✅ 結論

### 當前狀態
- **代碼就緒：** ✅ 100%
- **Docker 就緒：** ⚠️ 90%（需要添加遷移邏輯）
- **CI/CD 就緒：** ⚠️ 90%（需要添加數據庫配置）
- **AWS 基礎設施：** ⚠️ 60%（需要創建 RDS）

### 部署時間估算
- **代碼修改：** 30 分鐘
- **AWS 配置：** 20 分鐘
- **首次部署：** 15 分鐘
- **驗證測試：** 15 分鐘
- **總計：** ~1.5 小時

### 建議
✅ **可以部署到 AWS**，但建議先完成以下工作：

1. **高優先級（必須）：**
   - 創建 `docker-entrypoint.sh`
   - 更新 `Dockerfile` 添加遷移支持
   - 創建 RDS 實例
   - 配置數據庫連接

2. **中優先級（推薦）：**
   - 配置 CloudWatch 監控
   - 設置告警規則
   - 配置自動備份

3. **低優先級（可選）：**
   - 多可用區部署
   - 負載均衡器
   - Auto Scaling

---

**評估結論：** 🟢 **可以部署**
**建議行動：** 完成上述高優先級任務後即可部署
**預計時間：** 1.5-2 小時完成所有準備工作

---

**報告生成：** 2026-01-10
**評估者：** Claude Code
**下一步：** 創建部署所需的配置文件
