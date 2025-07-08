/*!
 * High-Performance Feature Store using redb Time-Series Database
 * 
 * Optimized for ultra-low latency feature storage and retrieval
 * Supports real-time streaming and historical data access
 */

use crate::core::types::*;
use anyhow::Result;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Serialize, Deserialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{info, error, debug};

/// Feature store table definitions
const FEATURES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("features");
const MARKET_DATA_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("market_data");
const LABELS_TABLE: TableDefinition<u64, f64> = TableDefinition::new("labels");
const METADATA_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("metadata");

/// Compressed feature representation for storage
#[derive(Serialize, Deserialize, Debug, Clone, bincode::Encode, bincode::Decode)]
pub struct CompressedFeatures {
    pub timestamp: u64,
    pub symbol: String,
    
    // Basic features (compressed to f32 for storage efficiency)
    pub best_bid: f32,
    pub best_ask: f32,
    pub mid_price: f32,
    pub spread_bps: f32,
    
    // OBI features
    pub obi_l1: f32,
    pub obi_l5: f32,
    pub obi_l10: f32,
    pub obi_l20: f32,
    
    // Depth features (log-compressed)
    pub log_bid_depth_l5: f32,
    pub log_ask_depth_l5: f32,
    pub log_bid_depth_l10: f32,
    pub log_ask_depth_l10: f32,
    
    // Imbalance features
    pub depth_imbalance_l5: f32,
    pub depth_imbalance_l10: f32,
    pub depth_imbalance_l20: f32,
    
    // Derived features
    pub price_momentum: f32,
    pub volume_imbalance: f32,
    
    // Quality indicators
    pub data_quality_score: f32,
    pub latency_us: u32,
}

impl From<&FeatureSet> for CompressedFeatures {
    fn from(features: &FeatureSet) -> Self {
        Self {
            timestamp: features.timestamp,
            symbol: "BTCUSDT".to_string(), // TODO: make configurable
            
            best_bid: features.best_bid.0 as f32,
            best_ask: features.best_ask.0 as f32,
            mid_price: features.mid_price.0 as f32,
            spread_bps: features.spread_bps as f32,
            
            obi_l1: features.obi_l1 as f32,
            obi_l5: features.obi_l5 as f32,
            obi_l10: features.obi_l10 as f32,
            obi_l20: features.obi_l20 as f32,
            
            // Log compression for depth values
            log_bid_depth_l5: if features.bid_depth_l5 > 0.0 { (features.bid_depth_l5 + 1.0).ln() as f32 } else { 0.0 },
            log_ask_depth_l5: if features.ask_depth_l5 > 0.0 { (features.ask_depth_l5 + 1.0).ln() as f32 } else { 0.0 },
            log_bid_depth_l10: if features.bid_depth_l10 > 0.0 { (features.bid_depth_l10 + 1.0).ln() as f32 } else { 0.0 },
            log_ask_depth_l10: if features.ask_depth_l10 > 0.0 { (features.ask_depth_l10 + 1.0).ln() as f32 } else { 0.0 },
            
            depth_imbalance_l5: features.depth_imbalance_l5 as f32,
            depth_imbalance_l10: features.depth_imbalance_l10 as f32,
            depth_imbalance_l20: features.depth_imbalance_l20 as f32,
            
            price_momentum: features.price_momentum as f32,
            volume_imbalance: features.volume_imbalance as f32,
            
            data_quality_score: features.data_quality_score as f32,
            latency_us: (features.latency_network_us + features.latency_processing_us) as u32,
        }
    }
}

impl From<&CompressedFeatures> for FeatureSet {
    fn from(compressed: &CompressedFeatures) -> Self {
        let mut feature_set = Self::default_enhanced();
        feature_set.timestamp = compressed.timestamp;
        feature_set.latency_network_us = compressed.latency_us as u64;
        feature_set.latency_processing_us = 0;
        
        feature_set.best_bid = (compressed.best_bid as f64).to_price();
        feature_set.best_ask = (compressed.best_ask as f64).to_price();
        feature_set.mid_price = (compressed.mid_price as f64).to_price();
        feature_set.spread = (compressed.best_ask - compressed.best_bid) as f64;
        feature_set.spread_bps = compressed.spread_bps as f64;
        
        feature_set.obi_l1 = compressed.obi_l1 as f64;
        feature_set.obi_l5 = compressed.obi_l5 as f64;
        feature_set.obi_l10 = compressed.obi_l10 as f64;
        feature_set.obi_l20 = compressed.obi_l20 as f64;
        
        // Decompress depth values
        feature_set.bid_depth_l5 = if compressed.log_bid_depth_l5 > 0.0 { (compressed.log_bid_depth_l5 as f64).exp() - 1.0 } else { 0.0 };
        feature_set.ask_depth_l5 = if compressed.log_ask_depth_l5 > 0.0 { (compressed.log_ask_depth_l5 as f64).exp() - 1.0 } else { 0.0 };
        feature_set.bid_depth_l10 = if compressed.log_bid_depth_l10 > 0.0 { (compressed.log_bid_depth_l10 as f64).exp() - 1.0 } else { 0.0 };
        feature_set.ask_depth_l10 = if compressed.log_ask_depth_l10 > 0.0 { (compressed.log_ask_depth_l10 as f64).exp() - 1.0 } else { 0.0 };
        feature_set.bid_depth_l20 = 0.0; // Not stored in compressed format
        feature_set.ask_depth_l20 = 0.0; // Not stored in compressed format
        
        feature_set.depth_imbalance_l5 = compressed.depth_imbalance_l5 as f64;
        feature_set.depth_imbalance_l10 = compressed.depth_imbalance_l10 as f64;
        feature_set.depth_imbalance_l20 = compressed.depth_imbalance_l20 as f64;
        
        feature_set.bid_slope = 0.0; // Not stored in compressed format
        feature_set.ask_slope = 0.0; // Not stored in compressed format
        
        feature_set.total_bid_levels = 10; // Default value
        feature_set.total_ask_levels = 10; // Default value
        
        feature_set.price_momentum = compressed.price_momentum as f64;
        feature_set.volume_imbalance = compressed.volume_imbalance as f64;
        
        feature_set.is_valid = true;
        feature_set.data_quality_score = compressed.data_quality_score as f64;
        
        feature_set
    }
}

/// Feature store configuration
#[derive(Debug, Clone)]
pub struct FeatureStoreConfig {
    pub db_path: String,
    pub max_file_size: u64,
    pub compression_enabled: bool,
    pub cache_size_mb: usize,
    pub write_buffer_size: usize,
    pub retention_days: u32,
}

impl Default for FeatureStoreConfig {
    fn default() -> Self {
        Self {
            db_path: "data/features.redb".to_string(),
            max_file_size: 1024 * 1024 * 1024, // 1GB
            compression_enabled: true,
            cache_size_mb: 256,
            write_buffer_size: 10000,
            retention_days: 30,
        }
    }
}

/// High-performance feature store using redb
pub struct FeatureStore {
    db: Arc<Database>,
    config: FeatureStoreConfig,
    write_buffer: Vec<CompressedFeatures>,
    stats: FeatureStoreStats,
}

#[derive(Debug, Clone, Default)]
pub struct FeatureStoreStats {
    pub total_features_stored: u64,
    pub total_queries: u64,
    pub avg_write_latency_us: f64,
    pub avg_read_latency_us: f64,
    pub storage_size_bytes: u64,
    pub compression_ratio: f64,
    pub cache_hit_rate: f64,
}

impl FeatureStore {
    /// Create or open feature store
    pub fn new(config: FeatureStoreConfig) -> Result<Self> {
        // Ensure directory exists
        if let Some(parent) = Path::new(&config.db_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        // Open database with optimized settings
        let db = Database::create(&config.db_path)?;
        
        // Initialize tables
        {
            let write_txn = db.begin_write()?;
            {
                let _table = write_txn.open_table(FEATURES_TABLE)?;
                let _table = write_txn.open_table(MARKET_DATA_TABLE)?;
                let _table = write_txn.open_table(LABELS_TABLE)?;
                let _table = write_txn.open_table(METADATA_TABLE)?;
            }
            write_txn.commit()?;
        }
        
        info!("Feature store initialized at: {}", config.db_path);
        
        Ok(Self {
            db: Arc::new(db),
            config,
            write_buffer: Vec::with_capacity(10000),
            stats: FeatureStoreStats::default(),
        })
    }
    
    /// Store feature set with ultra-low latency
    pub fn store_features(&mut self, features: &FeatureSet) -> Result<()> {
        let start_time = now_micros();
        
        let compressed = CompressedFeatures::from(features);
        
        if self.config.compression_enabled {
            // Add to write buffer for batch processing
            self.write_buffer.push(compressed);
            
            // Flush buffer if it's full
            if self.write_buffer.len() >= self.config.write_buffer_size {
                self.flush_buffer()?;
            }
        } else {
            // Direct write for minimal latency
            self.write_single_feature(&compressed)?;
        }
        
        let latency = now_micros() - start_time;
        self.update_write_stats(latency);
        
        debug!("Feature stored in {}μs", latency);
        Ok(())
    }
    
    /// Flush write buffer to disk
    pub fn flush_buffer(&mut self) -> Result<()> {
        if self.write_buffer.is_empty() {
            return Ok(());
        }
        
        let start_time = now_micros();
        let write_txn = self.db.begin_write()?;
        
        {
            let mut table = write_txn.open_table(FEATURES_TABLE)?;
            
            for feature in &self.write_buffer {
                let key = feature.timestamp;
                let value = bincode::encode_to_vec(feature, bincode::config::standard())?;
                table.insert(key, value.as_slice())?;
            }
        }
        
        write_txn.commit()?;
        
        let count = self.write_buffer.len();
        self.write_buffer.clear();
        
        let latency = now_micros() - start_time;
        info!("Flushed {} features to disk in {}μs", count, latency);
        
        self.stats.total_features_stored += count as u64;
        Ok(())
    }
    
    /// Write single feature directly (minimal latency mode)
    fn write_single_feature(&mut self, feature: &CompressedFeatures) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FEATURES_TABLE)?;
            let key = feature.timestamp;
            let value = bincode::encode_to_vec(feature, bincode::config::standard())?;
            table.insert(key, value.as_slice())?;
        }
        write_txn.commit()?;
        
        self.stats.total_features_stored += 1;
        Ok(())
    }
    
    /// Query features by time range
    pub fn query_features(
        &mut self,
        start_time: Timestamp,
        end_time: Timestamp,
    ) -> Result<Vec<FeatureSet>> {
        let query_start = now_micros();
        
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FEATURES_TABLE)?;
        
        let mut features = Vec::new();
        
        // Range query on timestamp index
        let range = start_time..=end_time;
        for result in table.range(range)? {
            let (_key, value) = result?;
            let (compressed, _): (CompressedFeatures, _) = bincode::decode_from_slice(value.value(), bincode::config::standard())?;
            features.push(FeatureSet::from(&compressed));
        }
        
        let latency = now_micros() - query_start;
        self.update_read_stats(latency);
        
        debug!("Query returned {} features in {}μs", features.len(), latency);
        Ok(features)
    }
    
    /// Get latest N features
    pub fn get_latest_features(&mut self, count: usize) -> Result<Vec<FeatureSet>> {
        let query_start = now_micros();
        
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FEATURES_TABLE)?;
        
        let mut features = Vec::new();
        
        // Get latest entries by iterating backwards
        let mut iter = table.iter()?;
        iter.next_back(); // Start from the end
        
        for _ in 0..count {
            if let Some(result) = iter.next_back() {
                let (_key, value) = result?;
                let (compressed, _): (CompressedFeatures, _) = bincode::decode_from_slice(value.value(), bincode::config::standard())?;
                features.push(FeatureSet::from(&compressed));
            } else {
                break;
            }
        }
        
        // Reverse to get chronological order
        features.reverse();
        
        let latency = now_micros() - query_start;
        self.update_read_stats(latency);
        
        debug!("Retrieved {} latest features in {}μs", features.len(), latency);
        Ok(features)
    }
    
    /// Store raw orderbook update
    pub fn store_orderbook_update(&mut self, update: &OrderBookUpdate) -> Result<()> {
        let start_time = now_micros();
        
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(MARKET_DATA_TABLE)?;
            let key = update.timestamp;
            let value = serde_json::to_vec(update)?;
            table.insert(key, value.as_slice())?;
        }
        write_txn.commit()?;
        
        let latency = now_micros() - start_time;
        self.update_write_stats(latency);
        self.stats.total_features_stored += 1;
        
        debug!("OrderBook update stored in {}μs", latency);
        Ok(())
    }

    /// Store training label
    pub fn store_label(&self, timestamp: Timestamp, label: f64) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(LABELS_TABLE)?;
            table.insert(timestamp, label)?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Query labels by time range
    pub fn query_labels(&self, start_time: Timestamp, end_time: Timestamp) -> Result<Vec<(Timestamp, f64)>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(LABELS_TABLE)?;
        
        let mut labels = Vec::new();
        let range = start_time..=end_time;
        
        for result in table.range(range)? {
            let (key, value) = result?;
            labels.push((key.value(), value.value()));
        }
        
        Ok(labels)
    }
    
    /// Store metadata
    pub fn store_metadata(&self, key: &str, value: &[u8]) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(METADATA_TABLE)?;
            table.insert(key, value)?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Get metadata
    pub fn get_metadata(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(METADATA_TABLE)?;
        
        match table.get(key)? {
            Some(value) => Ok(Some(value.value().to_vec())),
            None => Ok(None),
        }
    }
    
    /// Cleanup old data based on retention policy
    pub fn cleanup_old_data(&mut self) -> Result<u64> {
        let retention_micros = self.config.retention_days as u64 * 24 * 60 * 60 * 1_000_000;
        let cutoff_time = now_micros().saturating_sub(retention_micros);
        
        let write_txn = self.db.begin_write()?;
        let mut deleted_count = 0;
        
        {
            let mut table = write_txn.open_table(FEATURES_TABLE)?;
            let keys_to_delete: Vec<u64> = table
                .range(..cutoff_time)?
                .map(|result| result.map(|(key, _)| key.value()))
                .collect::<Result<Vec<_>, _>>()?;
            
            for key in keys_to_delete {
                table.remove(key)?;
                deleted_count += 1;
            }
        }
        
        write_txn.commit()?;
        
        info!("Cleaned up {} old feature records", deleted_count);
        Ok(deleted_count)
    }
    
    /// Get aggregated statistics
    pub fn get_stats(&self) -> FeatureStoreStats {
        self.stats.clone()
    }
    
    /// Update write statistics
    fn update_write_stats(&mut self, latency_us: u64) {
        let alpha = 0.1;
        self.stats.avg_write_latency_us = 
            alpha * latency_us as f64 + (1.0 - alpha) * self.stats.avg_write_latency_us;
    }
    
    /// Update read statistics
    fn update_read_stats(&mut self, latency_us: u64) {
        let alpha = 0.1;
        self.stats.avg_read_latency_us = 
            alpha * latency_us as f64 + (1.0 - alpha) * self.stats.avg_read_latency_us;
        self.stats.total_queries += 1;
    }
    
    /// Get database size on disk
    pub fn get_storage_size(&self) -> Result<u64> {
        let metadata = std::fs::metadata(&self.config.db_path)?;
        Ok(metadata.len())
    }
    
    /// Compact database to optimize storage
    pub fn compact(&self) -> Result<()> {
        // redb handles compaction automatically, but we can force it
        info!("Database compaction requested - redb handles this automatically");
        Ok(())
    }
}

impl Drop for FeatureStore {
    fn drop(&mut self) {
        if let Err(e) = self.flush_buffer() {
            error!("Failed to flush buffer on drop: {}", e);
        }
    }
}

/// Streaming feature writer for real-time data
pub struct StreamingFeatureWriter {
    store: FeatureStore,
    batch_buffer: Vec<FeatureSet>,
    batch_size: usize,
    last_flush: Timestamp,
    flush_interval_us: u64,
}

impl StreamingFeatureWriter {
    pub fn new(store: FeatureStore, batch_size: usize, flush_interval_ms: u64) -> Self {
        Self {
            store,
            batch_buffer: Vec::with_capacity(batch_size),
            batch_size,
            last_flush: now_micros(),
            flush_interval_us: flush_interval_ms * 1000,
        }
    }
    
    /// Add feature to streaming buffer
    pub fn add_feature(&mut self, feature: FeatureSet) -> Result<()> {
        self.batch_buffer.push(feature);
        
        let now = now_micros();
        let should_flush = self.batch_buffer.len() >= self.batch_size ||
                          (now - self.last_flush) >= self.flush_interval_us;
        
        if should_flush {
            self.flush_batch()?;
        }
        
        Ok(())
    }
    
    /// Flush current batch to store
    pub fn flush_batch(&mut self) -> Result<()> {
        if self.batch_buffer.is_empty() {
            return Ok(());
        }
        
        let start_time = now_micros();
        
        for feature in &self.batch_buffer {
            self.store.store_features(feature)?;
        }
        
        self.store.flush_buffer()?;
        
        let count = self.batch_buffer.len();
        self.batch_buffer.clear();
        self.last_flush = now_micros();
        
        let latency = now_micros() - start_time;
        debug!("Flushed batch of {} features in {}μs", count, latency);
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    
    #[test]
    fn test_feature_store_creation() {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb").to_string_lossy().to_string();
        
        let config = FeatureStoreConfig {
            db_path,
            ..Default::default()
        };
        
        let store = FeatureStore::new(config);
        assert!(store.is_ok());
    }
    
    #[test]
    fn test_feature_compression() {
        let mut original = FeatureSet::default_enhanced();
        original.timestamp = now_micros();
        original.latency_network_us = 10;
        original.latency_processing_us = 20;
        original.best_bid = 100.0.to_price();
        original.best_ask = 101.0.to_price();
        original.mid_price = 100.5.to_price();
        original.spread = 1.0;
        original.spread_bps = 100.0;
        original.obi_l1 = 0.1;
        original.obi_l5 = 0.05;
        original.obi_l10 = 0.02;
        original.obi_l20 = 0.01;
        original.bid_depth_l5 = 1000.0;
        original.ask_depth_l5 = 1100.0;
        original.bid_depth_l10 = 2000.0;
        original.ask_depth_l10 = 2200.0;
        original.bid_depth_l20 = 4000.0;
        original.ask_depth_l20 = 4400.0;
        original.depth_imbalance_l5 = -0.05;
        original.depth_imbalance_l10 = -0.05;
        original.depth_imbalance_l20 = -0.05;
        original.bid_slope = 0.1;
        original.ask_slope = 0.1;
        original.total_bid_levels = 10;
        original.total_ask_levels = 12;
        original.price_momentum = 0.001;
        original.volume_imbalance = 0.05;
        original.is_valid = true;
        original.data_quality_score = 0.95;
        
        let compressed = CompressedFeatures::from(&original);
        let decompressed = FeatureSet::from(&compressed);
        
        // Check key fields are preserved (allowing for compression loss)
        assert!((original.best_bid.0 - decompressed.best_bid.0).abs() < 0.01);
        assert!((original.obi_l10 - decompressed.obi_l10).abs() < 0.001);
        assert!((original.data_quality_score - decompressed.data_quality_score).abs() < 0.01);
    }
    
    #[test]
    fn test_store_and_query() {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb").to_string_lossy().to_string();
        
        let config = FeatureStoreConfig {
            db_path,
            write_buffer_size: 1, // Force immediate writes
            ..Default::default()
        };
        
        let mut store = FeatureStore::new(config).unwrap();
        
        // Store a test feature
        let mut feature = FeatureSet::default_enhanced();
        feature.timestamp = 1000000;
        feature.latency_network_us = 10;
        feature.latency_processing_us = 20;
        feature.best_bid = 100.0.to_price();
        feature.best_ask = 101.0.to_price();
        feature.mid_price = 100.5.to_price();
        feature.spread = 1.0;
        feature.spread_bps = 100.0;
        feature.obi_l1 = 0.1;
        feature.obi_l5 = 0.05;
        feature.obi_l10 = 0.02;
        feature.obi_l20 = 0.01;
        feature.bid_depth_l5 = 1000.0;
        feature.ask_depth_l5 = 1100.0;
        feature.bid_depth_l10 = 2000.0;
        feature.ask_depth_l10 = 2200.0;
        feature.bid_depth_l20 = 4000.0;
        feature.ask_depth_l20 = 4400.0;
        feature.depth_imbalance_l5 = -0.05;
        feature.depth_imbalance_l10 = -0.05;
        feature.depth_imbalance_l20 = -0.05;
        feature.bid_slope = 0.1;
        feature.ask_slope = 0.1;
        feature.total_bid_levels = 10;
        feature.total_ask_levels = 12;
        feature.price_momentum = 0.001;
        feature.volume_imbalance = 0.05;
        feature.is_valid = true;
        feature.data_quality_score = 0.95;
        
        store.store_features(&feature).unwrap();
        store.flush_buffer().unwrap();
        
        // Query it back
        let results = store.query_features(999999, 1000001).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].timestamp, 1000000);
    }
}