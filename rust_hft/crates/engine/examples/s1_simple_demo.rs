/*!
 * S1 階段簡化演示：Adapter→Engine 基本數據流
 *
 * 此示例驗證核心架構組件：
 * 1. Bitget Adapter 連接 (即使沒有實際數據)
 * 2. AdapterBridge 創建 SPSC ring buffer
 * 3. Engine 主循環運行
 * 4. MarketView 快照機制
 */

use engine::{
    Engine, EngineConfig, AdapterBridge, AdapterBridgeConfig,
    FlipPolicy, BackpressurePolicy
};
use engine::dataflow::IngestionConfig;
use data_adapter_bitget::BitgetMarketStream;
use hft_core::Symbol;
use std::time::Duration;
use tokio::time::sleep;
// 使用 println 而不是 tracing

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 S1 簡化演示開始");

    // 1. 配置 Engine
    let ingestion_config = IngestionConfig {
        queue_capacity: 16384,
        stale_threshold_us: 3000,
        flip_policy: FlipPolicy::OnUpdate,
        backpressure_policy: BackpressurePolicy::LastWins,
    };

    let engine_config = EngineConfig {
        ingestion: ingestion_config.clone(),
        max_events_per_cycle: 50,
        aggregation_symbols: vec![Symbol("BTCUSDT".to_string())],
    };

    let mut engine = Engine::new(engine_config);

    // 2. 配置 AdapterBridge
    let bridge_config = AdapterBridgeConfig {
        ingestion: ingestion_config,
        max_concurrent_adapters: 2,
    };

    let adapter_bridge = AdapterBridge::new(bridge_config);

    // 3. 創建 Bitget Adapter
    println!("📡 正在創建 Bitget 適配器...");
    let bitget_stream = BitgetMarketStream::new();
    let symbols = vec![Symbol("BTCUSDT".to_string())];
    
    let consumer = match adapter_bridge.bridge_stream(bitget_stream, symbols).await {
        Ok(consumer) => {
            println!("✅ Bitget 適配器橋接成功");
            consumer
        },
        Err(e) => {
            println!("⚠️ Bitget 適配器橋接失敗: {}", e);
            return Ok(()); // 繼續運行以測試其他組件
        }
    };

    // 4. 註冊 Consumer 到 Engine
    engine.register_event_consumer(consumer);

    // 5. 運行 Engine 主循環 (10 秒)
    println!("🔄 Engine 主循環開始 (10秒演示)");
    let mut total_ticks = 0;
    let mut total_events = 0;
    let start_time = std::time::Instant::now();
    
    while start_time.elapsed() < Duration::from_secs(10) {
        total_ticks += 1;
        
        match engine.tick() {
            Ok(result) => {
                total_events += result.events_total;
                
                if result.snapshot_published {
                    let market_view = engine.get_market_view();
                    println!(
                        "📸 快照發佈: {} 事件, {} 訂單簿, 時間戳: {}",
                        result.events_total,
                        market_view.orderbooks.len(),
                        market_view.timestamp
                    );
                }
                
                // 每 1000 tick 報告一次
                if total_ticks % 1000 == 0 {
                    let stats = engine.get_statistics();
                    println!(
                        "💡 Tick #{}: {} 事件總計, {} 消費者",
                        total_ticks, total_events, stats.consumers_count
                    );
                }
                
                // 短暫休眠避免 CPU 佔用
                if result.events_total == 0 {
                    sleep(Duration::from_micros(100)).await;
                }
            }
            Err(e) => {
                println!("💥 Engine tick 錯誤: {}", e);
                sleep(Duration::from_millis(1)).await;
            }
        }
    }

    let stats = engine.get_statistics();
    println!(
        "✅ 演示完成 - 總 Tick: {}, 總事件: {}, 消費者: {}",
        total_ticks, total_events, stats.consumers_count
    );

    Ok(())
}