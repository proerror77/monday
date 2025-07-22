# 🦀 Rust HFT API 完整參考

**核心API文檔** - 所有公共接口和使用示例

## 📚 模組概覽

```rust
rust_hft/
├── core/           // 核心組件
├── engine/         // 交易引擎  
├── ml/             // 機器學習
├── integrations/   // 交易所集成
├── utils/          // 工具函數
└── database/       // 數據存儲
```

## 🏗️ Core 模組

### OrderBook API

#### 基礎操作
```rust
use rust_hft::core::orderbook::OrderBook;
use rust_decimal::Decimal;

// 創建訂單簿
let mut orderbook = OrderBook::new("BTCUSDT".to_string());

// 更新買賣單 - 延遲 <100ns
orderbook.update_bid(
    Decimal::from_str("45000.0")?, 
    Decimal::from_str("1.5")?
)?;

orderbook.update_ask(
    Decimal::from_str("45001.0")?, 
    Decimal::from_str("1.2")?
)?;

// 獲取最佳價格
let (best_bid, best_ask) = orderbook.get_best_prices();
let spread = best_ask - best_bid;

// 獲取市場深度
let depth = orderbook.get_depth(10); // 前10檔
let total_bid_volume = orderbook.get_total_bid_volume();
```

#### 高級功能
```rust
// 訂單簿快照
let snapshot = orderbook.create_snapshot()?;
println!("Snapshot: {} bids, {} asks", 
         snapshot.bids.len(), 
         snapshot.asks.len());

// 增量更新
let updates = orderbook.get_incremental_updates_since(last_sequence_id)?;
for update in updates {
    match update.update_type {
        UpdateType::Insert => println!("New level: {} @ {}", update.price, update.quantity),
        UpdateType::Update => println!("Updated: {} @ {}", update.price, update.quantity),
        UpdateType::Delete => println!("Removed: {}", update.price),
    }
}

// 訂單簿統計
let stats = orderbook.calculate_statistics()?;
println!("Mid price: {}, Weighted mid: {}", stats.mid_price, stats.weighted_mid);
println!("OBI: {:.4}", stats.order_book_imbalance);
```

### LockFree OrderBook (無鎖版本)

```rust
use rust_hft::core::lockfree_orderbook::LockFreeOrderBook;
use std::sync::Arc;

// 多線程安全的訂單簿
let orderbook = Arc::new(LockFreeOrderBook::new("ETHUSDT".to_string()));

// 多線程更新 (無鎖操作)
let ob_clone = orderbook.clone();
tokio::spawn(async move {
    for i in 0..1000 {
        let price = 3000.0 + (i as f64 * 0.01);
        ob_clone.update_bid_atomic(price.into(), 1.0.into()).await?;
    }
    Ok::<_, anyhow::Error>(())
});

// 原子讀取
let (bid, ask) = orderbook.get_best_prices_atomic();
let depth_stats = orderbook.get_depth_statistics();
```

### Configuration API

```rust
use rust_hft::core::config::Config;

// 從文件加載配置
let config = Config::from_file("config/production.yaml").await?;

// 動態配置更新
config.trading.max_position_size = Decimal::from_str("5000.0")?;
config.save_to_file("config/updated.yaml").await?;

// 配置驗證
match config.validate() {
    Ok(_) => println!("Configuration valid"),
    Err(errors) => {
        for error in errors {
            eprintln!("Config error: {}", error);
        }
    }
}

// 環境變量覆蓋
let config = Config::from_file_with_env("config/base.yaml", "HFT_")?;
// HFT_TRADING_MAX_POSITION_SIZE=1000.0 會覆蓋文件中的值
```

## ⚡ Engine 模組

### Trading Strategy API

#### ML 策略
```rust
use rust_hft::engine::strategy::{MLStrategy, TradingConfig};

// 初始化ML策略
let config = TradingConfig {
    model_path: "models/hft_v2.onnx".into(),
    feature_window: 50,
    confidence_threshold: 0.7,
    ..Default::default()
};

let mut strategy = MLStrategy::new(config).await?;

// 實時預測 - 延遲 <1μs
let features = extract_features(&orderbook)?;
let signal = strategy.predict(&features)?;

match signal.action {
    TradingAction::Buy => {
        println!("買入信號: 概率 {:.2}%, 置信度 {:.2}%", 
                 signal.probability * 100.0, 
                 signal.confidence * 100.0);
    },
    TradingAction::Sell => {
        println!("賣出信號: 概率 {:.2}%, 置信度 {:.2}%", 
                 signal.probability * 100.0, 
                 signal.confidence * 100.0);
    },
    TradingAction::Hold => {
        println!("持有");
    }
}

// 零分配預測 (最高性能)
let signal = strategy.predict_zero_alloc(&features)?;
```

#### 風險管理
```rust
use rust_hft::engine::risk::{RiskManager, RiskParams};

let risk_params = RiskParams {
    max_position_size: Decimal::from_str("1000.0")?,
    stop_loss_pct: Decimal::from_str("0.02")?,        // 2%
    take_profit_pct: Decimal::from_str("0.05")?,      // 5% 
    max_drawdown: Decimal::from_str("0.10")?,         // 10%
    daily_loss_limit: Decimal::from_str("500.0")?,
    ..Default::default()
};

let mut risk_manager = RiskManager::new(risk_params);

// 風險檢查 - 延遲 <500ns
let signal = TradingSignal { /* ... */ };
let portfolio = get_current_portfolio()?;

match risk_manager.check_signal(&signal, &portfolio)? {
    RiskDecision::Approve => {
        println!("風險檢查通過，執行交易");
        execute_order(&signal)?;
    },
    RiskDecision::Reject(reason) => {
        println!("風險檢查拒絕: {}", reason);
    },
    RiskDecision::ModifyQuantity(new_qty) => {
        println!("風險調整數量: {} -> {}", signal.quantity, new_qty);
        execute_modified_order(&signal, new_qty)?;
    }
}

// 實時風險指標
let metrics = risk_manager.get_real_time_metrics()?;
println!("當前風險: VaR={:.2}, 最大回撤={:.2}%", 
         metrics.var_95, 
         metrics.max_drawdown * 100.0);
```

### Execution Engine API

```rust
use rust_hft::engine::execution::{OrderExecutor, ExecutionConfig};

let config = ExecutionConfig {
    exchange: "bitget".into(),
    api_credentials: load_api_credentials()?,
    max_orders_per_second: 100,
    retry_attempts: 3,
    timeout_ms: 5000,
};

let mut executor = OrderExecutor::new(config).await?;

// 同步執行 (阻塞直到完成)
let order = Order {
    symbol: "BTCUSDT".into(),
    side: OrderSide::Buy,
    quantity: Decimal::from_str("0.1")?,
    order_type: OrderType::Market,
    ..Default::default()
};

match executor.execute_sync(&order).await? {
    ExecutionResult::Filled { fill_price, fill_quantity, .. } => {
        println!("訂單成交: {} @ {}", fill_quantity, fill_price);
    },
    ExecutionResult::PartialFill { filled, remaining, .. } => {
        println!("部分成交: {} 已成交, {} 剩餘", filled, remaining);  
    },
    ExecutionResult::Rejected { reason } => {
        println!("訂單被拒: {}", reason);
    }
}

// 異步執行 (非阻塞)
let execution_id = executor.execute_async(&order).await?;
let result = executor.get_execution_result(execution_id).await?;

// 批量執行 (高吞吐)
let orders = vec![order1, order2, order3];
let results = executor.execute_batch(&orders).await?;
```

## 🧠 ML 模組

### Feature Extraction API

```rust
use rust_hft::ml::{FeatureExtractor, FeatureConfig};

let config = FeatureConfig {
    window_size: 50,                    // 50個歷史點
    indicators: vec![
        "sma_10", "ema_20", "rsi_14", 
        "bollinger_bands", "macd"
    ],
    include_orderbook_features: true,
    include_trade_features: true,
    normalization: NormalizationType::ZScore,
};

let mut extractor = FeatureExtractor::new(config);

// 提取特徵 - 延遲 <5μs  
let features = extractor.extract_features(
    &orderbook,
    sequence_id,
    timestamp
)?;

// 檢查特徵完整性
println!("提取了 {} 個特徵", features.len());
for (name, value) in &features {
    if value.is_nan() || value.is_infinite() {
        eprintln!("警告: 特徵 {} 值異常: {}", name, value);
    }
}

// 轉換為模型輸入格式
let feature_vector = features.to_vector();
let tensor = Tensor::from_slice(&feature_vector)?;
```

### Model Inference API

```rust  
use rust_hft::ml::inference::{ModelInference, InferenceConfig};

// CPU 推理
let config = InferenceConfig {
    model_path: "models/lstm_model.onnx".into(),
    device: Device::CPU,
    optimization_level: OptimizationLevel::All,
    thread_count: 4,
};

let mut inference = ModelInference::new(config).await?;

// GPU 推理 (如果支持)
let gpu_config = InferenceConfig {
    model_path: "models/lstm_model.onnx".into(),  
    device: Device::CUDA(0), // GPU 0
    batch_size: 1,
    fp16_enabled: true,      // 半精度加速
};

let mut gpu_inference = ModelInference::new(gpu_config).await?;

// 實時推理 - 延遲 <1μs
let prediction = inference.predict(&features)?;
println!("預測結果: 買入概率 {:.2}%", prediction.buy_probability * 100.0);

// 批量推理 (高吞吐量)
let batch_features = vec![features1, features2, features3];
let predictions = inference.predict_batch(&batch_features)?;

// 模型性能監控
let stats = inference.get_performance_stats();
println!("平均推理時間: {:.2}μs, 吞吐量: {} qps", 
         stats.avg_latency_us, 
         stats.queries_per_second);
```

## 🔌 Integrations 模組

### Bitget Connector API

```rust
use rust_hft::integrations::{
    UnifiedBitgetConnector, 
    UnifiedBitgetConfig, 
    BitgetChannel
};

// WebSocket 連接配置
let config = UnifiedBitgetConfig {
    ws_url: "wss://ws.bitget.com/v2/ws/public".into(),
    api_key: Some("your_api_key".into()),
    api_secret: Some("your_secret".into()),
    passphrase: Some("your_passphrase".into()),
    max_reconnect_attempts: 5,
    reconnect_delay_ms: 1000,
    enable_compression: true,
    ping_interval_secs: 30,
    connection_timeout_secs: 10,
};

let mut connector = UnifiedBitgetConnector::new(config);

// 訂閱市場數據
connector.subscribe("BTCUSDT", BitgetChannel::OrderBook5).await?;
connector.subscribe("ETHUSDT", BitgetChannel::Trades).await?;
connector.subscribe("SOLUSDT", BitgetChannel::Ticker).await?;

// 啟動連接並接收消息
let mut message_rx = connector.start().await?;

while let Some(message) = message_rx.recv().await {
    match message.channel {
        BitgetChannel::OrderBook5 => {
            // 處理訂單簿更新
            let orderbook_data = parse_orderbook_data(&message.data)?;
            update_local_orderbook(orderbook_data).await?;
        },
        BitgetChannel::Trades => {
            // 處理交易數據
            let trades = parse_trade_data(&message.data)?;
            process_trades(trades).await?;
        },
        BitgetChannel::Ticker => {
            // 處理ticker數據
            let ticker = parse_ticker_data(&message.data)?;
            update_price_display(ticker).await?;
        }
        _ => {}
    }
}

// 連接統計
let stats = connector.get_connection_stats()?;
println!("接收消息: {}, 重連次數: {}, 延遲: {:.2}ms", 
         stats.messages_received,
         stats.reconnect_count, 
         stats.avg_latency_ms);
```

### Multi-Exchange Support

```rust
use rust_hft::integrations::{ExchangeManager, ExchangeType};

let mut exchange_manager = ExchangeManager::new();

// 添加多個交易所
exchange_manager.add_exchange(
    ExchangeType::Bitget, 
    bitget_config
).await?;

exchange_manager.add_exchange(
    ExchangeType::Binance, 
    binance_config  
).await?;

// 跨交易所套利監控
let arbitrage_opportunities = exchange_manager
    .scan_arbitrage_opportunities(&["BTCUSDT", "ETHUSDT"])
    .await?;

for opportunity in arbitrage_opportunities {
    if opportunity.profit_bps > 10 { // >10 basis points
        println!("套利機會: {} -> {}, 利潤: {:.2}bps",
                 opportunity.buy_exchange,
                 opportunity.sell_exchange, 
                 opportunity.profit_bps);
    }
}
```

## 🗄️ Database 模組

### ClickHouse API

```rust
use rust_hft::database::{ClickHouseClient, BatchWriter};

// 連接ClickHouse
let client = ClickHouseClient::new("http://localhost:8123").await?;

// 批量寫入市場數據
let mut batch = BatchWriter::new(1000); // 1000條批量

for update in orderbook_updates {
    batch.add_market_data(MarketDataPoint {
        timestamp: update.timestamp,
        symbol: update.symbol.clone(),
        bid_price: update.best_bid_price,
        ask_price: update.best_ask_price,
        bid_volume: update.best_bid_volume,
        ask_volume: update.best_ask_volume,
        features: update.features.clone(),
    })?;
    
    // 自動批量提交
    if batch.should_flush() {
        client.write_batch(&batch).await?;
        batch.clear();
    }
}

// 查詢歷史數據
let historical_data = client.query_range(
    "BTCUSDT",
    start_time,
    end_time,
    Some("1m") // 1分鐘聚合
).await?;

println!("查詢到 {} 條歷史記錄", historical_data.len());
```

### Feature Store API

```rust
use rust_hft::utils::{FeatureStore, FeatureStoreConfig};

let config = FeatureStoreConfig {
    db_path: "data/features.redb".into(),
    compression_enabled: true,
    cache_size_mb: 256,
    write_buffer_size: 10000,
    retention_days: 30,
};

let mut store = FeatureStore::new(config)?;

// 存儲特徵 - 延遲 <10μs
let feature_set = FeatureSet {
    timestamp: now_micros(),
    symbol: "BTCUSDT".into(),
    features: feature_map,
};

store.store_features(&feature_set)?;

// 批量存儲
let feature_batch = vec![feature_set1, feature_set2, feature_set3];
store.store_features_batch(&feature_batch)?; // 更高吞吐量

// 查詢特徵 - 延遲 <50μs  
let features = store.query_features(
    "BTCUSDT",
    start_time,
    end_time
)?;

// 時序查詢
let recent_features = store.query_recent_features(
    "BTCUSDT", 
    Duration::from_secs(300) // 最近5分鐘
)?;

// 壓縮統計
let stats = store.get_compression_stats()?;
println!("壓縮率: {:.1}%, 節省空間: {:.1}MB", 
         stats.compression_ratio * 100.0,
         stats.space_saved_mb);
```

## ⚡ Utils 模組

### Performance Utilities

```rust
use rust_hft::utils::performance::*;

// 延遲測量
let start = now_nanos();
execute_trading_logic()?;
let elapsed = now_nanos() - start;
println!("執行時間: {}ns", elapsed);

// TSC 高精度計時 (如果支持)
if let Ok(freq) = calibrate_tsc_frequency() {
    let start_tsc = read_tsc();
    execute_critical_section()?;
    let end_tsc = read_tsc();
    let elapsed_us = ((end_tsc - start_tsc) as f64 * 1_000_000.0) / freq as f64;
    println!("高精度執行時間: {:.3}μs", elapsed_us);
}

// SIMD 功能檢測
let simd_support = detect_simd_capabilities();
match simd_support {
    SIMDCapability::AVX512 => println!("支持 AVX512"),
    SIMDCapability::AVX2 => println!("支持 AVX2"),
    SIMDCapability::SSE => println!("支持 SSE"),
    SIMDCapability::None => println!("無SIMD支持"),
}

// 內存預分配
prefault_memory(512 * 1024 * 1024)?; // 預分配512MB

// CPU親和性設置
let affinity_manager = CpuAffinityManager::new(2); // 綁定到核心2
affinity_manager.bind_to_cpu()?;
```

### Latency Tracker

```rust
use rust_hft::utils::performance::LatencyTracker;

let mut tracker = LatencyTracker::new();

// 記錄延遲
tracker.record("orderbook_update", 85);  // 85μs
tracker.record("feature_extraction", 3200);  // 3.2ms
tracker.record("ml_inference", 800);     // 0.8ms

// 獲取統計信息
let stats = tracker.get_stats("ml_inference").unwrap();
println!("ML推理統計:");
println!("  平均: {:.2}μs", stats.mean);
println!("  最小: {}μs", stats.min); 
println!("  最大: {}μs", stats.max);
println!("  計數: {}", stats.count);

// 百分位數
let p95 = tracker.get_percentile("ml_inference", 95.0).unwrap();
let p99 = tracker.get_percentile("ml_inference", 99.0).unwrap();
println!("  P95: {:.2}μs", p95);
println!("  P99: {:.2}μs", p99);

// 導出統計報告
tracker.export_csv("latency_report.csv")?;
```

## 🧪 Testing API

```rust
use rust_hft::testing::{TestFramework, MarketSimulator};

// 創建測試環境
let mut test_framework = TestFramework::new();

// 模擬市場數據
let market_sim = MarketSimulator::new()
    .with_symbols(vec!["BTCUSDT", "ETHUSDT"])
    .with_price_range(40000.0..50000.0)
    .with_volatility(0.02)
    .with_tick_rate(1000); // 1000 updates/second

// 運行策略測試
let results = test_framework.run_strategy_test(
    strategy,
    market_sim,
    Duration::from_minutes(10)
).await?;

println!("測試結果:");
println!("  總交易: {}", results.total_trades);
println!("  勝率: {:.1}%", results.win_rate * 100.0);
println!("  最大回撤: {:.2}%", results.max_drawdown * 100.0);
println!("  夏普比率: {:.2}", results.sharpe_ratio);
```

## 🔧 類型定義

### 核心數據類型
```rust
// 價格和數量
pub type Price = rust_decimal::Decimal;
pub type Quantity = rust_decimal::Decimal;
pub type Timestamp = u64;  // 微秒時間戳

// 交易相關
#[derive(Debug, Clone)]
pub struct TradingSignal {
    pub symbol: String,
    pub action: TradingAction,
    pub probability: f64,     // 0.0 - 1.0
    pub confidence: f64,      // 0.0 - 1.0
    pub timestamp: Timestamp,
    pub features: Vec<f64>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy)]
pub enum TradingAction {
    Buy,
    Sell, 
    Hold,
}

// 訂單相關
#[derive(Debug, Clone)]
pub struct Order {
    pub id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: Quantity,
    pub price: Option<Price>,
    pub status: OrderStatus,
    pub timestamp: Timestamp,
    pub filled_quantity: Quantity,
    pub average_price: Option<Price>,
}

#[derive(Debug, Clone, Copy)]
pub enum OrderSide { Buy, Sell }

#[derive(Debug, Clone, Copy)]  
pub enum OrderType { Market, Limit, StopLoss, TakeProfit }

#[derive(Debug, Clone, Copy)]
pub enum OrderStatus { Pending, PartiallyFilled, Filled, Cancelled, Rejected }
```

## ❗ 錯誤處理

```rust
use rust_hft::error::HftError;

// 統一錯誤類型
#[derive(Debug, thiserror::Error)]
pub enum HftError {
    #[error("訂單簿錯誤: {0}")]
    OrderBook(String),
    
    #[error("ML模型錯誤: {0}")]
    MachineLearning(String),
    
    #[error("網路連接錯誤: {0}")]
    Network(String),
    
    #[error("配置錯誤: {0}")]
    Configuration(String),
    
    #[error("風險管理錯誤: {0}")]
    RiskManagement(String),
    
    #[error("數據庫錯誤: {0}")]  
    Database(String),
}

// 結果類型
pub type HftResult<T> = Result<T, HftError>;

// 錯誤處理示例
match execute_trading_strategy().await {
    Ok(result) => println!("策略執行成功: {:?}", result),
    Err(HftError::Network(msg)) => {
        eprintln!("網路錯誤: {}", msg);
        // 重試邏輯
    },
    Err(HftError::RiskManagement(msg)) => {
        eprintln!("風險控制: {}", msg);
        // 停止交易
    },
    Err(e) => eprintln!("其他錯誤: {}", e),
}
```

---

這是Rust HFT系統的完整API參考。所有函數都經過延遲優化，適合高頻交易場景使用。

*API版本: v3.0 | 更新時間: 2025-07-22*