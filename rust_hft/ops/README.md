# HFT 系統運維指南

本目錄包含 HFT 系統的運維相關資產，包括部署、監控、數據庫等基礎設施組件。

## 目錄結構

```
ops/
├── clickhouse/          # ClickHouse 配置和初始化腳本
│   ├── config/         # ClickHouse 伺服器配置
│   ├── users/          # 用戶和權限配置
│   ├── init.sql        # 數據庫初始化腳本
│   └── multi_symbol_tables.sql
├── monitoring/         # Prometheus + Grafana 監控堆疊
│   ├── grafana/        # Grafana 配置和儀表板
│   └── prometheus.yml  # Prometheus 配置
├── deployment/         # 部署腳本和配置
├── docker-compose.yml  # 完整開發環境
└── docker-compose.test.yml  # 測試環境
```

## 快速開始

### 啟動完整開發環境

```bash
# 從項目根目錄執行
make dev
# 或手動執行
cd ops && docker-compose up -d
```

### 啟動測試環境

```bash
make test
# 或手動執行
cd ops && docker-compose -f docker-compose.test.yml up -d
```

### 停止所有服務

```bash
make down
# 或手動執行
cd ops && docker-compose down
```

## 服務說明

### ClickHouse 數據庫

- **端口**: 8123 (HTTP), 9000 (Native)
- **用途**: 歷史市場數據存儲和查詢
- **配置**: `clickhouse/config/`
- **初始化**: `clickhouse/init.sql`

### Prometheus 監控

- **端口**: 9090
- **用途**: 系統指標收集和存儲
- **配置**: `monitoring/prometheus.yml`

### Grafana 儀表板

- **端口**: 3000
- **用途**: 系統監控可視化
- **默認登錄**: admin/admin
- **配置**: `monitoring/grafana/`

### ClickHouse 指標端點

- **端口**: 9363 (`/metrics`)
- **用途**: 暴露 ClickHouse 內建 Prometheus 指標（system.metrics/events 等）

## 常用運維命令

### 數據庫管理

```bash
# 連接到 ClickHouse
clickhouse-client --host localhost --port 9000

# 查看表結構
DESCRIBE spot_books15;

# 清理舊數據（30天前）
DELETE FROM spot_books15 WHERE ts < now() - INTERVAL 30 DAY;
```

### 監控檢查

```bash
# 檢查 Prometheus 目標狀態
curl http://localhost:9090/api/v1/targets

# 檢查系統核心指標
curl http://localhost:9090/api/v1/query?query=hft_exec_latency_ms_p99

# 檢查數據攝取延遲
curl http://localhost:9090/api/v1/query?query=ingest_lag_ms
```

### 容器管理

```bash
# 檢查服務狀態
docker-compose ps

# 查看服務日誌
docker-compose logs -f clickhouse
docker-compose logs -f prometheus

# 重啟特定服務（示例）
docker-compose restart clickhouse

# 清理數據卷（謹慎使用）
docker-compose down -v
```

## 部署配置

### 環境變量

在 `.env` 文件中配置以下變量：

```bash
# ClickHouse 配置
CLICKHOUSE_USER=default
CLICKHOUSE_PASSWORD=your_password

# Grafana 配置
GRAFANA_ADMIN_PASSWORD=your_admin_password

# API 密鑰
BITGET_API_KEY=your_api_key
BITGET_SECRET=your_secret
```

### 生產環境部署

參見 `deployment/` 目錄中的 Kubernetes 部署配置。

## 監控告警

### 關鍵指標

- `hft_exec_latency_ms_p99 > 25` - 執行延遲過高
- `ingest_lag_ms > 500` - 數據攝取延遲
- `pos_dd > 0.05` - 持倉回撤過大
- `ws_reconnect_cnt > 3/h` - WebSocket 連接不穩定

### 告警規則

詳細的告警規則配置見 `monitoring/prometheus_alerts.yaml`。

## 故障排除

### 常見問題

1. **ClickHouse 連接失敗**
   - 檢查防火牆設置
   - 確認配置文件中的監聽地址
   - 驗證用戶權限設置

2. **Grafana 儀表板無數據**
   - 確認 Prometheus 數據源配置
   - 檢查應用程序是否正確暴露指標
   - 驗證網絡連通性

3. **內存使用過高**
   - 調整 ClickHouse 的 `max_memory_usage` 參數
   - 優化查詢和數據保留策略
   - 考慮增加系統內存

### 日誌位置

- ClickHouse: `/var/lib/clickhouse/logs/`
- Prometheus: Docker 容器日誌
- Grafana: Docker 容器日誌

## 安全注意事項

1. 更改默認密碼
2. 配置防火牆規則
3. 啟用 TLS/SSL 加密
4. 定期備份關鍵數據
5. 監控異常訪問模式

## 支持和聯繫

如遇到運維問題，請查看：
1. 系統日誌和監控指標
2. 本文檔的故障排除部分
3. 項目 GitHub Issues
