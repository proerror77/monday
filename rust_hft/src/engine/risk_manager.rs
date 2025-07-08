/*!
 * Barter-Integrated Risk Management System
 * 
 * Advanced risk management using barter ecosystem concepts
 * Provides real-time risk monitoring and position management
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::engine::barter_execution::{SimpleOrder, SimpleOrderStatus, BarterPortfolioManager};
use barter_instrument::Side as InstrumentSide;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn, error, debug};
use rust_decimal::prelude::ToPrimitive;

/// 風險管理統計
#[derive(Debug, Clone, Default)]
pub struct RiskStats {
    pub total_orders_checked: u64,
    pub orders_blocked: u64,
    pub risk_violations: u64,
    pub max_drawdown: f64,
    pub current_drawdown: f64,
    pub value_at_risk: f64,
    pub sharpe_ratio: f64,
    pub win_rate: f64,
}

/// 風險管理器
pub struct BarterRiskManager {
    /// 配置
    config: Config,
    
    /// 風險統計
    stats: Arc<Mutex<RiskStats>>,
    
    /// 當前倉位
    positions: Arc<Mutex<HashMap<String, f64>>>,
    
    /// 歷史PnL記錄
    pnl_history: Arc<Mutex<Vec<f64>>>,
    
    /// 最大倉位限制
    max_position_limits: HashMap<String, f64>,
    
    /// 風險閾值
    risk_thresholds: RiskThresholds,
    
    /// Portfolio管理器
    portfolio_manager: Arc<Mutex<BarterPortfolioManager>>,
    
    /// 活躍訂單風險追蹤
    order_risks: Arc<Mutex<HashMap<String, OrderRisk>>>,
}

/// 風險閾值配置
#[derive(Debug, Clone)]
pub struct RiskThresholds {
    pub max_position_size: f64,
    pub max_daily_loss: f64,
    pub max_drawdown: f64,
    pub max_order_value: f64,
    pub max_orders_per_minute: u32,
    pub min_confidence_threshold: f64,
    pub value_at_risk_limit: f64,
}

impl Default for RiskThresholds {
    fn default() -> Self {
        Self {
            max_position_size: 1.0,      // 1 BTC
            max_daily_loss: 1000.0,      // $1000
            max_drawdown: 0.05,          // 5%
            max_order_value: 500.0,      // $500
            max_orders_per_minute: 60,   // 每分鐘最多60單
            min_confidence_threshold: 0.7, // 最低信心度70%
            value_at_risk_limit: 2000.0, // VaR限制$2000
        }
    }
}

/// 單筆訂單風險
#[derive(Debug, Clone)]
pub struct OrderRisk {
    pub order_id: String,
    pub symbol: String,
    pub side: InstrumentSide,
    pub quantity: f64,
    pub price: f64,
    pub value: f64,
    pub risk_score: f64,
    pub timestamp: u64,
}

/// 風險檢查結果
#[derive(Debug, Clone)]
pub enum RiskCheckResult {
    Approved,
    Rejected(String),
    Warning(String),
}

impl BarterRiskManager {
    /// 創建新的風險管理器
    pub fn new(config: Config) -> Self {
        let mut max_position_limits = HashMap::new();
        max_position_limits.insert("BTCUSDT".to_string(), 1.0);
        max_position_limits.insert("ETHUSDT".to_string(), 10.0);
        
        let risk_thresholds = RiskThresholds {
            max_position_size: config.risk.max_position_value,
            max_daily_loss: config.risk.max_daily_loss,
            max_drawdown: 0.05, // 5% 預設值
            max_order_value: config.risk.max_position_value * 0.1, // 10% of max position
            max_orders_per_minute: config.risk.max_order_rate as u32,
            min_confidence_threshold: config.ml.prediction_threshold,
            value_at_risk_limit: 2000.0,
        };
        
        Self {
            config,
            stats: Arc::new(Mutex::new(RiskStats::default())),
            positions: Arc::new(Mutex::new(HashMap::new())),
            pnl_history: Arc::new(Mutex::new(Vec::new())),
            max_position_limits,
            risk_thresholds,
            portfolio_manager: Arc::new(Mutex::new(BarterPortfolioManager::new())),
            order_risks: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    /// 檢查交易信號風險
    pub async fn check_signal_risk(&self, signal: &TradingSignal) -> Result<RiskCheckResult> {
        let mut stats = self.stats.lock().await;
        stats.total_orders_checked += 1;
        
        // 1. 檢查信心度
        if signal.confidence < self.risk_thresholds.min_confidence_threshold {
            stats.orders_blocked += 1;
            return Ok(RiskCheckResult::Rejected(format!(
                "信號信心度過低: {:.3} < {:.3}",
                signal.confidence,
                self.risk_thresholds.min_confidence_threshold
            )));
        }
        
        // 2. 檢查訂單價值
        let order_value = signal.suggested_price.0 * signal.suggested_quantity.to_f64().unwrap_or(0.0);
        if order_value > self.risk_thresholds.max_order_value {
            stats.orders_blocked += 1;
            return Ok(RiskCheckResult::Rejected(format!(
                "訂單價值過大: ${:.2} > ${:.2}",
                order_value,
                self.risk_thresholds.max_order_value
            )));
        }
        
        // 3. 檢查倉位限制
        let position_check = self.check_position_limits(signal).await?;
        if let RiskCheckResult::Rejected(_) = position_check {
            stats.orders_blocked += 1;
            return Ok(position_check);
        }
        
        // 4. 檢查每日損失限制
        let daily_pnl_check = self.check_daily_pnl_limits().await?;
        if let RiskCheckResult::Rejected(_) = daily_pnl_check {
            stats.orders_blocked += 1;
            return Ok(daily_pnl_check);
        }
        
        // 5. 檢查最大回撤
        let drawdown_check = self.check_drawdown_limits().await?;
        if let RiskCheckResult::Rejected(_) = drawdown_check {
            stats.orders_blocked += 1;
            return Ok(drawdown_check);
        }
        
        // 6. 計算風險評分
        let risk_score = self.calculate_risk_score(signal).await;
        
        if risk_score > 0.8 {
            return Ok(RiskCheckResult::Warning(format!(
                "高風險信號，風險評分: {:.2}",
                risk_score
            )));
        }
        
        debug!("風險檢查通過，風險評分: {:.2}", risk_score);
        Ok(RiskCheckResult::Approved)
    }
    
    /// 檢查倉位限制
    async fn check_position_limits(&self, signal: &TradingSignal) -> Result<RiskCheckResult> {
        let positions = self.positions.lock().await;
        let symbol = &self.config.symbol();
        
        let current_position = positions.get(&symbol.to_string()).cloned().unwrap_or(0.0);
        let max_limit = self.max_position_limits.get(&symbol.to_string()).cloned().unwrap_or(1.0);
        
        let quantity_change = match signal.signal_type {
            SignalType::Buy => signal.suggested_quantity.to_f64().unwrap_or(0.0),
            SignalType::Sell => -signal.suggested_quantity.to_f64().unwrap_or(0.0),
            SignalType::Hold => 0.0,
        };
        
        let new_position = current_position + quantity_change;
        
        if new_position.abs() > max_limit {
            return Ok(RiskCheckResult::Rejected(format!(
                "倉位超限: 新倉位 {:.6} > 限制 {:.6}",
                new_position, max_limit
            )));
        }
        
        Ok(RiskCheckResult::Approved)
    }
    
    /// 檢查每日PnL限制
    async fn check_daily_pnl_limits(&self) -> Result<RiskCheckResult> {
        let pnl_history = self.pnl_history.lock().await;
        
        if pnl_history.is_empty() {
            return Ok(RiskCheckResult::Approved);
        }
        
        // 計算今日PnL（簡化為最新記錄）
        let latest_pnl = pnl_history.last().cloned().unwrap_or(0.0);
        
        if latest_pnl < -self.risk_thresholds.max_daily_loss {
            return Ok(RiskCheckResult::Rejected(format!(
                "每日損失超限: ${:.2} < -${:.2}",
                latest_pnl, self.risk_thresholds.max_daily_loss
            )));
        }
        
        Ok(RiskCheckResult::Approved)
    }
    
    /// 檢查最大回撤限制
    async fn check_drawdown_limits(&self) -> Result<RiskCheckResult> {
        let stats = self.stats.lock().await;
        
        if stats.current_drawdown > self.risk_thresholds.max_drawdown {
            return Ok(RiskCheckResult::Rejected(format!(
                "回撤超限: {:.2}% > {:.2}%",
                stats.current_drawdown * 100.0,
                self.risk_thresholds.max_drawdown * 100.0
            )));
        }
        
        Ok(RiskCheckResult::Approved)
    }
    
    /// 計算風險評分
    async fn calculate_risk_score(&self, signal: &TradingSignal) -> f64 {
        let mut risk_score = 0.0;
        
        // 基於信心度的風險
        risk_score += (1.0 - signal.confidence) * 0.3;
        
        // 基於價格波動的風險
        let order_value = signal.suggested_price.0 * signal.suggested_quantity.to_f64().unwrap_or(0.0);
        let value_risk = (order_value / self.risk_thresholds.max_order_value).min(1.0);
        risk_score += value_risk * 0.2;
        
        // 基於倉位集中度的風險
        let positions = self.positions.lock().await;
        let symbol = &self.config.symbol();
        let current_position = positions.get(&symbol.to_string()).cloned().unwrap_or(0.0);
        let max_limit = self.max_position_limits.get(&symbol.to_string()).cloned().unwrap_or(1.0);
        let position_risk = (current_position.abs() / max_limit).min(1.0);
        risk_score += position_risk * 0.3;
        
        // 基於市場波動的風險
        let market_risk = 0.2; // 簡化的市場風險評分
        risk_score += market_risk * 0.2;
        
        risk_score.min(1.0)
    }
    
    /// 更新倉位
    pub async fn update_position(&self, symbol: &str, quantity_change: f64) {
        let mut positions = self.positions.lock().await;
        *positions.entry(symbol.to_string()).or_insert(0.0) += quantity_change;
        
        let mut portfolio = self.portfolio_manager.lock().await;
        portfolio.update_position(symbol, quantity_change);
        
        info!("倉位更新: {} 變化 {:.6}, 當前倉位: {:.6}",
              symbol, quantity_change, positions.get(symbol).cloned().unwrap_or(0.0));
    }
    
    /// 記錄訂單風險
    pub async fn record_order_risk(&self, order: &SimpleOrder) {
        let order_risk = OrderRisk {
            order_id: order.id.clone(),
            symbol: order.symbol.clone(),
            side: order.side,
            quantity: order.quantity,
            price: order.price.unwrap_or(0.0),
            value: order.quantity * order.price.unwrap_or(0.0),
            risk_score: 0.5, // 簡化的風險評分
            timestamp: order.created_at,
        };
        
        let mut order_risks = self.order_risks.lock().await;
        order_risks.insert(order.id.clone(), order_risk);
        
        debug!("記錄訂單風險: {}", order.id);
    }
    
    /// 更新PnL歷史
    pub async fn update_pnl(&self, pnl: f64) {
        let mut pnl_history = self.pnl_history.lock().await;
        pnl_history.push(pnl);
        
        // 保持最近1000條記錄
        if pnl_history.len() > 1000 {
            pnl_history.remove(0);
        }
        
        // 更新統計
        let mut stats = self.stats.lock().await;
        self.calculate_risk_metrics(&pnl_history, &mut stats);
    }
    
    /// 計算風險指標
    fn calculate_risk_metrics(&self, pnl_history: &[f64], stats: &mut RiskStats) {
        if pnl_history.is_empty() {
            return;
        }
        
        // 計算回撤
        let mut peak = 0.0;
        let mut max_drawdown = 0.0;
        let mut current_drawdown = 0.0;
        
        for &pnl in pnl_history {
            if pnl > peak {
                peak = pnl;
            }
            
            let drawdown = (peak - pnl) / peak.max(1.0);
            if drawdown > max_drawdown {
                max_drawdown = drawdown;
            }
            
            if pnl == *pnl_history.last().unwrap() {
                current_drawdown = drawdown;
            }
        }
        
        stats.max_drawdown = max_drawdown;
        stats.current_drawdown = current_drawdown;
        
        // 計算夏普比率（簡化）
        if pnl_history.len() > 1 {
            let mean_return = pnl_history.iter().sum::<f64>() / pnl_history.len() as f64;
            let variance = pnl_history.iter()
                .map(|&x| (x - mean_return).powi(2))
                .sum::<f64>() / (pnl_history.len() - 1) as f64;
            let std_dev = variance.sqrt();
            
            if std_dev > 0.0 {
                stats.sharpe_ratio = mean_return / std_dev;
            }
        }
        
        // 計算勝率（簡化為正收益比例）
        let winning_trades = pnl_history.iter().filter(|&&x| x > 0.0).count();
        if !pnl_history.is_empty() {
            stats.win_rate = winning_trades as f64 / pnl_history.len() as f64;
        }
        
        // 計算VaR（簡化為5%分位數）
        let mut sorted_pnl = pnl_history.to_vec();
        sorted_pnl.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let var_index = (pnl_history.len() as f64 * 0.05) as usize;
        if var_index < sorted_pnl.len() {
            stats.value_at_risk = sorted_pnl[var_index].abs();
        }
    }
    
    /// 取得風險統計
    pub async fn get_stats(&self) -> RiskStats {
        self.stats.lock().await.clone()
    }
    
    /// 取得當前倉位
    pub async fn get_positions(&self) -> HashMap<String, f64> {
        self.positions.lock().await.clone()
    }
    
    /// 檢查系統風險狀態
    pub async fn get_risk_status(&self) -> RiskStatus {
        let stats = self.stats.lock().await;
        let positions = self.positions.lock().await;
        
        let mut risk_level = RiskLevel::Low;
        let mut warnings = Vec::new();
        
        // 檢查回撤
        if stats.current_drawdown > self.risk_thresholds.max_drawdown * 0.8 {
            risk_level = RiskLevel::High;
            warnings.push(format!("回撤接近限制: {:.2}%", stats.current_drawdown * 100.0));
        } else if stats.current_drawdown > self.risk_thresholds.max_drawdown * 0.6 {
            risk_level = RiskLevel::Medium;
            warnings.push(format!("回撤偏高: {:.2}%", stats.current_drawdown * 100.0));
        }
        
        // 檢查倉位
        for (symbol, &position) in positions.iter() {
            let limit = self.max_position_limits.get(symbol).cloned().unwrap_or(1.0);
            let utilization = position.abs() / limit;
            
            if utilization > 0.9 {
                risk_level = RiskLevel::High;
                warnings.push(format!("倉位接近限制: {} {:.2}%", symbol, utilization * 100.0));
            } else if utilization > 0.7 {
                if risk_level == RiskLevel::Low {
                    risk_level = RiskLevel::Medium;
                }
                warnings.push(format!("倉位偏高: {} {:.2}%", symbol, utilization * 100.0));
            }
        }
        
        // 檢查VaR
        if stats.value_at_risk > self.risk_thresholds.value_at_risk_limit * 0.9 {
            risk_level = RiskLevel::High;
            warnings.push(format!("VaR接近限制: ${:.2}", stats.value_at_risk));
        }
        
        RiskStatus {
            level: risk_level,
            warnings,
            drawdown: stats.current_drawdown,
            value_at_risk: stats.value_at_risk,
            sharpe_ratio: stats.sharpe_ratio,
        }
    }
    
    /// 重置風險統計
    pub async fn reset_stats(&self) {
        let mut stats = self.stats.lock().await;
        *stats = RiskStats::default();
        
        let mut pnl_history = self.pnl_history.lock().await;
        pnl_history.clear();
        
        let mut positions = self.positions.lock().await;
        positions.clear();
        
        info!("風險管理統計已重置");
    }
}

/// 風險等級
#[derive(Debug, Clone, PartialEq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// 風險狀態
#[derive(Debug, Clone)]
pub struct RiskStatus {
    pub level: RiskLevel,
    pub warnings: Vec<String>,
    pub drawdown: f64,
    pub value_at_risk: f64,
    pub sharpe_ratio: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_risk_manager_creation() {
        let config = Config::default();
        let risk_manager = BarterRiskManager::new(config);
        
        let stats = risk_manager.get_stats().await;
        assert_eq!(stats.total_orders_checked, 0);
    }
    
    #[tokio::test]
    async fn test_signal_risk_check() {
        let config = Config::default();
        let risk_manager = BarterRiskManager::new(config);
        
        let signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.8,
            suggested_price: 100.0.to_price(),
            suggested_quantity: 0.001.to_quantity(),
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 50,
        };
        
        let result = risk_manager.check_signal_risk(&signal).await.unwrap();
        matches!(result, RiskCheckResult::Approved);
    }
    
    #[tokio::test]
    async fn test_position_limits() {
        let config = Config::default();
        let risk_manager = BarterRiskManager::new(config);
        
        // 更新倉位接近限制
        risk_manager.update_position("BTCUSDT", 0.9).await;
        
        let signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.8,
            suggested_price: 100.0.to_price(),
            suggested_quantity: 0.2.to_quantity(), // 會超過1.0限制
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 50,
        };
        
        let result = risk_manager.check_signal_risk(&signal).await.unwrap();
        matches!(result, RiskCheckResult::Rejected(_));
    }
    
    #[tokio::test]
    async fn test_risk_score_calculation() {
        let config = Config::default();
        let risk_manager = BarterRiskManager::new(config);
        
        let low_confidence_signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.3, // 低信心度
            suggested_price: 100.0.to_price(),
            suggested_quantity: 0.001.to_quantity(),
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 50,
        };
        
        let risk_score = risk_manager.calculate_risk_score(&low_confidence_signal).await;
        assert!(risk_score > 0.2); // 低信心度應該有較高風險評分
    }
}