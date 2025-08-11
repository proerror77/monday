/*!
 * ML Module - TorchScript 推理和特徵提取
 * 
 * 只負責模型推理，不包含訓練功能
 * 模型訓練由 Python 層負責
 */

pub mod features;
pub mod lob_time_series_extractor;
pub mod lob_extractor;
pub mod inference_engine;

#[cfg(feature = "torchscript")]
pub mod torchscript_inference;

// 為沒有 torchscript 特性時提供基本推理接口
#[cfg(not(feature = "torchscript"))]
pub mod cpu_inference;

pub use features::FeatureExtractor;
// Re-export FeatureSet from core types
pub use crate::core::types::FeatureSet;
pub use lob_time_series_extractor::{
    LobTimeSeriesExtractor, 
    LobTimeSeriesConfig, 
    LobTimeSeriesSnapshot, 
    LobTimeSeriesSequence,
    LobTimeSeriesExtractorStats
};

#[cfg(feature = "torchscript")]
pub use torchscript_inference::{
    TorchScriptInference,
    InferenceResult,
    TradingAction,
    InferenceStatsSummary,
};

#[cfg(not(feature = "torchscript"))]
pub use cpu_inference::{
    CpuInference,
    InferenceResult,
    TradingAction,
    InferenceStatsSummary,
};

// Re-export the new inference engine
pub use inference_engine::{
    InferenceEngine, TradingSignal, SignalType, SignalAction, 
    ModelContainer, ModelType, ModelMetadata, InferenceConfig
};