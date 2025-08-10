/*!
 * LOB Time Series Sequence
 * 
 * 时间序列序列数据结构和操作
 */

use crate::ml::lob_extractor::snapshot::LobTimeSeriesSnapshot;
use crate::core::types::*;
use std::collections::VecDeque;
use serde::{Serialize, Deserialize};

/// Time series sequence for ML model input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobTimeSeriesSequence {
    /// Sequence of snapshots
    pub snapshots: VecDeque<LobTimeSeriesSnapshot>,
    
    /// Maximum sequence length
    pub max_length: usize,
    
    /// Sequence start timestamp
    pub start_timestamp: Timestamp,
    
    /// Sequence end timestamp  
    pub end_timestamp: Timestamp,
    
    /// Average data quality score
    pub avg_quality_score: f64,
}

impl LobTimeSeriesSequence {
    /// Create new sequence with maximum length
    pub fn new(max_length: usize) -> Self {
        Self {
            snapshots: VecDeque::with_capacity(max_length),
            max_length,
            start_timestamp: 0,
            end_timestamp: 0,
            avg_quality_score: 0.0,
        }
    }

    /// Add snapshot to sequence
    pub fn add_snapshot(&mut self, snapshot: LobTimeSeriesSnapshot) {
        // Remove oldest if at capacity
        if self.snapshots.len() >= self.max_length {
            self.snapshots.pop_front();
        }
        
        // Update timestamps
        if self.snapshots.is_empty() {
            self.start_timestamp = snapshot.timestamp;
        }
        self.end_timestamp = snapshot.timestamp;
        
        self.snapshots.push_back(snapshot);
        self.update_quality_score();
    }

    /// Get sequence as tensor (flattened)
    pub fn as_tensor(&self) -> Vec<f64> {
        let mut tensor = Vec::new();
        
        for snapshot in &self.snapshots {
            tensor.extend_from_slice(&snapshot.lob_tensor);
        }
        
        tensor
    }

    /// Get feature matrix (time x features)
    pub fn as_feature_matrix(&self) -> Vec<Vec<f64>> {
        self.snapshots.iter()
            .map(|s| self.extract_features_vector(s))
            .collect()
    }

    /// Check if sequence is ready for prediction
    pub fn is_ready_for_prediction(&self) -> bool {
        self.snapshots.len() >= self.max_length / 2 && 
        self.avg_quality_score > 0.7
    }

    /// Get sequence statistics
    pub fn get_stats(&self) -> SequenceStats {
        SequenceStats {
            length: self.snapshots.len(),
            max_length: self.max_length,
            duration_us: self.end_timestamp.saturating_sub(self.start_timestamp),
            avg_quality: self.avg_quality_score,
            memory_usage: self.calculate_memory_usage(),
        }
    }

    /// Clean old snapshots beyond time window
    pub fn cleanup_old_snapshots(&mut self, current_time: Timestamp, max_age_us: u64) {
        let cutoff_time = current_time.saturating_sub(max_age_us);
        
        while let Some(front) = self.snapshots.front() {
            if front.timestamp < cutoff_time {
                self.snapshots.pop_front();
            } else {
                break;
            }
        }
        
        // Update start timestamp
        if let Some(front) = self.snapshots.front() {
            self.start_timestamp = front.timestamp;
        }
        
        self.update_quality_score();
    }

    /// Extract features vector from snapshot
    fn extract_features_vector(&self, snapshot: &LobTimeSeriesSnapshot) -> Vec<f64> {
        let mut features = Vec::new();
        
        // Basic price features
        features.push(snapshot.features.mid_price.0);
        features.push(snapshot.features.spread);
        features.push(snapshot.features.spread_bps);
        
        // OBI features
        features.push(snapshot.features.obi_l1);
        features.push(snapshot.features.obi_l5);
        features.push(snapshot.features.obi_l10);
        features.push(snapshot.features.obi_l20);
        
        // Depth features
        features.push(snapshot.features.bid_depth_l5);
        features.push(snapshot.features.ask_depth_l5);
        features.push(snapshot.features.bid_depth_l10);
        features.push(snapshot.features.ask_depth_l10);
        
        // Add LOB tensor if available
        if !snapshot.lob_tensor.is_empty() {
            features.extend_from_slice(&snapshot.lob_tensor);
        }
        
        features
    }

    /// Update average quality score
    fn update_quality_score(&mut self) {
        if self.snapshots.is_empty() {
            self.avg_quality_score = 0.0;
            return;
        }
        
        let total_quality: f64 = self.snapshots.iter()
            .map(|s| s.data_quality_score)
            .sum();
            
        self.avg_quality_score = total_quality / self.snapshots.len() as f64;
    }

    /// Calculate total memory usage
    fn calculate_memory_usage(&self) -> usize {
        std::mem::size_of::<Self>() + 
        self.snapshots.iter()
            .map(|s| s.memory_usage_bytes())
            .sum::<usize>()
    }
}

/// Sequence statistics
#[derive(Debug, Clone)]
pub struct SequenceStats {
    pub length: usize,
    pub max_length: usize,
    pub duration_us: u64,
    pub avg_quality: f64,
    pub memory_usage: usize,
}