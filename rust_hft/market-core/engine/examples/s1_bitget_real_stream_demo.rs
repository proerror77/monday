/*!
 * S1 階段演示：Bitget 真實數據流 → Engine 有界匯流對接
 *
 * 此示例演示了完整的 S1 數據流：
 * 1. Bitget Adapter 訂閱真實市場數據 (books15 + trade)
 * 2. AdapterBridge 創建 SPSC ring buffer 處理背壓
 * 3. Engine 消費事件並更新 MarketView
 * 4. ArcSwap 發佈快照供外部讀取
 * 5. 實時統計與監控
 */

use data_adapter_bitget::BitgetMarketStream;
use engine::dataflow::IngestionConfig;
use engine::{
    AdapterBridge, AdapterBridgeConfig, BackpressurePolicy, Engine, EngineConfig, FlipPolicy,
};
use hft_core::{HftError, Symbol};
use num_traits::ToPrimitive;
use std::time::Duration;
use tokio::time::{interval, sleep};
use tracing::{debug, error, info, warn};

// 簡化配置結構
struct DemoConfig {
    symbol: String,
    queue_capacity: usize,
    stale_threshold_us: u64,
    runtime_secs: u64,
}

impl Default for DemoConfig {
    fn default() -> Self {
        Self {
            symbol: "BTCUSDT".to_string(),
            queue_capacity: 32768,
            stale_threshold_us: 3000,
            runtime_secs: 30,
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DemoConfig::default();

    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .ok(); // 忽略重複初始化錯誤

    info!(
        "🚀 S1 真實數據流演示開始\n   交易對: {}\n   隊列容量: {}\n   陳舊度閾值: {}μs\n   運行時間: {}s",
        config.symbol, config.queue_capacity, config.stale_threshold_us, config.runtime_secs
    );

    // 1. 配置 Engine
    let ingestion_config = IngestionConfig {
        queue_capacity: config.queue_capacity,
        stale_threshold_us: config.stale_threshold_us,
        flip_policy: FlipPolicy::OnUpdate,
        backpressure_policy: BackpressurePolicy::LastWins,
    };

    let engine_config = EngineConfig {
        ingestion: ingestion_config.clone(),
        max_events_per_cycle: 100,
        aggregation_symbols: vec![Symbol(config.symbol.clone())],
    };

    let mut engine = Engine::new(engine_config);

    // 2. 配置 AdapterBridge
    let bridge_config = AdapterBridgeConfig {
        ingestion: ingestion_config,
        max_concurrent_adapters: 4,
    };

    let adapter_bridge = AdapterBridge::new(bridge_config);

    // 3. 創建並連接 Bitget Adapter
    info!("📡 正在初始化 Bitget WebSocket 適配器...");
    let bitget_stream = BitgetMarketStream::new();
    let symbols = vec![Symbol(config.symbol.clone())];

    let consumer = match adapter_bridge.bridge_stream(bitget_stream, symbols).await {
        Ok(consumer) => {
            info!("✅ Bitget 適配器橋接成功，EventConsumer 已創建");
            consumer
        }
        Err(e) => {
            error!("❌ Bitget 適配器橋接失敗: {}", e);
            return Err(anyhow::anyhow!("橋接失敗: {}", e));
        }
    };

    // 4. 註冊 Consumer 到 Engine
    engine.register_event_consumer(consumer);

    // 5. 啟動統計監控任務
    let stats_task = {
        let runtime_secs = config.runtime_secs;
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(10));
            let mut elapsed_secs = 0;

            loop {
                interval.tick().await;
                elapsed_secs += 10;

                info!(
                    "📊 運行狀態 [{}/{}s]: WebSocket 連接中，等待數據...",
                    elapsed_secs, runtime_secs
                );

                if elapsed_secs >= runtime_secs {
                    info!("⏰ 達到預設運行時間，準備停止");
                    break;
                }
            }
        })
    };

    // 6. 運行 Engine 主循環
    let engine_task = {
        tokio::spawn(async move {
            let mut total_events = 0u64;
            let mut total_snapshots = 0u64;
            let mut total_ticks = 0u64;
            let mut last_report_events = 0u64;
            let mut last_report_time = tokio::time::Instant::now();

            info!("🔄 Engine 主循環開始，等待事件...");

            loop {
                total_ticks += 1;

                match engine.tick() {
                    Ok(result) => {
                        total_events += result.events_total as u64;

                        if result.snapshot_published {
                            total_snapshots += 1;
                            let market_view = engine.get_market_view();

                            // 檢查 MarketView 是否有數據
                            let orderbooks_count = market_view.orderbooks.len();
                            if orderbooks_count > 0 {
                                info!(
                                    "📸 快照 #{}: {} 新事件, {} 訂單簿, 時間戳: {}",
                                    total_snapshots,
                                    result.events_total,
                                    orderbooks_count,
                                    market_view.timestamp
                                );

                                // 顯示訂單簿摘要
                                for (vs, _ob) in &market_view.orderbooks {
                                    if let (Some(best_bid), Some(best_ask)) = (
                                        market_view.get_best_bid_for_venue(vs),
                                        market_view.get_best_ask_for_venue(vs),
                                    ) {
                                        if let Some(spread_bps) =
                                            market_view.get_spread_bps_for_venue(vs)
                                        {
                                            info!(
                                                "   {} - 最佳買: {:.4}, 最佳賣: {:.4}, 價差: {:.2}bps",
                                                vs.symbol.0,
                                                best_bid.0.to_f64().unwrap_or(0.0),
                                                best_ask.0.to_f64().unwrap_or(0.0),
                                                spread_bps.0.to_f64().unwrap_or(0.0)
                                            );
                                        }
                                    }
                                }
                            } else {
                                debug!(
                                    "📸 快照 #{}: {} 事件但無訂單簿數據",
                                    total_snapshots, result.events_total
                                );
                            }
                        }

                        // 每 5 秒報告一次吞吐統計
                        let now = tokio::time::Instant::now();
                        if now.duration_since(last_report_time) >= Duration::from_secs(5) {
                            let events_delta = total_events - last_report_events;
                            let duration_secs = now.duration_since(last_report_time).as_secs_f64();
                            let events_per_sec = events_delta as f64 / duration_secs;

                            if events_delta > 0 {
                                let stats = engine.get_statistics();
                                info!(
                                    "🎯 吞吐統計: {:.1} 事件/秒, 總計 {} 事件, {} 快照, {} 消費者",
                                    events_per_sec,
                                    total_events,
                                    total_snapshots,
                                    stats.consumers_count
                                );
                            } else {
                                info!(
                                    "💤 無事件 (Tick #{})，可能 WebSocket 仍在連接或無市場活動",
                                    total_ticks
                                );
                            }

                            last_report_events = total_events;
                            last_report_time = now;
                        }

                        // 如果沒有事件，短暫休眠避免 CPU 佔用
                        if result.events_total == 0 {
                            sleep(Duration::from_micros(100)).await;
                        }
                    }
                    Err(e) => {
                        error!("💥 Engine tick 錯誤: {}", e);
                        sleep(Duration::from_millis(1)).await;
                    }
                }
            }
        })
    };

    // 7. 等待運行完成
    tokio::select! {
        _ = stats_task => {
            info!("📊 統計任務完成");
        }
        _ = engine_task => {
            warn!("🔄 Engine 任務意外結束");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("🛑 收到中斷信號，正在停止...");
        }
    }

    info!("✅ S1 真實數據流演示完成");
    Ok(())
}
