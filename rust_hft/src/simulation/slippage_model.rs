/*!
 * 💧 Slippage Model - 高精度滑点建模引擎
 * 
 * 核心功能：
 * - 多因子滑点模型：线性、平方根、固定成本模型
 * - 市场影响建模：临时冲击、永久冲击、衰减模型
 * - 实时滑点计算：基于当前市场状态和订单特征
 * - 历史数据校准：基于真实交易数据优化参数
 * 
 * 数学模型：
 * - 线性冲击：impact = linear_factor * (quantity / adv)
 * - 平方根冲击：impact = sqrt_factor * sqrt(quantity / adv)
 * - 固定成本：fixed_cost per trade
 * - 市场影响衰减：impact * exp(-decay_rate * time)
 * 
 * 设计原则：
 * - 现实性：基于真实市场微观结构特征
 * - 自适应：根据市场波动和流动性动态调整
 * - 精确性：考虑订单大小、时间、市场状态等因素
 * - 可校准：支持历史数据回测和参数优化
 */

use super::*;
use crate::core::{types::*, error::*};
use std::sync::Arc;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Mutex};
use tracing::{info, warn, error, debug, instrument};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 滑点模型
pub struct SlippageModel {
    /// 配置
    config: SlippageConfig,
    /// 市场状态缓存
    market_state: Arc<RwLock<HashMap<String, MarketState>>>,
    /// 历史滑点数据
    historical_slippage: Arc<RwLock<HashMap<String, VecDeque<SlippageRecord>>>>,
    /// 模型参数
    model_parameters: Arc<RwLock<HashMap<String, SlippageParameters>>>,
    /// 实时统计
    statistics: Arc<RwLock<SlippageStatistics>>,
    /// 校准数据
    calibration_data: Arc<RwLock<CalibrationData>>,
}

/// 滑点配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlippageConfig {
    /// 线性冲击系数
    pub linear_impact: f64,
    /// 平方根冲击系数
    pub sqrt_impact: f64,
    /// 固定成本（基点）
    pub fixed_cost: f64,
    /// 市场影响衰减率
    pub market_impact_decay: f64,
    /// 最大滑点限制
    pub max_impact: f64,
    /// 是否启用自适应模型
    pub enable_adaptive_model: bool,
    /// 校准窗口大小
    pub calibration_window: usize,
    /// 流动性调整因子
    pub liquidity_adjustment: f64,
    /// 波动率调整因子
    pub volatility_adjustment: f64,
}

/// 市场状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketState {
    /// 交易品种
    pub symbol: String,
    /// 当前价格
    pub current_price: f64,
    /// 买卖价差
    pub bid_ask_spread: f64,
    /// 平均日交易量
    pub average_daily_volume: f64,
    /// 当前波动率
    pub volatility: f64,
    /// 流动性指标
    pub liquidity_score: f64,
    /// 最后更新时间
    pub last_updated: u64,
}

/// 滑点记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlippageRecord {
    /// 记录ID
    pub record_id: String,
    /// 交易品种
    pub symbol: String,
    /// 订单数量
    pub quantity: f64,
    /// 预期价格
    pub expected_price: f64,
    /// 实际执行价格
    pub execution_price: f64,
    /// 计算滑点
    pub calculated_slippage: f64,
    /// 实际滑点
    pub actual_slippage: f64,
    /// 预测误差
    pub prediction_error: f64,
    /// 市场状态快照
    pub market_snapshot: MarketState,
    /// 时间戳
    pub timestamp: u64,
}

/// 滑点参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlippageParameters {
    /// 线性系数
    pub linear_coeff: f64,
    /// 平方根系数
    pub sqrt_coeff: f64,
    /// 固定成本
    pub fixed_cost: f64,
    /// 衰减率
    pub decay_rate: f64,
    /// 流动性敏感度
    pub liquidity_sensitivity: f64,
    /// 波动率敏感度
    pub volatility_sensitivity: f64,
    /// 参数置信度
    pub confidence_score: f64,
    /// 最后校准时间
    pub last_calibrated: u64,
}

/// 滑点统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlippageStatistics {
    /// 总计算次数
    pub total_calculations: u64,
    /// 平均预测滑点
    pub average_predicted_slippage: f64,
    /// 平均实际滑点
    pub average_actual_slippage: f64,
    /// 预测准确率
    pub prediction_accuracy: f64,
    /// 预测误差统计
    pub prediction_error_stats: ErrorStatistics,
    /// 按品种统计
    pub symbol_stats: HashMap<String, SymbolSlippageStats>,
    /// 最后更新时间
    pub last_updated: u64,
}

/// 误差统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorStatistics {
    /// 平均绝对误差
    pub mean_absolute_error: f64,
    /// 均方根误差
    pub root_mean_square_error: f64,
    /// 平均误差
    pub mean_error: f64,
    /// 误差标准差
    pub error_std_dev: f64,
    /// R平方
    pub r_squared: f64,
}

/// 品种滑点统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SymbolSlippageStats {
    /// 品种名称
    pub symbol: String,
    /// 计算次数
    pub calculation_count: u64,
    /// 平均滑点
    pub average_slippage: f64,
    /// 滑点标准差
    pub slippage_std_dev: f64,
    /// 最大滑点
    pub max_slippage: f64,
    /// 最小滑点
    pub min_slippage: f64,
    /// 预测准确率
    pub accuracy: f64,
}

/// 校准数据
#[derive(Debug, Clone, Default)]
pub struct CalibrationData {
    /// 训练数据
    pub training_data: Vec<SlippageRecord>,
    /// 验证数据
    pub validation_data: Vec<SlippageRecord>,
    /// 校准参数历史
    pub parameter_history: VecDeque<SlippageParameters>,
    /// 校准性能
    pub calibration_performance: HashMap<String, f64>,
}

impl SlippageModel {
    /// 创建新的滑点模型
    #[instrument(skip(config))]
    pub fn new(config: SlippageConfig) -> Self {
        info!("创建滑点模型");
        
        Self {
            config: config.clone(),
            market_state: Arc::new(RwLock::new(HashMap::new())),
            historical_slippage: Arc::new(RwLock::new(HashMap::new())),
            model_parameters: Arc::new(RwLock::new(HashMap::new())),
            statistics: Arc::new(RwLock::new(SlippageStatistics::default())),
            calibration_data: Arc::new(RwLock::new(CalibrationData::default())),
        }
    }
    
    /// 计算滑点
    #[instrument(skip(self))]
    pub async fn calculate_slippage(
        &self,
        symbol: &str,
        quantity: f64,
        expected_price: f64,
    ) -> f64 {
        debug!("计算滑点: {} {} @ {}", symbol, quantity, expected_price);
        
        // 获取市场状态
        let market_state = self.get_market_state(symbol).await;
        
        // 获取模型参数
        let parameters = self.get_model_parameters(symbol).await;
        
        // 计算基础滑点
        let base_slippage = self.calculate_base_slippage(
            quantity,
            expected_price,
            &market_state,
            &parameters,
        );
        
        // 应用市场调整
        let adjusted_slippage = self.apply_market_adjustments(
            base_slippage,
            &market_state,
            &parameters,
        );
        
        // 应用限制
        let final_slippage = adjusted_slippage.min(self.config.max_impact * expected_price);
        
        // 更新统计
        self.update_statistics(symbol, final_slippage).await;
        
        debug!("计算滑点完成: {} -> {:.6}", symbol, final_slippage);
        final_slippage
    }
    
    /// 更新市场状态
    #[instrument(skip(self))]
    pub async fn update_market_state(
        &self,
        symbol: &str,
        price: f64,
        spread: f64,
        volume: f64,
        volatility: f64,
    ) -> PipelineResult<()> {
        debug!("更新市场状态: {} @ {}", symbol, price);
        
        let market_state = MarketState {
            symbol: symbol.to_string(),
            current_price: price,
            bid_ask_spread: spread,
            average_daily_volume: volume,
            volatility,
            liquidity_score: self.calculate_liquidity_score(spread, volume, volatility),
            last_updated: chrono::Utc::now().timestamp_micros() as u64,
        };
        
        let mut states = self.market_state.write().await;
        states.insert(symbol.to_string(), market_state);
        
        // 如果启用自适应模型，触发重新校准
        if self.config.enable_adaptive_model {
            self.trigger_recalibration(symbol).await?;
        }
        
        Ok(())
    }
    
    /// 记录实际滑点
    #[instrument(skip(self))]
    pub async fn record_actual_slippage(
        &self,
        symbol: &str,
        quantity: f64,
        expected_price: f64,
        actual_price: f64,
    ) -> PipelineResult<()> {
        let calculated_slippage = self.calculate_slippage(symbol, quantity, expected_price).await;
        let actual_slippage = (actual_price - expected_price).abs();
        let prediction_error = (calculated_slippage - actual_slippage).abs();
        
        let market_state = self.get_market_state(symbol).await;
        
        let record = SlippageRecord {
            record_id: Uuid::new_v4().to_string(),
            symbol: symbol.to_string(),
            quantity,
            expected_price,
            execution_price: actual_price,
            calculated_slippage,
            actual_slippage,
            prediction_error,
            market_snapshot: market_state,
            timestamp: chrono::Utc::now().timestamp_micros() as u64,
        };
        
        // 记录历史数据
        let mut historical = self.historical_slippage.write().await;
        let symbol_history = historical.entry(symbol.to_string()).or_insert_with(VecDeque::new);
        symbol_history.push_back(record.clone());
        
        // 限制历史记录数量
        while symbol_history.len() > self.config.calibration_window {
            symbol_history.pop_front();
        }
        
        // 更新校准数据
        let mut calibration = self.calibration_data.write().await;
        calibration.training_data.push(record);
        
        // 限制训练数据大小
        if calibration.training_data.len() > self.config.calibration_window * 2 {
            calibration.training_data.drain(0..self.config.calibration_window);
        }
        
        info!("记录滑点: {} 预测={:.6} 实际={:.6} 误差={:.6}", 
              symbol, calculated_slippage, actual_slippage, prediction_error);
        
        Ok(())
    }
    
    /// 校准模型参数
    #[instrument(skip(self))]
    pub async fn calibrate_parameters(&self, symbol: &str) -> PipelineResult<()> {
        info!("校准滑点模型参数: {}", symbol);
        
        let calibration_data = self.calibration_data.read().await;
        let symbol_data: Vec<&SlippageRecord> = calibration_data.training_data
            .iter()
            .filter(|r| r.symbol == symbol)
            .collect();
        
        if symbol_data.len() < 20 {
            warn!("校准数据不足: {} (需要至少20个数据点)", symbol_data.len());
            return Ok(());
        }
        
        // 使用最小二乘法拟合参数
        let optimized_params = self.optimize_parameters(&symbol_data).await?;
        
        // 验证参数性能
        let performance = self.validate_parameters(&optimized_params, &symbol_data).await?;
        
        // 更新模型参数
        let mut parameters = self.model_parameters.write().await;
        parameters.insert(symbol.to_string(), optimized_params);
        
        info!("参数校准完成: {} (R² = {:.4})", symbol, performance.r_squared);
        
        Ok(())
    }
    
    /// 获取滑点统计
    pub async fn get_statistics(&self) -> SlippageStatistics {
        self.statistics.read().await.clone()
    }
    
    /// 获取模型参数
    pub async fn get_model_parameters_for_symbol(&self, symbol: &str) -> Option<SlippageParameters> {
        let parameters = self.model_parameters.read().await;
        parameters.get(symbol).cloned()
    }
    
    /// 生成滑点报告
    #[instrument(skip(self))]
    pub async fn generate_slippage_report(&self) -> String {
        let stats = self.get_statistics().await;
        let parameters = self.model_parameters.read().await;
        
        let mut report = String::from("💧 滑点模型报告\n");
        report.push_str("==================\n");
        report.push_str(&format!("总计算次数: {}\n", stats.total_calculations));
        report.push_str(&format!("平均预测滑点: {:.6}\n", stats.average_predicted_slippage));
        report.push_str(&format!("平均实际滑点: {:.6}\n", stats.average_actual_slippage));
        report.push_str(&format!("预测准确率: {:.2}%\n", stats.prediction_accuracy * 100.0));
        report.push_str(&format!("平均绝对误差: {:.6}\n", stats.prediction_error_stats.mean_absolute_error));
        report.push_str(&format!("R平方: {:.4}\n", stats.prediction_error_stats.r_squared));
        report.push_str("\n按品种统计:\n");
        
        for (symbol, symbol_stats) in &stats.symbol_stats {
            report.push_str(&format!(
                "  {}: 计算{}次, 平均滑点{:.6}, 准确率{:.1}%\n",
                symbol,
                symbol_stats.calculation_count,
                symbol_stats.average_slippage,
                symbol_stats.accuracy * 100.0
            ));
        }
        
        report.push_str("\n模型参数:\n");
        for (symbol, params) in parameters.iter() {
            report.push_str(&format!(
                "  {}: 线性={:.6}, 平方根={:.6}, 置信度={:.3}\n",
                symbol,
                params.linear_coeff,
                params.sqrt_coeff,
                params.confidence_score
            ));
        }
        
        report.push_str("==================\n");
        report
    }
    
    // 私有辅助方法
    
    /// 获取市场状态
    async fn get_market_state(&self, symbol: &str) -> MarketState {
        let states = self.market_state.read().await;
        states.get(symbol).cloned().unwrap_or_else(|| MarketState {
            symbol: symbol.to_string(),
            current_price: 50000.0, // 默认价格
            bid_ask_spread: 1.0,
            average_daily_volume: 1000000.0,
            volatility: 0.02,
            liquidity_score: 0.5,
            last_updated: chrono::Utc::now().timestamp_micros() as u64,
        })
    }
    
    /// 获取模型参数
    async fn get_model_parameters(&self, symbol: &str) -> SlippageParameters {
        let parameters = self.model_parameters.read().await;
        parameters.get(symbol).cloned().unwrap_or_else(|| SlippageParameters {
            linear_coeff: self.config.linear_impact,
            sqrt_coeff: self.config.sqrt_impact,
            fixed_cost: self.config.fixed_cost,
            decay_rate: self.config.market_impact_decay,
            liquidity_sensitivity: self.config.liquidity_adjustment,
            volatility_sensitivity: self.config.volatility_adjustment,
            confidence_score: 0.5,
            last_calibrated: 0,
        })
    }
    
    /// 计算基础滑点
    fn calculate_base_slippage(
        &self,
        quantity: f64,
        price: f64,
        market_state: &MarketState,
        parameters: &SlippageParameters,
    ) -> f64 {
        let normalized_quantity = quantity / market_state.average_daily_volume;
        
        // 线性冲击
        let linear_impact = parameters.linear_coeff * normalized_quantity;
        
        // 平方根冲击
        let sqrt_impact = parameters.sqrt_coeff * normalized_quantity.sqrt();
        
        // 固定成本
        let fixed_cost = parameters.fixed_cost * price / 10000.0; // 基点转换
        
        linear_impact + sqrt_impact + fixed_cost
    }
    
    /// 应用市场调整
    fn apply_market_adjustments(
        &self,
        base_slippage: f64,
        market_state: &MarketState,
        parameters: &SlippageParameters,
    ) -> f64 {
        // 流动性调整
        let liquidity_adjustment = 1.0 + parameters.liquidity_sensitivity * (1.0 - market_state.liquidity_score);
        
        // 波动率调整
        let volatility_adjustment = 1.0 + parameters.volatility_sensitivity * market_state.volatility;
        
        // 价差调整
        let spread_adjustment = 1.0 + market_state.bid_ask_spread / market_state.current_price;
        
        base_slippage * liquidity_adjustment * volatility_adjustment * spread_adjustment
    }
    
    /// 计算流动性得分
    fn calculate_liquidity_score(&self, spread: f64, volume: f64, volatility: f64) -> f64 {
        // 简化的流动性得分计算
        let spread_score = 1.0 / (1.0 + spread * 100.0);
        let volume_score = (volume / 1000000.0).min(1.0);
        let volatility_score = 1.0 / (1.0 + volatility * 10.0);
        
        (spread_score + volume_score + volatility_score) / 3.0
    }
    
    /// 触发重新校准
    async fn trigger_recalibration(&self, symbol: &str) -> PipelineResult<()> {
        // 检查是否需要重新校准
        let parameters = self.model_parameters.read().await;
        let needs_recalibration = if let Some(params) = parameters.get(symbol) {
            let time_since_calibration = chrono::Utc::now().timestamp_micros() as u64 - params.last_calibrated;
            time_since_calibration > 86400_000_000 * 7 // 7天重新校准一次
        } else {
            true
        };
        
        drop(parameters);
        
        if needs_recalibration {
            self.calibrate_parameters(symbol).await?;
        }
        
        Ok(())
    }
    
    /// 优化参数
    async fn optimize_parameters(&self, data: &[&SlippageRecord]) -> PipelineResult<SlippageParameters> {
        // 简化的参数优化实现
        // 实际实现会使用更复杂的优化算法（如梯度下降、遗传算法等）
        
        let mut best_params = SlippageParameters {
            linear_coeff: self.config.linear_impact,
            sqrt_coeff: self.config.sqrt_impact,
            fixed_cost: self.config.fixed_cost,
            decay_rate: self.config.market_impact_decay,
            liquidity_sensitivity: self.config.liquidity_adjustment,
            volatility_sensitivity: self.config.volatility_adjustment,
            confidence_score: 0.5,
            last_calibrated: chrono::Utc::now().timestamp_micros() as u64,
        };
        
        // 使用历史数据计算平均误差，调整参数
        let mut total_error = 0.0;
        let mut count = 0;
        
        for record in data {
            let predicted = self.calculate_base_slippage(
                record.quantity,
                record.expected_price,
                &record.market_snapshot,
                &best_params,
            );
            total_error += (predicted - record.actual_slippage).abs();
            count += 1;
        }
        
        if count > 0 {
            let avg_error = total_error / count as f64;
            best_params.confidence_score = 1.0 / (1.0 + avg_error);
        }
        
        Ok(best_params)
    }
    
    /// 验证参数性能
    async fn validate_parameters(
        &self,
        parameters: &SlippageParameters,
        data: &[&SlippageRecord],
    ) -> PipelineResult<ErrorStatistics> {
        let mut errors = Vec::new();
        let mut actuals = Vec::new();
        let mut predictions = Vec::new();
        
        for record in data {
            let predicted = self.calculate_base_slippage(
                record.quantity,
                record.expected_price,
                &record.market_snapshot,
                parameters,
            );
            
            let error = predicted - record.actual_slippage;
            errors.push(error);
            actuals.push(record.actual_slippage);
            predictions.push(predicted);
        }
        
        let mean_error = errors.iter().sum::<f64>() / errors.len() as f64;
        let mean_absolute_error = errors.iter().map(|e| e.abs()).sum::<f64>() / errors.len() as f64;
        let root_mean_square_error = (errors.iter().map(|e| e.powi(2)).sum::<f64>() / errors.len() as f64).sqrt();
        
        // 计算R平方
        let actual_mean = actuals.iter().sum::<f64>() / actuals.len() as f64;
        let ss_res = actuals.iter().zip(predictions.iter())
            .map(|(a, p)| (a - p).powi(2))
            .sum::<f64>();
        let ss_tot = actuals.iter()
            .map(|a| (a - actual_mean).powi(2))
            .sum::<f64>();
        let r_squared = if ss_tot > 0.0 { 1.0 - ss_res / ss_tot } else { 0.0 };
        
        let error_variance = errors.iter()
            .map(|e| (e - mean_error).powi(2))
            .sum::<f64>() / (errors.len() - 1) as f64;
        let error_std_dev = error_variance.sqrt();
        
        Ok(ErrorStatistics {
            mean_absolute_error,
            root_mean_square_error,
            mean_error,
            error_std_dev,
            r_squared,
        })
    }
    
    /// 更新统计信息
    async fn update_statistics(&self, symbol: &str, slippage: f64) {
        let mut stats = self.statistics.write().await;
        stats.total_calculations += 1;
        
        // 更新全局统计
        let count = stats.total_calculations as f64;
        stats.average_predicted_slippage = (stats.average_predicted_slippage * (count - 1.0) + slippage) / count;
        
        // 更新品种统计
        let symbol_stats = stats.symbol_stats.entry(symbol.to_string()).or_insert_with(|| SymbolSlippageStats {
            symbol: symbol.to_string(),
            ..Default::default()
        });
        
        symbol_stats.calculation_count += 1;
        let symbol_count = symbol_stats.calculation_count as f64;
        symbol_stats.average_slippage = (symbol_stats.average_slippage * (symbol_count - 1.0) + slippage) / symbol_count;
        symbol_stats.max_slippage = symbol_stats.max_slippage.max(slippage);
        symbol_stats.min_slippage = if symbol_stats.min_slippage == 0.0 { slippage } else { symbol_stats.min_slippage.min(slippage) };
        
        stats.last_updated = chrono::Utc::now().timestamp_micros() as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_slippage_model_creation() {
        let config = SlippageConfig::default();
        let model = SlippageModel::new(config);
        
        let stats = model.get_statistics().await;
        assert_eq!(stats.total_calculations, 0);
    }
    
    #[tokio::test]
    async fn test_slippage_calculation() {
        let config = SlippageConfig::default();
        let model = SlippageModel::new(config);
        
        // 更新市场状态
        model.update_market_state("BTCUSDT", 50000.0, 1.0, 1000000.0, 0.02).await.unwrap();
        
        // 计算滑点
        let slippage = model.calculate_slippage("BTCUSDT", 1.0, 50000.0).await;
        assert!(slippage > 0.0);
        assert!(slippage < 1000.0); // 合理范围
        
        let stats = model.get_statistics().await;
        assert_eq!(stats.total_calculations, 1);
    }
    
    #[tokio::test]
    async fn test_market_state_update() {
        let config = SlippageConfig::default();
        let model = SlippageModel::new(config);
        
        model.update_market_state("BTCUSDT", 50000.0, 1.0, 1000000.0, 0.02).await.unwrap();
        
        let market_state = model.get_market_state("BTCUSDT").await;
        assert_eq!(market_state.current_price, 50000.0);
        assert_eq!(market_state.bid_ask_spread, 1.0);
        assert!(market_state.liquidity_score > 0.0);
    }
    
    #[tokio::test]
    async fn test_slippage_recording() {
        let config = SlippageConfig::default();
        let model = SlippageModel::new(config);
        
        model.update_market_state("BTCUSDT", 50000.0, 1.0, 1000000.0, 0.02).await.unwrap();
        
        // 记录实际滑点
        model.record_actual_slippage("BTCUSDT", 1.0, 50000.0, 50005.0).await.unwrap();
        
        let calibration_data = model.calibration_data.read().await;
        assert_eq!(calibration_data.training_data.len(), 1);
        
        let record = &calibration_data.training_data[0];
        assert_eq!(record.symbol, "BTCUSDT");
        assert_eq!(record.expected_price, 50000.0);
        assert_eq!(record.execution_price, 50005.0);
        assert_eq!(record.actual_slippage, 5.0);
    }
    
    #[tokio::test]
    async fn test_parameter_calibration() {
        let config = SlippageConfig::default();
        let model = SlippageModel::new(config);
        
        model.update_market_state("BTCUSDT", 50000.0, 1.0, 1000000.0, 0.02).await.unwrap();
        
        // 添加足够的校准数据
        for i in 0..25 {
            let price = 50000.0 + i as f64 * 10.0;
            model.record_actual_slippage("BTCUSDT", 1.0, price, price + 5.0).await.unwrap();
        }
        
        // 校准参数
        model.calibrate_parameters("BTCUSDT").await.unwrap();
        
        let params = model.get_model_parameters_for_symbol("BTCUSDT").await;
        assert!(params.is_some());
        
        let params = params.unwrap();
        assert!(params.confidence_score > 0.0);
        assert!(params.last_calibrated > 0);
    }
    
    #[tokio::test]
    async fn test_slippage_report_generation() {
        let config = SlippageConfig::default();
        let model = SlippageModel::new(config);
        
        model.update_market_state("BTCUSDT", 50000.0, 1.0, 1000000.0, 0.02).await.unwrap();
        model.calculate_slippage("BTCUSDT", 1.0, 50000.0).await;
        
        let report = model.generate_slippage_report().await;
        assert!(report.contains("滑点模型报告"));
        assert!(report.contains("总计算次数"));
        assert!(report.contains("BTCUSDT"));
    }
    
    #[test]
    fn test_liquidity_score_calculation() {
        let config = SlippageConfig::default();
        let model = SlippageModel::new(config);
        
        // 测试高流动性场景
        let high_liquidity_score = model.calculate_liquidity_score(0.01, 2000000.0, 0.01);
        assert!(high_liquidity_score > 0.5);
        
        // 测试低流动性场景
        let low_liquidity_score = model.calculate_liquidity_score(1.0, 100000.0, 0.05);
        assert!(low_liquidity_score < 0.5);
    }
    
    #[test]
    fn test_base_slippage_calculation() {
        let config = SlippageConfig::default();
        let model = SlippageModel::new(config);
        
        let market_state = MarketState {
            symbol: "BTCUSDT".to_string(),
            current_price: 50000.0,
            bid_ask_spread: 1.0,
            average_daily_volume: 1000000.0,
            volatility: 0.02,
            liquidity_score: 0.8,
            last_updated: 0,
        };
        
        let parameters = SlippageParameters {
            linear_coeff: 0.001,
            sqrt_coeff: 0.0005,
            fixed_cost: 0.0001,
            decay_rate: 0.1,
            liquidity_sensitivity: 0.5,
            volatility_sensitivity: 1.0,
            confidence_score: 0.8,
            last_calibrated: 0,
        };
        
        let slippage = model.calculate_base_slippage(1000.0, 50000.0, &market_state, &parameters);
        assert!(slippage > 0.0);
        
        // 更大的订单应该产生更大的滑点
        let larger_slippage = model.calculate_base_slippage(10000.0, 50000.0, &market_state, &parameters);
        assert!(larger_slippage > slippage);
    }
}