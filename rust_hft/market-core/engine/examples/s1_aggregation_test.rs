/*!
 * S1 聚合邏輯測試：驗證 TopN/Bar 聚合功能
 *
 * 此示例測試：
 * 1. TopNSnapshot 更新與 SoA 結構
 * 2. BarBuilder K線聚合
 * 3. AggregationEngine 事件處理
 * 4. MarketView 快照生成
 */

use engine::aggregation::{AggregationEngine, BarBuilder, MarketView, TopNSnapshot};
use engine::dataflow::IngestionConfig;
use engine::{
    AdapterBridge, AdapterBridgeConfig, BackpressurePolicy, Engine, EngineConfig, FlipPolicy,
};
use hft_core::*;
use ports::*;
use std::time::Duration;
use tokio::time::sleep;

// 模擬市場數據生成器
struct MockMarketDataGenerator {
    symbol: Symbol,
    base_price: f64,
    sequence: u64,
}

impl MockMarketDataGenerator {
    fn new(symbol: Symbol, base_price: f64) -> Self {
        Self {
            symbol,
            base_price,
            sequence: 0,
        }
    }

    fn generate_snapshot(&mut self) -> MarketSnapshot {
        self.sequence += 1;
        let timestamp = current_timestamp_us();

        // 生成 5 檔深度數據
        let mut bids = Vec::new();
        let mut asks = Vec::new();

        for i in 0..5 {
            let bid_price = self.base_price - (i as f64 * 0.1);
            let ask_price = self.base_price + (i as f64 * 0.1);
            let quantity = 1.0 + i as f64 * 0.5;

            bids.push(BookLevel {
                price: Price::from_f64(bid_price).unwrap(),
                quantity: Quantity::from_f64(quantity).unwrap(),
            });

            asks.push(BookLevel {
                price: Price::from_f64(ask_price).unwrap(),
                quantity: Quantity::from_f64(quantity).unwrap(),
            });
        }

        MarketSnapshot {
            symbol: self.symbol.clone(),
            timestamp,
            bids,
            asks,
            sequence: self.sequence,
        }
    }

    fn generate_trade(&mut self, side: Side) -> Trade {
        self.sequence += 1;
        let timestamp = current_timestamp_us();

        // 在中間價附近生成成交
        let price_offset = match side {
            Side::Buy => 0.05,
            Side::Sell => -0.05,
        };

        Trade {
            symbol: self.symbol.clone(),
            timestamp,
            price: Price::from_f64(self.base_price + price_offset).unwrap(),
            quantity: Quantity::from_f64(0.1).unwrap(),
            side,
            trade_id: format!("T{}", self.sequence),
        }
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
    println!("🧪 S1 聚合邏輯測試開始");

    // 1. 測試 TopNSnapshot 更新
    println!("\n📊 測試 TopNSnapshot 更新...");
    let mut generator = MockMarketDataGenerator::new(Symbol::new("BTCUSDT"), 50000.0);
    let mut topn = TopNSnapshot::new(Symbol::new("BTCUSDT"), 5);

    let snapshot = generator.generate_snapshot();
    topn.update_from_snapshot(&snapshot);

    println!("✅ TopN 快照更新成功:");
    println!(
        "   - 買方 5 檔: {:?}",
        topn.bid_prices
            .iter()
            .map(|p| p.0.to_f64().unwrap_or(0.0))
            .collect::<Vec<_>>()
    );
    println!(
        "   - 賣方 5 檔: {:?}",
        topn.ask_prices
            .iter()
            .map(|p| p.0.to_f64().unwrap_or(0.0))
            .collect::<Vec<_>>()
    );
    println!(
        "   - 中間價: {:?}",
        topn.get_mid_price().map(|p| p.0.to_f64().unwrap_or(0.0))
    );
    println!(
        "   - 價差: {:?} bps",
        topn.get_spread_bps().map(|b| b.0.to_f64().unwrap_or(0.0))
    );

    // 2. 測試 BarBuilder K線聚合
    println!("\n📈 測試 BarBuilder K線聚合...");
    let mut bar_builder = BarBuilder::new(Symbol::new("BTCUSDT"), 60000, current_timestamp_us());

    // 添加幾筆成交
    for i in 0..5 {
        let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
        let trade = generator.generate_trade(side);
        bar_builder.add_trade(&trade);
        println!(
            "   添加成交: 價格 {:.2}, 數量 {:.3}, 方向 {:?}",
            trade.price.0.to_f64().unwrap_or(0.0),
            trade.quantity.0.to_f64().unwrap_or(0.0),
            trade.side
        );
    }

    if let Some(bar) = bar_builder.build() {
        println!("✅ K線聚合成功:");
        println!(
            "   - OHLC: {:.2}/{:.2}/{:.2}/{:.2}",
            bar.open.0.to_f64().unwrap_or(0.0),
            bar.high.0.to_f64().unwrap_or(0.0),
            bar.low.0.to_f64().unwrap_or(0.0),
            bar.close.0.to_f64().unwrap_or(0.0)
        );
        println!(
            "   - 成交量: {:.3}, 成交筆數: {}",
            bar.volume.0.to_f64().unwrap_or(0.0),
            bar.trade_count
        );
    }

    // 3. 測試 AggregationEngine 完整流程
    println!("\n🔄 測試 AggregationEngine 完整流程...");
    let mut agg_engine = AggregationEngine::new();

    // 處理快照事件
    let snapshot_event = MarketEvent::Snapshot(generator.generate_snapshot());
    let snapshot_events = agg_engine.handle_event(snapshot_event)?;
    println!(
        "   處理快照事件，觸發快照發佈: {}",
        !snapshot_events.is_empty()
    );

    // 處理成交事件
    let trade_event = MarketEvent::Trade(generator.generate_trade(Side::Buy));
    let trade_events = agg_engine.handle_event(trade_event)?;
    println!(
        "   處理成交事件，觸發快照發佈: {}",
        !trade_events.is_empty()
    );

    // 4. 測試 MarketView 快照生成
    println!("\n📸 測試 MarketView 快照生成...");
    let market_view = agg_engine.build_market_view();

    println!("✅ MarketView 快照生成成功:");
    println!("   - 訂單簿數量: {}", market_view.orderbooks.len());
    println!(
        "   - 套利機會數量: {}",
        market_view.arbitrage_opportunities.len()
    );
    println!("   - 時間戳: {}", market_view.timestamp);

    // 測試 MarketView 查詢方法
    let symbol = Symbol::new("BTCUSDT");
    if let Some((bid_price, bid_qty)) = market_view.get_best_bid_any(&symbol) {
        println!(
            "   - 最佳買價: {:.2} @ {:.3}",
            bid_price.0.to_f64().unwrap_or(0.0),
            bid_qty.0.to_f64().unwrap_or(0.0)
        );
    }

    if let Some((ask_price, ask_qty)) = market_view.get_best_ask_any(&symbol) {
        println!(
            "   - 最佳賣價: {:.2} @ {:.3}",
            ask_price.0.to_f64().unwrap_or(0.0),
            ask_qty.0.to_f64().unwrap_or(0.0)
        );
    }

    if let Some(mid_price) = market_view.get_mid_price_any(&symbol) {
        println!("   - 中間價: {:.2}", mid_price.0.to_f64().unwrap_or(0.0));
    }

    if let Some(spread_bps) = market_view.get_spread_bps_for_venue(&hft_core::VenueSymbol::new(
        hft_core::VenueId::MOCK,
        symbol.clone(),
    )) {
        println!("   - 價差: {:.2} bps", spread_bps.0.to_f64().unwrap_or(0.0));
    }

    // 5. 測試端到端集成
    println!("\n🔗 測試端到端集成...");

    let ingestion_config = IngestionConfig {
        queue_capacity: 1024,
        stale_threshold_us: 5000,
        flip_policy: FlipPolicy::OnUpdate,
        backpressure_policy: BackpressurePolicy::LastWins,
    };

    let engine_config = EngineConfig {
        ingestion: ingestion_config.clone(),
        max_events_per_cycle: 10,
        aggregation_symbols: vec![Symbol::new("BTCUSDT")],
    };

    let mut engine = Engine::new(engine_config);

    // 創建 MockAdapter 來注入測試數據
    let mock_adapter = MockEventAdapter::new();
    let bridge_config = AdapterBridgeConfig {
        ingestion: ingestion_config,
        max_concurrent_adapters: 1,
    };
    let adapter_bridge = AdapterBridge::new(bridge_config);

    // 橋接並運行幾個 tick
    let consumer = adapter_bridge
        .bridge_stream(mock_adapter, vec![Symbol::new("BTCUSDT")])
        .await?;
    engine.register_event_consumer(consumer);

    for i in 0..5 {
        let result = engine.tick()?;
        if result.snapshot_published {
            let view = engine.get_market_view();
            println!(
                "   Tick #{}: {} 事件, {} 訂單簿",
                i + 1,
                result.events_total,
                view.orderbooks.len()
            );
        }
        sleep(Duration::from_millis(10)).await;
    }

    println!("\n✅ S1 聚合邏輯測試完成");
    Ok(())
}

// 簡單的 Mock Adapter 用於測試
struct MockEventAdapter;

impl MockEventAdapter {
    fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl MarketStream for MockEventAdapter {
    async fn subscribe(&self, _symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        use async_stream::stream;

        let stream = stream! {
            // 空流，用於測試架構
            yield Ok(MarketEvent::Disconnect { reason: "Mock adapter test".to_string() });
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
