/*!
 * ML Module - 機器學習和特徵工程
 * 
 * 包含特徵提取、模型訓練和在線學習功能
 */

pub mod features;
pub mod model_training_simple;
pub mod online_learning;
pub mod lob_time_series_extractor;
pub mod dl_trend_predictor;

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