# HFT系統Docker資源整合遷移指南

## 📋 概覽

本指南詳細說明如何從現有的多個docker-compose配置遷移到統一的Docker資源架構。

## 🎯 解決的問題

### 原有問題
1. **端口衝突**: 多個Redis/ClickHouse實例使用相同端口
2. **網絡隔離問題**: 服務間無法正確通信
3. **配置重複**: 基礎設施服務在多個文件中重複定義
4. **資源浪費**: 同一服務的多個實例同時運行
5. **管理複雜**: 需要管理多個compose文件

### 解決方案
1. **統一架構**: 單一docker-compose.unified.yml文件
2. **分層網絡**: 6個專用網絡確保安全隔離
3. **服務依賴**: 基礎設施→Rust核心→Agno工作區的啟動順序
4. **資源優化**: 合理的CPU/內存限制配置
5. **環境統一**: .env.unified環境變量集中管理

## 🏗️ 新架構設計

### 分層架構
```
┌─────────────────────────────────────────┐
│              L3: ML Workspace           │
│         (Batch Training Agent)          │
├─────────────────────────────────────────┤
│            L2: Ops Workspace            │
│        (Real-time Monitoring)           │
├─────────────────────────────────────────┤
│           L1: Rust HFT Core             │
│         (Microsecond Trading)           │
├─────────────────────────────────────────┤
│          L0: Infrastructure             │
│    (Redis, ClickHouse, Prometheus)      │
└─────────────────────────────────────────┘
```

### 網絡分離
- `hft-infrastructure`: 基礎設施服務 (172.20.0.0/24)
- `hft-trading`: Rust核心與Ops監控 (172.21.0.0/24)
- `hft-monitoring`: Prometheus與Grafana (172.22.0.0/24)
- `hft-master`: Master工作區 (172.23.0.0/24)
- `hft-ops`: Ops工作區 (172.24.0.0/24)
- `hft-ml`: ML工作區 (172.25.0.0/24)

## 🚀 遷移步驟

### 步驟1: 停止現有服務
```bash
# 停止所有現有的Docker服務
docker-compose -f docker-compose.hft.yml down
docker-compose -f rust_hft/docker-compose.yml down
docker-compose -f deployment/docker-compose.yml down

# 清理孤立的容器和網絡
docker system prune -f
docker network prune -f
```

### 步驟2: 備份數據（可選）
```bash
# 備份現有數據卷
docker run --rm -v hft_redis_data:/source -v $(pwd)/backup:/backup alpine tar czf /backup/redis-backup.tar.gz -C /source .
docker run --rm -v hft_clickhouse_data:/source -v $(pwd)/backup:/backup alpine tar czf /backup/clickhouse-backup.tar.gz -C /source .
```

### 步驟3: 設置統一環境
```bash
# 複製統一環境配置
cp .env.unified .env

# 編輯環境變量（設置你的API密鑰）
nano .env
```

### 步驟4: 創建必要目錄
```bash
# 創建數據和配置目錄
mkdir -p data/{redis,clickhouse,prometheus,grafana}
mkdir -p logs/clickhouse
mkdir -p config/{prometheus,grafana}
```

### 步驟5: 啟動統一系統
```bash
# 使用管理腳本啟動完整系統
chmod +x docker-unified-manager.sh
./docker-unified-manager.sh start

# 或分步驟啟動
./docker-unified-manager.sh start-infra    # 基礎設施
./docker-unified-manager.sh start-rust     # Rust核心
./docker-unified-manager.sh start-workspaces # Agno工作區
```

## 🔧 配置說明

### 端口映射
| 服務 | 端口 | 說明 |
|------|------|------|
| Redis | 6379 | 實時消息隊列 |
| ClickHouse HTTP | 8123 | 時序數據查詢 |
| ClickHouse Native | 9000 | 原生客戶端 |
| Prometheus | 9090 | 監控指標 |
| Grafana | 3000 | 監控面板 |
| Rust gRPC | 50051 | 交易引擎API |
| Rust Metrics | 8080 | 性能指標 |
| Master UI | 8504 | 主控制台 |
| Master API | 8002 | 主控制台API |
| Ops UI | 8503 | 運維監控 |
| Ops API | 8003 | 運維API |
| ML UI | 8502 | 機器學習 |
| ML API | 8001 | 機器學習API |

### 資源限制
```yaml
# 生產環境建議配置
hft-rust-core:
  limits: { cpus: '6.0', memory: '8G' }
  reservations: { cpus: '4.0', memory: '4G' }

hft-clickhouse:
  limits: { cpus: '4.0', memory: '8G' }
  reservations: { cpus: '2.0', memory: '4G' }

hft-ml-trainer:
  limits: { cpus: '8.0', memory: '16G' }
  reservations: { cpus: '4.0', memory: '8G' }
```

## 📊 驗證遷移成功

### 1. 檢查服務狀態
```bash
./docker-unified-manager.sh status
```

### 2. 驗證網絡連通性
```bash
# 測試Redis連接
docker exec hft-rust-core redis-cli -h hft-redis ping

# 測試ClickHouse連接
docker exec hft-rust-core curl -s http://hft-clickhouse:8123/ping

# 測試gRPC連接
docker exec hft-ops-agent grpcurl -plaintext hft-rust-core:50051 list
```

### 3. 檢查服務端點
- Grafana: http://localhost:3000 (admin/hft123)
- Prometheus: http://localhost:9090
- Master Workspace: http://localhost:8504
- Ops Workspace: http://localhost:8503
- ML Workspace: http://localhost:8502

## 🛠️ 故障排除

### 常見問題

#### 1. 端口已被占用
```bash
# 檢查端口使用情況
sudo lsof -i :6379
sudo lsof -i :8123

# 停止占用服務
sudo fuser -k 6379/tcp
```

#### 2. 服務無法啟動
```bash
# 查看服務日志
./docker-unified-manager.sh logs hft-redis
./docker-unified-manager.sh logs hft-clickhouse

# 檢查健康狀態
docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
```

#### 3. 網絡連接問題
```bash
# 檢查網絡配置
docker network ls
docker network inspect hft-infrastructure

# 重建網絡
docker-compose --env-file .env.unified -f docker-compose.unified.yml down
docker network prune -f
./docker-unified-manager.sh start
```

## 📈 性能優化

### 1. 系統調優
```bash
# 調整網絡參數（需要root權限）
echo 'net.core.rmem_max = 134217728' >> /etc/sysctl.conf
echo 'net.core.wmem_max = 134217728' >> /etc/sysctl.conf
sysctl -p
```

### 2. Docker調優
```bash
# 增加Docker daemon配置
cat > /etc/docker/daemon.json << EOF
{
  "log-driver": "json-file",
  "log-opts": {
    "max-size": "10m",
    "max-file": "3"
  },
  "storage-driver": "overlay2"
}
EOF

sudo systemctl restart docker
```

## 🔄 回滾計劃

如果遇到問題需要回滾：

```bash
# 1. 停止統一系統
./docker-unified-manager.sh stop

# 2. 恢復原有配置
docker-compose -f docker-compose.hft.yml up -d

# 3. 恢復數據（如果有備份）
docker run --rm -v $(pwd)/backup:/backup -v hft_redis_data:/target alpine tar xzf /backup/redis-backup.tar.gz -C /target
```

## 📝 維護操作

### 日常維護
```bash
# 查看服務狀態
./docker-unified-manager.sh status

# 查看日志
./docker-unified-manager.sh logs

# 重啟服務
./docker-unified-manager.sh restart

# 備份數據
./docker-unified-manager.sh backup
```

### ML訓練任務
```bash
# 啟動訓練任務
./docker-unified-manager.sh ml-train

# 監控訓練進度
./docker-unified-manager.sh logs hft-ml-trainer
```

## ✅ 成功指標

遷移成功的標準：
- [ ] 所有服務健康檢查通過
- [ ] 網絡連通性測試成功
- [ ] Web界面可正常訪問
- [ ] 性能指標正常
- [ ] 數據持久化正常
- [ ] Agno工作區正常運行

## 📞 支持

如果遇到問題，請按以下順序排查：
1. 檢查日志文件
2. 驗證網絡配置
3. 確認資源限制
4. 查看Docker daemon狀態
5. 聯繫系統管理員