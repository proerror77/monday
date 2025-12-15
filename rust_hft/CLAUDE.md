# HFT 系統狀態與設計總覽 (CLAUDE.md)

**版本**: 6.0 (2025-12-15)
**狀態**: 🟢 **Rust-Centric 閉環架構完成，端到端測試通過**

---

## 1. Executive Summary

系統已完成從「Python 多服務架構」到「Rust-Centric 閉環架構」的重構。

**核心變更**:
- ❌ 移除 `control_ws` (Python 控制工作區)
- ✅ 新增 `Sentinel` (Rust 自動化風控哨兵)
- ✅ 新增 `ModelManager` (Rust 模型熱加載)
- ✅ 新增真實延遲統計 (p50/p95/p99)
- ✅ 新增 PnL/DD 回撤追蹤整合 Sentinel

**新架構優勢**:
- **微秒級響應**: Sentinel 完全在 Rust 內執行，無 Python/gRPC 延遲
- **文件系統通訊**: 模型通過文件系統熱加載，取代複雜的 Redis Pub/Sub
- **簡化部署**: 單一 Rust 二進制 + 定時訓練任務
- **完整可觀測性**: 延遲分階段統計、回撤追蹤、uptime 監控

---

## 2. 系統架構

```
┌─────────────────────────────────────────────────────────────┐
│                    Rust HFT 核心引擎                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────────┐  │
│  │  數據接入 │→│  訂單簿   │→│  策略執行 │→│   執行網關   │  │
│  │ (WS/REST)│  │ (Engine) │  │(Strategy)│  │(Execution) │  │
│  └──────────┘  └──────────┘  └──────────┘  └─────────────┘  │
│       ↓              ↓             ↓              ↓         │
│  ┌──────────────────────────────────────────────────────┐   │
│  │              Sentinel (自動化風控)                    │   │
│  │  • 延遲監控 (latency_guard) - p50/p95/p99            │   │
│  │  • 回撤監控 (dd_guard) - 實時 PnL/DD 追蹤            │   │
│  │  • 自動 Degrade/Stop/EmergencyExit                   │   │
│  └──────────────────────────────────────────────────────┘   │
│       ↓                                                     │
│  ┌──────────────────────────────────────────────────────┐   │
│  │           ModelManager (模型熱加載)                   │   │
│  │  • 監控 models/current/ 目錄                         │   │
│  │  • 自動加載 .pt/.onnx 模型                           │   │
│  │  • 版本管理和回滾                                    │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                              ↑
                    (文件系統熱加載)
                              ↑
┌─────────────────────────────────────────────────────────────┐
│                 ml_trainer (定時訓練)                        │
│  ┌──────────────────────────────────────────────────────┐   │
│  │  ClickHouse → 特徵工程 → LSTM+Attention → 評估 → 部署  │   │
│  │                                                      │   │
│  │  部署條件: IC ≥ 0.03 ∧ IR ≥ 1.2 ∧ MaxDD ≤ 5%         │   │
│  │  輸出: models/current/strategy_dl.pt                 │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                              ↑
                     (每日批次查詢)
                              ↑
┌─────────────────────────────────────────────────────────────┐
│                    ClickHouse (數據存儲)                     │
│  • spot_books15: 訂單簿快照                                 │
│  • spot_trades: 成交記錄                                    │
│  • hft_features_*: 39維本地訂單簿特徵                       │
└─────────────────────────────────────────────────────────────┘
```

---

## 3. 核心模組狀態

| 模組 | 路徑 | 狀態 | 說明 |
|------|------|------|------|
| **Sentinel** | `risk-control/risk/src/sentinel.rs` | ✅ 完成 | Rust 自動化風控，取代 Python control_ws |
| **ModelManager** | `infra-services/core/model-manager/` | ✅ 完成 | 模型目錄監控、熱加載、版本管理 |
| **Live App 整合** | `apps/live/src/helpers/` | ✅ 完成 | Sentinel + ModelManager 已整合 |
| **延遲統計** | `market-core/engine/src/lib.rs` | ✅ 完成 | EngineLatencyStats (p50/p95/p99) |
| **回撤追蹤** | `risk-control/portfolio-core/src/lib.rs` | ✅ 完成 | AccountView 整合 high_water_mark/drawdown |
| **ml_trainer** | `ml_trainer/` | ✅ 完成 | 精簡訓練腳本，支持 ClickHouse 和合成數據 |
| **Docker 部署** | `deploy/docker-compose.yml` | ✅ 完成 | ClickHouse + 可選 Redis |

---

## 4. 最新實現功能

### 4.1. 延遲統計 (EngineLatencyStats)

```rust
pub struct EngineLatencyStats {
    pub end_to_end_p50_us: u64,
    pub end_to_end_p95_us: u64,
    pub end_to_end_p99_us: u64,
    pub ingestion_p99_us: u64,
    pub aggregation_p99_us: u64,
    pub strategy_p99_us: u64,
    pub risk_p99_us: u64,
    pub execution_p99_us: u64,
    pub sample_count: u64,
}
```

### 4.2. Sentinel 統計 (SentinelStats)

```rust
pub struct SentinelStats {
    pub latency_p99_us: u64,
    pub latency_p50_us: u64,
    pub pnl: f64,
    pub unrealized_pnl: f64,
    pub drawdown_pct: f64,
    pub max_drawdown_pct: f64,
    pub high_water_mark: f64,
}
```

### 4.3. 回撤追蹤 (AccountView)

```rust
pub struct AccountView {
    pub cash_balance: Decimal,
    pub positions: HashMap<Symbol, Position>,
    pub unrealized_pnl: Decimal,
    pub realized_pnl: Decimal,
    pub high_water_mark: Decimal,      // 新增
    pub drawdown_pct: f64,             // 新增
    pub max_drawdown_pct: f64,         // 新增
    pub session_start_us: u64,         // 新增
}
```

---

## 5. 閉環流程

### 5.1. 訓練流程 (Cron Job)

```bash
# 每日凌晨 2:00 執行
0 2 * * * cd /opt/hft && python ml_trainer/train.py >> /var/log/hft-trainer.log 2>&1
```

1. 從 ClickHouse 加載 T-7 日數據
2. 計算 39 維本地訂單簿特徵
3. 訓練 LSTM+Attention 模型
4. 評估 IC/IR/Sharpe/MaxDD
5. 達標後部署到 `models/current/strategy_dl.pt`
6. 更新 `models/metadata.json`

### 5.2. 熱加載流程 (Real-time)

1. `ModelWatcher` 監控 `models/current/` 目錄
2. 檢測到新 `.pt` 或 `.onnx` 文件
3. 驗證版本號
4. 加載到 ONNX Runtime / tch-rs
5. 原子切換模型引用

### 5.3. 風控流程 (100ms 間隔)

1. `Sentinel` 每 100ms 檢查系統狀態
2. 從 Engine 獲取 `SentinelStats` (延遲 + PnL/DD)
3. 根據閾值觸發動作:
   - `Continue`: 正常運行
   - `Warn`: 記錄警告
   - `Degrade`: 降頻模式 (減少交易頻率)
   - `Stop`: 停止交易
   - `EmergencyExit`: 緊急平倉

---

## 6. 配置參數

### 6.1. Sentinel 配置

```rust
SentinelConfig {
    // 延遲閾值 (microseconds)
    latency_warn_us: 15_000,      // 15ms 警告
    latency_degrade_us: 25_000,   // 25ms 降頻
    latency_stop_us: 50_000,      // 50ms 停止

    // 回撤閾值 (百分比)
    drawdown_warn_pct: 2.0,       // 2% 警告
    drawdown_degrade_pct: 3.0,    // 3% 降頻
    drawdown_stop_pct: 5.0,       // 5% 停止
    drawdown_emergency_pct: 7.0,  // 7% 緊急平倉

    // 恢復條件
    recovery_latency_below_us: 10_000,
    recovery_cooldown_secs: 300,
}
```

### 6.2. 模型部署條件

```yaml
deployment:
  min_ic: 0.03        # 最小 Information Coefficient
  min_ir: 1.2         # 最小 Information Ratio
  max_drawdown: 0.05  # 最大回撤 5%
```

### 6.3. Live App 命令行參數

```bash
cargo run -p hft-live -- \
  --sentinel-enable=true \
  --sentinel-interval-ms=100 \
  --sentinel-latency-warn-us=15000 \
  --sentinel-drawdown-stop-pct=5.0 \
  --ml-enable=true \
  --ml-model=models/current/strategy_dl.onnx
```

---

## 7. 目錄結構

```
monday/
├── rust_hft/                          # Rust HFT 核心
│   ├── apps/live/                     # 真盤應用
│   │   └── src/helpers/
│   │       ├── inference.rs           # ONNX 推理 + 熱加載
│   │       ├── sentinel.rs            # Sentinel 整合
│   │       └── metrics.rs             # Prometheus 指標
│   ├── market-core/engine/
│   │   └── src/lib.rs                 # EngineLatencyStats, SentinelStats
│   ├── risk-control/
│   │   ├── risk/src/sentinel.rs       # Sentinel 核心邏輯
│   │   └── portfolio-core/src/lib.rs  # 回撤追蹤
│   └── infra-services/core/
│       └── model-manager/             # 模型熱加載管理
│
├── ml_trainer/                        # Python 訓練腳本
│   ├── train.py                       # 主訓練腳本
│   ├── config.yaml                    # 配置文件
│   ├── test_e2e.py                    # 端到端測試
│   └── requirements.txt               # 依賴
│
├── models/                            # 模型目錄
│   ├── current/                       # 當前使用的模型
│   │   └── strategy_dl.pt
│   ├── archive/                       # 歷史版本
│   └── metadata.json                  # 模型元數據
│
└── deploy/
    └── docker-compose.yml             # 部署配置
```

---

## 8. 運維指南

### 8.1. 啟動流程

```bash
# 1. 啟動基礎設施
cd deploy && docker compose up -d clickhouse

# 2. 編譯並啟動 Rust 核心
cd rust_hft && cargo run -p hft-live --release

# 3. 設置定時訓練
crontab -e
# 添加: 0 2 * * * cd /opt/hft && python ml_trainer/train.py
```

### 8.2. 手動訓練

```bash
cd ml_trainer
source .venv/bin/activate
python train.py --dry-run  # 測試模式
python train.py            # 正式訓練
```

### 8.3. 模型回滾

```bash
# 查看歷史版本
ls models/archive/

# 手動回滾
cp models/archive/strategy_dl_20251213_100000.pt models/current/strategy_dl.pt
```

---

## 9. 測試狀態

| 測試 | 狀態 | 說明 |
|------|------|------|
| Sentinel 單元測試 | ✅ 5/5 通過 | `cargo test -p hft-risk sentinel` |
| ModelManager 單元測試 | ✅ 3/3 通過 | `cargo test -p hft-model-manager` |
| ml_trainer dry-run | ✅ 通過 | 使用合成數據測試 |
| E2E 測試 | ✅ 通過 | 訓練→部署→驗證完整流程 |
| Release Build | ✅ 通過 | `cargo build -p hft-live --release` |
| Clippy 檢查 | ✅ 0 警告 | `cargo clippy --workspace` |

---

## 10. 代碼質量

### 10.1. 已完成優化

- ✅ 所有 clippy 警告已修復
- ✅ unsafe 函數添加 Safety 文檔
- ✅ 複雜類型使用 type alias 簡化
- ✅ 適當使用 derive macros
- ✅ WebSocket 常量集中管理

### 10.2. 剩餘 TODO 項目

| 類別 | 數量 | 說明 |
|------|------|------|
| Phase 2 功能 | 3 | secrets 後端 (Vault/AWS/Local) |
| 測試框架 | 15 | adapter 和 e2e 測試存根 |
| IPC 處理器 | 10 | 系統控制命令 (DefaultHandler) |
| gRPC 服務 | 2 | 模型加載和風控更新 |

---

## 11. 下一步計劃

1. **gRPC 模型加載**: 實現下載、驗證、加載完整流程
2. **風控參數動態更新**: 添加 RiskManager trait 方法
3. **測試覆蓋**: 完善 adapter 和 e2e 測試
4. **ONNX 推理優化**: GPU 加速支持
5. **生產部署**: K8s Helm Chart
