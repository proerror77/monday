# NBA Swing Trading Strategy - MVP 完整文檔

## 概述

這是一個基於 **Model-based Value** 的 NBA moneyline swing 交易策略，專注於在 Polymarket 上交易強隊落後時的翻盤機會。

## 核心理念

**不是「價格 < 0.20 就買」，而是「有可驗證的 edge 才買」**

- **Edge 來源**：Live win probability 模型比市場更準確
- **風險控制**：Market microstructure 濾網防止在糟糕條件下交易
- **可歸因**：每筆交易都知道為什麼進場、為什麼出場、盈虧來自哪裡

## 系統架構

```
┌─────────────────────────────────────────────────────────┐
│                   數據收集層                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │ Polymarket   │  │ NBA Live     │  │ Team Stats   │  │
│  │ LOB (10s)    │  │ Score (30s)  │  │ (cached)     │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│                   預測層                                  │
│  ┌──────────────────────────────────────────────────┐  │
│  │ Live Win Probability Model                        │  │
│  │ - Logistic regression (10 features)              │  │
│  │ - Calibrated probabilities                       │  │
│  │ - Uncertainty estimation                         │  │
│  └──────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│                   濾網層                                  │
│  ┌──────────────────────────────────────────────────┐  │
│  │ Market Microstructure Filters (6 checks)         │  │
│  │ - Spread, Depth, Velocity, Latency               │  │
│  │ - Order flow, Imbalance                          │  │
│  └──────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│                   決策層                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │ Entry Logic  │  │ Exit Logic   │  │ State        │  │
│  │ (5 checks)   │  │ (6 strategies)│  │ Machine      │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│                   執行層                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │ Order        │  │ Position     │  │ Risk         │  │
│  │ Executor     │  │ Manager      │  │ Manager      │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  │
└─────────────────────────────────────────────────────────┘
```

## 組件詳解

### 1. Live Win Probability Model

**文件**：`src/strategy/nba_winprob.rs`

**功能**：預測比賽勝率

**輸入特徵**：
- `point_diff`: 分差（正數 = 領先）
- `time_remaining`: 剩餘時間（分鐘）
- `quarter`: 節數（1-4）
- `possession`: 球權（1 = 有球，0 = 無球）
- `pregame_spread`: 賽前讓分
- `elo_diff`: Elo 評級差距

**輸出**：
- `win_prob`: 勝率（0.0 - 1.0）
- `confidence`: 信心（1 - uncertainty）
- `uncertainty`: 不確定性（基於時間、分差、極端情況）

**範例**：
```rust
let model = LiveWinProbModel::from_file("model.json")?;
let features = GameFeatures {
    point_diff: -12.0,  // 落後 12 分
    time_remaining: 8.0, // 剩 8 分鐘
    quarter: 3,
    possession: 1.0,
    pregame_spread: 5.0, // 賽前被看好
    elo_diff: 50.0,
};
let prediction = model.predict(&features);
// prediction.win_prob = 0.25 (25% 勝率)
// prediction.confidence = 0.95 (95% 信心)
```

### 2. Market Microstructure Filters

**文件**：`src/strategy/nba_filters.rs`

**功能**：防禦性風險控制

**六大濾網**：

| 濾網 | 檢查項目 | 默認閾值 |
|------|---------|---------|
| Spread | 價差 | ≤ 200 bps (2%) |
| Depth | 訂單簿深度 | ≥ $1000 |
| Velocity | 價格變動速度 | ≤ 1%/秒 |
| Latency | 數據延遲 | ≤ 2000ms |
| Order Flow | 連續同向成交 | ≤ 5 筆 |
| Imbalance | 買賣深度不平衡 | ≤ 80/20 |

**範例**：
```rust
let filters = MarketFilters::new(FilterConfig::default());
let context = MarketContext {
    spread_bps: Some(150),
    bid_depth: Decimal::new(2000, 0),
    ask_depth: Decimal::new(1800, 0),
    price_velocity: Some(0.005),
    data_latency_ms: 800,
    consecutive_same_side_trades: 3,
    depth_imbalance: 0.05,
    // ...
};
let result = filters.can_enter(&context);
// result.passed = true (所有檢查通過)
```

### 3. Entry Logic

**文件**：`src/strategy/nba_entry.rs`

**功能**：進場決策

**五層檢查**：

1. **Filters**：Market microstructure 通過
2. **Price Sanity**：0.05 ≤ price ≤ 0.80
3. **Edge**：p_model - p_market ≥ 5%
4. **Confidence**：模型信心 ≥ 70%
5. **EV**：扣除費用後 EV ≥ 2%

**EV 計算**：
```
Gross EV = p_model × 1.0 - p_market
Fees = p_market × 0.02 (2%)
Slippage = 0.005 (0.5%)
Net EV = Gross EV - Fees - Slippage
```

**範例**：
```rust
let entry_logic = EntryLogic::new(EntryConfig::default());
let decision = entry_logic.should_enter(
    &prediction,           // 來自 winprob model
    market_price,          // 當前市場價格
    &filter_result,        // 來自 filters
);

match decision {
    EntryDecision::Approve(signal) => {
        // signal.edge = 0.10 (10% edge)
        // signal.net_ev = 0.085 (8.5% net EV)
        // 進場！
    },
    EntryDecision::Reject { reason, .. } => {
        // 拒絕，記錄原因
    },
}
```

### 4. Exit Logic

**文件**：`src/strategy/nba_exit.rs`

**功能**：出場決策

**六種策略（按優先級）**：

| 優先級 | 策略 | 條件 | 緊急度 |
|--------|------|------|--------|
| 1 | Edge Disappeared | edge < -1% | Medium |
| 2 | Liquidity Risk | 深度 < 2x 倉位 | High |
| 3 | Trailing Stop | 從峰值回撤 > 10% | Medium |
| 4 | Partial Profit | edge < 2% 且盈利 | - |
| 5 | Time Stop | Q4 < 2min 且利潤 < 10% | Medium |
| 6 | Hold | 以上都不滿足 | - |

**範例**：
```rust
let exit_logic = ExitLogic::new(ExitConfig::default());
let decision = exit_logic.should_exit(
    &position,             // 倉位狀態
    &current_prediction,   // 當前預測
    current_market_price,  // 當前價格
    &market_context,       // 市場狀況
);

match decision {
    ExitDecision::FullExit { reason, urgency, .. } => {
        // 全部出場
    },
    ExitDecision::PartialExit { pct, .. } => {
        // 部分出場（例如 50%）
    },
    ExitDecision::Hold { .. } => {
        // 繼續持有
    },
}
```

### 5. State Machine

**文件**：`src/strategy/nba_state_machine.rs`

**功能**：管理策略狀態轉換

**狀態流程**：
```
WATCH → ARMED → ENTERING → MANAGING → EXITING → EXITED → WATCH
          ↓                                                    ↑
        HALT ────────────────────────────────────────────────┘
```

**範例**：
```rust
let mut state_machine = StateMachine::new();

// Watch → Armed
state_machine.transition(StateEvent::SignalDetected)?;

// Armed → Entering
state_machine.transition(StateEvent::EntryOrderSubmitted)?;

// Entering → Managing
state_machine.transition(StateEvent::EntryFilled)?;

// Managing → Exiting
state_machine.transition(StateEvent::ExitSignal)?;

// Exiting → Exited
state_machine.transition(StateEvent::ExitFilled)?;

// Exited → Watch
state_machine.transition(StateEvent::Reset)?;
```

### 6. Data Collector

**文件**：`src/strategy/nba_data_collector.rs`

**功能**：數據收集與同步

**數據源**：
- Polymarket LOB（每 10 秒）
- NBA live scores（每 30 秒）
- Team statistics（緩存）

**關鍵特性**：
- 時間戳同步（所有數據源必須在 1 秒內）
- 數據新鮮度檢查（最多 5 秒延遲）
- 自動拒絕過時數據

**範例**：
```rust
let collector = DataCollector::new(CollectorConfig::default());

// 獲取同步的市場快照
let snapshot = collector.get_snapshot("market_id", "game_id");

if let Some(snap) = snapshot {
    // snap.orderbook: LOB 數據
    // snap.game_state: 比賽狀態
    // snap.data_latency_ms: 數據延遲
    // snap.sources_synced: 是否同步
}
```

## 配置參數

### Entry Config

```rust
EntryConfig {
    min_edge: 0.05,              // 5% 最小 edge
    min_confidence: 0.70,        // 70% 最小信心
    min_ev_after_fees: 0.02,     // 2% 最小淨 EV
    fee_rate: 0.02,              // 2% 交易費
    slippage_estimate: 0.005,    // 0.5% 滑點
    min_market_price: 0.05,      // 最低價格
    max_market_price: 0.80,      // 最高價格
}
```

### Exit Config

```rust
ExitConfig {
    partial_exit_threshold: 0.02,      // 2% edge 時部分止盈
    partial_exit_pct: 0.50,            // 止盈 50%
    edge_disappear_threshold: -0.01,   // -1% edge 時出場
    trailing_stop_pct: 0.10,           // 10% trailing stop
    min_exit_liquidity_ratio: 2.0,     // 2x 流動性要求
    time_stop_quarter: 4,
    time_stop_minutes: 2.0,            // Q4 最後 2 分鐘
    time_stop_min_profit_pct: 0.10,    // 10% 利潤閾值
}
```

### Filter Config

```rust
FilterConfig {
    max_spread_bps: 200,              // 200 bps 最大價差
    min_book_depth_usd: 1000,         // $1000 最小深度
    max_price_velocity: 0.01,         // 1%/秒 最大速度
    max_data_latency_ms: 2000,        // 2 秒最大延遲
    max_consecutive_same_side: 5,     // 5 筆連續成交
    max_depth_imbalance: 0.8,         // 80/20 最大不平衡
}
```

## 兩週 MVP 計劃

### Week 1：基礎設施

**Day 1-2**：數據收集
- 實現 Polymarket LOB collector
- 實現 NBA API collector
- 驗證時間戳同步

**Day 3-4**：模型訓練
- 收集歷史 NBA 數據（過去 2 季）
- 訓練 logistic regression
- Calibration（isotonic scaling）
- 驗證準確度（Brier score < 0.20）

**Day 5-7**：策略實現
- 整合所有組件
- 實現狀態機
- 設置風險限制

### Week 2：紙上交易

**Day 8-14**：全量記錄模式
- 每個信號都記錄（即使不下單）
- 記錄：
  - 為什麼產生信號
  - 為什麼通過/不通過濾網
  - 如果下單，預期 PnL 是多少
  - 實際市場後續走勢
  - Fill 情況（maker/taker）
  - Adverse selection 指標

### 驗證指標

**模型準確度**：
- Brier score < 0.20
- Calibration curve 接近對角線

**Edge 驗證**：
- 平均 edge > 3%
- Edge 為正的比例 > 60%

**執行質量**：
- Fill rate > 70%（maker）
- Adverse selection < 30%

**PnL Attribution**：
- 賺在哪裡？（模型準 vs 市場錯價 vs 運氣）
- 輸在哪裡？（模型錯 vs 費用 vs 滑點 vs 延遲）

## 文件結構

```
src/strategy/
├── nba_winprob.rs           # Win probability model
├── nba_filters.rs           # Market microstructure filters
├── nba_entry.rs             # Entry logic
├── nba_exit.rs              # Exit logic
├── nba_state_machine.rs     # State machine
└── nba_data_collector.rs    # Data collection

tests/
├── test_winprob_logic.rs    # Model logic test
├── test_filters.rs          # Filters test
└── test_entry_logic.rs      # Entry logic integration test
```

## 下一步

1. **訓練模型**：收集歷史數據，訓練並校準模型
2. **實現數據收集**：連接 Polymarket WebSocket 和 NBA API
3. **紙上交易**：運行 2 週，收集數據，驗證 edge
4. **優化參數**：根據實際數據調整閾值
5. **實盤測試**：小倉位測試，逐步放大

## 關鍵原則

1. **Edge First**：沒有可驗證的 edge 不交易
2. **Defense First**：濾網保護比進場機會更重要
3. **Attribution Always**：每筆交易都要知道為什麼
4. **Iterate Fast**：快速驗證假設，快速調整

---

**版本**：MVP v0.1.0
**日期**：2026-01-13
**狀態**：核心組件完成，待訓練模型和數據收集
