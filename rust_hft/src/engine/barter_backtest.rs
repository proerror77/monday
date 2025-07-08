/*!
 * Barter-Integrated Backtesting System
 * 
 * 基於 barter-rs 架構的回測系統，提供與實盤交易相同的引擎
 * 支持歷史數據回測和性能分析
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::engine::barter_engine::{BarterUnifiedEngine, BarterEvent, MarketDataEvent, SystemControlEvent};
use crate::ml::features::FeatureExtractor;
use anyhow::Result;
use std::collections::HashMap;
use tracing::{info, debug};
use serde::{Serialize, Deserialize};
use rust_decimal::prelude::ToPrimitive;

/// 回測配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// 開始時間
    pub start_time: u64,
    /// 結束時間  
    pub end_time: u64,
    /// 初始資金
    pub initial_capital: f64,
    /// 手續費率
    pub commission_rate: f64,
    /// 滑點 (bps)
    pub slippage_bps: f64,
    /// 數據頻率 (微秒)
    pub data_frequency_us: u64,
    /// 是否啟用實時性能監控
    pub enable_live_metrics: bool,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            start_time: 0,
            end_time: 0,
            initial_capital: 10000.0,
            commission_rate: 0.001, // 0.1%
            slippage_bps: 5.0,      // 5 bps
            data_frequency_us: 1000000, // 1秒
            enable_live_metrics: true,
        }
    }
}

/// 回測結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResults {
    /// 配置
    pub config: BacktestConfig,
    /// 性能指標
    pub performance: PerformanceMetrics,
    /// 交易記錄
    pub trades: Vec<TradeRecord>,
    /// 資金曲線
    pub equity_curve: Vec<EquityPoint>,
    /// 風險指標
    pub risk_metrics: RiskMetrics,
    /// 回測統計
    pub statistics: BacktestStatistics,
}

/// 性能指標
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    /// 總收益率
    pub total_return: f64,
    /// 年化收益率
    pub annualized_return: f64,
    /// 夏普比率
    pub sharpe_ratio: f64,
    /// 索提諾比率
    pub sortino_ratio: f64,
    /// 最大回撤
    pub max_drawdown: f64,
    /// 勝率
    pub win_rate: f64,
    /// 盈虧比
    pub profit_factor: f64,
    /// 平均交易收益
    pub avg_trade_return: f64,
    /// Calmar比率
    pub calmar_ratio: f64,
}

/// 交易記錄
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub trade_id: String,
    pub symbol: String,
    pub side: String,
    pub quantity: f64,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub entry_time: u64,
    pub exit_time: Option<u64>,
    pub pnl: f64,
    pub commission: f64,
    pub slippage: f64,
    pub net_pnl: f64,
}

/// 資金曲線點
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp: u64,
    pub equity: f64,
    pub drawdown: f64,
    pub position_value: f64,
}

/// 風險指標
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskMetrics {
    /// 風險價值 (VaR)
    pub value_at_risk_95: f64,
    pub value_at_risk_99: f64,
    /// 條件風險價值 (CVaR)
    pub conditional_var_95: f64,
    pub conditional_var_99: f64,
    /// 波動率
    pub volatility: f64,
    /// 偏度
    pub skewness: f64,
    /// 峰度
    pub kurtosis: f64,
}

/// 回測統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestStatistics {
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub avg_holding_period_us: u64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub largest_win: f64,
    pub largest_loss: f64,
    pub consecutive_wins: u64,
    pub consecutive_losses: u64,
}

/// Barter 回測引擎
pub struct BarterBacktestEngine {
    /// 統一引擎
    engine: BarterUnifiedEngine,
    /// 回測配置
    config: BacktestConfig,
    /// 模擬交易所
    mock_exchange: MockExchange,
    /// 數據源
    data_source: Box<dyn BacktestDataSource + Send + Sync>,
    /// 結果收集器
    results_collector: BacktestResultsCollector,
}

/// 模擬交易所
pub struct MockExchange {
    /// 當前價格
    current_prices: HashMap<String, f64>,
    /// 持倉
    positions: HashMap<String, f64>,
    /// 可用資金
    available_cash: f64,
    /// 總資產
    total_equity: f64,
    /// 手續費設置
    commission_rate: f64,
    /// 滑點設置
    slippage_bps: f64,
}

impl MockExchange {
    pub fn new(initial_capital: f64, commission_rate: f64, slippage_bps: f64) -> Self {
        Self {
            current_prices: HashMap::new(),
            positions: HashMap::new(),
            available_cash: initial_capital,
            total_equity: initial_capital,
            commission_rate,
            slippage_bps,
        }
    }
    
    /// 更新價格
    pub fn update_price(&mut self, symbol: &str, price: f64) {
        self.current_prices.insert(symbol.to_string(), price);
        self.recalculate_equity();
    }
    
    /// 執行交易
    pub fn execute_trade(&mut self, symbol: &str, quantity: f64, is_buy: bool) -> Result<TradeRecord> {
        let price = self.current_prices.get(symbol)
            .ok_or_else(|| anyhow::anyhow!("沒有 {} 的價格數據", symbol))?;
        
        // 計算滑點
        let slippage_factor = if is_buy { 1.0 + self.slippage_bps / 10000.0 } else { 1.0 - self.slippage_bps / 10000.0 };
        let execution_price = price * slippage_factor;
        
        // 計算手續費
        let trade_value = quantity * execution_price;
        let commission = trade_value * self.commission_rate;
        let slippage_cost = (execution_price - price) * quantity;
        
        // 檢查資金是否足夠
        if is_buy && (trade_value + commission) > self.available_cash {
            return Err(anyhow::anyhow!("資金不足"));
        }
        
        // 執行交易
        let position_change = if is_buy { quantity } else { -quantity };
        *self.positions.entry(symbol.to_string()).or_insert(0.0) += position_change;
        
        let cash_change = if is_buy { -(trade_value + commission) } else { trade_value - commission };
        self.available_cash += cash_change;
        
        self.recalculate_equity();
        
        let pnl = if is_buy { -slippage_cost - commission } else { slippage_cost - commission };
        
        Ok(TradeRecord {
            trade_id: format!("trade_{}", now_micros()),
            symbol: symbol.to_string(),
            side: if is_buy { "buy".to_string() } else { "sell".to_string() },
            quantity,
            entry_price: execution_price,
            exit_price: None,
            entry_time: now_micros(),
            exit_time: None,
            pnl,
            commission,
            slippage: slippage_cost,
            net_pnl: pnl,
        })
    }
    
    /// 重新計算總資產
    fn recalculate_equity(&mut self) {
        let mut position_value = 0.0;
        
        for (symbol, &position) in &self.positions {
            if let Some(&price) = self.current_prices.get(symbol) {
                position_value += position * price;
            }
        }
        
        self.total_equity = self.available_cash + position_value;
    }
    
    /// 獲取當前權益
    pub fn get_equity(&self) -> f64 {
        self.total_equity
    }
    
    /// 獲取持倉
    pub fn get_positions(&self) -> &HashMap<String, f64> {
        &self.positions
    }
}

/// 回測數據源 trait
pub trait BacktestDataSource {
    /// 獲取下一個數據點
    fn next_data(&mut self) -> Option<OrderBookUpdate>;
    /// 重置數據源
    fn reset(&mut self);
    /// 是否還有數據
    fn has_more_data(&self) -> bool;
}

/// 文件數據源實現
pub struct FileDataSource {
    data: Vec<OrderBookUpdate>,
    current_index: usize,
}

impl FileDataSource {
    pub fn new(data: Vec<OrderBookUpdate>) -> Self {
        Self {
            data,
            current_index: 0,
        }
    }
}

impl BacktestDataSource for FileDataSource {
    fn next_data(&mut self) -> Option<OrderBookUpdate> {
        if self.current_index < self.data.len() {
            let data = self.data[self.current_index].clone();
            self.current_index += 1;
            Some(data)
        } else {
            None
        }
    }
    
    fn reset(&mut self) {
        self.current_index = 0;
    }
    
    fn has_more_data(&self) -> bool {
        self.current_index < self.data.len()
    }
}

/// 回測結果收集器
pub struct BacktestResultsCollector {
    trades: Vec<TradeRecord>,
    equity_curve: Vec<EquityPoint>,
    initial_capital: f64,
}

impl BacktestResultsCollector {
    pub fn new(initial_capital: f64) -> Self {
        Self {
            trades: Vec::new(),
            equity_curve: Vec::new(),
            initial_capital,
        }
    }
    
    /// 記錄交易
    pub fn record_trade(&mut self, trade: TradeRecord) {
        self.trades.push(trade);
    }
    
    /// 記錄權益點
    pub fn record_equity_point(&mut self, point: EquityPoint) {
        self.equity_curve.push(point);
    }
    
    /// 生成最終結果
    pub fn generate_results(&self, config: BacktestConfig) -> BacktestResults {
        let performance = self.calculate_performance_metrics();
        let risk_metrics = self.calculate_risk_metrics();
        let statistics = self.calculate_statistics();
        
        BacktestResults {
            config,
            performance,
            trades: self.trades.clone(),
            equity_curve: self.equity_curve.clone(),
            risk_metrics,
            statistics,
        }
    }
    
    /// 計算性能指標
    fn calculate_performance_metrics(&self) -> PerformanceMetrics {
        if self.equity_curve.is_empty() {
            return PerformanceMetrics {
                total_return: 0.0,
                annualized_return: 0.0,
                sharpe_ratio: 0.0,
                sortino_ratio: 0.0,
                max_drawdown: 0.0,
                win_rate: 0.0,
                profit_factor: 0.0,
                avg_trade_return: 0.0,
                calmar_ratio: 0.0,
            };
        }
        
        let final_equity = self.equity_curve.last().unwrap().equity;
        let total_return = (final_equity / self.initial_capital) - 1.0;
        
        // 計算最大回撤
        let max_drawdown = self.equity_curve.iter()
            .map(|point| point.drawdown)
            .fold(0.0, f64::max);
        
        // 計算勝率
        let winning_trades = self.trades.iter().filter(|t| t.net_pnl > 0.0).count();
        let win_rate = if self.trades.is_empty() { 0.0 } else { 
            winning_trades as f64 / self.trades.len() as f64 
        };
        
        // 計算盈虧比
        let gross_profit: f64 = self.trades.iter()
            .filter(|t| t.net_pnl > 0.0)
            .map(|t| t.net_pnl)
            .sum();
        let gross_loss: f64 = self.trades.iter()
            .filter(|t| t.net_pnl < 0.0)
            .map(|t| t.net_pnl.abs())
            .sum();
        
        let profit_factor = if gross_loss > 0.0 { gross_profit / gross_loss } else { 0.0 };
        
        // 簡化的年化收益率計算
        let time_span_years = if let (Some(first), Some(last)) = (self.equity_curve.first(), self.equity_curve.last()) {
            (last.timestamp - first.timestamp) as f64 / (365.25 * 24.0 * 3600.0 * 1_000_000.0)
        } else {
            1.0
        };
        
        let annualized_return = if time_span_years > 0.0 {
            ((final_equity / self.initial_capital).powf(1.0 / time_span_years)) - 1.0
        } else {
            total_return
        };
        
        // 簡化的夏普比率計算
        let returns: Vec<f64> = self.equity_curve.windows(2)
            .map(|w| (w[1].equity / w[0].equity) - 1.0)
            .collect();
        
        let sharpe_ratio = if !returns.is_empty() {
            let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
            let volatility = {
                let variance = returns.iter()
                    .map(|r| (r - mean_return).powi(2))
                    .sum::<f64>() / (returns.len() - 1) as f64;
                variance.sqrt()
            };
            
            if volatility > 0.0 { mean_return / volatility * (252.0_f64).sqrt() } else { 0.0 }
        } else {
            0.0
        };
        
        let avg_trade_return = if !self.trades.is_empty() {
            self.trades.iter().map(|t| t.net_pnl).sum::<f64>() / self.trades.len() as f64
        } else {
            0.0
        };
        
        let calmar_ratio = if max_drawdown > 0.0 { annualized_return / max_drawdown } else { 0.0 };
        
        PerformanceMetrics {
            total_return,
            annualized_return,
            sharpe_ratio,
            sortino_ratio: sharpe_ratio, // 簡化
            max_drawdown,
            win_rate,
            profit_factor,
            avg_trade_return,
            calmar_ratio,
        }
    }
    
    /// 計算風險指標
    fn calculate_risk_metrics(&self) -> RiskMetrics {
        let returns: Vec<f64> = self.equity_curve.windows(2)
            .map(|w| (w[1].equity / w[0].equity) - 1.0)
            .collect();
        
        if returns.is_empty() {
            return RiskMetrics {
                value_at_risk_95: 0.0,
                value_at_risk_99: 0.0,
                conditional_var_95: 0.0,
                conditional_var_99: 0.0,
                volatility: 0.0,
                skewness: 0.0,
                kurtosis: 0.0,
            };
        }
        
        let mut sorted_returns = returns.clone();
        sorted_returns.sort_by(|a, b| a.partial_cmp(b).unwrap());
        
        let var_95_index = (returns.len() as f64 * 0.05) as usize;
        let var_99_index = (returns.len() as f64 * 0.01) as usize;
        
        let value_at_risk_95 = if var_95_index < sorted_returns.len() {
            sorted_returns[var_95_index].abs()
        } else {
            0.0
        };
        
        let value_at_risk_99 = if var_99_index < sorted_returns.len() {
            sorted_returns[var_99_index].abs()
        } else {
            0.0
        };
        
        // 簡化的 CVaR 計算
        let conditional_var_95 = if var_95_index > 0 {
            sorted_returns[..var_95_index].iter().map(|r| r.abs()).sum::<f64>() / var_95_index as f64
        } else {
            value_at_risk_95
        };
        
        let conditional_var_99 = if var_99_index > 0 {
            sorted_returns[..var_99_index].iter().map(|r| r.abs()).sum::<f64>() / var_99_index as f64
        } else {
            value_at_risk_99
        };
        
        // 計算波動率
        let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let volatility = {
            let variance = returns.iter()
                .map(|r| (r - mean_return).powi(2))
                .sum::<f64>() / (returns.len() - 1) as f64;
            variance.sqrt() * (252.0_f64).sqrt() // 年化
        };
        
        RiskMetrics {
            value_at_risk_95,
            value_at_risk_99,
            conditional_var_95,
            conditional_var_99,
            volatility,
            skewness: 0.0, // 簡化
            kurtosis: 0.0,  // 簡化
        }
    }
    
    /// 計算統計數據
    fn calculate_statistics(&self) -> BacktestStatistics {
        let winning_trades = self.trades.iter().filter(|t| t.net_pnl > 0.0).count() as u64;
        let losing_trades = self.trades.iter().filter(|t| t.net_pnl < 0.0).count() as u64;
        
        let avg_win = if winning_trades > 0 {
            self.trades.iter()
                .filter(|t| t.net_pnl > 0.0)
                .map(|t| t.net_pnl)
                .sum::<f64>() / winning_trades as f64
        } else {
            0.0
        };
        
        let avg_loss = if losing_trades > 0 {
            self.trades.iter()
                .filter(|t| t.net_pnl < 0.0)
                .map(|t| t.net_pnl)
                .sum::<f64>() / losing_trades as f64
        } else {
            0.0
        };
        
        let largest_win = self.trades.iter()
            .map(|t| t.net_pnl)
            .fold(0.0, f64::max);
        
        let largest_loss = self.trades.iter()
            .map(|t| t.net_pnl)
            .fold(0.0, f64::min);
        
        BacktestStatistics {
            total_trades: self.trades.len() as u64,
            winning_trades,
            losing_trades,
            avg_holding_period_us: 0, // 簡化
            avg_win,
            avg_loss,
            largest_win,
            largest_loss,
            consecutive_wins: 0, // 簡化
            consecutive_losses: 0, // 簡化
        }
    }
}

impl BarterBacktestEngine {
    /// 創建新的回測引擎
    pub fn new(
        config: Config,
        backtest_config: BacktestConfig,
        data_source: Box<dyn BacktestDataSource + Send + Sync>,
    ) -> Result<Self> {
        let engine = BarterUnifiedEngine::new(config)?;
        let mock_exchange = MockExchange::new(
            backtest_config.initial_capital,
            backtest_config.commission_rate,
            backtest_config.slippage_bps,
        );
        let results_collector = BacktestResultsCollector::new(backtest_config.initial_capital);
        
        Ok(Self {
            engine,
            config: backtest_config,
            mock_exchange,
            data_source,
            results_collector,
        })
    }
    
    /// 運行回測
    pub async fn run_backtest(&mut self) -> Result<BacktestResults> {
        info!("🔬 開始回測...");
        
        // 啟動引擎
        self.engine.send_event(BarterEvent::SystemControl(SystemControlEvent::StartTrading))?;
        self.engine.send_event(BarterEvent::SystemControl(SystemControlEvent::EnableAlgorithm))?;
        
        let mut data_points = 0;
        let _feature_extractor = FeatureExtractor::new(100);
        
        // 主回測循環
        while self.data_source.has_more_data() {
            if let Some(orderbook_update) = self.data_source.next_data() {
                data_points += 1;
                
                // 更新模擬交易所價格
                if let Some(mid_price) = Self::calculate_mid_price(&orderbook_update) {
                    self.mock_exchange.update_price(&orderbook_update.symbol, mid_price);
                }
                
                // 記錄權益點
                let equity_point = EquityPoint {
                    timestamp: orderbook_update.timestamp,
                    equity: self.mock_exchange.get_equity(),
                    drawdown: 0.0, // 計算在後處理中完成
                    position_value: self.calculate_position_value(),
                };
                self.results_collector.record_equity_point(equity_point);
                
                // 模擬特徵提取和信號生成
                if data_points % 10 == 0 { // 每10個數據點生成一次信號
                    // 簡化的特徵生成
                    let features = self.generate_mock_features(&orderbook_update);
                    
                    let market_data = MarketDataEvent {
                        symbol: orderbook_update.symbol.clone(),
                        timestamp: orderbook_update.timestamp,
                        orderbook_update,
                        features: Some(features),
                    };
                    
                    // 發送市場數據事件
                    self.engine.send_event(BarterEvent::MarketData(market_data))?;
                }
                
                if data_points % 10000 == 0 {
                    debug!("已處理 {} 個數據點", data_points);
                }
            }
        }
        
        // 計算回撤
        self.calculate_drawdowns();
        
        info!("✅ 回測完成，共處理 {} 個數據點", data_points);
        
        // 生成結果
        let results = self.results_collector.generate_results(self.config.clone());
        
        // 輸出摘要
        self.print_results_summary(&results);
        
        Ok(results)
    }
    
    /// 計算中間價
    fn calculate_mid_price(update: &OrderBookUpdate) -> Option<f64> {
        if let (Some(bid), Some(ask)) = (update.bids.first(), update.asks.first()) {
            Some((bid.price.0 + ask.price.0) / 2.0)
        } else {
            None
        }
    }
    
    /// 計算持倉價值
    fn calculate_position_value(&self) -> f64 {
        self.mock_exchange.get_positions().iter()
            .map(|(symbol, &quantity)| {
                let price = self.mock_exchange.current_prices.get(symbol).cloned().unwrap_or(0.0);
                quantity * price
            })
            .sum()
    }
    
    /// 生成模擬特徵
    fn generate_mock_features(&self, update: &OrderBookUpdate) -> FeatureSet {
        let mut features = FeatureSet::default_enhanced();
        features.timestamp = update.timestamp;
        
        if let (Some(bid), Some(ask)) = (update.bids.first(), update.asks.first()) {
            features.best_bid = bid.price;
            features.best_ask = ask.price;
            features.mid_price = ((bid.price.0 + ask.price.0) / 2.0).to_price();
            features.spread = ask.price.0 - bid.price.0;
            features.spread_bps = (features.spread / features.mid_price.0) * 10000.0;
        }
        
        // 簡化的 OBI 計算
        let bid_volume: f64 = update.bids.iter().map(|l| l.quantity.to_f64().unwrap_or(0.0)).sum();
        let ask_volume: f64 = update.asks.iter().map(|l| l.quantity.to_f64().unwrap_or(0.0)).sum();
        
        if bid_volume + ask_volume > 0.0 {
            features.obi_l10 = (bid_volume - ask_volume) / (bid_volume + ask_volume);
        }
        
        features
    }
    
    /// 計算回撤
    fn calculate_drawdowns(&mut self) {
        let mut peak = self.config.initial_capital;
        
        for point in &mut self.results_collector.equity_curve {
            if point.equity > peak {
                peak = point.equity;
            }
            point.drawdown = (peak - point.equity) / peak;
        }
    }
    
    /// 打印結果摘要
    fn print_results_summary(&self, results: &BacktestResults) {
        info!("📊 回測結果摘要:");
        info!("總收益率: {:.2}%", results.performance.total_return * 100.0);
        info!("年化收益率: {:.2}%", results.performance.annualized_return * 100.0);
        info!("夏普比率: {:.2}", results.performance.sharpe_ratio);
        info!("最大回撤: {:.2}%", results.performance.max_drawdown * 100.0);
        info!("勝率: {:.2}%", results.performance.win_rate * 100.0);
        info!("盈虧比: {:.2}", results.performance.profit_factor);
        info!("總交易次數: {}", results.statistics.total_trades);
        info!("VaR (95%): {:.4}", results.risk_metrics.value_at_risk_95);
        info!("波動率: {:.2}%", results.risk_metrics.volatility * 100.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_mock_exchange() {
        let mut exchange = MockExchange::new(10000.0, 0.001, 5.0);
        
        // 更新價格
        exchange.update_price("BTCUSDT", 50000.0);
        assert_eq!(exchange.get_equity(), 10000.0);
        
        // 執行買入交易
        let trade = exchange.execute_trade("BTCUSDT", 0.1, true).unwrap();
        assert!(trade.net_pnl < 0.0); // 由於手續費和滑點
        
        // 檢查持倉
        assert_eq!(*exchange.get_positions().get("BTCUSDT").unwrap(), 0.1);
    }
    
    #[test]
    fn test_file_data_source() {
        let data = vec![
            OrderBookUpdate::default(),
            OrderBookUpdate::default(),
        ];
        
        let mut source = FileDataSource::new(data);
        assert!(source.has_more_data());
        
        let first = source.next_data();
        assert!(first.is_some());
        
        let second = source.next_data();
        assert!(second.is_some());
        
        let third = source.next_data();
        assert!(third.is_none());
        assert!(!source.has_more_data());
    }
    
    #[test]
    fn test_backtest_config() {
        let config = BacktestConfig::default();
        assert_eq!(config.initial_capital, 10000.0);
        assert_eq!(config.commission_rate, 0.001);
    }
}