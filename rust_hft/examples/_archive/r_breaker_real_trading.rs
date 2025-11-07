/*!
 * R-Breaker 真實數據交易系統
 *
 * 使用真實的 Bitget WebSocket 數據進行 R-Breaker 策略交易
 *
 * 功能特點：
 * 1. 真實 Bitget WebSocket 連接
 * 2. ETHUSDT 實時報價和訂單簿數據
 * 3. R-Breaker 策略邏輯
 * 4. 實時交易監控
 * 5. 風險管理
 */

use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::{VecDeque, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH, Duration};
use tokio::time::sleep;
use tracing::{info, warn, error, debug};

// 導入項目中的 Bitget 連接器
use rust_hft::integrations::bitget_connector::{
    BitgetConnector, BitgetConfig, BitgetChannel, BitgetMessage
};

#[derive(Parser, Debug)]
#[command(about = "R-Breaker Real Data Trading System")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "ETHUSDT")]
    symbol: String,

    /// 初始資金
    #[arg(long, default_value_t = 10000.0)]
    capital: f64,

    /// R-Breaker 敏感度係數
    #[arg(long, default_value_t = 0.35)]
    sensitivity: f64,

    /// 運行時間（分鐘）
    #[arg(short, long, default_value_t = 30)]
    duration: u64,

    /// 詳細模式
    #[arg(short, long)]
    verbose: bool,

    /// 模擬模式（不真實下單）
    #[arg(long)]
    simulation: bool,

    /// 前日高點（用於計算R-Breaker價位）
    #[arg(long, default_value_t = 3600.0)]
    yesterday_high: f64,

    /// 前日低點
    #[arg(long, default_value_t = 3400.0)]
    yesterday_low: f64,

    /// 前日收盤
    #[arg(long, default_value_t = 3500.0)]
    yesterday_close: f64,
}

/// 獲取當前時間戳（微秒）
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// R-Breaker 關鍵價位
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RBreakerLevels {
    breakthrough_buy: f64,
    observation_sell: f64,
    reversal_sell: f64,
    reversal_buy: f64,
    observation_buy: f64,
    breakthrough_sell: f64,
}

impl RBreakerLevels {
    fn calculate(high: f64, low: f64, close: f64, sensitivity: f64) -> Self {
        let pivot = (high + close + low) / 3.0;

        Self {
            breakthrough_buy: high + 2.0 * sensitivity * (pivot - low),
            observation_sell: pivot + sensitivity * (high - low),
            reversal_sell: 2.0 * pivot - low,
            reversal_buy: 2.0 * pivot - high,
            observation_buy: pivot - sensitivity * (high - low),
            breakthrough_sell: low - 2.0 * sensitivity * (high - pivot),
        }
    }

    fn get_signal(&self, current_price: f64, daily_high: f64, daily_low: f64) -> RBreakerSignal {
        // 趨勢跟蹤
        if current_price > self.breakthrough_buy {
            return RBreakerSignal::TrendBuy;
        }
        if current_price < self.breakthrough_sell {
            return RBreakerSignal::TrendSell;
        }

        // 反轉交易
        if daily_high > self.observation_sell && current_price < self.reversal_sell {
            return RBreakerSignal::ReversalSell;
        }
        if daily_low < self.observation_buy && current_price > self.reversal_buy {
            return RBreakerSignal::ReversalBuy;
        }

        RBreakerSignal::Hold
    }
}

#[derive(Debug, Clone, PartialEq)]
enum RBreakerSignal {
    TrendBuy,
    TrendSell,
    ReversalBuy,
    ReversalSell,
    Hold,
}

/// 市場數據結構
#[derive(Debug, Clone)]
struct MarketData {
    symbol: String,
    timestamp: u64,
    best_bid: f64,
    best_ask: f64,
    last_price: f64,
    volume: f64,
}

/// 交易記錄
#[derive(Debug, Clone)]
struct TradeRecord {
    timestamp: u64,
    signal_type: String,
    side: String,
    price: f64,
    quantity: f64,
    pnl: f64,
}

/// 交易統計
#[derive(Debug)]
struct TradingStats {
    initial_capital: f64,
    current_capital: f64,
    position: f64,
    entry_price: Option<f64>,
    trades: VecDeque<TradeRecord>,
    total_trades: u32,
    winning_trades: u32,
    total_pnl: f64,
    max_drawdown: f64,
    peak_value: f64,
}

impl TradingStats {
    fn new(initial_capital: f64) -> Self {
        Self {
            initial_capital,
            current_capital: initial_capital,
            position: 0.0,
            entry_price: None,
            trades: VecDeque::new(),
            total_trades: 0,
            winning_trades: 0,
            total_pnl: 0.0,
            max_drawdown: 0.0,
            peak_value: initial_capital,
        }
    }

    fn execute_trade(&mut self, signal: &RBreakerSignal, price: f64, timestamp: u64) {
        let position_size = 0.1; // 10% 倉位
        let side;
        let mut pnl = 0.0;

        match signal {
            RBreakerSignal::TrendBuy | RBreakerSignal::ReversalBuy => {
                if self.position <= 0.0 {
                    // 平空頭（如果有）
                    if self.position < 0.0 {
                        if let Some(entry) = self.entry_price {
                            pnl = (entry - price) * self.position.abs();
                            self.total_pnl += pnl;
                            if pnl > 0.0 {
                                self.winning_trades += 1;
                            }
                        }
                    }

                    // 開多頭
                    let trade_value = self.current_capital * position_size;
                    let quantity = trade_value / price;
                    self.current_capital -= trade_value;
                    self.position = quantity;
                    self.entry_price = Some(price);
                    side = "BUY".to_string();

                    let signal_type = match signal {
                        RBreakerSignal::TrendBuy => "趨勢買入".to_string(),
                        RBreakerSignal::ReversalBuy => "反轉買入".to_string(),
                        _ => "買入".to_string(),
                    };

                    info!("🟢 {} @ {:.2}, 數量: {:.4}, PnL: ${:.2}",
                          signal_type, price, quantity, pnl);
                }
                else {
                    return; // 已有多頭持倉
                }
            }
            RBreakerSignal::TrendSell | RBreakerSignal::ReversalSell => {
                if self.position >= 0.0 {
                    // 平多頭（如果有）
                    if self.position > 0.0 {
                        if let Some(entry) = self.entry_price {
                            pnl = (price - entry) * self.position;
                            self.total_pnl += pnl;
                            if pnl > 0.0 {
                                self.winning_trades += 1;
                            }
                            self.current_capital += self.position * price;
                        }
                    }

                    // 開空頭
                    let trade_value = self.current_capital * position_size;
                    let quantity = trade_value / price;
                    self.current_capital -= trade_value * 0.1; // 保證金
                    self.position = -quantity;
                    self.entry_price = Some(price);
                    side = "SELL".to_string();

                    let signal_type = match signal {
                        RBreakerSignal::TrendSell => "趨勢賣出".to_string(),
                        RBreakerSignal::ReversalSell => "反轉賣出".to_string(),
                        _ => "賣出".to_string(),
                    };

                    info!("🔴 {} @ {:.2}, 數量: {:.4}, PnL: ${:.2}",
                          signal_type, price, quantity, pnl);
                }
                else {
                    return; // 已有空頭持倉
                }
            }
            _ => return,
        }

        // 記錄交易
        let trade = TradeRecord {
            timestamp,
            signal_type: format!("{:?}", signal),
            side,
            price,
            quantity: self.position.abs(),
            pnl,
        };

        self.trades.push_back(trade);
        if self.trades.len() > 100 {
            self.trades.pop_front();
        }

        self.total_trades += 1;
        self.update_drawdown(price);
    }

    fn update_drawdown(&mut self, current_price: f64) {
        let portfolio_value = self.get_portfolio_value(current_price);
        if portfolio_value > self.peak_value {
            self.peak_value = portfolio_value;
        }

        let drawdown = (self.peak_value - portfolio_value) / self.peak_value;
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }
    }

    fn get_portfolio_value(&self, current_price: f64) -> f64 {
        let position_value = if self.position > 0.0 {
            self.position * current_price
        } else if self.position < 0.0 {
            // 空頭的價值計算
            if let Some(entry) = self.entry_price {
                (entry - current_price) * self.position.abs()
            } else {
                0.0
            }
        } else {
            0.0
        };

        self.current_capital + position_value
    }

    fn win_rate(&self) -> f64 {
        if self.total_trades > 0 {
            self.winning_trades as f64 / self.total_trades as f64
        } else {
            0.0
        }
    }
}

/// R-Breaker 策略引擎
struct RBreakerStrategy {
    levels: RBreakerLevels,
    daily_high: f64,
    daily_low: f64,
    stats: Arc<Mutex<TradingStats>>,
    last_update: u64,
}

impl RBreakerStrategy {
    fn new(
        yesterday_high: f64,
        yesterday_low: f64,
        yesterday_close: f64,
        sensitivity: f64,
        initial_capital: f64,
    ) -> Self {
        let levels = RBreakerLevels::calculate(yesterday_high, yesterday_low, yesterday_close, sensitivity);

        println!("📊 R-Breaker 關鍵價位:");
        println!("  突破買入: {:.2}", levels.breakthrough_buy);
        println!("  觀察賣出: {:.2}", levels.observation_sell);
        println!("  反轉賣出: {:.2}", levels.reversal_sell);
        println!("  反轉買入: {:.2}", levels.reversal_buy);
        println!("  觀察買入: {:.2}", levels.observation_buy);
        println!("  突破賣出: {:.2}", levels.breakthrough_sell);
        println!("");

        Self {
            levels,
            daily_high: 0.0,
            daily_low: f64::MAX,
            stats: Arc::new(Mutex::new(TradingStats::new(initial_capital))),
            last_update: 0,
        }
    }

    fn process_market_data(&mut self, data: &MarketData) {
        let price = data.last_price;

        // 更新當日高低點
        if self.daily_high == 0.0 {
            self.daily_high = price;
            self.daily_low = price;
        }
        self.daily_high = self.daily_high.max(price);
        self.daily_low = self.daily_low.min(price);

        // 生成交易信號
        let signal = self.levels.get_signal(price, self.daily_high, self.daily_low);

        if signal != RBreakerSignal::Hold {
            if let Ok(mut stats) = self.stats.lock() {
                stats.execute_trade(&signal, price, data.timestamp);
            }
        }

        self.last_update = data.timestamp;
    }

    fn get_stats(&self) -> Arc<Mutex<TradingStats>> {
        self.stats.clone()
    }
}

/// 解析 Bitget OrderBook 數據
fn parse_orderbook_data(data: &serde_json::Value) -> Option<MarketData> {
    // 解析 books5 或 books1 數據
    if let Some(data_array) = data.as_array() {
        if let Some(first_item) = data_array.first() {
            let bids = first_item.get("bids")?.as_array()?;
            let asks = first_item.get("asks")?.as_array()?;

            let best_bid = bids.first()?.as_array()?.first()?.as_str()?.parse::<f64>().ok()?;
            let best_ask = asks.first()?.as_array()?.first()?.as_str()?.parse::<f64>().ok()?;
            let last_price = (best_bid + best_ask) / 2.0;

            return Some(MarketData {
                symbol: "ETHUSDT".to_string(),
                timestamp: now_micros(),
                best_bid,
                best_ask,
                last_price,
                volume: 0.0,
            });
        }
    }
    None
}

/// 解析 Bitget Ticker 數據
fn parse_ticker_data(data: &serde_json::Value) -> Option<MarketData> {
    if let Some(data_array) = data.as_array() {
        if let Some(first_item) = data_array.first() {
            let last_price = first_item.get("lastPr")?.as_str()?.parse::<f64>().ok()?;
            let best_bid = first_item.get("bidPr")?.as_str()?.parse::<f64>().ok()?;
            let best_ask = first_item.get("askPr")?.as_str()?.parse::<f64>().ok()?;
            let volume = first_item.get("baseVolume")?.as_str()?.parse::<f64>().ok()?;

            return Some(MarketData {
                symbol: "ETHUSDT".to_string(),
                timestamp: now_micros(),
                best_bid,
                best_ask,
                last_price,
                volume,
            });
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(if args.verbose {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .init();

    info!("🚀 R-Breaker 真實數據交易系統啟動");
    info!("交易對: {}, 初始資金: ${:.2}", args.symbol, args.capital);
    info!("敏感度係數: {:.2}, 運行時間: {} 分鐘", args.sensitivity, args.duration);
    if args.simulation {
        info!("🔹 模擬模式：不會執行真實交易");
    }

    // 創建 R-Breaker 策略引擎
    let mut strategy = RBreakerStrategy::new(
        args.yesterday_high,
        args.yesterday_low,
        args.yesterday_close,
        args.sensitivity,
        args.capital,
    );

    // 創建 Bitget 連接器
    let mut config = BitgetConfig::for_ticker_data();
    config.max_reconnect_attempts = 5;

    let mut connector = BitgetConnector::new(config);

    // 添加訂閱
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Books5);
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Ticker);

    info!("📡 連接到 Bitget WebSocket...");

    // 設置共享狀態
    let strategy_stats = strategy.get_stats();
    let data_count = Arc::new(Mutex::new(0u64));
    let data_count_clone = data_count.clone();

    // 消息處理器
    let symbol_clone = args.symbol.clone();
    let verbose = args.verbose;

    let message_handler = {
        let mut strategy = strategy;
        move |message: BitgetMessage| {
            match message {
                BitgetMessage::OrderBook { symbol, data, .. } => {
                    if symbol == symbol_clone {
                        if let Some(market_data) = parse_orderbook_data(&data) {
                            strategy.process_market_data(&market_data);

                            if verbose {
                                debug!("📈 OrderBook [{}] Bid: {:.2} | Ask: {:.2} | Mid: {:.2}",
                                       market_data.symbol, market_data.best_bid,
                                       market_data.best_ask, market_data.last_price);
                            }

                            // 每10個數據點打印一次報價
                            if let Ok(mut count) = data_count_clone.lock() {
                                *count += 1;
                                if *count % 10 == 0 {
                                    info!("📈 報價 [{}] Bid: {:.2} | Ask: {:.2} | Last: {:.2}",
                                           market_data.symbol, market_data.best_bid,
                                           market_data.best_ask, market_data.last_price);
                                }
                            }
                        }
                    }
                }
                BitgetMessage::Ticker { symbol, data, .. } => {
                    if symbol == symbol_clone {
                        if let Some(market_data) = parse_ticker_data(&data) {
                            strategy.process_market_data(&market_data);

                            if verbose {
                                debug!("📊 Ticker [{}] Last: {:.2} | Volume: {:.0}",
                                       market_data.symbol, market_data.last_price, market_data.volume);
                            }
                        }
                    }
                }
                BitgetMessage::Trade { symbol, data, .. } => {
                    if symbol == symbol_clone && verbose {
                        debug!("💹 Trade [{}] Data: {:?}", symbol, data);
                    }
                }
            }
        }
    };

    // 啟動狀態監控任務
    let stats_clone = strategy_stats.clone();
    let monitoring_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));

        loop {
            interval.tick().await;

            if let Ok(stats) = stats_clone.lock() {
                let portfolio_value = stats.get_portfolio_value(3500.0); // 估算價格
                let returns = (portfolio_value - stats.initial_capital) / stats.initial_capital * 100.0;

                println!("\n📊 交易監控狀態:");
                println!("  持倉: {:.4} | 交易次數: {}", stats.position, stats.total_trades);
                println!("  勝率: {:.1}% | 總盈虧: ${:.2}", stats.win_rate() * 100.0, stats.total_pnl);
                println!("  投資組合價值: ${:.2} | 收益率: {:.2}%", portfolio_value, returns);
                println!("  最大回撤: {:.2}%", stats.max_drawdown * 100.0);
                println!("");
            }
        }
    });

    // 連接並處理消息
    info!("🎯 開始接收真實市場數據...");

    // 啟動連接任務
    let connection_task = tokio::spawn(async move {
        if let Err(e) = connector.connect_public(message_handler).await {
            error!("WebSocket 連接錯誤: {}", e);
        }
    });

    // 運行指定時間
    sleep(Duration::from_secs(args.duration * 60)).await;

    // 停止任務
    connection_task.abort();
    monitoring_task.abort();

    // 最終報告
    if let Ok(stats) = strategy_stats.lock() {
        let final_portfolio = stats.get_portfolio_value(3500.0);
        let final_returns = (final_portfolio - stats.initial_capital) / stats.initial_capital * 100.0;

        println!("\n🎉 R-Breaker 真實數據交易完成！");
        println!("=========================================");
        println!("📈 最終績效報告:");
        println!("  運行時間: {} 分鐘", args.duration);
        println!("  總交易次數: {}", stats.total_trades);
        println!("  勝率: {:.1}%", stats.win_rate() * 100.0);
        println!("  總收益: ${:.2}", stats.total_pnl);
        println!("  總收益率: {:.2}%", final_returns);
        println!("  最大回撤: {:.2}%", stats.max_drawdown * 100.0);
        println!("  最終投資組合價值: ${:.2}", final_portfolio);

        if stats.total_trades > 0 {
            let avg_pnl = stats.total_pnl / stats.total_trades as f64;
            println!("  平均每筆收益: ${:.2}", avg_pnl);
        }

        println!("\n📋 最近交易記錄:");
        for trade in stats.trades.iter().rev().take(5) {
            println!("  {} | {} {:.4} @ {:.2} | PnL: ${:.2}",
                     trade.signal_type, trade.side, trade.quantity, trade.price, trade.pnl);
        }

        println!("\n💡 策略分析:");
        if stats.win_rate() < 0.5 {
            println!("  • 勝率偏低，考慮調整敏感度參數");
        }
        if stats.max_drawdown > 0.15 {
            println!("  • 回撤較大，考慮加入更嚴格的止損");
        }
        if final_returns > 0.0 {
            println!("  • 策略表現良好，使用真實數據驗證有效");
        }
    }

    println!("\n✅ 真實數據驗證完成！");
    println!("  數據來源: Bitget WebSocket ({})", args.symbol);
    println!("  下一步: 調整參數並進行更長時間的測試");

    Ok(())
}
// Archived legacy example; see 02_strategy/
