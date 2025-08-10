/*!
 * LOB Time Series Snapshot
 * 
 * 时间序列快照数据结构
 */

use crate::core::types::*;
use serde::{Serialize, Deserialize};

/// LOB time series snapshot at a specific timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobTimeSeriesSnapshot {
    /// Timestamp of this snapshot
    pub timestamp: Timestamp,
    
    /// Complete feature set at this moment
    pub features: FeatureSet,
    
    /// LOB tensor data (flattened)
    pub lob_tensor: Vec<f64>,
    
    /// Raw bid levels (price, size) for top N levels
    pub bid_levels: Vec<(f64, f64)>,
    
    /// Raw ask levels (price, size) for top N levels  
    pub ask_levels: Vec<(f64, f64)>,
    
    /// Processing metadata
    pub processing_time_us: u64,
    pub data_quality_score: f64,
}

impl LobTimeSeriesSnapshot {
    /// Create new empty snapshot
    pub fn new(timestamp: Timestamp) -> Self {
        Self {
            timestamp,
            features: FeatureSet::default(),
            lob_tensor: Vec::new(),
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            processing_time_us: 0,
            data_quality_score: 0.0,
        }
    }

    /// Check if snapshot is valid
    pub fn is_valid(&self) -> bool {
        self.data_quality_score > 0.5 && 
        !self.bid_levels.is_empty() && 
        !self.ask_levels.is_empty()
    }

    /// Get snapshot age in microseconds
    pub fn age_us(&self, current_time: Timestamp) -> u64 {
        if current_time > self.timestamp {
            current_time - self.timestamp
        } else {
            0
        }
    }

    /// Calculate memory usage
    pub fn memory_usage_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + 
        self.lob_tensor.len() * std::mem::size_of::<f64>() +
        self.bid_levels.len() * std::mem::size_of::<(f64, f64)>() +
        self.ask_levels.len() * std::mem::size_of::<(f64, f64)>()
    }
}