# GitHub分支保護規則配置

## 概述

本文檔描述了HFT交易平台項目的分支保護規則配置，確保代碼質量和發佈安全性。

## 分支保護策略

### Main分支保護規則

**目的**: 確保生產代碼的穩定性和質量

```json
{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "quick-validation",
      "rust-tests",
      "python-tests", 
      "integration-tests",
      "performance-tests",
      "security-scan"
    ]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "required_approving_review_count": 2,
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true,
    "require_last_push_approval": true
  },
  "restrictions": {
    "users": [],
    "teams": ["hft-core-team", "senior-developers"]
  },
  "allow_force_pushes": false,
  "allow_deletions": false,
  "required_linear_history": true
}
```

**關鍵設定說明**:
- 需要2個審查者批准
- 強制執行所有CI檢查通過
- 禁止強制推送和刪除
- 要求線性歷史記錄
- 管理員也需遵守規則

### Develop分支保護規則

**目的**: 維護開發主線的穩定性，允許適度靈活性

```json
{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "quick-validation",
      "rust-tests",
      "python-tests",
      "integration-tests"
    ]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": false
  },
  "allow_force_pushes": false,
  "allow_deletions": false
}
```

**關鍵設定說明**:
- 需要1個審查者批准
- 核心CI檢查必須通過
- 管理員可以繞過規則（緊急情況）
- 不強制要求Code Owner審查

### Feature分支保護規則

**目的**: 鼓勵快速迭代，但確保基本質量

```json
{
  "required_status_checks": {
    "strict": false,
    "contexts": [
      "quick-validation"
    ]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "required_approving_review_count": 0
  },
  "allow_force_pushes": true,
  "allow_deletions": true
}
```

**關鍵設定說明**:
- 僅需要基本驗證通過
- 不強制要求PR審查
- 允許強制推送（便於重寫歷史）
- 允許分支刪除

### Release分支保護規則

**目的**: 確保發佈候選的穩定性和完整性

```json
{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "version-check",
      "full-test-suite (unit)",
      "full-test-suite (integration)", 
      "full-test-suite (performance)",
      "full-test-suite (security)",
      "build-artifacts",
      "build-docker-images"
    ]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "required_approving_review_count": 2,
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true
  },
  "allow_force_pushes": false,
  "allow_deletions": false,
  "required_linear_history": true
}
```

**關鍵設定說明**:
- 最嚴格的檢查要求
- 必須通過完整測試套件
- 需要2個審查者和Code Owner批准
- 完全禁止強制推送和刪除

## 實施步驟

### 1. 使用GitHub CLI配置

```bash
# 安裝GitHub CLI
brew install gh

# 登錄GitHub
gh auth login

# 配置main分支保護
gh api repos/proerror77/monday/branches/main/protection \
  --method PUT \
  --field required_status_checks='{"strict":true,"contexts":["quick-validation","rust-tests","python-tests","integration-tests","performance-tests","security-scan"]}' \
  --field enforce_admins=true \
  --field required_pull_request_reviews='{"required_approving_review_count":2,"dismiss_stale_reviews":true,"require_code_owner_reviews":true,"require_last_push_approval":true}' \
  --field restrictions='{"users":[],"teams":["hft-core-team","senior-developers"]}' \
  --field allow_force_pushes=false \
  --field allow_deletions=false \
  --field required_linear_history=true

# 配置develop分支保護
gh api repos/proerror77/monday/branches/develop/protection \
  --method PUT \
  --field required_status_checks='{"strict":true,"contexts":["quick-validation","rust-tests","python-tests","integration-tests"]}' \
  --field enforce_admins=false \
  --field required_pull_request_reviews='{"required_approving_review_count":1,"dismiss_stale_reviews":true,"require_code_owner_reviews":false}' \
  --field allow_force_pushes=false \
  --field allow_deletions=false
```

### 2. 使用GitHub Web界面配置

1. 進入GitHub倉庫頁面
2. 點擊 `Settings` → `Branches`
3. 點擊 `Add rule` 創建分支保護規則
4. 按照上述JSON配置設定每個分支的規則

### 3. 使用Terraform配置（推薦）

```hcl
# terraform/github-branch-protection.tf
resource "github_branch_protection" "main" {
  repository_id = "monday"
  pattern       = "main"

  required_status_checks {
    strict   = true
    contexts = [
      "quick-validation",
      "rust-tests", 
      "python-tests",
      "integration-tests",
      "performance-tests",
      "security-scan"
    ]
  }

  required_pull_request_reviews {
    required_approving_review_count = 2
    dismiss_stale_reviews           = true
    require_code_owner_reviews      = true
    require_last_push_approval      = true
  }

  enforce_admins         = true
  allow_force_pushes     = false
  allow_deletions        = false
  required_linear_history = true

  restrict_pushes {
    blocks_creations = false
    push_allowances  = ["hft-core-team", "senior-developers"]
  }
}

resource "github_branch_protection" "develop" {
  repository_id = "monday"
  pattern       = "develop"

  required_status_checks {
    strict   = true
    contexts = [
      "quick-validation",
      "rust-tests",
      "python-tests", 
      "integration-tests"
    ]
  }

  required_pull_request_reviews {
    required_approving_review_count = 1
    dismiss_stale_reviews           = true
    require_code_owner_reviews      = false
  }

  enforce_admins     = false
  allow_force_pushes = false
  allow_deletions    = false
}

resource "github_branch_protection" "release" {
  repository_id = "monday"
  pattern       = "release/*"

  required_status_checks {
    strict   = true
    contexts = [
      "version-check",
      "full-test-suite (unit)",
      "full-test-suite (integration)",
      "full-test-suite (performance)", 
      "full-test-suite (security)",
      "build-artifacts",
      "build-docker-images"
    ]
  }

  required_pull_request_reviews {
    required_approving_review_count = 2
    dismiss_stale_reviews           = true
    require_code_owner_reviews      = true
  }

  enforce_admins         = true
  allow_force_pushes     = false
  allow_deletions        = false
  required_linear_history = true
}
```

## Code Owners配置

創建`.github/CODEOWNERS`文件定義代碼審查責任：

```
# Global owners (fallback)
* @hft-core-team

# Rust核心引擎
/rust_hft/src/core/ @rust-experts @performance-team
/rust_hft/src/engine/ @rust-experts @trading-team
/rust_hft/src/ml/ @ml-team @rust-experts

# Python代理系統
/agno_hft/ @python-team @ai-team

# 性能關鍵代碼
/rust_hft/src/utils/performance.rs @performance-team
/rust_hft/benches/ @performance-team

# CI/CD配置
/.github/ @devops-team @hft-core-team

# 文檔
/CLAUDE.md @hft-core-team @tech-writers
/README.md @hft-core-team @tech-writers

# 配置文件
/rust_hft/config/ @devops-team @trading-team
/docker-compose.yml @devops-team
```

## 緊急繞過流程

### 情況1: 生產緊急修復

1. 創建hotfix分支從main
2. 進行必要修復
3. 申請緊急合併權限
4. 快速審查並合併
5. 事後補充完整測試

### 情況2: 關鍵依賴更新

1. 創建專用分支
2. 更新依賴並測試
3. 申請expedited review
4. 加速審查流程

### 情況3: 安全漏洞修復

1. 私有分支進行修復
2. 安全團隊審查
3. 緊急部署流程
4. 公開修復後的代碼

## 監控和報告

### 分支保護合規性檢查

```bash
#!/bin/bash
# 檢查所有分支保護規則是否正確配置

REPO="proerror77/monday"

echo "檢查分支保護規則合規性..."

for branch in main develop; do
  echo "檢查 $branch 分支..."
  gh api repos/$REPO/branches/$branch/protection \
    --jq '.required_status_checks.contexts[]' \
    | while read context; do
      echo "  - 必需檢查: $context"
    done
done
```

### 違規報告

定期生成分支保護規則違規報告，包括：
- 繞過審查的提交
- 強制推送事件  
- 規則修改歷史
- 合規性評分

## 最佳實踐

1. **漸進式保護**: 從寬鬆到嚴格逐步收緊規則
2. **團隊培訓**: 確保所有開發者理解規則目的
3. **定期審查**: 每季度檢查和更新保護規則
4. **自動化監控**: 設置告警監控規則變更
5. **文檔同步**: 保持文檔與實際配置一致

## 常見問題

**Q: 為什麼性能測試失敗但可以合併到develop？**
A: develop分支允許適度靈活性，但main分支必須通過所有檢查。

**Q: 如何處理CI系統故障導致無法合併？**
A: 管理員可以臨時調整規則或使用緊急繞過流程。

**Q: Code Owner不在時如何審查？**
A: 可以指定備用審查者或臨時調整CODEOWNERS文件。