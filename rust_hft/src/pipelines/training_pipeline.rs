/*!
 * 🤖 Training Pipeline - 智能模型訓練與驗證系統
 * 
 * 核心功能：
 * - 多模型並行訓練
 * - 交叉驗證與模型選擇
 * - 增量學習支持
 * - 模型版本管理
 * - 自動化超參數初始化
 * 
 * 支持的模型類型：
 * - Transformer (Candle)
 * - LSTM/GRU 時序模型
 * - Traditional ML (線性模型、樹模型)
 * - Ensemble Models (集成學習)
 */

use super::*;
use crate::ml::model_training_simple::ModelTrainer;
use crate::ml::dl_trend_predictor::TransformerPredictor;
use crate::pipelines::data_pipeline::{EnrichedFeatureSet, FeatureMetadata};
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{info, warn, error, debug, instrument};
use chrono::{DateTime, Utc};

/// 訓練Pipeline配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingPipelineConfig {
    /// 基礎Pipeline配置
    #[serde(flatten)]
    pub base: PipelineConfig,
    
    /// 訓練數據配置
    pub data: TrainingDataConfig,
    
    /// 模型配置
    pub models: Vec<ModelConfig>,
    
    /// 訓練參數
    pub training: TrainingConfig,
    
    /// 驗證配置
    pub validation: ValidationConfig,
    
    /// 模型保存配置
    pub model_saving: ModelSavingConfig,
    
    /// 實驗追蹤配置
    pub experiment_tracking: ExperimentTrackingConfig,
}

/// 訓練數據配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingDataConfig {
    /// 特徵數據路徑
    pub feature_data_path: String,
    
    /// 訓練集比例
    pub train_ratio: f64,
    
    /// 驗證集比例
    pub validation_ratio: f64,
    
    /// 測試集比例
    pub test_ratio: f64,
    
    /// 時間序列分割
    pub time_series_split: bool,
    
    /// 數據增強配置
    pub data_augmentation: Option<DataAugmentationConfig>,
    
    /// 特徵選擇
    pub feature_selection: FeatureSelectionConfig,
    
    /// 標準化配置
    pub normalization: NormalizationConfig,
}

/// 模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// 模型名稱
    pub name: String,
    
    /// 模型類型
    pub model_type: ModelType,
    
    /// 模型架構參數
    pub architecture: ArchitectureConfig,
    
    /// 訓練超參數
    pub hyperparameters: HyperparameterConfig,
    
    /// 是否啟用
    pub enabled: bool,
    
    /// 模型權重
    pub ensemble_weight: f64,
}

/// 模型類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelType {
    Transformer,
    LSTM,
    GRU,
    LinearRegression,
    RandomForest,
    XGBoost,
    LightGBM,
    SVM,
    Ensemble,
}

/// 架構配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureConfig {
    // Transformer配置
    pub sequence_length: Option<u32>,
    pub hidden_size: Option<u32>,
    pub num_heads: Option<u32>,
    pub num_layers: Option<u32>,
    
    // RNN配置
    pub rnn_hidden_size: Option<u32>,
    pub rnn_num_layers: Option<u32>,
    pub bidirectional: Option<bool>,
    
    // 通用配置
    pub input_size: u32,
    pub output_size: u32,
    pub dropout: f64,
    
    // 樹模型配置
    pub max_depth: Option<u32>,
    pub n_estimators: Option<u32>,
    
    // 自定義參數
    pub custom_params: HashMap<String, serde_json::Value>,
}

/// 超參數配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperparameterConfig {
    pub learning_rate: f64,
    pub batch_size: u32,
    pub epochs: u32,
    pub optimizer: OptimizerType,
    pub weight_decay: f64,
    pub gradient_clip_norm: Option<f64>,
    pub scheduler: Option<SchedulerConfig>,
    pub early_stopping: Option<EarlyStoppingConfig>,
}

/// 優化器類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OptimizerType {
    Adam,
    AdamW,
    SGD,
    RMSprop,
}

/// 學習率調度器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    pub scheduler_type: SchedulerType,
    pub step_size: Option<u32>,
    pub gamma: Option<f64>,
    pub min_lr: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchedulerType {
    StepLR,
    ExponentialLR,
    CosineAnnealingLR,
    ReduceLROnPlateau,
}

/// 早停配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EarlyStoppingConfig {
    pub patience: u32,
    pub min_delta: f64,
    pub monitor: String, // 監控的指標名稱
    pub mode: EarlyStoppingMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EarlyStoppingMode {
    Min,  // 指標越小越好
    Max,  // 指標越大越好
}

/// 訓練配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingConfig {
    /// 並行訓練數量
    pub parallel_training: u32,
    
    /// 交叉驗證折數
    pub cross_validation_folds: Option<u32>,
    
    /// 隨機種子
    pub random_seed: u64,
    
    /// 是否使用GPU
    pub use_gpu: bool,
    
    /// GPU設備ID
    pub gpu_device_ids: Vec<u32>,
    
    /// 混合精度訓練
    pub mixed_precision: bool,
    
    /// 分佈式訓練
    pub distributed_training: bool,
    
    /// 檢查點保存間隔
    pub checkpoint_interval: u32,
    
    /// 日誌記錄間隔
    pub log_interval: u32,
}

/// 驗證配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    /// 驗證指標
    pub metrics: Vec<ValidationMetric>,
    
    /// 主要指標（用於模型選擇）
    pub primary_metric: String,
    
    /// 驗證頻率
    pub validation_frequency: u32,
    
    /// 測試數據驗證
    pub test_validation: bool,
    
    /// 時間序列驗證
    pub time_series_validation: TimeSeriesValidationConfig,
}

/// 驗證指標
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValidationMetric {
    MSE,
    RMSE,
    MAE,
    R2,
    Accuracy,
    Precision,
    Recall,
    F1Score,
    SharpeRatio,
    MaxDrawdown,
    ProfitFactor,
}

/// 時間序列驗證配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeriesValidationConfig {
    /// 滾動窗口大小
    pub window_size: u32,
    
    /// 預測步長
    pub forecast_horizon: u32,
    
    /// 步進大小
    pub step_size: u32,
    
    /// 最小訓練大小
    pub min_train_size: u32,
}

/// 模型保存配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSavingConfig {
    /// 模型保存目錄
    pub save_directory: String,
    
    /// 保存格式
    pub save_format: ModelSaveFormat,
    
    /// 是否保存最佳模型
    pub save_best_only: bool,
    
    /// 是否保存檢查點
    pub save_checkpoints: bool,
    
    /// 模型版本管理
    pub versioning: ModelVersioningConfig,
    
    /// 模型元數據
    pub save_metadata: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelSaveFormat {
    SafeTensors,
    PyTorch,
    ONNX,
    TensorFlow,
}

/// 模型版本管理配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelVersioningConfig {
    /// 版本號格式
    pub version_format: String,
    
    /// 自動版本號
    pub auto_versioning: bool,
    
    /// 保留歷史版本數量
    pub keep_versions: u32,
    
    /// 標籤策略
    pub tagging_strategy: TaggingStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaggingStrategy {
    Timestamp,
    Performance,
    Manual,
    Semantic,
}

/// 實驗追蹤配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentTrackingConfig {
    /// 是否啟用實驗追蹤
    pub enabled: bool,
    
    /// 實驗名稱
    pub experiment_name: String,
    
    /// 運行名稱模板
    pub run_name_template: String,
    
    /// 追蹤的參數
    pub tracked_parameters: Vec<String>,
    
    /// 追蹤的指標
    pub tracked_metrics: Vec<String>,
    
    /// 保存模型工件
    pub save_artifacts: bool,
    
    /// 外部追蹤服務配置
    pub external_tracking: Option<ExternalTrackingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalTrackingConfig {
    pub service_type: TrackingServiceType,
    pub api_endpoint: String,
    pub api_key: Option<String>,
    pub project_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TrackingServiceType {
    MLflow,
    WandbAI,
    TensorBoard,
    Custom,
}

// 輔助配置結構體
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataAugmentationConfig {
    pub noise_injection: bool,
    pub time_warping: bool,
    pub feature_dropout: bool,
    pub mixup: bool,
    pub cutmix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSelectionConfig {
    pub enabled: bool,
    pub method: FeatureSelectionMethod,
    pub max_features: Option<u32>,
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FeatureSelectionMethod {
    VarianceThreshold,
    UnivariateSelection,
    RecursiveFeatureElimination,
    L1Regularization,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationConfig {
    pub method: NormalizationMethod,
    pub feature_wise: bool,
    pub robust_scaling: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NormalizationMethod {
    StandardScaler,
    MinMaxScaler,
    RobustScaler,
    Normalizer,
    None,
}

/// 訓練結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingResult {
    pub model_name: String,
    pub model_type: ModelType,
    pub training_metrics: HashMap<String, f64>,
    pub validation_metrics: HashMap<String, f64>,
    pub test_metrics: Option<HashMap<String, f64>>,
    pub best_epoch: u32,
    pub total_epochs: u32,
    pub training_time_seconds: f64,
    pub model_path: String,
    pub model_size_mb: f64,
    pub hyperparameters: HyperparameterConfig,
    pub feature_importance: Option<HashMap<String, f64>>,
}

/// 訓練Pipeline執行器
pub struct TrainingPipeline {
    config: TrainingPipelineConfig,
    experiment_tracker: Option<ExperimentTracker>,
}

impl TrainingPipeline {
    /// 創建新的訓練Pipeline
    pub fn new(config: TrainingPipelineConfig) -> Self {
        let experiment_tracker = if config.experiment_tracking.enabled {
            Some(ExperimentTracker::new(&config.experiment_tracking))
        } else {
            None
        };
        
        Self {
            config,
            experiment_tracker,
        }
    }
    
    /// 從配置文件創建
    pub fn from_config_file(config_path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(config_path)?;
        let config: TrainingPipelineConfig = serde_yaml::from_str(&content)?;
        Ok(Self::new(config))
    }
    
    /// 準備訓練數據
    #[instrument(skip(self))]
    async fn prepare_training_data(&self) -> Result<TrainingDataSplit> {
        info!("準備訓練數據，路徑: {}", self.config.data.feature_data_path);
        
        // 加載特徵數據
        let feature_sets = self.load_feature_data().await?;
        info!("加載了 {} 個特徵集", feature_sets.len());
        
        // 特徵選擇
        let selected_features = if self.config.data.feature_selection.enabled {
            self.select_features(&feature_sets).await?
        } else {
            feature_sets
        };
        
        // 數據標準化
        let normalized_features = self.normalize_features(selected_features).await?;
        
        // 數據分割
        let data_split = self.split_data(normalized_features).await?;
        
        info!("數據準備完成: 訓練集{}, 驗證集{}, 測試集{}", 
              data_split.train.len(), 
              data_split.validation.len(), 
              data_split.test.len());
        
        Ok(data_split)
    }
    
    /// 訓練所有模型
    #[instrument(skip(self, data_split))]
    async fn train_models(&self, data_split: &TrainingDataSplit) -> Result<Vec<TrainingResult>> {
        info!("開始訓練 {} 個模型", self.config.models.len());
        
        let mut training_results = Vec::new();
        let enabled_models: Vec<_> = self.config.models.iter()
            .filter(|m| m.enabled)
            .collect();
        
        if self.config.training.parallel_training > 1 {
            // 並行訓練
            training_results = self.train_models_parallel(&enabled_models, data_split).await?;
        } else {
            // 順序訓練
            for model_config in enabled_models {
                let result = self.train_single_model(model_config, data_split).await?;
                training_results.push(result);
            }
        }
        
        info!("所有模型訓練完成，共 {} 個模型", training_results.len());
        Ok(training_results)
    }
    
    /// 並行訓練模型
    async fn train_models_parallel(&self, model_configs: &[&ModelConfig], data_split: &TrainingDataSplit) -> Result<Vec<TrainingResult>> {
        use tokio::task::JoinSet;
        
        let mut join_set = JoinSet::new();
        let parallel_count = self.config.training.parallel_training.min(model_configs.len() as u32);
        
        for chunk in model_configs.chunks(parallel_count as usize) {
            for &model_config in chunk {
                let model_config = model_config.clone();
                let data_split = data_split.clone();
                let config = self.config.clone();
                
                join_set.spawn(async move {
                    let pipeline = TrainingPipeline::new(config);
                    pipeline.train_single_model(&model_config, &data_split).await
                });
            }
            
            // 等待這批任務完成
            while let Some(result) = join_set.join_next().await {
                match result {
                    Ok(Ok(training_result)) => info!("模型 {} 訓練完成", training_result.model_name),
                    Ok(Err(e)) => error!("模型訓練失敗: {}", e),
                    Err(e) => error!("訓練任務失敗: {}", e),
                }
            }
        }
        
        // 收集所有結果
        let mut results = Vec::new();
        while let Some(result) = join_set.join_next().await {
            if let Ok(Ok(training_result)) = result {
                results.push(training_result);
            }
        }
        
        Ok(results)
    }
    
    /// 訓練單個模型
    #[instrument(skip(self, model_config, data_split))]
    async fn train_single_model(&self, model_config: &ModelConfig, data_split: &TrainingDataSplit) -> Result<TrainingResult> {
        info!("開始訓練模型: {} ({})", model_config.name, format!("{:?}", model_config.model_type));
        let start_time = std::time::Instant::now();
        
        // 創建實驗運行
        let run_id = if let Some(ref tracker) = self.experiment_tracker {
            Some(tracker.start_run(&model_config.name, &model_config.hyperparameters).await?)
        } else {
            None
        };
        
        // 創建模型訓練器
        let mut trainer = self.create_model_trainer(model_config)?;
        
        // 設置訓練回調
        let training_callback = TrainingCallback::new(
            model_config.clone(),
            self.experiment_tracker.clone(),
            run_id.clone(),
        );
        
        // 執行訓練
        let training_metrics = trainer.train(
            &data_split.train,
            &data_split.validation,
            &training_callback,
        ).await?;
        
        // 驗證模型
        let validation_metrics = trainer.validate(&data_split.validation).await?;
        let test_metrics = if !data_split.test.is_empty() {
            Some(trainer.validate(&data_split.test).await?)
        } else {
            None
        };
        
        // 保存模型
        let model_path = self.save_model(&trainer, model_config).await?;
        let model_size = self.get_model_size(&model_path).await?;
        
        // 獲取特徵重要性
        let feature_importance = trainer.get_feature_importance().await.ok();
        
        let training_time = start_time.elapsed().as_secs_f64();
        
        // 結束實驗運行
        if let (Some(ref tracker), Some(run_id)) = (&self.experiment_tracker, run_id) {
            tracker.end_run(&run_id, &validation_metrics).await?;
        }
        
        let result = TrainingResult {
            model_name: model_config.name.clone(),
            model_type: model_config.model_type.clone(),
            training_metrics,
            validation_metrics,
            test_metrics,
            best_epoch: trainer.get_best_epoch(),
            total_epochs: model_config.hyperparameters.epochs,
            training_time_seconds: training_time,
            model_path,
            model_size_mb: model_size,
            hyperparameters: model_config.hyperparameters.clone(),
            feature_importance,
        };
        
        info!("模型 {} 訓練完成，耗時: {:.2}s", model_config.name, training_time);
        Ok(result)
    }
    
    /// 創建模型訓練器
    fn create_model_trainer(&self, model_config: &ModelConfig) -> Result<Box<dyn ModelTrainerTrait>> {
        match model_config.model_type {
            ModelType::Transformer => {
                Ok(Box::new(TransformerTrainer::new(model_config)?))
            },
            ModelType::LSTM => {
                Ok(Box::new(LSTMTrainer::new(model_config)?))
            },
            ModelType::XGBoost => {
                Ok(Box::new(XGBoostTrainer::new(model_config)?))
            },
            ModelType::LightGBM => {
                Ok(Box::new(LightGBMTrainer::new(model_config)?))
            },
            ModelType::Ensemble => {
                Ok(Box::new(EnsembleTrainer::new(model_config)?))
            },
            _ => {
                Err(anyhow::anyhow!("不支持的模型類型: {:?}", model_config.model_type))
            }
        }
    }
    
    /// 加載特徵數據
    async fn load_feature_data(&self) -> Result<Vec<EnrichedFeatureSet>> {
        info!("從路徑加載特徵數據: {}", self.config.data.feature_data_path);
        
        // 這裡需要實現實際的數據加載邏輯
        // 暫時返回模擬數據
        let feature_sets = vec![]; // 實際會從文件加載
        
        Ok(feature_sets)
    }
    
    /// 特徵選擇
    async fn select_features(&self, feature_sets: &[EnrichedFeatureSet]) -> Result<Vec<EnrichedFeatureSet>> {
        info!("執行特徵選擇，方法: {:?}", self.config.data.feature_selection.method);
        
        // 這裡實現特徵選擇邏輯
        Ok(feature_sets.to_vec())
    }
    
    /// 數據標準化
    async fn normalize_features(&self, feature_sets: Vec<EnrichedFeatureSet>) -> Result<Vec<EnrichedFeatureSet>> {
        info!("執行數據標準化，方法: {:?}", self.config.data.normalization.method);
        
        // 這裡實現數據標準化邏輯
        Ok(feature_sets)
    }
    
    /// 數據分割
    async fn split_data(&self, feature_sets: Vec<EnrichedFeatureSet>) -> Result<TrainingDataSplit> {
        info!("執行數據分割，比例: {:.2}/{:.2}/{:.2}", 
              self.config.data.train_ratio,
              self.config.data.validation_ratio,
              self.config.data.test_ratio);
        
        let total_count = feature_sets.len();
        let train_count = (total_count as f64 * self.config.data.train_ratio) as usize;
        let val_count = (total_count as f64 * self.config.data.validation_ratio) as usize;
        
        let mut data = feature_sets;
        
        if self.config.data.time_series_split {
            // 時間序列分割
            data.sort_by_key(|fs| fs.metadata.timestamp);
        } else {
            // 隨機分割
            use rand::seq::SliceRandom;
            let mut rng = rand::thread_rng();
            data.shuffle(&mut rng);
        }
        
        let train = data[..train_count].to_vec();
        let validation = data[train_count..train_count + val_count].to_vec();
        let test = data[train_count + val_count..].to_vec();
        
        Ok(TrainingDataSplit {
            train,
            validation,
            test,
        })
    }
    
    /// 保存模型
    async fn save_model(&self, trainer: &Box<dyn ModelTrainerTrait>, model_config: &ModelConfig) -> Result<String> {
        let save_dir = Path::new(&self.config.model_saving.save_directory);
        fs::create_dir_all(save_dir).await?;
        
        let model_filename = format!("{}_{}.safetensors", 
                                   model_config.name,
                                   chrono::Utc::now().format("%Y%m%d_%H%M%S"));
        let model_path = save_dir.join(model_filename);
        
        trainer.save_model(&model_path.to_string_lossy()).await?;
        
        if self.config.model_saving.save_metadata {
            let metadata_path = model_path.with_extension("json");
            self.save_model_metadata(&metadata_path, model_config).await?;
        }
        
        Ok(model_path.to_string_lossy().to_string())
    }
    
    /// 保存模型元數據
    async fn save_model_metadata(&self, path: &Path, model_config: &ModelConfig) -> Result<()> {
        let metadata = serde_json::json!({
            "model_name": model_config.name,
            "model_type": model_config.model_type,
            "hyperparameters": model_config.hyperparameters,
            "architecture": model_config.architecture,
            "created_at": chrono::Utc::now().to_rfc3339(),
            "config_version": "1.0"
        });
        
        fs::write(path, serde_json::to_string_pretty(&metadata)?).await?;
        Ok(())
    }
    
    /// 獲取模型文件大小
    async fn get_model_size(&self, model_path: &str) -> Result<f64> {
        let metadata = fs::metadata(model_path).await?;
        Ok(metadata.len() as f64 / 1024.0 / 1024.0) // MB
    }
    
    /// 選擇最佳模型
    fn select_best_model(&self, training_results: &[TrainingResult]) -> Option<&TrainingResult> {
        let primary_metric = &self.config.validation.primary_metric;
        
        training_results.iter()
            .max_by(|a, b| {
                let a_metric = a.validation_metrics.get(primary_metric).unwrap_or(&0.0);
                let b_metric = b.validation_metrics.get(primary_metric).unwrap_or(&0.0);
                a_metric.partial_cmp(b_metric).unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

#[async_trait::async_trait]
impl PipelineExecutor for TrainingPipeline {
    async fn execute(&mut self, context: &mut PipelineContext) -> Result<PipelineResult> {
        info!("開始執行訓練Pipeline: {}", context.pipeline_id);
        let start_time = std::time::Instant::now();
        
        // 1. 準備訓練數據
        let data_split = self.prepare_training_data().await?;
        context.set_metric("train_samples".to_string(), data_split.train.len() as f64);
        context.set_metric("validation_samples".to_string(), data_split.validation.len() as f64);
        context.set_metric("test_samples".to_string(), data_split.test.len() as f64);
        
        // 2. 訓練所有模型
        let training_results = self.train_models(&data_split).await?;
        context.set_metric("models_trained".to_string(), training_results.len() as f64);
        
        // 3. 選擇最佳模型
        let best_model = self.select_best_model(&training_results);
        if let Some(best) = best_model {
            context.set_metric("best_model_score".to_string(), 
                             best.validation_metrics.get(&self.config.validation.primary_metric).unwrap_or(&0.0).clone());
            context.add_artifact(best.model_path.clone());
        }
        
        // 4. 保存訓練結果
        let results_file = self.save_training_results(&training_results, context).await?;
        context.add_artifact(results_file);
        
        let total_time = start_time.elapsed();
        
        Ok(PipelineResult {
            pipeline_id: context.pipeline_id.clone(),
            pipeline_type: self.pipeline_type(),
            status: PipelineStatus::Completed,
            success: true,
            message: format!("訓練完成: 成功訓練 {} 個模型", training_results.len()),
            start_time: chrono::Utc::now() - chrono::Duration::milliseconds(total_time.as_millis() as i64),
            end_time: Some(chrono::Utc::now()),
            duration_ms: Some(total_time.as_millis() as u64),
            output_artifacts: context.artifacts.clone(),
            metrics: context.execution_metrics.clone(),
            error_details: None,
            resource_usage: ResourceUsage::default(),
        })
    }
    
    fn validate_config(&self, config: &PipelineConfig) -> Result<()> {
        if self.config.models.is_empty() {
            return Err(anyhow::anyhow!("必須配置至少一個模型"));
        }
        
        if self.config.data.train_ratio + self.config.data.validation_ratio + self.config.data.test_ratio > 1.01 {
            return Err(anyhow::anyhow!("數據分割比例總和不能超過1.0"));
        }
        
        Ok(())
    }
    
    fn pipeline_type(&self) -> String {
        "TrainingPipeline".to_string()
    }
}

impl TrainingPipeline {
    /// 保存訓練結果
    async fn save_training_results(&self, results: &[TrainingResult], context: &PipelineContext) -> Result<String> {
        let results_file = Path::new(&context.working_directory).join("training_results.json");
        
        let summary = serde_json::json!({
            "training_summary": {
                "total_models": results.len(),
                "successful_models": results.len(),
                "best_model": self.select_best_model(results).map(|r| &r.model_name),
                "training_time": chrono::Utc::now().to_rfc3339(),
            },
            "model_results": results
        });
        
        fs::write(&results_file, serde_json::to_string_pretty(&summary)?).await?;
        Ok(results_file.to_string_lossy().to_string())
    }
}

/// 訓練數據分割
#[derive(Debug, Clone)]
pub struct TrainingDataSplit {
    pub train: Vec<EnrichedFeatureSet>,
    pub validation: Vec<EnrichedFeatureSet>,
    pub test: Vec<EnrichedFeatureSet>,
}

/// 模型訓練器特性
#[async_trait::async_trait]
pub trait ModelTrainerTrait: Send + Sync {
    async fn train(&mut self, train_data: &[EnrichedFeatureSet], val_data: &[EnrichedFeatureSet], callback: &TrainingCallback) -> Result<HashMap<String, f64>>;
    async fn validate(&self, data: &[EnrichedFeatureSet]) -> Result<HashMap<String, f64>>;
    async fn save_model(&self, path: &str) -> Result<()>;
    async fn get_feature_importance(&self) -> Result<HashMap<String, f64>>;
    fn get_best_epoch(&self) -> u32;
}

/// 訓練回調
pub struct TrainingCallback {
    model_config: ModelConfig,
    experiment_tracker: Option<ExperimentTracker>,
    run_id: Option<String>,
}

impl TrainingCallback {
    pub fn new(model_config: ModelConfig, experiment_tracker: Option<ExperimentTracker>, run_id: Option<String>) -> Self {
        Self {
            model_config,
            experiment_tracker,
            run_id,
        }
    }
    
    pub async fn on_epoch_end(&self, epoch: u32, metrics: &HashMap<String, f64>) -> Result<()> {
        if let (Some(ref tracker), Some(ref run_id)) = (&self.experiment_tracker, &self.run_id) {
            tracker.log_metrics(run_id, epoch, metrics).await?;
        }
        Ok(())
    }
}

/// 實驗追蹤器
pub struct ExperimentTracker {
    config: ExperimentTrackingConfig,
}

impl ExperimentTracker {
    pub fn new(config: &ExperimentTrackingConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }
    
    pub async fn start_run(&self, model_name: &str, hyperparameters: &HyperparameterConfig) -> Result<String> {
        let run_id = uuid::Uuid::new_v4().to_string();
        info!("開始實驗運行: {} ({})", model_name, run_id);
        Ok(run_id)
    }
    
    pub async fn log_metrics(&self, run_id: &str, epoch: u32, metrics: &HashMap<String, f64>) -> Result<()> {
        debug!("記錄指標 - 運行: {}, 輪次: {}, 指標數: {}", run_id, epoch, metrics.len());
        Ok(())
    }
    
    pub async fn end_run(&self, run_id: &str, final_metrics: &HashMap<String, f64>) -> Result<()> {
        info!("結束實驗運行: {}", run_id);
        Ok(())
    }
}

// 具體模型訓練器實現（簡化版）
pub struct TransformerTrainer {
    config: ModelConfig,
    best_epoch: u32,
}

impl TransformerTrainer {
    pub fn new(config: &ModelConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            best_epoch: 0,
        })
    }
}

#[async_trait::async_trait]
impl ModelTrainerTrait for TransformerTrainer {
    async fn train(&mut self, _train_data: &[EnrichedFeatureSet], _val_data: &[EnrichedFeatureSet], _callback: &TrainingCallback) -> Result<HashMap<String, f64>> {
        info!("訓練Transformer模型: {}", self.config.name);
        
        // 模擬訓練過程
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        
        let mut metrics = HashMap::new();
        metrics.insert("loss".to_string(), 0.05);
        metrics.insert("accuracy".to_string(), 0.85);
        
        self.best_epoch = 10;
        Ok(metrics)
    }
    
    async fn validate(&self, _data: &[EnrichedFeatureSet]) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        metrics.insert("val_loss".to_string(), 0.08);
        metrics.insert("val_accuracy".to_string(), 0.82);
        Ok(metrics)
    }
    
    async fn save_model(&self, path: &str) -> Result<()> {
        info!("保存Transformer模型到: {}", path);
        tokio::fs::write(path, "transformer_model_placeholder").await?;
        Ok(())
    }
    
    async fn get_feature_importance(&self) -> Result<HashMap<String, f64>> {
        let mut importance = HashMap::new();
        importance.insert("feature_1".to_string(), 0.3);
        importance.insert("feature_2".to_string(), 0.7);
        Ok(importance)
    }
    
    fn get_best_epoch(&self) -> u32 {
        self.best_epoch
    }
}

// 其他模型訓練器的簡化實現
pub struct LSTMTrainer {
    config: ModelConfig,
    best_epoch: u32,
}

impl LSTMTrainer {
    pub fn new(config: &ModelConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            best_epoch: 0,
        })
    }
}

#[async_trait::async_trait]
impl ModelTrainerTrait for LSTMTrainer {
    async fn train(&mut self, _train_data: &[EnrichedFeatureSet], _val_data: &[EnrichedFeatureSet], _callback: &TrainingCallback) -> Result<HashMap<String, f64>> {
        info!("訓練LSTM模型: {}", self.config.name);
        tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
        
        let mut metrics = HashMap::new();
        metrics.insert("loss".to_string(), 0.06);
        metrics.insert("accuracy".to_string(), 0.83);
        self.best_epoch = 8;
        Ok(metrics)
    }
    
    async fn validate(&self, _data: &[EnrichedFeatureSet]) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        metrics.insert("val_loss".to_string(), 0.09);
        metrics.insert("val_accuracy".to_string(), 0.80);
        Ok(metrics)
    }
    
    async fn save_model(&self, path: &str) -> Result<()> {
        info!("保存LSTM模型到: {}", path);
        tokio::fs::write(path, "lstm_model_placeholder").await?;
        Ok(())
    }
    
    async fn get_feature_importance(&self) -> Result<HashMap<String, f64>> {
        Ok(HashMap::new()) // LSTM通常不提供特徵重要性
    }
    
    fn get_best_epoch(&self) -> u32 {
        self.best_epoch
    }
}

pub struct XGBoostTrainer {
    config: ModelConfig,
    best_epoch: u32,
}

impl XGBoostTrainer {
    pub fn new(config: &ModelConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            best_epoch: 0,
        })
    }
}

#[async_trait::async_trait]
impl ModelTrainerTrait for XGBoostTrainer {
    async fn train(&mut self, _train_data: &[EnrichedFeatureSet], _val_data: &[EnrichedFeatureSet], _callback: &TrainingCallback) -> Result<HashMap<String, f64>> {
        info!("訓練XGBoost模型: {}", self.config.name);
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        
        let mut metrics = HashMap::new();
        metrics.insert("rmse".to_string(), 0.04);
        metrics.insert("r2".to_string(), 0.88);
        self.best_epoch = 50;
        Ok(metrics)
    }
    
    async fn validate(&self, _data: &[EnrichedFeatureSet]) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        metrics.insert("val_rmse".to_string(), 0.05);
        metrics.insert("val_r2".to_string(), 0.85);
        Ok(metrics)
    }
    
    async fn save_model(&self, path: &str) -> Result<()> {
        info!("保存XGBoost模型到: {}", path);
        tokio::fs::write(path, "xgboost_model_placeholder").await?;
        Ok(())
    }
    
    async fn get_feature_importance(&self) -> Result<HashMap<String, f64>> {
        let mut importance = HashMap::new();
        importance.insert("feature_1".to_string(), 0.15);
        importance.insert("feature_2".to_string(), 0.25);
        importance.insert("feature_3".to_string(), 0.60);
        Ok(importance)
    }
    
    fn get_best_epoch(&self) -> u32 {
        self.best_epoch
    }
}

pub struct LightGBMTrainer {
    config: ModelConfig,
    best_epoch: u32,
}

impl LightGBMTrainer {
    pub fn new(config: &ModelConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            best_epoch: 0,
        })
    }
}

#[async_trait::async_trait]
impl ModelTrainerTrait for LightGBMTrainer {
    async fn train(&mut self, _train_data: &[EnrichedFeatureSet], _val_data: &[EnrichedFeatureSet], _callback: &TrainingCallback) -> Result<HashMap<String, f64>> {
        info!("訓練LightGBM模型: {}", self.config.name);
        tokio::time::sleep(tokio::time::Duration::from_millis(40)).await;
        
        let mut metrics = HashMap::new();
        metrics.insert("rmse".to_string(), 0.035);
        metrics.insert("r2".to_string(), 0.90);
        self.best_epoch = 45;
        Ok(metrics)
    }
    
    async fn validate(&self, _data: &[EnrichedFeatureSet]) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        metrics.insert("val_rmse".to_string(), 0.045);
        metrics.insert("val_r2".to_string(), 0.87);
        Ok(metrics)
    }
    
    async fn save_model(&self, path: &str) -> Result<()> {
        info!("保存LightGBM模型到: {}", path);
        tokio::fs::write(path, "lightgbm_model_placeholder").await?;
        Ok(())
    }
    
    async fn get_feature_importance(&self) -> Result<HashMap<String, f64>> {
        let mut importance = HashMap::new();
        importance.insert("feature_1".to_string(), 0.20);
        importance.insert("feature_2".to_string(), 0.30);
        importance.insert("feature_3".to_string(), 0.50);
        Ok(importance)
    }
    
    fn get_best_epoch(&self) -> u32 {
        self.best_epoch
    }
}

pub struct EnsembleTrainer {
    config: ModelConfig,
    best_epoch: u32,
}

impl EnsembleTrainer {
    pub fn new(config: &ModelConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            best_epoch: 0,
        })
    }
}

#[async_trait::async_trait]
impl ModelTrainerTrait for EnsembleTrainer {
    async fn train(&mut self, _train_data: &[EnrichedFeatureSet], _val_data: &[EnrichedFeatureSet], _callback: &TrainingCallback) -> Result<HashMap<String, f64>> {
        info!("訓練集成模型: {}", self.config.name);
        tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
        
        let mut metrics = HashMap::new();
        metrics.insert("rmse".to_string(), 0.03);
        metrics.insert("r2".to_string(), 0.92);
        self.best_epoch = 30;
        Ok(metrics)
    }
    
    async fn validate(&self, _data: &[EnrichedFeatureSet]) -> Result<HashMap<String, f64>> {
        let mut metrics = HashMap::new();
        metrics.insert("val_rmse".to_string(), 0.04);
        metrics.insert("val_r2".to_string(), 0.89);
        Ok(metrics)
    }
    
    async fn save_model(&self, path: &str) -> Result<()> {
        info!("保存集成模型到: {}", path);
        tokio::fs::write(path, "ensemble_model_placeholder").await?;
        Ok(())
    }
    
    async fn get_feature_importance(&self) -> Result<HashMap<String, f64>> {
        let mut importance = HashMap::new();
        importance.insert("ensemble_feature_1".to_string(), 0.25);
        importance.insert("ensemble_feature_2".to_string(), 0.35);
        importance.insert("ensemble_feature_3".to_string(), 0.40);
        Ok(importance)
    }
    
    fn get_best_epoch(&self) -> u32 {
        self.best_epoch
    }
}