# Rust HFT × Agno 24/7 AI Trading Platform — Product Requirements Document

**Version 2.0 — 2025-07-08**

**變更記錄 (v2.0)**
- 基於現有代碼結構進行深度分析和重構設計
- 明確雙平面架構：Rust執行平面 + Python控制平面
- 詳細的模塊劃分和API設計
- 完整的部署和監控方案
- 階段性重構計劃

---

## 0 · 項目概述

### 目標
構建一個7×24小時運行的高頻加密貨幣交易系統，結合：
- **Rust執行平面**：微秒級決策引擎，熱備份架構
- **Python Agno控制平面**：七角色智能代理 + 機器學習生命週期管理
- **完整可觀測性**：Prometheus/Grafana + Supabase + Redis + PagerDuty/Feishu告警

### 核心指標
| 指標 | 目標 | 測量方法 |
|------|------|----------|
| **決策延遲** | ≤ 1μs P95 | Criterion bench |
| **模型推理** | ≤ 50μs P95 | 專用benchmark |
| **Python↔Rust RTT** | ≤ 5ms P95 | IPC測試 |
| **系統可用性** | ≥ 99.5% | 24小時壓力測試 |
| **風險控制** | ≤ 200 USDT最大回撤 | 實時監控 |

### 項目範圍
**包含功能：**
- Bitget WS V2 + REST API集成
- 高性能訂單簿和策略引擎
- Candle/PyTorch深度學習模型
- 七角色智能代理系統
- 完整的監控和告警
- 藍綠部署和CI/CD

**暫不包含：**
- 多交易所支持（僅Bitget）
- Web GUI界面
- 跨資產組合風險管理

---

## 1 · 現狀分析

### 1.1 現有代碼優勢
基於對現有代碼結構的分析，項目具備以下優勢：

**技術選型精良：**
- ✅ Rust核心語言，追求極致性能和內存安全
- ✅ ClickHouse時序數據庫，適合HFT海量數據
- ✅ Redis實時狀態管理和消息隊列
- ✅ Docker容器化部署

**功能覆蓋全面：**
- ✅ 數據採集、回測、模型訓練、實盤交易全鏈路
- ✅ 性能基準測試和優化報告
- ✅ SIMD優化和無鎖架構已實現
- ✅ 已達到10k+ msg/s吞吐量，<10μs處理延遲

### 1.2 關鍵問題
**代碼組織混亂：**
```
當前問題結構：
examples/                    # 19個重複example
examples_backup/             # 冗餘備份  
examples/old_replaced_examples/  # 廢棄代碼
```

**根本原因：**
1. "示例驅動開發"，業務邏輯散落在examples中
2. 缺乏統一入口點，多個main.rs並存
3. 文檔碎片化，缺乏權威參考
4. 庫與應用邊界模糊

---

## 2 · 系統架構

### 2.1 雙平面架構設計

```
┌─────────────────────────────────────────────────────────────────┐
│                    Python Control Plane                       │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌─────┐│
│  │SupervisorAgent│  │  TrainAgent  │  │ MonitorAgent │  │Chat ││
│  │   (HA/調度)   │  │  (ML訓練)   │  │  (系統監控)  │  │Agent││
│  └──────────────┘  └──────────────┘  └──────────────┘  └─────┘│
│                              │                                │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐        │
│  │  TradeAgent  │  │ ConfigAgent  │  │  RiskAgent   │        │
│  │  (策略執行)  │  │  (配置管理)  │  │  (風險控制)  │        │
│  └──────────────┘  └──────────────┘  └──────────────┘        │
└─────────────────────────┬───────────────────────────────────────┘
                         │ IPC Bus (UDS + MsgPack)
┌─────────────────────────┼───────────────────────────────────────┐
│                    Rust Execution Plane                      │
│  ┌─────────────────────┼───────────────────────────────────┐   │
│  │                Hot Path (<1μs)                         │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐    │   │
│  │  │ OrderBook   │  │ Strategy    │  │ Execution   │    │   │
│  │  │  Manager    │  │  Engine     │  │  Engine     │    │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘    │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │                  Cold Path (>1ms)                      │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐    │   │
│  │  │ Data        │  │ Risk        │  │ Persistence │    │   │
│  │  │ Collection  │  │ Manager     │  │ Layer       │    │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘    │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 架構設計原則

**嚴格分層隔離：**
1. **L1執行平面（Rust）**：擁有所有延遲敏感工作，100% Rust實現，零動態分配
2. **L2控制平面（Python）**：處理ML訓練、配置管理、監控告警等非延遲敏感任務
3. **通信限制**：僅通過UDS + MsgPack IPC通信，禁止跨層直接調用

**職責邊界清晰：**
| 平面 | 組件 | 運行時 | 硬實時 | 典型延遲 | 示例功能 |
|------|------|--------|--------|----------|----------|
| L1執行 | orderbook.rs | Rust線程池 | 是 | 100ns-5μs | update_book() |
| L1執行 | strategy.rs | Rust | 是 | 500ns-10μs | compute_quotes() |
| L1執行 | execution.rs | Rust async | 是 | 20μs-30ms | place_order() |
| L2控制 | TrainAgent | Python+PyO3 | 否 | 秒-小時 | pipeline_train() |
| L2控制 | TradeAgent | Python+PyO3 | 軟 | <5ms | reload_model() |
| L2控制 | MonitorAgent | Python | 軟 | 1-5s | check_latency() |

---

## 3 · 重構計劃

### 3.1 目標代碼結構

```
src/
├── main.rs              # 統一CLI入口
├── lib.rs               # Python API導出
├── core/                # 核心組件
│   ├── config.rs        # 配置系統
│   ├── types.rs         # 數據結構
│   ├── logger.rs        # 日誌系統
│   └── error.rs         # 錯誤處理
├── data_feed/           # 數據源
│   ├── mod.rs
│   ├── bitget_ws.rs     # WebSocket客戶端
│   ├── parser.rs        # 數據解析
│   └── validator.rs     # 數據驗證
├── engine/              # 交易引擎
│   ├── mod.rs
│   ├── orderbook.rs     # 高性能訂單簿
│   ├── strategy.rs      # 策略引擎
│   ├── execution.rs     # 訂單執行
│   └── risk_manager.rs  # 風險管理
├── ml/                  # 機器學習
│   ├── mod.rs
│   ├── model_loader.rs  # 模型加載
│   ├── inference.rs     # 推理引擎
│   └── feature_extractor.rs # 特徵提取
├── services/            # 後台服務
│   ├── mod.rs
│   ├── recorder.rs      # 數據記錄
│   ├── redis_pub.rs     # 實時發布
│   └── health_check.rs  # 健康檢查
├── api/                 # 對外接口
│   ├── mod.rs
│   ├── python.rs        # PyO3綁定
│   ├── cli.rs           # 命令行接口
│   └── rest.rs          # REST API（可選）
└── utils/               # 工具函數
    ├── mod.rs
    ├── performance.rs   # 性能監控
    └── testing.rs       # 測試工具
```

### 3.2 重構步驟

**階段1：代碼整合清理 (2週)**
```bash
# 1. 創建feature分支
git checkout -b feature/code-restructure

# 2. 分析現有examples
find examples/ -name "*.rs" -exec grep -l "main" {} \;

# 3. 提取通用邏輯到src/
# 例如：將訂單簿邏輯從examples/移動到src/engine/orderbook.rs

# 4. 重寫examples為薄包裝
# 例如：examples/live_trading.rs 只調用 src/api/cli.rs

# 5. 刪除冗餘目錄
rm -rf examples_backup/ examples/old_replaced_examples/
```

**階段2：統一入口和配置 (1週)**
```rust
// src/main.rs - 統一CLI入口
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rust-hft")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    #[arg(short, long)]
    config: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    Live { asset: String },
    Backtest { config: String },
    Train { pipeline: String },
    Monitor,
}
```

**階段3：建立Python API (1週)**
```rust
// src/api/python.rs
use pyo3::prelude::*;

#[pymodule]
fn rust_hft(py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(pipeline_train, m)?)?;
    m.add_function(wrap_pyfunction!(hft_start_live, m)?)?;
    m.add_function(wrap_pyfunction!(hft_stop_live, m)?)?;
    m.add_function(wrap_pyfunction!(get_real_time_metrics, m)?)?;
    Ok(())
}
```

---

## 4 · Python Agno智能代理架構

### 4.1 代理職責矩陣

| 代理 | 職責 | 運行模式 | 典型延遲 | 主要功能 |
|------|------|----------|----------|----------|
| **SupervisorAgent** | 全局調度、高可用 | 守護進程 | 1-5s | 健康檢查、故障切換、任務分發 |
| **TrainAgent** | 模型訓練、評估 | 按需啟動 | 小時級 | 數據處理、模型訓練、驗證 |
| **TradeAgent** | 策略執行、模型部署 | 實時 | <5ms | 模型熱加載、策略參數調整 |
| **MonitorAgent** | 系統監控、告警 | 實時 | 1-5s | 指標收集、異常檢測、告警 |
| **RiskAgent** | 風險控制、緊急停止 | 實時 | <1ms | 風險檢查、緊急熔斷 |
| **ConfigAgent** | 配置管理、熱更新 | 按需 | 10-100ms | 配置驗證、熱更新、版本管理 |
| **ChatAgent** | 用戶交互、查詢 | 按需 | 1-5s | 命令解析、狀態查詢、報告生成 |

### 4.2 代理通信協議

```python
# 標準消息格式
@dataclass
class AgentMessage:
    timestamp: int
    source: str
    target: str  
    message_type: str
    payload: Dict[str, Any]
    correlation_id: str
    
# 消息類型定義
class MessageType:
    # 模型相關
    MODEL_READY = "MODEL_READY"
    MODEL_DEPLOY = "MODEL_DEPLOY"
    MODEL_ROLLBACK = "MODEL_ROLLBACK"
    
    # 交易相關
    START_TRADING = "START_TRADING"
    STOP_TRADING = "STOP_TRADING"
    POSITION_UPDATE = "POSITION_UPDATE"
    
    # 風險相關
    RISK_ALERT = "RISK_ALERT"
    EMERGENCY_STOP = "EMERGENCY_STOP"
    
    # 系統相關
    HEALTH_CHECK = "HEALTH_CHECK"
    CONFIG_UPDATE = "CONFIG_UPDATE"

# 示例：訓練完成通知
training_complete_msg = AgentMessage(
    timestamp=int(time.time()),
    source="TrainAgent",
    target="TradeAgent",
    message_type=MessageType.MODEL_READY,
    payload={
        "asset": "BTCUSDT",
        "model_path": "/models/btc_v12.safetensors",
        "performance": {
            "sharpe": 1.8, 
            "max_drawdown": 0.05,
            "accuracy": 0.68
        },
        "validation_results": {
            "backtest_pnl": 1250.5,
            "win_rate": 0.55
        }
    },
    correlation_id="train_btc_20250708_001"
)
```

### 4.3 代理實現示例

```python
# agents/train_agent.py
class TrainAgent(BaseAgent):
    def __init__(self, config: Dict[str, Any]):
        super().__init__("TrainAgent", config)
        self.rust_engine = rust_hft
        
    async def run_training_pipeline(self, asset: str, config_path: str):
        """執行完整的模型訓練流水線"""
        try:
            # 1. 數據準備
            self.logger.info(f"開始訓練 {asset} 模型")
            
            # 2. 調用Rust訓練函數
            task_id = self.rust_engine.pipeline_train(asset, config_path)
            
            # 3. 監控訓練進度
            while True:
                status = self.rust_engine.get_task_status(task_id)
                if status["state"] == "completed":
                    break
                elif status["state"] == "failed":
                    raise Exception(f"訓練失敗: {status['error']}")
                await asyncio.sleep(10)
            
            # 4. 通知TradeAgent模型就緒
            await self.send_message(AgentMessage(
                timestamp=int(time.time()),
                source=self.name,
                target="TradeAgent",
                message_type=MessageType.MODEL_READY,
                payload={
                    "asset": asset,
                    "model_path": status["model_path"],
                    "performance": status["metrics"]
                },
                correlation_id=f"train_{asset}_{int(time.time())}"
            ))
            
        except Exception as e:
            self.logger.error(f"訓練失敗: {e}")
            await self.send_alert("training_failed", str(e))
```

---

## 5 · 機器學習生命週期

### 5.1 YAML驅動的ML Pipeline

```yaml
# config/pipelines/btc_lstm.yaml
asset: BTCUSDT
version: "1.2"

data_slice:
  start_date: "2024-01-01"
  end_date: "2024-12-31"
  data_sources: 
    - bitget_ws
    - historical_lob
  validation_split: 0.2
  
preprocessing:
  cleaning:
    remove_outliers: true
    outlier_threshold: 3.0
    fill_missing: "forward_fill"
    max_gap_seconds: 5
    
  feature_engineering:
    windows: [30, 60, 120, 300]
    indicators:
      - name: "sma"
        window: [5, 10, 20]
      - name: "ema" 
        window: [12, 26]
      - name: "atr"
        window: 14
      - name: "obi"
        levels: [1, 5, 10, 20]
      - name: "vwap"
        window: 60
    
  normalization:
    method: "z_score"
    rolling_window: 1000
    
model_training:
  algorithm: "lstm"
  hyperparameters:
    hidden_size: 128
    num_layers: 2
    dropout: 0.1
    learning_rate: 0.001
    batch_size: 256
    epochs: 100
    patience: 10
    
  hardware:
    device: "auto"  # auto, cpu, cuda, mps
    mixed_precision: true
    compile_model: true
    
validation:
  method: "time_series_split"
  n_splits: 5
  test_size: 0.2
  metrics:
    - sharpe_ratio
    - max_drawdown
    - calmar_ratio
    - win_rate
    
  criteria:
    min_sharpe: 1.5
    max_drawdown: 0.15
    min_win_rate: 0.52
    
deployment:
  strategy: "blue_green"
  shadow_period: "1h"
  shadow_position_ratio: 0.01
  
  promotion_criteria:
    min_profit: 0.01
    max_correlation: 0.8
    min_trades: 100
    
  rollback_criteria:
    max_loss: -0.05
    max_consecutive_losses: 5
    
risk_management:
  position_limits:
    max_position_usd: 10000
    max_leverage: 2.0
  
  stop_loss:
    method: "trailing"
    percentage: 0.02
    
  take_profit:
    method: "dynamic"
    min_ratio: 1.5  # risk:reward ratio
```

### 5.2 藍綠部署流程

```python
# agents/trade_agent.py
class BlueGreenDeployment:
    def __init__(self, trade_agent):
        self.trade_agent = trade_agent
        self.rust_engine = trade_agent.rust_engine
        
    async def deploy_model(self, asset: str, model_path: str, config: Dict):
        """執行藍綠部署流程"""
        deployment_id = f"deploy_{asset}_{int(time.time())}"
        
        try:
            # 1. 驗證模型文件
            if not self.validate_model_file(model_path):
                raise ValueError(f"模型文件驗證失敗: {model_path}")
            
            # 2. 加載到blue環境（影子模式）
            self.logger.info(f"開始藍綠部署: {deployment_id}")
            blue_pid = self.rust_engine.hft_start_live(
                asset=asset,
                model_path=model_path,
                dry_run=True  # 影子交易模式
            )
            
            # 3. 影子交易監控（1小時）
            shadow_config = config["deployment"]
            shadow_duration = self.parse_duration(shadow_config["shadow_period"])
            
            metrics = await self.monitor_shadow_trading(
                asset=asset,
                blue_pid=blue_pid,
                duration=shadow_duration,
                position_ratio=shadow_config["shadow_position_ratio"]
            )
            
            # 4. 決策：切換或回滾
            if self.should_promote(metrics, shadow_config["promotion_criteria"]):
                await self.promote_to_green(asset, model_path, blue_pid)
                self.logger.info(f"模型成功切換到生產: {deployment_id}")
            else:
                await self.rollback_blue(asset, blue_pid)
                self.logger.warning(f"模型未通過驗證，已回滾: {deployment_id}")
                
        except Exception as e:
            self.logger.error(f"部署失敗: {deployment_id}, 錯誤: {e}")
            await self.emergency_rollback(asset)
            
    async def monitor_shadow_trading(
        self, asset: str, blue_pid: int, duration: int, position_ratio: float
    ) -> Dict[str, float]:
        """監控影子交易性能"""
        start_time = time.time()
        metrics = {
            "total_trades": 0,
            "profit": 0.0,
            "max_drawdown": 0.0,
            "sharpe_ratio": 0.0,
            "win_rate": 0.0
        }
        
        while time.time() - start_time < duration:
            # 獲取blue環境實時指標
            blue_metrics = self.rust_engine.get_live_metrics(blue_pid)
            
            # 更新累計指標
            metrics.update(blue_metrics)
            
            # 檢查回滾條件
            if self.should_rollback(metrics):
                raise Exception("影子交易觸發回滾條件")
                
            await asyncio.sleep(60)  # 每分鐘檢查一次
            
        return metrics
        
    def should_promote(self, metrics: Dict, criteria: Dict) -> bool:
        """判斷是否應該提升到生產環境"""
        return (
            metrics["profit"] >= criteria["min_profit"] and
            metrics["correlation"] <= criteria["max_correlation"] and
            metrics["total_trades"] >= criteria["min_trades"]
        )
        
    async def promote_to_green(self, asset: str, model_path: str, blue_pid: int):
        """提升模型到生產環境"""
        # 1. 停止當前生產模型
        current_green_pid = self.get_current_green_pid(asset)
        if current_green_pid:
            self.rust_engine.hft_stop_live(current_green_pid)
        
        # 2. 將blue模型切換為生產模式
        green_pid = self.rust_engine.hft_start_live(
            asset=asset,
            model_path=model_path,
            dry_run=False  # 生產模式
        )
        
        # 3. 停止blue環境
        self.rust_engine.hft_stop_live(blue_pid)
        
        # 4. 更新生產環境記錄
        self.update_green_registry(asset, green_pid, model_path)
```

---

## 6 · 系統API設計

### 6.1 Rust→Python FFI接口

```rust
// src/api/python.rs
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::collections::HashMap;

#[pyfunction]
fn pipeline_train(
    py: Python<'_>, 
    asset: &str, 
    config_path: &str
) -> PyResult<String> {
    py.allow_threads(|| {
        let task_id = crate::ml::train::run_pipeline(asset, config_path)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                format!("訓練失敗: {}", e)
            ))?;
        Ok(task_id)
    })
}

#[pyfunction]
fn hft_start_live(
    py: Python<'_>, 
    asset: &str, 
    model_path: &str,
    dry_run: bool
) -> PyResult<u32> {
    py.allow_threads(|| {
        let config = crate::core::config::TradingConfig {
            asset: asset.to_string(),
            model_path: model_path.to_string(),
            dry_run,
            ..Default::default()
        };
        
        let pid = crate::engine::start_trading_engine(config)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                format!("啟動交易引擎失敗: {}", e)
            ))?;
        Ok(pid)
    })
}

#[pyfunction]
fn hft_stop_live(py: Python<'_>, pid: u32) -> PyResult<()> {
    py.allow_threads(|| {
        crate::engine::stop_trading_engine(pid)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                format!("停止交易引擎失敗: {}", e)
            ))
    })
}

#[pyfunction]
fn get_real_time_metrics(py: Python<'_>) -> PyResult<PyObject> {
    let metrics = py.allow_threads(|| {
        crate::services::metrics::get_current_metrics()
    })?;
    
    let dict = PyDict::new(py);
    for (key, value) in metrics {
        dict.set_item(key, value)?;
    }
    Ok(dict.to_object(py))
}

#[pyfunction]
fn get_task_status(py: Python<'_>, task_id: &str) -> PyResult<PyObject> {
    let status = py.allow_threads(|| {
        crate::core::task_manager::get_task_status(task_id)
    })?;
    
    let dict = PyDict::new(py);
    dict.set_item("state", status.state.to_string())?;
    dict.set_item("progress", status.progress)?;
    if let Some(error) = status.error {
        dict.set_item("error", error)?;
    }
    if let Some(result) = status.result {
        dict.set_item("result", result)?;
    }
    Ok(dict.to_object(py))
}

#[pymodule]
fn rust_hft(py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(pipeline_train, m)?)?;
    m.add_function(wrap_pyfunction!(hft_start_live, m)?)?;
    m.add_function(wrap_pyfunction!(hft_stop_live, m)?)?;
    m.add_function(wrap_pyfunction!(get_real_time_metrics, m)?)?;
    m.add_function(wrap_pyfunction!(get_task_status, m)?)?;
    Ok(())
}
```

### 6.2 IPC通信協議

```rust
// src/core/ipc.rs
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum Command {
    StartLiveTrading { 
        asset: String, 
        model_path: String,
        config: TradingConfig 
    },
    StopLiveTrading { 
        asset: String 
    },
    LoadModel { 
        asset: String, 
        model_path: String 
    },
    UpdateRiskParams { 
        asset: String,
        params: RiskParams 
    },
    GetSystemStatus,
    Emergency { 
        reason: String,
        severity: AlertSeverity 
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Event {
    OrderFilled { 
        order_id: String, 
        asset: String,
        price: f64, 
        quantity: f64,
        side: OrderSide,
        timestamp: i64
    },
    PositionChanged { 
        asset: String, 
        position: f64,
        unrealized_pnl: f64,
        timestamp: i64
    },
    RiskAlert { 
        asset: String,
        severity: AlertSeverity, 
        message: String,
        metrics: RiskMetrics,
        timestamp: i64
    },
    SystemMetrics { 
        latency_us: f64, 
        throughput: f64,
        memory_usage: f64,
        cpu_usage: f64,
        timestamp: i64
    },
    ModelInference {
        asset: String,
        prediction: f64,
        confidence: f64,
        latency_us: f64,
        timestamp: i64
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct IpcMessage {
    pub id: String,
    pub timestamp: i64,
    pub source: String,
    pub target: String,
    pub content: MessageContent,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum MessageContent {
    Command(Command),
    Event(Event),
    Response { success: bool, data: Option<serde_json::Value> },
}
```

### 6.3 REST API（可選）

```rust
// src/api/rest.rs
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct ApiResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

#[derive(Serialize)]
struct SystemStatus {
    uptime_seconds: u64,
    active_assets: Vec<String>,
    total_trades: u64,
    current_pnl: f64,
    engine_status: String,
}

#[derive(Deserialize)]
struct StartTradingRequest {
    asset: String,
    model_path: String,
    dry_run: bool,
}

async fn get_status() -> Json<ApiResponse<SystemStatus>> {
    match crate::services::system::get_status() {
        Ok(status) => Json(ApiResponse {
            success: true,
            data: Some(status),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn get_asset_pnl(Path(asset): Path<String>) -> Json<ApiResponse<f64>> {
    match crate::services::pnl::get_asset_pnl(&asset) {
        Ok(pnl) => Json(ApiResponse {
            success: true,
            data: Some(pnl),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn start_trading(Json(req): Json<StartTradingRequest>) -> Result<Json<ApiResponse<u32>>, StatusCode> {
    match crate::api::python::start_live_trading(&req.asset, &req.model_path, req.dry_run) {
        Ok(pid) => Ok(Json(ApiResponse {
            success: true,
            data: Some(pid),
            error: None,
        })),
        Err(e) => Ok(Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        })),
    }
}

pub fn create_router() -> Router {
    Router::new()
        .route("/api/v1/status", get(get_status))
        .route("/api/v1/pnl/:asset", get(get_asset_pnl))
        .route("/api/v1/trading/start", post(start_trading))
}
```

---

## 7 · 監控和可觀測性

### 7.1 關鍵指標定義

```yaml
# config/monitoring/metrics.yaml
metrics:
  # 延遲指標
  - name: "hft_decision_latency_us"
    type: "histogram"
    description: "決策延遲（微秒）"
    buckets: [0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 50.0, 100.0]
    labels: ["asset", "strategy"]
    alert_rules:
      - condition: "p95 > 3"
        duration: "1m"
        severity: "warning"
        action: "feishu_notify"
      - condition: "p99 > 10"
        duration: "30s"
        severity: "critical"
        action: "emergency_alert"
        
  - name: "hft_model_inference_latency_us"
    type: "histogram"
    description: "模型推理延遲（微秒）"
    buckets: [10, 25, 50, 100, 200, 500, 1000]
    labels: ["asset", "model_version"]
    alert_rules:
      - condition: "p99 > 100"
        duration: "30s"
        severity: "warning"
        action: "log_alert"
        
  - name: "hft_ipc_latency_us"
    type: "histogram"
    description: "Python↔Rust IPC延遲（微秒）"
    buckets: [100, 500, 1000, 2000, 5000, 10000]
    alert_rules:
      - condition: "p95 > 5000"
        duration: "1m"
        severity: "warning"

  # 業務指標
  - name: "asset_pnl_usd"
    type: "gauge"
    description: "每資產P&L（美元）"
    labels: ["asset"]
    alert_rules:
      - condition: "value < -200"
        duration: "immediate"
        severity: "critical"
        action: "emergency_stop"
      - condition: "value < -100"
        duration: "5m"
        severity: "warning"
        action: "risk_alert"
        
  - name: "position_exposure_ratio"
    type: "gauge"
    description: "倉位風險敞口比例"
    labels: ["asset"]
    alert_rules:
      - condition: "value > 0.8"
        duration: "1m"
        severity: "warning"
        action: "reduce_position"
      - condition: "value > 0.9"
        duration: "immediate"
        severity: "critical"
        action: "force_close"

  # 系統指標
  - name: "hft_engine_alive"
    type: "gauge"
    description: "交易引擎存活狀態"
    labels: ["node", "asset"]
    alert_rules:
      - condition: "value < 1"
        duration: "immediate"
        severity: "critical"
        action: "failover"
        
  - name: "order_fill_rate"
    type: "gauge"
    description: "訂單成交率"
    labels: ["asset", "side"]
    alert_rules:
      - condition: "value < 0.8"
        duration: "5m"
        severity: "warning"
        action: "market_alert"

  # ML指標
  - name: "model_accuracy"
    type: "gauge"
    description: "模型預測準確率"
    labels: ["asset", "model_version"]
    alert_rules:
      - condition: "value < 0.55"
        duration: "1h"
        severity: "warning"
        action: "model_retrain"
        
  - name: "feature_drift_score"
    type: "gauge"
    description: "特徵漂移分數"
    labels: ["asset", "feature"]
    alert_rules:
      - condition: "value > 0.3"
        duration: "30m"
        severity: "warning"
        action: "drift_alert"
```

### 7.2 告警系統

```python
# monitoring/alert_manager.py
from enum import Enum
from dataclasses import dataclass
from typing import List, Dict, Any, Optional

class AlertSeverity(Enum):
    INFO = "info"
    WARNING = "warning"  
    ERROR = "error"
    CRITICAL = "critical"

class AlertAction(Enum):
    LOG_ALERT = "log_alert"
    FEISHU_NOTIFY = "feishu_notify"
    EMAIL_NOTIFY = "email_notify"
    PAGER_DUTY = "pager_duty"
    EMERGENCY_STOP = "emergency_stop"
    RISK_ALERT = "risk_alert"
    FAILOVER = "failover"
    MODEL_RETRAIN = "model_retrain"

@dataclass
class AlertRule:
    name: str
    metric: str
    condition: str
    duration: str
    severity: AlertSeverity
    actions: List[AlertAction]
    labels: Dict[str, str] = None

@dataclass
class Alert:
    id: str
    rule_name: str
    severity: AlertSeverity
    message: str
    labels: Dict[str, str]
    timestamp: int
    resolved: bool = False

class AlertManager:
    def __init__(self, config: Dict[str, Any]):
        self.config = config
        self.rules = self.load_rules()
        self.active_alerts: Dict[str, Alert] = {}
        
    def load_rules(self) -> List[AlertRule]:
        """從配置文件加載告警規則"""
        rules = []
        for metric_config in self.config["metrics"]:
            if "alert_rules" in metric_config:
                for rule_config in metric_config["alert_rules"]:
                    rule = AlertRule(
                        name=f"{metric_config['name']}_{rule_config['condition'].replace(' ', '_')}",
                        metric=metric_config["name"],
                        condition=rule_config["condition"],
                        duration=rule_config["duration"],
                        severity=AlertSeverity(rule_config["severity"]),
                        actions=[AlertAction(action) for action in rule_config.get("actions", [])]
                    )
                    rules.append(rule)
        return rules
    
    async def check_metrics(self, metrics: Dict[str, float]):
        """檢查指標並觸發告警"""
        for rule in self.rules:
            if rule.metric in metrics:
                metric_value = metrics[rule.metric]
                if self.evaluate_condition(rule.condition, metric_value):
                    await self.trigger_alert(rule, metric_value)
                    
    def evaluate_condition(self, condition: str, value: float) -> bool:
        """評估告警條件"""
        # 簡化的條件評估，實際應該支持更複雜的表達式
        if ">" in condition:
            threshold = float(condition.split(">")[1].strip())
            return value > threshold
        elif "<" in condition:
            threshold = float(condition.split("<")[1].strip())
            return value < threshold
        return False
        
    async def trigger_alert(self, rule: AlertRule, value: float):
        """觸發告警"""
        alert_id = f"{rule.name}_{int(time.time())}"
        
        alert = Alert(
            id=alert_id,
            rule_name=rule.name,
            severity=rule.severity,
            message=f"指標 {rule.metric} 值為 {value}，觸發條件 {rule.condition}",
            labels=rule.labels or {},
            timestamp=int(time.time())
        )
        
        self.active_alerts[alert_id] = alert
        
        # 執行告警動作
        for action in rule.actions:
            await self.execute_action(action, alert, value)
            
    async def execute_action(self, action: AlertAction, alert: Alert, value: float):
        """執行告警動作"""
        if action == AlertAction.FEISHU_NOTIFY:
            await self.send_feishu_notification(alert)
        elif action == AlertAction.EMERGENCY_STOP:
            await self.emergency_stop_trading(alert)
        elif action == AlertAction.PAGER_DUTY:
            await self.send_pager_duty_alert(alert)
        elif action == AlertAction.FAILOVER:
            await self.trigger_failover(alert)
        # ... 其他動作
        
    async def send_feishu_notification(self, alert: Alert):
        """發送飛書通知"""
        webhook_url = self.config["feishu"]["webhook_url"]
        message = {
            "msg_type": "text",
            "content": {
                "text": f"🚨 HFT Alert [{alert.severity.value.upper()}]\n"
                       f"告警: {alert.rule_name}\n"
                       f"消息: {alert.message}\n"
                       f"時間: {datetime.fromtimestamp(alert.timestamp)}"
            }
        }
        
        async with aiohttp.ClientSession() as session:
            await session.post(webhook_url, json=message)
            
    async def emergency_stop_trading(self, alert: Alert):
        """緊急停止交易"""
        # 通知RiskAgent執行緊急停止
        await self.send_agent_message(
            target="RiskAgent",
            message_type="EMERGENCY_STOP",
            payload={
                "reason": alert.message,
                "alert_id": alert.id,
                "severity": alert.severity.value
            }
        )
```

### 7.3 Grafana儀表板

```json
{
  "dashboard": {
    "title": "HFT Trading System",
    "panels": [
      {
        "title": "決策延遲 (μs)",
        "type": "graph",
        "targets": [
          {
            "expr": "histogram_quantile(0.95, hft_decision_latency_us_bucket)",
            "legendFormat": "P95"
          },
          {
            "expr": "histogram_quantile(0.99, hft_decision_latency_us_bucket)",
            "legendFormat": "P99"
          }
        ],
        "yAxes": [
          {
            "label": "延遲 (μs)",
            "max": 10
          }
        ],
        "alert": {
          "conditions": [
            {
              "query": {
                "queryType": "",
                "refId": "A"
              },
              "reducer": {
                "type": "last",
                "params": []
              },
              "evaluator": {
                "params": [3],
                "type": "gt"
              }
            }
          ],
          "executionErrorState": "alerting",
          "frequency": "10s",
          "handler": 1,
          "name": "高延遲告警",
          "noDataState": "no_data"
        }
      },
      {
        "title": "實時P&L",
        "type": "stat",
        "targets": [
          {
            "expr": "sum(asset_pnl_usd)",
            "legendFormat": "總P&L"
          }
        ],
        "fieldConfig": {
          "defaults": {
            "color": {
              "mode": "thresholds"
            },
            "thresholds": {
              "steps": [
                {
                  "color": "red",
                  "value": -200
                },
                {
                  "color": "yellow", 
                  "value": -50
                },
                {
                  "color": "green",
                  "value": 0
                }
              ]
            }
          }
        }
      },
      {
        "title": "系統吞吐量",
        "type": "graph",
        "targets": [
          {
            "expr": "rate(hft_orders_total[1m])",
            "legendFormat": "訂單/秒"
          },
          {
            "expr": "rate(hft_fills_total[1m])",
            "legendFormat": "成交/秒"
          }
        ]
      },
      {
        "title": "模型準確率",
        "type": "gauge",
        "targets": [
          {
            "expr": "model_accuracy",
            "legendFormat": "{{asset}}"
          }
        ],
        "fieldConfig": {
          "defaults": {
            "min": 0,
            "max": 1,
            "thresholds": {
              "steps": [
                {
                  "color": "red",
                  "value": 0
                },
                {
                  "color": "yellow",
                  "value": 0.55
                },
                {
                  "color": "green", 
                  "value": 0.65
                }
              ]
            }
          }
        }
      }
    ]
  }
}
```

---

## 8 · 部署和運維

### 8.1 Docker Compose部署

```yaml
# docker-compose.yml
version: '3.8'

services:
  # Rust執行引擎
  hft-engine:
    build:
      context: .
      dockerfile: Dockerfile.hft
      args:
        RUST_VERSION: "1.75"
    container_name: hft-engine
    hostname: hft-engine
    depends_on:
      - redis
      - clickhouse
    environment:
      - RUST_LOG=info
      - CONFIG_PATH=/config/production.yaml
      - REDIS_URL=redis://redis:6379
      - CLICKHOUSE_URL=http://clickhouse:8123
    volumes:
      - ./config:/config:ro
      - ./models:/models:ro
      - ./logs:/logs
      - /dev/shm:/dev/shm  # 共享內存，用於高性能IPC
    restart: unless-stopped
    ulimits:
      memlock:
        soft: -1
        hard: -1
    networks:
      - hft-network
    deploy:
      resources:
        reservations:
          cpus: '4.0'
          memory: 8G
    
  # Python代理控制層
  agno-agents:
    build:
      context: .
      dockerfile: Dockerfile.agno
      args:
        PYTHON_VERSION: "3.11"
    container_name: agno-agents
    hostname: agno-agents
    depends_on:
      - hft-engine
      - supabase
    environment:
      - PYTHONPATH=/app
      - SUPABASE_URL=${SUPABASE_URL}
      - SUPABASE_KEY=${SUPABASE_KEY}
      - REDIS_URL=redis://redis:6379
      - RUST_ENGINE_HOST=hft-engine
    volumes:
      - ./config:/config:ro
      - ./models:/models
      - ./notebooks:/notebooks
      - ./logs:/logs
    restart: unless-stopped
    networks:
      - hft-network
    deploy:
      resources:
        reservations:
          cpus: '2.0'
          memory: 4G
    
  # 時序數據庫
  clickhouse:
    image: clickhouse/clickhouse-server:23.8
    container_name: clickhouse
    hostname: clickhouse
    environment:
      - CLICKHOUSE_DB=hft_data
      - CLICKHOUSE_USER=hft_user
      - CLICKHOUSE_PASSWORD=${CLICKHOUSE_PASSWORD}
    volumes:
      - clickhouse_data:/var/lib/clickhouse
      - ./clickhouse/config:/etc/clickhouse-server/config.d:ro
      - ./clickhouse/users:/etc/clickhouse-server/users.d:ro
    ports:
      - "8123:8123"
      - "9000:9000"
    restart: unless-stopped
    networks:
      - hft-network
    deploy:
      resources:
        reservations:
          cpus: '2.0'
          memory: 4G
    
  # 內存數據庫
  redis:
    image: redis:7.2-alpine
    container_name: redis
    hostname: redis
    command: redis-server --appendonly yes --maxmemory 2gb --maxmemory-policy allkeys-lru
    volumes:
      - redis_data:/data
      - ./redis/redis.conf:/usr/local/etc/redis/redis.conf:ro
    ports:
      - "6379:6379"
    restart: unless-stopped
    networks:
      - hft-network
    deploy:
      resources:
        reservations:
          memory: 2G
    
  # 監控堆棧
  prometheus:
    image: prom/prometheus:v2.45.0
    container_name: prometheus
    hostname: prometheus
    command:
      - '--config.file=/etc/prometheus/prometheus.yml'
      - '--storage.tsdb.path=/prometheus'
      - '--web.console.libraries=/etc/prometheus/console_libraries'
      - '--web.console.templates=/etc/prometheus/consoles'
      - '--storage.tsdb.retention.time=30d'
      - '--web.enable-lifecycle'
    volumes:
      - ./monitoring/prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - ./monitoring/rules:/etc/prometheus/rules:ro
      - prometheus_data:/prometheus
    ports:
      - "9090:9090"
    restart: unless-stopped
    networks:
      - hft-network
      
  grafana:
    image: grafana/grafana:10.0.0
    container_name: grafana
    hostname: grafana
    environment:
      - GF_SECURITY_ADMIN_PASSWORD=${GRAFANA_ADMIN_PASSWORD}
      - GF_USERS_ALLOW_SIGN_UP=false
    volumes:
      - grafana_data:/var/lib/grafana
      - ./monitoring/grafana/provisioning:/etc/grafana/provisioning:ro
      - ./monitoring/grafana/dashboards:/var/lib/grafana/dashboards:ro
    ports:
      - "3000:3000"
    restart: unless-stopped
    networks:
      - hft-network
      
  # Nginx反向代理（可選）
  nginx:
    image: nginx:1.25-alpine
    container_name: nginx
    hostname: nginx
    volumes:
      - ./nginx/nginx.conf:/etc/nginx/nginx.conf:ro
      - ./nginx/ssl:/etc/nginx/ssl:ro
    ports:
      - "80:80"
      - "443:443"
    depends_on:
      - grafana
      - prometheus
    restart: unless-stopped
    networks:
      - hft-network

volumes:
  clickhouse_data:
    driver: local
  redis_data:
    driver: local
  grafana_data:
    driver: local
  prometheus_data:
    driver: local

networks:
  hft-network:
    driver: bridge
    ipam:
      config:
        - subnet: 172.20.0.0/16
```

### 8.2 Dockerfile

```dockerfile
# Dockerfile.hft - Rust執行引擎
FROM rust:1.75 as builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# 構建優化的生產版本
RUN cargo build --release --bin rust-hft

# 運行時鏡像
FROM debian:bookworm-slim

# 安裝運行時依賴
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# 創建非root用戶
RUN useradd -r -s /bin/false hft

WORKDIR /app
COPY --from=builder /app/target/release/rust-hft ./
COPY --chown=hft:hft ./config ./config

USER hft
EXPOSE 8080

CMD ["./rust-hft", "--config", "/config/production.yaml"]
```

```dockerfile
# Dockerfile.agno - Python智能代理
FROM python:3.11-slim

WORKDIR /app

# 安裝系統依賴
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    && rm -rf /var/lib/apt/lists/*

# 安裝Python依賴
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

# 安裝rust_hft Python模塊
COPY --from=hft-engine /app/target/release/libruSt_hft.so /usr/local/lib/python3.11/site-packages/

# 複製代理代碼
COPY agents ./agents
COPY config ./config

# 創建非root用戶
RUN useradd -r -s /bin/false agno
USER agno

CMD ["python", "-m", "agents.supervisor"]
```

### 8.3 CI/CD流水線

```yaml
# .github/workflows/ci-cd.yml
name: HFT CI/CD Pipeline

on:
  push:
    branches: [main, develop]
    tags: ['v*']
  pull_request:
    branches: [main]

env:
  REGISTRY: ghcr.io
  IMAGE_NAME: ${{ github.repository }}

jobs:
  test:
    name: 測試和驗證
    runs-on: ubuntu-latest
    
    steps:
      - name: Checkout代碼
        uses: actions/checkout@v4
        
      - name: 設置Rust工具鏈
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
          components: clippy, rustfmt
          
      - name: 設置Python環境
        uses: actions/setup-python@v4
        with:
          python-version: '3.11'
          
      - name: 緩存Rust依賴
        uses: actions/cache@v3
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: ${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}
          
      - name: 緩存Python依賴
        uses: actions/cache@v3
        with:
          path: ~/.cache/pip
          key: ${{ runner.os }}-pip-${{ hashFiles('**/requirements.txt') }}
          
      - name: 運行Rust測試
        run: |
          cargo test --release --all-features
          cargo clippy --all-targets --all-features -- -D warnings
          cargo fmt --check
          
      - name: 運行性能基準測試
        run: |
          cargo bench --bench decision_latency -- --output-format json | tee bench_results.json
          cargo bench --bench inference_latency -- --output-format json | tee -a bench_results.json
          
      - name: 驗證性能指標
        run: |
          python scripts/validate_performance.py bench_results.json
          
      - name: 運行Python測試
        run: |
          pip install -r requirements.txt
          pytest agents/tests/ -v --cov=agents
          
      - name: 安全掃描
        run: |
          cargo audit
          pip install safety
          safety check
          
  build:
    name: 構建Docker鏡像
    runs-on: ubuntu-latest
    needs: test
    
    steps:
      - name: Checkout代碼
        uses: actions/checkout@v4
        
      - name: 設置Docker Buildx
        uses: docker/setup-buildx-action@v3
        
      - name: 登錄到Container Registry
        uses: docker/login-action@v3
        with:
          registry: ${{ env.REGISTRY }}
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
          
      - name: 提取元數據
        id: meta
        uses: docker/metadata-action@v5
        with:
          images: ${{ env.REGISTRY }}/${{ env.IMAGE_NAME }}
          tags: |
            type=ref,event=branch
            type=ref,event=pr
            type=semver,pattern={{version}}
            type=semver,pattern={{major}}.{{minor}}
            
      - name: 構建和推送Rust引擎鏡像
        uses: docker/build-push-action@v5
        with:
          context: .
          file: ./Dockerfile.hft
          push: true
          tags: ${{ steps.meta.outputs.tags }}-hft
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha
          cache-to: type=gha,mode=max
          
      - name: 構建和推送Python代理鏡像
        uses: docker/build-push-action@v5
        with:
          context: .
          file: ./Dockerfile.agno
          push: true
          tags: ${{ steps.meta.outputs.tags }}-agno
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha
          cache-to: type=gha,mode=max

  deploy-staging:
    name: 部署到預發布環境
    runs-on: ubuntu-latest
    needs: build
    if: github.ref == 'refs/heads/develop'
    environment: staging
    
    steps:
      - name: Checkout代碼
        uses: actions/checkout@v4
        
      - name: 部署到預發布
        run: |
          echo "部署到預發布環境"
          # 使用docker-compose或k8s部署
          docker-compose -f docker-compose.staging.yml up -d
          
      - name: 運行集成測試
        run: |
          python scripts/integration_tests.py --env staging
          
      - name: 運行壓力測試
        run: |
          python scripts/stress_test.py --duration 10m --target-throughput 1000
          
  deploy-production:
    name: 部署到生產環境
    runs-on: ubuntu-latest
    needs: [build, deploy-staging]
    if: github.ref == 'refs/heads/main'
    environment: production
    
    steps:
      - name: Checkout代碼
        uses: actions/checkout@v4
        
      - name: 部署到生產環境
        run: |
          echo "部署到生產環境"
          # 使用docker-compose或k8s部署
          docker-compose -f docker-compose.production.yml up -d
          
      - name: 健康檢查
        run: |
          python scripts/health_check.py --timeout 300
          
      - name: 通知部署成功
        if: success()
        run: |
          curl -X POST ${{ secrets.FEISHU_WEBHOOK }} \
            -H "Content-Type: application/json" \
            -d '{"msg_type":"text","content":{"text":"🚀 HFT系統成功部署到生產環境"}}'
```

---

## 9 · 實施計劃

### 9.1 階段性里程碑

**階段1：代碼重構和基礎架構 (2週)**

*目標：清理現有代碼，建立清晰的架構基礎*

**任務清單：**
- [ ] **代碼清理**
  - 分析examples目錄中的19個文件，識別重複邏輯
  - 將通用功能提取到src/模塊中
  - 刪除examples_backup/和old_replaced_examples/
  - 建立新的目標代碼結構

- [ ] **統一入口和配置**
  - 重寫src/main.rs為統一CLI入口
  - 實現基於clap的命令行參數解析
  - 建立YAML配置系統
  - 統一日誌和錯誤處理

- [ ] **基礎PyO3 API**
  - 實現核心的Python綁定函數
  - 建立IPC通信機制
  - 創建基本的測試框架

**交付物：**
- 清理後的代碼結構
- 統一的CLI接口
- 基本的Python API
- 文檔更新

**驗收標準：**
- cargo test 100%通過
- 代碼行數減少30%+
- 單一入口點啟動
- 基本Python調用成功

---

**階段2：Rust核心引擎完善 (4週)**

*目標：完善高性能交易引擎，達到性能指標*

**任務清單：**
- [ ] **高性能OrderBook**
  - 基於現有代碼優化訂單簿實現
  - 集成SIMD優化
  - 實現無鎖數據結構
  - 性能基準測試

- [ ] **策略引擎**
  - 重構現有策略代碼
  - 實現可插拔策略架構
  - 集成ML推理引擎
  - 風險管理模塊

- [ ] **執行引擎**
  - 完善Bitget API集成
  - 實現訂單生命週期管理
  - 錯誤處理和重試邏輯
  - 實時監控指標

- [ ] **ML推理優化**
  - 集成Candle推理引擎
  - 實現模型熱加載
  - ONNX/Safetensors支持
  - GPU加速（可選）

**交付物：**
- 完整的Rust交易引擎
- 性能基準測試報告
- API文檔
- 集成測試套件

**驗收標準：**
- 決策延遲 < 1μs P95
- 模型推理 < 50μs P95
- 10k+ msg/s吞吐量
- 零崩潰運行24小時

---

**階段3：Python Agno代理開發 (3週)**

*目標：實現智能代理系統和ML流水線*

**任務清單：**
- [ ] **智能代理實現**
  - SupervisorAgent：全局調度和HA
  - TrainAgent：模型訓練和評估
  - TradeAgent：策略執行和部署
  - MonitorAgent：系統監控和告警
  - RiskAgent：風險控制
  - ConfigAgent：配置管理
  - ChatAgent：用戶交互

- [ ] **ML訓練流水線**
  - YAML配置解析器
  - 數據預處理管道
  - 模型訓練和驗證
  - 自動超參數調優

- [ ] **藍綠部署系統**
  - 影子交易實現
  - 性能對比分析
  - 自動提升/回滾
  - 風險控制邏輯

- [ ] **代理間通信**
  - 消息路由系統
  - 事件發布/訂閱
  - 錯誤處理和重試
  - 監控和日誌

**交付物：**
- 7個智能代理
- ML訓練流水線
- 藍綠部署系統
- 代理通信框架

**驗收標準：**
- 代理RTT < 5ms P95
- 自動訓練成功率 > 95%
- 藍綠部署無中斷
- 24小時穩定運行

---

**階段4：監控、部署和生產上線 (2週)**

*目標：建立完整的監控體系，實現生產部署*

**任務清單：**
- [ ] **監控系統**
  - Prometheus指標收集
  - Grafana儀表板
  - 告警規則配置
  - 日誌聚合和分析

- [ ] **部署系統**
  - Docker Compose配置
  - CI/CD流水線
  - 環境配置管理
  - 自動化測試

- [ ] **運維工具**
  - 健康檢查腳本
  - 故障診斷工具
  - 備份和恢復
  - 性能調優

- [ ] **生產部署**
  - 預發布環境測試
  - 生產環境部署
  - 壓力測試驗證
  - 監控告警驗證

**交付物：**
- 完整的監控系統
- 自動化部署流水線
- 運維手冊
- 生產環境

**驗收標準：**
- 系統可用性 > 99.5%
- 告警響應時間 < 30秒
- 自動部署成功
- 所有功能正常運行

### 9.2 成功標準

| 階段 | 關鍵指標 | 目標值 | 驗證方式 |
|------|----------|--------|----------|
| **階段1** | 代碼清理度 | 減少30%+ | 代碼行數統計 |
| | 入口統一 | 單一CLI | 手動測試 |
| | Python集成 | 基本調用成功 | 自動化測試 |
| **階段2** | 決策延遲 | <1μs P95 | Criterion基準測試 |
| | 推理延遲 | <50μs P95 | 專用benchmark |
| | 吞吐量 | >10k msg/s | 壓力測試 |
| | 穩定性 | 24小時零崩潰 | 長期測試 |
| **階段3** | 代理延遲 | <5ms P95 | IPC測試 |
| | 訓練成功率 | >95% | 統計分析 |
| | 部署成功率 | >98% | 自動化測試 |
| | 系統集成 | 端到端正常 | 集成測試 |
| **階段4** | 系統可用性 | >99.5% | 監控統計 |
| | 告警響應 | <30秒 | 告警測試 |
| | 部署效率 | <5分鐘 | 自動化測試 |
| | 性能穩定 | 滿足所有指標 | 生產驗證 |

### 9.3 風險評估和緩解

**高風險項：**

1. **性能指標無法達成**
   - 風險：決策延遲超過1μs，影響HFT競爭力
   - 緩解：分階段性能優化，預留性能裕度，備用方案

2. **代碼重構引入Bug**
   - 風險：重構過程中破壞現有功能
   - 緩解：增量重構，完整測試覆蓋，回歸測試

3. **Python-Rust集成複雜性**
   - 風險：PyO3集成困難，穩定性問題
   - 緩解：簡化接口設計，充分測試，備用IPC方案

**中風險項：**

1. **ML模型性能不穩定**
   - 風險：模型在生產環境表現差異
   - 緩解：充分回測驗證，影子交易，快速回滾

2. **監控系統複雜性**
   - 風險：監控配置復雜，告警誤報
   - 緩解：逐步配置，測試驗證，持續調優

**低風險項：**

1. **部署環境問題**
   - 風險：Docker/K8s配置問題
   - 緩解：標準化配置，自動化測試

### 9.4 資源需求

**開發資源：**
- Rust開發工程師：2人×4個月
- Python開發工程師：1人×3個月  
- DevOps工程師：1人×2個月
- 測試工程師：1人×4個月

**硬件資源：**
- 開發環境：高性能工作站×4台
- 測試環境：模擬生產環境
- 生產環境：高性能服務器集群

**外部依賴：**
- Bitget API訪問權限
- ClickHouse企業支持（可選）
- 監控告警服務（PagerDuty等）

---

## 10 · 總結

本PRD v2.0基於對現有代碼的深入分析，設計了一個現實可行的HFT系統重構方案。核心特點包括：

### 10.1 技術優勢

1. **雙平面架構清晰**：Rust執行平面保證性能，Python控制平面提供靈活性
2. **性能目標可達成**：基於現有優化基礎，1μs決策延遲技術可行
3. **智能代理系統**：7個專業代理分工明確，責任邊界清晰
4. **完整ML生命週期**：從訓練到部署的全自動化流水線
5. **生產級運維**：監控、告警、部署的完整解決方案

### 10.2 實施可行性

1. **現有基礎扎實**：10k+ msg/s吞吐量、SIMD優化已實現
2. **技術路徑明確**：每個階段都有具體的任務和驗收標準
3. **風險可控**：識別了主要風險點並制定了緩解措施
4. **資源需求合理**：4個月開發週期，團隊規模適中

### 10.3 商業價值

1. **競爭優勢**：微秒級延遲在HFT領域具有顯著競爭優勢
2. **可擴展性**：模塊化設計支持多資產、多策略擴展
3. **智能化**：AI驅動的自動化運維減少人工干預
4. **可維護性**：清晰的架構和完整的文檔降低維護成本

本PRD為項目重構提供了清晰的路線圖，既利用了現有的技術積累，又解決了代碼組織混亂的問題，是一個平衡技術追求與工程實踐的優秀方案。

---

**附錄：**
- A. 現有代碼分析報告
- B. 性能基準測試結果  
- C. API接口詳細規範
- D. 部署操作手冊
- E. 故障診斷指南

---

*Document Version: 2.0*  
*Last Updated: 2025-07-08*  
*Next Review: 2025-07-15*