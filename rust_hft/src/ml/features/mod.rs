/*!
 * Feature Engineering Module - 特征工程模块
 * 
 * 将原有1123行的巨大文件拆分为可管理的模块
 * 严格遵守CLAUDE.md的750行限制原则
 */

pub mod basic_features;
pub mod technical_indicators;
pub mod microstructure_features;
pub mod advanced_features;

// Re-export all feature types
pub use basic_features::{BasicFeatureExtractor, BasicFeatures};
pub use technical_indicators::{TechnicalIndicators, TechnicalIndicatorCalculator};
pub use microstructure_features::{MicrostructureFeatures, MicrostructureExtractor};
pub use advanced_features::{AdvancedFeatures, AdvancedFeatureExtractor};

use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use anyhow::Result;
use std::collections::VecDeque;
use tracing::{debug, warn};

/// Unified Feature Extractor - 统一特征提取器
/// 整合所有子模块的功能，提供统一接口
#[derive(Debug)]
pub struct FeatureExtractor {
    basic_extractor: BasicFeatureExtractor,
    technical_calculator: TechnicalIndicatorCalculator,
    microstructure_extractor: MicrostructureExtractor,
    advanced_extractor: AdvancedFeatureExtractor,
    
    window_size: usize,
    extraction_count: u64,
    last_extraction: Timestamp,
}

impl FeatureExtractor {
    /// Create new unified feature extractor
    pub fn new(window_size: usize) -> Self {
        Self {
            basic_extractor: BasicFeatureExtractor::new(window_size),
            technical_calculator: TechnicalIndicatorCalculator::new(window_size),
            microstructure_extractor: MicrostructureExtractor::new(window_size),
            advanced_extractor: AdvancedFeatureExtractor::new(window_size),
            window_size,
            extraction_count: 0,
            last_extraction: now_micros(),
        }
    }

    /// Extract comprehensive feature set from orderbook
    pub fn extract_features(
        &mut self,
        orderbook: &OrderBook,
        network_latency_us: u64,
        processing_start: Timestamp,
    ) -> Result<FeatureSet> {
        let start_time = now_micros();
        let processing_latency = start_time - processing_start;

        // Basic orderbook validation
        if !orderbook.is_valid {
            warn!("Attempting to extract features from invalid orderbook");
        }

        // Extract from all modules
        let basic_features = self.basic_extractor.extract(orderbook)?;
        let technical_indicators = self.technical_calculator.calculate(&basic_features)?;
        let microstructure_features = self.microstructure_extractor.extract(orderbook)?;
        let advanced_features = self.advanced_extractor.extract(orderbook, &basic_features)?;

        // Combine all features
        let mut features = FeatureSet::default_enhanced();
        features.timestamp = start_time;
        features.latency_network_us = network_latency_us;
        features.latency_processing_us = processing_latency;
        
        // Populate from basic features
        features.best_bid = basic_features.best_bid;
        features.best_ask = basic_features.best_ask;
        features.mid_price = basic_features.mid_price;
        features.spread = basic_features.spread;
        features.spread_bps = basic_features.spread_bps;
        
        // Populate from microstructure features
        features.obi_l1 = microstructure_features.obi_l1;
        features.obi_l5 = microstructure_features.obi_l5;
        features.obi_l10 = microstructure_features.obi_l10;
        features.obi_l20 = microstructure_features.obi_l20;
        
        // Update extraction metrics
        self.extraction_count += 1;
        self.last_extraction = start_time;

        debug!(
            "Feature extraction completed in {} μs",
            now_micros() - start_time
        );

        Ok(features)
    }

    /// Get extraction statistics
    pub fn get_stats(&self) -> FeatureExtractionStats {
        FeatureExtractionStats {
            total_extractions: self.extraction_count,
            last_extraction: self.last_extraction,
            window_size: self.window_size,
        }
    }
}

/// Feature extraction statistics
#[derive(Debug, Clone)]
pub struct FeatureExtractionStats {
    pub total_extractions: u64,
    pub last_extraction: Timestamp,
    pub window_size: usize,
}