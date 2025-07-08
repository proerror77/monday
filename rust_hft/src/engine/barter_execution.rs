/*!
 * Barter-Integrated Order Execution Engine
 * 
 * Integrates with barter ecosystem for standardized order management
 * Provides enhanced execution algorithms and portfolio management
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::integrations::bitget_connector::{BitgetConnector, BitgetConfig, ReconnectStrategy};
use anyhow::Result;
use crossbeam_channel::Receiver;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, error, debug, warn};
// Barter imports - using available modules
use barter_instrument::{instrument::Instrument, Side as InstrumentSide};
use rust_decimal::prelude::ToPrimitive;

/// Barter集成的執行統計
#[derive(Debug, Clone, Default)]
pub struct BarterExecutionStats {
    pub signals_received: u64,
    pub orders_submitted: u64,
    pub orders_filled: u64,
    pub orders_cancelled: u64,
    pub orders_rejected: u64,
    pub avg_execution_latency_us: f64,
    pub total_volume: f64,
    pub total_pnl: f64,
    pub active_orders: usize,
    pub fill_rate: f64,
}

/// Barter集成執行引擎的主要入口點
pub async fn run_barter_execution(
    config: Config,
    signal_rx: Receiver<TradingSignal>,
) -> Result<()> {
    info!("🎯 Barter執行引擎啟動中...");
    
    let mut executor = BarterExecutionEngine::new(config).await?;
    
    info!("Barter執行引擎初始化完成，等待交易信號...");
    
    // 主執行循環
    while let Ok(signal) = signal_rx.recv() {
        let execution_start = now_micros();
        
        match executor.execute_signal(signal, execution_start).await {
            Ok(_) => {
                debug!("信號執行成功");
            },
            Err(e) => {
                error!("信號執行錯誤: {}", e);
                executor.stats.orders_rejected += 1;
            }
        }
    }
    
    info!("🎯 Barter執行引擎關閉中");
    Ok(())
}

/// Barter集成的訂單執行引擎
pub struct BarterExecutionEngine {
    /// 配置
    config: Config,
    
    /// 執行統計
    stats: BarterExecutionStats,
    
    /// 活躍訂單追蹤
    active_orders: Arc<Mutex<HashMap<String, SimpleOrder>>>,
    
    /// 帳戶餘額
    account_balance: Arc<Mutex<HashMap<String, f64>>>,
    
    /// Bitget連接器
    bitget_connector: BitgetConnector,
    
    /// 訂單ID計數器
    order_counter: u64,
}

/// 簡化的訂單結構
#[derive(Debug, Clone)]
pub struct SimpleOrder {
    pub id: String,
    pub symbol: String,
    pub side: InstrumentSide,
    pub quantity: f64,
    pub price: Option<f64>,
    pub status: SimpleOrderStatus,
    pub created_at: u64,
}

/// 簡化的訂單狀態
#[derive(Debug, Clone, PartialEq)]
pub enum SimpleOrderStatus {
    New,
    Pending,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

impl BarterExecutionEngine {
    /// 創建新的Barter執行引擎
    pub async fn new(config: Config) -> Result<Self> {
        // 創建Bitget連接器
        let bitget_config = BitgetConfig {
            public_ws_url: config.websocket_url().to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 3,
            reconnect_strategy: ReconnectStrategy::Standard,
        };
        
        let bitget_connector = BitgetConnector::new(bitget_config);
        
        // 初始化帳戶餘額
        let mut initial_balance = HashMap::new();
        initial_balance.insert("USDT".to_string(), 1000.0);
        initial_balance.insert("BTC".to_string(), 0.1);
        
        Ok(Self {
            config,
            stats: BarterExecutionStats::default(),
            active_orders: Arc::new(Mutex::new(HashMap::new())),
            account_balance: Arc::new(Mutex::new(initial_balance)),
            bitget_connector,
            order_counter: 0,
        })
    }
    
    /// 執行交易信號
    pub async fn execute_signal(
        &mut self,
        signal: TradingSignal,
        _execution_start: Timestamp,
    ) -> Result<()> {
        let start_time = now_micros();
        self.stats.signals_received += 1;
        
        // 檢查執行是否啟用
        if !self.config.execution_enabled() {
            info!("🔄 模擬模式 - 信號: {:?} @ {:.2} (數量: {})", 
                  signal.signal_type, signal.suggested_price, signal.suggested_quantity);
            return Ok(());
        }
        
        // 驗證信號和風險
        self.validate_signal(&signal).await?;
        
        // 創建訂單
        let order = self.create_order_from_signal(signal).await?;
        
        // 提交訂單
        match self.submit_order(order.clone()).await {
            Ok(order_id) => {
                info!("✅ 訂單已提交: {} {} {} @ {:.2}", 
                      order_id,
                      format!("{:?}", order.side).to_uppercase(),
                      order.quantity, 
                      order.price.unwrap_or(0.0));
                
                // 存儲活躍訂單
                let mut orders = self.active_orders.lock().await;
                orders.insert(order_id.clone(), order);
                
                self.stats.orders_submitted += 1;
                self.stats.active_orders = orders.len();
                
                // 異步監控訂單狀態
                self.monitor_order_async(order_id).await;
            },
            Err(e) => {
                error!("❌ 訂單提交失敗: {}", e);
                self.stats.orders_rejected += 1;
                return Err(e);
            }
        }
        
        let execution_latency = now_micros() - start_time;
        self.update_execution_stats(execution_latency);
        
        debug!("訂單執行延遲: {}μs", execution_latency);
        
        Ok(())
    }
    
    /// 驗證交易信號
    async fn validate_signal(&self, signal: &TradingSignal) -> Result<()> {
        // 檢查信心閾值
        if signal.confidence < self.config.ml.prediction_threshold {
            return Err(anyhow::anyhow!("信號信心度過低: {:.3} < {:.3}", 
                                     signal.confidence, self.config.ml.prediction_threshold));
        }
        
        // 檢查餘額
        let balance = self.account_balance.lock().await;
        let required_margin = signal.suggested_price.0 * signal.suggested_quantity.to_f64().unwrap_or(0.0);
        
        match signal.signal_type {
            SignalType::Buy => {
                let usdt_balance = balance.get("USDT").cloned().unwrap_or(0.0);
                if usdt_balance < required_margin {
                    return Err(anyhow::anyhow!("USDT餘額不足: 需要 {:.2}, 可用 {:.2}", 
                                             required_margin, usdt_balance));
                }
            },
            SignalType::Sell => {
                let btc_balance = balance.get("BTC").cloned().unwrap_or(0.0);
                let required_btc = signal.suggested_quantity.to_f64().unwrap_or(0.0);
                if btc_balance < required_btc {
                    return Err(anyhow::anyhow!("BTC餘額不足: 需要 {:.6}, 可用 {:.6}", 
                                             required_btc, btc_balance));
                }
            },
            SignalType::Hold => {
                return Err(anyhow::anyhow!("無法為Hold信號創建訂單"));
            }
        }
        
        // 檢查活躍訂單限制
        let active_orders = self.active_orders.lock().await;
        if active_orders.len() >= self.config.risk.max_order_rate as usize {
            return Err(anyhow::anyhow!("活躍訂單數量過多: {}", active_orders.len()));
        }
        
        Ok(())
    }
    
    /// 從交易信號創建訂單
    async fn create_order_from_signal(
        &mut self,
        signal: TradingSignal,
    ) -> Result<SimpleOrder> {
        self.order_counter += 1;
        let order_id = format!("barter_hft_{}", self.order_counter);
        
        let side = match signal.signal_type {
            SignalType::Buy => InstrumentSide::Buy,
            SignalType::Sell => InstrumentSide::Sell,
            SignalType::Hold => {
                return Err(anyhow::anyhow!("無法為Hold信號創建訂單"));
            }
        };
        
        let order = SimpleOrder {
            id: order_id,
            symbol: self.config.symbol().to_string(),
            side,
            quantity: signal.suggested_quantity.to_f64().unwrap_or(0.0),
            price: Some(signal.suggested_price.0),
            status: SimpleOrderStatus::New,
            created_at: now_micros(),
        };
        
        Ok(order)
    }
    
    /// 提交訂單
    async fn submit_order(&self, order: SimpleOrder) -> Result<String> {
        // 在這裡，我們模擬訂單提交
        // 在實際實現中，這會調用Bitget API
        
        info!("提交訂單到交易所: {:?}", order);
        
        // 模擬API調用延遲
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        
        // 模擬成功響應
        Ok(order.id)
    }
    
    /// 異步監控訂單狀態
    async fn monitor_order_async(&self, order_id: String) {
        let active_orders = self.active_orders.clone();
        
        tokio::spawn(async move {
            let mut check_count = 0;
            let max_checks = 30; // 最多檢查30次
            
            while check_count < max_checks {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                check_count += 1;
                
                // 模擬訂單狀態檢查
                let new_status = match check_count {
                    1..=5 => SimpleOrderStatus::Pending,
                    6..=25 => {
                        // 80%機率成交
                        if fastrand::f32() < 0.8 {
                            SimpleOrderStatus::Filled
                        } else {
                            SimpleOrderStatus::Pending
                        }
                    },
                    _ => SimpleOrderStatus::Cancelled, // 超時取消
                };
                
                // 更新訂單狀態
                let mut orders = active_orders.lock().await;
                if let Some(order) = orders.get_mut(&order_id) {
                    order.status = new_status.clone();
                    
                    match new_status {
                        SimpleOrderStatus::Filled => {
                            info!("✅ 訂單已成交: {}", order_id);
                            orders.remove(&order_id);
                            break;
                        },
                        SimpleOrderStatus::Cancelled => {
                            warn!("⚠️ 訂單已取消: {}", order_id);
                            orders.remove(&order_id);
                            break;
                        },
                        SimpleOrderStatus::Rejected => {
                            error!("❌ 訂單被拒絕: {}", order_id);
                            orders.remove(&order_id);
                            break;
                        },
                        _ => {
                            debug!("🔄 訂單狀態: {} - {:?}", order_id, new_status);
                        }
                    }
                }
            }
            
            if check_count >= max_checks {
                warn!("訂單監控超時: {}", order_id);
            }
        });
    }
    
    /// 更新執行統計
    fn update_execution_stats(&mut self, latency_us: u64) {
        if self.stats.orders_submitted == 0 {
            self.stats.avg_execution_latency_us = latency_us as f64;
        } else {
            let alpha = 0.1;
            self.stats.avg_execution_latency_us = 
                alpha * latency_us as f64 + (1.0 - alpha) * self.stats.avg_execution_latency_us;
        }
        
        // 計算成交率
        if self.stats.orders_submitted > 0 {
            self.stats.fill_rate = self.stats.orders_filled as f64 / self.stats.orders_submitted as f64;
        }
    }
    
    /// 取得執行統計
    pub fn get_stats(&self) -> BarterExecutionStats {
        self.stats.clone()
    }
    
    /// 取得活躍訂單數量
    pub async fn get_active_order_count(&self) -> usize {
        self.active_orders.lock().await.len()
    }
    
    /// 取得帳戶餘額
    pub async fn get_account_balance(&self) -> HashMap<String, f64> {
        self.account_balance.lock().await.clone()
    }
    
    /// 更新帳戶餘額（模擬成交後的餘額變化）
    pub async fn update_balance_after_fill(&self, order: &SimpleOrder) {
        let mut balance = self.account_balance.lock().await;
        
        match order.side {
            InstrumentSide::Buy => {
                // 買入：減少USDT，增加BTC
                let usdt_cost = order.quantity * order.price.unwrap_or(0.0);
                if let Some(usdt_balance) = balance.get_mut("USDT") {
                    *usdt_balance -= usdt_cost;
                }
                if let Some(btc_balance) = balance.get_mut("BTC") {
                    *btc_balance += order.quantity;
                }
                info!("餘額更新 - 買入 {:.6} BTC，花費 {:.2} USDT", order.quantity, usdt_cost);
            },
            InstrumentSide::Sell => {
                // 賣出：減少BTC，增加USDT
                let usdt_received = order.quantity * order.price.unwrap_or(0.0);
                if let Some(btc_balance) = balance.get_mut("BTC") {
                    *btc_balance -= order.quantity;
                }
                if let Some(usdt_balance) = balance.get_mut("USDT") {
                    *usdt_balance += usdt_received;
                }
                info!("餘額更新 - 賣出 {:.6} BTC，收到 {:.2} USDT", order.quantity, usdt_received);
            }
        }
    }
}

/// Portfolio管理器（使用barter概念）
pub struct BarterPortfolioManager {
    positions: HashMap<String, f64>,
    pnl: f64,
    total_volume: f64,
}

impl BarterPortfolioManager {
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
            pnl: 0.0,
            total_volume: 0.0,
        }
    }
    
    pub fn update_position(&mut self, symbol: &str, quantity: f64) {
        *self.positions.entry(symbol.to_string()).or_insert(0.0) += quantity;
    }
    
    pub fn get_position(&self, symbol: &str) -> f64 {
        self.positions.get(symbol).cloned().unwrap_or(0.0)
    }
    
    pub fn calculate_pnl(&mut self, fills: &[SimpleOrder]) -> f64 {
        // 簡化的PnL計算
        let mut total_pnl = 0.0;
        
        for fill in fills {
            match fill.side {
                InstrumentSide::Buy => {
                    total_pnl -= fill.quantity * fill.price.unwrap_or(0.0);
                },
                InstrumentSide::Sell => {
                    total_pnl += fill.quantity * fill.price.unwrap_or(0.0);
                }
            }
        }
        
        self.pnl = total_pnl;
        total_pnl
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_barter_execution_engine_creation() {
        let config = Config::default();
        let engine = BarterExecutionEngine::new(config).await;
        assert!(engine.is_ok());
    }
    
    #[tokio::test]
    async fn test_signal_validation() {
        let config = Config::default();
        let engine = BarterExecutionEngine::new(config).await.unwrap();
        
        let signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.8,
            suggested_price: 100.0.to_price(),
            suggested_quantity: 0.001.to_quantity(),
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 50,
        };
        
        let result = engine.validate_signal(&signal).await;
        assert!(result.is_ok());
    }
    
    #[tokio::test]
    async fn test_order_creation() {
        let config = Config::default();
        let mut engine = BarterExecutionEngine::new(config).await.unwrap();
        
        let signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.8,
            suggested_price: 100.0.to_price(),
            suggested_quantity: 0.001.to_quantity(),
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 50,
        };
        
        let order = engine.create_order_from_signal(signal).await;
        assert!(order.is_ok());
        
        let order = order.unwrap();
        assert_eq!(order.side, InstrumentSide::Buy);
        assert_eq!(order.quantity, 0.001);
    }
    
    #[test]
    fn test_portfolio_manager() {
        let mut portfolio = BarterPortfolioManager::new();
        
        portfolio.update_position("BTCUSDT", 0.1);
        assert_eq!(portfolio.get_position("BTCUSDT"), 0.1);
        
        portfolio.update_position("BTCUSDT", -0.05);
        assert_eq!(portfolio.get_position("BTCUSDT"), 0.05);
    }
}