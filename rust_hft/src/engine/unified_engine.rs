/*!
 * Unified Trading Engine
 * 
 * 統一的交易引擎架構，整合策略、執行、風險管理和回測功能
 * 支持單符號、多符號交易，以及實時和回測模式
 */

// use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use anyhow::{Result, Context};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};
use tracing::{info, warn, error, debug};

/// 統一引擎配置
#[derive(Debug, Clone)]
pub struct UnifiedEngineConfig {
    /// 引擎模式
    pub mode: EngineMode,
    
    /// 支持的交易符號
    pub symbols: Vec<String>,
    
    /// 風險管理配置
    pub risk_config: RiskConfig,
    
    /// 執行配置
    pub execution_config: ExecutionConfig,
    
    /// 性能配置
    pub performance_config: PerformanceConfig,
}

/// 引擎運行模式
#[derive(Debug, Clone)]
pub enum EngineMode {
    /// 實時交易模式
    Live {
        dry_run: bool,
        enable_paper_trading: bool,
    },
    
    /// 回測模式
    Backtest {
        start_time: u64,
        end_time: u64,
        initial_capital: f64,
    },
    
    /// 模擬模式（用於測試）
    Simulation {
        speed_multiplier: f64,
    },
}

/// 風險管理配置
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// 最大持倉比例
    pub max_position_ratio: f64,
    
    /// 單筆最大損失比例
    pub max_loss_per_trade: f64,
    
    /// 每日最大損失比例
    pub max_daily_loss: f64,
    
    /// 最大槓桿倍數
    pub max_leverage: f64,
    
    /// 是否啟用動態風險調整
    pub enable_dynamic_adjustment: bool,
    
    /// Kelly 準則參數
    pub kelly_fraction: Option<f64>,
}

/// 執行配置
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// 訂單類型偏好
    pub preferred_order_type: OrderType,
    
    /// 滑點容忍度（基點）
    pub slippage_tolerance_bps: f64,
    
    /// 訂單超時（毫秒）
    pub order_timeout_ms: u64,
    
    /// 是否啟用智能路由
    pub enable_smart_routing: bool,
    
    /// 是否啟用冰山訂單
    pub enable_iceberg: bool,
}

/// 性能配置
#[derive(Debug, Clone)]
pub struct PerformanceConfig {
    /// 是否啟用性能監控
    pub enable_metrics: bool,
    
    /// 指標更新間隔（秒）
    pub metrics_update_interval_secs: u64,
    
    /// 是否啟用延遲追蹤
    pub enable_latency_tracking: bool,
    
    /// 是否啟用內存優化
    pub enable_memory_optimization: bool,
}

/// 策略接口
#[async_trait]
pub trait TradingStrategy: Send + Sync {
    /// 策略名稱
    fn name(&self) -> &str;
    
    /// 初始化策略
    async fn initialize(&mut self, symbols: &[String]) -> Result<()>;
    
    /// 處理市場數據更新
    async fn on_market_data(&mut self, symbol: &str, data: &MarketData) -> Result<Vec<Signal>>;
    
    /// 處理訂單簿更新
    async fn on_orderbook(&mut self, symbol: &str, orderbook: &OrderBook) -> Result<Vec<Signal>>;
    
    /// 處理成交更新
    async fn on_trade(&mut self, symbol: &str, trade: &Trade) -> Result<Vec<Signal>>;
    
    /// 處理訂單狀態更新
    async fn on_order_update(&mut self, order: &Order) -> Result<()>;
    
    /// 獲取策略狀態
    async fn get_state(&self) -> StrategyState;
}

/// 風險管理器接口
#[async_trait]
pub trait RiskManager: Send + Sync {
    /// 檢查交易信號
    async fn check_signal(&self, signal: &Signal, portfolio: &Portfolio) -> Result<RiskDecision>;
    
    /// 檢查訂單
    async fn check_order(&self, order: &Order, portfolio: &Portfolio) -> Result<RiskDecision>;
    
    /// 更新風險指標
    async fn update_metrics(&mut self, portfolio: &Portfolio) -> Result<()>;
    
    /// 獲取風險報告
    async fn get_risk_report(&self) -> RiskReport;
}

/// 執行管理器接口
#[async_trait]
pub trait ExecutionManager: Send + Sync {
    /// 執行交易信號
    async fn execute_signal(&mut self, signal: Signal) -> Result<Order>;
    
    /// 取消訂單
    async fn cancel_order(&mut self, order_id: &str) -> Result<()>;
    
    /// 修改訂單
    async fn modify_order(&mut self, order_id: &str, new_params: OrderModification) -> Result<()>;
    
    /// 獲取訂單狀態
    async fn get_order_status(&self, order_id: &str) -> Result<OrderStatus>;
}

/// 市場數據
#[derive(Debug, Clone)]
pub struct MarketData {
    pub symbol: String,
    pub timestamp: u64,
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    pub volume: f64,
}

/// 交易信號
#[derive(Debug, Clone)]
pub struct Signal {
    pub id: String,
    pub symbol: String,
    pub action: SignalAction,
    pub quantity: f64,
    pub price: Option<f64>,
    pub timestamp: u64,
    pub confidence: f64,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// 信號動作
#[derive(Debug, Clone)]
pub enum SignalAction {
    Buy,
    Sell,
    Close,
    Adjust { target_position: f64 },
}

/// 訂單
#[derive(Debug, Clone)]
pub struct Order {
    pub id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
    pub status: OrderStatus,
    pub timestamp: u64,
    pub filled_quantity: f64,
    pub average_price: f64,
}

/// 訂單類型
#[derive(Debug, Clone)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
}

/// 訂單方向
#[derive(Debug, Clone)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// 訂單狀態
#[derive(Debug, Clone)]
pub enum OrderStatus {
    Pending,
    Submitted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// 投資組合
#[derive(Debug, Clone)]
pub struct Portfolio {
    pub cash: f64,
    pub positions: HashMap<String, Position>,
    pub total_value: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
}

/// 持倉
#[derive(Debug, Clone)]
pub struct Position {
    pub symbol: String,
    pub quantity: f64,
    pub average_price: f64,
    pub current_price: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
}

/// 風險決策
#[derive(Debug, Clone)]
pub enum RiskDecision {
    Approve,
    Reject { reason: String },
    Modify { adjustments: HashMap<String, f64> },
}

/// 風險報告
#[derive(Debug, Clone)]
pub struct RiskReport {
    pub timestamp: u64,
    pub var_95: f64,
    pub var_99: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub current_drawdown: f64,
    pub exposure: HashMap<String, f64>,
    pub risk_level: RiskLevel,
}

/// 風險級別
#[derive(Debug, Clone)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// 策略狀態
#[derive(Debug, Clone)]
pub struct StrategyState {
    pub name: String,
    pub is_active: bool,
    pub positions: HashMap<String, f64>,
    pub signals_generated: u64,
    pub last_update: u64,
}

/// 訂單修改參數
#[derive(Debug, Clone)]
pub struct OrderModification {
    pub new_quantity: Option<f64>,
    pub new_price: Option<f64>,
}

/// 成交記錄
#[derive(Debug, Clone)]
pub struct Trade {
    pub id: String,
    pub symbol: String,
    pub price: f64,
    pub quantity: f64,
    pub side: OrderSide,
    pub timestamp: u64,
}

/// 引擎事件
#[derive(Debug, Clone)]
pub enum EngineEvent {
    MarketData(MarketData),
    OrderUpdate(Order),
    Trade(Trade),
    Signal(Signal),
    RiskAlert(String),
    Error(String),
}

/// 統一交易引擎
pub struct UnifiedTradingEngine {
    config: UnifiedEngineConfig,
    strategy: Box<dyn TradingStrategy>,
    risk_manager: Box<dyn RiskManager>,
    execution_manager: Box<dyn ExecutionManager>,
    portfolio: Arc<RwLock<Portfolio>>,
    event_sender: mpsc::UnboundedSender<EngineEvent>,
    event_receiver: Arc<RwLock<mpsc::UnboundedReceiver<EngineEvent>>>,
    is_running: Arc<RwLock<bool>>,
    metrics: Arc<RwLock<EngineMetrics>>,
}

/// 引擎性能指標
#[derive(Debug, Default, Clone)]
struct EngineMetrics {
    pub signals_generated: u64,
    pub orders_submitted: u64,
    pub orders_filled: u64,
    pub orders_rejected: u64,
    pub total_volume: f64,
    pub latency_us: f64,
}

impl UnifiedTradingEngine {
    /// 創建新的統一引擎
    pub fn new(
        config: UnifiedEngineConfig,
        strategy: Box<dyn TradingStrategy>,
        risk_manager: Box<dyn RiskManager>,
        execution_manager: Box<dyn ExecutionManager>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        
        let initial_portfolio = Portfolio {
            cash: match &config.mode {
                EngineMode::Backtest { initial_capital, .. } => *initial_capital,
                _ => 100000.0, // 默認資金
            },
            positions: HashMap::new(),
            total_value: 0.0,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
        };
        
        Self {
            config,
            strategy,
            risk_manager,
            execution_manager,
            portfolio: Arc::new(RwLock::new(initial_portfolio)),
            event_sender: tx,
            event_receiver: Arc::new(RwLock::new(rx)),
            is_running: Arc::new(RwLock::new(false)),
            metrics: Arc::new(RwLock::new(EngineMetrics::default())),
        }
    }
    
    /// 啟動引擎
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting unified trading engine in {:?} mode", self.config.mode);
        
        *self.is_running.write().await = true;
        
        // 初始化策略
        self.strategy.initialize(&self.config.symbols).await?;
        
        // 啟動事件處理循環
        let engine = self.clone_internals();
        tokio::spawn(async move {
            if let Err(e) = engine.event_loop().await {
                error!("Event loop error: {}", e);
            }
        });
        
        // 啟動性能監控（如果啟用）
        if self.config.performance_config.enable_metrics {
            let engine = self.clone_internals();
            let interval = self.config.performance_config.metrics_update_interval_secs;
            tokio::spawn(async move {
                engine.metrics_loop(interval).await;
            });
        }
        
        Ok(())
    }
    
    /// 停止引擎
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping unified trading engine");
        *self.is_running.write().await = false;
        Ok(())
    }
    
    /// 處理市場數據
    pub async fn on_market_data(&mut self, data: MarketData) -> Result<()> {
        // 更新策略
        let signals = self.strategy.on_market_data(&data.symbol, &data).await?;
        
        // 處理生成的信號
        for signal in signals {
            self.process_signal(signal).await?;
        }
        
        // 發送事件
        self.event_sender.send(EngineEvent::MarketData(data))?;
        
        Ok(())
    }
    
    /// 處理訂單簿更新
    pub async fn on_orderbook(&mut self, symbol: &str, orderbook: &OrderBook) -> Result<()> {
        let signals = self.strategy.on_orderbook(symbol, orderbook).await?;
        
        for signal in signals {
            self.process_signal(signal).await?;
        }
        
        Ok(())
    }
    
    /// 獲取投資組合
    pub async fn get_portfolio(&self) -> Portfolio {
        self.portfolio.read().await.clone()
    }
    
    /// 獲取引擎指標
    pub async fn get_metrics(&self) -> EngineMetrics {
        let metrics_guard = self.metrics.read().await;
        metrics_guard.clone()
    }
    
    // --- 內部方法 ---
    
    async fn process_signal(&mut self, signal: Signal) -> Result<()> {
        let mut metrics = self.metrics.write().await;
        metrics.signals_generated += 1;
        
        // 風險檢查
        let portfolio = self.portfolio.read().await.clone();
        match self.risk_manager.check_signal(&signal, &portfolio).await? {
            RiskDecision::Approve => {
                // 執行信號
                match self.execution_manager.execute_signal(signal.clone()).await {
                    Ok(order) => {
                        metrics.orders_submitted += 1;
                        self.event_sender.send(EngineEvent::OrderUpdate(order))?;
                    }
                    Err(e) => {
                        error!("Execution failed: {}", e);
                        self.event_sender.send(EngineEvent::Error(e.to_string()))?;
                    }
                }
            }
            RiskDecision::Reject { reason } => {
                warn!("Signal rejected: {}", reason);
                metrics.orders_rejected += 1;
                self.event_sender.send(EngineEvent::RiskAlert(reason))?;
            }
            RiskDecision::Modify { adjustments } => {
                // TODO: 實現信號修改邏輯
                info!("Signal modified with adjustments: {:?}", adjustments);
            }
        }
        
        Ok(())
    }
    
    async fn event_loop(&self) -> Result<()> {
        let mut receiver = self.event_receiver.write().await;
        
        while *self.is_running.read().await {
            match receiver.recv().await {
                Some(event) => {
                    if let Err(e) = self.handle_event(event).await {
                        error!("Event handling error: {}", e);
                    }
                }
                None => {
                    warn!("Event channel closed");
                    break;
                }
            }
        }
        
        Ok(())
    }
    
    async fn handle_event(&self, event: EngineEvent) -> Result<()> {
        match event {
            EngineEvent::OrderUpdate(order) => {
                // 更新投資組合
                self.update_portfolio_from_order(&order).await?;
                
                // 更新指標
                if matches!(order.status, OrderStatus::Filled) {
                    let mut metrics = self.metrics.write().await;
                    metrics.orders_filled += 1;
                    metrics.total_volume += order.filled_quantity * order.average_price;
                }
            }
            EngineEvent::Trade(trade) => {
                // 處理成交
                debug!("Trade executed: {:?}", trade);
            }
            _ => {}
        }
        
        Ok(())
    }
    
    async fn update_portfolio_from_order(&self, order: &Order) -> Result<()> {
        if matches!(order.status, OrderStatus::Filled | OrderStatus::PartiallyFilled) {
            let mut portfolio = self.portfolio.write().await;
            
            // 計算現金變動和已實現盈虧
            let (cash_change, realized_change, should_remove_position) = {
                // 更新持倉
                let position = portfolio.positions.entry(order.symbol.clone())
                    .or_insert(Position {
                        symbol: order.symbol.clone(),
                        quantity: 0.0,
                        average_price: 0.0,
                        current_price: 0.0,
                        unrealized_pnl: 0.0,
                        realized_pnl: 0.0,
                    });
                
                let (cash_change, realized_change) = match order.side {
                    OrderSide::Buy => {
                        let total_cost = position.quantity * position.average_price + 
                                       order.filled_quantity * order.average_price;
                        position.quantity += order.filled_quantity;
                        position.average_price = total_cost / position.quantity;
                        (-(order.filled_quantity * order.average_price), 0.0)
                    }
                    OrderSide::Sell => {
                        position.quantity -= order.filled_quantity;
                        
                        // 計算已實現盈虧
                        let realized = order.filled_quantity * (order.average_price - position.average_price);
                        position.realized_pnl += realized;
                        (order.filled_quantity * order.average_price, realized)
                    }
                };
                
                let should_remove = position.quantity == 0.0;
                (cash_change, realized_change, should_remove)
            };
            
            // 更新portfolio的現金和已實現盈虧
            portfolio.cash += cash_change;
            portfolio.realized_pnl += realized_change;
            
            // 移除空倉位
            if should_remove_position {
                portfolio.positions.remove(&order.symbol);
            }
        }
        
        Ok(())
    }
    
    async fn metrics_loop(&self, interval_secs: u64) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
        
        while *self.is_running.read().await {
            interval.tick().await;
            
            // 更新風險指標
            let _portfolio = self.portfolio.read().await.clone();
            // TODO: 實現風險指標更新邏輯
            // if let Err(e) = self.risk_manager.update_metrics(&portfolio).await {
            //     error!("Failed to update risk metrics: {}", e);
            // }
            
            // 記錄性能指標
            let metrics_guard = self.metrics.read().await;
            let metrics = metrics_guard.clone();
            info!("Engine metrics: {:?}", metrics);
        }
    }
    
    fn clone_internals(&self) -> EngineInternals {
        EngineInternals {
            portfolio: self.portfolio.clone(),
            event_sender: self.event_sender.clone(),
            event_receiver: self.event_receiver.clone(),
            is_running: self.is_running.clone(),
            metrics: self.metrics.clone(),
            risk_manager: self.risk_manager.as_ref() as *const dyn RiskManager,
        }
    }
}

/// 引擎內部狀態（用於異步任務）
struct EngineInternals {
    portfolio: Arc<RwLock<Portfolio>>,
    event_sender: mpsc::UnboundedSender<EngineEvent>,
    event_receiver: Arc<RwLock<mpsc::UnboundedReceiver<EngineEvent>>>,
    is_running: Arc<RwLock<bool>>,
    metrics: Arc<RwLock<EngineMetrics>>,
    risk_manager: *const dyn RiskManager,
}

unsafe impl Send for EngineInternals {}
unsafe impl Sync for EngineInternals {}

impl EngineInternals {
    /// 事件處理循環
    async fn event_loop(&self) -> Result<()> {
        while *self.is_running.read().await {
            let mut receiver = self.event_receiver.write().await;
            
            match receiver.try_recv() {
                Ok(event) => {
                    if let Err(e) = self.handle_event(event).await {
                        error!("Error handling event: {}", e);
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    // 沒有事件，短暫休眠
                    tokio::time::sleep(Duration::from_micros(100)).await;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    error!("Event channel disconnected");
                    break;
                }
            }
        }
        Ok(())
    }
    
    /// 處理單個事件
    async fn handle_event(&self, event: EngineEvent) -> Result<()> {
        match event {
            EngineEvent::Signal(signal) => {
                debug!("Processing trading signal: {:?}", signal);
                // 這裡可以添加信號處理邏輯
            }
            EngineEvent::OrderUpdate(update) => {
                debug!("Processing order update: {:?}", update);
                // 這裡可以添加訂單更新處理邏輯
            }
            EngineEvent::MarketData(data) => {
                debug!("Processing market data update: {:?}", data);
                // 這裡可以添加市場數據更新處理邏輯
            }
            EngineEvent::RiskAlert(alert) => {
                warn!("Risk alert received: {:?}", alert);
                // 這裡可以添加風險警報處理邏輯
            }
            EngineEvent::Trade(trade) => {
                debug!("Processing trade: {:?}", trade);
                // TODO: 實現組合更新邏輯
            }
            EngineEvent::Error(error) => {
                error!("Engine error: {}", error);
                // 這裡可以添加錯誤處理邏輯
            }
        }
        Ok(())
    }
    
    /// 性能監控循環
    async fn metrics_loop(&self, interval_secs: u64) {
        let interval = Duration::from_secs(interval_secs);
        
        while *self.is_running.read().await {
            // 記錄指標
            let metrics = self.metrics.read().await;
            info!(
                "Engine metrics - Signals: {}, Orders: {}, Filled: {}, Volume: {:.2}, Latency: {:.2}μs",
                metrics.signals_generated,
                metrics.orders_submitted,
                metrics.orders_filled,
                metrics.total_volume,
                metrics.latency_us
            );
            
            tokio::time::sleep(interval).await;
        }
    }
}

/// 引擎構建器
pub struct UnifiedEngineBuilder {
    config: Option<UnifiedEngineConfig>,
    strategy: Option<Box<dyn TradingStrategy>>,
    risk_manager: Option<Box<dyn RiskManager>>,
    execution_manager: Option<Box<dyn ExecutionManager>>,
}

impl UnifiedEngineBuilder {
    pub fn new() -> Self {
        Self {
            config: None,
            strategy: None,
            risk_manager: None,
            execution_manager: None,
        }
    }
    
    pub fn with_config(mut self, config: UnifiedEngineConfig) -> Self {
        self.config = Some(config);
        self
    }
    
    pub fn with_strategy(mut self, strategy: Box<dyn TradingStrategy>) -> Self {
        self.strategy = Some(strategy);
        self
    }
    
    pub fn with_risk_manager(mut self, risk_manager: Box<dyn RiskManager>) -> Self {
        self.risk_manager = Some(risk_manager);
        self
    }
    
    pub fn with_execution_manager(mut self, execution_manager: Box<dyn ExecutionManager>) -> Self {
        self.execution_manager = Some(execution_manager);
        self
    }
    
    pub fn build(self) -> Result<UnifiedTradingEngine> {
        let config = self.config.context("Config not provided")?;
        let strategy = self.strategy.context("Strategy not provided")?;
        let risk_manager = self.risk_manager.context("Risk manager not provided")?;
        let execution_manager = self.execution_manager.context("Execution manager not provided")?;
        
        Ok(UnifiedTradingEngine::new(config, strategy, risk_manager, execution_manager))
    }
}