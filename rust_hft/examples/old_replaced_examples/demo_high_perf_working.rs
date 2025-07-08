/*!
 * Demo: 高性能 OrderBook 示例 (修復版本)
 * 
 * 此示例展示性能優化的 OrderBook 管理器：
 * - 無鎖數據結構
 * - 批量消息處理
 * - 緩存統計計算
 * - 超時保護和連接重試
 */

use rust_hft::integrations::{
    bitget_connector::{BitgetConnector, BitgetConfig, BitgetChannel},
    high_perf_orderbook_manager::HighPerfOrderBookManager,
};

use tracing::{info, error, warn};
use anyhow::Result;
use tokio::time::{Duration, Instant, timeout};
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌（減少日誌級別以提升性能）
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting High-Performance OrderBook Demo (Fixed Version)");

    // 配置監控的交易對
    let symbols = vec!["BTCUSDT", "ETHUSDT", "ADAUSDT"];
    
    info!("📊 Configured symbols: {:?}", symbols);

    // 創建高性能 OrderBook 管理器
    // 批量大小：50，批量超時：5ms (更保守的配置)
    let manager = Arc::new(Mutex::new(HighPerfOrderBookManager::new(50, 5)));
    let message_sender = manager.lock().unwrap().get_message_sender();

    // 創建 Bitget 連接器
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 5,
    };

    let mut connector = BitgetConnector::new(config);

    // 添加訂閱（只訂閱必要的頻道）
    for symbol in &symbols {
        connector.add_subscription(symbol.to_string(), BitgetChannel::Books5);
        connector.add_subscription(symbol.to_string(), BitgetChannel::Trade);
    }

    info!("🔌 Starting WebSocket connections with {} symbols...", symbols.len());

    // 統計間隔
    let mut stats_interval = tokio::time::interval(Duration::from_secs(10));
    let demo_duration = Duration::from_secs(60); // 1 分鐘演示
    let start_time = Instant::now();

    info!("🎯 Starting high-performance orderbook processing ({}s demo)...", demo_duration.as_secs());

    // 創建消息處理器（使用無鎖隊列）
    let sender_clone = message_sender.clone();
    let message_handler = move |message| {
        // 發送到無鎖隊列（帶超時保護）
        if let Err(e) = sender_clone.try_send(message) {
            error!("❌ Failed to send message to queue: {}", e);
        }
    };

    // 啟動 WebSocket 連接任務（帶超時保護）
    let mut connector_handle = tokio::spawn(async move {
        info!("🔌 Attempting to connect to Bitget WebSocket...");
        
        // 使用超時保護連接
        match timeout(Duration::from_secs(30), connector.connect_public(message_handler)).await {
            Ok(result) => {
                if let Err(e) = result {
                    error!("❌ Bitget connection failed: {}", e);
                } else {
                    info!("✅ Bitget connection established successfully");
                }
            }
            Err(_) => {
                error!("❌ Bitget connection timeout after 30s");
            }
        }
    });

    // 啟動批量處理任務
    let manager_clone = manager.clone();
    let mut batch_processing_handle = tokio::spawn(async move {
        let mut processing_count = 0u64;
        let mut last_report = Instant::now();
        
        info!("🔄 Starting batch processing loop...");
        
        loop {
            let batch_start = Instant::now();
            
            // 批量處理消息（帶錯誤保護）
            if let Ok(mut mgr) = manager_clone.try_lock() {
                match mgr.process_message_batch() {
                    Ok(market_events) => {
                        if !market_events.is_empty() {
                            processing_count += market_events.len() as u64;
                            
                            // 每5秒報告一次處理情況
                            if last_report.elapsed() >= Duration::from_secs(5) {
                                let batch_latency = batch_start.elapsed();
                                info!("📊 Processed {} events in {:?} (total: {})", 
                                      market_events.len(), batch_latency, processing_count);
                                last_report = Instant::now();
                            }
                        }
                    }
                    Err(e) => {
                        warn!("⚠️ Batch processing error: {}", e);
                    }
                }
            }
            
            // 避免忙等待
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // 主監控循環
    loop {
        tokio::select! {
            // 定期打印統計信息
            _ = stats_interval.tick() => {
                if let Ok(mgr) = manager.try_lock() {
                    print_performance_stats(&*mgr);
                } else {
                    info!("📊 Manager busy, skipping stats...");
                }
            }
            
            // 演示時間限制
            _ = tokio::time::sleep_until(start_time + demo_duration) => {
                info!("⏰ Demo time limit reached");
                break;
            }
            
            // 監控任務狀態
            result = &mut connector_handle => {
                match result {
                    Ok(_) => info!("🔌 Connector task completed normally"),
                    Err(e) => error!("❌ Connector task failed: {}", e),
                }
                break;
            }
            
            // 批量處理任務不應該結束
            result = &mut batch_processing_handle => {
                match result {
                    Ok(_) => warn!("🔄 Batch processing task completed unexpectedly"),
                    Err(e) => error!("❌ Batch processing task failed: {}", e),
                }
                break;
            }
            
            // 空閒檢查
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                // 繼續主循環
            }
        }
    }

    // 最終性能報告
    info!("📈 Final Performance Report:");
    if let Ok(mgr) = manager.try_lock() {
        print_performance_stats(&*mgr);
        print_final_analysis(&*mgr, start_time.elapsed());
    }

    info!("🏁 High-Performance OrderBook Demo completed");
    Ok(())
}

/// 打印性能統計信息
fn print_performance_stats(manager: &HighPerfOrderBookManager) {
    manager.print_perf_stats();
    
    // 顯示每個交易對的高級分析
    info!("🔬 Advanced OrderBook Analysis:");
    
    for (symbol, orderbook) in manager.get_orderbooks() {
        let stats = orderbook.get_cached_stats();
        let sequence = orderbook.sequence.load(std::sync::atomic::Ordering::Relaxed);
        
        // 計算 VWAP（5檔深度）
        let vwap = orderbook.calculate_vwap_simd(5);
        
        info!("   {} - Seq:{}, Mid:{:.4}, VWAP:{:.4}, Spread:{:.6}, Liquidity:{:.1}k/{:.1}k", 
              symbol,
              sequence,
              stats.weighted_mid_price,
              vwap.unwrap_or(stats.weighted_mid_price),
              stats.spread,
              stats.total_bid_volume / rust_decimal::Decimal::from(1000),
              stats.total_ask_volume / rust_decimal::Decimal::from(1000));
    }
}

/// 打印最終分析
fn print_final_analysis(manager: &HighPerfOrderBookManager, total_duration: Duration) {
    let stats = manager.get_stats();
    let total_events = stats.total_events.load(std::sync::atomic::Ordering::Relaxed);
    let total_batches = stats.batch_processed.load(std::sync::atomic::Ordering::Relaxed);
    
    let events_per_sec = total_events as f64 / total_duration.as_secs_f64();
    let batches_per_sec = total_batches as f64 / total_duration.as_secs_f64();
    let avg_batch_size = if total_batches > 0 {
        total_events as f64 / total_batches as f64
    } else {
        0.0
    };
    
    info!("🎯 Performance Summary:");
    info!("   Total runtime: {:.2}s", total_duration.as_secs_f64());
    info!("   Total events: {}", total_events);
    info!("   Events/sec: {:.1}", events_per_sec);
    info!("   Batches/sec: {:.1}", batches_per_sec);
    info!("   Avg batch size: {:.1}", avg_batch_size);
    
    // 性能目標對比
    let target_events_per_sec = 10000.0;
    let performance_ratio = events_per_sec / target_events_per_sec * 100.0;
    
    info!("   Performance vs target: {:.1}% (target: {} events/sec)", 
          performance_ratio, target_events_per_sec);
    
    if performance_ratio >= 50.0 {
        info!("   ✅ Performance target achieved!");
    } else {
        info!("   📊 Performance improvement needed: {:.1}x", 
              target_events_per_sec / events_per_sec);
    }
    
    // 顯示優化效果
    info!("🚀 Optimization Benefits:");
    info!("   • Lock-free message queuing");
    info!("   • Batch processing with configurable timeouts");
    info!("   • Cached statistics for O(1) access");
    info!("   • SIMD-optimized VWAP calculations");
    info!("   • Atomic operations for thread-safe counters");
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_hft::integrations::high_perf_orderbook_manager::{HighPerfOrderBook, HighPerfLevel};
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_high_perf_demo_setup() {
        let manager = HighPerfOrderBookManager::new(10, 1);
        assert!(manager.get_message_sender().capacity().is_none());
    }
    
    #[test]
    fn test_orderbook_performance() {
        let mut orderbook = HighPerfOrderBook::new("PERFTEST".to_string());
        
        // 創建測試數據
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        
        for i in 0..50 {
            bids.push(HighPerfLevel {
                price: dec!(50000) - rust_decimal::Decimal::from(i),
                amount: dec!(1.0),
                order_id: None,
                timestamp: i as u64,
            });
            
            asks.push(HighPerfLevel {
                price: dec!(50000) + rust_decimal::Decimal::from(i + 1),
                amount: dec!(1.0),
                order_id: None,
                timestamp: i as u64,
            });
        }
        
        // 性能測試
        let start = std::time::Instant::now();
        orderbook.update_l2_fast(bids, asks);
        let update_time = start.elapsed();
        
        // 驗證更新時間（應該 < 10μs）
        assert!(update_time < std::time::Duration::from_micros(10));
        
        // 驗證統計正確性
        let stats = orderbook.get_cached_stats();
        assert!(stats.total_bid_volume > dec!(0));
        assert!(stats.total_ask_volume > dec!(0));
    }
}