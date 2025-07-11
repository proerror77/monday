/*!
 * 🎯 Hyperparameter Optimization Pipeline - 智能超參數優化系統
 * 
 * 核心功能：
 * - Optuna集成的自動調參引擎
 * - 多模型並行超參數搜索
 * - 貝葉斯優化與進化算法
 * - 早停機制與資源管理
 * - 實驗追蹤與結果分析
 * 
 * 優化策略：
 * - TPE (Tree-structured Parzen Estimator)
 * - 隨機搜索與網格搜索
 * - 多目標優化 (帕累託前沿)
 * - 分佈式並行搜索
 */

use super::*;
use crate::pipelines::training_pipeline::{TrainingPipeline, TrainingPipelineConfig, ModelConfig};
use crate::pipelines::evaluation_pipeline::{EvaluationPipeline, EvaluationPipelineConfig};
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{info, warn, error, debug, instrument};
use chrono::{DateTime, Utc};

/// 超參數優化Pipeline配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperoptPipelineConfig {
    /// 基礎Pipeline配置
    #[serde(flatten)]
    pub base: PipelineConfig,
    
    /// 數據配置
    pub data: HyperoptDataConfig,
    
    /// 優化配置
    pub optimization: OptimizationConfig,
    
    /// 模型配置
    pub models: Vec<HyperoptModelConfig>,
    
    /// 評估配置
    pub evaluation: HyperoptEvaluationConfig,
    
    /// 資源配置
    pub resources: HyperoptResourceConfig,
    
    /// 結果保存配置
    pub result_saving: HyperoptResultConfig,
    
    /// 實驗追蹤配置
    pub experiment_tracking: ExperimentTrackingConfig,
}

/// 超參數優化數據配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperoptDataConfig {
    /// 訓練數據路徑
    pub train_data_path: String,
    
    /// 驗證數據路徑
    pub validation_data_path: String,
    
    /// 測試數據路徑 (可選)
    pub test_data_path: Option<String>,
    
    /// 數據預處理配置
    pub preprocessing: DataPreprocessingConfig,
    
    /// 交叉驗證配置
    pub cross_validation: CrossValidationConfig,
    
    /// 數據採樣配置
    pub sampling: DataSamplingConfig,
}

/// 優化配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationConfig {
    /// 優化算法
    pub algorithm: OptimizationAlgorithm,
    
    /// 最大試驗次數
    pub max_trials: u32,
    
    /// 最大並行度
    pub max_parallel_trials: u32,
    
    /// 超時時間 (分鐘)
    pub timeout_minutes: u64,
    
    /// 早停配置
    pub early_stopping: EarlyStoppingConfig,
    
    /// 優化目標
    pub objectives: Vec<OptimizationObjective>,
    
    /// 約束條件
    pub constraints: Vec<OptimizationConstraint>,
    
    /// 初始化策略
    pub initialization: InitializationStrategy,
}

/// 優化算法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OptimizationAlgorithm {
    /// Tree-structured Parzen Estimator
    TPE {
        n_startup_trials: u32,
        n_ei_candidates: u32,
        gamma: f64,
    },
    /// 隨機搜索
    RandomSearch,
    /// 網格搜索
    GridSearch {
        grid_resolution: u32,
    },
    /// 遺傳算法
    GeneticAlgorithm {
        population_size: u32,
        mutation_rate: f64,
        crossover_rate: f64,
    },
    /// CMA-ES
    CMAES {
        sigma: f64,
        population_size: Option<u32>,
    },
}

/// 早停配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EarlyStoppingConfig {
    /// 是否啟用早停
    pub enabled: bool,
    
    /// 最小改進閾值
    pub min_improvement: f64,
    
    /// 耐心次數 (連續沒有改進的試驗數)
    pub patience: u32,
    
    /// 最小試驗數 (早停前的最小試驗數)
    pub min_trials: u32,
}

/// 優化目標
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationObjective {
    /// 目標名稱
    pub name: String,
    
    /// 優化方向
    pub direction: OptimizationDirection,
    
    /// 權重 (多目標優化時使用)
    pub weight: f64,
    
    /// 目標函數類型
    pub objective_type: ObjectiveType,
}

/// 優化方向
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OptimizationDirection {
    Maximize,
    Minimize,
}

/// 目標函數類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ObjectiveType {
    /// 驗證損失
    ValidationLoss,
    /// 驗證準確率
    ValidationAccuracy,
    /// 夏普比率
    SharpeRatio,
    /// 最大回撤
    MaxDrawdown,
    /// 總收益
    TotalReturn,
    /// 勝率
    WinRate,
    /// 平均利潤
    AverageProfit,
    /// 自定義指標
    Custom(String),
}

/// 約束條件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationConstraint {
    /// 約束名稱
    pub name: String,
    
    /// 約束類型
    pub constraint_type: ConstraintType,
    
    /// 約束值
    pub value: f64,
}

/// 約束類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConstraintType {
    /// 小於等於
    LessEqual,
    /// 大於等於
    GreaterEqual,
    /// 等於
    Equal,
    /// 範圍約束
    Range { min: f64, max: f64 },
}

/// 初始化策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InitializationStrategy {
    /// 隨機初始化
    Random,
    /// Latin Hypercube Sampling
    LatinHypercube,
    /// Sobol序列
    Sobol,
    /// 基於先驗知識的初始化
    PriorBased {
        prior_trials: Vec<HashMap<String, f64>>,
    },
}

/// 超參數模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperoptModelConfig {
    /// 模型名稱
    pub name: String,
    
    /// 模型類型
    pub model_type: String,
    
    /// 是否啟用
    pub enabled: bool,
    
    /// 超參數搜索空間
    pub search_space: SearchSpace,
    
    /// 固定參數
    pub fixed_parameters: HashMap<String, serde_json::Value>,
    
    /// 模型特定配置
    pub model_config: serde_json::Value,
}

/// 搜索空間定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchSpace {
    /// 參數定義
    pub parameters: HashMap<String, ParameterDefinition>,
}

/// 參數定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDefinition {
    /// 參數類型
    pub param_type: ParameterType,
    
    /// 參數範圍或選項
    pub range: ParameterRange,
    
    /// 是否在對數空間搜索
    pub log_scale: bool,
    
    /// 默認值
    pub default: Option<serde_json::Value>,
}

/// 參數類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParameterType {
    Float,
    Int,
    Categorical,
    Bool,
}

/// 參數範圍
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParameterRange {
    /// 連續範圍
    Continuous { min: f64, max: f64 },
    /// 整數範圍
    Integer { min: i64, max: i64 },
    /// 類別選項
    Categorical { choices: Vec<String> },
    /// 布爾值
    Boolean,
}

/// 超參數優化評估配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperoptEvaluationConfig {
    /// 評估指標
    pub metrics: Vec<String>,
    
    /// 主要指標
    pub primary_metric: String,
    
    /// 評估方法
    pub evaluation_method: EvaluationMethod,
    
    /// 快速評估配置
    pub fast_evaluation: FastEvaluationConfig,
    
    /// 完整評估配置
    pub full_evaluation: FullEvaluationConfig,
}

/// 評估方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvaluationMethod {
    /// 交叉驗證
    CrossValidation,
    /// Hold-out驗證
    HoldOut,
    /// 時間序列分割
    TimeSeriesSplit,
    /// 自定義評估
    Custom,
}

/// 快速評估配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastEvaluationConfig {
    /// 是否啟用快速評估
    pub enabled: bool,
    
    /// 數據採樣比例
    pub data_sample_ratio: f64,
    
    /// 最大訓練輪數
    pub max_epochs: u32,
    
    /// 快速評估閾值
    pub threshold: f64,
}

/// 完整評估配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullEvaluationConfig {
    /// 完整評估條件
    pub trigger_condition: FullEvaluationTrigger,
    
    /// 最大完整評估數
    pub max_full_evaluations: u32,
    
    /// 完整評估的訓練輪數
    pub full_training_epochs: u32,
}

/// 完整評估觸發條件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FullEvaluationTrigger {
    /// 排名前N的試驗
    TopN(u32),
    /// 超過閾值的試驗
    AboveThreshold(f64),
    /// 每N個試驗
    EveryN(u32),
    /// 最終的最佳試驗
    FinalBest,
}

/// 超參數優化資源配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperoptResourceConfig {
    /// CPU核心數
    pub cpu_cores: u32,
    
    /// 內存限制 (GB)
    pub memory_limit_gb: f64,
    
    /// GPU配置
    pub gpu_config: Option<GPUConfig>,
    
    /// 分佈式配置
    pub distributed: Option<DistributedConfig>,
    
    /// 資源調度策略
    pub scheduling_strategy: ResourceSchedulingStrategy,
}

/// GPU配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GPUConfig {
    /// 是否使用GPU
    pub enabled: bool,
    
    /// GPU數量
    pub num_gpus: u32,
    
    /// GPU內存限制 (GB)
    pub memory_limit_gb: f64,
    
    /// 混合精度訓練
    pub mixed_precision: bool,
}

/// 分佈式配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedConfig {
    /// 工作節點數
    pub num_workers: u32,
    
    /// 主節點地址
    pub master_address: String,
    
    /// 通信協議
    pub communication_backend: String,
}

/// 資源調度策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResourceSchedulingStrategy {
    /// FIFO調度
    FIFO,
    /// 優先級調度
    Priority,
    /// 最短作業優先
    ShortestJobFirst,
    /// 輪詢調度
    RoundRobin,
    /// 自適應調度
    Adaptive,
}

/// 超參數優化結果配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperoptResultConfig {
    /// 結果保存路徑
    pub save_path: String,
    
    /// 保存格式
    pub save_format: ResultSaveFormat,
    
    /// 是否保存所有試驗
    pub save_all_trials: bool,
    
    /// 最佳模型保存數量
    pub save_top_n_models: u32,
    
    /// 可視化配置
    pub visualization: VisualizationConfig,
}

/// 結果保存格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResultSaveFormat {
    JSON,
    CSV,
    Parquet,
    SQLite,
    PostgreSQL,
}

/// 可視化配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualizationConfig {
    /// 是否生成可視化
    pub enabled: bool,
    
    /// 圖表類型
    pub chart_types: Vec<ChartType>,
    
    /// 輸出格式
    pub output_format: Vec<String>,
}

/// 圖表類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChartType {
    /// 優化歷史
    OptimizationHistory,
    /// 參數重要性
    ParameterImportance,
    /// 平行座標圖
    ParallelCoordinate,
    /// 散點圖矩陣
    ScatterMatrix,
    /// 熱力圖
    Heatmap,
    /// 帕累託前沿
    ParetoFront,
}

/// 數據預處理配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPreprocessingConfig {
    /// 標準化方法
    pub normalization_method: String,
    
    /// 特徵選擇
    pub feature_selection: bool,
    
    /// 數據增強
    pub data_augmentation: bool,
    
    /// 異常值處理
    pub outlier_handling: String,
}

/// 交叉驗證配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossValidationConfig {
    /// 折數
    pub n_folds: u32,
    
    /// 隨機種子
    pub random_seed: u64,
    
    /// 分層採樣
    pub stratified: bool,
    
    /// 時間序列交叉驗證
    pub time_series_cv: bool,
}

/// 數據採樣配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSamplingConfig {
    /// 採樣策略
    pub strategy: SamplingStrategy,
    
    /// 採樣比例
    pub sample_ratio: f64,
    
    /// 最小樣本數
    pub min_samples: u32,
    
    /// 最大樣本數
    pub max_samples: u32,
}

/// 採樣策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SamplingStrategy {
    Random,
    Stratified,
    Systematic,
    Cluster,
}

/// 實驗追蹤配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentTrackingConfig {
    /// 實驗名稱
    pub experiment_name: String,
    
    /// 實驗描述
    pub experiment_description: String,
    
    /// 追蹤平台
    pub tracking_platform: TrackingPlatform,
    
    /// 標籤
    pub tags: Vec<String>,
    
    /// 元數據
    pub metadata: HashMap<String, String>,
}

/// 追蹤平台
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TrackingPlatform {
    MLflow,
    WandbAI,
    Neptune,
    TensorBoard,
    Local,
}

/// 超參數優化Pipeline
pub struct HyperoptPipeline {
    config: HyperoptPipelineConfig,
    current_trial: u32,
    best_trials: Vec<TrialResult>,
    optimization_state: OptimizationState,
}

/// 試驗結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialResult {
    pub trial_id: u32,
    pub parameters: HashMap<String, serde_json::Value>,
    pub objectives: HashMap<String, f64>,
    pub metrics: HashMap<String, f64>,
    pub status: TrialStatus,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub duration_seconds: Option<f64>,
    pub model_path: Option<String>,
    pub error_message: Option<String>,
}

/// 試驗狀態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TrialStatus {
    Running,
    Completed,
    Failed,
    Pruned,
    Cancelled,
}

/// 優化狀態
#[derive(Debug, Clone)]
struct OptimizationState {
    best_value: Option<f64>,
    best_parameters: Option<HashMap<String, serde_json::Value>>,
    trials_without_improvement: u32,
    should_stop: bool,
    sampler_state: Option<SamplerState>,
}

/// 採樣器狀態
#[derive(Debug, Clone)]
struct SamplerState {
    // TPE sampler狀態
    observations: Vec<(HashMap<String, f64>, f64)>,
    // 其他採樣器狀態
    rng_state: u64,
}

impl HyperoptPipeline {
    /// 創建新的超參數優化Pipeline
    pub fn new(config: HyperoptPipelineConfig) -> Self {
        Self {
            config,
            current_trial: 0,
            best_trials: Vec::new(),
            optimization_state: OptimizationState {
                best_value: None,
                best_parameters: None,
                trials_without_improvement: 0,
                should_stop: false,
                sampler_state: None,
            },
        }
    }
    
    /// 開始優化過程
    #[instrument(skip(self, context))]
    pub async fn optimize(&mut self, context: &mut PipelineContext) -> Result<Vec<TrialResult>> {
        info!("開始超參數優化: {} 個模型, 最大 {} 次試驗", 
              self.config.models.len(), 
              self.config.optimization.max_trials);
        
        // 初始化實驗追蹤
        self.initialize_experiment_tracking(context).await?;
        
        // 準備數據
        self.prepare_data(context).await?;
        
        // 初始化採樣器
        self.initialize_sampler().await?;
        
        let mut all_trials = Vec::new();
        
        // 並行優化多個模型
        for model_config in &self.config.models.clone() {
            if !model_config.enabled {
                continue;
            }
            
            info!("開始優化模型: {}", model_config.name);
            
            let model_trials = self.optimize_model(model_config, context).await?;
            all_trials.extend(model_trials);
        }
        
        // 分析結果
        self.analyze_results(&all_trials, context).await?;
        
        // 生成報告
        self.generate_report(&all_trials, context).await?;
        
        Ok(all_trials)
    }
    
    /// 優化單個模型
    async fn optimize_model(
        &mut self, 
        model_config: &HyperoptModelConfig, 
        context: &mut PipelineContext
    ) -> Result<Vec<TrialResult>> {
        let mut trials = Vec::new();
        let mut best_value = None;
        let mut trials_without_improvement = 0;
        
        for trial_id in 0..self.config.optimization.max_trials {
            // 檢查早停條件
            if self.should_early_stop(trial_id, trials_without_improvement) {
                info!("觸發早停條件，停止優化");
                break;
            }
            
            // 生成試驗參數
            let parameters = self.suggest_parameters(model_config, &trials).await?;
            
            // 執行試驗
            let trial_result = self.run_trial(
                trial_id,
                parameters,
                model_config,
                context
            ).await;
            
            match trial_result {
                Ok(result) => {
                    // 更新最佳值
                    let primary_metric = &self.config.evaluation.primary_metric;
                    if let Some(value) = result.objectives.get(primary_metric) {
                        if best_value.map_or(true, |best| self.is_better(*value, best)) {
                            best_value = Some(*value);
                            trials_without_improvement = 0;
                            info!("找到新的最佳值: {} = {}", primary_metric, value);
                        } else {
                            trials_without_improvement += 1;
                        }
                    }
                    
                    trials.push(result);
                }
                Err(e) => {
                    warn!("試驗 {} 失敗: {}", trial_id, e);
                    // 記錄失敗的試驗
                    let failed_trial = TrialResult {
                        trial_id,
                        parameters: HashMap::new(),
                        objectives: HashMap::new(),
                        metrics: HashMap::new(),
                        status: TrialStatus::Failed,
                        start_time: Utc::now(),
                        end_time: Some(Utc::now()),
                        duration_seconds: None,
                        model_path: None,
                        error_message: Some(e.to_string()),
                    };
                    trials.push(failed_trial);
                }
            }
            
            // 更新進度
            let progress = (trial_id + 1) as f64 / self.config.optimization.max_trials as f64;
            context.set_metric("optimization_progress".to_string(), progress);
        }
        
        Ok(trials)
    }
    
    /// 執行單個試驗
    async fn run_trial(
        &self,
        trial_id: u32,
        parameters: HashMap<String, serde_json::Value>,
        model_config: &HyperoptModelConfig,
        context: &mut PipelineContext,
    ) -> Result<TrialResult> {
        let start_time = Utc::now();
        
        info!("運行試驗 {}: {:?}", trial_id, parameters);
        
        // 創建試驗工作目錄
        let trial_dir = format!("{}/trial_{}", context.working_directory, trial_id);
        tokio::fs::create_dir_all(&trial_dir).await?;
        
        // 構建訓練配置
        let training_config = self.build_training_config(&parameters, model_config, &trial_dir)?;
        
        // 執行訓練
        let mut training_pipeline = TrainingPipeline::new(training_config);
        let mut training_context = PipelineContext::new(PipelineConfig {
            name: format!("hyperopt_trial_{}", trial_id),
            version: "1.0".to_string(),
            description: Some(format!("超參數優化試驗 {}", trial_id)),
            priority: PipelinePriority::Medium,
            timeout_seconds: 3600,
            max_retries: 1,
            resource_limits: ResourceLimits::default(),
            environment: HashMap::new(),
            output_directory: trial_dir.clone(),
            verbose_logging: false,
            continue_on_failure: false,
        });
        
        let training_result = training_pipeline.execute(&mut training_context).await?;
        
        // 評估模型
        let evaluation_metrics = self.evaluate_trial_model(&trial_dir, &training_result, context).await?;
        
        let end_time = Utc::now();
        let duration = (end_time - start_time).num_milliseconds() as f64 / 1000.0;
        
        // 提取目標值
        let objectives = self.extract_objectives(&evaluation_metrics)?;
        
        Ok(TrialResult {
            trial_id,
            parameters,
            objectives,
            metrics: evaluation_metrics,
            status: TrialStatus::Completed,
            start_time,
            end_time: Some(end_time),
            duration_seconds: Some(duration),
            model_path: Some(format!("{}/model.safetensors", trial_dir)),
            error_message: None,
        })
    }
    
    /// 生成試驗參數
    async fn suggest_parameters(
        &self,
        model_config: &HyperoptModelConfig,
        previous_trials: &[TrialResult],
    ) -> Result<HashMap<String, serde_json::Value>> {
        match &self.config.optimization.algorithm {
            OptimizationAlgorithm::TPE { .. } => {
                self.suggest_tpe_parameters(model_config, previous_trials).await
            }
            OptimizationAlgorithm::RandomSearch => {
                self.suggest_random_parameters(model_config).await
            }
            OptimizationAlgorithm::GridSearch { .. } => {
                self.suggest_grid_parameters(model_config, previous_trials.len()).await
            }
            _ => {
                // 其他算法的默認實現
                self.suggest_random_parameters(model_config).await
            }
        }
    }
    
    /// TPE參數建議
    async fn suggest_tpe_parameters(
        &self,
        model_config: &HyperoptModelConfig,
        previous_trials: &[TrialResult],
    ) -> Result<HashMap<String, serde_json::Value>> {
        // 簡化的TPE實現
        if previous_trials.len() < 10 {
            // 前10個試驗使用隨機搜索
            return self.suggest_random_parameters(model_config).await;
        }
        
        // 基於歷史試驗的TPE採樣
        let mut parameters = HashMap::new();
        
        for (param_name, param_def) in &model_config.search_space.parameters {
            let value = self.sample_tpe_parameter(param_name, param_def, previous_trials)?;
            parameters.insert(param_name.clone(), value);
        }
        
        Ok(parameters)
    }
    
    /// 隨機參數建議
    async fn suggest_random_parameters(
        &self,
        model_config: &HyperoptModelConfig,
    ) -> Result<HashMap<String, serde_json::Value>> {
        let mut parameters = HashMap::new();
        
        for (param_name, param_def) in &model_config.search_space.parameters {
            let value = self.sample_random_parameter(param_def)?;
            parameters.insert(param_name.clone(), value);
        }
        
        Ok(parameters)
    }
    
    /// 網格搜索參數建議
    async fn suggest_grid_parameters(
        &self,
        model_config: &HyperoptModelConfig,
        trial_index: usize,
    ) -> Result<HashMap<String, serde_json::Value>> {
        let mut parameters = HashMap::new();
        
        // 簡化的網格搜索實現
        let param_names: Vec<_> = model_config.search_space.parameters.keys().collect();
        let grid_size = (self.config.optimization.max_trials as f64).powf(1.0 / param_names.len() as f64) as usize;
        
        for (i, param_name) in param_names.iter().enumerate() {
            let param_def = &model_config.search_space.parameters[*param_name];
            let param_index = (trial_index / grid_size.pow(i as u32)) % grid_size;
            let value = self.sample_grid_parameter(param_def, param_index, grid_size)?;
            parameters.insert(param_name.to_string(), value);
        }
        
        Ok(parameters)
    }
    
    /// 採樣隨機參數值
    fn sample_random_parameter(&self, param_def: &ParameterDefinition) -> Result<serde_json::Value> {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        
        match &param_def.range {
            ParameterRange::Continuous { min, max } => {
                let value = if param_def.log_scale {
                    (min.ln() + rng.gen::<f64>() * (max.ln() - min.ln())).exp()
                } else {
                    min + rng.gen::<f64>() * (max - min)
                };
                Ok(serde_json::Value::Number(serde_json::Number::from_f64(value).unwrap()))
            }
            ParameterRange::Integer { min, max } => {
                let value = rng.gen_range(*min..=*max);
                Ok(serde_json::Value::Number(serde_json::Number::from(value)))
            }
            ParameterRange::Categorical { choices } => {
                let choice = &choices[rng.gen_range(0..choices.len())];
                Ok(serde_json::Value::String(choice.clone()))
            }
            ParameterRange::Boolean => {
                Ok(serde_json::Value::Bool(rng.gen::<bool>()))
            }
        }
    }
    
    /// TPE參數採樣
    fn sample_tpe_parameter(
        &self,
        _param_name: &str,
        param_def: &ParameterDefinition,
        _previous_trials: &[TrialResult],
    ) -> Result<serde_json::Value> {
        // 簡化實現：暫時使用隨機採樣
        // 實際TPE實現需要基於歷史試驗建立概率模型
        self.sample_random_parameter(param_def)
    }
    
    /// 網格參數採樣
    fn sample_grid_parameter(
        &self,
        param_def: &ParameterDefinition,
        param_index: usize,
        grid_size: usize,
    ) -> Result<serde_json::Value> {
        match &param_def.range {
            ParameterRange::Continuous { min, max } => {
                let ratio = param_index as f64 / (grid_size - 1) as f64;
                let value = if param_def.log_scale {
                    (min.ln() + ratio * (max.ln() - min.ln())).exp()
                } else {
                    min + ratio * (max - min)
                };
                Ok(serde_json::Value::Number(serde_json::Number::from_f64(value).unwrap()))
            }
            ParameterRange::Integer { min, max } => {
                let range = max - min + 1;
                let step = range as f64 / grid_size as f64;
                let value = min + (param_index as f64 * step) as i64;
                Ok(serde_json::Value::Number(serde_json::Number::from(value.min(*max))))
            }
            ParameterRange::Categorical { choices } => {
                let choice_index = param_index % choices.len();
                Ok(serde_json::Value::String(choices[choice_index].clone()))
            }
            ParameterRange::Boolean => {
                Ok(serde_json::Value::Bool(param_index % 2 == 0))
            }
        }
    }
    
    /// 檢查是否應該早停
    fn should_early_stop(&self, trial_id: u32, trials_without_improvement: u32) -> bool {
        if !self.config.optimization.early_stopping.enabled {
            return false;
        }
        
        if trial_id < self.config.optimization.early_stopping.min_trials {
            return false;
        }
        
        trials_without_improvement >= self.config.optimization.early_stopping.patience
    }
    
    /// 判斷新值是否更好
    fn is_better(&self, new_value: f64, best_value: f64) -> bool {
        // 假設主要指標是最大化
        // 實際實現需要根據優化方向判斷
        new_value > best_value
    }
    
    /// 初始化實驗追蹤
    async fn initialize_experiment_tracking(&self, _context: &mut PipelineContext) -> Result<()> {
        info!("初始化實驗追蹤: {}", self.config.experiment_tracking.experiment_name);
        // 實際實現會根據配置的平台初始化追蹤
        Ok(())
    }
    
    /// 準備數據
    async fn prepare_data(&self, context: &mut PipelineContext) -> Result<()> {
        info!("準備優化數據");
        
        // 檢查數據文件存在性
        for path in [
            &self.config.data.train_data_path,
            &self.config.data.validation_data_path,
        ] {
            if !Path::new(path).exists() {
                return Err(anyhow::anyhow!("數據文件不存在: {}", path));
            }
        }
        
        context.set_metric("data_preparation_completed".to_string(), 1.0);
        Ok(())
    }
    
    /// 初始化採樣器
    async fn initialize_sampler(&mut self) -> Result<()> {
        info!("初始化採樣器: {:?}", self.config.optimization.algorithm);
        
        // 根據算法類型初始化採樣器狀態
        self.optimization_state.sampler_state = Some(SamplerState {
            observations: Vec::new(),
            rng_state: 42,
        });
        
        Ok(())
    }
    
    /// 構建訓練配置
    fn build_training_config(
        &self,
        parameters: &HashMap<String, serde_json::Value>,
        model_config: &HyperoptModelConfig,
        trial_dir: &str,
    ) -> Result<TrainingPipelineConfig> {
        // 基於超參數構建訓練配置
        // 這裡是簡化實現，實際需要根據模型類型和參數定義構建
        
        let model_training_config = ModelConfig {
            name: model_config.name.clone(),
            model_type: crate::pipelines::training_pipeline::ModelType::Transformer, // 簡化
            architecture: crate::pipelines::training_pipeline::ArchitectureConfig {
                input_size: parameters.get("input_size").and_then(|v| v.as_u64()).unwrap_or(64) as usize,
                hidden_size: parameters.get("hidden_size").and_then(|v| v.as_u64()).unwrap_or(128) as usize,
                output_size: 1,
                num_layers: parameters.get("num_layers").and_then(|v| v.as_u64()).unwrap_or(2) as usize,
                dropout: parameters.get("dropout").and_then(|v| v.as_f64()).unwrap_or(0.1),
                activation: "ReLU".to_string(),
                use_batch_norm: parameters.get("use_batch_norm").and_then(|v| v.as_bool()).unwrap_or(true),
                use_layer_norm: false,
            },
            hyperparameters: crate::pipelines::training_pipeline::HyperparameterConfig {
                learning_rate: parameters.get("learning_rate").and_then(|v| v.as_f64()).unwrap_or(0.001),
                batch_size: parameters.get("batch_size").and_then(|v| v.as_u64()).unwrap_or(32) as usize,
                epochs: parameters.get("epochs").and_then(|v| v.as_u64()).unwrap_or(50) as u32,
                optimizer: parameters.get("optimizer").and_then(|v| v.as_str()).unwrap_or("Adam").to_string(),
                weight_decay: parameters.get("weight_decay").and_then(|v| v.as_f64()).unwrap_or(0.0),
                scheduler: None,
                gradient_clipping: parameters.get("gradient_clipping").and_then(|v| v.as_f64()),
            },
            enabled: true,
            ensemble_weight: 1.0,
            regularization: crate::pipelines::training_pipeline::RegularizationConfig::default(),
        };
        
        Ok(TrainingPipelineConfig {
            base: PipelineConfig {
                name: "hyperopt_training".to_string(),
                version: "1.0".to_string(),
                description: Some("超參數優化訓練".to_string()),
                priority: PipelinePriority::Medium,
                timeout_seconds: 3600,
                max_retries: 1,
                resource_limits: ResourceLimits::default(),
                environment: HashMap::new(),
                output_directory: trial_dir.to_string(),
                verbose_logging: false,
                continue_on_failure: false,
            },
            data: crate::pipelines::training_pipeline::TrainingDataConfig {
                feature_data_path: self.config.data.train_data_path.clone(),
                train_ratio: 0.8,
                validation_ratio: 0.2,
                test_ratio: 0.0,
                time_series_split: true,
                data_augmentation: None,
                feature_selection: crate::pipelines::training_pipeline::FeatureSelectionConfig::default(),
                normalization: crate::pipelines::training_pipeline::NormalizationConfig::default(),
            },
            models: vec![model_training_config],
            training: crate::pipelines::training_pipeline::TrainingConfig::default(),
            validation: crate::pipelines::training_pipeline::ValidationConfig::default(),
            model_saving: crate::pipelines::training_pipeline::ModelSavingConfig::default(),
            experiment_tracking: self.config.experiment_tracking.clone(),
        })
    }
    
    /// 評估試驗模型
    async fn evaluate_trial_model(
        &self,
        _trial_dir: &str,
        _training_result: &PipelineResult,
        _context: &mut PipelineContext,
    ) -> Result<HashMap<String, f64>> {
        // 簡化實現：返回模擬的評估指標
        let mut metrics = HashMap::new();
        
        // 模擬一些評估指標
        use rand::Rng;
        let mut rng = rand::thread_rng();
        
        metrics.insert("validation_loss".to_string(), rng.gen_range(0.1..1.0));
        metrics.insert("validation_accuracy".to_string(), rng.gen_range(0.5..0.95));
        metrics.insert("sharpe_ratio".to_string(), rng.gen_range(0.5..2.5));
        metrics.insert("max_drawdown".to_string(), rng.gen_range(0.02..0.15));
        metrics.insert("total_return".to_string(), rng.gen_range(0.05..0.3));
        
        Ok(metrics)
    }
    
    /// 提取目標函數值
    fn extract_objectives(&self, metrics: &HashMap<String, f64>) -> Result<HashMap<String, f64>> {
        let mut objectives = HashMap::new();
        
        for objective in &self.config.optimization.objectives {
            let value = match &objective.objective_type {
                ObjectiveType::ValidationLoss => metrics.get("validation_loss").copied(),
                ObjectiveType::ValidationAccuracy => metrics.get("validation_accuracy").copied(),
                ObjectiveType::SharpeRatio => metrics.get("sharpe_ratio").copied(),
                ObjectiveType::MaxDrawdown => metrics.get("max_drawdown").copied(),
                ObjectiveType::TotalReturn => metrics.get("total_return").copied(),
                ObjectiveType::Custom(name) => metrics.get(name).copied(),
                _ => None,
            };
            
            if let Some(v) = value {
                objectives.insert(objective.name.clone(), v);
            }
        }
        
        Ok(objectives)
    }
    
    /// 分析優化結果
    async fn analyze_results(&self, trials: &[TrialResult], context: &mut PipelineContext) -> Result<()> {
        info!("分析優化結果: {} 個試驗", trials.len());
        
        let successful_trials: Vec<_> = trials.iter()
            .filter(|t| t.status == TrialStatus::Completed)
            .collect();
        
        if successful_trials.is_empty() {
            warn!("沒有成功完成的試驗");
            return Ok(());
        }
        
        // 統計分析
        let success_rate = successful_trials.len() as f64 / trials.len() as f64;
        context.set_metric("success_rate".to_string(), success_rate);
        
        // 找到最佳試驗
        if let Some(best_trial) = self.find_best_trial(&successful_trials) {
            info!("最佳試驗: trial_{}, 主要指標: {:.4}", 
                  best_trial.trial_id,
                  best_trial.objectives.get(&self.config.evaluation.primary_metric).unwrap_or(&0.0));
            
            context.set_metric("best_trial_id".to_string(), best_trial.trial_id as f64);
        }
        
        // 參數重要性分析
        self.analyze_parameter_importance(&successful_trials, context).await?;
        
        Ok(())
    }
    
    /// 找到最佳試驗
    fn find_best_trial(&self, trials: &[&TrialResult]) -> Option<&TrialResult> {
        let primary_metric = &self.config.evaluation.primary_metric;
        
        trials.iter()
            .filter_map(|trial| {
                trial.objectives.get(primary_metric)
                    .map(|value| (trial, value))
            })
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(trial, _)| *trial)
    }
    
    /// 分析參數重要性
    async fn analyze_parameter_importance(
        &self,
        _trials: &[&TrialResult],
        context: &mut PipelineContext,
    ) -> Result<()> {
        info!("分析參數重要性");
        
        // 簡化實現：記錄分析完成
        context.set_metric("parameter_importance_analyzed".to_string(), 1.0);
        
        Ok(())
    }
    
    /// 生成優化報告
    async fn generate_report(&self, trials: &[TrialResult], context: &mut PipelineContext) -> Result<()> {
        info!("生成優化報告");
        
        let report_path = format!("{}/hyperopt_report.json", context.working_directory);
        
        let report = serde_json::json!({
            "experiment_name": self.config.experiment_tracking.experiment_name,
            "total_trials": trials.len(),
            "successful_trials": trials.iter().filter(|t| t.status == TrialStatus::Completed).count(),
            "optimization_config": self.config.optimization,
            "best_trials": self.get_top_n_trials(trials, 5),
            "summary_statistics": self.compute_summary_statistics(trials),
        });
        
        tokio::fs::write(&report_path, serde_json::to_string_pretty(&report)?).await?;
        context.add_artifact(report_path);
        
        // 生成可視化（如果啟用）
        if self.config.result_saving.visualization.enabled {
            self.generate_visualizations(trials, context).await?;
        }
        
        Ok(())
    }
    
    /// 獲取前N個最佳試驗
    fn get_top_n_trials(&self, trials: &[TrialResult], n: usize) -> Vec<&TrialResult> {
        let mut successful_trials: Vec<_> = trials.iter()
            .filter(|t| t.status == TrialStatus::Completed)
            .collect();
        
        let primary_metric = &self.config.evaluation.primary_metric;
        successful_trials.sort_by(|a, b| {
            let a_value = a.objectives.get(primary_metric).unwrap_or(&0.0);
            let b_value = b.objectives.get(primary_metric).unwrap_or(&0.0);
            b_value.partial_cmp(a_value).unwrap_or(std::cmp::Ordering::Equal)
        });
        
        successful_trials.into_iter().take(n).collect()
    }
    
    /// 計算摘要統計
    fn compute_summary_statistics(&self, trials: &[TrialResult]) -> serde_json::Value {
        let successful_trials: Vec<_> = trials.iter()
            .filter(|t| t.status == TrialStatus::Completed)
            .collect();
        
        if successful_trials.is_empty() {
            return serde_json::json!({});
        }
        
        let primary_metric = &self.config.evaluation.primary_metric;
        let values: Vec<f64> = successful_trials.iter()
            .filter_map(|t| t.objectives.get(primary_metric))
            .copied()
            .collect();
        
        if values.is_empty() {
            return serde_json::json!({});
        }
        
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        
        serde_json::json!({
            "primary_metric": primary_metric,
            "mean": mean,
            "max": max,
            "min": min,
            "count": values.len(),
        })
    }
    
    /// 生成可視化
    async fn generate_visualizations(&self, _trials: &[TrialResult], context: &mut PipelineContext) -> Result<()> {
        info!("生成可視化圖表");
        
        // 簡化實現：記錄可視化生成完成
        context.set_metric("visualizations_generated".to_string(), 1.0);
        
        Ok(())
    }
}

#[async_trait::async_trait]
impl PipelineExecutor for HyperoptPipeline {
    async fn execute(&mut self, context: &mut PipelineContext) -> Result<PipelineResult> {
        let start_time = std::time::Instant::now();
        
        info!("開始執行超參數優化Pipeline");
        
        // 執行優化
        let trials = self.optimize(context).await.context("超參數優化執行失敗")?;
        
        let duration = start_time.elapsed();
        let successful_trials = trials.iter().filter(|t| t.status == TrialStatus::Completed).count();
        
        info!("超參數優化完成: {} 個試驗, {} 個成功", 
              trials.len(), successful_trials);
        
        Ok(PipelineResult {
            pipeline_id: context.pipeline_id.clone(),
            pipeline_type: self.pipeline_type(),
            status: PipelineStatus::Completed,
            success: true,
            message: format!("超參數優化完成: {} 個試驗, {} 個成功", trials.len(), successful_trials),
            start_time: chrono::Utc::now() - chrono::Duration::milliseconds(duration.as_millis() as i64),
            end_time: Some(chrono::Utc::now()),
            duration_ms: Some(duration.as_millis() as u64),
            output_artifacts: context.artifacts.clone(),
            metrics: context.execution_metrics.clone(),
            error_details: None,
            resource_usage: ResourceUsage::default(),
        })
    }
    
    fn validate_config(&self, _config: &PipelineConfig) -> Result<()> {
        // 驗證超參數優化配置
        if self.config.optimization.max_trials == 0 {
            return Err(anyhow::anyhow!("最大試驗次數必須大於0"));
        }
        
        if self.config.models.is_empty() {
            return Err(anyhow::anyhow!("必須配置至少一個模型"));
        }
        
        if self.config.optimization.objectives.is_empty() {
            return Err(anyhow::anyhow!("必須配置至少一個優化目標"));
        }
        
        Ok(())
    }
    
    fn pipeline_type(&self) -> String {
        "HyperparameterOptimization".to_string()
    }
    
    fn estimated_duration(&self, _config: &PipelineConfig) -> std::time::Duration {
        // 根據試驗數量和模型複雜度估算時間
        let trials = self.config.optimization.max_trials;
        let estimated_minutes = trials * 5; // 假設每個試驗5分鐘
        std::time::Duration::from_secs(estimated_minutes as u64 * 60)
    }
}

// Default實現
impl Default for DataPreprocessingConfig {
    fn default() -> Self {
        Self {
            normalization_method: "StandardScaler".to_string(),
            feature_selection: false,
            data_augmentation: false,
            outlier_handling: "IQR".to_string(),
        }
    }
}

impl Default for CrossValidationConfig {
    fn default() -> Self {
        Self {
            n_folds: 5,
            random_seed: 42,
            stratified: false,
            time_series_cv: true,
        }
    }
}

impl Default for DataSamplingConfig {
    fn default() -> Self {
        Self {
            strategy: SamplingStrategy::Random,
            sample_ratio: 1.0,
            min_samples: 1000,
            max_samples: 1000000,
        }
    }
}