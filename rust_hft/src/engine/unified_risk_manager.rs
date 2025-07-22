/*!
 * Unified Risk Manager
 * 
 * 統一的風險管理實現，整合基礎和高級風險控制功能
 */

use crate::engine::unified_engine::{
    RiskManager, RiskDecision, RiskReport, RiskLevel, Signal, Order, Portfolio
};
use async_trait::async_trait;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 統一風險管理器
pub struct UnifiedRiskManager {
    config: RiskManagerConfig,
    metrics: Arc<RwLock<RiskMetrics>>,
    limits: Arc<RwLock<RiskLimits>>,
}

/// 風險管理配置
#[derive(Debug, Clone)]
pub struct RiskManagerConfig {
    /// 最大持倉比例（相對於總資金）
    pub max_position_ratio: f64,
    
    /// 單筆最大損失比例
    pub max_loss_per_trade: f64,
    
    /// 每日最大損失比例
    pub max_daily_loss: f64,
    
    /// 最大槓桿
    pub max_leverage: f64,
    
    /// 最大持倉數量
    pub max_positions: usize,
    
    /// 最小持倉間隔時間（秒）
    pub min_trade_interval_secs: u64,
    
    /// 是否啟用 Kelly 準則
    pub enable_kelly_criterion: bool,
    
    /// Kelly 準則縮放因子（0-1）
    pub kelly_fraction: f64,
    
    /// VaR 計算置信度
    pub var_confidence: f64,
    
    /// 是否啟用動態調整
    pub enable_dynamic_adjustment: bool,
}

impl Default for RiskManagerConfig {
    fn default() -> Self {
        Self {
            max_position_ratio: 0.1,  // 單個持倉最多10%
            max_loss_per_trade: 0.02, // 單筆最大損失2%
            max_daily_loss: 0.05,     // 每日最大損失5%
            max_leverage: 1.0,        // 無槓桿
            max_positions: 10,        // 最多10個持倉
            min_trade_interval_secs: 5,
            enable_kelly_criterion: false,
            kelly_fraction: 0.25,     // 保守的 Kelly 比例
            var_confidence: 0.95,
            enable_dynamic_adjustment: true,
        }
    }
}

/// 風險指標
#[derive(Debug, Default)]
struct RiskMetrics {
    /// 當日已實現損失
    daily_realized_loss: f64,
    
    /// 當日交易次數
    daily_trades: u64,
    
    /// 最後交易時間
    last_trade_time: Option<u64>,
    
    /// 歷史收益率（用於計算 VaR 和 Sharpe）
    returns_history: Vec<f64>,
    
    /// 當前最大回撤
    current_drawdown: f64,
    
    /// 歷史最大回撤
    max_drawdown: f64,
    
    /// 各符號的暴露
    symbol_exposure: HashMap<String, f64>,
}

/// 風險限制
#[derive(Debug)]
struct RiskLimits {
    /// 剩餘可用資金
    available_capital: f64,
    
    /// 剩餘日損失額度
    remaining_daily_loss: f64,
    
    /// 當前槓桿
    current_leverage: f64,
    
    /// 當前持倉數
    current_positions: usize,
}

impl UnifiedRiskManager {
    pub fn new(config: RiskManagerConfig) -> Self {
        Self {
            config,
            metrics: Arc::new(RwLock::new(RiskMetrics::default())),
            limits: Arc::new(RwLock::new(RiskLimits {
                available_capital: 0.0,
                remaining_daily_loss: 0.0,
                current_leverage: 0.0,
                current_positions: 0,
            })),
        }
    }
    
    /// 計算建議的持倉大小
    async fn calculate_position_size(
        &self,
        signal: &Signal,
        portfolio: &Portfolio,
    ) -> Result<f64> {
        let total_value = portfolio.cash + portfolio.positions.values()
            .map(|p| p.quantity * p.current_price)
            .sum::<f64>();
        
        // 基礎持倉大小（風險平價）
        let base_size = total_value * self.config.max_position_ratio;
        
        // 如果啟用 Kelly 準則
        let size = if self.config.enable_kelly_criterion {
            let kelly_size = self.calculate_kelly_size(signal, &base_size).await?;
            kelly_size.min(base_size)
        } else {
            base_size
        };
        
        // 應用其他限制
        let final_size = self.apply_size_limits(size, signal, portfolio).await?;
        
        Ok(final_size)
    }
    
    /// 計算 Kelly 準則建議的持倉大小
    async fn calculate_kelly_size(&self, signal: &Signal, base_size: &f64) -> Result<f64> {
        // 簡化的 Kelly 公式: f = (p*b - q) / b
        // 其中 p = 獲勝概率, q = 失敗概率, b = 賠率
        
        // 從信號置信度推斷獲勝概率
        let p = signal.confidence;
        let q = 1.0 - p;
        let b = 2.0; // 假設固定賠率 2:1
        
        let kelly_fraction = (p * b - q) / b;
        let adjusted_fraction = kelly_fraction * self.config.kelly_fraction; // 保守調整
        
        Ok(base_size * adjusted_fraction.max(0.0))
    }
    
    /// 應用持倉大小限制
    async fn apply_size_limits(
        &self,
        size: f64,
        signal: &Signal,
        portfolio: &Portfolio,
    ) -> Result<f64> {
        let limits = self.limits.read().await;
        
        // 檢查可用資金
        let max_by_capital = limits.available_capital;
        
        // 檢查日損失限制
        let max_by_daily_loss = limits.remaining_daily_loss / self.config.max_loss_per_trade;
        
        // 取最小值
        let final_size = size.min(max_by_capital).min(max_by_daily_loss);
        
        Ok(final_size.max(0.0))
    }
    
    /// 計算 VaR（風險價值）
    async fn calculate_var(&self, portfolio: &Portfolio, confidence: f64) -> f64 {
        let metrics = self.metrics.read().await;
        
        if metrics.returns_history.len() < 30 {
            // 數據不足，返回保守估計
            return portfolio.total_value * 0.05;
        }
        
        // 簡化的歷史 VaR 計算
        let mut sorted_returns = metrics.returns_history.clone();
        sorted_returns.sort_by(|a, b| a.partial_cmp(b).unwrap());
        
        let index = ((1.0 - confidence) * sorted_returns.len() as f64) as usize;
        let var_return = sorted_returns[index];
        
        portfolio.total_value * var_return.abs()
    }
    
    /// 計算 Sharpe 比率
    async fn calculate_sharpe_ratio(&self) -> f64 {
        let metrics = self.metrics.read().await;
        
        if metrics.returns_history.len() < 30 {
            return 0.0;
        }
        
        let mean_return = metrics.returns_history.iter().sum::<f64>() / metrics.returns_history.len() as f64;
        let variance = metrics.returns_history.iter()
            .map(|r| (r - mean_return).powi(2))
            .sum::<f64>() / metrics.returns_history.len() as f64;
        let std_dev = variance.sqrt();
        
        if std_dev > 0.0 {
            mean_return / std_dev * (252.0_f64).sqrt() // 年化
        } else {
            0.0
        }
    }
    
    /// 更新每日指標（應該在每日開始時調用）
    pub async fn reset_daily_metrics(&mut self) {
        let mut metrics = self.metrics.write().await;
        metrics.daily_realized_loss = 0.0;
        metrics.daily_trades = 0;
        
        let mut limits = self.limits.write().await;
        limits.remaining_daily_loss = self.config.max_daily_loss;
    }
}

#[async_trait]
impl RiskManager for UnifiedRiskManager {
    async fn check_signal(&self, signal: &Signal, portfolio: &Portfolio) -> Result<RiskDecision> {
        let mut metrics = self.metrics.write().await;
        let limits = self.limits.read().await;
        
        // 1. 檢查持倉數量限制
        if portfolio.positions.len() >= self.config.max_positions {
            return Ok(RiskDecision::Reject {
                reason: format!("Maximum positions ({}) reached", self.config.max_positions)
            });
        }
        
        // 2. 檢查交易頻率
        if let Some(last_trade) = metrics.last_trade_time {
            let now = chrono::Utc::now().timestamp_millis() as u64;
            if now - last_trade < self.config.min_trade_interval_secs * 1000 {
                return Ok(RiskDecision::Reject {
                    reason: "Too frequent trading".to_string()
                });
            }
        }
        
        // 3. 檢查日損失限制
        if limits.remaining_daily_loss <= 0.0 {
            return Ok(RiskDecision::Reject {
                reason: "Daily loss limit reached".to_string()
            });
        }
        
        // 4. 檢查槓桿限制
        if limits.current_leverage >= self.config.max_leverage {
            return Ok(RiskDecision::Reject {
                reason: format!("Maximum leverage ({}) exceeded", self.config.max_leverage)
            });
        }
        
        // 5. 計算建議的持倉大小
        let suggested_size = self.calculate_position_size(signal, portfolio).await?;
        
        if suggested_size <= 0.0 {
            return Ok(RiskDecision::Reject {
                reason: "Insufficient capital or risk budget".to_string()
            });
        }
        
        // 6. 如果需要調整大小
        if (suggested_size - signal.quantity).abs() > 0.01 {
            let mut adjustments = HashMap::new();
            adjustments.insert("quantity".to_string(), suggested_size);
            return Ok(RiskDecision::Modify { adjustments });
        }
        
        // 更新指標
        metrics.last_trade_time = Some(chrono::Utc::now().timestamp_millis() as u64);
        metrics.daily_trades += 1;
        
        Ok(RiskDecision::Approve)
    }
    
    async fn check_order(&self, order: &Order, portfolio: &Portfolio) -> Result<RiskDecision> {
        // 訂單級別的風險檢查
        // 這裡可以實現更細粒度的檢查
        
        Ok(RiskDecision::Approve)
    }
    
    async fn update_metrics(&mut self, portfolio: &Portfolio) -> Result<()> {
        let mut metrics = self.metrics.write().await;
        let mut limits = self.limits.write().await;
        
        // 更新資金和槓桿
        let total_value = portfolio.cash + portfolio.positions.values()
            .map(|p| p.quantity * p.current_price)
            .sum::<f64>();
        
        limits.available_capital = portfolio.cash;
        limits.current_positions = portfolio.positions.len();
        
        // 計算當前槓桿
        let position_value = portfolio.positions.values()
            .map(|p| p.quantity * p.current_price)
            .sum::<f64>();
        limits.current_leverage = if total_value > 0.0 {
            position_value / total_value
        } else {
            0.0
        };
        
        // 更新回撤
        if total_value > 0.0 {
            let drawdown = (portfolio.unrealized_pnl + portfolio.realized_pnl).min(0.0) / total_value;
            metrics.current_drawdown = drawdown;
            metrics.max_drawdown = metrics.max_drawdown.min(drawdown);
        }
        
        // 更新暴露
        metrics.symbol_exposure.clear();
        for (symbol, position) in &portfolio.positions {
            let exposure = position.quantity * position.current_price / total_value;
            metrics.symbol_exposure.insert(symbol.clone(), exposure);
        }
        
        // 更新收益率歷史（保留最近 252 個數據點）
        if total_value > 0.0 {
            let daily_return = portfolio.realized_pnl / total_value;
            metrics.returns_history.push(daily_return);
            if metrics.returns_history.len() > 252 {
                metrics.returns_history.remove(0);
            }
        }
        
        Ok(())
    }
    
    async fn get_risk_report(&self) -> RiskReport {
        let metrics = self.metrics.read().await;
        let limits = self.limits.read().await;
        
        // 計算風險級別
        let risk_level = if limits.current_leverage > self.config.max_leverage * 0.9 {
            RiskLevel::Critical
        } else if metrics.current_drawdown < -0.1 {
            RiskLevel::High
        } else if limits.remaining_daily_loss < self.config.max_daily_loss * 0.2 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };
        
        RiskReport {
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
            var_95: 0.0, // 需要實際計算
            var_99: 0.0, // 需要實際計算
            sharpe_ratio: 0.0, // 需要實際計算
            max_drawdown: metrics.max_drawdown,
            current_drawdown: metrics.current_drawdown,
            exposure: metrics.symbol_exposure.clone(),
            risk_level,
        }
    }
}

/// 創建默認配置的風險管理器
pub fn create_default_risk_manager() -> UnifiedRiskManager {
    UnifiedRiskManager::new(RiskManagerConfig::default())
}

/// 創建保守配置的風險管理器
pub fn create_conservative_risk_manager() -> UnifiedRiskManager {
    let config = RiskManagerConfig {
        max_position_ratio: 0.05,     // 5% 最大持倉
        max_loss_per_trade: 0.01,     // 1% 最大單筆損失
        max_daily_loss: 0.02,         // 2% 最大日損失
        max_leverage: 1.0,            // 無槓桿
        max_positions: 5,             // 最多 5 個持倉
        min_trade_interval_secs: 30,  // 30 秒最小間隔
        enable_kelly_criterion: true,
        kelly_fraction: 0.1,          // 非常保守的 Kelly
        var_confidence: 0.99,         // 99% VaR
        enable_dynamic_adjustment: true,
    };
    
    UnifiedRiskManager::new(config)
}

/// 創建激進配置的風險管理器
pub fn create_aggressive_risk_manager() -> UnifiedRiskManager {
    let config = RiskManagerConfig {
        max_position_ratio: 0.2,      // 20% 最大持倉
        max_loss_per_trade: 0.05,     // 5% 最大單筆損失
        max_daily_loss: 0.1,          // 10% 最大日損失
        max_leverage: 2.0,            // 2x 槓桿
        max_positions: 20,            // 最多 20 個持倉
        min_trade_interval_secs: 1,   // 1 秒最小間隔
        enable_kelly_criterion: true,
        kelly_fraction: 0.5,          // 激進的 Kelly
        var_confidence: 0.90,         // 90% VaR
        enable_dynamic_adjustment: false,
    };
    
    UnifiedRiskManager::new(config)
}