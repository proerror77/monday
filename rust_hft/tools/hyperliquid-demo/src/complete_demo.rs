//! Hyperliquid 完整集成示例
//!
//! 展示如何同時使用：
//! - 數據適配器（Market Data Stream）
//! - 執行適配器（Order Management）
//! - 配置管理和錯誤處理
//!
//! 運行方式：
//! ```bash
//! # Paper 模式測試
//! cargo run --example hyperliquid_complete_demo -- --mode paper
//!
//! # Live 模式（需要私鑰）
//! HYPERLIQUID_PRIVATE_KEY=your_key cargo run --example hyperliquid_complete_demo -- --mode live
//! ```

use clap::{Parser, ValueEnum};
use futures::StreamExt;
use std::env;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce};
use ports::{ExecutionClient, MarketEvent, MarketStream, OrderIntent};

// 數據適配器
use adapter_hyperliquid_data::{HyperliquidMarketConfig, HyperliquidMarketStream};

// 執行適配器
use adapter_hyperliquid_execution::{
    ExecutionMode, HyperliquidExecutionClient, HyperliquidExecutionConfig,
};

#[derive(Debug, Clone, ValueEnum)]
enum TradingMode {
    Paper,
    Live,
}

#[derive(Parser)]
#[command(author, version, about = "Hyperliquid 完整集成演示")]
struct Args {
    /// 交易模式
    #[arg(short, long, value_enum, default_value = "paper")]
    mode: TradingMode,

    /// 演示持續時間（秒）
    #[arg(short, long, default_value = "30")]
    duration: u64,

    /// 訂閱的交易對
    #[arg(short, long, value_delimiter = ',', default_values = &["BTC-PERP", "ETH-PERP"])]
    symbols: Vec<String>,

    /// 測試下單（僅 Paper 模式）
    #[arg(long)]
    test_orders: bool,
}

/// 簡單的策略 - 在收到交易數據時記錄信息
struct SimpleLoggingStrategy {
    last_trade_prices: std::collections::HashMap<Symbol, Price>,
}

impl SimpleLoggingStrategy {
    fn new() -> Self {
        Self {
            last_trade_prices: std::collections::HashMap::new(),
        }
    }

    fn on_market_event(&mut self, event: &MarketEvent) {
        match event {
            MarketEvent::Trade(trade) => {
                let prev_price = self.last_trade_prices.get(&trade.symbol);
                let direction = if let Some(prev) = prev_price {
                    if trade.price > *prev {
                        "📈"
                    } else if trade.price < *prev {
                        "📉"
                    } else {
                        "➡️"
                    }
                } else {
                    "🔥"
                };

                info!(
                    "{} {} Trade: {} @ {} (Size: {}, Side: {:?})",
                    direction,
                    trade.symbol.0,
                    trade.trade_id,
                    trade.price,
                    trade.quantity,
                    trade.side
                );

                self.last_trade_prices
                    .insert(trade.symbol.clone(), trade.price);
            }
            MarketEvent::Snapshot(snapshot) => {
                let best_bid = snapshot.bids.first();
                let best_ask = snapshot.asks.first();

                if let (Some(bid), Some(ask)) = (best_bid, best_ask) {
                    debug!(
                        "📊 {} OrderBook: Bid {} @ {} | Ask {} @ {}",
                        snapshot.symbol.0, bid.price, bid.quantity, ask.price, ask.quantity
                    );
                }
            }
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("info,adapter_hyperliquid_data=debug,adapter_hyperliquid_execution=debug")
        .init();

    let args = Args::parse();

    info!("🚀 Hyperliquid 完整集成演示開始");
    info!("模式: {:?}, 持續時間: {}秒", args.mode, args.duration);
    info!("交易對: {:?}", args.symbols);

    // 1. 設置數據流
    let symbols: Vec<Symbol> = args.symbols.iter().map(|s| Symbol(s.clone())).collect();

    let mut market_config = HyperliquidMarketConfig::default();
    market_config.symbols = symbols.clone();

    let market_stream = HyperliquidMarketStream::new(market_config);

    info!("📡 數據適配器已創建");

    // 2. 設置執行客戶端
    let execution_config = match args.mode {
        TradingMode::Paper => {
            info!("📝 使用 Paper 模式 - 安全的模擬交易");
            HyperliquidExecutionConfig {
                mode: ExecutionMode::Paper,
                ..Default::default()
            }
        }
        TradingMode::Live => {
            let private_key = env::var("HYPERLIQUID_PRIVATE_KEY")
                .map_err(|_| "Live 模式需要設置 HYPERLIQUID_PRIVATE_KEY 環境變量")?;

            warn!("⚠️  使用 Live 模式 - 將執行真實交易！");
            HyperliquidExecutionConfig {
                mode: ExecutionMode::Live,
                private_key,
                ..Default::default()
            }
        }
    };

    let mut execution_client = HyperliquidExecutionClient::new(execution_config);

    // 連接執行客戶端
    execution_client.connect().await?;
    info!("💼 執行適配器已連接");

    // 3. 測試下單功能（僅在啟用時）
    if args.test_orders && matches!(args.mode, TradingMode::Paper) {
        info!("🧪 測試下單功能");

        let test_order = OrderIntent {
            symbol: Symbol("BTC-PERP".to_string()),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001)?,
            price: Some(Price::from_f64(40000.0)?), // 遠離市價的限價單
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            strategy_id: "demo_strategy".to_string(),
            target_venue: None,
        };

        match execution_client.place_order(test_order).await {
            Ok(order_id) => {
                info!("✅ 測試訂單已提交: {:?}", order_id);

                // 等待一下然後取消
                sleep(Duration::from_secs(2)).await;

                match execution_client.cancel_order(&order_id).await {
                    Ok(_) => info!("❌ 測試訂單已取消"),
                    Err(e) => warn!("取消訂單失敗: {}", e),
                }
            }
            Err(e) => error!("下單失敗: {}", e),
        }
    }

    // 4. 啟動數據流並處理事件
    info!("📈 啟動市場數據流");

    let mut strategy = SimpleLoggingStrategy::new();
    let mut data_stream = market_stream.subscribe(symbols).await?;

    let start_time = std::time::Instant::now();
    let duration_secs = Duration::from_secs(args.duration);

    info!("⏱️  開始接收市場數據 ({} 秒)...", args.duration);

    let mut event_count = 0;
    let mut trade_count = 0;
    let mut snapshot_count = 0;

    while start_time.elapsed() < duration_secs {
        tokio::select! {
            // 處理市場數據
            event_result = data_stream.next() => {
                match event_result {
                    Some(Ok(event)) => {
                        event_count += 1;

                        match &event {
                            MarketEvent::Trade(_) => trade_count += 1,
                            MarketEvent::Snapshot(_) => snapshot_count += 1,
                            _ => {}
                        }

                        strategy.on_market_event(&event);
                    },
                    Some(Err(e)) => {
                        error!("市場數據錯誤: {}", e);
                    },
                    None => {
                        warn!("市場數據流結束");
                        break;
                    }
                }
            },

            // 定期健康檢查
            _ = sleep(Duration::from_secs(10)) => {
                let health = execution_client.health().await;
                debug!("🏥 執行客戶端健康狀態: 連接={}, 延遲={:?}ms",
                      health.connected, health.latency_ms);
            }
        }
    }

    // 5. 統計信息
    info!("📊 演示結束統計:");
    info!("  - 總事件數: {}", event_count);
    info!("  - 交易事件: {}", trade_count);
    info!("  - 快照事件: {}", snapshot_count);
    info!(
        "  - 最後價格記錄: {} 個交易對",
        strategy.last_trade_prices.len()
    );

    for (symbol, price) in &strategy.last_trade_prices {
        info!("    {} 最後價格: {}", symbol.0, price);
    }

    // 6. 清理連接
    execution_client.disconnect().await?;
    info!("🔌 連接已斷開");

    info!("✨ 演示完成！");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_simple_logging_strategy() {
        let mut strategy = SimpleLoggingStrategy::new();

        // 測試交易事件處理
        let trade = ports::Trade {
            symbol: Symbol("BTC-PERP".to_string()),
            timestamp: 1234567890,
            price: Price::from_f64(50000.0).unwrap(),
            quantity: Quantity::from_f64(0.1).unwrap(),
            side: Side::Buy,
            trade_id: "test_trade".to_string(),
            source_venue: Some(hft_core::VenueId::HYPERLIQUID),
        };

        let event = MarketEvent::Trade(trade);
        strategy.on_market_event(&event);

        assert_eq!(strategy.last_trade_prices.len(), 1);
        assert!(strategy
            .last_trade_prices
            .contains_key(&Symbol("BTC-PERP".to_string())));
    }

    #[test]
    fn test_args_parsing() {
        // 測試默認參數
        let args = Args::parse_from(&["test"]);
        assert!(matches!(args.mode, TradingMode::Paper));
        assert_eq!(args.duration, 30);
        assert_eq!(args.symbols, vec!["BTC-PERP", "ETH-PERP"]);

        // 測試自定義參數
        let args = Args::parse_from(&[
            "test",
            "--mode",
            "live",
            "--duration",
            "60",
            "--symbols",
            "BTC-PERP,SOL-PERP",
            "--test-orders",
        ]);
        assert!(matches!(args.mode, TradingMode::Live));
        assert_eq!(args.duration, 60);
        assert_eq!(args.symbols, vec!["BTC-PERP", "SOL-PERP"]);
        assert!(args.test_orders);
    }
}
