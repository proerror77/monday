/*!
 * LOB Time Series Extractor - Main Implementation
 * 
 * 主要的时间序列提取器实现
 */

use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use crate::ml::features::FeatureExtractor;
use crate::ml::lob_extractor::{
    config::LobTimeSeriesConfig,
    snapshot::LobTimeSeriesSnapshot,
    sequence::{LobTimeSeriesSequence, SequenceStats},
};
use anyhow::Result;
use tracing::{debug, warn};

/// Main LOB time series extractor
#[derive(Debug)]
pub struct LobTimeSeriesExtractor {
    /// Configuration
    config: LobTimeSeriesConfig,
    
    /// Feature extractor
    feature_extractor: FeatureExtractor,
    
    /// DL sequence (for trend prediction)
    dl_sequence: LobTimeSeriesSequence,
    
    /// RL sequence (for scalping)
    rl_sequence: LobTimeSeriesSequence,
    
    /// Last extraction timestamp
    last_extraction: Timestamp,
    
    /// Last memory cleanup timestamp
    last_cleanup: Timestamp,
    
    /// Total extractions count
    total_extractions: u64,
    
    /// Failed extractions count
    failed_extractions: u64,
}

impl LobTimeSeriesExtractor {
    /// Create new extractor with configuration
    pub fn new(config: LobTimeSeriesConfig) -> Result<Self> {
        config.validate()?;
        
        let dl_seq_len = ((config.dl_lookback_seconds * 1_000_000) / config.extraction_interval_us) as usize;
        let rl_seq_len = ((config.rl_state_window_ms * 1000) / config.extraction_interval_us) as usize;
        
        Ok(Self {
            feature_extractor: FeatureExtractor::new(100),
            dl_sequence: LobTimeSeriesSequence::new(dl_seq_len.min(config.max_sequence_length)),
            rl_sequence: LobTimeSeriesSequence::new(rl_seq_len.min(50)),
            last_extraction: 0,
            last_cleanup: 0,
            total_extractions: 0,
            failed_extractions: 0,
            config,
        })
    }

    /// Create with default configuration
    pub fn with_defaults() -> Result<Self> {
        Self::new(LobTimeSeriesConfig::default())
    }

    /// Extract time series features from orderbook
    pub fn extract(&mut self, orderbook: &OrderBook) -> Result<()> {
        let current_time = now_micros();
        
        // Check extraction interval
        if current_time - self.last_extraction < self.config.extraction_interval_us {
            return Ok(());
        }

        let extraction_start = now_micros();
        
        // Extract features
        match self.feature_extractor.extract_features(orderbook, 0, extraction_start) {
            Ok(features) => {
                // Create snapshot
                let snapshot = self.create_snapshot(features, orderbook, extraction_start)?;
                
                // Add to sequences
                self.dl_sequence.add_snapshot(snapshot.clone());
                self.rl_sequence.add_snapshot(snapshot);
                
                self.total_extractions += 1;
            }
            Err(e) => {
                warn!("Feature extraction failed: {}", e);
                self.failed_extractions += 1;
            }
        }
        
        self.last_extraction = current_time;
        
        // Periodic cleanup
        if current_time - self.last_cleanup > self.config.memory_cleanup_interval * 1_000_000 {
            self.cleanup_memory(current_time);
            self.last_cleanup = current_time;
        }
        
        Ok(())
    }

    /// Get DL sequence for trend prediction
    pub fn get_dl_sequence(&self) -> &LobTimeSeriesSequence {
        &self.dl_sequence
    }

    /// Get RL sequence for scalping
    pub fn get_rl_sequence(&self) -> &LobTimeSeriesSequence {
        &self.rl_sequence
    }

    /// Get DL tensor for model input
    pub fn get_dl_tensor(&self) -> Vec<f64> {
        self.dl_sequence.as_tensor()
    }

    /// Get RL state vector
    pub fn get_rl_state(&self) -> Vec<f64> {
        if let Some(latest) = self.rl_sequence.snapshots.back() {
            self.extract_rl_state_vector(latest)
        } else {
            vec![0.0; 50] // Default empty state
        }
    }

    /// Check if ready for DL prediction
    pub fn is_ready_for_dl_prediction(&self) -> bool {
        self.dl_sequence.is_ready_for_prediction()
    }

    /// Check if ready for RL action
    pub fn is_ready_for_rl_action(&self) -> bool {
        self.rl_sequence.snapshots.len() >= 5 // Need at least 5 states
    }

    /// Get extraction statistics
    pub fn get_stats(&self) -> LobTimeSeriesExtractorStats {
        let dl_stats = self.dl_sequence.get_stats();
        let rl_stats = self.rl_sequence.get_stats();
        
        LobTimeSeriesExtractorStats {
            total_extractions: self.total_extractions,
            failed_extractions: self.failed_extractions,
            success_rate: if self.total_extractions > 0 {
                (self.total_extractions - self.failed_extractions) as f64 / self.total_extractions as f64
            } else {
                0.0
            },
            dl_sequence_stats: dl_stats,
            rl_sequence_stats: rl_stats,
            memory_usage_bytes: self.calculate_memory_usage(),
            last_extraction: self.last_extraction,
        }
    }

    /// Create snapshot from features and orderbook
    fn create_snapshot(
        &self, 
        features: FeatureSet, 
        orderbook: &OrderBook,
        start_time: Timestamp
    ) -> Result<LobTimeSeriesSnapshot> {
        let processing_time = now_micros() - start_time;
        
        // Extract LOB levels
        let mut bid_levels = Vec::new();
        let mut ask_levels = Vec::new();
        
        for level in 1..=self.config.lob_depth {
            if let Some(bid) = orderbook.get_level(level, Side::Bid) {
                bid_levels.push((bid.price.0, bid.size));
            }
            if let Some(ask) = orderbook.get_level(level, Side::Ask) {
                ask_levels.push((ask.price.0, ask.size));
            }
        }

        // Create LOB tensor (simplified)
        let mut lob_tensor = Vec::new();
        for (price, size) in &bid_levels {
            lob_tensor.push(*price);
            lob_tensor.push(*size);
        }
        for (price, size) in &ask_levels {
            lob_tensor.push(*price);
            lob_tensor.push(*size);
        }

        // Calculate data quality score
        let data_quality_score = self.calculate_data_quality(&features, &bid_levels, &ask_levels);

        Ok(LobTimeSeriesSnapshot {
            timestamp: features.timestamp,
            features,
            lob_tensor,
            bid_levels,
            ask_levels,
            processing_time_us: processing_time,
            data_quality_score,
        })
    }

    /// Extract RL state vector from snapshot
    fn extract_rl_state_vector(&self, snapshot: &LobTimeSeriesSnapshot) -> Vec<f64> {
        let mut state = Vec::new();
        
        // Price and spread features
        state.push(snapshot.features.mid_price.0);
        state.push(snapshot.features.spread);
        state.push(snapshot.features.spread_bps);
        
        // Imbalance features
        state.push(snapshot.features.obi_l1);
        state.push(snapshot.features.obi_l5);
        
        // Depth features (top 5 levels)
        for i in 0..5.min(snapshot.bid_levels.len()) {
            state.push(snapshot.bid_levels[i].0); // price
            state.push(snapshot.bid_levels[i].1); // size
        }
        for i in 0..5.min(snapshot.ask_levels.len()) {
            state.push(snapshot.ask_levels[i].0); // price
            state.push(snapshot.ask_levels[i].1); // size
        }
        
        // Pad to fixed size if needed
        while state.len() < 50 {
            state.push(0.0);
        }
        
        state.truncate(50); // Ensure fixed size
        state
    }

    /// Calculate data quality score
    fn calculate_data_quality(
        &self, 
        features: &FeatureSet,
        bid_levels: &[(f64, f64)],
        ask_levels: &[(f64, f64)]
    ) -> f64 {
        let mut score = 1.0;
        
        // Penalize for wide spreads
        if features.spread_bps > 20.0 {
            score *= 0.7;
        }
        
        // Penalize for insufficient depth
        if bid_levels.len() < 5 || ask_levels.len() < 5 {
            score *= 0.8;
        }
        
        // Penalize for zero volumes
        let total_bid_volume: f64 = bid_levels.iter().map(|(_, size)| size).sum();
        let total_ask_volume: f64 = ask_levels.iter().map(|(_, size)| size).sum();
        
        if total_bid_volume < 100.0 || total_ask_volume < 100.0 {
            score *= 0.6;
        }
        
        score.max(0.0).min(1.0)
    }

    /// Cleanup old data from memory
    fn cleanup_memory(&mut self, current_time: Timestamp) {
        let dl_max_age = self.config.dl_lookback_seconds * 1_500_000; // 1.5x lookback
        let rl_max_age = self.config.rl_state_window_ms * 1_500; // 1.5x window
        
        self.dl_sequence.cleanup_old_snapshots(current_time, dl_max_age);
        self.rl_sequence.cleanup_old_snapshots(current_time, rl_max_age);
        
        debug!("Memory cleanup completed");
    }

    /// Calculate total memory usage
    fn calculate_memory_usage(&self) -> usize {
        std::mem::size_of::<Self>() + 
        self.dl_sequence.get_stats().memory_usage +
        self.rl_sequence.get_stats().memory_usage
    }
}

/// Extractor statistics
#[derive(Debug, Clone)]
pub struct LobTimeSeriesExtractorStats {
    pub total_extractions: u64,
    pub failed_extractions: u64,
    pub success_rate: f64,
    pub dl_sequence_stats: SequenceStats,
    pub rl_sequence_stats: SequenceStats,
    pub memory_usage_bytes: usize,
    pub last_extraction: Timestamp,
}