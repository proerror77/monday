/*!
 * Feature Engineering - 特征工程统一入口
 * 
 * 从原来1123行的巨大文件重构为模块化结构
 * 严格遵循CLAUDE.md的750行限制原则
 */

// Re-export all feature modules
pub use features::*;

// Import modular features
pub mod features;

// Legacy compatibility - 兼容旧接口
pub use features::{
    FeatureExtractor,
    FeatureExtractionStats,
    BasicFeatures,
    TechnicalIndicators, 
    MicrostructureFeatures,
    AdvancedFeatures,
};

use crate::core::types::*;
use anyhow::Result;

/// Create default feature extractor with standard window size
pub fn create_default_extractor() -> FeatureExtractor {
    FeatureExtractor::new(100)
}

/// Create fast feature extractor with smaller window for low latency
pub fn create_fast_extractor() -> FeatureExtractor {
    FeatureExtractor::new(20)
}

/// Create comprehensive feature extractor with larger window for accuracy
pub fn create_comprehensive_extractor() -> FeatureExtractor {
    FeatureExtractor::new(200)
}

/// Validate feature extraction result
pub fn validate_features(features: &FeatureSet) -> Result<()> {
    // Basic validation
    if features.mid_price.0 <= 0.0 {
        return Err(anyhow::anyhow!("Invalid mid price: {}", features.mid_price.0));
    }
    
    if features.spread < 0.0 {
        return Err(anyhow::anyhow!("Invalid spread: {}", features.spread));
    }
    
    // Additional validation can be added here
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::orderbook::OrderBook;

    #[test]
    fn test_feature_extractor_creation() {
        let extractor = create_default_extractor();
        let stats = extractor.get_stats();
        assert_eq!(stats.window_size, 100);
        assert_eq!(stats.total_extractions, 0);
    }

    #[test] 
    fn test_fast_extractor_creation() {
        let extractor = create_fast_extractor();
        let stats = extractor.get_stats();
        assert_eq!(stats.window_size, 20);
    }

    #[test]
    fn test_comprehensive_extractor_creation() {
        let extractor = create_comprehensive_extractor();
        let stats = extractor.get_stats();
        assert_eq!(stats.window_size, 200);
    }
}