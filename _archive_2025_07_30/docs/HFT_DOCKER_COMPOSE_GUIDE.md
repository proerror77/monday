# HFT Docker Compose Stack 部署指南

## 🎯 概述

根據 **Agno 官方 GitHub Issue #2661** 的確認，手動創建 Docker Compose 來管理 Agno workspaces 是完全符合規範且被推薦的方法。這個 Docker Compose stack 提供了比分散容器管理更優雅、高效的解決方案。

## ✨ 主要優勢

### 相比現有方式的改進：

| 方面 | 舊方式 (分散容器) | 新方式 (Docker Compose Stack) |
|------|------------------|------------------------------|
| **管理複雜度** | 需要手動管理 11+ 個容器 | 一個配置文件統一管理 |
| **網絡配置** | 手動創建多個網絡 | 自動管理服務間通信 |
| **依賴管理** | 手動處理啟動順序 | 自動處理服務依賴 |
| **健康檢查** | 基本狀態檢查 | 完整健康檢查和自動重啟 |
| **擴展性** | 困難 | 輕鬆擴展和修改 |
| **環境一致性** | 配置分散 | 統一環境變數管理 |

## 🚀 快速開始

### 1. 系統要求
```bash
# 檢查 Docker 版本 (需要支持 Compose v2)
docker --version
docker compose version
```

### 2. 環境配置
```bash
# 複製並編輯環境變數
cp .env.hft .env
# 編輯 .env 文件，設置您的 API 密鑰
nano .env
```

### 3. 基本操作
```bash
# 啟動所有服務
./hft-compose.sh up

# 查看狀態
./hft-compose.sh status

# 查看日誌
./hft-compose.sh logs

# 健康檢查
./hft-compose.sh health

# 停止所有服務
./hft-compose.sh down
```

## 🏗️ 架構設計

### 服務組織結構
```
HFT Docker Compose Stack
├── Infrastructure (共享)
│   ├── hft-redis (端口: 6379)
│   └── hft-clickhouse (端口: 8123, 9000)
├── Master Workspace (端口: 8504)
│   ├── hft-master-db (端口: 5432)
│   ├── hft-master-ui (端口: 8504)
│   └── hft-master-api (端口: 8002)
├── Ops Workspace (端口: 8503)
│   ├── hft-ops-db (端口: 5434)
│   ├── hft-ops-ui (端口: 8503)
│   └── hft-ops-api (端口: 8003)
└── ML Workspace (端口: 8502)
    ├── hft-ml-db (端口: 5433)
    ├── hft-ml-ui (端口: 8502)
    └── hft-ml-api (端口: 8001)
```

### 網絡設計
- **hft-shared**: 基礎設施網絡，供所有服務共享資源
- **hft-master**: Master workspace 內部網絡
- **hft-ops**: Operations workspace 內部網絡  
- **hft-ml**: ML workspace 內部網絡

## 📋 詳細命令參考

### 服務管理
```bash
# 啟動特定工作區
./hft-compose.sh up master     # 只啟動 Master 工作區
./hft-compose.sh up ops        # 只啟動 Ops 工作區
./hft-compose.sh up ml         # 只啟動 ML 工作區
./hft-compose.sh up infra      # 只啟動基礎設施

# 重啟服務
./hft-compose.sh restart                    # 重啟所有服務
./hft-compose.sh restart hft-master-ui      # 重啟特定服務

# 停止服務
./hft-compose.sh down          # 停止所有服務
./hft-compose.sh down master   # 停止 Master 工作區
```

### 監控和調試
```bash
# 查看實時日誌
./hft-compose.sh logs                     # 所有服務日誌
./hft-compose.sh logs hft-master-ui       # 特定服務日誌
./hft-compose.sh logs hft-master-ui 100   # 最後 100 行日誌

# 系統狀態
./hft-compose.sh status        # 詳細狀態報告
./hft-compose.sh health        # 健康檢查
```

### 維護操作
```bash
# 重新構建鏡像
./hft-compose.sh build

# 清理系統
./hft-compose.sh cleanup       # 清理未使用的資源
```

## 🔧 高級配置

### 自定義環境變數
```bash
# .env 文件中的關鍵配置
COMPOSE_PROJECT_NAME=hft-system
OPENAI_API_KEY=your_key_here
RUNTIME_ENV=dev
DEBUG_MODE=true
```

### 服務擴展
如需擴展服務，編輯 `docker-compose.hft.yml`：
```yaml
# 添加新服務
hft-new-service:
  build:
    context: ./new-workspace
  ports:
    - "8505:8505"
  networks:
    - hft-shared
```

### 資源限制
```yaml
# 在服務定義中添加資源限制
deploy:
  resources:
    limits:
      cpus: '2.0'
      memory: 2G
    reservations:
      cpus: '1.0'
      memory: 1G
```

## 🚦 生產部署建議

### 1. 安全配置
```bash
# 生產環境設置
ENVIRONMENT=production
DEBUG_MODE=false

# 使用 Docker Secrets 管理敏感信息
docker secret create openai_key openai_key.txt
```

### 2. 性能優化
- 使用 SSD 存儲 volumes
- 配置適當的內存和 CPU 限制
- 啟用 Docker 日誌輪轉

### 3. 監控集成
- 集成 Prometheus/Grafana
- 配置日誌聚合
- 設置告警規則

## 🔄 遷移指南

### 從現有容器遷移到 Compose Stack

#### 1. 備份數據
```bash
# 備份數據庫
docker exec hft-master-db pg_dump -U ai ai > master_backup.sql
docker exec hft-ops-db pg_dump -U ai ai > ops_backup.sql
docker exec hft-ml-db pg_dump -U ai ai > ml_backup.sql
```

#### 2. 停止現有容器
```bash
# 使用現有的管理工具停止容器
python docker_hft_manager.py stop master
python docker_hft_manager.py stop ops
python docker_hft_manager.py stop ml
```

#### 3. 啟動新 Stack
```bash
# 啟動新的 Compose stack
./hft-compose.sh up
```

#### 4. 恢復數據（如需要）
```bash
# 恢復數據庫
docker exec -i hft-master-db psql -U ai ai < master_backup.sql
```

## 📊 監控和告警

### 內置健康檢查
每個服務都配置了適當的健康檢查：
- **數據庫**: PostgreSQL 連接檢查
- **Redis**: Ping 檢查
- **ClickHouse**: HTTP endpoint 檢查
- **UI 服務**: Streamlit health endpoint
- **API 服務**: FastAPI health endpoint

### 資源監控
```bash
# 實時資源使用情況
docker stats $(docker compose -f docker-compose.hft.yml ps -q)

# 服務狀態監控
watch -n 5 './hft-compose.sh status'
```

## 🛠️ 故障排除

### 常見問題

#### 1. 端口衝突
```bash
# 檢查端口使用情況
lsof -i :8504
netstat -tulpn | grep 8504
```

#### 2. 服務啟動失敗
```bash
# 查看詳細日誌
./hft-compose.sh logs service-name
docker compose -f docker-compose.hft.yml logs service-name
```

#### 3. 數據庫連接問題
```bash
# 測試數據庫連接
docker exec hft-master-db pg_isready -U ai
```

#### 4. 網絡問題
```bash
# 檢查網絡配置
docker network ls
docker network inspect hft-shared
```

## 📞 支持和貢獻

這個 Docker Compose stack 完全符合 Agno 官方規範，基於：
- **Agno GitHub Issue #2661** 的官方確認
- **Docker Compose 2025 最佳實踐**
- **Production-ready 安全和性能配置**

如有問題或建議，請參考：
- Agno 官方文檔：https://docs.agno.com
- Agno GitHub：https://github.com/agno-agi/agno
- Docker Compose 文檔：https://docs.docker.com/compose/

---

**總結**: 這個 Docker Compose stack 提供了一個完整、可擴展、符合 Agno 規範的 HFT 系統部署解決方案，大大簡化了管理複雜度並提高了系統可靠性。