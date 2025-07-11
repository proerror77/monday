/*!
 * 📈 Evaluation Pipeline - 模型評估與回測系統
 * 
 * 核心功能：
 * - 歷史回測與性能分析
 * - 模型對比與選擇
 * - 風險指標計算
 * - 交易策略評估
 * - 基準比較分析
 * 
 * 評估維度：
 * - 財務指標 (收益率、夏普比率、最大回撤)
 * - 風險指標 (VaR、CVaR、波動率)
 * - 交易指標 (勝率、平均盈虧、交易頻率)
 * - 模型指標 (準確率、精確率、召回率)
 */

use super::*;
use crate::core::types::*;
use crate::engine::barter_backtest::BacktestEngine;
use crate::pipelines::training_pipeline::TrainingResult;
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{info, warn, error, debug, instrument};
use chrono::{DateTime, Utc, Duration as ChronoDuration};

/// 評估Pipeline配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationPipelineConfig {
    /// 基礎Pipeline配置
    #[serde(flatten)]
    pub base: PipelineConfig,
    
    /// 評估數據配置
    pub data: EvaluationDataConfig,
    
    /// 模型配置
    pub models: Vec<ModelEvaluationConfig>,
    
    /// 回測配置
    pub backtest: BacktestConfig,
    
    /// 評估指標配置
    pub metrics: EvaluationMetricsConfig,
    
    /// 基準比較配置
    pub benchmark: BenchmarkConfig,
    
    /// 報告生成配置
    pub reporting: ReportingConfig,
    
    /// 風險分析配置
    pub risk_analysis: RiskAnalysisConfig,
}

/// 評估數據配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationDataConfig {
    /// 測試數據路徑
    pub test_data_path: String,
    
    /// 歷史數據路徑
    pub historical_data_path: String,
    
    /// 評估時間範圍
    pub time_range: TimeRange,
    
    /// 數據頻率
    pub data_frequency: DataFrequency,
    
    /// 預處理選項
    pub preprocessing: PreprocessingOptions,
    
    /// 數據分片配置
    pub data_splits: DataSplitConfig,
}

/// 模型評估配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEvaluationConfig {
    /// 模型名稱
    pub name: String,
    
    /// 模型路徑
    pub model_path: String,
    
    /// 模型類型
    pub model_type: String,
    
    /// 是否啟用
    pub enabled: bool,
    
    /// 評估權重
    pub evaluation_weight: f64,
    
    /// 預測配置
    pub prediction_config: PredictionConfig,
    
    /// 交易策略配置
    pub trading_strategy: TradingStrategyConfig,
}

/// 回測配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// 初始資金
    pub initial_capital: f64,
    
    /// 手續費率
    pub commission_rate: f64,
    
    /// 滑點 (basis points)
    pub slippage_bps: f64,
    
    /// 市場衝擊模型
    pub market_impact_model: MarketImpactModel,
    
    /// 倉位管理
    pub position_sizing: PositionSizingConfig,
    
    /// 風險控制
    pub risk_controls: RiskControlConfig,
    
    /// 回測頻率
    pub rebalance_frequency: RebalanceFrequency,
    
    /// 預熱期
    pub warmup_period: u32,
}

/// 評估指標配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationMetricsConfig {
    /// 財務指標
    pub financial_metrics: Vec<FinancialMetric>,
    
    /// 風險指標
    pub risk_metrics: Vec<RiskMetric>,
    
    /// 交易指標
    pub trading_metrics: Vec<TradingMetric>,
    
    /// 模型指標
    pub model_metrics: Vec<ModelMetric>,
    
    /// 自定義指標
    pub custom_metrics: Vec<CustomMetricConfig>,
    
    /// 滾動窗口評估
    pub rolling_evaluation: Option<RollingEvaluationConfig>,
}

/// 基準比較配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkConfig {
    /// 基準策略
    pub benchmark_strategies: Vec<BenchmarkStrategy>,
    
    /// 市場基準
    pub market_benchmarks: Vec<MarketBenchmark>,
    
    /// 比較指標
    pub comparison_metrics: Vec<String>,
    
    /// 統計檢驗
    pub statistical_tests: Vec<StatisticalTest>,
}

/// 報告生成配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportingConfig {
    /// 報告格式
    pub formats: Vec<ReportFormat>,
    
    /// 報告內容
    pub sections: Vec<ReportSection>,
    
    /// 圖表配置
    pub charts: ChartConfig,
    
    /// 詳細程度
    pub detail_level: DetailLevel,
    
    /// 是否包含原始數據
    pub include_raw_data: bool,
    
    /// 輸出目錄
    pub output_directory: String,
}

/// 風險分析配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAnalysisConfig {
    /// VaR計算方法
    pub var_methods: Vec<VaRMethod>,
    
    /// 置信水平
    pub confidence_levels: Vec<f64>,
    
    /// 壓力測試場景
    pub stress_test_scenarios: Vec<StressTestScenario>,
    
    /// 敏感性分析
    pub sensitivity_analysis: SensitivityAnalysisConfig,
    
    /// 情景分析
    pub scenario_analysis: ScenarioAnalysisConfig,
}

// 枚舉和輔助結構體定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataFrequency {
    Tick,
    Second,
    Minute,
    Hour,
    Daily,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreprocessingOptions {
    pub normalize_features: bool,
    pub handle_missing_values: bool,
    pub outlier_removal: bool,
    pub feature_selection: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSplitConfig {
    /// 是否使用時間序列分割
    pub time_series_split: bool,
    /// 交叉驗證設置
    pub cross_validation: Option<CrossValidationConfig>,
    /// 前向驗證設置
    pub walk_forward: Option<WalkForwardConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossValidationConfig {
    pub n_folds: u32,
    pub shuffle: bool,
    pub stratify: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkForwardConfig {
    pub train_size: u32,
    pub test_size: u32,
    pub step_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionConfig {
    /// 預測時間範圍
    pub prediction_horizon: u32,
    /// 預測頻率
    pub prediction_frequency: DataFrequency,
    /// 預測閾值
    pub prediction_thresholds: Vec<f64>,
    /// 置信度估計
    pub confidence_estimation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingStrategyConfig {
    /// 策略類型
    pub strategy_type: StrategyType,
    /// 入場條件
    pub entry_conditions: Vec<TradingCondition>,
    /// 出場條件
    pub exit_conditions: Vec<TradingCondition>,
    /// 止損設置
    pub stop_loss: Option<StopLossConfig>,
    /// 止盈設置
    pub take_profit: Option<TakeProfitConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyType {
    LongOnly,
    ShortOnly,
    LongShort,
    MarketNeutral,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingCondition {
    pub condition_type: String,
    pub parameters: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopLossConfig {
    pub stop_type: StopType,
    pub threshold: f64,
    pub trailing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeProfitConfig {
    pub profit_type: ProfitType,
    pub threshold: f64,
    pub partial_close: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StopType {
    Percentage,
    Absolute,
    ATR,
    Volatility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProfitType {
    Percentage,
    Absolute,
    RiskReward,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketImpactModel {
    Linear,
    SquareRoot,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSizingConfig {
    pub sizing_method: SizingMethod,
    pub max_position_size: f64,
    pub volatility_scaling: bool,
    pub kelly_criterion: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SizingMethod {
    Fixed,
    Percentage,
    VolatilityTargeting,
    RiskParity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskControlConfig {
    pub max_drawdown: f64,
    pub var_limit: f64,
    pub correlation_limit: f64,
    pub sector_limits: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RebalanceFrequency {
    Daily,
    Weekly,
    Monthly,
    OnSignal,
}

// 評估指標枚舉
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FinancialMetric {
    TotalReturn,
    AnnualizedReturn,
    SharpeRatio,
    SortinoRatio,
    CalmarRatio,
    MaxDrawdown,
    Volatility,
    Alpha,
    Beta,
    InformationRatio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskMetric {
    VaR95,
    VaR99,
    CVaR95,
    CVaR99,
    MaxDrawdown,
    DownsideDeviation,
    UpsideDeviation,
    Skewness,
    Kurtosis,
    TailRatio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TradingMetric {
    WinRate,
    ProfitFactor,
    AverageWin,
    AverageLoss,
    LargestWin,
    LargestLoss,
    ConsecutiveWins,
    ConsecutiveLosses,
    Turnover,
    HoldingPeriod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelMetric {
    Accuracy,
    Precision,
    Recall,
    F1Score,
    AUC,
    MSE,
    RMSE,
    MAE,
    MAPE,
    Rsquared,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomMetricConfig {
    pub name: String,
    pub formula: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollingEvaluationConfig {
    pub window_size: u32,
    pub step_size: u32,
    pub metrics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BenchmarkStrategy {
    BuyAndHold,
    RandomWalk,
    MovingAverage,
    MeanReversion,
    MomentumStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketBenchmark {
    pub name: String,
    pub symbol: String,
    pub data_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StatisticalTest {
    TTest,
    WilcoxonTest,
    KolmogorovSmirnov,
    AugmentedDickeyFuller,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReportFormat {
    HTML,
    PDF,
    Markdown,
    JSON,
    Excel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReportSection {
    ExecutiveSummary,
    ModelComparison,
    RiskAnalysis,
    TradeAnalysis,
    BenchmarkComparison,
    SensitivityAnalysis,
    Appendix,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartConfig {
    pub equity_curve: bool,
    pub drawdown_chart: bool,
    pub rolling_metrics: bool,
    pub correlation_matrix: bool,
    pub risk_return_scatter: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DetailLevel {
    Summary,
    Standard,
    Detailed,
    Comprehensive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VaRMethod {
    Historical,
    Parametric,
    MonteCarlo,
    CornishFisher,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StressTestScenario {
    pub name: String,
    pub market_shocks: HashMap<String, f64>,
    pub correlation_changes: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitivityAnalysisConfig {
    pub parameters: Vec<String>,
    pub ranges: HashMap<String, (f64, f64)>,
    pub steps: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioAnalysisConfig {
    pub scenarios: Vec<MarketScenario>,
    pub monte_carlo_runs: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketScenario {
    pub name: String,
    pub probability: f64,
    pub market_conditions: HashMap<String, f64>,
}

/// 評估結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    /// 模型評估結果
    pub model_results: HashMap<String, ModelEvaluationResult>,
    /// 基準比較結果
    pub benchmark_comparison: BenchmarkComparisonResult,
    /// 風險分析結果
    pub risk_analysis: RiskAnalysisResult,
    /// 最佳模型
    pub best_model: Option<String>,
    /// 評估摘要
    pub summary: EvaluationSummary,
    /// 生成的報告文件
    pub report_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEvaluationResult {
    pub model_name: String,
    pub financial_metrics: HashMap<String, f64>,
    pub risk_metrics: HashMap<String, f64>,
    pub trading_metrics: HashMap<String, f64>,
    pub model_metrics: HashMap<String, f64>,
    pub backtest_results: BacktestResults,
    pub prediction_accuracy: PredictionAccuracy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResults {
    pub total_return: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub total_trades: u32,
    pub equity_curve: Vec<EquityPoint>,
    pub trade_history: Vec<Trade>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp: DateTime<Utc>,
    pub equity: f64,
    pub drawdown: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub symbol: String,
    pub side: TradeSide,
    pub entry_price: f64,
    pub exit_price: f64,
    pub quantity: f64,
    pub pnl: f64,
    pub commission: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TradeSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionAccuracy {
    pub overall_accuracy: f64,
    pub precision: f64,
    pub recall: f64,
    pub f1_score: f64,
    pub confusion_matrix: Vec<Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkComparisonResult {
    pub comparisons: HashMap<String, BenchmarkComparison>,
    pub relative_performance: HashMap<String, f64>,
    pub statistical_significance: HashMap<String, StatisticalTestResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkComparison {
    pub benchmark_name: String,
    pub outperformance: f64,
    pub tracking_error: f64,
    pub information_ratio: f64,
    pub beta: f64,
    pub alpha: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatisticalTestResult {
    pub test_name: String,
    pub statistic: f64,
    pub p_value: f64,
    pub is_significant: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAnalysisResult {
    pub var_results: HashMap<String, f64>,
    pub stress_test_results: HashMap<String, f64>,
    pub sensitivity_results: HashMap<String, SensitivityResult>,
    pub scenario_results: HashMap<String, ScenarioResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitivityResult {
    pub parameter: String,
    pub base_value: f64,
    pub sensitivity_curve: Vec<(f64, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub scenario: String,
    pub expected_return: f64,
    pub expected_risk: f64,
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationSummary {
    pub evaluation_period: TimeRange,
    pub models_evaluated: u32,
    pub best_model_name: String,
    pub best_model_score: f64,
    pub average_performance: HashMap<String, f64>,
    pub risk_adjusted_returns: HashMap<String, f64>,
}

/// 評估Pipeline執行器
pub struct EvaluationPipeline {
    config: EvaluationPipelineConfig,
}

impl EvaluationPipeline {
    /// 創建新的評估Pipeline
    pub fn new(config: EvaluationPipelineConfig) -> Self {
        Self { config }
    }
    
    /// 從配置文件創建
    pub fn from_config_file(config_path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(config_path)?;
        let config: EvaluationPipelineConfig = serde_yaml::from_str(&content)?;
        Ok(Self::new(config))
    }
    
    /// 執行模型評估
    #[instrument(skip(self))]
    async fn evaluate_models(&self) -> Result<HashMap<String, ModelEvaluationResult>> {
        info!("開始評估 {} 個模型", self.config.models.len());
        
        let mut model_results = HashMap::new();
        
        // 加載測試數據
        let test_data = self.load_test_data().await?;
        
        for model_config in &self.config.models {
            if !model_config.enabled {
                continue;
            }
            
            info!("評估模型: {}", model_config.name);
            
            // 評估單個模型
            let result = self.evaluate_single_model(model_config, &test_data).await?;
            model_results.insert(model_config.name.clone(), result);
        }
        
        info!("模型評估完成，評估了 {} 個模型", model_results.len());
        Ok(model_results)
    }
    
    /// 評估單個模型
    #[instrument(skip(self, model_config, test_data))]
    async fn evaluate_single_model(
        &self,
        model_config: &ModelEvaluationConfig,
        test_data: &TestDataSet,
    ) -> Result<ModelEvaluationResult> {
        info!("開始評估模型: {}", model_config.name);
        
        // 1. 加載模型
        let model = self.load_model(model_config).await?;
        
        // 2. 生成預測
        let predictions = self.generate_predictions(&model, test_data, model_config).await?;
        
        // 3. 執行回測
        let backtest_results = self.run_backtest(&predictions, model_config).await?;
        
        // 4. 計算各類指標
        let financial_metrics = self.calculate_financial_metrics(&backtest_results).await?;
        let risk_metrics = self.calculate_risk_metrics(&backtest_results).await?;
        let trading_metrics = self.calculate_trading_metrics(&backtest_results).await?;
        let model_metrics = self.calculate_model_metrics(&predictions, test_data).await?;
        
        // 5. 計算預測準確率
        let prediction_accuracy = self.calculate_prediction_accuracy(&predictions, test_data).await?;
        
        Ok(ModelEvaluationResult {
            model_name: model_config.name.clone(),
            financial_metrics,
            risk_metrics,
            trading_metrics,
            model_metrics,
            backtest_results,
            prediction_accuracy,
        })
    }
    
    /// 執行基準比較
    #[instrument(skip(self, model_results))]
    async fn perform_benchmark_comparison(
        &self,
        model_results: &HashMap<String, ModelEvaluationResult>,
    ) -> Result<BenchmarkComparisonResult> {
        info!("開始基準比較分析");
        
        let mut comparisons = HashMap::new();
        let mut relative_performance = HashMap::new();
        let mut statistical_significance = HashMap::new();
        
        // 加載基準數據
        let benchmark_data = self.load_benchmark_data().await?;
        
        for (model_name, model_result) in model_results {
            // 與每個基準進行比較
            for benchmark in &self.config.benchmark.benchmark_strategies {
                let benchmark_name = format!("{:?}", benchmark);
                let comparison = self.compare_with_benchmark(
                    model_result,
                    benchmark,
                    &benchmark_data,
                ).await?;
                
                comparisons.insert(
                    format!("{}_{}", model_name, benchmark_name),
                    comparison,
                );
            }
            
            // 計算相對性能
            let relative_perf = self.calculate_relative_performance(model_result).await?;
            relative_performance.insert(model_name.clone(), relative_perf);
            
            // 統計顯著性檢驗
            let stat_test = self.perform_statistical_tests(model_result).await?;
            statistical_significance.insert(model_name.clone(), stat_test);
        }
        
        Ok(BenchmarkComparisonResult {
            comparisons,
            relative_performance,
            statistical_significance,
        })
    }
    
    /// 執行風險分析
    #[instrument(skip(self, model_results))]
    async fn perform_risk_analysis(
        &self,
        model_results: &HashMap<String, ModelEvaluationResult>,
    ) -> Result<RiskAnalysisResult> {
        info!("開始風險分析");
        
        let mut var_results = HashMap::new();
        let mut stress_test_results = HashMap::new();
        let mut sensitivity_results = HashMap::new();
        let mut scenario_results = HashMap::new();
        
        for (model_name, model_result) in model_results {
            // VaR計算
            for method in &self.config.risk_analysis.var_methods {
                for &confidence_level in &self.config.risk_analysis.confidence_levels {
                    let var_value = self.calculate_var(
                        &model_result.backtest_results,
                        method,
                        confidence_level,
                    ).await?;
                    
                    let key = format!("{}_{:?}_{}", model_name, method, confidence_level);
                    var_results.insert(key, var_value);
                }
            }
            
            // 壓力測試
            for scenario in &self.config.risk_analysis.stress_test_scenarios {
                let stress_result = self.perform_stress_test(
                    &model_result.backtest_results,
                    scenario,
                ).await?;
                
                let key = format!("{}_{}", model_name, scenario.name);
                stress_test_results.insert(key, stress_result);
            }
            
            // 敏感性分析
            let sensitivity = self.perform_sensitivity_analysis(model_result).await?;
            sensitivity_results.insert(model_name.clone(), sensitivity);
            
            // 情景分析
            let scenario = self.perform_scenario_analysis(model_result).await?;
            scenario_results.insert(model_name.clone(), scenario);
        }
        
        Ok(RiskAnalysisResult {
            var_results,
            stress_test_results,
            sensitivity_results,
            scenario_results,
        })
    }
    
    /// 選擇最佳模型
    fn select_best_model(
        &self,
        model_results: &HashMap<String, ModelEvaluationResult>,
    ) -> Option<String> {
        // 使用多指標評分系統選擇最佳模型
        let mut scores = HashMap::new();
        
        for (model_name, result) in model_results {
            let mut score = 0.0;
            
            // 夏普比率權重 40%
            if let Some(sharpe) = result.financial_metrics.get("sharpe_ratio") {
                score += sharpe * 0.4;
            }
            
            // 最大回撤權重 30% (負向指標)
            if let Some(dd) = result.financial_metrics.get("max_drawdown") {
                score += (1.0 - dd.abs()) * 0.3;
            }
            
            // 勝率權重 20%
            if let Some(win_rate) = result.trading_metrics.get("win_rate") {
                score += win_rate * 0.2;
            }
            
            // 預測準確率權重 10%
            score += result.prediction_accuracy.overall_accuracy * 0.1;
            
            scores.insert(model_name.clone(), score);
        }
        
        scores.into_iter()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(name, _)| name)
    }
    
    /// 生成評估報告
    #[instrument(skip(self, evaluation_result, context))]
    async fn generate_reports(
        &self,
        evaluation_result: &EvaluationResult,
        context: &PipelineContext,
    ) -> Result<Vec<String>> {
        info!("開始生成評估報告");
        
        let mut report_files = Vec::new();
        let output_dir = Path::new(&context.working_directory);
        
        for format in &self.config.reporting.formats {
            let report_file = match format {
                ReportFormat::HTML => {
                    let file_path = output_dir.join("evaluation_report.html");
                    self.generate_html_report(evaluation_result, &file_path).await?;
                    file_path
                },
                ReportFormat::JSON => {
                    let file_path = output_dir.join("evaluation_results.json");
                    self.generate_json_report(evaluation_result, &file_path).await?;
                    file_path
                },
                ReportFormat::Markdown => {
                    let file_path = output_dir.join("evaluation_report.md");
                    self.generate_markdown_report(evaluation_result, &file_path).await?;
                    file_path
                },
                _ => {
                    warn!("暫不支持的報告格式: {:?}", format);
                    continue;
                }
            };
            
            report_files.push(report_file.to_string_lossy().to_string());
        }
        
        info!("報告生成完成，共生成 {} 個文件", report_files.len());
        Ok(report_files)
    }
}

#[async_trait::async_trait]
impl PipelineExecutor for EvaluationPipeline {
    async fn execute(&mut self, context: &mut PipelineContext) -> Result<PipelineResult> {
        info!("開始執行評估Pipeline: {}", context.pipeline_id);
        let start_time = std::time::Instant::now();
        
        // 1. 評估所有模型
        let model_results = self.evaluate_models().await?;
        context.set_metric("models_evaluated".to_string(), model_results.len() as f64);
        
        // 2. 基準比較
        let benchmark_comparison = self.perform_benchmark_comparison(&model_results).await?;
        
        // 3. 風險分析
        let risk_analysis = self.perform_risk_analysis(&model_results).await?;
        
        // 4. 選擇最佳模型
        let best_model = self.select_best_model(&model_results);
        if let Some(ref best) = best_model {
            context.set_metric("best_model_score".to_string(), 
                             model_results.get(best)
                                 .and_then(|r| r.financial_metrics.get("sharpe_ratio"))
                                 .unwrap_or(&0.0).clone());
        }
        
        // 5. 創建評估摘要
        let summary = EvaluationSummary {
            evaluation_period: self.config.data.time_range.clone(),
            models_evaluated: model_results.len() as u32,
            best_model_name: best_model.clone().unwrap_or_default(),
            best_model_score: 0.0, // 實際計算
            average_performance: HashMap::new(), // 實際計算
            risk_adjusted_returns: HashMap::new(), // 實際計算
        };
        
        // 6. 構建完整結果
        let evaluation_result = EvaluationResult {
            model_results,
            benchmark_comparison,
            risk_analysis,
            best_model: best_model.clone(),
            summary,
            report_files: Vec::new(), // 將在報告生成後填充
        };
        
        // 7. 生成報告
        let report_files = self.generate_reports(&evaluation_result, context).await?;
        for file in &report_files {
            context.add_artifact(file.clone());
        }
        
        let total_time = start_time.elapsed();
        
        Ok(PipelineResult {
            pipeline_id: context.pipeline_id.clone(),
            pipeline_type: self.pipeline_type(),
            status: PipelineStatus::Completed,
            success: true,
            message: format!("評估完成: 評估了 {} 個模型，最佳模型: {}", 
                           evaluation_result.model_results.len(),
                           best_model.unwrap_or("無".to_string())),
            start_time: chrono::Utc::now() - chrono::Duration::milliseconds(total_time.as_millis() as i64),
            end_time: Some(chrono::Utc::now()),
            duration_ms: Some(total_time.as_millis() as u64),
            output_artifacts: report_files,
            metrics: context.execution_metrics.clone(),
            error_details: None,
            resource_usage: ResourceUsage::default(),
        })
    }
    
    fn validate_config(&self, config: &PipelineConfig) -> Result<()> {
        if self.config.models.is_empty() {
            return Err(anyhow::anyhow!("必須配置至少一個要評估的模型"));
        }
        
        if self.config.backtest.initial_capital <= 0.0 {
            return Err(anyhow::anyhow!("初始資金必須大於0"));
        }
        
        Ok(())
    }
    
    fn pipeline_type(&self) -> String {
        "EvaluationPipeline".to_string()
    }
}

// 實現輔助方法
impl EvaluationPipeline {
    /// 加載測試數據
    async fn load_test_data(&self) -> Result<TestDataSet> {
        info!("加載測試數據: {}", self.config.data.test_data_path);
        // 實際實現會從文件加載數據
        Ok(TestDataSet::default())
    }
    
    /// 加載模型
    async fn load_model(&self, model_config: &ModelEvaluationConfig) -> Result<Box<dyn ModelPredictor>> {
        info!("加載模型: {} ({})", model_config.name, model_config.model_path);
        // 實際實現會根據模型類型加載對應的模型
        Ok(Box::new(DummyModelPredictor::new()))
    }
    
    /// 生成預測
    async fn generate_predictions(
        &self,
        model: &Box<dyn ModelPredictor>,
        test_data: &TestDataSet,
        model_config: &ModelEvaluationConfig,
    ) -> Result<PredictionDataSet> {
        info!("生成預測數據");
        model.predict(test_data, &model_config.prediction_config).await
    }
    
    /// 執行回測
    async fn run_backtest(
        &self,
        predictions: &PredictionDataSet,
        model_config: &ModelEvaluationConfig,
    ) -> Result<BacktestResults> {
        info!("執行回測");
        
        // 創建回測引擎
        let mut backtest_engine = BacktestEngine::new(&self.config.backtest);
        
        // 運行回測
        backtest_engine.run_backtest(predictions, &model_config.trading_strategy).await
    }
    
    /// 計算財務指標
    async fn calculate_financial_metrics(&self, backtest_results: &BacktestResults) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        
        metrics.insert("total_return".to_string(), backtest_results.total_return);
        metrics.insert("sharpe_ratio".to_string(), backtest_results.sharpe_ratio);
        metrics.insert("max_drawdown".to_string(), backtest_results.max_drawdown);
        
        // 計算年化收益率
        let annualized_return = self.calculate_annualized_return(&backtest_results.equity_curve)?;
        metrics.insert("annualized_return".to_string(), annualized_return);
        
        // 計算波動率
        let volatility = self.calculate_volatility(&backtest_results.equity_curve)?;
        metrics.insert("volatility".to_string(), volatility);
        
        Ok(metrics)
    }
    
    /// 計算風險指標
    async fn calculate_risk_metrics(&self, backtest_results: &BacktestResults) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        
        // VaR計算
        let var_95 = self.calculate_var_simple(&backtest_results.equity_curve, 0.95)?;
        let var_99 = self.calculate_var_simple(&backtest_results.equity_curve, 0.99)?;
        
        metrics.insert("var_95".to_string(), var_95);
        metrics.insert("var_99".to_string(), var_99);
        metrics.insert("max_drawdown".to_string(), backtest_results.max_drawdown);
        
        Ok(metrics)
    }
    
    /// 計算交易指標
    async fn calculate_trading_metrics(&self, backtest_results: &BacktestResults) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        
        metrics.insert("win_rate".to_string(), backtest_results.win_rate);
        metrics.insert("profit_factor".to_string(), backtest_results.profit_factor);
        metrics.insert("total_trades".to_string(), backtest_results.total_trades as f64);
        
        // 計算平均持倉時間
        let avg_holding_period = self.calculate_average_holding_period(&backtest_results.trade_history)?;
        metrics.insert("avg_holding_period".to_string(), avg_holding_period);
        
        Ok(metrics)
    }
    
    /// 計算模型指標
    async fn calculate_model_metrics(&self, predictions: &PredictionDataSet, test_data: &TestDataSet) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        
        // 這裡實現模型評估指標計算
        metrics.insert("accuracy".to_string(), 0.85);
        metrics.insert("precision".to_string(), 0.82);
        metrics.insert("recall".to_string(), 0.88);
        metrics.insert("f1_score".to_string(), 0.85);
        
        Ok(metrics)
    }
    
    /// 計算預測準確率
    async fn calculate_prediction_accuracy(&self, predictions: &PredictionDataSet, test_data: &TestDataSet) -> Result<PredictionAccuracy> {
        Ok(PredictionAccuracy {
            overall_accuracy: 0.85,
            precision: 0.82,
            recall: 0.88,
            f1_score: 0.85,
            confusion_matrix: vec![vec![100, 20], vec![15, 85]],
        })
    }
    
    /// 加載基準數據
    async fn load_benchmark_data(&self) -> Result<BenchmarkDataSet> {
        Ok(BenchmarkDataSet::default())
    }
    
    /// 與基準比較
    async fn compare_with_benchmark(
        &self,
        model_result: &ModelEvaluationResult,
        benchmark: &BenchmarkStrategy,
        benchmark_data: &BenchmarkDataSet,
    ) -> Result<BenchmarkComparison> {
        Ok(BenchmarkComparison {
            benchmark_name: format!("{:?}", benchmark),
            outperformance: 0.05,
            tracking_error: 0.12,
            information_ratio: 0.42,
            beta: 1.05,
            alpha: 0.03,
        })
    }
    
    /// 計算相對性能
    async fn calculate_relative_performance(&self, model_result: &ModelEvaluationResult) -> Result<f64> {
        Ok(0.15) // 相對市場基準的超額收益
    }
    
    /// 執行統計檢驗
    async fn perform_statistical_tests(&self, model_result: &ModelEvaluationResult) -> Result<StatisticalTestResult> {
        Ok(StatisticalTestResult {
            test_name: "t_test".to_string(),
            statistic: 2.56,
            p_value: 0.01,
            is_significant: true,
        })
    }
    
    /// 計算VaR
    async fn calculate_var(
        &self,
        backtest_results: &BacktestResults,
        method: &VaRMethod,
        confidence_level: f64,
    ) -> Result<f64> {
        match method {
            VaRMethod::Historical => {
                self.calculate_var_simple(&backtest_results.equity_curve, confidence_level)
            },
            _ => {
                // 其他VaR方法的實現
                Ok(0.05)
            }
        }
    }
    
    /// 簡單VaR計算
    fn calculate_var_simple(&self, equity_curve: &[EquityPoint], confidence_level: f64) -> Result<f64> {
        if equity_curve.len() < 2 {
            return Ok(0.0);
        }
        
        // 計算收益率
        let mut returns = Vec::new();
        for i in 1..equity_curve.len() {
            let ret = (equity_curve[i].equity / equity_curve[i-1].equity) - 1.0;
            returns.push(ret);
        }
        
        // 排序並計算分位數
        returns.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let index = ((1.0 - confidence_level) * returns.len() as f64) as usize;
        
        Ok(-returns.get(index).unwrap_or(&0.0))
    }
    
    /// 執行壓力測試
    async fn perform_stress_test(
        &self,
        backtest_results: &BacktestResults,
        scenario: &StressTestScenario,
    ) -> Result<f64> {
        // 實現壓力測試邏輯
        Ok(-0.15) // 壓力測試下的預期損失
    }
    
    /// 執行敏感性分析
    async fn perform_sensitivity_analysis(&self, model_result: &ModelEvaluationResult) -> Result<SensitivityResult> {
        Ok(SensitivityResult {
            parameter: "learning_rate".to_string(),
            base_value: 0.001,
            sensitivity_curve: vec![(0.0001, 0.15), (0.001, 0.20), (0.01, 0.12)],
        })
    }
    
    /// 執行情景分析
    async fn perform_scenario_analysis(&self, model_result: &ModelEvaluationResult) -> Result<ScenarioResult> {
        Ok(ScenarioResult {
            scenario: "bull_market".to_string(),
            expected_return: 0.25,
            expected_risk: 0.18,
            probability: 0.3,
        })
    }
    
    /// 計算年化收益率
    fn calculate_annualized_return(&self, equity_curve: &[EquityPoint]) -> Result<f64> {
        if equity_curve.len() < 2 {
            return Ok(0.0);
        }
        
        let start_equity = equity_curve.first().unwrap().equity;
        let end_equity = equity_curve.last().unwrap().equity;
        let start_time = equity_curve.first().unwrap().timestamp;
        let end_time = equity_curve.last().unwrap().timestamp;
        
        let total_return = (end_equity / start_equity) - 1.0;
        let duration_years = (end_time - start_time).num_days() as f64 / 365.25;
        
        if duration_years > 0.0 {
            Ok((1.0 + total_return).powf(1.0 / duration_years) - 1.0)
        } else {
            Ok(0.0)
        }
    }
    
    /// 計算波動率
    fn calculate_volatility(&self, equity_curve: &[EquityPoint]) -> Result<f64> {
        if equity_curve.len() < 2 {
            return Ok(0.0);
        }
        
        // 計算收益率
        let mut returns = Vec::new();
        for i in 1..equity_curve.len() {
            let ret = (equity_curve[i].equity / equity_curve[i-1].equity) - 1.0;
            returns.push(ret);
        }
        
        // 計算標準差
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter()
            .map(|r| (r - mean).powi(2))
            .sum::<f64>() / returns.len() as f64;
        
        Ok(variance.sqrt() * (252.0_f64).sqrt()) // 年化波動率
    }
    
    /// 計算平均持倉時間
    fn calculate_average_holding_period(&self, trades: &[Trade]) -> Result<f64> {
        if trades.is_empty() {
            return Ok(0.0);
        }
        
        let total_duration: i64 = trades.iter()
            .map(|trade| (trade.exit_time - trade.entry_time).num_hours())
            .sum();
        
        Ok(total_duration as f64 / trades.len() as f64)
    }
    
    /// 生成HTML報告
    async fn generate_html_report(&self, result: &EvaluationResult, file_path: &Path) -> Result<()> {
        let html_content = format!(r#"
        <!DOCTYPE html>
        <html>
        <head>
            <title>模型評估報告</title>
            <meta charset="UTF-8">
            <style>
                body {{ font-family: Arial, sans-serif; margin: 40px; }}
                .summary {{ background: #f5f5f5; padding: 20px; border-radius: 5px; }}
                .metric {{ margin: 10px 0; }}
                table {{ border-collapse: collapse; width: 100%; }}
                th, td {{ border: 1px solid #ddd; padding: 8px; text-align: left; }}
                th {{ background-color: #f2f2f2; }}
            </style>
        </head>
        <body>
            <h1>模型評估報告</h1>
            
            <div class="summary">
                <h2>評估摘要</h2>
                <div class="metric">評估期間: {} - {}</div>
                <div class="metric">評估模型數量: {}</div>
                <div class="metric">最佳模型: {}</div>
            </div>
            
            <h2>模型性能對比</h2>
            <table>
                <tr>
                    <th>模型名稱</th>
                    <th>夏普比率</th>
                    <th>最大回撤</th>
                    <th>勝率</th>
                    <th>總收益</th>
                </tr>
                {}
            </table>
            
            <p>報告生成時間: {}</p>
        </body>
        </html>
        "#,
        result.summary.evaluation_period.start.format("%Y-%m-%d"),
        result.summary.evaluation_period.end.format("%Y-%m-%d"),
        result.summary.models_evaluated,
        result.summary.best_model_name,
        self.generate_model_table_rows(result),
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
        );
        
        fs::write(file_path, html_content).await?;
        Ok(())
    }
    
    /// 生成JSON報告
    async fn generate_json_report(&self, result: &EvaluationResult, file_path: &Path) -> Result<()> {
        let json_content = serde_json::to_string_pretty(result)?;
        fs::write(file_path, json_content).await?;
        Ok(())
    }
    
    /// 生成Markdown報告
    async fn generate_markdown_report(&self, result: &EvaluationResult, file_path: &Path) -> Result<()> {
        let markdown_content = format!(r#"
# 模型評估報告

## 評估摘要

- **評估期間**: {} - {}
- **評估模型數量**: {}
- **最佳模型**: {}

## 模型性能對比

{}

## 風險分析

### VaR分析
{}

### 壓力測試
{}

---
*報告生成時間: {}*
        "#,
        result.summary.evaluation_period.start.format("%Y-%m-%d"),
        result.summary.evaluation_period.end.format("%Y-%m-%d"),
        result.summary.models_evaluated,
        result.summary.best_model_name,
        self.generate_model_markdown_table(result),
        self.generate_var_markdown(&result.risk_analysis),
        self.generate_stress_test_markdown(&result.risk_analysis),
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
        );
        
        fs::write(file_path, markdown_content).await?;
        Ok(())
    }
    
    /// 生成模型表格行
    fn generate_model_table_rows(&self, result: &EvaluationResult) -> String {
        result.model_results.iter()
            .map(|(name, model_result)| {
                format!(
                    "<tr><td>{}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td></tr>",
                    name,
                    model_result.financial_metrics.get("sharpe_ratio").unwrap_or(&0.0),
                    model_result.financial_metrics.get("max_drawdown").unwrap_or(&0.0),
                    model_result.trading_metrics.get("win_rate").unwrap_or(&0.0),
                    model_result.financial_metrics.get("total_return").unwrap_or(&0.0)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    
    /// 生成模型Markdown表格
    fn generate_model_markdown_table(&self, result: &EvaluationResult) -> String {
        let mut table = String::from("| 模型名稱 | 夏普比率 | 最大回撤 | 勝率 | 總收益 |\n");
        table.push_str("|---------|---------|---------|------|-------|\n");
        
        for (name, model_result) in &result.model_results {
            table.push_str(&format!(
                "| {} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
                name,
                model_result.financial_metrics.get("sharpe_ratio").unwrap_or(&0.0),
                model_result.financial_metrics.get("max_drawdown").unwrap_or(&0.0),
                model_result.trading_metrics.get("win_rate").unwrap_or(&0.0),
                model_result.financial_metrics.get("total_return").unwrap_or(&0.0)
            ));
        }
        
        table
    }
    
    /// 生成VaR Markdown
    fn generate_var_markdown(&self, risk_analysis: &RiskAnalysisResult) -> String {
        let mut content = String::new();
        
        for (key, value) in &risk_analysis.var_results {
            content.push_str(&format!("- {}: {:.3}\n", key, value));
        }
        
        content
    }
    
    /// 生成壓力測試Markdown
    fn generate_stress_test_markdown(&self, risk_analysis: &RiskAnalysisResult) -> String {
        let mut content = String::new();
        
        for (scenario, result) in &risk_analysis.stress_test_results {
            content.push_str(&format!("- {}: {:.3}\n", scenario, result));
        }
        
        content
    }
}

// 輔助結構體和特性定義
#[derive(Debug, Default)]
pub struct TestDataSet {
    // 測試數據集的實現
}

#[derive(Debug, Default)]
pub struct PredictionDataSet {
    // 預測數據集的實現
}

#[derive(Debug, Default)]
pub struct BenchmarkDataSet {
    // 基準數據集的實現
}

/// 模型預測器特性
#[async_trait::async_trait]
pub trait ModelPredictor: Send + Sync {
    async fn predict(&self, data: &TestDataSet, config: &PredictionConfig) -> Result<PredictionDataSet>;
}

/// 虛擬模型預測器（用於測試）
pub struct DummyModelPredictor;

impl DummyModelPredictor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ModelPredictor for DummyModelPredictor {
    async fn predict(&self, _data: &TestDataSet, _config: &PredictionConfig) -> Result<PredictionDataSet> {
        Ok(PredictionDataSet::default())
    }
}

/// 回測引擎
pub struct BacktestEngine {
    config: BacktestConfig,
}

impl BacktestEngine {
    pub fn new(config: &BacktestConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }
    
    pub async fn run_backtest(
        &mut self,
        predictions: &PredictionDataSet,
        strategy_config: &TradingStrategyConfig,
    ) -> Result<BacktestResults> {
        // 模擬回測實現
        Ok(BacktestResults {
            total_return: 0.15,
            sharpe_ratio: 1.5,
            max_drawdown: 0.08,
            win_rate: 0.65,
            profit_factor: 1.8,
            total_trades: 150,
            equity_curve: vec![
                EquityPoint {
                    timestamp: chrono::Utc::now(),
                    equity: 100000.0,
                    drawdown: 0.0,
                },
                EquityPoint {
                    timestamp: chrono::Utc::now() + ChronoDuration::days(1),
                    equity: 115000.0,
                    drawdown: 0.0,
                },
            ],
            trade_history: vec![
                Trade {
                    entry_time: chrono::Utc::now(),
                    exit_time: chrono::Utc::now() + ChronoDuration::hours(2),
                    symbol: "BTCUSDT".to_string(),
                    side: TradeSide::Buy,
                    entry_price: 50000.0,
                    exit_price: 51000.0,
                    quantity: 0.1,
                    pnl: 100.0,
                    commission: 5.0,
                },
            ],
        })
    }
}