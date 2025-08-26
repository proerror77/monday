//! Binance 市場數據適配器測試
//!
//! 用法:
//! cargo run --example test_binance_adapter --features="binance"

#[cfg(feature = "binance")]
use data_adapter_binance::BinanceMarketStream;
#[cfg(feature = "binance")]
use hft_core::Symbol;
#[cfg(feature = "binance")]
use ports::MarketStream;
#[cfg(feature = "binance")]
use tokio::time::{timeout, Duration};
#[cfg(feature = "binance")]
use tracing::{info, error, warn, Level};
#[cfg(feature = "binance")]
use futures::StreamExt;

#[cfg(not(feature = "binance"))]
fn main() {
    eprintln!("This example requires --features=binance to compile.");
}

#[cfg(feature = "binance")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    info!("啟動 Binance 市場數據適配器測試");

    // 創建 Binance 適配器
    let mut binance_stream = BinanceMarketStream::new();

    // 測試連接
    info!("測試連接到 Binance...");
    match binance_stream.connect().await {
        Ok(_) => info!("✅ 連接 Binance 成功"),
        Err(e) => {
            error!("❌ 連接 Binance 失敗: {}", e);
            return Ok(());
        }
    }

    // 測試健康檢查
    let health = binance_stream.health().await;
    info!("連接健康狀態: connected={}, latency={:?}", 
        health.connected, health.latency_ms);

    // 測試市場數據訂閱
    let symbols = vec![
        Symbol("BTCUSDT".to_string()),
        Symbol("ETHUSDT".to_string()),
    ];

    info!("訂閱市場數據: {:?}", symbols);
    
    match binance_stream.subscribe(symbols).await {
        Ok(mut stream) => {
            info!("✅ 市場數據訂閱成功");
            
            // 設置 30 秒超時
            let timeout_duration = Duration::from_secs(30);
            let mut event_count = 0;
            const MAX_EVENTS: usize = 10;
            
            info!("開始接收市場數據 (最多 {} 個事件, 超時 {} 秒)...", MAX_EVENTS, timeout_duration.as_secs());
            
            while event_count < MAX_EVENTS {
                match timeout(timeout_duration, stream.next()).await {
                    Ok(Some(result)) => {
                        match result {
                            Ok(market_event) => {
                                event_count += 1;
                                match market_event {
                                    ports::MarketEvent::Snapshot(snapshot) => {
                                        info!("📈 快照事件: {} 序號={} 買檔={} 賣檔={}", 
                                            snapshot.symbol, snapshot.sequence,
                                            snapshot.bids.len(), snapshot.asks.len());
                                        
                                        // 顯示前3檔
                                        if !snapshot.bids.is_empty() {
                                            info!("   買一: {} @ {}", 
                                                snapshot.bids[0].price, snapshot.bids[0].quantity);
                                        }
                                        if !snapshot.asks.is_empty() {
                                            info!("   賣一: {} @ {}", 
                                                snapshot.asks[0].price, snapshot.asks[0].quantity);
                                        }
                                    }
                                    ports::MarketEvent::Update(update) => {
                                        info!("🔄 增量更新: {} 序號={} 買檔更新={} 賣檔更新={}",
                                            update.symbol, update.sequence,
                                            update.bids.len(), update.asks.len());
                                    }
                                    ports::MarketEvent::Trade(trade) => {
                                        info!("💰 交易事件: {} {} {} @ {} ID={}",
                                            trade.symbol, 
                                            if trade.side == hft_core::Side::Buy { "買入" } else { "賣出" },
                                            trade.quantity, trade.price, trade.trade_id);
                                    }
                                    ports::MarketEvent::Bar(bar) => {
                                        info!("📊 K線事件: {} 時間={}ms O={} H={} L={} C={} V={}",
                                            bar.symbol, bar.interval_ms,
                                            bar.open, bar.high, bar.low, bar.close, bar.volume);
                                    }
                                    ports::MarketEvent::Disconnect { reason } => {
                                        warn!("⚠️ 連接斷開: {}", reason);
                                        break;
                                    }
                                    _ => {
                                        info!("❓ 其他事件: {:?}", market_event);
                                    }
                                }
                            }
                            Err(e) => {
                                error!("❌ 接收市場事件失敗: {}", e);
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        warn!("流結束");
                        break;
                    }
                    Err(_) => {
                        warn!("⏰ 接收市場事件超時");
                        break;
                    }
                }
            }
            
            info!("收到 {} 個市場事件", event_count);
        }
        Err(e) => {
            error!("❌ 市場數據訂閱失敗: {}", e);
        }
    }

    // 斷開連接
    info!("斷開連接...");
    if let Err(e) = binance_stream.disconnect().await {
        error!("斷開連接失敗: {}", e);
    } else {
        info!("✅ 斷開連接成功");
    }

    info!("🎉 Binance 適配器測試完成");
    Ok(())
}
