/*!
 * S1 階段演示：Adapter→Engine 有界匯流對接
 *
 * 此示例演示了完整的 S1 數據流：
 * 1. Bitget Adapter 訂閱市場數據
 * 2. AdapterBridge 創建 SPSC ring buffer
 * 3. Engine 消費事件並更新 MarketView
 * 4. ArcSwap 發佈快照供外部讀取
 */

use anyhow::Result;
use clap::Parser;
use engine::{
    Engine, EngineConfig, AdapterBridge, AdapterBridgeConfig,
    FlipPolicy, BackpressurePolicy
};
use engine::dataflow::IngestionConfig;
use data_adapter_bitget::BitgetMarketStream;
use hft_core::{Symbol, HftError};
use ports::MarketStream;
use std::time::Duration;
use tokio::time::{sleep, interval};
use tracing::{info, warn, error, debug};

#[derive(Parser, Debug)]
#[command(about = "S1 階段演示：Adapter→Engine 有界匯流對接")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,
    
    /// Ring buffer 容量 (2的冪)
    #[arg(long, default_value = "32768")]
    queue_capacity: usize,
    
    /// 陳舊度閾值 (微秒)
    #[arg(long, default_value = "3000")]
    stale_threshold_us: u64,
    
    /// 運行時間 (秒)
    #[arg(long, default_value = "60")]
    runtime_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    
    info!(
        "🚀 S1 演示開始 - 交易對: {}, 隊列容量: {}, 運行時間: {}s",
        args.symbol, args.queue_capacity, args.runtime_secs
    );

    // 1. 配置 Engine
    let ingestion_config = IngestionConfig {
        queue_capacity: args.queue_capacity,
        stale_threshold_us: args.stale_threshold_us,
        flip_policy: FlipPolicy::OnUpdate,
        backpressure_policy: BackpressurePolicy::LastWins,
    };

    let engine_config = EngineConfig {
        ingestion: ingestion_config.clone(),
        max_events_per_cycle: 100,
        aggregation_symbols: vec![Symbol(args.symbol.clone())],
    };

    let mut engine = Engine::new(engine_config);

    // 2. 配置 AdapterBridge
    let bridge_config = AdapterBridgeConfig {
        ingestion: ingestion_config,
        max_concurrent_adapters: 4,
    };

    let adapter_bridge = AdapterBridge::new(bridge_config);

    // 3. 創建並連接 Bitget Adapter
    info!("📡 正在連接 Bitget WebSocket...");
    let bitget_stream = BitgetMarketStream::new();
    let symbols = vec![Symbol(args.symbol.clone())];
    
    let consumer = match adapter_bridge.bridge_stream(bitget_stream, symbols).await {
        Ok(consumer) => consumer,
        Err(e) => {
            error!("❌ Bitget 連接失敗: {}", e);
            return Err(anyhow::anyhow!("連接失敗: {}", e));
        }
    };

    info!("✅ Bitget 連接成功，EventConsumer 已創建");

    // 4. 註冊 Consumer 到 Engine
    engine.register_event_consumer(consumer);

    // 5. 啟動監控任務
    let engine_stats_task = {
        let runtime_secs = args.runtime_secs;
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(5));
            let mut tick_count = 0;
            
            loop {
                interval.tick().await;
                tick_count += 1;
                
                info!(
                    "📊 統計 [{}min]: 運行中...", 
                    tick_count * 5 / 60
                );
                
                if tick_count * 5 >= runtime_secs {
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
            
            info!("🔄 Engine 主循環開始");
            
            loop {
                match engine.tick() {
                    Ok(result) => {
                        total_events += result.events_total as u64;
                        
                        if result.snapshot_published {
                            total_snapshots += 1;
                            let market_view = engine.get_market_view();
                            debug!(
                                "📸 快照 #{}: {} 事件, 時間戳: {}",
                                total_snapshots,
                                result.events_total,
                                market_view.timestamp
                            );
                        }
                        
                        // 每處理 1000 個事件報告一次
                        if total_events % 1000 == 0 && total_events > 0 {
                            let stats = engine.get_statistics();
                            info!(
                                "🎯 處理進度: {} 事件, {} 快照, {} 消費者",
                                total_events, total_snapshots, stats.consumers_count
                            );
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
        _ = engine_stats_task => {
            info!("📊 監控任務完成");
        }
        _ = engine_task => {
            warn!("🔄 Engine 任務意外結束");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("🛑 收到中斷信號，正在停止...");
        }
    }

    info!("✅ S1 演示完成");
    Ok(())
}
// Archived legacy example; see 03_execution/
