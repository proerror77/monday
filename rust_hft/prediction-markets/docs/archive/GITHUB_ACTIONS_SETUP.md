# 🚀 GitHub Actions 自動部署指南

**目標**：使用 GitHub Actions 自動部署到 tango-2-1

---

## 📋 設置步驟

### 步驟 1：配置 GitHub Secrets

在你的 GitHub 倉庫中添加以下 Secrets：

1. 進入 GitHub 倉庫
2. Settings → Secrets and variables → Actions
3. 點擊 "New repository secret"
4. 添加以下 secrets：

| Secret 名稱 | 值 | 說明 |
|------------|-----|------|
| `AWS_ACCESS_KEY_ID` | 你的 AWS Access Key ID | AWS 訪問密鑰 |
| `AWS_SECRET_ACCESS_KEY` | 你的 AWS Secret Access Key | AWS 密鑰 |

#### 如何獲取 AWS 憑證

```bash
# 查看當前 AWS 配置
cat ~/.aws/credentials

# 或創建新的 IAM 用戶
# 1. 打開 AWS Console
# 2. IAM → Users → Create user
# 3. 附加策略：AmazonEC2FullAccess, AmazonS3FullAccess, AmazonSSMFullAccess
# 4. 創建訪問密鑰
```

---

### 步驟 2：推送代碼到 GitHub

```bash
# 初始化 git（如果還沒有）
git init

# 添加遠程倉庫
git remote add origin https://github.com/YOUR_USERNAME/ploy.git

# 添加所有文件
git add .

# 提交
git commit -m "Add GitHub Actions deployment workflow"

# 推送到 main 分支
git push -u origin main
```

---

### 步驟 3：觸發部署

#### 方式 1：自動觸發（推送代碼）

每次推送到 `main` 分支時自動部署：

```bash
git add .
git commit -m "Update code"
git push
```

#### 方式 2：手動觸發

1. 進入 GitHub 倉庫
2. Actions → Deploy to tango-2-1
3. 點擊 "Run workflow"
4. 選擇分支（main）
5. 點擊 "Run workflow"

---

## 🔍 工作流程說明

### 部署流程

```
1. Checkout 代碼
   ↓
2. 構建前端（npm ci && npm run build）
   ↓
3. 創建 S3 bucket
   ↓
4. 上傳前端到 S3
   ↓
5. 打包並上傳後端到 S3
   ↓
6. 創建部署腳本並上傳到 S3
   ↓
7. 通過 SSM 在 EC2 上執行部署
   ↓
8. 驗證部署
   ↓
9. 清理（可選）
```

### 部署時間

- **總時間**：約 10-15 分鐘
  - 構建前端：2-3 分鐘
  - 上傳文件：1-2 分鐘
  - EC2 部署：5-10 分鐘
  - 驗證：1 分鐘

---

## 📊 查看部署狀態

### 在 GitHub Actions 中查看

1. 進入 GitHub 倉庫
2. 點擊 "Actions" 標籤
3. 選擇最新的工作流運行
4. 查看每個步驟的日誌

### 部署摘要

每次部署完成後，會在 Actions 頁面顯示摘要：

- 實例信息
- IP 地址
- S3 Bucket
- 訪問 URL

---

## 🌐 訪問應用

部署完成後訪問：

- **前端**：http://3.112.247.26
- **NBA Swing**：http://3.112.247.26/nba-swing
- **策略監控**：http://3.112.247.26/monitor-strategy

---

## 🛠️ 自定義配置

### 修改目標 EC2

編輯 `.github/workflows/deploy-tango21.yml`：

```yaml
env:
  EC2_INSTANCE_ID: i-01de34df55726073d  # 修改為你的實例 ID
  EC2_IP: 3.112.247.26                  # 修改為你的 IP
```

### 修改觸發條件

```yaml
on:
  push:
    branches:
      - main        # 推送到 main 分支時觸發
      - develop     # 添加其他分支
  pull_request:     # PR 時觸發
  workflow_dispatch: # 手動觸發
```

### 啟用自動清理 S3

編輯 `.github/workflows/deploy-tango21.yml`，取消註釋：

```yaml
- name: Cleanup S3 (optional)
  if: success()
  run: |
    # 取消下面這行的註釋
    aws s3 rb s3://${{ env.S3_BUCKET }} --force
```

---

## 🔒 安全最佳實踐

### 1. 使用 IAM 角色（推薦）

為 GitHub Actions 創建專用的 IAM 用戶，只授予必要的權限：

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "s3:CreateBucket",
        "s3:PutObject",
        "s3:GetObject",
        "s3:DeleteBucket",
        "s3:DeleteObject"
      ],
      "Resource": "arn:aws:s3:::ploy-deployment-*"
    },
    {
      "Effect": "Allow",
      "Action": [
        "ssm:SendCommand",
        "ssm:GetCommandInvocation"
      ],
      "Resource": [
        "arn:aws:ec2:ap-northeast-1:*:instance/i-01de34df55726073d",
        "arn:aws:ssm:ap-northeast-1:*:*"
      ]
    }
  ]
}
```

### 2. 輪換訪問密鑰

定期更新 GitHub Secrets 中的 AWS 憑證。

### 3. 使用環境保護規則

在 GitHub 中設置環境保護規則：

1. Settings → Environments → New environment
2. 添加 "production" 環境
3. 配置保護規則（需要審批、等待時間等）

---

## 🆘 故障排除

### SSM 連接失敗

**問題**：無法通過 SSM 連接到 EC2

**解決方案**：

1. 確保 EC2 實例有 SSM Agent
   ```bash
   # 在 EC2 上檢查
   sudo systemctl status amazon-ssm-agent
   ```

2. 確保 EC2 實例有正確的 IAM 角色
   - 需要 `AmazonSSMManagedInstanceCore` 策略

3. 檢查安全組
   - 出站規則允許 HTTPS (443)

### 構建失敗

**問題**：前端構建失敗

**解決方案**：

1. 檢查 Node.js 版本
   ```yaml
   - name: Setup Node.js
     uses: actions/setup-node@v4
     with:
       node-version: '18'  # 修改版本
   ```

2. 清理緩存
   - Actions → Caches → 刪除緩存

### 部署超時

**問題**：部署步驟超時

**解決方案**：

1. 增加超時時間
   ```yaml
   - name: Deploy to EC2 via SSM
     timeout-minutes: 30  # 增加超時時間
   ```

2. 檢查 EC2 資源
   - CPU 和內存使用情況
   - 磁盤空間

---

## 📋 檢查清單

### 部署前

- [ ] 已添加 AWS Secrets 到 GitHub
- [ ] 已推送代碼到 GitHub
- [ ] EC2 實例正在運行
- [ ] EC2 有 SSM Agent
- [ ] EC2 有正確的 IAM 角色

### 部署後

- [ ] GitHub Actions 工作流成功完成
- [ ] 前端可以訪問
- [ ] 後端服務正在運行
- [ ] 所有頁面正常工作
- [ ] 已清理 S3 bucket（可選）

---

## 🎯 快速命令

```bash
# 查看 GitHub Actions 狀態（使用 gh CLI）
gh run list

# 查看最新運行的日誌
gh run view --log

# 手動觸發工作流
gh workflow run deploy-tango21.yml

# 檢查 EC2 狀態
aws ec2 describe-instances --instance-ids i-01de34df55726073d \
  --query 'Reservations[0].Instances[0].[State.Name,PublicIpAddress]' \
  --output text

# 連接到 EC2
aws ssm start-session --target i-01de34df55726073d

# 查看服務狀態（在 EC2 上）
sudo systemctl status nginx ploy-backend
```

---

## 📚 相關資源

- [GitHub Actions 文檔](https://docs.github.com/en/actions)
- [AWS SSM 文檔](https://docs.aws.amazon.com/systems-manager/)
- [GitHub CLI](https://cli.github.com/)

---

## 🎉 總結

使用 GitHub Actions 自動部署的優勢：

- ✅ **自動化**：推送代碼即自動部署
- ✅ **可追蹤**：完整的部署日誌
- ✅ **可重複**：每次部署都一致
- ✅ **安全**：使用 GitHub Secrets 管理憑證
- ✅ **快速**：10-15 分鐘完成部署

**立即開始**：
1. 添加 AWS Secrets 到 GitHub
2. 推送代碼
3. 查看 Actions 標籤
4. 等待部署完成
5. 訪問 http://3.112.247.26

---

**版本**：v1.0.0
**日期**：2026-01-13
**狀態**：✅ GitHub Actions 工作流已創建
