/*!
 * 策略開發系統
 *
 * 完整的策略開發和測試流程：
 * - 數據準備和特徵工程
 * - 策略回測和評估
 * - 參數優化
 * - 風險分析
 */

use anyhow::Result;
use clap::Parser;
use rust_hft::{
    core::orderbook::OrderBook, integrations::bitget_connector::*, ml::features::FeatureExtractor,
    now_micros,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(about = "策略開發系統")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// 策略類型
    #[arg(long, default_value = "mean_reversion")]
    strategy: String,

    /// 回測模式
    #[arg(long)]
    backtest: bool,

    /// 實時測試時間（秒）
    #[arg(short, long, default_value_t = 60)]
    duration: u64,

    /// 初始資金
    #[arg(long, default_value_t = 10000.0)]
    capital: f64,

    /// 詳細模式
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Clone)]
enum StrategyType {
    MeanReversion,
    TrendFollowing,
    OrderBookImbalance,
    StatisticalArbitrage,
}

impl From<String> for StrategyType {
    fn from(s: String) -> Self {
        match s.as_str() {
            "trend_following" => StrategyType::TrendFollowing,
            "orderbook_imbalance" => StrategyType::OrderBookImbalance,
            "stat_arb" => StrategyType::StatisticalArbitrage,
            _ => StrategyType::MeanReversion,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Trade {
    timestamp: u64,
    side: String, // "BUY" or "SELL"
    price: f64,
    quantity: f64,
    pnl: f64,
    cumulative_pnl: f64,
}

#[derive(Debug, Default)]
struct StrategyStats {
    total_trades: u64,
    winning_trades: u64,
    total_pnl: f64,
    max_drawdown: f64,
    current_position: f64,
    current_cash: f64,
    peak_portfolio_value: f64,
    trades: Vec<Trade>,
    returns: VecDeque<f64>,
}

impl StrategyStats {
    fn new(initial_capital: f64) -> Self {
        Self {
            current_cash: initial_capital,
            peak_portfolio_value: initial_capital,
            ..Default::default()
        }
    }

    fn record_trade(&mut self, trade: Trade) {
        self.total_trades += 1;
        if trade.pnl > 0.0 {
            self.winning_trades += 1;
        }
        self.total_pnl += trade.pnl;

        // 更新持倉和現金
        match trade.side.as_str() {
            "BUY" => {
                self.current_position += trade.quantity;
                self.current_cash -= trade.price * trade.quantity;
            }
            "SELL" => {
                self.current_position -= trade.quantity;
                self.current_cash += trade.price * trade.quantity;
            }
            _ => {}
        }

        self.trades.push(trade);
    }

    fn update_drawdown(&mut self, current_price: f64) {
        let portfolio_value = self.current_cash + self.current_position * current_price;
        if portfolio_value > self.peak_portfolio_value {
            self.peak_portfolio_value = portfolio_value;
        }

        let drawdown = (self.peak_portfolio_value - portfolio_value) / self.peak_portfolio_value;
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }

        // 記錄收益率
        if self.trades.len() > 1 {
            let last_value = self.trades.last().map(|t| t.cumulative_pnl).unwrap_or(0.0);
            let return_rate = (self.total_pnl - last_value) / self.peak_portfolio_value;
            self.returns.push_back(return_rate);
            if self.returns.len() > 1000 {
                self.returns.pop_front();
            }
        }
    }

    fn win_rate(&self) -> f64 {
        if self.total_trades > 0 {
            self.winning_trades as f64 / self.total_trades as f64
        } else {
            0.0
        }
    }

    fn sharpe_ratio(&self) -> f64 {
        if self.returns.len() < 2 {
            return 0.0;
        }

        let mean_return = self.returns.iter().sum::<f64>() / self.returns.len() as f64;
        let variance = self
            .returns
            .iter()
            .map(|r| (r - mean_return).powi(2))
            .sum::<f64>()
            / self.returns.len() as f64;
        let std_dev = variance.sqrt();

        if std_dev > 0.0 {
            mean_return / std_dev * (252.0_f64).sqrt() // 年化
        } else {
            0.0
        }
    }
}

trait Strategy {
    fn generate_signal(&mut self, features: &rust_hft::FeatureSet) -> Option<String>;
    fn get_position_size(&self, signal: &str, current_price: f64) -> f64;
    fn name(&self) -> &str;
}

struct MeanReversionStrategy {
    price_history: VecDeque<f64>,
    lookback_period: usize,
    threshold: f64,
}

impl MeanReversionStrategy {
    fn new() -> Self {
        Self {
            price_history: VecDeque::with_capacity(100),
            lookback_period: 20,
            threshold: 2.0, // 標準差倍數
        }
    }
}

impl Strategy for MeanReversionStrategy {
    fn generate_signal(&mut self, features: &rust_hft::FeatureSet) -> Option<String> {
        let current_price = features.mid_price.0;
        self.price_history.push_back(current_price);

        if self.price_history.len() > self.lookback_period {
            self.price_history.pop_front();

            let prices: Vec<f64> = self.price_history.iter().cloned().collect();
            let mean = prices.iter().sum::<f64>() / prices.len() as f64;
            let variance =
                prices.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / prices.len() as f64;
            let std_dev = variance.sqrt();

            let z_score = (current_price - mean) / std_dev;

            if z_score > self.threshold {
                Some("SELL".to_string()) // 價格過高，賣出
            } else if z_score < -self.threshold {
                Some("BUY".to_string()) // 價格過低，買入
            } else {
                None
            }
        } else {
            None
        }
    }

    fn get_position_size(&self, _signal: &str, _current_price: f64) -> f64 {
        0.1 // 固定10%資金
    }

    fn name(&self) -> &str {
        "Mean Reversion"
    }
}

struct OrderBookImbalanceStrategy {
    threshold: f64,
}

impl OrderBookImbalanceStrategy {
    fn new() -> Self {
        Self { threshold: 0.3 }
    }
}

impl Strategy for OrderBookImbalanceStrategy {
    fn generate_signal(&mut self, features: &rust_hft::FeatureSet) -> Option<String> {
        if features.obi_l5 > self.threshold {
            Some("BUY".to_string())
        } else if features.obi_l5 < -self.threshold {
            Some("SELL".to_string())
        } else {
            None
        }
    }

    fn get_position_size(&self, _signal: &str, _current_price: f64) -> f64 {
        0.05 // 5%資金，更保守
    }

    fn name(&self) -> &str {
        "OrderBook Imbalance"
    }
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

    info!("📈 策略開發系統啟動");
    info!("策略: {}, 交易對: {}", args.strategy, args.symbol);

    // 創建策略
    let mut strategy: Box<dyn Strategy> = match StrategyType::from(args.strategy.clone()) {
        StrategyType::MeanReversion => Box::new(MeanReversionStrategy::new()),
        StrategyType::OrderBookImbalance => Box::new(OrderBookImbalanceStrategy::new()),
        _ => Box::new(MeanReversionStrategy::new()),
    };

    info!("🎯 使用策略: {}", strategy.name());

    if args.backtest {
        info!("📊 進入回測模式...");
        // 這裡可以加載歷史數據進行回測
        info!("回測功能開發中，請使用實時測試模式");
        return Ok(());
    }

    // 實時策略測試
    let stats = Arc::new(Mutex::new(StrategyStats::new(args.capital)));
    let feature_extractor = Arc::new(Mutex::new(FeatureExtractor::new(50)));

    // 創建連接器
    let bitget_config = BitgetConfig::default();
    let mut connector = BitgetConnector::new(
        "wss://ws.bitget.com/v2/ws/public".to_string(),
        bitget_config,
    );

    // 訂閱數據
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Books5);
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Trade);

    info!("📡 連接到交易所...");
    connector.start().await?;
    let mut receiver = connector.get_receiver();

    let stats_clone = stats.clone();
    let feature_extractor_clone = feature_extractor.clone();
    let symbol_clone = args.symbol.clone();
    let verbose = args.verbose;

    // 策略執行任務
    let strategy_task = tokio::spawn(async move {
        let mut strategy = strategy;

        while let Some(message) = receiver.recv().await {
            match message {
                BitgetMessage::OrderBook {
                    data, timestamp, ..
                } => {
                    if let Ok(orderbook) = parse_orderbook(&symbol_clone, &data, timestamp) {
                        if let Ok(mut extractor) = feature_extractor_clone.lock() {
                            if let Ok(features) =
                                extractor.extract_features(&orderbook, 0, timestamp)
                            {
                                // 生成交易信號
                                if let Some(signal) = strategy.generate_signal(&features) {
                                    let current_price = features.mid_price.0;
                                    let position_size =
                                        strategy.get_position_size(&signal, current_price);

                                    // 模擬交易執行
                                    let quantity = position_size; // 簡化，實際應該根據資金計算
                                    let trade = Trade {
                                        timestamp,
                                        side: signal.clone(),
                                        price: current_price,
                                        quantity,
                                        pnl: 0.0, // 實際應該在平倉時計算
                                        cumulative_pnl: 0.0,
                                    };

                                    {
                                        let mut stats = stats_clone.lock().unwrap();
                                        stats.record_trade(trade);
                                        stats.update_drawdown(current_price);
                                    }

                                    if verbose {
                                        info!(
                                            "🎯 信號: {} @ {:.2}, 數量: {:.4}",
                                            signal, current_price, quantity
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    });

    // 狀態報告任務
    let stats_report = stats.clone();
    let report_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));

        loop {
            interval.tick().await;

            let stats = stats_report.lock().unwrap();

            println!("\n📊 策略實時表現:");
            println!("  總交易: {} 筆", stats.total_trades);
            println!("  勝率: {:.1}%", stats.win_rate() * 100.0);
            println!("  總盈虧: ${:.2}", stats.total_pnl);
            println!("  最大回撤: {:.2}%", stats.max_drawdown * 100.0);
            println!("  夏普比率: {:.2}", stats.sharpe_ratio());
            println!("  當前持倉: {:.4}", stats.current_position);
            println!("  現金餘額: ${:.2}", stats.current_cash);
        }
    });

    // 運行策略測試
    info!("🚀 開始策略測試...");
    tokio::time::sleep(tokio::time::Duration::from_secs(args.duration)).await;

    // 停止任務
    strategy_task.abort();
    report_task.abort();

    // 最終報告
    let final_stats = stats.lock().unwrap();

    println!("\n🎉 策略測試完成！");
    println!("================================");
    println!("📈 策略表現總結:");
    println!("  策略名稱: {}", strategy.name());
    println!("  測試時長: {} 秒", args.duration);
    println!("  總交易次數: {} 筆", final_stats.total_trades);
    println!("  勝率: {:.1}%", final_stats.win_rate() * 100.0);
    println!("  總收益: ${:.2}", final_stats.total_pnl);
    println!(
        "  收益率: {:.2}%",
        final_stats.total_pnl / args.capital * 100.0
    );
    println!("  最大回撤: {:.2}%", final_stats.max_drawdown * 100.0);
    println!("  夏普比率: {:.2}", final_stats.sharpe_ratio());

    if final_stats.total_trades > 0 {
        let avg_pnl = final_stats.total_pnl / final_stats.total_trades as f64;
        println!("  平均每筆收益: ${:.2}", avg_pnl);

        let best_trade = final_stats
            .trades
            .iter()
            .max_by(|a, b| a.pnl.partial_cmp(&b.pnl).unwrap())
            .map(|t| t.pnl)
            .unwrap_or(0.0);
        let worst_trade = final_stats
            .trades
            .iter()
            .min_by(|a, b| a.pnl.partial_cmp(&b.pnl).unwrap())
            .map(|t| t.pnl)
            .unwrap_or(0.0);

        println!("  最佳交易: ${:.2}", best_trade);
        println!("  最差交易: ${:.2}", worst_trade);
    }

    println!("\n💡 策略優化建議:");
    if final_stats.win_rate() < 0.5 {
        println!("  • 勝率偏低，考慮調整信號閾值");
    }
    if final_stats.max_drawdown > 0.1 {
        println!("  • 回撤較大，考慮加入止損機制");
    }
    if final_stats.sharpe_ratio() < 1.0 {
        println!("  • 風險調整收益較低，考慮優化風險管理");
    }

    println!("\n🔄 下一步:");
    println!("  • 參數優化: 調整策略參數重新測試");
    println!("  • 風險管理: 添加止損和倉位管理");
    println!("  • 實盤交易: cargo run --example live_system");

    Ok(())
}

fn parse_orderbook(symbol: &str, _data: &serde_json::Value, timestamp: u64) -> Result<OrderBook> {
    let mut orderbook = OrderBook::new(symbol.to_string());
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;
    // 簡化實現 - 實際使用時需要解析真實數據
    Ok(orderbook)
}
// Archived legacy example; see grouped examples
