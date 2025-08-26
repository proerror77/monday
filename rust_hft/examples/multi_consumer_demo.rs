//! 多消費者事件分發系統演示
//!
//! 此示例展示了如何使用新的事件分發系統支持多個消費者同時處理市場數據和執行回報。
//!
//! 運行方式：
//! ```bash
//! cargo run --example multi_consumer_demo
//! ```

use rust_hft::core::types::{OrderSide, OrderStatus, OrderType};
use rust_hft::exchanges::*;
use hft_core::UnifiedTimestamp;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, sleep};
use tracing::{error, info, warn};

/// 高頻交易策略消費者
struct HftStrategy {
    id: String,
    target_symbol: String,
    position: f64,
    pnl: f64,
}

impl HftStrategy {
    fn new(id: String, target_symbol: String) -> Self {
        Self {
            id,
            target_symbol,
            position: 0.0,
            pnl: 0.0,
        }
    }

    async fn run(
        &mut self,
        mut market_receiver: tokio::sync::broadcast::Receiver<Arc<MarketEvent>>,
    ) {
        info!(
            "HFT Strategy {} started for {}",
            self.id, self.target_symbol
        );

        let mut last_price = 0.0;
        let mut event_count = 0;

        while let Ok(event) = market_receiver.recv().await {
            event_count += 1;

            match event.as_ref() {
                MarketEvent::OrderBookUpdate {
                    symbol, bids, asks, ..
                } if symbol == &self.target_symbol => {
                    if !bids.is_empty() && !asks.is_empty() {
                        let mid_price = (bids[0].price + asks[0].price) / 2.0;
                        let spread = asks[0].price - bids[0].price;

                        // 簡單的均值回歸策略
                        if last_price > 0.0 {
                            let price_change = (mid_price - last_price) / last_price;

                            // 如果價格變化超過閾值，反向交易
                            if price_change > 0.001 && self.position > -10.0 {
                                // 做空信號
                                self.position -= 1.0;
                                self.pnl -= mid_price; // 簡化的 PnL 計算
                                info!(
                                    "Strategy {}: SELL {} at {:.4}, position: {:.1}, PnL: {:.2}",
                                    self.id, symbol, mid_price, self.position, self.pnl
                                );
                            } else if price_change < -0.001 && self.position < 10.0 {
                                // 做多信號
                                self.position += 1.0;
                                self.pnl += mid_price; // 簡化的 PnL 計算
                                info!(
                                    "Strategy {}: BUY {} at {:.4}, position: {:.1}, PnL: {:.2}",
                                    self.id, symbol, mid_price, self.position, self.pnl
                                );
                            }
                        }

                        last_price = mid_price;

                        if event_count % 100 == 0 {
                            info!(
                                "Strategy {}: processed {} events, spread: {:.4}",
                                self.id, event_count, spread
                            );
                        }
                    }
                }
                MarketEvent::Trade {
                    symbol,
                    price,
                    quantity,
                    side,
                    ..
                } if symbol == &self.target_symbol => {
                    info!(
                        "Strategy {}: {} trade: {:?} {:.6} @ {:.4}",
                        self.id, symbol, side, quantity, price
                    );
                }
                _ => {}
            }
        }

        warn!("HFT Strategy {} stopped", self.id);
    }
}

/// 市場數據記錄器
struct MarketDataLogger {
    id: String,
    symbols: Vec<String>,
}

impl MarketDataLogger {
    fn new(id: String, symbols: Vec<String>) -> Self {
        Self { id, symbols }
    }

    async fn run(&self, mut receiver: tokio::sync::broadcast::Receiver<Arc<MarketEvent>>) {
        info!(
            "Market Data Logger {} started for symbols: {:?}",
            self.id, self.symbols
        );

        let mut orderbook_updates = 0;
        let mut trade_count = 0;
        let mut ticker_updates = 0;

        while let Ok(event) = receiver.recv().await {
            match event.as_ref() {
                MarketEvent::OrderBookUpdate {
                    symbol, timestamp, ..
                } => {
                    orderbook_updates += 1;
                    if orderbook_updates % 1000 == 0 {
                        info!(
                            "Logger {}: {} orderbook updates for {}",
                            self.id, orderbook_updates, symbol
                        );
                    }

                    // 這裡可以將數據寫入數據庫或文件
                    self.log_to_storage("orderbook", symbol, *timestamp).await;
                }
                MarketEvent::Trade {
                    symbol,
                    price,
                    quantity,
                    timestamp,
                    ..
                } => {
                    trade_count += 1;
                    info!(
                        "Logger {}: Trade {} @ {:.4} x {:.6} at {}",
                        self.id, symbol, price, quantity, timestamp
                    );

                    self.log_to_storage("trade", symbol, *timestamp).await;
                }
                MarketEvent::Ticker {
                    symbol,
                    last_price,
                    timestamp,
                    ..
                } => {
                    ticker_updates += 1;
                    if ticker_updates % 50 == 0 {
                        info!(
                            "Logger {}: Ticker {} @ {:.4} at {}",
                            self.id, symbol, last_price, timestamp
                        );
                    }

                    self.log_to_storage("ticker", symbol, *timestamp).await;
                }
                _ => {}
            }
        }

        warn!("Market Data Logger {} stopped", self.id);
    }

    async fn log_to_storage(&self, data_type: &str, symbol: &str, timestamp: u64) {
        // 模擬數據存儲操作
        if timestamp % 1000 == 0 {
            // 偶爾記錄
            info!(
                "Logger {}: Stored {} data for {} at {}",
                self.id, data_type, symbol, timestamp
            );
        }
    }
}

/// 風險監控器
struct RiskMonitor {
    id: String,
    max_position: f64,
    max_drawdown: f64,
}

impl RiskMonitor {
    fn new(id: String) -> Self {
        Self {
            id,
            max_position: 100.0,
            max_drawdown: -1000.0,
        }
    }

    async fn run(
        &self,
        mut execution_receiver: tokio::sync::broadcast::Receiver<Arc<ExecutionReport>>,
    ) {
        info!("Risk Monitor {} started", self.id);

        let mut total_position = 0.0;
        let mut total_pnl = 0.0;
        let mut executed_orders = 0;

        while let Ok(report) = execution_receiver.recv().await {
            executed_orders += 1;

            // 更新持倉
            match report.side {
                OrderSide::Buy => {
                    total_position += report.executed_quantity;
                }
                OrderSide::Sell => {
                    total_position -= report.executed_quantity;
                }
            }

            // 簡化的 PnL 計算
            let trade_value = report.executed_quantity * report.avg_price;
            match report.side {
                OrderSide::Buy => total_pnl -= trade_value,
                OrderSide::Sell => total_pnl += trade_value,
            }

            info!("Risk Monitor {}: Order {} filled: {:?} {:.6} {} @ {:.4}, Position: {:.2}, PnL: {:.2}", 
                 self.id, report.order_id, report.side, report.executed_quantity, 
                 report.symbol, report.avg_price, total_position, total_pnl);

            // 風險檢查
            if total_position.abs() > self.max_position {
                error!(
                    "Risk Monitor {}: POSITION LIMIT EXCEEDED! Position: {:.2}, Limit: {:.2}",
                    self.id, total_position, self.max_position
                );
            }

            if total_pnl < self.max_drawdown {
                error!(
                    "Risk Monitor {}: DRAWDOWN LIMIT EXCEEDED! PnL: {:.2}, Limit: {:.2}",
                    self.id, total_pnl, self.max_drawdown
                );
            }

            if executed_orders % 10 == 0 {
                info!(
                    "Risk Monitor {}: {} orders processed, total position: {:.2}, total PnL: {:.2}",
                    self.id, executed_orders, total_position, total_pnl
                );
            }
        }

        warn!("Risk Monitor {} stopped", self.id);
    }
}

/// 系統健康監控器
struct HealthMonitor {
    id: String,
}

impl HealthMonitor {
    fn new(id: String) -> Self {
        Self { id }
    }

    async fn run(&self, mut receiver: tokio::sync::broadcast::Receiver<Arc<MarketEvent>>) {
        info!("Health Monitor {} started", self.id);

        let mut last_heartbeat = std::time::Instant::now();
        let mut error_count = 0;
        let mut connection_issues = 0;

        while let Ok(event) = receiver.recv().await {
            match event.as_ref() {
                MarketEvent::Heartbeat {
                    exchange,
                    timestamp,
                } => {
                    last_heartbeat = std::time::Instant::now();
                    info!(
                        "Health Monitor {}: Heartbeat from {} at {}",
                        self.id, exchange, timestamp
                    );
                }
                MarketEvent::Error {
                    exchange,
                    error,
                    timestamp,
                } => {
                    error_count += 1;
                    error!(
                        "Health Monitor {}: Error from {} at {}: {}",
                        self.id, exchange, timestamp, error
                    );
                }
                MarketEvent::ConnectionStatus {
                    exchange,
                    status,
                    timestamp,
                } => {
                    info!(
                        "Health Monitor {}: {} connection status changed to {} at {}",
                        self.id, exchange, status, timestamp
                    );

                    if status != "Connected" {
                        connection_issues += 1;
                        warn!(
                            "Health Monitor {}: Connection issue detected for {}",
                            self.id, exchange
                        );
                    }
                }
                _ => {}
            }

            // 檢查心跳超時
            if last_heartbeat.elapsed() > Duration::from_secs(30) {
                error!(
                    "Health Monitor {}: HEARTBEAT TIMEOUT! Last heartbeat: {:?} ago",
                    self.id,
                    last_heartbeat.elapsed()
                );
            }

            // 定期報告統計
            if (error_count + connection_issues) % 10 == 0 && (error_count + connection_issues) > 0
            {
                warn!(
                    "Health Monitor {}: {} errors, {} connection issues detected",
                    self.id, error_count, connection_issues
                );
            }
        }

        warn!("Health Monitor {} stopped", self.id);
    }
}

/// 模擬市場數據生成器
async fn market_data_simulator(event_hub: Arc<ExchangeEventHub>) {
    info!("Market data simulator started");

    let symbols = vec!["BTCUSDT", "ETHUSDT", "ADAUSDT"];
    let mut prices = vec![50000.0, 3000.0, 1.5];
    let mut sequence = 0u64;

    let mut tick_interval = interval(Duration::from_millis(100));
    let mut heartbeat_interval = interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            _ = tick_interval.tick() => {
                // 生成市場數據
                for (i, symbol) in symbols.iter().enumerate() {
                    let price_change = (rand::random::<f64>() - 0.5) * 0.001;
                    prices[i] *= 1.0 + price_change;

                    // 生成訂單簿更新
                    let orderbook_event = MarketEvent::OrderBookUpdate {
                        symbol: symbol.to_string(),
                        exchange: "bitget".to_string(),
                        bids: vec![
                            OrderBookLevel {
                                price: prices[i] - 0.5,
                                quantity: 1.0 + rand::random::<f64>() * 2.0,
                                order_count: 5,
                            },
                            OrderBookLevel {
                                price: prices[i] - 1.0,
                                quantity: 2.0 + rand::random::<f64>() * 3.0,
                                order_count: 8,
                            },
                        ],
                        asks: vec![
                            OrderBookLevel {
                                price: prices[i] + 0.5,
                                quantity: 1.5 + rand::random::<f64>() * 2.0,
                                order_count: 3,
                            },
                            OrderBookLevel {
                                price: prices[i] + 1.0,
                                quantity: 2.5 + rand::random::<f64>() * 3.0,
                                order_count: 6,
                            },
                        ],
                        timestamp: hft_core::UnifiedTimestamp::current_timestamp(),
                        sequence,
                        is_snapshot: false,
                    };

                    event_hub.broadcast_market_event(orderbook_event).await;
                    sequence += 1;

                    // 偶爾生成成交事件
                    if rand::random::<f64>() > 0.7 {
                        let trade_event = MarketEvent::Trade {
                            symbol: symbol.to_string(),
                            exchange: "bitget".to_string(),
                            trade_id: format!("trade_{}_{}", symbol, sequence),
                            price: prices[i],
                            quantity: 0.1 + rand::random::<f64>() * 2.0,
                            side: if rand::random::<bool>() {
                                OrderSide::Buy
                            } else {
                                OrderSide::Sell
                            },
                            timestamp: hft_core::UnifiedTimestamp::current_timestamp(),
                            buyer_maker: rand::random::<bool>(),
                        };

                        event_hub.broadcast_market_event(trade_event).await;
                    }

                    // 偶爾生成 Ticker 更新
                    if rand::random::<f64>() > 0.9 {
                        let ticker_event = MarketEvent::Ticker {
                            symbol: symbol.to_string(),
                            exchange: "bitget".to_string(),
                            last_price: prices[i],
                            bid_price: prices[i] - 0.5,
                            ask_price: prices[i] + 0.5,
                            bid_size: 1.0,
                            ask_size: 1.0,
                            volume_24h: 10000.0,
                            change_24h: price_change,
                            timestamp: hft_core::UnifiedTimestamp::current_timestamp(),
                        };

                        event_hub.broadcast_market_event(ticker_event).await;
                    }
                }

                // 偶爾生成執行回報
                if rand::random::<f64>() > 0.8 {
                    let symbol = symbols[rand::random::<usize>() % symbols.len()];
                    let price_index = symbols.iter().position(|&s| s == symbol).unwrap();

                    let execution_report = ExecutionReport {
                        order_id: format!("order_{}_{}", symbol, sequence),
                        client_order_id: Some(format!("client_{}", sequence)),
                        exchange: "bitget".to_string(),
                        symbol: symbol.to_string(),
                        side: if rand::random::<bool>() {
                            OrderSide::Buy
                        } else {
                            OrderSide::Sell
                        },
                        order_type: OrderType::Limit,
                        status: OrderStatus::Filled,
                        original_quantity: 1.0,
                        executed_quantity: 1.0,
                        remaining_quantity: 0.0,
                        price: prices[price_index],
                        avg_price: prices[price_index],
                        last_executed_price: prices[price_index],
                        last_executed_quantity: 1.0,
                        commission: 0.1,
                        commission_asset: "USDT".to_string(),
                        create_time: hft_core::UnifiedTimestamp::current_timestamp(),
                        update_time: hft_core::UnifiedTimestamp::current_timestamp(),
                        transaction_time: hft_core::UnifiedTimestamp::current_timestamp(),
                        reject_reason: None,
                    };

                    event_hub.broadcast_execution_report(execution_report).await;
                }
            }

            _ = heartbeat_interval.tick() => {
                // 發送心跳
                let heartbeat_event = MarketEvent::Heartbeat {
                    exchange: "bitget".to_string(),
                    timestamp: hft_core::UnifiedTimestamp::current_timestamp(),
                };

                event_hub.broadcast_market_event(heartbeat_event).await;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Starting multi-consumer event hub demo");

    // 創建事件分發中心
    let event_hub = Arc::new(ExchangeEventHub::with_backpressure_config(
        BackpressureConfig {
            slow_consumer_threshold: 500,
            warning_interval: Duration::from_secs(10),
            auto_disconnect_slow: false, // 為了演示，不自動斷開慢消費者
            max_lag_count: 100,
        },
    ));

    // 創建不同的消費者

    // 1. HFT 策略（只關注 BTC）
    let btc_strategy_receiver = event_hub
        .subscribe_market_events(
            "btc_hft_strategy".to_string(),
            1000,
            EventFilter::SymbolAndExchange {
                symbol: "BTCUSDT".to_string(),
                exchange: "bitget".to_string(),
            },
        )
        .await;

    // 2. 市場數據記錄器（記錄所有數據）
    let logger_receiver = event_hub
        .subscribe_market_events("market_data_logger".to_string(), 5000, EventFilter::All)
        .await;

    // 3. 風險監控器（監控執行回報）
    let risk_execution_receiver = event_hub
        .subscribe_execution_reports("risk_monitor".to_string(), 1000, EventFilter::All)
        .await;

    // 4. 健康監控器（只關注系統事件）
    let health_receiver = event_hub
        .subscribe_market_events(
            "health_monitor".to_string(),
            500,
            EventFilter::Or(vec![
                EventFilter::EventType(EventType::Heartbeat),
                EventFilter::EventType(EventType::Error),
                EventFilter::EventType(EventType::ConnectionStatus),
            ]),
        )
        .await;

    // 啟動消費者任務
    let mut btc_strategy = HftStrategy::new("BTC_Momentum".to_string(), "BTCUSDT".to_string());
    let strategy_task = tokio::spawn(async move {
        btc_strategy.run(btc_strategy_receiver).await;
    });

    let logger = MarketDataLogger::new(
        "MainLogger".to_string(),
        vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "ADAUSDT".to_string(),
        ],
    );
    let logger_task = tokio::spawn(async move {
        logger.run(logger_receiver).await;
    });

    let risk_monitor = RiskMonitor::new("MainRisk".to_string());
    let risk_task = tokio::spawn(async move {
        risk_monitor.run(risk_execution_receiver).await;
    });

    let health_monitor = HealthMonitor::new("SystemHealth".to_string());
    let health_task = tokio::spawn(async move {
        health_monitor.run(health_receiver).await;
    });

    // 啟動市場數據模擬器
    let simulator_hub = event_hub.clone();
    let simulator_task = tokio::spawn(async move {
        market_data_simulator(simulator_hub).await;
    });

    // 定期報告統計信息
    let stats_hub = event_hub.clone();
    let stats_task = tokio::spawn(async move {
        let mut stats_interval = interval(Duration::from_secs(30));

        loop {
            stats_interval.tick().await;

            let hub_stats = stats_hub.get_hub_stats().await;
            let market_stats = stats_hub.get_market_subscriber_stats().await;
            let execution_stats = stats_hub.get_execution_subscriber_stats().await;

            info!("=== HUB STATISTICS ===");
            info!("Total events sent: {}", hub_stats.total_events_sent);
            info!("Total events dropped: {}", hub_stats.total_events_dropped);
            info!("Active subscribers: {}", hub_stats.active_subscribers);
            info!(
                "Average fan-out latency: {:.2} μs",
                hub_stats.avg_fan_out_latency_us
            );

            info!("=== MARKET SUBSCRIBER STATS ===");
            for stat in market_stats {
                info!(
                    "  {}: {} events received, {} filtered, queue size: {}, lagging: {}",
                    stat.subscriber_id,
                    stat.events_received,
                    stat.events_filtered,
                    stat.queue_size,
                    stat.is_lagging
                );
            }

            info!("=== EXECUTION SUBSCRIBER STATS ===");
            for stat in execution_stats {
                info!(
                    "  {}: {} events received, queue size: {}, lagging: {}",
                    stat.subscriber_id, stat.events_received, stat.queue_size, stat.is_lagging
                );
            }

            // 清理慢消費者
            stats_hub.cleanup_slow_consumers().await;
        }
    });

    info!("Demo is running... Press Ctrl+C to stop");

    // 運行所有任務
    tokio::select! {
        _ = strategy_task => error!("Strategy task ended unexpectedly"),
        _ = logger_task => error!("Logger task ended unexpectedly"),
        _ = risk_task => error!("Risk task ended unexpectedly"),
        _ = health_task => error!("Health task ended unexpectedly"),
        _ = simulator_task => error!("Simulator task ended unexpectedly"),
        _ = stats_task => error!("Stats task ended unexpectedly"),
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C, shutting down...");
        }
    }

    info!("Multi-consumer demo completed");
    Ok(())
}
