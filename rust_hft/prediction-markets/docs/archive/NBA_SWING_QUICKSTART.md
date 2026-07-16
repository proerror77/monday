# NBA Swing Strategy - 快速開始指南

## 概述

這是一個基於模型的 NBA moneyline swing 交易策略，用於在 Polymarket 上交易強隊落後時的翻盤機會。

## 核心理念

**不是「價格 < 0.20 就買」，而是「有可驗證的 edge 才買」**

## 快速測試

### 1. 測試 Win Probability Model

```bash
rustc test_winprob_logic.rs -o /tmp/test_winprob && /tmp/test_winprob
```

**預期輸出**：
```
Test 1: Ahead by 10 in Q4, 5 min left
  Win Prob: 88.1%
  Confidence: 95.0%
  ✓ 通過
```

### 2. 測試 Market Filters

```bash
rustc test_filters.rs -o /tmp/test_filters && /tmp/test_filters
```

**預期輸出**：
```
Test 1: Good Market Conditions
  Result: ✓ PASS

Test 2: Wide Spread (500 bps)
  Result: ✗ FAIL (expected)
  ✓ 通過
```

### 3. 測試 Entry Logic（完整集成）

```bash
rustc test_entry_logic.rs -o /tmp/test_entry && /tmp/test_entry
```

**預期輸出**：
```
Scenario 1: Perfect Entry Conditions
  ✓ 或 ✗ (取決於模型係數)
```

## 使用範例

### 基本流程

```rust
use ploy::strategy::{
    LiveWinProbModel, GameFeatures,
    MarketFilters, FilterConfig, MarketContext,
    EntryLogic, EntryConfig, EntryDecision,
};

// 1. 加載模型
let model = LiveWinProbModel::from_file("model.json")?;

// 2. 創建濾網
let filters = MarketFilters::new(FilterConfig::default());

// 3. 創建 entry logic
let entry_logic = EntryLogic::new(EntryConfig::default());

// 4. 獲取比賽數據
let features = GameFeatures {
    point_diff: -12.0,      // 落後 12 分
    time_remaining: 8.0,    // 剩 8 分鐘
    quarter: 3,
    possession: 1.0,
    pregame_spread: 5.0,
    elo_diff: 50.0,
};

// 5. 預測勝率
let prediction = model.predict(&features);
println!("Win Prob: {:.1}%", prediction.win_prob * 100.0);

// 6. 檢查市場條件
let market_context = MarketContext {
    spread_bps: Some(150),
    bid_depth: Decimal::new(2000, 0),
    ask_depth: Decimal::new(1800, 0),
    price_velocity: Some(0.005),
    data_latency_ms: 800,
    // ...
};

let filter_result = filters.can_enter(&market_context);

// 7. 決定是否進場
let market_price = Decimal::new(15, 2); // 0.15
let decision = entry_logic.should_enter(
    &prediction,
    market_price,
    &filter_result,
);

match decision {
    EntryDecision::Approve(signal) => {
        println!("✓ 進場！");
        println!("  Edge: {:.2}%", signal.edge * 100.0);
        println!("  Net EV: {:.2}%", signal.net_ev * 100.0);
    },
    EntryDecision::Reject { reason, .. } => {
        println!("✗ 拒絕：{}", reason);
    },
}
```

## 配置調整

### 保守配置（推薦 MVP）

```rust
EntryConfig {
    min_edge: 0.05,              // 5% 最小 edge
    min_confidence: 0.70,        // 70% 最小信心
    min_ev_after_fees: 0.02,     // 2% 最小淨 EV
    // ...
}

FilterConfig {
    max_spread_bps: 200,         // 2% 最大價差
    min_book_depth_usd: 1000,    // $1000 最小深度
    max_data_latency_ms: 2000,   // 2 秒最大延遲
    // ...
}
```

### 激進配置（僅供參考）

```rust
EntryConfig {
    min_edge: 0.03,              // 3% 最小 edge（更寬鬆）
    min_confidence: 0.60,        // 60% 最小信心
    min_ev_after_fees: 0.01,     // 1% 最小淨 EV
    // ...
}

FilterConfig {
    max_spread_bps: 300,         // 3% 最大價差
    min_book_depth_usd: 500,     // $500 最小深度
    max_data_latency_ms: 3000,   // 3 秒最大延遲
    // ...
}
```

## 兩週 MVP 檢查清單

### Week 1：基礎設施

- [ ] **Day 1-2**：數據收集
  - [ ] 連接 Polymarket WebSocket
  - [ ] 連接 NBA API
  - [ ] 驗證時間戳同步
  - [ ] 測試數據質量

- [ ] **Day 3-4**：模型訓練
  - [ ] 收集歷史數據（2 季）
  - [ ] 訓練 logistic regression
  - [ ] Calibration（isotonic）
  - [ ] 驗證 Brier score < 0.20

- [ ] **Day 5-7**：策略整合
  - [ ] 整合所有組件
  - [ ] 端到端測試
  - [ ] 設置風險限制

### Week 2：紙上交易

- [ ] **Day 8-14**：全量記錄
  - [ ] 運行紙上交易
  - [ ] 記錄所有信號
  - [ ] 記錄市場走勢
  - [ ] 分析結果

### 驗證指標

- [ ] 模型準確度：Brier score < 0.20
- [ ] Edge 驗證：平均 edge > 3%
- [ ] 執行質量：Fill rate > 70%
- [ ] PnL Attribution：識別盈虧來源

## 常見問題

### Q: 模型係數從哪裡來？

A: 需要用歷史 NBA 數據訓練。當前代碼使用佔位符係數，僅供測試。

### Q: 如何獲取 Polymarket 數據？

A: 使用 Polymarket CLOB API：
- WebSocket: `wss://clob.polymarket.com`
- REST API: `https://clob.polymarket.com`

### Q: 如何獲取 NBA 數據？

A: 可選：
- ESPN API（免費，但有限制）
- NBA Stats API（官方）
- SportsRadar（付費，最準確）

### Q: 為什麼不用 Grok？

A: MVP 階段優先驗證基礎 edge。Grok 延遲高（1-5秒）、成本高、不可回測。未來可以加入。

### Q: 如何調整參數？

A: 根據紙上交易結果：
- 如果信號太少 → 降低 min_edge
- 如果虧損太多 → 提高 min_edge
- 如果經常被拒絕 → 放寬 filters

### Q: 如何驗證 edge 是否真實？

A: 兩週紙上交易後：
1. 計算平均 edge
2. 計算 edge 為正的比例
3. 分析盈虧來源（模型準 vs 運氣）
4. 如果平均 edge > 3% 且比例 > 60%，edge 可能真實存在

## 文件結構

```
src/strategy/
├── nba_winprob.rs           # Win probability model
├── nba_filters.rs           # Market filters
├── nba_entry.rs             # Entry logic
├── nba_exit.rs              # Exit logic
├── nba_state_machine.rs     # State machine
└── nba_data_collector.rs    # Data collection

docs/
├── NBA_SWING_STRATEGY_MVP.md        # 完整文檔
└── NBA_SWING_STRATEGY_COMPLETION.md # 完成總結

tests/
├── test_winprob_logic.rs    # Model test
├── test_filters.rs          # Filters test
└── test_entry_logic.rs      # Integration test
```

## 下一步

1. **訓練模型**：收集歷史數據，訓練係數
2. **實現數據收集**：連接 Polymarket 和 NBA API
3. **紙上交易**：運行 2 週，驗證 edge
4. **優化參數**：根據結果調整
5. **實盤測試**：小倉位開始

## 支持

- 完整文檔：`docs/NBA_SWING_STRATEGY_MVP.md`
- 完成總結：`docs/NBA_SWING_STRATEGY_COMPLETION.md`
- 測試腳本：`test_*.rs`

---

**版本**：MVP v0.1.0
**日期**：2026-01-13
