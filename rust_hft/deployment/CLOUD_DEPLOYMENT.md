# HFT 系統雲端部署指南

## 1. 服務拆分架構

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           HFT 系統雲端部署架構                               │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    Layer 1: 交易核心 (低延遲區)                      │   │
│  │                   部署位置: 交易所附近 (新加坡/東京)                  │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                  │   │
│  │  │ Trading     │  │ Market Data │  │ Execution   │                  │   │
│  │  │ Engine      │  │ Ingestion   │  │ Gateway     │                  │   │
│  │  │ (hft-live)  │  │ (WS連接)    │  │ (下單)      │                  │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘                  │   │
│  │        │                │                │                          │   │
│  │        └────────────────┼────────────────┘                          │   │
│  │                         │ Redis (本地)                              │   │
│  └─────────────────────────┼───────────────────────────────────────────┘   │
│                            │                                               │
│  ┌─────────────────────────┼───────────────────────────────────────────┐   │
│  │                    Layer 2: 監控與風控                               │   │
│  │                   部署位置: 同區域或鄰近區域                          │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                  │   │
│  │  │ Sentinel    │  │ Prometheus  │  │ Grafana     │                  │   │
│  │  │ Monitor     │  │ Metrics     │  │ Dashboard   │                  │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘                  │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    Layer 3: 數據與存儲                               │   │
│  │                   部署位置: 任意區域 (成本優化)                       │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                  │   │
│  │  │ ClickHouse  │  │ PostgreSQL  │  │ Object      │                  │   │
│  │  │ (時序數據)  │  │ (配置/狀態) │  │ Storage     │                  │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘                  │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                    Layer 4: ML 訓練 (批次)                           │   │
│  │                   部署位置: GPU 區域 (按需啟動)                       │   │
│  │  ┌─────────────┐  ┌─────────────┐                                   │   │
│  │  │ ML Training │  │ Model       │                                   │   │
│  │  │ Pipeline    │  │ Registry    │                                   │   │
│  │  └─────────────┘  └─────────────┘                                   │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

## 2. 服務拆分說明

| 服務 | 容器/Pod | 資源需求 | 部署策略 | 擴展性 |
|------|----------|----------|----------|--------|
| **Trading Engine** | `hft-trading` | 4 CPU, 8GB RAM | 單實例, 高可用 | 不擴展 |
| **Market Data** | `hft-market-data` | 2 CPU, 4GB RAM | 按交易所拆分 | 水平擴展 |
| **Execution Gateway** | `hft-execution` | 2 CPU, 4GB RAM | 按交易所拆分 | 水平擴展 |
| **Sentinel Monitor** | `hft-sentinel` | 1 CPU, 2GB RAM | 單實例 | 不擴展 |
| **Redis** | `redis` | 2 CPU, 4GB RAM | 單實例/集群 | 視需求 |
| **ClickHouse** | `clickhouse` | 4 CPU, 16GB RAM | 單實例/集群 | 視數據量 |
| **Prometheus** | `prometheus` | 2 CPU, 4GB RAM | 單實例 | 不擴展 |
| **Grafana** | `grafana` | 1 CPU, 2GB RAM | 單實例 | 不擴展 |
| **ML Training** | `ml-trainer` | GPU (按需) | CronJob | 按需 |

## 3. 雲端選擇建議

### 3.1 AWS 部署 (推薦)

```yaml
# 區域選擇 (按交易所位置)
Bitget/Binance/Bybit: ap-southeast-1 (新加坡)
Hyperliquid: us-east-1 (維吉尼亞)
Global: 多區域部署

# 實例類型
Trading Engine: c6i.xlarge (計算優化, 低延遲)
Market Data: c6i.large
ClickHouse: r6i.xlarge (記憶體優化)
ML Training: p3.2xlarge (GPU, 按需)
```

### 3.2 GCP 部署

```yaml
# 區域選擇
asia-southeast1 (新加坡)
us-central1 (美國中部)

# 實例類型
Trading Engine: c2-standard-4
Market Data: c2-standard-2
ClickHouse: n2-highmem-4
ML Training: a2-highgpu-1g (按需)
```

## 4. 延遲優化策略

### 4.1 網路優化
- 使用專用主機 (Dedicated Hosts) 避免 noisy neighbors
- 啟用 Enhanced Networking (AWS ENA / GCP gVNIC)
- 使用 Placement Groups 確保低延遲通訊
- TCP_NODELAY 已在代碼中啟用

### 4.2 部署位置
```
交易所          推薦區域              預估延遲
─────────────────────────────────────────────
Binance         ap-southeast-1       < 5ms
Bitget          ap-southeast-1       < 5ms
Bybit           ap-southeast-1       < 5ms
Hyperliquid     us-east-1            < 10ms
```

## 5. 高可用設計

```
                    ┌─────────────┐
                    │   Route53   │
                    │  /CloudDNS  │
                    └──────┬──────┘
                           │
              ┌────────────┴────────────┐
              │                         │
      ┌───────▼───────┐         ┌───────▼───────┐
      │  Region A     │         │  Region B     │
      │  (Primary)    │         │  (Standby)    │
      │               │         │               │
      │ ┌───────────┐ │         │ ┌───────────┐ │
      │ │ Trading   │ │   同步   │ │ Trading   │ │
      │ │ Engine    │◄├─────────►│ Engine    │ │
      │ └───────────┘ │         │ └───────────┘ │
      │               │         │               │
      │ ┌───────────┐ │         │ ┌───────────┐ │
      │ │ Redis     │◄├─────────►│ Redis     │ │
      │ └───────────┘ │ 複製     │ └───────────┘ │
      └───────────────┘         └───────────────┘
```

## 6. 配置管理

### 6.1 環境變數
```bash
# 生產環境
HFT_ENV=production
HFT_LOG_LEVEL=info

# 交易所配置
BITGET_API_KEY=xxx
BITGET_API_SECRET=xxx
BINANCE_API_KEY=xxx
BINANCE_API_SECRET=xxx

# 基礎設施
REDIS_URL=redis://redis:6379
CLICKHOUSE_URL=http://clickhouse:8123
PROMETHEUS_GATEWAY=http://prometheus:9091

# 風控參數
MAX_POSITION_USD=100000
MAX_DRAWDOWN_PCT=5
KILL_SWITCH_ENABLED=true
```

### 6.2 Secrets 管理
- AWS: Secrets Manager / Parameter Store
- GCP: Secret Manager
- K8s: External Secrets Operator

## 7. 監控與告警

### 7.1 關鍵指標
```yaml
# Prometheus 告警規則
- alert: TradingEngineDown
  expr: up{job="hft-trading"} == 0
  for: 10s
  severity: critical

- alert: HighLatency
  expr: hft_exec_latency_p99 > 0.025
  for: 30s
  severity: warning

- alert: DrawdownExceeded
  expr: hft_portfolio_drawdown > 0.05
  for: 10s
  severity: critical

- alert: WebSocketDisconnected
  expr: hft_ws_connected == 0
  for: 30s
  severity: critical
```

### 7.2 日誌聚合
- 使用 Fluentd/Fluent Bit 收集日誌
- 發送到 CloudWatch Logs / Cloud Logging
- 設置關鍵字告警 (ERROR, PANIC, kill-switch)

## 8. 部署流程

### 8.1 CI/CD Pipeline
```yaml
# GitHub Actions
stages:
  - test         # 單元測試
  - build        # 構建容器
  - security     # 安全掃描
  - staging      # 部署到測試環境
  - production   # 部署到生產環境 (手動批准)
```

### 8.2 藍綠部署
```
1. 部署新版本到 Green 環境
2. 運行健康檢查和冒煙測試
3. 切換流量到 Green
4. 監控 5 分鐘
5. 確認無問題後刪除 Blue
```

## 9. 成本估算 (AWS)

| 組件 | 實例類型 | 月成本 (USD) |
|------|----------|--------------|
| Trading Engine | c6i.xlarge (Reserved) | ~$80 |
| Market Data x2 | c6i.large | ~$120 |
| ClickHouse | r6i.xlarge | ~$150 |
| Redis | r6i.large | ~$80 |
| Monitoring | t3.medium x2 | ~$60 |
| ML Training | p3.2xlarge (10h/month) | ~$30 |
| Network/Storage | - | ~$50 |
| **Total** | | **~$570/月** |

## 10. 快速開始

```bash
# 1. 構建所有容器
make docker-build-all

# 2. 推送到 ECR/GCR
make docker-push-all

# 3. 部署基礎設施
cd deployment/terraform
terraform init
terraform apply

# 4. 部署應用
cd deployment/k8s
kubectl apply -f namespace.yaml
kubectl apply -f secrets.yaml
kubectl apply -f .
```
