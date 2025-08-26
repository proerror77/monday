//! 深度學習策略框架
//! 
//! Phase 2 實現要求：
//! 1. TorchScript 模型加載 (tch-rs)
//! 2. TopN → 歸一化特徵流水線
//! 3. 有界隊列 + last-wins 推理背壓  
//! 4. 超時/錯誤率觸發降級機制

pub mod config;
pub mod feature_pipeline;
pub mod model_loader;
pub mod inference_engine;
pub mod dl_strategy;

pub use config::{DlStrategyConfig, ModelConfig, FeatureConfig, InferenceConfig};
pub use feature_pipeline::{FeaturePipeline, FeatureExtractor};
pub use model_loader::{ModelLoader, ModelHandle};
pub use inference_engine::{InferenceEngine, InferenceRequest, InferenceResult};
pub use dl_strategy::{DlStrategy, DlStrategyStats};

use hft_core::{HftResult, HftError};