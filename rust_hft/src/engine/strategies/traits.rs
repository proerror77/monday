/*!
 * Trading Strategy Traits
 * 
 * 交易策略的接口定义
 */

use crate::core::orderbook::OrderBook;
use crate::core::types::*;
use anyhow::Result;
use async_trait::async_trait;

/// 交易策略特徵
#[async_trait]
pub trait TradingStrategy: Send + Sync {
    /// 策略名稱
    fn name(&self) -> &str;
    
    /// 初始化策略
    async fn initialize(&mut self, context: &StrategyContext) -> Result<()>;
    
    /// 處理訂單簿更新
    async fn on_orderbook_update(&mut self, symbol: &str, orderbook: &OrderBook) -> Result<TradingSignal>;
    
    /// 處理交易執行結果
    async fn on_trade_execution(&mut self, trade: &TradeExecution) -> Result<()>;
    
    /// 獲取策略狀態
    fn get_state(&self) -> StrategyState;
    
    /// 重置策略
    async fn reset(&mut self) -> Result<()>;
    
    /// 策略是否準備好交易
    fn is_ready(&self) -> bool;
}

/// 策略上下文
#[derive(Debug, Clone)]
pub struct StrategyContext {
    pub symbols: Vec<String>,
    pub initial_capital: f64,
    pub max_position_size: f64,
    pub risk_tolerance: f64,
    pub strategy_params: StrategyParameters,
}

/// 策略參數
#[derive(Debug, Clone)]
pub struct StrategyParameters {
    pub lookback_period: usize,
    pub threshold: f64,
    pub stop_loss: f64,
    pub take_profit: f64,
    pub max_trades_per_day: u32,
}

/// 交易信號
#[derive(Debug, Clone)]
pub enum TradingSignal {
    Buy { quantity: f64, price: Option<f64>, confidence: f64 },
    Sell { quantity: f64, price: Option<f64>, confidence: f64 },
    Hold,
    Close { ratio: f64 }, // Close partial position
    CloseAll,
}

/// 交易執行結果
#[derive(Debug, Clone)]
pub struct TradeExecution {
    pub order_id: String,
    pub symbol: String,
    pub side: Side,
    pub quantity: f64,
    pub price: f64,
    pub timestamp: Timestamp,
    pub commission: f64,
}

/// 策略狀態
#[derive(Debug, Clone)]
pub struct StrategyState {
    pub name: String,
    pub status: StrategyStatus,
    pub performance: StrategyPerformance,
    pub positions: Vec<StrategyPosition>,
    pub last_signal: Option<TradingSignal>,
    pub last_update: Timestamp,
}

/// 策略狀態
#[derive(Debug, Clone)]
pub enum StrategyStatus {
    Initializing,
    Ready,
    Active,
    Paused,
    Stopped,
    Error(String),
}

/// 策略績效
#[derive(Debug, Clone)]
pub struct StrategyPerformance {
    pub total_trades: u32,
    pub winning_trades: u32,
    pub losing_trades: u32,
    pub total_pnl: f64,
    pub max_drawdown: f64,
    pub sharpe_ratio: f64,
    pub win_rate: f64,
}

/// 策略持倉
#[derive(Debug, Clone)]
pub struct StrategyPosition {
    pub symbol: String,
    pub side: Side,
    pub quantity: f64,
    pub average_price: f64,
    pub unrealized_pnl: f64,
    pub entry_time: Timestamp,
}

/// 策略結果類型
pub type StrategyResult<T> = Result<T>;

impl Default for StrategyContext {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: 100000.0,
            max_position_size: 0.1,
            risk_tolerance: 0.02,
            strategy_params: StrategyParameters::default(),
        }
    }
}

impl Default for StrategyParameters {
    fn default() -> Self {
        Self {
            lookback_period: 20,
            threshold: 0.01,
            stop_loss: 0.02,
            take_profit: 0.04,
            max_trades_per_day: 10,
        }
    }
}

impl Default for StrategyPerformance {
    fn default() -> Self {
        Self {
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            total_pnl: 0.0,
            max_drawdown: 0.0,
            sharpe_ratio: 0.0,
            win_rate: 0.0,
        }
    }
}

impl TradingSignal {
    /// 獲取信號的信心度
    pub fn confidence(&self) -> f64 {
        match self {
            TradingSignal::Buy { confidence, .. } => *confidence,
            TradingSignal::Sell { confidence, .. } => *confidence,
            TradingSignal::Hold => 0.0,
            TradingSignal::Close { .. } => 1.0,
            TradingSignal::CloseAll => 1.0,
        }
    }

    /// 判斷是否為買入信號
    pub fn is_buy(&self) -> bool {
        matches!(self, TradingSignal::Buy { .. })
    }

    /// 判斷是否為賣出信號
    pub fn is_sell(&self) -> bool {
        matches!(self, TradingSignal::Sell { .. })
    }

    /// 判斷是否為平倉信號
    pub fn is_close(&self) -> bool {
        matches!(self, TradingSignal::Close { .. } | TradingSignal::CloseAll)
    }
}