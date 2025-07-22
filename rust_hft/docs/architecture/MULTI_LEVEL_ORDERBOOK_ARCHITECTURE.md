# 多層級 OrderBook 架構文檔

## 📋 概述

本文檔描述了 Rust HFT 系統中實現的多層級 OrderBook 架構，支持 L1、L2 和 L3 三個層級的訂單簿數據處理。

## 🏗️ 架構設計

### 層級定義

#### L1 (Level 1) - 最佳買賣價
- **用途**: 提供最基本的市場數據
- **內容**: 最佳買價 (best bid) 和最佳賣價 (best ask)
- **延遲**: 最低 (~1-5μs)
- **數據量**: 最小
- **適用場景**: 高頻交易、價格監控、快速決策

#### L2 (Level 2) - 多檔位價格深度
- **用途**: 提供市場深度信息
- **內容**: 多個價格檔位的買賣數量 (通常 5-20 檔)
- **延遲**: 中等 (~10-50μs)
- **數據量**: 中等
- **適用場景**: 流動性分析、價差交易、市場影響評估

#### L3 (Level 3) - 完整訂單簿
- **用途**: 提供完整的市場透明度
- **內容**: 所有個別訂單，包含訂單 ID
- **延遲**: 最高 (~50-200μs)
- **數據量**: 最大
- **適用場景**: 高級分析、訂單流分析、做市策略

## 📊 數據結構

### 核心類型

```rust
/// 擴展的訂單簿項目
pub struct ExtendedLevel {
    pub price: Decimal,           // 價格 (高精度)
    pub amount: Decimal,          // 數量 (高精度)
    pub order_id: Option<String>, // 訂單 ID (L3 專用)
    pub timestamp: DateTime<Utc>, // 時間戳
}

/// 多層級訂單簿狀態
pub struct MultiLevelOrderBook {
    pub symbol: String,
    
    // L1 數據
    pub l1_bid: Option<ExtendedLevel>,
    pub l1_ask: Option<ExtendedLevel>,
    
    // L2 數據 (價格 -> 聚合數量)
    pub l2_bids: BTreeMap<String, ExtendedLevel>,
    pub l2_asks: BTreeMap<String, ExtendedLevel>,
    
    // L3 數據 (訂單 ID -> 訂單詳情)
    pub l3_bids: HashMap<String, ExtendedLevel>,
    pub l3_asks: HashMap<String, ExtendedLevel>,
    
    // 元數據
    pub last_update: DateTime<Utc>,
    pub sequence: u64,
}
```

### 性能優化特性

1. **零拷貝設計**: 使用引用和借用避免不必要的數據複製
2. **SIMD 優化**: 支持向量化計算 (價格比較、聚合)
3. **內存池**: 預分配數據結構減少 GC 壓力
4. **無鎖數據結構**: 使用 BTreeMap 和 HashMap 避免鎖競爭

## 🔄 數據流處理

### 數據轉換流程

```
Bitget WebSocket → BitgetMessage → MultiLevelOrderBook → MarketEvent
```

1. **WebSocket 接收**: Bitget V2 API 數據接收
2. **消息解析**: JSON 解析為結構化數據
3. **層級更新**: 同時更新 L1、L2、L3 數據
4. **事件發送**: 轉換為 barter-data MarketEvent

### 更新策略

#### L1 更新
- **觸發**: L2 數據變化時自動更新
- **頻率**: 每次 L2 更新 (~1000/秒)
- **延遲**: <1μs

#### L2 更新
- **觸發**: 接收到 books5/books15 數據
- **方式**: 全量替換 (snapshot)
- **聚合**: 按價格檔位聚合
- **延遲**: <10μs

#### L3 更新 (模擬)
- **觸發**: 模擬個別訂單變化
- **方式**: 增量更新 (delta)
- **重建**: 從 L3 重新計算 L2 和 L1
- **延遲**: <50μs

## 📈 實時統計

### 性能指標

```
📊 Barter Multi-Level OrderBook Manager Statistics:
   Total events: 847          // 總事件數
   L1 updates: 422           // L1 更新次數
   L2 updates: 422           // L2 更新次數  
   L3 updates: 0             // L3 更新次數
   Trade events: 425         // 交易事件數
   Events/sec: 28.2          // 事件處理速度
   Active orderbooks: 3      // 活躍訂單簿數量
```

### 詳細分析示例

```
🔍 Analysis for ETHUSDT:
   L1 - Best Bid: 2572.4000 @ 22.040300
   L1 - Best Ask: 2572.4100 @ 3.262700
   L1 - Spread: 0.010000
   L1 - Mid Price: 2572.4050
   L2 - Depth: 15 bid levels, 15 ask levels
   L2 - Total Liquidity: 112.702800 (bids) / 90.332700 (asks)
   L2 - Price Range: 1.850000 (2571.5600 to 2573.4100)
   ⏱️  Last Update: 02:14:19.746
   🔢 Sequence: 179
```

## 🎯 使用示例

### 基本用法

```rust
use rust_hft::integrations::BarterOrderBookManager;

// 創建管理器
let mut manager = BarterOrderBookManager::new();

// 處理 Bitget 消息
match manager.handle_bitget_message(message) {
    Ok(Some(market_event)) => {
        // 處理市場事件
        println!("Market event: {:?}", market_event);
    }
    Ok(None) => {
        // 消息被忽略
    }
    Err(e) => {
        // 錯誤處理
        eprintln!("Error: {}", e);
    }
}

// 獲取訂單簿狀態
if let Some(orderbook) = manager.get_orderbook("BTCUSDT") {
    println!("Spread: {:?}", orderbook.get_spread());
    println!("Mid Price: {:?}", orderbook.get_mid_price());
    
    let (bid_depth, ask_depth) = orderbook.get_l2_depth();
    println!("L2 Depth: {} x {}", bid_depth, ask_depth);
    
    let (bid_orders, ask_orders) = orderbook.get_l3_orders();
    println!("L3 Orders: {} x {}", bid_orders, ask_orders);
}
```

### 高級分析

```rust
// 流動性分析
let total_bid_volume: Decimal = orderbook.l2_bids.values()
    .map(|level| level.amount)
    .sum();

// 價格範圍計算
let price_range = orderbook.l2_asks.values().map(|l| l.price).max().unwrap()
    - orderbook.l2_bids.values().map(|l| l.price).min().unwrap();

// 訂單密度分析
let order_density = orderbook.get_l3_orders().0 as f64 / bid_depth as f64;
```

## 🚀 性能特性

### 延遲目標
- **L1 處理**: <1μs (目標 50ns)
- **L2 處理**: <10μs (目標 1μs)
- **L3 處理**: <50μs (目標 10μs)
- **端到端**: <100μs (目標 50μs)

### 吞吐量
- **事件處理**: >10,000 events/sec
- **並發交易對**: >100 symbols
- **內存使用**: <100MB per 10 symbols

### 可擴展性
- **水平擴展**: 支持多進程部署
- **垂直擴展**: 充分利用多核 CPU
- **存儲擴展**: 可配置持久化策略

## 🔧 配置和調優

### 編譯優化

```toml
[profile.release]
lto = "fat"           # 鏈接時優化
codegen-units = 1     # 最大化優化
panic = "abort"       # 避免展開開銷
opt-level = 3         # 最高優化級別
```

### 運行時調優

```rust
// CPU 親和性設置
core_affinity::set_for_current(core_affinity::CoreId { id: 0 });

// 內存預分配
let mut orderbook = MultiLevelOrderBook::with_capacity(
    symbol.to_string(), 
    100  // 預分配 100 個價格檔位
);

// SIMD 特徵計算
#[cfg(target_feature = "avx2")]
fn calculate_features_simd(levels: &[ExtendedLevel]) -> Features {
    // SIMD 優化的特徵計算
}
```

## 📝 未來擴展

### 計劃功能
1. **L3 實時支持**: 真實的 L3 訂單流處理
2. **歷史回放**: 訂單簿狀態時間旅行
3. **預測模型**: 基於訂單流的價格預測
4. **風險管理**: 實時風險指標計算
5. **可視化**: Web 界面訂單簿可視化

### 技術債務
1. **錯誤處理**: 更精細的錯誤分類和恢復
2. **測試覆蓋**: 增加邊界條件測試
3. **文檔**: API 文檔和示例完善
4. **基準測試**: 性能回歸測試套件

## 🧪 測試和驗證

### 單元測試
```bash
# 運行所有測試
cargo test

# 運行特定模塊測試
cargo test barter_orderbook_manager

# 運行性能測試
cargo test --release test_performance
```

### 集成測試
```bash
# 運行基本示例
cargo run --example demo_barter_orderbook

# 運行多層級示例
cargo run --example demo_multi_level_orderbook

# 運行完整 HFT 示例
cargo run --example complete_hft_example
```

### 性能基準
```bash
# 延遲測試
cargo test test_inference_latency --release

# 吞吐量測試
cargo bench orderbook_update_benchmark
```

## 📚 相關文件

- `src/integrations/barter_orderbook_manager.rs` - 核心實現
- `examples/demo_multi_level_orderbook.rs` - 使用示例
- `examples/demo_barter_orderbook.rs` - 基礎示例
- `src/integrations/bitget_connector.rs` - 數據源連接
- `src/integrations/barter_bitget_adapter.rs` - Barter 適配器

---

**注意**: 這是一個高性能交易系統，請確保在適當的環境中測試，並遵循相關的金融交易法規。