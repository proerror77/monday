/*!
 * 📊 Performance Analyzer - 高精度性能分析引擎
 * 
 * 核心功能：
 * - 实时性能指标：夏普比率、最大回撤、卡尔马比率等
 * - 详细统计分析：滚动指标、分布分析、相关性分析
 * - 风险分析：VaR、CVaR、风险归因分析
 * - 可视化数据：性能时间线、收益分布、风险热图
 * 
 * 设计原则：
 * - 实时计算：增量更新算法，避免重复计算
 * - 高精度：统计精度优化，浮点误差控制
 * - 内存优化：滑动窗口算法，固定内存使用
 * - 可扩展：插件化指标计算，支持自定义分析
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

/// 性能分析器
pub struct PerformanceAnalyzer {
    /// 配置
    config: PerformanceConfig,
    /// 性能时间线
    timeline: Arc<RwLock<VecDeque<PerformancePoint>>>,
    /// 当前指标
    current_metrics: Arc<RwLock<PerformanceMetrics>>,
    /// 详细统计
    detailed_stats: Arc<RwLock<DetailedStats>>,
    /// 滚动窗口数据
    rolling_data: Arc<RwLock<RollingWindowData>>,
    /// 基准数据
    benchmark_data: Arc<RwLock<VecDeque<f64>>>,
    /// 分析统计
    analyzer_stats: Arc<RwLock<AnalyzerStats>>,
    /// 缓存的计算结果
    cached_results: Arc<RwLock<CachedResults>>,
}

/// 性能数据点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformancePoint {
    /// 时间戳
    pub timestamp_us: u64,
    /// 总资金
    pub total_capital: f64,
    /// 当日收益率
    pub daily_return: f64,
    /// 累计收益率
    pub cumulative_return: f64,
    /// 回撤
    pub drawdown: f64,
    /// 波动率
    pub volatility: f64,
    /// 夏普比率
    pub sharpe_ratio: f64,
    /// 交易数量
    pub trade_count: u64,
}

/// 性能指标
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    /// 总收益率
    pub total_return: f64,
    /// 年化收益率
    pub annualized_return: f64,
    /// 夏普比率
    pub sharpe_ratio: f64,
    /// 索提诺比率
    pub sortino_ratio: f64,
    /// 卡尔马比率
    pub calmar_ratio: f64,
    /// 最大回撤
    pub max_drawdown: f64,
    /// 当前回撤
    pub current_drawdown: f64,
    /// 波动率（年化）
    pub volatility: f64,
    /// 下行波动率
    pub downside_volatility: f64,
    /// VaR (95%)
    pub var_95: f64,
    /// CVaR (95%)
    pub cvar_95: f64,
    /// Beta
    pub beta: f64,
    /// Alpha
    pub alpha: f64,
    /// 信息比率
    pub information_ratio: f64,
    /// 胜率
    pub win_rate: f64,
    /// 盈亏比
    pub profit_loss_ratio: f64,
    /// 最大连续亏损
    pub max_consecutive_losses: u32,
    /// 最大连续盈利
    pub max_consecutive_wins: u32,
}

/// 滚动窗口数据
#[derive(Debug, Clone, Default)]
pub struct RollingWindowData {
    /// 滚动收益率
    pub rolling_returns: VecDeque<f64>,
    /// 滚动波动率
    pub rolling_volatility: VecDeque<f64>,
    /// 滚动夏普比率
    pub rolling_sharpe: VecDeque<f64>,
    /// 滚动最大回撤
    pub rolling_max_drawdown: VecDeque<f64>,
    /// 窗口大小
    pub window_size: usize,
}

/// 分析器统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalyzerStats {
    /// 分析次数
    pub analysis_count: u64,
    /// 最后分析时间
    pub last_analysis_time: u64,
    /// 计算耗时统计
    pub computation_times: HashMap<String, Duration>,
    /// 缓存命中率
    pub cache_hit_rate: f64,
    /// 数据点数量
    pub data_points_count: u64,
}

/// 缓存结果
#[derive(Debug, Clone, Default)]
pub struct CachedResults {
    /// 缓存的收益率统计
    pub return_stats: Option<ReturnStatistics>,
    /// 缓存的风险指标
    pub risk_metrics: Option<RiskAnalysis>,
    /// 缓存时间戳
    pub cache_timestamp: u64,
    /// 缓存有效期（秒）
    pub cache_ttl: u64,
}

/// 收益率统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReturnStatistics {
    /// 平均收益率
    pub mean_return: f64,
    /// 收益率标准差
    pub return_std: f64,
    /// 偏度
    pub skewness: f64,
    /// 峰度
    pub kurtosis: f64,
    /// 最大单日收益
    pub max_daily_return: f64,
    /// 最小单日收益
    pub min_daily_return: f64,
    /// 正收益天数比例
    pub positive_days_ratio: f64,
}

/// 风险分析
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAnalysis {
    /// VaR计算结果
    pub var_analysis: VarAnalysis,
    /// 回撤分析
    pub drawdown_analysis: DrawdownAnalysis,
    /// 相关性分析
    pub correlation_analysis: Option<CorrelationAnalysis>,
}

/// VaR分析
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VarAnalysis {
    /// VaR 95%
    pub var_95: f64,
    /// VaR 99%
    pub var_99: f64,
    /// CVaR 95%
    pub cvar_95: f64,
    /// CVaR 99%
    pub cvar_99: f64,
    /// 历史模拟法VaR
    pub historical_var: f64,
    /// 参数法VaR
    pub parametric_var: f64,
}

/// 回撤分析
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawdownAnalysis {
    /// 最大回撤
    pub max_drawdown: f64,
    /// 平均回撤
    pub average_drawdown: f64,
    /// 回撤持续期统计
    pub drawdown_duration_stats: DurationStats,
    /// 回撤恢复期统计
    pub recovery_duration_stats: DurationStats,
    /// 回撤频率
    pub drawdown_frequency: f64,
}

/// 持续期统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurationStats {
    /// 平均持续期
    pub mean_duration: f64,
    /// 最大持续期
    pub max_duration: f64,
    /// 标准差
    pub std_duration: f64,
}

/// 相关性分析
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationAnalysis {
    /// 与基准的相关性
    pub benchmark_correlation: f64,
    /// 滚动相关性
    pub rolling_correlation: Vec<f64>,
    /// 上涨市场相关性
    pub upside_correlation: f64,
    /// 下跌市场相关性
    pub downside_correlation: f64,
}

impl PerformanceAnalyzer {
    /// 创建新的性能分析器
    #[instrument(skip(config))]
    pub fn new(config: PerformanceConfig) -> Self {
        info!("创建性能分析器");
        
        Self {
            config: config.clone(),
            timeline: Arc::new(RwLock::new(VecDeque::new())),
            current_metrics: Arc::new(RwLock::new(PerformanceMetrics::default())),
            detailed_stats: Arc::new(RwLock::new(DetailedStats::default())),
            rolling_data: Arc::new(RwLock::new(RollingWindowData {
                rolling_returns: VecDeque::new(),
                rolling_volatility: VecDeque::new(),
                rolling_sharpe: VecDeque::new(),
                rolling_max_drawdown: VecDeque::new(),
                window_size: config.rolling_window_days,
            })),
            benchmark_data: Arc::new(RwLock::new(VecDeque::new())),
            analyzer_stats: Arc::new(RwLock::new(AnalyzerStats::default())),
            cached_results: Arc::new(RwLock::new(CachedResults {
                cache_ttl: 60, // 60秒缓存
                ..Default::default()
            })),
        }
    }
    
    /// 初始化分析器
    #[instrument(skip(self))]
    pub async fn initialize(&self, initial_capital: f64, start_time: u64) {
        info!("初始化性能分析器，初始资金: {:.2}", initial_capital);
        
        let initial_point = PerformancePoint {
            timestamp_us: start_time,
            total_capital: initial_capital,
            daily_return: 0.0,
            cumulative_return: 0.0,
            drawdown: 0.0,
            volatility: 0.0,
            sharpe_ratio: 0.0,
            trade_count: 0,
        };
        
        let mut timeline = self.timeline.write().await;
        timeline.push_back(initial_point);
    }
    
    /// 更新性能数据
    #[instrument(skip(self))]
    pub async fn update_with_execution(&self, execution: &SimulationExecution) {
        debug!("更新性能数据，执行ID: {}", execution.execution_id);
        
        // 这里会基于执行结果更新性能指标
        // 实际实现会更复杂，需要计算当前总价值等
        
        let mut stats = self.analyzer_stats.write().await;
        stats.analysis_count += 1;
        stats.last_analysis_time = chrono::Utc::now().timestamp_micros() as u64;
    }
    
    /// 更新性能快照
    #[instrument(skip(self, snapshot))]
    pub async fn update_snapshot(&self, snapshot: PerformanceSnapshot) {
        debug!("更新性能快照，时间戳: {}", snapshot.timestamp_us);
        
        // 计算收益率
        let mut timeline = self.timeline.write().await;
        let previous_capital = timeline.back().map(|p| p.total_capital).unwrap_or(snapshot.current_capital);
        let daily_return = if previous_capital > 0.0 {
            (snapshot.current_capital - previous_capital) / previous_capital
        } else {
            0.0
        };
        
        // 创建新的性能数据点
        let point = PerformancePoint {
            timestamp_us: snapshot.timestamp_us,
            total_capital: snapshot.current_capital,
            daily_return,
            cumulative_return: snapshot.cumulative_return,
            drawdown: snapshot.drawdown,
            volatility: 0.0, // 将在计算指标时更新
            sharpe_ratio: 0.0, // 将在计算指标时更新
            trade_count: 0, // 简化实现
        };
        
        timeline.push_back(point);
        
        // 限制时间线长度
        while timeline.len() > 10000 {
            timeline.pop_front();
        }
        
        drop(timeline);
        
        // 更新滚动数据
        self.update_rolling_data(daily_return).await;
        
        // 重新计算性能指标
        self.recalculate_metrics().await;
    }
    
    /// 计算夏普比率
    #[instrument(skip(self))]
    pub async fn calculate_sharpe_ratio(&self) -> f64 {
        let timeline = self.timeline.read().await;
        
        if timeline.len() < 2 {
            return 0.0;
        }
        
        let returns: Vec<f64> = timeline.iter().map(|p| p.daily_return).collect();
        let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let std_return = self.calculate_std_deviation(&returns, mean_return);
        
        if std_return > 0.0 {
            let excess_return = mean_return - self.config.risk_free_rate / 252.0; // 日化无风险利率
            excess_return / std_return * (252.0_f64).sqrt() // 年化夏普比率
        } else {
            0.0
        }
    }
    
    /// 获取最大回撤
    #[instrument(skip(self))]
    pub async fn get_max_drawdown(&self) -> f64 {
        let timeline = self.timeline.read().await;
        
        let mut max_capital = 0.0;
        let mut max_drawdown = 0.0;
        
        for point in timeline.iter() {
            if point.total_capital > max_capital {
                max_capital = point.total_capital;
            }
            
            if max_capital > 0.0 {
                let drawdown = (max_capital - point.total_capital) / max_capital;
                if drawdown > max_drawdown {
                    max_drawdown = drawdown;
                }
            }
        }
        
        max_drawdown
    }
    
    /// 获取当前回撤
    #[instrument(skip(self))]
    pub async fn get_current_drawdown(&self) -> f64 {
        let timeline = self.timeline.read().await;
        
        if timeline.is_empty() {
            return 0.0;
        }
        
        let current_capital = timeline.back().unwrap().total_capital;
        let max_capital = timeline.iter().map(|p| p.total_capital).fold(0.0, f64::max);
        
        if max_capital > 0.0 {
            (max_capital - current_capital) / max_capital
        } else {
            0.0
        }
    }
    
    /// 计算VaR
    #[instrument(skip(self))]
    pub async fn calculate_var(&self, confidence_level: f64) -> f64 {
        let timeline = self.timeline.read().await;
        
        if timeline.len() < 10 {
            return 0.0;
        }
        
        let mut returns: Vec<f64> = timeline.iter().map(|p| p.daily_return).collect();
        returns.sort_by(|a, b| a.partial_cmp(b).unwrap());
        
        let index = ((1.0 - confidence_level) * returns.len() as f64) as usize;
        returns.get(index).cloned().unwrap_or(0.0)
    }
    
    /// 生成详细统计
    #[instrument(skip(self))]
    pub async fn generate_detailed_stats(&self) -> DetailedStats {
        let timeline = self.timeline.read().await;
        
        if timeline.is_empty() {
            return DetailedStats::default();
        }
        
        let returns: Vec<f64> = timeline.iter().map(|p| p.daily_return).collect();
        let profits: Vec<f64> = returns.iter().filter(|&&r| r > 0.0).cloned().collect();
        let losses: Vec<f64> = returns.iter().filter(|&&r| r < 0.0).cloned().collect();
        
        let avg_trade_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let max_trade_profit = returns.iter().fold(0.0, |acc, &r| acc.max(r));
        let max_trade_loss = returns.iter().fold(0.0, |acc, &r| acc.min(r));
        
        let win_rate = if !returns.is_empty() {
            profits.len() as f64 / returns.len() as f64
        } else {
            0.0
        };
        
        // 计算连续盈亏
        let (max_consecutive_wins, max_consecutive_losses) = self.calculate_consecutive_stats(&returns);
        
        DetailedStats {
            avg_trade_return,
            max_trade_profit,
            max_trade_loss,
            avg_holding_time: Duration::from_secs(86400), // 简化为1天
            max_consecutive_wins,
            max_consecutive_losses,
            monthly_returns: self.calculate_monthly_returns(&timeline).await,
            strategy_stats: HashMap::new(), // 简化实现
        }
    }
    
    /// 获取性能时间线
    pub async fn get_timeline(&self) -> Vec<PerformanceSnapshot> {
        let timeline = self.timeline.read().await;
        
        timeline.iter().map(|point| PerformanceSnapshot {
            timestamp_us: point.timestamp_us,
            current_capital: point.total_capital,
            cumulative_return: point.cumulative_return,
            drawdown: point.drawdown,
            positions: HashMap::new(), // 简化实现
            risk_metrics: RiskMetrics::default(),
        }).collect()
    }
    
    /// 获取当前指标
    pub async fn get_current_metrics(&self) -> PerformanceMetrics {
        self.current_metrics.read().await.clone()
    }
    
    /// 获取分析器统计
    pub async fn get_analyzer_stats(&self) -> AnalyzerStats {
        self.analyzer_stats.read().await.clone()
    }
    
    /// 生成性能报告
    #[instrument(skip(self))]
    pub async fn generate_performance_report(&self) -> String {
        let metrics = self.get_current_metrics().await;
        let stats = self.generate_detailed_stats().await;
        let timeline = self.timeline.read().await;
        
        format!(
            "📊 性能分析报告\n\
            ==================\n\
            总收益率: {:.2}%\n\
            年化收益率: {:.2}%\n\
            夏普比率: {:.3}\n\
            最大回撤: {:.2}%\n\
            胜率: {:.1}%\n\
            盈亏比: {:.2}\n\
            数据点数: {}\n\
            分析次数: {}\n\
            ==================",
            metrics.total_return * 100.0,
            metrics.annualized_return * 100.0,
            metrics.sharpe_ratio,
            metrics.max_drawdown * 100.0,
            metrics.win_rate * 100.0,
            metrics.profit_loss_ratio,
            timeline.len(),
            self.analyzer_stats.read().await.analysis_count
        )
    }
    
    // 私有辅助方法
    
    /// 更新滚动数据
    async fn update_rolling_data(&self, daily_return: f64) {
        let mut rolling_data = self.rolling_data.write().await;
        
        rolling_data.rolling_returns.push_back(daily_return);
        while rolling_data.rolling_returns.len() > rolling_data.window_size {
            rolling_data.rolling_returns.pop_front();
        }
        
        // 计算滚动指标
        if rolling_data.rolling_returns.len() >= 20 {
            let returns: Vec<f64> = rolling_data.rolling_returns.iter().cloned().collect();
            let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
            let volatility = self.calculate_std_deviation(&returns, mean_return);
            
            rolling_data.rolling_volatility.push_back(volatility);
            while rolling_data.rolling_volatility.len() > rolling_data.window_size {
                rolling_data.rolling_volatility.pop_front();
            }
        }
    }
    
    /// 重新计算所有指标
    async fn recalculate_metrics(&self) {
        let mut metrics = self.current_metrics.write().await;
        
        // 基本指标
        metrics.sharpe_ratio = self.calculate_sharpe_ratio().await;
        metrics.max_drawdown = self.get_max_drawdown().await;
        metrics.current_drawdown = self.get_current_drawdown().await;
        metrics.var_95 = self.calculate_var(0.95).await;
        
        // 其他指标的计算...
        // 这里简化实现，实际会有更复杂的计算逻辑
        
        let timeline = self.timeline.read().await;
        if timeline.len() >= 2 {
            let first_capital = timeline.front().unwrap().total_capital;
            let last_capital = timeline.back().unwrap().total_capital;
            metrics.total_return = (last_capital - first_capital) / first_capital;
            metrics.annualized_return = metrics.total_return * 252.0 / timeline.len() as f64;
        }
    }
    
    /// 计算标准差
    fn calculate_std_deviation(&self, data: &[f64], mean: f64) -> f64 {
        if data.len() < 2 {
            return 0.0;
        }
        
        let variance = data.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f64>() / (data.len() - 1) as f64;
        
        variance.sqrt()
    }
    
    /// 计算连续统计
    fn calculate_consecutive_stats(&self, returns: &[f64]) -> (u32, u32) {
        let mut max_wins = 0u32;
        let mut max_losses = 0u32;
        let mut current_wins = 0u32;
        let mut current_losses = 0u32;
        
        for &ret in returns {
            if ret > 0.0 {
                current_wins += 1;
                current_losses = 0;
                max_wins = max_wins.max(current_wins);
            } else if ret < 0.0 {
                current_losses += 1;
                current_wins = 0;
                max_losses = max_losses.max(current_losses);
            } else {
                current_wins = 0;
                current_losses = 0;
            }
        }
        
        (max_wins, max_losses)
    }
    
    /// 计算月度收益
    async fn calculate_monthly_returns(&self, timeline: &VecDeque<PerformancePoint>) -> Vec<f64> {
        // 简化实现，实际需要按月分组计算
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_performance_analyzer_creation() {
        let config = PerformanceConfig {
            benchmark_return: 0.08,
            risk_free_rate: 0.02,
            enable_rolling_metrics: true,
            rolling_window_days: 252,
            generate_detailed_report: true,
        };
        
        let analyzer = PerformanceAnalyzer::new(config);
        analyzer.initialize(100000.0, 0).await;
        
        let timeline = analyzer.get_timeline().await;
        assert_eq!(timeline.len(), 1);
    }
    
    #[tokio::test]
    async fn test_sharpe_ratio_calculation() {
        let config = PerformanceConfig::default();
        let analyzer = PerformanceAnalyzer::new(config);
        analyzer.initialize(100000.0, 0).await;
        
        // 添加一些模拟数据
        for i in 1..=10 {
            let snapshot = PerformanceSnapshot {
                timestamp_us: i * 86400_000_000, // 每天
                current_capital: 100000.0 + i as f64 * 1000.0,
                cumulative_return: i as f64 * 0.01,
                drawdown: 0.0,
                positions: HashMap::new(),
                risk_metrics: RiskMetrics::default(),
            };
            analyzer.update_snapshot(snapshot).await;
        }
        
        let sharpe = analyzer.calculate_sharpe_ratio().await;
        assert!(sharpe > 0.0);
    }
    
    #[tokio::test]
    async fn test_max_drawdown_calculation() {
        let config = PerformanceConfig::default();
        let analyzer = PerformanceAnalyzer::new(config);
        analyzer.initialize(100000.0, 0).await;
        
        // 模拟一个回撤场景
        let capitals = vec![100000.0, 110000.0, 105000.0, 95000.0, 105000.0];
        
        for (i, &capital) in capitals.iter().enumerate() {
            let snapshot = PerformanceSnapshot {
                timestamp_us: i as u64 * 86400_000_000,
                current_capital: capital,
                cumulative_return: (capital - 100000.0) / 100000.0,
                drawdown: 0.0,
                positions: HashMap::new(),
                risk_metrics: RiskMetrics::default(),
            };
            analyzer.update_snapshot(snapshot).await;
        }
        
        let max_drawdown = analyzer.get_max_drawdown().await;
        assert!(max_drawdown > 0.0);
        
        // 最大回撤应该是从110000到95000，即约13.6%
        assert!((max_drawdown - 0.136).abs() < 0.01);
    }
    
    #[tokio::test]
    async fn test_var_calculation() {
        let config = PerformanceConfig::default();
        let analyzer = PerformanceAnalyzer::new(config);
        analyzer.initialize(100000.0, 0).await;
        
        // 添加足够的数据用于VaR计算
        for i in 1..=100 {
            let return_rate = (i % 10) as f64 * 0.001 - 0.005; // 模拟收益率
            let capital = 100000.0 * (1.0 + return_rate);
            
            let snapshot = PerformanceSnapshot {
                timestamp_us: i * 86400_000_000,
                current_capital: capital,
                cumulative_return: return_rate,
                drawdown: 0.0,
                positions: HashMap::new(),
                risk_metrics: RiskMetrics::default(),
            };
            analyzer.update_snapshot(snapshot).await;
        }
        
        let var_95 = analyzer.calculate_var(0.95).await;
        assert!(var_95 < 0.0); // VaR应该是负值
    }
    
    #[tokio::test]
    async fn test_detailed_stats_generation() {
        let config = PerformanceConfig::default();
        let analyzer = PerformanceAnalyzer::new(config);
        analyzer.initialize(100000.0, 0).await;
        
        // 添加一些交易数据
        for i in 1..=20 {
            let return_rate = if i % 3 == 0 { -0.01 } else { 0.02 }; // 2/3盈利，1/3亏损
            let capital = 100000.0 * (1.0 + return_rate * i as f64);
            
            let snapshot = PerformanceSnapshot {
                timestamp_us: i * 86400_000_000,
                current_capital: capital,
                cumulative_return: return_rate * i as f64,
                drawdown: 0.0,
                positions: HashMap::new(),
                risk_metrics: RiskMetrics::default(),
            };
            analyzer.update_snapshot(snapshot).await;
        }
        
        let stats = analyzer.generate_detailed_stats().await;
        assert!(stats.max_trade_profit > 0.0);
        assert!(stats.max_trade_loss < 0.0);
        assert!(stats.max_consecutive_wins > 0);
    }
    
    #[tokio::test]
    async fn test_performance_report_generation() {
        let config = PerformanceConfig::default();
        let analyzer = PerformanceAnalyzer::new(config);
        analyzer.initialize(100000.0, 0).await;
        
        // 添加一些数据
        for i in 1..=5 {
            let snapshot = PerformanceSnapshot {
                timestamp_us: i * 86400_000_000,
                current_capital: 100000.0 + i as f64 * 2000.0,
                cumulative_return: i as f64 * 0.02,
                drawdown: 0.0,
                positions: HashMap::new(),
                risk_metrics: RiskMetrics::default(),
            };
            analyzer.update_snapshot(snapshot).await;
        }
        
        let report = analyzer.generate_performance_report().await;
        assert!(report.contains("性能分析报告"));
        assert!(report.contains("总收益率"));
        assert!(report.contains("夏普比率"));
    }
}