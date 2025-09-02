# HFT Scripts 使用指南

## 新的腳本組織結構

為了簡化系統管理，我們已將大部分常用操作遷移到根目錄的 `Makefile`。

## 推薦的新命令

### 基礎設施管理
```bash
# 替代舊的 docker-compose 命令
make dev          # 啟動完整開發環境
make down         # 停止所有服務
make status       # 檢查服務狀態
make restart      # 重啟服務
make logs         # 查看服務日誌
```

### 應用構建和運行
```bash
make build        # 構建所有應用
make build-apps   # 構建所有 apps
make run-live     # 運行真盤應用
make run-paper    # 運行模擬盤應用
make run-replay   # 運行歷史回放
```

### 測試
```bash
make test-unit    # 運行單元測試
make test-all     # 運行所有測試
make check        # 檢查代碼質量
```

### 數據庫管理
```bash
make db-init      # 初始化數據庫
make db-connect   # 連接到 ClickHouse
make health       # 檢查系統健康狀態
```

## 仍然保留的專用腳本

這些腳本提供特殊功能，無法用 Makefile 替代：

- `start_test_environment.sh` - 完整的測試環境啟動和健康檢查
- `run_comprehensive_db_test.sh` - 完整的數據庫壓力測試
- `run_test_suite.sh` - 完整的測試套件（統一入口）
- `run_full_test_suite.sh` - 向後相容 shim，轉呼叫 `run_test_suite.sh`
- `verify_project_structure.sh` - 項目結構驗證

## 遷移指南

| 舊命令 | 新命令 |
|--------|--------|
| `docker-compose up -d` | `make dev` |
| `docker-compose down` | `make down` |
| `docker-compose ps` | `make status` |
| `docker-compose logs -f` | `make logs` |
| `cargo build --release` | `make build` |
| `cargo test` | `make test-all` |

## 基礎設施文件位置

所有基礎設施相關文件已移動到 `ops/` 目錄：
- `ops/docker-compose.yml` - 開發環境配置
- `ops/clickhouse/` - ClickHouse 配置
- `ops/monitoring/` - Prometheus + Grafana 配置
