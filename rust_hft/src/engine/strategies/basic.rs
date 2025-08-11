/*!
 * Basic Trading Strategies
 * 
 * Simple rule-based trading strategies
 */

use super::traits::{
    TradingStrategy, StrategyContext, StrategyState as TraitState, 
    StrategyStatus, StrategyPerformance, TradingSignal
};
use crate::core::orderbook::OrderBook;
use anyhow::Result;
use async_trait::async_trait;

/// Basic trading strategy
#[derive(Debug)]
pub struct BasicStrategy {
    name: String,
    context: Option<StrategyContext>,
    state: TraitState,
}

impl BasicStrategy {
    pub fn new(name: String) -> Self {
        Self {
            name: name.clone(),
            context: None,
            state: TraitState {
                name: name.clone(),
                status: StrategyStatus::Initializing,
                performance: StrategyPerformance::default(),
                positions: Vec::new(),
                last_signal: None,
                last_update: chrono::Utc::now().timestamp_micros() as u64,
            },
        }
    }
}

#[async_trait]
impl TradingStrategy for BasicStrategy {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn initialize(&mut self, context: &StrategyContext) -> Result<()> {
        self.context = Some(context.clone());
        self.state.status = StrategyStatus::Ready;
        Ok(())
    }
    
    async fn on_orderbook_update(&mut self, _symbol: &str, _orderbook: &OrderBook) -> Result<TradingSignal> {
        // Simple hold strategy for now
        let signal = TradingSignal::Hold;
        self.state.last_signal = Some(signal.clone());
        self.state.last_update = chrono::Utc::now().timestamp_micros() as u64;
        Ok(signal)
    }
    
    async fn on_trade_execution(&mut self, _trade: &super::traits::TradeExecution) -> Result<()> {
        self.state.performance.total_trades += 1;
        Ok(())
    }
    
    fn get_state(&self) -> TraitState {
        self.state.clone()
    }
    
    async fn reset(&mut self) -> Result<()> {
        self.state.status = StrategyStatus::Ready;
        self.state.performance = StrategyPerformance::default();
        self.state.positions.clear();
        self.state.last_signal = None;
        Ok(())
    }
    
    fn is_ready(&self) -> bool {
        matches!(self.state.status, StrategyStatus::Ready | StrategyStatus::Active)
    }
}

/// Momentum strategy
#[derive(Debug)]
pub struct MomentumStrategy {
    name: String,
    context: Option<StrategyContext>,
    state: TraitState,
}

impl MomentumStrategy {
    pub fn new(name: String) -> Self {
        Self {
            name: name.clone(),
            context: None,
            state: TraitState {
                name: name.clone(),
                status: StrategyStatus::Initializing,
                performance: StrategyPerformance::default(),
                positions: Vec::new(),
                last_signal: None,
                last_update: chrono::Utc::now().timestamp_micros() as u64,
            },
        }
    }
}

#[async_trait]
impl TradingStrategy for MomentumStrategy {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn initialize(&mut self, context: &StrategyContext) -> Result<()> {
        self.context = Some(context.clone());
        self.state.status = StrategyStatus::Ready;
        Ok(())
    }
    
    async fn on_orderbook_update(&mut self, _symbol: &str, _orderbook: &OrderBook) -> Result<TradingSignal> {
        // Momentum strategy logic here
        let signal = TradingSignal::Hold;
        self.state.last_signal = Some(signal.clone());
        self.state.last_update = chrono::Utc::now().timestamp_micros() as u64;
        Ok(signal)
    }
    
    async fn on_trade_execution(&mut self, _trade: &super::traits::TradeExecution) -> Result<()> {
        self.state.performance.total_trades += 1;
        Ok(())
    }
    
    fn get_state(&self) -> TraitState {
        self.state.clone()
    }
    
    async fn reset(&mut self) -> Result<()> {
        self.state.status = StrategyStatus::Ready;
        self.state.performance = StrategyPerformance::default();
        self.state.positions.clear();
        self.state.last_signal = None;
        Ok(())
    }
    
    fn is_ready(&self) -> bool {
        matches!(self.state.status, StrategyStatus::Ready | StrategyStatus::Active)
    }
}

/// Mean reversion strategy
#[derive(Debug)]
pub struct MeanReversionStrategy {
    name: String,
    context: Option<StrategyContext>,
    state: TraitState,
}

impl MeanReversionStrategy {
    pub fn new(name: String) -> Self {
        Self {
            name: name.clone(),
            context: None,
            state: TraitState {
                name: name.clone(),
                status: StrategyStatus::Initializing,
                performance: StrategyPerformance::default(),
                positions: Vec::new(),
                last_signal: None,
                last_update: chrono::Utc::now().timestamp_micros() as u64,
            },
        }
    }
}

#[async_trait]
impl TradingStrategy for MeanReversionStrategy {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn initialize(&mut self, context: &StrategyContext) -> Result<()> {
        self.context = Some(context.clone());
        self.state.status = StrategyStatus::Ready;
        Ok(())
    }
    
    async fn on_orderbook_update(&mut self, _symbol: &str, _orderbook: &OrderBook) -> Result<TradingSignal> {
        // Mean reversion strategy logic here
        let signal = TradingSignal::Hold;
        self.state.last_signal = Some(signal.clone());
        self.state.last_update = chrono::Utc::now().timestamp_micros() as u64;
        Ok(signal)
    }
    
    async fn on_trade_execution(&mut self, _trade: &super::traits::TradeExecution) -> Result<()> {
        self.state.performance.total_trades += 1;
        Ok(())
    }
    
    fn get_state(&self) -> TraitState {
        self.state.clone()
    }
    
    async fn reset(&mut self) -> Result<()> {
        self.state.status = StrategyStatus::Ready;
        self.state.performance = StrategyPerformance::default();
        self.state.positions.clear();
        self.state.last_signal = None;
        Ok(())
    }
    
    fn is_ready(&self) -> bool {
        matches!(self.state.status, StrategyStatus::Ready | StrategyStatus::Active)
    }
}