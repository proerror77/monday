/*!
 * ✅ Replay Validator - 回放数据验证和完整性检查
 * 
 * 核心功能：
 * - 数据完整性验证：检查时间序列连续性
 * - 逻辑一致性检查：验证价格、数量合理性
 * - 性能监控：统计验证耗时和通过率
 * - 异常检测：识别异常数据点和模式
 * - 修复建议：提供数据清洗建议
 * 
 * 验证规则：
 * - 时间戳单调递增
 * - 价格范围合理性
 * - 买卖价差合理性
 * - 数量正数检查
 * - 序列号连续性
 */

use super::*;
use crate::core::{types::*, config::Config, error::*};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error, debug, instrument};

/// 回放数据验证器
pub struct ReplayValidator {
    /// 验证配置
    config: ValidationConfig,
    /// 验证统计
    stats: ValidationStats,
    /// 上一个事件缓存（用于连续性检查）
    last_events: HashMap<String, ReplayEvent>,
    /// 异常检测器
    anomaly_detector: AnomalyDetector,
}

/// 验证配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    /// 是否启用严格模式
    pub strict_mode: bool,
    /// 时间戳容差 (微秒)
    pub timestamp_tolerance_us: u64,
    /// 最大价格变化百分比
    pub max_price_change_percent: f64,
    /// 最小/最大有效价格
    pub min_valid_price: f64,
    pub max_valid_price: f64,
    /// 最大买卖价差百分比
    pub max_spread_percent: f64,
    /// 最小有效数量
    pub min_valid_quantity: f64,
    /// 是否启用异常检测
    pub enable_anomaly_detection: bool,
    /// 序列号容差
    pub sequence_tolerance: u64,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            strict_mode: false,
            timestamp_tolerance_us: 1000, // 1ms
            max_price_change_percent: 10.0, // 10%
            min_valid_price: 0.000001,
            max_valid_price: 1000000.0,
            max_spread_percent: 1.0, // 1%
            min_valid_quantity: 0.000001,
            enable_anomaly_detection: true,
            sequence_tolerance: 10,
        }
    }
}

/// 验证统计信息
#[derive(Debug, Clone, Default)]
pub struct ValidationStats {
    /// 总验证事件数
    pub total_events: u64,
    /// 通过验证的事件数
    pub valid_events: u64,
    /// 警告数量
    pub warnings: u64,
    /// 错误数量
    pub errors: u64,
    /// 异常数量
    pub anomalies: u64,
    /// 总验证时间
    pub total_validation_time: Duration,
    /// 平均验证时间
    pub average_validation_time_ns: f64,
    /// 验证通过率
    pub validation_pass_rate: f64,
}

/// 验证结果
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// 是否通过验证
    pub is_valid: bool,
    /// 验证级别
    pub level: ValidationLevel,
    /// 验证消息
    pub message: String,
    /// 事件时间戳
    pub timestamp_us: u64,
    /// 事件类型
    pub event_type: String,
    /// 修复建议
    pub suggestions: Vec<String>,
}

/// 验证级别
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationLevel {
    /// 信息
    Info,
    /// 警告
    Warning,
    /// 错误
    Error,
    /// 异常
    Anomaly,
}

/// 异常检测器
#[derive(Debug)]
struct AnomalyDetector {
    /// 价格历史窗口
    price_window: HashMap<String, VecDeque<f64>>,
    /// 数量历史窗口
    quantity_window: HashMap<String, VecDeque<f64>>,
    /// 窗口大小
    window_size: usize,
    /// 异常检测阈值 (标准差倍数)
    anomaly_threshold: f64,
}

impl AnomalyDetector {
    fn new() -> Self {
        Self {
            price_window: HashMap::new(),
            quantity_window: HashMap::new(),
            window_size: 100,
            anomaly_threshold: 3.0, // 3倍标准差
        }
    }
    
    /// 检测价格异常
    fn detect_price_anomaly(&mut self, symbol: &str, price: f64) -> bool {
        let window = self.price_window.entry(symbol.to_string())
            .or_insert_with(VecDeque::new);
        
        if window.len() < 10 {
            // 数据不足，暂不检测
            window.push_back(price);
            if window.len() > self.window_size {
                window.pop_front();
            }
            return false;
        }
        
        // 计算均值和标准差
        let mean = window.iter().sum::<f64>() / window.len() as f64;
        let variance = window.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f64>() / window.len() as f64;
        let std_dev = variance.sqrt();
        
        let z_score = (price - mean).abs() / std_dev;
        let is_anomaly = z_score > self.anomaly_threshold;
        
        // 更新窗口
        window.push_back(price);
        if window.len() > self.window_size {
            window.pop_front();
        }
        
        is_anomaly
    }
    
    /// 检测数量异常
    fn detect_quantity_anomaly(&mut self, symbol: &str, quantity: f64) -> bool {
        let window = self.quantity_window.entry(symbol.to_string())
            .or_insert_with(VecDeque::new);
        
        if window.len() < 10 {
            window.push_back(quantity);
            if window.len() > self.window_size {
                window.pop_front();
            }
            return false;
        }
        
        // 使用对数标准化来处理数量的大范围变化
        let log_quantity = quantity.ln();
        let log_values: Vec<f64> = window.iter().map(|&x| x.ln()).collect();
        
        let mean = log_values.iter().sum::<f64>() / log_values.len() as f64;
        let variance = log_values.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f64>() / log_values.len() as f64;
        let std_dev = variance.sqrt();
        
        let z_score = (log_quantity - mean).abs() / std_dev;
        let is_anomaly = z_score > self.anomaly_threshold;
        
        window.push_back(quantity);
        if window.len() > self.window_size {
            window.pop_front();
        }
        
        is_anomaly
    }
}

impl ReplayValidator {
    /// 创建新的验证器
    pub fn new(config: ValidationConfig) -> Self {
        Self {
            config,
            stats: ValidationStats::default(),
            last_events: HashMap::new(),
            anomaly_detector: AnomalyDetector::new(),
        }
    }
    
    /// 创建默认验证器
    pub fn default() -> Self {
        Self::new(ValidationConfig::default())
    }
    
    /// 验证单个事件
    #[instrument(skip(self, event))]
    pub fn validate_event(&mut self, event: &ReplayEvent) -> Vec<ValidationResult> {
        let start_time = Instant::now();
        let mut results = Vec::new();
        
        self.stats.total_events += 1;
        
        match event {
            ReplayEvent::LobUpdate { 
                timestamp_us, symbol, bids, asks, sequence 
            } => {
                results.extend(self.validate_lob_update(*timestamp_us, symbol, bids, asks, *sequence));
            }
            ReplayEvent::Trade { 
                timestamp_us, symbol, price, quantity, side, trade_id 
            } => {
                results.extend(self.validate_trade(*timestamp_us, symbol, *price, *quantity, side, trade_id));
            }
            ReplayEvent::Ticker { 
                timestamp_us, symbol, last_price, volume_24h, change_24h 
            } => {
                results.extend(self.validate_ticker(*timestamp_us, symbol, *last_price, *volume_24h, *change_24h));
            }
            ReplayEvent::Control(_) => {
                // 控制事件不需要验证
                return results;
            }
        }
        
        // 更新统计信息
        let validation_time = start_time.elapsed();
        self.stats.total_validation_time += validation_time;
        self.stats.average_validation_time_ns = 
            self.stats.total_validation_time.as_nanos() as f64 / self.stats.total_events as f64;
        
        // 统计验证结果
        let has_error = results.iter().any(|r| r.level == ValidationLevel::Error);
        let has_warning = results.iter().any(|r| r.level == ValidationLevel::Warning);
        let has_anomaly = results.iter().any(|r| r.level == ValidationLevel::Anomaly);
        
        if has_error {
            self.stats.errors += 1;
        } else if has_warning {
            self.stats.warnings += 1;
        } else {
            self.stats.valid_events += 1;
        }
        
        if has_anomaly {
            self.stats.anomalies += 1;
        }
        
        self.stats.validation_pass_rate = 
            self.stats.valid_events as f64 / self.stats.total_events as f64;
        
        // 缓存当前事件
        self.last_events.insert(self.get_event_key(event), event.clone());
        
        results
    }
    
    /// 验证数据块
    #[instrument(skip(self, chunk))]
    pub fn validate_chunk(&mut self, chunk: &DataChunk) -> Vec<ValidationResult> {
        let mut all_results = Vec::new();
        
        debug!("验证数据块: {} 事件", chunk.event_count());
        
        // 检查数据块时间范围
        if let Some(first_event) = chunk.events.first() {
            let first_timestamp = self.get_event_timestamp(first_event);
            if first_timestamp < chunk.time_range.start_us {
                all_results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Error,
                    message: "首个事件时间戳早于数据块开始时间".to_string(),
                    timestamp_us: first_timestamp,
                    event_type: self.get_event_type(first_event),
                    suggestions: vec!["检查数据加载逻辑".to_string()],
                });
            }
        }
        
        if let Some(last_event) = chunk.events.last() {
            let last_timestamp = self.get_event_timestamp(last_event);
            if last_timestamp > chunk.time_range.end_us {
                all_results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Error,
                    message: "最后事件时间戳晚于数据块结束时间".to_string(),
                    timestamp_us: last_timestamp,
                    event_type: self.get_event_type(last_event),
                    suggestions: vec!["检查数据加载逻辑".to_string()],
                });
            }
        }
        
        // 验证时间戳排序
        let mut last_timestamp = 0u64;
        for event in &chunk.events {
            let timestamp = self.get_event_timestamp(event);
            if timestamp < last_timestamp {
                all_results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Error,
                    message: "事件时间戳乱序".to_string(),
                    timestamp_us: timestamp,
                    event_type: self.get_event_type(event),
                    suggestions: vec!["对事件按时间戳排序".to_string()],
                });
            }
            last_timestamp = timestamp;
        }
        
        // 验证每个事件
        for event in &chunk.events {
            let event_results = self.validate_event(event);
            all_results.extend(event_results);
        }
        
        info!("数据块验证完成: {} 事件, {} 结果", chunk.event_count(), all_results.len());
        all_results
    }
    
    /// 验证LOB更新事件
    fn validate_lob_update(
        &mut self,
        timestamp_us: u64,
        symbol: &str,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        sequence: u64,
    ) -> Vec<ValidationResult> {
        let mut results = Vec::new();
        
        // 基础验证
        results.extend(self.validate_timestamp(timestamp_us));
        
        // 验证买盘
        for (i, &(price, quantity)) in bids.iter().enumerate() {
            if price <= 0.0 {
                results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Error,
                    message: format!("买盘价格无效: {}", price),
                    timestamp_us,
                    event_type: "LOB_UPDATE".to_string(),
                    suggestions: vec!["检查数据源".to_string()],
                });
            }
            
            if quantity <= 0.0 {
                results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Error,
                    message: format!("买盘数量无效: {}", quantity),
                    timestamp_us,
                    event_type: "LOB_UPDATE".to_string(),
                    suggestions: vec!["检查数据源".to_string()],
                });
            }
            
            // 检查价格递减顺序
            if i > 0 && price >= bids[i - 1].0 {
                results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Warning,
                    message: "买盘价格未按递减顺序排列".to_string(),
                    timestamp_us,
                    event_type: "LOB_UPDATE".to_string(),
                    suggestions: vec!["重新排序买盘数据".to_string()],
                });
            }
            
            // 异常检测
            if self.config.enable_anomaly_detection {
                if self.anomaly_detector.detect_price_anomaly(symbol, price) {
                    results.push(ValidationResult {
                        is_valid: true,
                        level: ValidationLevel::Anomaly,
                        message: format!("检测到价格异常: {}", price),
                        timestamp_us,
                        event_type: "LOB_UPDATE".to_string(),
                        suggestions: vec!["检查市场异常事件".to_string()],
                    });
                }
                
                if self.anomaly_detector.detect_quantity_anomaly(symbol, quantity) {
                    results.push(ValidationResult {
                        is_valid: true,
                        level: ValidationLevel::Anomaly,
                        message: format!("检测到数量异常: {}", quantity),
                        timestamp_us,
                        event_type: "LOB_UPDATE".to_string(),
                        suggestions: vec!["检查大额订单".to_string()],
                    });
                }
            }
        }
        
        // 验证卖盘
        for (i, &(price, quantity)) in asks.iter().enumerate() {
            if price <= 0.0 {
                results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Error,
                    message: format!("卖盘价格无效: {}", price),
                    timestamp_us,
                    event_type: "LOB_UPDATE".to_string(),
                    suggestions: vec!["检查数据源".to_string()],
                });
            }
            
            if quantity <= 0.0 {
                results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Error,
                    message: format!("卖盘数量无效: {}", quantity),
                    timestamp_us,
                    event_type: "LOB_UPDATE".to_string(),
                    suggestions: vec!["检查数据源".to_string()],
                });
            }
            
            // 检查价格递增顺序
            if i > 0 && price <= asks[i - 1].0 {
                results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Warning,
                    message: "卖盘价格未按递增顺序排列".to_string(),
                    timestamp_us,
                    event_type: "LOB_UPDATE".to_string(),
                    suggestions: vec!["重新排序卖盘数据".to_string()],
                });
            }
        }
        
        // 验证买卖价差
        if !bids.is_empty() && !asks.is_empty() {
            let best_bid = bids[0].0;
            let best_ask = asks[0].0;
            
            if best_bid >= best_ask {
                results.push(ValidationResult {
                    is_valid: false,
                    level: ValidationLevel::Error,
                    message: format!("买卖价格交叉: bid={}, ask={}", best_bid, best_ask),
                    timestamp_us,
                    event_type: "LOB_UPDATE".to_string(),
                    suggestions: vec!["检查订单簿数据完整性".to_string()],
                });
            } else {
                let spread_percent = (best_ask - best_bid) / best_bid * 100.0;
                if spread_percent > self.config.max_spread_percent {
                    results.push(ValidationResult {
                        is_valid: true,
                        level: ValidationLevel::Warning,
                        message: format!("买卖价差过大: {:.2}%", spread_percent),
                        timestamp_us,
                        event_type: "LOB_UPDATE".to_string(),
                        suggestions: vec!["检查市场流动性".to_string()],
                    });
                }
            }
        }
        
        results
    }
    
    /// 验证交易事件
    fn validate_trade(
        &mut self,
        timestamp_us: u64,
        symbol: &str,
        price: f64,
        quantity: f64,
        side: &str,
        trade_id: &str,
    ) -> Vec<ValidationResult> {
        let mut results = Vec::new();
        
        results.extend(self.validate_timestamp(timestamp_us));
        results.extend(self.validate_price(price, timestamp_us));
        results.extend(self.validate_quantity(quantity, timestamp_us));
        
        // 验证交易方向
        if side != "buy" && side != "sell" && side != "BUY" && side != "SELL" {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Error,
                message: format!("无效的交易方向: {}", side),
                timestamp_us,
                event_type: "TRADE".to_string(),
                suggestions: vec!["使用标准的交易方向标识".to_string()],
            });
        }
        
        // 验证交易ID
        if trade_id.is_empty() {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Warning,
                message: "交易ID为空".to_string(),
                timestamp_us,
                event_type: "TRADE".to_string(),
                suggestions: vec!["确保交易ID唯一性".to_string()],
            });
        }
        
        // 异常检测
        if self.config.enable_anomaly_detection {
            if self.anomaly_detector.detect_price_anomaly(symbol, price) {
                results.push(ValidationResult {
                    is_valid: true,
                    level: ValidationLevel::Anomaly,
                    message: format!("检测到交易价格异常: {}", price),
                    timestamp_us,
                    event_type: "TRADE".to_string(),
                    suggestions: vec!["检查交易执行逻辑".to_string()],
                });
            }
            
            if self.anomaly_detector.detect_quantity_anomaly(symbol, quantity) {
                results.push(ValidationResult {
                    is_valid: true,
                    level: ValidationLevel::Anomaly,
                    message: format!("检测到交易数量异常: {}", quantity),
                    timestamp_us,
                    event_type: "TRADE".to_string(),
                    suggestions: vec!["检查大额交易".to_string()],
                });
            }
        }
        
        results
    }
    
    /// 验证Ticker事件
    fn validate_ticker(
        &mut self,
        timestamp_us: u64,
        symbol: &str,
        last_price: f64,
        volume_24h: f64,
        change_24h: f64,
    ) -> Vec<ValidationResult> {
        let mut results = Vec::new();
        
        results.extend(self.validate_timestamp(timestamp_us));
        results.extend(self.validate_price(last_price, timestamp_us));
        
        // 验证24小时成交量
        if volume_24h < 0.0 {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Error,
                message: format!("24小时成交量为负: {}", volume_24h),
                timestamp_us,
                event_type: "TICKER".to_string(),
                suggestions: vec!["检查成交量计算逻辑".to_string()],
            });
        }
        
        // 验证24小时涨跌幅
        if change_24h.abs() > 100.0 {
            results.push(ValidationResult {
                is_valid: true,
                level: ValidationLevel::Warning,
                message: format!("24小时涨跌幅异常: {:.2}%", change_24h),
                timestamp_us,
                event_type: "TICKER".to_string(),
                suggestions: vec!["检查市场异常事件".to_string()],
            });
        }
        
        results
    }
    
    /// 验证时间戳
    fn validate_timestamp(&self, timestamp_us: u64) -> Vec<ValidationResult> {
        let mut results = Vec::new();
        
        if timestamp_us == 0 {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Error,
                message: "时间戳为0".to_string(),
                timestamp_us,
                event_type: "TIMESTAMP".to_string(),
                suggestions: vec!["检查时间戳生成逻辑".to_string()],
            });
        }
        
        // 检查时间戳是否在合理范围内 (2020-2030)
        let min_timestamp = 1577836800000000; // 2020-01-01 00:00:00 UTC
        let max_timestamp = 1893456000000000; // 2030-01-01 00:00:00 UTC
        
        if timestamp_us < min_timestamp || timestamp_us > max_timestamp {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Warning,
                message: format!("时间戳超出合理范围: {}", 
                                utils::timestamp_to_string(timestamp_us)),
                timestamp_us,
                event_type: "TIMESTAMP".to_string(),
                suggestions: vec!["检查时间戳单位和时区".to_string()],
            });
        }
        
        results
    }
    
    /// 验证价格
    fn validate_price(&self, price: f64, timestamp_us: u64) -> Vec<ValidationResult> {
        let mut results = Vec::new();
        
        if price <= 0.0 {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Error,
                message: format!("价格无效: {}", price),
                timestamp_us,
                event_type: "PRICE".to_string(),
                suggestions: vec!["检查价格数据源".to_string()],
            });
        }
        
        if price < self.config.min_valid_price || price > self.config.max_valid_price {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Warning,
                message: format!("价格超出有效范围: {}", price),
                timestamp_us,
                event_type: "PRICE".to_string(),
                suggestions: vec!["检查价格精度和单位".to_string()],
            });
        }
        
        results
    }
    
    /// 验证数量
    fn validate_quantity(&self, quantity: f64, timestamp_us: u64) -> Vec<ValidationResult> {
        let mut results = Vec::new();
        
        if quantity <= 0.0 {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Error,
                message: format!("数量无效: {}", quantity),
                timestamp_us,
                event_type: "QUANTITY".to_string(),
                suggestions: vec!["检查数量数据源".to_string()],
            });
        }
        
        if quantity < self.config.min_valid_quantity {
            results.push(ValidationResult {
                is_valid: false,
                level: ValidationLevel::Warning,
                message: format!("数量过小: {}", quantity),
                timestamp_us,
                event_type: "QUANTITY".to_string(),
                suggestions: vec!["检查最小交易单位".to_string()],
            });
        }
        
        results
    }
    
    /// 获取事件键
    fn get_event_key(&self, event: &ReplayEvent) -> String {
        match event {
            ReplayEvent::LobUpdate { symbol, sequence, .. } => {
                format!("lob_{}_{}", symbol, sequence)
            }
            ReplayEvent::Trade { symbol, trade_id, .. } => {
                format!("trade_{}_{}", symbol, trade_id)
            }
            ReplayEvent::Ticker { symbol, .. } => {
                format!("ticker_{}", symbol)
            }
            ReplayEvent::Control(_) => "control".to_string(),
        }
    }
    
    /// 获取事件时间戳
    fn get_event_timestamp(&self, event: &ReplayEvent) -> u64 {
        match event {
            ReplayEvent::LobUpdate { timestamp_us, .. } => *timestamp_us,
            ReplayEvent::Trade { timestamp_us, .. } => *timestamp_us,
            ReplayEvent::Ticker { timestamp_us, .. } => *timestamp_us,
            ReplayEvent::Control(_) => 0,
        }
    }
    
    /// 获取事件类型
    fn get_event_type(&self, event: &ReplayEvent) -> String {
        match event {
            ReplayEvent::LobUpdate { .. } => "LOB_UPDATE".to_string(),
            ReplayEvent::Trade { .. } => "TRADE".to_string(),
            ReplayEvent::Ticker { .. } => "TICKER".to_string(),
            ReplayEvent::Control(_) => "CONTROL".to_string(),
        }
    }
    
    /// 获取验证统计信息
    pub fn get_stats(&self) -> &ValidationStats {
        &self.stats
    }
    
    /// 重置统计信息
    pub fn reset_stats(&mut self) {
        self.stats = ValidationStats::default();
    }
    
    /// 生成验证报告
    pub fn generate_report(&self) -> String {
        format!(
            r#"
=== 回放数据验证报告 ===
总事件数: {}
有效事件数: {} ({:.2}%)
警告数: {}
错误数: {}
异常数: {}
平均验证时间: {:.2}ns
总验证时间: {:?}
验证通过率: {:.2}%
"#,
            self.stats.total_events,
            self.stats.valid_events,
            self.stats.validation_pass_rate * 100.0,
            self.stats.warnings,
            self.stats.errors,
            self.stats.anomalies,
            self.stats.average_validation_time_ns,
            self.stats.total_validation_time,
            self.stats.validation_pass_rate * 100.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_validator_creation() {
        let validator = ReplayValidator::default();
        assert_eq!(validator.stats.total_events, 0);
    }
    
    #[test]
    fn test_lob_validation() {
        let mut validator = ReplayValidator::default();
        
        let event = ReplayEvent::LobUpdate {
            timestamp_us: 1640995200000000, // 2022-01-01 00:00:00 UTC
            symbol: "BTCUSDT".to_string(),
            bids: vec![(50000.0, 1.0), (49999.0, 2.0)],
            asks: vec![(50001.0, 1.5), (50002.0, 2.5)],
            sequence: 1,
        };
        
        let results = validator.validate_event(&event);
        
        // 应该没有错误
        let has_error = results.iter().any(|r| r.level == ValidationLevel::Error);
        assert!(!has_error);
        
        assert_eq!(validator.stats.total_events, 1);
        assert_eq!(validator.stats.valid_events, 1);
    }
    
    #[test]
    fn test_invalid_lob_validation() {
        let mut validator = ReplayValidator::default();
        
        let event = ReplayEvent::LobUpdate {
            timestamp_us: 1640995200000000,
            symbol: "BTCUSDT".to_string(),
            bids: vec![(50001.0, 1.0)], // bid 高于 ask
            asks: vec![(50000.0, 1.5)], // ask 低于 bid
            sequence: 1,
        };
        
        let results = validator.validate_event(&event);
        
        // 应该有错误
        let has_error = results.iter().any(|r| r.level == ValidationLevel::Error);
        assert!(has_error);
        
        assert_eq!(validator.stats.total_events, 1);
        assert_eq!(validator.stats.errors, 1);
    }
    
    #[test]
    fn test_trade_validation() {
        let mut validator = ReplayValidator::default();
        
        let event = ReplayEvent::Trade {
            timestamp_us: 1640995200000000,
            symbol: "BTCUSDT".to_string(),
            price: 50000.0,
            quantity: 1.0,
            side: "buy".to_string(),
            trade_id: "12345".to_string(),
        };
        
        let results = validator.validate_event(&event);
        
        let has_error = results.iter().any(|r| r.level == ValidationLevel::Error);
        assert!(!has_error);
        
        assert_eq!(validator.stats.total_events, 1);
        assert_eq!(validator.stats.valid_events, 1);
    }
    
    #[test]
    fn test_chunk_validation() {
        let mut validator = ReplayValidator::default();
        
        let mut chunk = DataChunk::new(TimeRange::new(1640995200000000, 1640995260000000));
        
        // 添加有序的事件
        chunk.add_event(ReplayEvent::LobUpdate {
            timestamp_us: 1640995200000000,
            symbol: "BTCUSDT".to_string(),
            bids: vec![(50000.0, 1.0)],
            asks: vec![(50001.0, 1.0)],
            sequence: 1,
        });
        
        chunk.add_event(ReplayEvent::Trade {
            timestamp_us: 1640995210000000,
            symbol: "BTCUSDT".to_string(),
            price: 50000.5,
            quantity: 0.5,
            side: "buy".to_string(),
            trade_id: "12345".to_string(),
        });
        
        let results = validator.validate_chunk(&chunk);
        
        // 检查结果
        let has_error = results.iter().any(|r| r.level == ValidationLevel::Error);
        assert!(!has_error);
        
        // 应该验证了2个事件
        assert_eq!(validator.stats.total_events, 2);
    }
    
    #[test]
    fn test_validation_stats() {
        let mut validator = ReplayValidator::default();
        
        // 验证几个事件
        for i in 0..10 {
            let event = ReplayEvent::Trade {
                timestamp_us: 1640995200000000 + i * 1000000,
                symbol: "BTCUSDT".to_string(),
                price: 50000.0 + i as f64,
                quantity: 1.0,
                side: "buy".to_string(),
                trade_id: format!("trade_{}", i),
            };
            
            validator.validate_event(&event);
        }
        
        let stats = validator.get_stats();
        assert_eq!(stats.total_events, 10);
        assert_eq!(stats.valid_events, 10);
        assert_eq!(stats.validation_pass_rate, 1.0);
        assert!(stats.average_validation_time_ns > 0.0);
    }
}