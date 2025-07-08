/*!
 * Unified Barter Engine Integration
 * 
 * 統一的 barter 引擎實現，整合我們的 HFT 系統與 barter-rs 架構
 * 提供事件驅動的交易引擎，支持實盤交易和回測
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::engine::risk_manager::{BarterRiskManager, RiskCheckResult};
use crate::ml::features::FeatureExtractor;
use crate::integrations::bitget_connector::BitgetConnector;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, error, debug, warn};
use barter_instrument::Side as InstrumentSide;
use rust_decimal::prelude::ToPrimitive;
use uuid::Uuid;

/// Barter 統一引擎事件類型
#[derive(Debug, Clone)]
pub enum BarterEvent {
    /// 市場數據事件
    MarketData(MarketDataEvent),
    /// 交易信號事件
    TradingSignal(TradingSignal),
    /// 訂單事件
    Order(OrderEvent),
    /// 風險事件
    Risk(RiskEvent),
    /// 系統控制事件
    SystemControl(SystemControlEvent),
    /// 性能監控事件
    Performance(PerformanceEvent),
}

/// 市場數據事件
#[derive(Debug, Clone)]
pub struct MarketDataEvent {
    pub symbol: String,
    pub timestamp: u64,
    pub orderbook_update: OrderBookUpdate,
    pub features: Option<FeatureSet>,
}

/// 訂單事件
#[derive(Debug, Clone)]
pub struct OrderEvent {
    pub order_id: String,
    pub symbol: String,
    pub side: InstrumentSide,
    pub quantity: f64,
    pub price: Option<f64>,
    pub order_type: OrderType,
    pub status: OrderStatus,
    pub timestamp: u64,
}

/// 風險事件
#[derive(Debug, Clone)]
pub struct RiskEvent {
    pub event_type: RiskEventType,
    pub description: String,
    pub severity: RiskSeverity,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub enum RiskEventType {
    PositionLimit,
    DailyLoss,
    Drawdown,
    OrderValue,
    SystemRisk,
}

#[derive(Debug, Clone)]
pub enum RiskSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// 系統控制事件
#[derive(Debug, Clone)]
pub enum SystemControlEvent {
    StartTrading,
    StopTrading,
    EnableAlgorithm,
    DisableAlgorithm,
    CloseAllPositions,
    GetStatistics,
    Reset,
}

/// 性能監控事件
#[derive(Debug, Clone)]
pub struct PerformanceEvent {
    pub metric_type: String,
    pub value: f64,
    pub timestamp: u64,
    pub additional_data: HashMap<String, String>,
}

/// Barter 統一引擎狀態
#[derive(Debug, Clone)]
pub struct BarterEngineState {
    pub trading_enabled: bool,
    pub algorithm_enabled: bool,
    pub system_health: SystemHealth,
    pub total_orders: u64,
    pub active_positions: HashMap<String, f64>,
    pub total_pnl: f64,
    pub last_update: u64,
}

#[derive(Debug, Clone)]
pub enum SystemHealth {
    Healthy,
    Warning,
    Error,
    Critical,
}

impl Default for BarterEngineState {
    fn default() -> Self {
        Self {
            trading_enabled: false,
            algorithm_enabled: false,
            system_health: SystemHealth::Healthy,
            total_orders: 0,
            active_positions: HashMap::new(),
            total_pnl: 0.0,
            last_update: now_micros(),
        }
    }
}

/// Barter 統一引擎實現
pub struct BarterUnifiedEngine {
    /// 配置
    config: Config,
    
    /// 引擎狀態
    state: Arc<Mutex<BarterEngineState>>,
    
    /// 風險管理器
    risk_manager: Arc<Mutex<BarterRiskManager>>,
    
    /// 特徵提取器
    feature_extractor: Arc<Mutex<FeatureExtractor>>,
    
    /// 事件發送器
    event_sender: Sender<BarterEvent>,
    
    /// 事件接收器
    event_receiver: Receiver<BarterEvent>,
    
    /// 統計數據
    statistics: Arc<Mutex<EngineStatistics>>,
    
    /// 活躍訂單
    active_orders: Arc<Mutex<HashMap<String, OrderEvent>>>,
}

/// 引擎統計數據
#[derive(Debug, Clone, Default)]
pub struct EngineStatistics {
    pub events_processed: u64,
    pub orders_placed: u64,
    pub orders_filled: u64,
    pub orders_cancelled: u64,
    pub signals_generated: u64,
    pub risk_violations: u64,
    pub total_pnl: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub win_rate: f64,
    pub avg_processing_latency_us: f64,
}

impl BarterUnifiedEngine {
    /// 創建新的統一引擎
    pub fn new(config: Config) -> Result<Self> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded();
        let risk_manager = Arc::new(Mutex::new(BarterRiskManager::new(config.clone())));
        let feature_extractor = Arc::new(Mutex::new(FeatureExtractor::new(100)));
        
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(BarterEngineState::default())),
            risk_manager,
            feature_extractor,
            event_sender,
            event_receiver,
            statistics: Arc::new(Mutex::new(EngineStatistics::default())),
            active_orders: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    
    /// 啟動引擎
    pub async fn start(&mut self) -> Result<()> {
        info!("🚀 Barter統一引擎啟動中...");
        
        // 初始化組件
        self.initialize_components().await?;
        
        // 啟動主事件循環
        self.run_event_loop().await?;
        
        Ok(())
    }
    
    /// 初始化組件
    async fn initialize_components(&self) -> Result<()> {
        // 初始化風險管理器
        debug!("初始化風險管理器...");
        
        // 初始化特徵提取器
        debug!("初始化特徵提取器...");
        
        // 設置初始狀態
        let mut state = self.state.lock().await;
        state.last_update = now_micros();
        
        info!("✅ 所有組件初始化完成");
        Ok(())
    }
    
    /// 主事件循環
    async fn run_event_loop(&self) -> Result<()> {
        info!("開始主事件循環...");
        
        while let Ok(event) = self.event_receiver.recv() {
            let processing_start = now_micros();
            
            match self.process_event(event).await {
                Ok(_) => {
                    let mut stats = self.statistics.lock().await;
                    stats.events_processed += 1;
                    
                    let latency = now_micros() - processing_start;
                    self.update_processing_latency(&mut stats, latency);
                },
                Err(e) => {
                    error!("事件處理錯誤: {}", e);
                }
            }
        }
        
        info!("事件循環結束");
        Ok(())
    }
    
    /// 處理事件
    async fn process_event(&self, event: BarterEvent) -> Result<()> {
        debug!("處理事件: {:?}", std::mem::discriminant(&event));
        
        match event {
            BarterEvent::MarketData(market_data) => {
                self.handle_market_data(market_data).await?;
            },
            BarterEvent::TradingSignal(signal) => {
                self.handle_trading_signal(signal).await?;
            },
            BarterEvent::Order(order) => {
                self.handle_order_event(order).await?;
            },
            BarterEvent::Risk(risk) => {
                self.handle_risk_event(risk).await?;
            },
            BarterEvent::SystemControl(control) => {
                self.handle_system_control(control).await?;
            },
            BarterEvent::Performance(perf) => {
                self.handle_performance_event(perf).await?;
            },
        }
        
        Ok(())
    }
    
    /// 處理市場數據
    async fn handle_market_data(&self, market_data: MarketDataEvent) -> Result<()> {
        debug!("處理市場數據: {}", market_data.symbol);
        
        // 提取特徵
        if let Some(features) = market_data.features {
            // 生成交易信號
            let signal = self.generate_trading_signal(features).await?;
            
            if let Some(signal) = signal {
                // 發送交易信號事件
                self.event_sender.send(BarterEvent::TradingSignal(signal))?;
            }
        }
        
        Ok(())
    }
    
    /// 處理交易信號
    async fn handle_trading_signal(&self, signal: TradingSignal) -> Result<()> {
        debug!("處理交易信號: {:?}", signal.signal_type);
        
        // 風險檢查
        let risk_result = {
            let risk_manager = self.risk_manager.lock().await;
            risk_manager.check_signal_risk(&signal).await?
        };
        
        match risk_result {
            RiskCheckResult::Approved => {
                // 創建訂單事件
                let order_event = self.create_order_from_signal(signal).await?;
                self.event_sender.send(BarterEvent::Order(order_event))?;
                
                let mut stats = self.statistics.lock().await;
                stats.signals_generated += 1;
            },
            RiskCheckResult::Rejected(reason) => {
                warn!("交易信號被風險管理器拒絕: {}", reason);
                
                let risk_event = RiskEvent {
                    event_type: RiskEventType::SystemRisk,
                    description: reason,
                    severity: RiskSeverity::Medium,
                    timestamp: now_micros(),
                };
                
                self.event_sender.send(BarterEvent::Risk(risk_event))?;
                
                let mut stats = self.statistics.lock().await;
                stats.risk_violations += 1;
            },
            RiskCheckResult::Warning(warning) => {
                warn!("交易信號風險警告: {}", warning);
                
                // 仍然執行，但記錄警告
                let order_event = self.create_order_from_signal(signal).await?;
                self.event_sender.send(BarterEvent::Order(order_event))?;
                
                let mut stats = self.statistics.lock().await;
                stats.signals_generated += 1;
            }
        }
        
        Ok(())
    }
    
    /// 處理訂單事件
    async fn handle_order_event(&self, order: OrderEvent) -> Result<()> {
        debug!("處理訂單事件: {} - {:?}", order.order_id, order.status);
        
        // 更新活躍訂單
        let mut active_orders = self.active_orders.lock().await;
        
        match order.status {
            OrderStatus::New | OrderStatus::PartiallyFilled => {
                active_orders.insert(order.order_id.clone(), order);
            },
            OrderStatus::Filled => {
                active_orders.remove(&order.order_id);
                
                let mut stats = self.statistics.lock().await;
                stats.orders_filled += 1;
                
                // 更新倉位
                self.update_position(&order).await?;
            },
            OrderStatus::Cancelled => {
                active_orders.remove(&order.order_id);
                
                let mut stats = self.statistics.lock().await;
                stats.orders_cancelled += 1;
            },
            _ => {}
        }
        
        Ok(())
    }
    
    /// 處理風險事件
    async fn handle_risk_event(&self, risk: RiskEvent) -> Result<()> {
        match risk.severity {
            RiskSeverity::Critical => {
                error!("嚴重風險事件: {} - {}", 
                       format!("{:?}", risk.event_type), risk.description);
                
                // 停止交易
                self.event_sender.send(BarterEvent::SystemControl(
                    SystemControlEvent::StopTrading
                ))?;
            },
            RiskSeverity::High => {
                warn!("高風險事件: {} - {}", 
                      format!("{:?}", risk.event_type), risk.description);
            },
            _ => {
                debug!("風險事件: {} - {}", 
                       format!("{:?}", risk.event_type), risk.description);
            }
        }
        
        Ok(())
    }
    
    /// 處理系統控制事件
    async fn handle_system_control(&self, control: SystemControlEvent) -> Result<()> {
        debug!("處理系統控制: {:?}", control);
        
        let mut state = self.state.lock().await;
        
        match control {
            SystemControlEvent::StartTrading => {
                state.trading_enabled = true;
                info!("✅ 交易已啟用");
            },
            SystemControlEvent::StopTrading => {
                state.trading_enabled = false;
                info!("⏹️ 交易已停止");
            },
            SystemControlEvent::EnableAlgorithm => {
                state.algorithm_enabled = true;
                info!("🤖 算法交易已啟用");
            },
            SystemControlEvent::DisableAlgorithm => {
                state.algorithm_enabled = false;
                info!("🤖 算法交易已停用");
            },
            SystemControlEvent::CloseAllPositions => {
                info!("🔒 關閉所有倉位");
                self.close_all_positions().await?;
            },
            SystemControlEvent::GetStatistics => {
                let stats = self.statistics.lock().await;
                info!("📊 引擎統計: {:?}", *stats);
            },
            SystemControlEvent::Reset => {
                info!("🔄 重置引擎狀態");
                *state = BarterEngineState::default();
            },
        }
        
        state.last_update = now_micros();
        Ok(())
    }
    
    /// 處理性能事件
    async fn handle_performance_event(&self, perf: PerformanceEvent) -> Result<()> {
        debug!("性能指標: {} = {:.2}", perf.metric_type, perf.value);
        Ok(())
    }
    
    /// 生成交易信號
    async fn generate_trading_signal(&self, features: FeatureSet) -> Result<Option<TradingSignal>> {
        // 簡化的信號生成邏輯
        // 在實際實現中，這裡會使用 ML 模型
        
        let confidence = if features.obi_l10 > 0.05 {
            0.8
        } else if features.obi_l10 < -0.05 {
            0.7
        } else {
            0.3
        };
        
        if confidence > self.config.ml.prediction_threshold {
            let signal_type = if features.obi_l10 > 0.0 {
                SignalType::Buy
            } else {
                SignalType::Sell
            };
            
            let signal = TradingSignal {
                signal_type,
                confidence,
                suggested_price: features.mid_price,
                suggested_quantity: 0.001.to_quantity(),
                timestamp: now_micros(),
                features_timestamp: features.timestamp,
                signal_latency_us: now_micros() - features.timestamp,
            };
            
            Ok(Some(signal))
        } else {
            Ok(None)
        }
    }
    
    /// 從信號創建訂單
    async fn create_order_from_signal(&self, signal: TradingSignal) -> Result<OrderEvent> {
        let order_id = format!("barter_order_{}", Uuid::new_v4());
        let side = match signal.signal_type {
            SignalType::Buy => InstrumentSide::Buy,
            SignalType::Sell => InstrumentSide::Sell,
            SignalType::Hold => return Err(anyhow::anyhow!("無法為Hold信號創建訂單")),
        };
        
        let order = OrderEvent {
            order_id,
            symbol: self.config.symbol().to_string(),
            side,
            quantity: signal.suggested_quantity.to_f64().unwrap_or(0.0),
            price: Some(signal.suggested_price.0),
            order_type: OrderType::Limit,
            status: OrderStatus::New,
            timestamp: now_micros(),
        };
        
        Ok(order)
    }
    
    /// 更新倉位
    async fn update_position(&self, order: &OrderEvent) -> Result<()> {
        let mut state = self.state.lock().await;
        
        let position_change = match order.side {
            InstrumentSide::Buy => order.quantity,
            InstrumentSide::Sell => -order.quantity,
        };
        
        *state.active_positions.entry(order.symbol.clone()).or_insert(0.0) += position_change;
        
        // 更新風險管理器倉位
        let risk_manager = self.risk_manager.lock().await;
        risk_manager.update_position(&order.symbol, position_change).await;
        
        info!("倉位更新: {} {:.6}", order.symbol, position_change);
        Ok(())
    }
    
    /// 關閉所有倉位
    async fn close_all_positions(&self) -> Result<()> {
        let state = self.state.lock().await;
        
        for (symbol, &position) in &state.active_positions {
            if position.abs() > 1e-8 {
                let side = if position > 0.0 {
                    InstrumentSide::Sell
                } else {
                    InstrumentSide::Buy
                };
                
                let close_order = OrderEvent {
                    order_id: format!("close_{}", Uuid::new_v4()),
                    symbol: symbol.clone(),
                    side,
                    quantity: position.abs(),
                    price: None, // 市價單
                    order_type: OrderType::Market,
                    status: OrderStatus::New,
                    timestamp: now_micros(),
                };
                
                self.event_sender.send(BarterEvent::Order(close_order))?;
            }
        }
        
        Ok(())
    }
    
    /// 更新處理延遲統計
    fn update_processing_latency(&self, stats: &mut EngineStatistics, latency_us: u64) {
        if stats.events_processed == 1 {
            stats.avg_processing_latency_us = latency_us as f64;
        } else {
            let alpha = 0.1;
            stats.avg_processing_latency_us = 
                alpha * latency_us as f64 + (1.0 - alpha) * stats.avg_processing_latency_us;
        }
    }
    
    /// 發送事件
    pub fn send_event(&self, event: BarterEvent) -> Result<()> {
        self.event_sender.send(event)?;
        Ok(())
    }
    
    /// 獲取引擎狀態
    pub async fn get_state(&self) -> BarterEngineState {
        self.state.lock().await.clone()
    }
    
    /// 獲取統計數據
    pub async fn get_statistics(&self) -> EngineStatistics {
        self.statistics.lock().await.clone()
    }
    
    /// 獲取活躍訂單
    pub async fn get_active_orders(&self) -> HashMap<String, OrderEvent> {
        self.active_orders.lock().await.clone()
    }
}

/// Barter 引擎工廠
pub struct BarterEngineBuilder {
    config: Config,
}

impl BarterEngineBuilder {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
    
    pub fn build(self) -> Result<BarterUnifiedEngine> {
        BarterUnifiedEngine::new(self.config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_barter_engine_creation() {
        let config = Config::default();
        let engine = BarterUnifiedEngine::new(config);
        assert!(engine.is_ok());
    }
    
    #[tokio::test]
    async fn test_event_processing() {
        let config = Config::default();
        let engine = BarterUnifiedEngine::new(config).unwrap();
        
        let market_data = MarketDataEvent {
            symbol: "BTCUSDT".to_string(),
            timestamp: now_micros(),
            orderbook_update: OrderBookUpdate::default(),
            features: None,
        };
        
        let result = engine.process_event(BarterEvent::MarketData(market_data)).await;
        assert!(result.is_ok());
    }
    
    #[tokio::test]
    async fn test_system_control() {
        let config = Config::default();
        let engine = BarterUnifiedEngine::new(config).unwrap();
        
        let result = engine.handle_system_control(SystemControlEvent::StartTrading).await;
        assert!(result.is_ok());
        
        let state = engine.get_state().await;
        assert!(state.trading_enabled);
    }
}