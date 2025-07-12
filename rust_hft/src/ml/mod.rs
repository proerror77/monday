/*!
 * ML Module - 機器學習和特徵工程
 * 
 * 統一架構：Python訓練，Rust執行
 * 
 * 🧠 核心模組：
 * - unified_model_engine: 統一模型加載和推理引擎 (tch-rs + candle)
 * - parallel_feature_engineering: DL/RL特徵工程
 * - 傳統ML模組: 兼容性支持
 */

pub mod features;
pub mod model_training_simple;
pub mod online_learning;
pub mod lob_time_series_extractor;
pub mod dl_trend_predictor;
pub mod parallel_feature_engineering;
pub mod dl_rl_extractors;
pub mod unified_model_engine;  // 🧠 統一模型引擎 (主要接口)
pub mod torchscript_inference; // ⚡ TorchScript超低延迟推理引擎

pub use features::FeatureExtractor;
// Re-export FeatureSet from core types
pub use crate::core::types::FeatureSet;
pub use model_training_simple::{SimplifiedPredictor, generate_synthetic_data};
pub use online_learning::{OnlineLearningEngine, OnlineLearningConfig};
pub use lob_time_series_extractor::{
    LobTimeSeriesExtractor, 
    LobTimeSeriesConfig, 
    LobTimeSeriesSnapshot, 
    LobTimeSeriesSequence,
    LobTimeSeriesExtractorStats
};
pub use dl_trend_predictor::{
    DlTrendPredictor,
    DlTrendPredictorConfig,
    TrendClass,
    TrendPrediction,
    DlTrendPredictorStats,
    LobTransformerModel,
};

// DL/RL特徵工程相關導出
pub use parallel_feature_engineering::{
    DLRLFeatureEngine,
    DLRLFeatureConfig,
    EnhancedFeatureMatrix,
    FeatureType,
    FeatureMetadata,
    DLRLFeatureEngineStats,
    DeepLearningConfig,
    ReinforcementLearningConfig,
    FeatureExtractorType,
    CNNConfig,
    LSTMConfig,
    TransformerConfig,
    AutoEncoderConfig,
    GNNConfig,
    RewardFunctionType,
    QNetworkConfig,
    PolicyNetworkConfig,
    ExplorationStrategy,
    GPUConfig,
};

pub use dl_rl_extractors::{
    PriceCNNExtractor,
    SequenceLSTMExtractor,
    AttentionTransformerExtractor,
    AutoEncoderExtractor,
    MarketGNNExtractor,
    RLStateEncoder,
    MultiModalFusionExtractor,
    CNNFeatureConfig,
    LSTMFeatureConfig,
    TransformerFeatureConfig,
    AutoEncoderFeatureConfig,
    GNNFeatureConfig,
    RLFeatureConfig,
    FusionFeatureConfig,
    AttentionLayer,
    PositionalEncoding,
};

// 🧠 統一模型引擎相關導出 (主要接口)
pub use unified_model_engine::{
    UnifiedModelEngine,
    ModelEngineConfig,
    ModelType,
    ModelDevice,
    ModelOutput,
    BatchModelOutput,
    ModelEngineStats,
    LoadedModel,
    PreprocessingConfig,
    NormalizationConfig,
    FeatureScalingConfig,
    SequencePaddingConfig,
};

// ⚡ TorchScript推理引擎相關導出
pub use torchscript_inference::{
    TorchScriptInferenceEngine,
    ModelCacheManager,
    InferenceConfig,
    InferenceRequest,
    InferenceResult,
    InferenceInput,
    InferenceOutput,
    InputData,
    OutputData,
    InferenceOptions,
    InferencePriority,
    InferenceStats,
    ModelMetadata,
    PerformanceProfile,
    DeviceManager,
    InferenceDevice,
    DeviceType,
};