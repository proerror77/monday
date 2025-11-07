/*!
 * R-Breaker Live Trading System
 *
 * R-Breaker 策略是一種成熟的日內反轉策略，基於前一日的高低點計算阻力支撐位
 *
 * 策略原理：
 * 1. 計算六個關鍵價位：突破買入、觀察賣出、反轉賣出、反轉買入、觀察買入、突破賣出
 * 2. 基於價格突破這些關鍵位進行交易決策
 * 3. 支持趨勢跟蹤和反轉交易雙重邏輯
 */

use anyhow::Result;
use clap::Parser;
use rust_hft::{
    core::{
        orderbook::OrderBook,
        types::{Side, Timestamp, Price, Quantity},
    },
    engine::strategies::traits::{
        TradingStrategy, StrategyContext, TradingSignal, StrategyState,
        StrategyStatus, StrategyPerformance, StrategyPosition
    },
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
};
use serde::{Deserialize, Serialize};
use std::collections::{VecDeque, HashMap};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, interval};
use tracing::{info, warn, error, debug};
use async_trait::async_trait;

/// 獲取當前時間戳（微秒）
fn now_micros() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

#[derive(Parser, Debug)]
#[command(about = "R-Breaker Live Trading System")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// 初始資金
    #[arg(long, default_value_t = 10000.0)]
    capital: f64,

    /// 單筆交易最大倉位比例
    #[arg(long, default_value_t = 0.1)]
    max_position_ratio: f64,

    /// 止損比例
    #[arg(long, default_value_t = 0.02)]
    stop_loss: f64,

    /// 止盈比例
    #[arg(long, default_value_t = 0.04)]
    take_profit: f64,

    /// R-Breaker 敏感度係數
    #[arg(long, default_value_t = 0.35)]
    sensitivity: f64,

    /// 運行時間（分鐘）
    #[arg(short, long, default_value_t = 60)]
    duration: u64,

    /// 詳細模式
    #[arg(short, long)]
    verbose: bool,

    /// 模擬模式（不真實下單）
    #[arg(long)]
    simulation: bool,
}

/// R-Breaker 關鍵價位
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RBreakerLevels {
    /// 突破買入價位
    breakthrough_buy: f64,
    /// 觀察賣出價位
    observation_sell: f64,
    /// 反轉賣出價位
    reversal_sell: f64,
    /// 反轉買入價位
    reversal_buy: f64,
    /// 觀察買入價位
    observation_buy: f64,
    /// 突破賣出價位
    breakthrough_sell: f64,
    /// 前日高點
    yesterday_high: f64,
    /// 前日低點
    yesterday_low: f64,
    /// 前日收盤
    yesterday_close: f64,
}

impl RBreakerLevels {
    /// 根據前一日OHLC計算R-Breaker關鍵價位
    fn calculate(high: f64, low: f64, close: f64, sensitivity: f64) -> Self {
        let pivot = (high + close + low) / 3.0;

        // R-Breaker六個關鍵價位
        let breakthrough_buy = high + 2.0 * sensitivity * (pivot - low);
        let observation_sell = pivot + sensitivity * (high - low);
        let reversal_sell = 2.0 * pivot - low;
        let reversal_buy = 2.0 * pivot - high;
        let observation_buy = pivot - sensitivity * (high - low);
        let breakthrough_sell = low - 2.0 * sensitivity * (high - pivot);

        Self {
            breakthrough_buy,
            observation_sell,
            reversal_sell,
            reversal_buy,
            observation_buy,
            breakthrough_sell,
            yesterday_high: high,
            yesterday_low: low,
            yesterday_close: close,
        }
    }

    /// 獲取當前價格對應的交易信號
    fn get_signal(&self, current_price: f64, highest_since_open: f64, lowest_since_open: f64) -> RBreakerSignal {
        // 趨勢跟蹤邏輯
        if current_price > self.breakthrough_buy {
            return RBreakerSignal::TrendBuy;
        }
        if current_price < self.breakthrough_sell {
            return RBreakerSignal::TrendSell;
        }

        // 反轉交易邏輯
        if highest_since_open > self.observation_sell && current_price < self.reversal_sell {
            return RBreakerSignal::ReversalSell;
        }
        if lowest_since_open < self.observation_buy && current_price > self.reversal_buy {
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

/// K線數據
#[derive(Debug, Clone)]
struct KlineData {
    timestamp: u64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

/// R-Breaker 策略實現
pub struct RBreakerStrategy {
    /// 策略名稱
    name: String,
    /// 當前狀態
    status: StrategyStatus,
    /// R-Breaker 關鍵價位
    levels: Option<RBreakerLevels>,
    /// 當日最高價
    daily_high: f64,
    /// 當日最低價
    daily_low: f64,
    /// 當日開盤價
    daily_open: f64,
    /// 1分鐘K線歷史數據
    kline_history: VecDeque<KlineData>,
    /// 價格歷史（用於計算前日OHLC）
    price_history: VecDeque<f64>,
    /// 當前1分鐘K線累積
    current_kline: Option<KlineData>,
    /// 上次K線時間戳
    last_minute_timestamp: u64,
    /// 策略參數
    sensitivity: f64,
    stop_loss: f64,
    take_profit: f64,
    max_position_ratio: f64,
    /// 當前持倉
    current_position: f64,
    /// 入場價格
    entry_price: Option<f64>,
    /// 交易統計
    total_trades: u32,
    winning_trades: u32,
    total_pnl: f64,
    max_drawdown: f64,
    /// 最後更新時間
    last_update: u64,
    /// 最後信號
    last_signal: Option<TradingSignal>,
}

impl RBreakerStrategy {
    pub fn new(
        sensitivity: f64,
        stop_loss: f64,
        take_profit: f64,
        max_position_ratio: f64,
    ) -> Self {
        Self {
            name: "R-Breaker".to_string(),
            status: StrategyStatus::Initializing,
            levels: None,
            daily_high: 0.0,
            daily_low: f64::MAX,
            daily_open: 0.0,
            kline_history: VecDeque::with_capacity(1440), // 24小時 * 60分鐘
            price_history: VecDeque::with_capacity(10000),
            current_kline: None,
            last_minute_timestamp: 0,
            sensitivity,
            stop_loss,
            take_profit,
            max_position_ratio,
            current_position: 0.0,
            entry_price: None,
            total_trades: 0,
            winning_trades: 0,
            total_pnl: 0.0,
            max_drawdown: 0.0,
            last_update: 0,
            last_signal: None,
        }
    }

    /// 更新1分鐘K線數據
    fn update_kline(&mut self, price: f64, volume: f64, timestamp: u64) {
        let minute_timestamp = (timestamp / 60_000_000) * 60_000_000; // 對齊到分鐘

        // 檢查是否是新的一分鐘
        if minute_timestamp != self.last_minute_timestamp {
            // 保存上一分鐘的K線
            if let Some(kline) = self.current_kline.take() {
                self.kline_history.push_back(kline);
                if self.kline_history.len() > 1440 {
                    self.kline_history.pop_front();
                }
            }

            // 開始新的K線
            self.current_kline = Some(KlineData {
                timestamp: minute_timestamp,
                open: price,
                high: price,
                low: price,
                close: price,
                volume,
            });

            self.last_minute_timestamp = minute_timestamp;

            // 檢查是否需要更新R-Breaker價位（新的一天）
            self.check_daily_reset(timestamp);
        } else {
            // 更新當前K線
            if let Some(ref mut kline) = self.current_kline {
                kline.high = kline.high.max(price);
                kline.low = kline.low.min(price);
                kline.close = price;
                kline.volume += volume;
            }
        }

        // 更新當日高低點
        if self.daily_open == 0.0 {
            self.daily_open = price;
        }
        self.daily_high = self.daily_high.max(price);
        self.daily_low = self.daily_low.min(price);

        // 更新價格歷史
        self.price_history.push_back(price);
        if self.price_history.len() > 10000 {
            self.price_history.pop_front();
        }
    }

    /// 檢查是否需要重置當日數據（新的一天）
    fn check_daily_reset(&mut self, timestamp: u64) {
        // 簡化實現：假設每24小時重置一次
        // 實際實現中應該根據交易所的具體時區來判斷
        let hours_since_epoch = timestamp / (3600 * 1000 * 1000);
        let current_day = hours_since_epoch / 24;

        // 這裡需要更精確的邏輯來判斷新的交易日
        // 暫時使用簡化實現
    }

    /// 計算前一日OHLC並更新R-Breaker價位
    fn update_rbreaker_levels(&mut self) {
        if self.kline_history.len() >= 1440 {
            // 獲取前一日的K線數據
            let yesterday_start = self.kline_history.len().saturating_sub(1440);
            let yesterday_klines: Vec<&KlineData> = self.kline_history
                .range(yesterday_start..)
                .collect();

            if !yesterday_klines.is_empty() {
                let high = yesterday_klines.iter().map(|k| k.high).fold(0.0, f64::max);
                let low = yesterday_klines.iter().map(|k| k.low).fold(f64::MAX, f64::min);
                let close = yesterday_klines.last().unwrap().close;

                self.levels = Some(RBreakerLevels::calculate(high, low, close, self.sensitivity));

                debug!("R-Breaker價位更新: {:?}", self.levels);
            }
        }
    }

    /// 生成交易信號
    fn generate_signal(&mut self, current_price: f64) -> Option<TradingSignal> {
        let levels = self.levels.as_ref()?;

        let signal = levels.get_signal(current_price, self.daily_high, self.daily_low);

        match signal {
            RBreakerSignal::TrendBuy | RBreakerSignal::ReversalBuy => {
                if self.current_position <= 0.0 {
                    Some(TradingSignal::Buy {
                        quantity: self.max_position_ratio,
                        price: Some(current_price),
                        confidence: self.calculate_confidence(&signal, current_price),
                    })
                } else {
                    None
                }
            }
            RBreakerSignal::TrendSell | RBreakerSignal::ReversalSell => {
                if self.current_position >= 0.0 {
                    Some(TradingSignal::Sell {
                        quantity: self.max_position_ratio,
                        price: Some(current_price),
                        confidence: self.calculate_confidence(&signal, current_price),
                    })
                } else {
                    None
                }
            }
            RBreakerSignal::Hold => {
                // 檢查止損止盈
                self.check_stop_loss_take_profit(current_price)
            }
        }
    }

    /// 計算信號信心度
    fn calculate_confidence(&self, signal: &RBreakerSignal, current_price: f64) -> f64 {
        let levels = match &self.levels {
            Some(l) => l,
            None => return 0.5,
        };

        match signal {
            RBreakerSignal::TrendBuy => {
                let distance = current_price - levels.breakthrough_buy;
                let range = levels.yesterday_high - levels.yesterday_low;
                0.7 + (distance / range).min(0.3)
            }
            RBreakerSignal::TrendSell => {
                let distance = levels.breakthrough_sell - current_price;
                let range = levels.yesterday_high - levels.yesterday_low;
                0.7 + (distance / range).min(0.3)
            }
            RBreakerSignal::ReversalBuy => 0.8,
            RBreakerSignal::ReversalSell => 0.8,
            RBreakerSignal::Hold => 0.0,
        }
    }

    /// 檢查止損止盈
    fn check_stop_loss_take_profit(&self, current_price: f64) -> Option<TradingSignal> {
        if let Some(entry_price) = self.entry_price {
            if self.current_position > 0.0 {
                // 多頭持倉
                let pnl_ratio = (current_price - entry_price) / entry_price;
                if pnl_ratio <= -self.stop_loss {
                    return Some(TradingSignal::CloseAll);
                }
                if pnl_ratio >= self.take_profit {
                    return Some(TradingSignal::CloseAll);
                }
            } else if self.current_position < 0.0 {
                // 空頭持倉
                let pnl_ratio = (entry_price - current_price) / entry_price;
                if pnl_ratio <= -self.stop_loss {
                    return Some(TradingSignal::CloseAll);
                }
                if pnl_ratio >= self.take_profit {
                    return Some(TradingSignal::CloseAll);
                }
            }
        }
        None
    }
}

#[async_trait]
impl TradingStrategy for RBreakerStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    async fn initialize(&mut self, _context: &StrategyContext) -> Result<()> {
        self.status = StrategyStatus::Ready;
        info!("R-Breaker策略初始化完成");
        Ok(())
    }

    async fn on_orderbook_update(
        &mut self,
        _symbol: &str,
        orderbook: &OrderBook,
    ) -> Result<TradingSignal> {
        let current_price = (orderbook.best_bid + orderbook.best_ask) / 2.0;
        let timestamp = orderbook.last_update;

        // 更新K線數據
        self.update_kline(current_price, 1.0, timestamp);

        // 如果還沒有R-Breaker價位，嘗試計算
        if self.levels.is_none() {
            self.update_rbreaker_levels();
        }

        self.last_update = timestamp;

        // 生成交易信號
        let signal = self.generate_signal(current_price).unwrap_or(TradingSignal::Hold);
        self.last_signal = Some(signal.clone());

        Ok(signal)
    }

    async fn on_trade_execution(&mut self, trade: &TradeExecution) -> Result<()> {
        self.total_trades += 1;

        match trade.side {
            Side::Buy => {
                self.current_position += trade.quantity;
                self.entry_price = Some(trade.price);
            }
            Side::Sell => {
                if self.current_position > 0.0 {
                    // 平多頭
                    let pnl = (trade.price - self.entry_price.unwrap_or(trade.price)) * trade.quantity;
                    self.total_pnl += pnl;
                    if pnl > 0.0 {
                        self.winning_trades += 1;
                    }
                }
                self.current_position -= trade.quantity;
                if self.current_position <= 0.0 {
                    self.entry_price = Some(trade.price);
                }
            }
        }

        // 如果平倉完畢
        if self.current_position.abs() < 0.0001 {
            self.entry_price = None;
        }

        info!("交易執行: {:?} {} @ {:.2}, 當前持倉: {:.4}",
              trade.side, trade.quantity, trade.price, self.current_position);

        Ok(())
    }

    fn get_state(&self) -> StrategyState {
        let performance = StrategyPerformance {
            total_trades: self.total_trades,
            winning_trades: self.winning_trades,
            losing_trades: self.total_trades - self.winning_trades,
            total_pnl: self.total_pnl,
            max_drawdown: self.max_drawdown,
            sharpe_ratio: 0.0, // 需要更複雜的計算
            win_rate: if self.total_trades > 0 {
                self.winning_trades as f64 / self.total_trades as f64
            } else {
                0.0
            },
        };

        let positions = if self.current_position.abs() > 0.0001 {
            vec![StrategyPosition {
                symbol: "BTCUSDT".to_string(),
                side: if self.current_position > 0.0 { Side::Buy } else { Side::Sell },
                quantity: self.current_position.abs(),
                average_price: self.entry_price.unwrap_or(0.0),
                unrealized_pnl: 0.0, // 需要當前價格計算
                entry_time: self.last_update,
            }]
        } else {
            vec![]
        };

        StrategyState {
            name: self.name.clone(),
            status: self.status.clone(),
            performance,
            positions,
            last_signal: self.last_signal.clone(),
            last_update: self.last_update,
        }
    }

    async fn reset(&mut self) -> Result<()> {
        self.status = StrategyStatus::Initializing;
        self.current_position = 0.0;
        self.entry_price = None;
        self.total_trades = 0;
        self.winning_trades = 0;
        self.total_pnl = 0.0;
        self.daily_high = 0.0;
        self.daily_low = f64::MAX;
        self.daily_open = 0.0;
        info!("R-Breaker策略已重置");
        Ok(())
    }

    fn is_ready(&self) -> bool {
        matches!(self.status, StrategyStatus::Ready | StrategyStatus::Active)
    }
}

/// 交易執行器（模擬）
struct SimulatedTradeExecutor {
    initial_capital: f64,
    current_capital: f64,
    current_position: f64,
    commission_rate: f64,
}

impl SimulatedTradeExecutor {
    fn new(initial_capital: f64) -> Self {
        Self {
            initial_capital,
            current_capital: initial_capital,
            current_position: 0.0,
            commission_rate: 0.001, // 0.1% 手續費
        }
    }

    async fn execute_signal(
        &mut self,
        signal: &TradingSignal,
        current_price: f64,
        timestamp: u64,
    ) -> Option<TradeExecution> {
        match signal {
            TradingSignal::Buy { quantity, .. } => {
                let trade_value = self.current_capital * quantity;
                let shares = trade_value / current_price;
                let commission = trade_value * self.commission_rate;

                if self.current_capital >= trade_value + commission {
                    self.current_capital -= trade_value + commission;
                    self.current_position += shares;

                    Some(TradeExecution {
                        order_id: format!("buy_{}", timestamp),
                        symbol: "BTCUSDT".to_string(),
                        side: Side::Buy,
                        quantity: shares,
                        price: current_price,
                        timestamp,
                        commission,
                    })
                } else {
                    None
                }
            }
            TradingSignal::Sell { quantity, .. } => {
                let shares = self.current_position * quantity;
                let trade_value = shares * current_price;
                let commission = trade_value * self.commission_rate;

                if self.current_position >= shares {
                    self.current_capital += trade_value - commission;
                    self.current_position -= shares;

                    Some(TradeExecution {
                        order_id: format!("sell_{}", timestamp),
                        symbol: "BTCUSDT".to_string(),
                        side: Side::Sell,
                        quantity: shares,
                        price: current_price,
                        timestamp,
                        commission,
                    })
                } else {
                    None
                }
            }
            TradingSignal::CloseAll => {
                if self.current_position > 0.0 {
                    let trade_value = self.current_position * current_price;
                    let commission = trade_value * self.commission_rate;

                    self.current_capital += trade_value - commission;
                    let shares = self.current_position;
                    self.current_position = 0.0;

                    Some(TradeExecution {
                        order_id: format!("close_{}", timestamp),
                        symbol: "BTCUSDT".to_string(),
                        side: Side::Sell,
                        quantity: shares,
                        price: current_price,
                        timestamp,
                        commission,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn get_portfolio_value(&self, current_price: f64) -> f64 {
        self.current_capital + self.current_position * current_price
    }

    fn get_returns(&self, current_price: f64) -> f64 {
        (self.get_portfolio_value(current_price) - self.initial_capital) / self.initial_capital
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

    info!("🚀 R-Breaker Live Trading System 啟動");
    info!("交易對: {}, 初始資金: ${:.2}", args.symbol, args.capital);
    info!("參數 - 敏感度: {:.2}, 止損: {:.2}%, 止盈: {:.2}%",
          args.sensitivity, args.stop_loss * 100.0, args.take_profit * 100.0);

    // 創建策略
    let mut strategy = RBreakerStrategy::new(
        args.sensitivity,
        args.stop_loss,
        args.take_profit,
        args.max_position_ratio,
    );

    // 初始化策略
    let context = StrategyContext::default();
    strategy.initialize(&context).await?;

    // 創建模擬交易執行器
    let executor = Arc::new(Mutex::new(SimulatedTradeExecutor::new(args.capital)));

    // 創建連接器
    let bitget_config = BitgetConfig::default();
    let mut connector = BitgetConnector::new(
        "wss://ws.bitget.com/v2/ws/public".to_string(),
        bitget_config,
    );

    // 訂閱數據
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Books5);
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Trade);

    info!("📡 連接到Bitget交易所...");
    connector.start().await?;
    let mut receiver = connector.get_receiver();

    let strategy = Arc::new(Mutex::new(strategy));
    let strategy_clone = strategy.clone();
    let executor_clone = executor.clone();
    let symbol_clone = args.symbol.clone();
    let verbose = args.verbose;

    // 主要交易邏輯
    let trading_task = tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            match message {
                BitgetMessage::OrderBook { data, timestamp, .. } => {
                    if let Ok(orderbook) = parse_orderbook(&symbol_clone, &data, timestamp) {
                        let current_price = (orderbook.best_bid + orderbook.best_ask) / 2.0;

                        // 策略信號生成
                        if let Ok(mut strat) = strategy_clone.lock() {
                            if let Ok(signal) = strat.on_orderbook_update(&symbol_clone, &orderbook).await {
                                if !matches!(signal, TradingSignal::Hold) {
                                    if verbose {
                                        info!("📊 信號: {:?} @ {:.2}", signal, current_price);
                                    }

                                    // 執行交易
                                    if let Ok(mut exec) = executor_clone.lock() {
                                        if let Some(trade) = exec.execute_signal(&signal, current_price, timestamp).await {
                                            if let Err(e) = strat.on_trade_execution(&trade).await {
                                                error!("交易執行回調失敗: {}", e);
                                            }

                                            let portfolio_value = exec.get_portfolio_value(current_price);
                                            let returns = exec.get_returns(current_price);

                                            info!("✅ 交易執行: {:?} {:.4} @ {:.2}",
                                                  trade.side, trade.quantity, trade.price);
                                            info!("💰 投資組合價值: ${:.2} (收益率: {:.2}%)",
                                                  portfolio_value, returns * 100.0);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                BitgetMessage::Trade { data, timestamp, .. } => {
                    // 處理成交數據，用於更新K線
                    if verbose {
                        debug!("📈 Trade data received at {}", timestamp);
                    }
                }
                _ => {}
            }
        }
    });

    // 狀態報告任務
    let strategy_report = strategy.clone();
    let executor_report = executor.clone();
    let report_task = tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(30));

        loop {
            interval.tick().await;

            if let (Ok(strat), Ok(exec)) = (strategy_report.lock(), executor_report.lock()) {
                let state = strat.get_state();
                let portfolio_value = exec.get_portfolio_value(0.0); // 需要當前價格
                let returns = exec.get_returns(0.0);

                println!("\n📊 R-Breaker 策略實時報告");
                println!("========================================");
                println!("策略狀態: {:?}", state.status);
                println!("總交易次數: {}", state.performance.total_trades);
                println!("勝率: {:.1}%", state.performance.win_rate * 100.0);
                println!("總盈虧: ${:.2}", state.performance.total_pnl);
                println!("最大回撤: {:.2}%", state.performance.max_drawdown * 100.0);
                println!("當前持倉: {:.4}", exec.current_position);
                println!("現金餘額: ${:.2}", exec.current_capital);
                println!("投資組合價值: ${:.2}", portfolio_value);
                println!("總收益率: {:.2}%", returns * 100.0);

                if let Some(levels) = &strat.levels {
                    println!("\nR-Breaker 關鍵價位:");
                    println!("  突破買入: {:.2}", levels.breakthrough_buy);
                    println!("  觀察賣出: {:.2}", levels.observation_sell);
                    println!("  反轉賣出: {:.2}", levels.reversal_sell);
                    println!("  反轉買入: {:.2}", levels.reversal_buy);
                    println!("  觀察買入: {:.2}", levels.observation_buy);
                    println!("  突破賣出: {:.2}", levels.breakthrough_sell);
                }
            }
        }
    });

    // 運行指定時間
    info!("🎯 開始R-Breaker實時交易，運行時間: {} 分鐘", args.duration);
    tokio::time::sleep(Duration::from_secs(args.duration * 60)).await;

    // 停止任務
    trading_task.abort();
    report_task.abort();

    // 最終報告
    if let (Ok(strat), Ok(exec)) = (strategy.lock(), executor.lock()) {
        let state = strat.get_state();
        let final_returns = exec.get_returns(0.0);

        println!("\n🎉 R-Breaker 交易完成！");
        println!("========================================");
        println!("📈 最終績效報告:");
        println!("  運行時間: {} 分鐘", args.duration);
        println!("  總交易次數: {}", state.performance.total_trades);
        println!("  勝率: {:.1}%", state.performance.win_rate * 100.0);
        println!("  總收益: ${:.2}", state.performance.total_pnl);
        println!("  總收益率: {:.2}%", final_returns * 100.0);
        println!("  最大回撤: {:.2}%", state.performance.max_drawdown * 100.0);
        println!("  最終投資組合價值: ${:.2}", exec.get_portfolio_value(0.0));

        if state.performance.total_trades > 0 {
            let avg_pnl = state.performance.total_pnl / state.performance.total_trades as f64;
            println!("  平均每筆收益: ${:.2}", avg_pnl);
        }

        println!("\n💡 策略分析:");
        if state.performance.win_rate < 0.5 {
            println!("  • 勝率偏低，考慮調整敏感度參數");
        }
        if state.performance.max_drawdown > 0.15 {
            println!("  • 回撤較大，考慮收緊止損設置");
        }
        if final_returns > 0.05 {
            println!("  • 策略表現良好，可考慮增加倉位");
        }
    }

    Ok(())
}

/// 簡化的訂單簿解析函數
fn parse_orderbook(symbol: &str, _data: &serde_json::Value, timestamp: u64) -> Result<OrderBook> {
    let mut orderbook = OrderBook::new(symbol.to_string());
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;

    // 這裡應該解析真實的訂單簿數據
    // 簡化實現，需要根據實際Bitget數據格式進行解析
    orderbook.best_bid = 67000.0; // 示例價格
    orderbook.best_ask = 67001.0;

    Ok(orderbook)
}
// Archived legacy example; see 02_strategy/
