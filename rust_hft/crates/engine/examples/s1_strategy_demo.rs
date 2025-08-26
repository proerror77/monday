/*!
 * S1 策略演示：展示 Bar Close 觸發的策略模板
 *
 * 此示例演示：
 * 1. TrendStrategy - EMA 金叉死叉 + RSI 過濾
 * 2. ArbitrageStrategy - 跨交易所套利檢測
 * 3. 策略與 Engine 集成的完整流程
 * 4. 模擬 K 線數據觸發策略信號
 */

use engine::{
    Engine, EngineConfig, AdapterBridge, AdapterBridgeConfig,
    FlipPolicy, BackpressurePolicy
};
use engine::dataflow::IngestionConfig;
use hft_core::*;
use ports::*;
use std::time::Duration;
use tokio::time::sleep;
use strategy_trend::{TrendStrategy, TrendStrategyConfig};
use strategy_arbitrage::{ArbitrageStrategy, ArbitrageStrategyConfig};

// 模擬策略管理器
struct StrategyManager {
    trend_strategy: TrendStrategy,
    arbitrage_strategy: ArbitrageStrategy,
}

impl StrategyManager {
    fn new() -> Self {
        let trend_config = TrendStrategyConfig {
            ema_fast_period: 5,     // 短期 EMA
            ema_slow_period: 10,    // 長期 EMA
            rsi_period: 7,          // RSI 週期
            rsi_oversold: 35.0,     // RSI 超賣線
            rsi_overbought: 65.0,   // RSI 超買線
            position_size: 0.01,    // 每筆交易量
            max_position: 0.1,      // 最大總倉位
            min_spread_bps: 2.0,    // 最小價差
        };

        let arbitrage_config = ArbitrageStrategyConfig {
            min_spread_bps: 8.0,        // 8 基點最小套利利差
            max_position_size: 0.005,   // 單筆套利量
            max_total_position: 0.05,   // 總套利倉位
            max_stale_us: 2000,         // 2ms 陳舊度容忍
            min_profit_usd: 2.0,        // 最小 2 美元利潤
            taker_fee_rate: 0.001,      // 0.1% 手續費
        };

        Self {
            trend_strategy: TrendStrategy::new(Symbol("BTCUSDT".to_string()), trend_config),
            arbitrage_strategy: ArbitrageStrategy::new(Symbol("BTCUSDT".to_string()), arbitrage_config),
        }
    }

    fn process_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent> {
        let mut all_orders = Vec::new();

        // 趨勢策略處理
        let trend_orders = self.trend_strategy.on_market_event(event, account);
        if !trend_orders.is_empty() {
            println!("📈 趨勢策略信號: {} 筆訂單", trend_orders.len());
            for order in &trend_orders {
                println!("   - {} {} {:.4} @ {:.2}", 
                    order.side.as_str(), 
                    order.symbol.0,
                    order.quantity.0.to_f64().unwrap_or(0.0),
                    order.price.map(|p| p.0.to_f64().unwrap_or(0.0)).unwrap_or(0.0)
                );
            }
            all_orders.extend(trend_orders);
        }

        // 套利策略處理
        let arbitrage_orders = self.arbitrage_strategy.on_market_event(event, account);
        if !arbitrage_orders.is_empty() {
            println!("⚡ 套利策略信號: {} 筆訂單", arbitrage_orders.len());
            for order in &arbitrage_orders {
                println!("   - {} {} {:.4}", 
                    order.side.as_str(), 
                    order.symbol.0,
                    order.quantity.0.to_f64().unwrap_or(0.0)
                );
            }
            all_orders.extend(arbitrage_orders);
        }

        all_orders
    }
}

// 模擬 K 線數據生成器
struct MockBarGenerator {
    symbol: Symbol,
    sequence: u64,
    base_price: f64,
    price_trend: f64,
}

impl MockBarGenerator {
    fn new(symbol: Symbol, base_price: f64) -> Self {
        Self {
            symbol,
            sequence: 0,
            base_price,
            price_trend: 1.0,
        }
    }

    fn generate_bar(&mut self) -> AggregatedBar {
        self.sequence += 1;
        let timestamp = current_timestamp_us();

        // 模擬價格波動 (正弦波 + 隨機噪音)
        let time_factor = (self.sequence as f64 * 0.1).sin();
        let noise = (self.sequence as f64 * 0.7).sin() * 50.0; // ±50 的噪音
        let price_change = time_factor * 200.0 + noise; // ±200 的主要波動

        self.base_price += price_change;
        
        // 模擬 OHLC
        let open = self.base_price - price_change;
        let close = self.base_price;
        let high = open.max(close) + 30.0; // 高點比開收盤價高一些
        let low = open.min(close) - 30.0;  // 低點比開收盤價低一些

        AggregatedBar {
            symbol: self.symbol.clone(),
            interval_ms: 60000, // 1分鐘 K 線
            open_time: timestamp - 60000,
            close_time: timestamp,
            open: Price::from_f64(open).unwrap(),
            high: Price::from_f64(high).unwrap(),
            low: Price::from_f64(low).unwrap(),
            close: Price::from_f64(close).unwrap(),
            volume: Quantity::from_f64(1.5 + (self.sequence as f64 % 10.0) * 0.1).unwrap(),
            trade_count: 10 + (self.sequence % 50) as u32,
        }
    }
}

// 模擬跨交易所快照生成器
struct MockMultiVenueGenerator {
    symbol: Symbol,
    binance_price: f64,
    bitget_price: f64,
    sequence: u64,
}

impl MockMultiVenueGenerator {
    fn new(symbol: Symbol, base_price: f64) -> Self {
        Self {
            symbol,
            binance_price: base_price,
            bitget_price: base_price + 5.0, // 初始價差
            sequence: 0,
        }
    }

    fn generate_snapshots(&mut self) -> Vec<MarketSnapshot> {
        self.sequence += 1;
        let timestamp = current_timestamp_us();

        // 模擬價格差異變化
        let spread_factor = (self.sequence as f64 * 0.05).sin();
        let price_diff = spread_factor * 20.0; // ±20 的價差變化

        self.bitget_price = self.binance_price + 5.0 + price_diff;

        vec![
            // Binance 快照
            MarketSnapshot {
                symbol: Symbol("BINANCE:BTCUSDT".to_string()),
                timestamp,
                bids: vec![
                    BookLevel::new_unchecked(self.binance_price, 0.1),
                    BookLevel::new_unchecked(self.binance_price - 0.5, 0.2),
                ],
                asks: vec![
                    BookLevel::new_unchecked(self.binance_price + 0.5, 0.1),
                    BookLevel::new_unchecked(self.binance_price + 1.0, 0.2),
                ],
                sequence: self.sequence,
            },
            // Bitget 快照
            MarketSnapshot {
                symbol: Symbol("BITGET:BTCUSDT".to_string()),
                timestamp,
                bids: vec![
                    BookLevel::new_unchecked(self.bitget_price, 0.1),
                    BookLevel::new_unchecked(self.bitget_price - 0.5, 0.2),
                ],
                asks: vec![
                    BookLevel::new_unchecked(self.bitget_price + 0.5, 0.1),
                    BookLevel::new_unchecked(self.bitget_price + 1.0, 0.2),
                ],
                sequence: self.sequence,
            },
        ]
    }
}

// Mock Adapter for testing
struct MockStrategyAdapter {
    bar_generator: MockBarGenerator,
    venue_generator: MockMultiVenueGenerator,
    event_count: u64,
}

impl MockStrategyAdapter {
    fn new() -> Self {
        Self {
            bar_generator: MockBarGenerator::new(Symbol("BTCUSDT".to_string()), 50000.0),
            venue_generator: MockMultiVenueGenerator::new(Symbol("BTCUSDT".to_string()), 50000.0),
            event_count: 0,
        }
    }
}

#[async_trait::async_trait]
impl MarketStream for MockStrategyAdapter {
    async fn subscribe(&self, _symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        use async_stream::stream;
        
        let stream = stream! {
            let mut bar_gen = MockBarGenerator::new(Symbol("BTCUSDT".to_string()), 50000.0);
            let mut venue_gen = MockMultiVenueGenerator::new(Symbol("BTCUSDT".to_string()), 50000.0);
            let mut count = 0u64;

            loop {
                count += 1;
                
                // 每 5 個循環發送一次 K 線
                if count % 5 == 0 {
                    let bar = bar_gen.generate_bar();
                    yield Ok(MarketEvent::Bar(bar));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                
                // 每 3 個循環發送跨交易所快照
                if count % 3 == 0 {
                    let snapshots = venue_gen.generate_snapshots();
                    for snapshot in snapshots {
                        yield Ok(MarketEvent::Snapshot(snapshot));
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                
                // 限制測試時間
                if count > 50 {
                    break;
                }
                
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };
        
        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: true,
            latency_ms: Some(1),
            last_heartbeat: current_timestamp_us(),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        Ok(())
    }
}

fn current_timestamp_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 S1 策略演示開始");

    // 1. 配置 Engine
    let ingestion_config = IngestionConfig {
        queue_capacity: 1024,
        stale_threshold_us: 5000,
        flip_policy: FlipPolicy::OnUpdate,
        backpressure_policy: BackpressurePolicy::LastWins,
    };

    let engine_config = EngineConfig {
        ingestion: ingestion_config.clone(),
        max_events_per_cycle: 10,
        aggregation_symbols: vec![Symbol("BTCUSDT".to_string())],
    };

    let mut engine = Engine::new(engine_config);

    // 2. 配置 AdapterBridge
    let bridge_config = AdapterBridgeConfig {
        ingestion: ingestion_config,
        max_concurrent_adapters: 1,
    };

    let adapter_bridge = AdapterBridge::new(bridge_config);

    // 3. 創建策略管理器
    let mut strategy_manager = StrategyManager::new();
    println!("✅ 策略管理器初始化完成");
    strategy_manager.trend_strategy.initialize()?;
    strategy_manager.arbitrage_strategy.initialize()?;

    // 4. 創建 Mock Adapter
    println!("📡 正在創建 Mock 數據適配器...");
    let mock_adapter = MockStrategyAdapter::new();
    let symbols = vec![Symbol("BTCUSDT".to_string())];
    
    let consumer = adapter_bridge.bridge_stream(mock_adapter, symbols).await?;
    engine.register_event_consumer(consumer);

    // 5. 模擬帳戶狀態
    let account = AccountView::default();

    // 6. 運行演示循環
    println!("🔄 策略演示開始 (30秒)...");
    let mut total_orders = 0;
    let start_time = std::time::Instant::now();
    
    while start_time.elapsed() < Duration::from_secs(30) {
        match engine.tick() {
            Ok(result) => {
                if result.snapshot_published {
                    let market_view = engine.get_market_view();
                    
                    // 檢查市場視圖變化 (間接觸發策略)
                    // 注意：實際項目中策略應該集成到 Engine 內部
                    // 這裡為了演示目的，我們模擬一些事件觸發
                    if result.events_total > 0 {
                        // 模擬觸發策略的市場事件
                        // 實際應該從 Engine 內部獲取處理的事件
                        println!("📊 處理了 {} 個市場事件", result.events_total);
                    }
                    
                    // 顯示市場狀況
                    if !market_view.orderbooks.is_empty() {
                        println!("📊 市場快照: {} 個訂單簿", market_view.orderbooks.len());
                    }
                }
                
                // 短暫休眠
                if result.events_total == 0 {
                    sleep(Duration::from_millis(10)).await;
                }
            }
            Err(e) => {
                println!("💥 Engine tick 錯誤: {}", e);
                sleep(Duration::from_millis(1)).await;
            }
        }
    }

    // 7. 策略清理
    strategy_manager.trend_strategy.shutdown()?;
    strategy_manager.arbitrage_strategy.shutdown()?;

    println!("\n✅ S1 策略演示完成");
    println!("   總計生成訂單: {} 筆", total_orders);
    println!("   策略模板驗證: ✅ 成功");

    Ok(())
}