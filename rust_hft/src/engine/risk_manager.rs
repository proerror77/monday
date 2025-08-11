//! 風險管理系統
//!
//! 實時監控交易風險，實施風控措施，防止過度損失

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error, debug};
use serde::{Serialize, Deserialize};

use crate::core::types::*;
use crate::ml::TradingSignal;
use crate::exchanges::{OrderRequest, ExecutionReport};

/// 風險管理器
pub struct RiskManager {
    /// 風險限制配置
    limits: Arc<RwLock<RiskLimits>>,
    
    /// 當前風險狀態
    current_state: Arc<RwLock<RiskState>>,
    
    /// 持倉跟蹤
    positions: Arc<RwLock<HashMap<String, PositionRisk>>>, // symbol -> position
    
    /// 風險事件發送器
    risk_event_sender: mpsc::UnboundedSender<RiskEvent>,
    
    /// 配置
    config: RiskConfig,
    
    /// 統計信息
    stats: Arc<RwLock<RiskStats>>,
}

/// 風險限制
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskLimits {
    /// 最大總虧損（USD）
    pub max_total_loss: f64,
    
    /// 最大日虧損（USD）
    pub max_daily_loss: f64,
    
    /// 最大單筆虧損（USD）
    pub max_single_loss: f64,
    
    /// 最大持倉值（USD）
    pub max_position_value: f64,
    
    /// 單交易對最大持倉（USD）
    pub max_symbol_position: f64,
    
    /// 最大槓桿倍數
    pub max_leverage: f64,
    
    /// 最大回撤比例
    pub max_drawdown_pct: f64,
    
    /// 最大勝率要求
    pub min_win_rate: f64,
    
    /// 最大單日交易次數
    pub max_daily_trades: u32,
    
    /// 停損閾值（基點）
    pub stop_loss_bps: f64,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_total_loss: 10000.0,     // $10,000
            max_daily_loss: 2000.0,      // $2,000
            max_single_loss: 500.0,      // $500
            max_position_value: 50000.0, // $50,000
            max_symbol_position: 10000.0,// $10,000
            max_leverage: 3.0,           // 3倍槓桿
            max_drawdown_pct: 5.0,       // 5%回撤
            min_win_rate: 0.45,          // 45%勝率
            max_daily_trades: 1000,      // 1000次/天
            stop_loss_bps: 100.0,        // 100個基點止損
        }
    }
}

/// 當前風險狀態
#[derive(Debug, Clone, Default)]
pub struct RiskState {
    /// 總PnL
    pub total_pnl: f64,
    
    /// 日PnL
    pub daily_pnl: f64,
    
    /// 總持倉值
    pub total_position_value: f64,
    
    /// 當前回撤
    pub current_drawdown_pct: f64,
    
    /// 歷史最高PnL
    pub high_water_mark: f64,
    
    /// 當前勝率
    pub current_win_rate: f64,
    
    /// 今日交易次數
    pub daily_trades: u32,
    
    /// 風險級別
    pub risk_level: RiskLevel,
    
    /// 是否暫停交易
    pub trading_paused: bool,
    
    /// 暫停原因
    pub pause_reason: Option<String>,
}

/// 持倉風險
#[derive(Debug, Clone)]
pub struct PositionRisk {
    /// 交易對
    pub symbol: String,
    
    /// 持倉規模
    pub position_size: f64,
    
    /// 持倉價值
    pub position_value: f64,
    
    /// 平均入場價
    pub avg_entry_price: f64,
    
    /// 當前價格
    pub current_price: f64,
    
    /// 未實現PnL
    pub unrealized_pnl: f64,
    
    /// 已實現PnL  
    pub realized_pnl: f64,
    
    /// 最大虧損
    pub max_loss: f64,
    
    /// 止損價格
    pub stop_loss_price: Option<f64>,
    
    /// 最後更新時間
    pub last_update: u64,
}

/// 風險級別
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum RiskLevel {
    /// 低風險
    #[default]
    Low,
    
    /// 中等風險
    Medium,
    
    /// 高風險
    High,
    
    /// 危險
    Critical,
    
    /// 緊急停止
    Emergency,
}

/// 風險事件
#[derive(Debug, Clone)]
pub enum RiskEvent {
    /// 風險限制觸發
    LimitTriggered {
        limit_type: LimitType,
        current_value: f64,
        limit_value: f64,
        symbol: Option<String>,
    },
    
    /// 交易暫停
    TradingPaused {
        reason: String,
        timestamp: u64,
    },
    
    /// 交易恢復
    TradingResumed {
        timestamp: u64,
    },
    
    /// 強制平倉
    ForcedLiquidation {
        symbol: String,
        position_size: f64,
        reason: String,
        timestamp: u64,
    },
    
    /// 風險級別變化
    RiskLevelChanged {
        from: RiskLevel,
        to: RiskLevel,
        reason: String,
    },
    
    /// 止損觸發
    StopLossTriggered {
        symbol: String,
        trigger_price: f64,
        position_size: f64,
    },
}

/// 限制類型
#[derive(Debug, Clone)]
pub enum LimitType {
    TotalLoss,
    DailyLoss,
    SingleLoss,
    PositionValue,
    SymbolPosition,
    Leverage,
    Drawdown,
    WinRate,
    DailyTrades,
}

/// 風險配置
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// 風險檢查間隔（毫秒）
    pub check_interval_ms: u64,
    
    /// PnL更新間隔（毫秒）
    pub pnl_update_interval_ms: u64,
    
    /// 是否啟用自動止損
    pub enable_auto_stop_loss: bool,
    
    /// 是否啟用強制平倉
    pub enable_force_liquidation: bool,
    
    /// 風險報告間隔（分鐘）
    pub risk_report_interval_min: u64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            check_interval_ms: 1000,        // 1秒檢查一次
            pnl_update_interval_ms: 5000,   // 5秒更新PnL
            enable_auto_stop_loss: true,
            enable_force_liquidation: true,
            risk_report_interval_min: 5,    // 5分鐘報告一次
        }
    }
}

/// 風險統計
#[derive(Debug, Clone, Default)]
pub struct RiskStats {
    /// 風險檢查次數
    pub risk_checks: u64,
    
    /// 限制觸發次數
    pub limits_triggered: u64,
    
    /// 交易暫停次數
    pub trading_pauses: u64,
    
    /// 強制平倉次數
    pub forced_liquidations: u64,
    
    /// 止損觸發次數
    pub stop_loss_triggers: u64,
    
    /// 最大回撤記錄
    pub max_drawdown_recorded: f64,
    
    /// 風險調整次數
    pub risk_adjustments: u64,
}

impl RiskManager {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<RiskEvent>) {
        let (risk_event_sender, risk_event_receiver) = mpsc::unbounded_channel();
        
        let manager = Self {
            limits: Arc::new(RwLock::new(RiskLimits::default())),
            current_state: Arc::new(RwLock::new(RiskState::default())),
            positions: Arc::new(RwLock::new(HashMap::new())),
            risk_event_sender,
            config: RiskConfig::default(),
            stats: Arc::new(RwLock::new(RiskStats::default())),
        };
        
        (manager, risk_event_receiver)
    }
    
    pub fn with_config(limits: RiskLimits, config: RiskConfig) -> (Self, mpsc::UnboundedReceiver<RiskEvent>) {
        let (mut manager, receiver) = Self::new();
        *manager.limits.blocking_write() = limits;
        manager.config = config;
        (manager, receiver)
    }
    
    /// 啟動風險管理器
    pub async fn start(&self) -> Result<(), String> {
        info!("Starting Risk Manager");
        
        // 啟動風險檢查任務
        self.start_risk_check_task().await;
        
        // 啟動PnL更新任務
        self.start_pnl_update_task().await;
        
        // 啟動風險報告任務
        self.start_risk_report_task().await;
        
        info!("Risk Manager started successfully");
        Ok(())
    }
    
    /// 檢查訂單風險
    pub async fn check_order_risk(&self, order: &OrderRequest, signal: &TradingSignal) -> Result<OrderRiskDecision, String> {
        // 檢查是否暫停交易
        {
            let state = self.current_state.read().await;
            if state.trading_paused {
                return Ok(OrderRiskDecision::Reject { 
                    reason: state.pause_reason.clone().unwrap_or_else(|| "Trading is paused".to_string()) 
                });
            }
        }
        
        // 計算訂單風險
        let order_value = order.quantity * order.price.unwrap_or(0.0);
        let potential_loss = order_value * (self.limits.read().await.stop_loss_bps / 10000.0);
        
        // 檢查各項風險限制
        let risk_checks = vec![
            self.check_single_loss_limit(potential_loss).await,
            self.check_position_limit(&order.symbol, order_value).await,
            self.check_daily_trades_limit().await,
            self.check_signal_quality(signal).await,
        ];
        
        // 如果任何檢查失敗，拒絕訂單
        for check in risk_checks {
            if let OrderRiskDecision::Reject { reason } = check {
                return Ok(OrderRiskDecision::Reject { reason });
            }
        }
        
        // 計算建議的頭寸大小
        let suggested_size = self.calculate_suggested_position_size(order, signal).await?;
        
        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.risk_checks += 1;
        }
        
        Ok(OrderRiskDecision::Approve { 
            original_size: order.quantity,
            suggested_size,
            stop_loss_price: self.calculate_stop_loss_price(order).await,
        })
    }
    
    /// 處理執行回報
    pub async fn handle_execution_report(&self, report: &ExecutionReport) -> Result<(), String> {
        // 更新持倉
        self.update_position_from_execution(report).await?;
        
        // 更新PnL
        self.update_pnl().await?;
        
        // 檢查風險狀態
        self.evaluate_risk_level().await?;
        
        Ok(())
    }
    
    /// 更新市場價格
    pub async fn update_market_price(&self, symbol: &str, price: f64) -> Result<(), String> {
        let mut positions = self.positions.write().await;
        
        if let Some(position) = positions.get_mut(symbol) {
            let old_price = position.current_price;
            position.current_price = price;
            position.last_update = chrono::Utc::now().timestamp_nanos() as u64;
            
            // 更新未實現PnL
            position.unrealized_pnl = (price - position.avg_entry_price) * position.position_size;
            
            // 檢查止損
            if let Some(stop_loss_price) = position.stop_loss_price {
                if self.should_trigger_stop_loss(position, price, stop_loss_price) {
                    self.trigger_stop_loss(symbol, position.clone()).await?;
                }
            }
            
            debug!("Updated price for {}: {} -> {}, unrealized PnL: {:.2}", 
                   symbol, old_price, price, position.unrealized_pnl);
        }
        
        Ok(())
    }
    
    /// 獲取當前風險狀態
    pub async fn get_risk_state(&self) -> RiskState {
        self.current_state.read().await.clone()
    }
    
    /// 獲取風險統計
    pub async fn get_stats(&self) -> RiskStats {
        self.stats.read().await.clone()
    }
    
    /// 暫停交易
    pub async fn pause_trading(&self, reason: String) -> Result<(), String> {
        let mut state = self.current_state.write().await;
        state.trading_paused = true;
        state.pause_reason = Some(reason.clone());
        
        let _ = self.risk_event_sender.send(RiskEvent::TradingPaused {
            reason,
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
        });
        
        warn!("Trading paused: {}", state.pause_reason.as_ref().unwrap());
        Ok(())
    }
    
    /// 恢復交易
    pub async fn resume_trading(&self) -> Result<(), String> {
        let mut state = self.current_state.write().await;
        state.trading_paused = false;
        state.pause_reason = None;
        
        let _ = self.risk_event_sender.send(RiskEvent::TradingResumed {
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
        });
        
        info!("Trading resumed");
        Ok(())
    }
    
    /// 檢查單筆虧損限制
    async fn check_single_loss_limit(&self, potential_loss: f64) -> OrderRiskDecision {
        let limits = self.limits.read().await;
        if potential_loss > limits.max_single_loss {
            OrderRiskDecision::Reject {
                reason: format!("Potential loss ${:.2} exceeds single loss limit ${:.2}", 
                               potential_loss, limits.max_single_loss)
            }
        } else {
            OrderRiskDecision::Approve {
                original_size: 0.0,
                suggested_size: 0.0,
                stop_loss_price: None,
            }
        }
    }
    
    /// 檢查持倉限制
    async fn check_position_limit(&self, symbol: &str, order_value: f64) -> OrderRiskDecision {
        let limits = self.limits.read().await;
        let positions = self.positions.read().await;
        
        // 檢查單交易對持倉限制
        if let Some(position) = positions.get(symbol) {
            let new_position_value = position.position_value.abs() + order_value;
            if new_position_value > limits.max_symbol_position {
                return OrderRiskDecision::Reject {
                    reason: format!("New position value ${:.2} for {} exceeds symbol limit ${:.2}",
                                   new_position_value, symbol, limits.max_symbol_position)
                };
            }
        }
        
        // 檢查總持倉限制
        let state = self.current_state.read().await;
        let new_total_value = state.total_position_value + order_value;
        if new_total_value > limits.max_position_value {
            OrderRiskDecision::Reject {
                reason: format!("New total position value ${:.2} exceeds limit ${:.2}",
                               new_total_value, limits.max_position_value)
            }
        } else {
            OrderRiskDecision::Approve {
                original_size: 0.0,
                suggested_size: 0.0,
                stop_loss_price: None,
            }
        }
    }
    
    /// 檢查日交易次數限制
    async fn check_daily_trades_limit(&self) -> OrderRiskDecision {
        let limits = self.limits.read().await;
        let state = self.current_state.read().await;
        
        if state.daily_trades >= limits.max_daily_trades {
            OrderRiskDecision::Reject {
                reason: format!("Daily trades {} exceeds limit {}", 
                               state.daily_trades, limits.max_daily_trades)
            }
        } else {
            OrderRiskDecision::Approve {
                original_size: 0.0,
                suggested_size: 0.0,
                stop_loss_price: None,
            }
        }
    }
    
    /// 檢查信號質量
    async fn check_signal_quality(&self, signal: &TradingSignal) -> OrderRiskDecision {
        // 檢查信號置信度
        if signal.confidence < 0.5 {
            return OrderRiskDecision::Reject {
                reason: format!("Signal confidence {:.2} too low", signal.confidence)
            };
        }
        
        // 檢查信號強度
        if signal.strength.abs() < 0.3 {
            return OrderRiskDecision::Reject {
                reason: format!("Signal strength {:.2} too weak", signal.strength)
            };
        }
        
        OrderRiskDecision::Approve {
            original_size: 0.0,
            suggested_size: 0.0,
            stop_loss_price: None,
        }
    }
    
    /// 計算建議頭寸規模
    async fn calculate_suggested_position_size(&self, order: &OrderRequest, signal: &TradingSignal) -> Result<f64, String> {
        // 基於Kelly公式和風險調整的頭寸規模計算
        let base_size = order.quantity;
        let confidence_adjustment = signal.confidence;
        let strength_adjustment = signal.strength.abs();
        
        // 風險調整因子
        let risk_adjustment = match self.current_state.read().await.risk_level {
            RiskLevel::Low => 1.0,
            RiskLevel::Medium => 0.8,
            RiskLevel::High => 0.6,
            RiskLevel::Critical => 0.3,
            RiskLevel::Emergency => 0.1,
        };
        
        let suggested_size = base_size * confidence_adjustment * strength_adjustment * risk_adjustment;
        Ok(suggested_size.max(0.001)) // 最小頭寸規模
    }
    
    /// 計算止損價格
    async fn calculate_stop_loss_price(&self, order: &OrderRequest) -> Option<f64> {
        if let Some(entry_price) = order.price {
            let limits = self.limits.read().await;
            let stop_loss_pct = limits.stop_loss_bps / 10000.0;
            
            let stop_price = match order.side {
                OrderSide::Buy => entry_price * (1.0 - stop_loss_pct),
                OrderSide::Sell => entry_price * (1.0 + stop_loss_pct),
            };
            
            Some(stop_price)
        } else {
            None
        }
    }
    
    /// 從執行回報更新持倉
    async fn update_position_from_execution(&self, report: &ExecutionReport) -> Result<(), String> {
        let mut positions = self.positions.write().await;
        
        let position = positions.entry(report.symbol.clone()).or_insert_with(|| {
            PositionRisk {
                symbol: report.symbol.clone(),
                position_size: 0.0,
                position_value: 0.0,
                avg_entry_price: 0.0,
                current_price: report.avg_price,
                unrealized_pnl: 0.0,
                realized_pnl: 0.0,
                max_loss: 0.0,
                stop_loss_price: None,
                last_update: chrono::Utc::now().timestamp_nanos() as u64,
            }
        });
        
        // 更新持倉規模和平均價格
        let old_size = position.position_size;
        let executed_qty = match report.side {
            OrderSide::Buy => report.executed_quantity,
            OrderSide::Sell => -report.executed_quantity,
        };
        
        let new_size = old_size + executed_qty;
        
        if new_size != 0.0 {
            // 更新平均入場價
            position.avg_entry_price = ((position.avg_entry_price * old_size.abs()) + 
                                       (report.avg_price * executed_qty.abs())) / new_size.abs();
        }
        
        position.position_size = new_size;
        position.position_value = new_size * report.avg_price;
        position.current_price = report.avg_price;
        position.last_update = chrono::Utc::now().timestamp_nanos() as u64;
        
        // 更新已實現PnL
        if old_size * executed_qty < 0.0 {
            // 部分或全部平倉
            let closed_qty = executed_qty.abs().min(old_size.abs());
            let realized_pnl = (report.avg_price - position.avg_entry_price) * closed_qty;
            position.realized_pnl += realized_pnl;
        }
        
        debug!("Updated position for {}: size {:.6} -> {:.6}, avg_price: {:.2}", 
               report.symbol, old_size, new_size, position.avg_entry_price);
        
        Ok(())
    }
    
    /// 更新PnL
    async fn update_pnl(&self) -> Result<(), String> {
        let positions = self.positions.read().await;
        
        let mut total_unrealized = 0.0;
        let mut total_realized = 0.0;
        let mut total_position_value = 0.0;
        
        for position in positions.values() {
            total_unrealized += position.unrealized_pnl;
            total_realized += position.realized_pnl;
            total_position_value += position.position_value.abs();
        }
        
        let mut state = self.current_state.write().await;
        state.total_pnl = total_unrealized + total_realized;
        state.total_position_value = total_position_value;
        
        // 更新高水位線和回撤
        if state.total_pnl > state.high_water_mark {
            state.high_water_mark = state.total_pnl;
        }
        
        if state.high_water_mark > 0.0 {
            state.current_drawdown_pct = ((state.high_water_mark - state.total_pnl) / state.high_water_mark) * 100.0;
        }
        
        debug!("Updated PnL: total {:.2}, unrealized {:.2}, realized {:.2}, drawdown {:.2}%", 
               state.total_pnl, total_unrealized, total_realized, state.current_drawdown_pct);
        
        Ok(())
    }
    
    /// 評估風險級別
    async fn evaluate_risk_level(&self) -> Result<(), String> {
        let state = self.current_state.read().await.clone();
        let limits = self.limits.read().await;
        
        let old_level = state.risk_level.clone();
        let new_level = if state.current_drawdown_pct >= limits.max_drawdown_pct {
            RiskLevel::Emergency
        } else if state.current_drawdown_pct >= limits.max_drawdown_pct * 0.8 {
            RiskLevel::Critical
        } else if state.daily_pnl <= -limits.max_daily_loss * 0.8 {
            RiskLevel::High
        } else if state.daily_pnl <= -limits.max_daily_loss * 0.5 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };
        
        if new_level != old_level {
            // 更新風險級別
            {
                let mut state_mut = self.current_state.write().await;
                state_mut.risk_level = new_level.clone();
            }
            
            // 發送風險級別變化事件
            let _ = self.risk_event_sender.send(RiskEvent::RiskLevelChanged {
                from: old_level,
                to: new_level.clone(),
                reason: format!("Drawdown: {:.2}%, Daily PnL: {:.2}", 
                               state.current_drawdown_pct, state.daily_pnl),
            });
            
            // 檢查是否需要緊急行動
            if new_level == RiskLevel::Emergency {
                self.pause_trading("Emergency risk level triggered".to_string()).await?;
            }
            
            info!("Risk level changed to: {:?}", new_level);
        }
        
        Ok(())
    }
    
    /// 檢查是否觸發止損
    fn should_trigger_stop_loss(&self, position: &PositionRisk, current_price: f64, stop_loss_price: f64) -> bool {
        match position.position_size > 0.0 {
            true => current_price <= stop_loss_price,  // 多頭止損
            false => current_price >= stop_loss_price, // 空頭止損
        }
    }
    
    /// 觸發止損
    async fn trigger_stop_loss(&self, symbol: &str, position: PositionRisk) -> Result<(), String> {
        let _ = self.risk_event_sender.send(RiskEvent::StopLossTriggered {
            symbol: symbol.to_string(),
            trigger_price: position.current_price,
            position_size: position.position_size,
        });
        
        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.stop_loss_triggers += 1;
        }
        
        warn!("Stop loss triggered for {}: price {:.2}, position {:.6}", 
              symbol, position.current_price, position.position_size);
        
        Ok(())
    }
    
    /// 啟動風險檢查任務
    async fn start_risk_check_task(&self) {
        let current_state = self.current_state.clone();
        let limits = self.limits.clone();
        let risk_event_sender = self.risk_event_sender.clone();
        let stats = self.stats.clone();
        let check_interval = self.config.check_interval_ms;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(check_interval));
            
            loop {
                interval.tick().await;
                
                let state = current_state.read().await;
                let limits_guard = limits.read().await;
                
                // 檢查各項風險限制
                if state.total_pnl <= -limits_guard.max_total_loss {
                    let _ = risk_event_sender.send(RiskEvent::LimitTriggered {
                        limit_type: LimitType::TotalLoss,
                        current_value: state.total_pnl,
                        limit_value: -limits_guard.max_total_loss,
                        symbol: None,
                    });
                }
                
                if state.daily_pnl <= -limits_guard.max_daily_loss {
                    let _ = risk_event_sender.send(RiskEvent::LimitTriggered {
                        limit_type: LimitType::DailyLoss,
                        current_value: state.daily_pnl,
                        limit_value: -limits_guard.max_daily_loss,
                        symbol: None,
                    });
                }
                
                // 更新統計
                {
                    let mut stats_guard = stats.write().await;
                    stats_guard.risk_checks += 1;
                }
                
                debug!("Risk check completed");
            }
        });
    }
    
    /// 啟動PnL更新任務
    async fn start_pnl_update_task(&self) {
        let positions = self.positions.clone();
        let update_interval = self.config.pnl_update_interval_ms;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(update_interval));
            
            loop {
                interval.tick().await;
                
                // TODO: 定期從市場數據更新持倉的當前價格
                // 這裡應該訂閱市場數據並更新價格
                debug!("PnL update task tick");
            }
        });
    }
    
    /// 啟動風險報告任務
    async fn start_risk_report_task(&self) {
        let current_state = self.current_state.clone();
        let stats = self.stats.clone();
        let report_interval = self.config.risk_report_interval_min;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(report_interval * 60));
            
            loop {
                interval.tick().await;
                
                let state = current_state.read().await;
                let stats_guard = stats.read().await;
                
                info!("=== Risk Report ===");
                info!("Total PnL: ${:.2}", state.total_pnl);
                info!("Daily PnL: ${:.2}", state.daily_pnl);
                info!("Current Drawdown: {:.2}%", state.current_drawdown_pct);
                info!("Risk Level: {:?}", state.risk_level);
                info!("Daily Trades: {}", state.daily_trades);
                info!("Trading Paused: {}", state.trading_paused);
                info!("Risk Checks: {}", stats_guard.risk_checks);
                info!("Limits Triggered: {}", stats_guard.limits_triggered);
                info!("==================");
            }
        });
    }
}

/// 訂單風險決策
#[derive(Debug, Clone)]
pub enum OrderRiskDecision {
    /// 批准訂單
    Approve {
        original_size: f64,
        suggested_size: f64,
        stop_loss_price: Option<f64>,
    },
    
    /// 拒絕訂單
    Reject {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::{SignalAction, SignalType, ModelInfo};
    
    #[tokio::test]
    async fn test_risk_manager_creation() {
        let (manager, _receiver) = RiskManager::new();
        let state = manager.get_risk_state().await;
        assert_eq!(state.risk_level, RiskLevel::Low);
        assert!(!state.trading_paused);
    }
    
    #[tokio::test]
    async fn test_order_risk_check() {
        let (manager, _receiver) = RiskManager::new();
        
        let order = OrderRequest {
            client_order_id: "test_1".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 0.01,
            price: Some(50000.0),
            stop_price: None,
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: HashMap::new(),
        };
        
        let signal = TradingSignal {
            symbol: "BTCUSDT".to_string(),
            signal_type: SignalType::Directional,
            strength: 0.8,
            confidence: 0.9,
            action: SignalAction::Buy,
            target_price: None,
            stop_loss: None,
            position_size: Some(0.01),
            validity_ms: 5000,
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            model_info: ModelInfo {
                name: "test_model".to_string(),
                version: "1.0".to_string(),
                inference_latency_us: 1000,
            },
        };
        
        let decision = manager.check_order_risk(&order, &signal).await.unwrap();
        
        match decision {
            OrderRiskDecision::Approve { .. } => {
                // 測試通過
            },
            OrderRiskDecision::Reject { reason } => {
                panic!("Order should be approved but was rejected: {}", reason);
            }
        }
    }
}