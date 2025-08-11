/*!
 * Unified Trading Engine - Core Implementation
 * 
 * 统一交易引擎的核心实现
 */

use super::{UnifiedEngineConfig, builder::{RiskManager, ExecutionManager}};
use crate::engine::strategy::TradingStrategy;
use crate::engine::strategies::traits::TradingSignal;
use crate::core::orderbook::OrderBook;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tracing::{info, debug};


/// 統一交易引擎
pub struct UnifiedTradingEngine {
    /// 引擎配置
    config: UnifiedEngineConfig,
    
    /// 交易策略
    strategy: Box<dyn TradingStrategy>,
    
    /// 風險管理器
    risk_manager: Box<dyn RiskManager>,
    
    /// 執行管理器
    execution_manager: Box<dyn ExecutionManager>,
    
    /// 當前持倉
    positions: Arc<RwLock<HashMap<String, Position>>>,
    
    /// 訂單簿數據
    orderbooks: Arc<RwLock<HashMap<String, OrderBook>>>,
    
    /// 引擎狀態
    state: Arc<RwLock<EngineState>>,
    
    /// 事件通道
    event_sender: mpsc::UnboundedSender<EngineEvent>,
    event_receiver: mpsc::UnboundedReceiver<EngineEvent>,
    
    /// 性能統計
    stats: Arc<RwLock<EngineStats>>,
}

/// 引擎狀態
#[derive(Debug, Clone)]
pub enum EngineState {
    Initializing,
    Running,
    Paused,
    Stopping,
    Stopped,
    Error(String),
}

/// 持倉信息
#[derive(Debug, Clone)]
pub struct Position {
    pub symbol: String,
    pub side: PositionSide,
    pub size: f64,
    pub average_price: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub last_update: u64,
}

/// 持倉方向
#[derive(Debug, Clone)]
pub enum PositionSide {
    Long,
    Short,
    Flat,
}

/// 引擎事件
#[derive(Debug, Clone)]
pub enum EngineEvent {
    OrderbookUpdate { symbol: String },
    PositionUpdate { symbol: String, position: Position },
    OrderExecuted { order_id: String, price: f64, quantity: f64 },
    RiskAlert { message: String },
    StrategySignal { symbol: String, signal: TradingSignal },
    EngineStateChange { old_state: EngineState, new_state: EngineState },
}


/// 引擎統計
#[derive(Debug, Clone, Default)]
pub struct EngineStats {
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub total_pnl: f64,
    pub max_drawdown: f64,
    pub avg_latency_us: f64,
    pub uptime_seconds: u64,
}

impl UnifiedTradingEngine {
    /// 創建新的統一交易引擎
    pub fn new(
        config: UnifiedEngineConfig,
        strategy: Box<dyn TradingStrategy>,
        risk_manager: Box<dyn RiskManager>,
        execution_manager: Box<dyn ExecutionManager>,
    ) -> Result<Self> {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        
        Ok(Self {
            config,
            strategy,
            risk_manager,
            execution_manager,
            positions: Arc::new(RwLock::new(HashMap::new())),
            orderbooks: Arc::new(RwLock::new(HashMap::new())),
            state: Arc::new(RwLock::new(EngineState::Initializing)),
            event_sender,
            event_receiver,
            stats: Arc::new(RwLock::new(EngineStats::default())),
        })
    }

    /// 啟動引擎
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting unified trading engine");
        
        // 設置狀態為運行中
        self.set_state(EngineState::Running).await;
        
        // 初始化持倉
        for symbol in &self.config.symbols {
            self.positions.write().await.insert(
                symbol.clone(), 
                Position::new_flat(symbol.clone())
            );
        }
        
        info!("Unified trading engine started successfully");
        Ok(())
    }

    /// 停止引擎
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping unified trading engine");
        
        self.set_state(EngineState::Stopping).await;
        
        // 平倉所有持倉
        self.close_all_positions().await?;
        
        self.set_state(EngineState::Stopped).await;
        
        info!("Unified trading engine stopped successfully");
        Ok(())
    }

    /// 處理訂單簿更新
    pub async fn on_orderbook_update(&mut self, symbol: String, orderbook: OrderBook) -> Result<()> {
        // 更新訂單簿
        self.orderbooks.write().await.insert(symbol.clone(), orderbook.clone());
        
        // 發送事件
        let _ = self.event_sender.send(EngineEvent::OrderbookUpdate { symbol: symbol.clone() });
        
        // 運行策略
        let strategy_signal = self.strategy.on_orderbook_update(&symbol, &orderbook).await?;
        
        // 處理策略信號（直接使用策略層信號型別）
        self.handle_strategy_signal(symbol, strategy_signal).await?;
        
        Ok(())
    }

    /// 處理策略信號
    async fn handle_strategy_signal(&mut self, symbol: String, signal: TradingSignal) -> Result<()> {
        debug!("Handling strategy signal for {}: {:?}", symbol, signal);
        
        // 發送信號事件
        let _ = self.event_sender.send(EngineEvent::StrategySignal { 
            symbol: symbol.clone(), 
            signal: signal.clone() 
        });
        
        match signal {
            TradingSignal::Buy { quantity, price, .. } => {
                self.execute_buy_order(symbol, quantity, price).await?;
            },
            TradingSignal::Sell { quantity, price, .. } => {
                self.execute_sell_order(symbol, quantity, price).await?;
            },
            TradingSignal::Hold => {
                // 不做任何操作
            },
            TradingSignal::Close { ratio } => {
                self.close_position(symbol, ratio).await?;
            },
            TradingSignal::CloseAll => {
                self.close_position(symbol, 1.0).await?;
            },
        }
        
        Ok(())
    }

    /// 執行買入訂單
    async fn execute_buy_order(&mut self, symbol: String, quantity: f64, price: Option<f64>) -> Result<()> {
        // 實現買入邏輯
        debug!("Executing buy order: {} {} @ {:?}", symbol, quantity, price);
        Ok(())
    }

    /// 執行賣出訂單
    async fn execute_sell_order(&mut self, symbol: String, quantity: f64, price: Option<f64>) -> Result<()> {
        // 實現賣出邏輯
        debug!("Executing sell order: {} {} @ {:?}", symbol, quantity, price);
        Ok(())
    }

    /// 平倉
    async fn close_position(&mut self, symbol: String, ratio: f64) -> Result<()> {
        debug!("Closing position for {}: {}%", symbol, ratio * 100.0);
        Ok(())
    }

    /// 平倉所有持倉
    async fn close_all_positions(&mut self) -> Result<()> {
        let symbols: Vec<String> = {
            let positions = self.positions.read().await;
            positions.keys().cloned().collect()
        };
        
        for symbol in symbols {
            self.close_position(symbol, 1.0).await?;
        }
        Ok(())
    }

    /// 設置引擎狀態
    async fn set_state(&self, new_state: EngineState) {
        let mut state = self.state.write().await;
        let old_state = state.clone();
        *state = new_state.clone();
        
        let _ = self.event_sender.send(EngineEvent::EngineStateChange { old_state, new_state });
    }

    /// 獲取當前狀態
    pub async fn get_state(&self) -> EngineState {
        self.state.read().await.clone()
    }

    /// 獲取統計信息
    pub async fn get_stats(&self) -> EngineStats {
        self.stats.read().await.clone()
    }
}

impl Position {
    /// 創建新的平倉持倉
    pub fn new_flat(symbol: String) -> Self {
        Self {
            symbol,
            side: PositionSide::Flat,
            size: 0.0,
            average_price: 0.0,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            last_update: crate::core::types::now_micros(),
        }
    }
}
