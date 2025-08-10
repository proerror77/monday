/*!
 * LOB Time Series Feature Extractor for DL+RL Trading System
 * 
 * Specialized time series feature extraction for:
 * - DL Trend Prediction: 30-second sequences → 10-second predictions
 * - RL Scalping: Millisecond-level state representation
 * 
 * Based on existing FeatureExtractor but optimized for temporal modeling
 */

use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use crate::ml::features::{FeatureExtractor, validate_features};
use anyhow::Result;
use std::collections::VecDeque;
use tracing::{debug, warn};
use serde::{Serialize, Deserialize};
// 注意：Device 和 Tensor 類型需要在啟用 torchscript 特性時導入
// 目前為了兼容性，使用 mock 實現
use rust_decimal::prelude::ToPrimitive;

/// Time series configuration for LOB analysis
#[derive(Debug, Clone)]
pub struct LobTimeSeriesConfig {
    /// Lookback window for DL trend prediction (seconds)
    pub dl_lookback_seconds: u64,          // 30 seconds
    
    /// Prediction horizon for DL model (seconds)  
    pub dl_prediction_seconds: u64,        // 10 seconds
    
    /// RL state history window (milliseconds)
    pub rl_state_window_ms: u64,           // 5000 ms (5 seconds)
    
    /// Feature extraction frequency (microseconds)
    pub extraction_interval_us: u64,      // 500_000 μs (500ms)
    
    /// Maximum sequence length for Transformer
    pub max_sequence_length: usize,       // 60 steps (30s / 500ms)
    
    /// LOB depth for tensor representation
    pub lob_depth: usize,                 // 20 levels
    
    /// Enable advanced LOB features
    pub enable_advanced_features: bool,    // true
    
    /// Memory optimization frequency
    pub memory_cleanup_interval: u64,     // 60 seconds
}

impl Default for LobTimeSeriesConfig {
    fn default() -> Self {
        Self {
            dl_lookback_seconds: 30,
            dl_prediction_seconds: 10,
            rl_state_window_ms: 5000,
            extraction_interval_us: 500_000, // 500ms
            max_sequence_length: 60,          // 30s / 500ms
            lob_depth: 20,
            enable_advanced_features: true,
            memory_cleanup_interval: 60,
        }
    }
}

/// Time series feature snapshot for a single timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobTimeSeriesSnapshot {
    /// Timestamp of this snapshot
    pub timestamp: Timestamp,
    
    /// Basic LOB features
    pub basic_features: LobBasicFeatures,
    
    /// Microstructure features
    pub microstructure_features: LobMicrostructureFeatures,
    
    /// Order flow features
    pub order_flow_features: LobOrderFlowFeatures,
    
    /// LOB tensor representation (L20)
    pub lob_tensor: Vec<f64>,  // Flattened 2L x depth tensor
    
    /// Price and volume deltas from previous snapshot
    pub deltas: LobDeltas,
    
    /// Market regime indicators
    pub regime_indicators: LobRegimeIndicators,
}

/// Basic LOB features for each timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobBasicFeatures {
    pub mid_price: f64,
    pub spread_bps: f64,
    pub bid_ask_ratio: f64,
    pub depth_ratio_l5: f64,   // bid_depth_l5 / ask_depth_l5
    pub depth_ratio_l10: f64,
    pub depth_ratio_l20: f64,
    pub obi_l5: f64,
    pub obi_l10: f64,
    pub obi_l20: f64,
}

/// Microstructure features capturing market dynamics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobMicrostructureFeatures {
    pub microprice: f64,
    pub effective_spread: f64,
    pub realized_volatility: f64,
    pub bid_slope: f64,
    pub ask_slope: f64,
    pub price_impact: f64,
    pub liquidity_score: f64,
    pub order_arrival_intensity: f64,
}

/// Order flow features for detecting momentum
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobOrderFlowFeatures {
    pub order_flow_imbalance: f64,
    pub trade_intensity: f64,
    pub volume_acceleration: f64,
    pub depth_pressure_bid: f64,
    pub depth_pressure_ask: f64,
    pub cancellation_rate: f64,
    pub fill_rate_estimate: f64,
    pub aggressive_order_ratio: f64,
}

/// Price/volume deltas from previous snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobDeltas {
    pub price_delta_bps: f64,      // Price change in basis points
    pub volume_delta_pct: f64,     // Volume change percentage
    pub spread_delta_bps: f64,     // Spread change in basis points
    pub obi_delta: f64,            // OBI change
    pub depth_delta_pct: f64,      // Total depth change percentage
    pub microprice_delta_bps: f64, // Microprice change in basis points
}

/// Market regime indicators
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobRegimeIndicators {
    pub volatility_regime: f64,    // High/low volatility indicator
    pub liquidity_regime: f64,     // High/low liquidity indicator
    pub trending_regime: f64,      // Trending vs mean-reverting
    pub stress_regime: f64,        // Market stress indicator
    pub momentum_regime: f64,      // Momentum vs reversal
}

/// Time series sequence for DL model input
#[derive(Debug, Clone)]
pub struct LobTimeSeriesSequence {
    /// Sequence of snapshots
    pub snapshots: VecDeque<LobTimeSeriesSnapshot>,
    
    /// Start timestamp of sequence
    pub start_timestamp: Timestamp,
    
    /// End timestamp of sequence
    pub end_timestamp: Timestamp,
    
    /// Sequence length
    pub length: usize,
    
    /// Target values for training (if available)
    pub targets: Option<Vec<f64>>,
}

impl LobTimeSeriesSequence {
    /// Convert sequence to feature matrix (準備供 TorchScript 使用)
    pub fn to_feature_matrix(&self) -> Result<Vec<Vec<f64>>> {
        let seq_len = self.snapshots.len();
        if seq_len == 0 {
            return Err(anyhow::anyhow!("Empty sequence cannot be converted to feature matrix"));
        }
        
        let mut matrix = Vec::with_capacity(seq_len);
        
        for snapshot in &self.snapshots {
            let feature_vector = self.snapshot_to_vector(snapshot);
            matrix.push(feature_vector);
        }
        
        Ok(matrix)
    }
    
    /// Convert to tensor when torchscript feature is enabled
    #[cfg(feature = "torchscript")]
    pub fn to_tensor(&self, device: &tch::Device) -> Result<tch::Tensor> {
        let matrix = self.to_feature_matrix()?;
        let seq_len = matrix.len();
        let feature_dim = if seq_len > 0 { matrix[0].len() } else { 0 };
        
        let mut data: Vec<f32> = Vec::with_capacity(seq_len * feature_dim);
        
        for row in matrix {
            data.extend(row.into_iter().map(|x| x as f32));
        }
        
        // Create tensor with shape [1, seq_len, feature_dim] for batch processing
        let tensor = tch::Tensor::of_slice(&data).reshape(&[1i64, seq_len as i64, feature_dim as i64]).to_device(*device);
        
        Ok(tensor)
    }
    
    /// Convert snapshot to feature vector
    fn snapshot_to_vector(&self, snapshot: &LobTimeSeriesSnapshot) -> Vec<f64> {
        let mut vector = Vec::new();
        
        // Basic features (9)
        vector.push(snapshot.basic_features.mid_price);
        vector.push(snapshot.basic_features.spread_bps);
        vector.push(snapshot.basic_features.bid_ask_ratio);
        vector.push(snapshot.basic_features.depth_ratio_l5);
        vector.push(snapshot.basic_features.depth_ratio_l10);
        vector.push(snapshot.basic_features.depth_ratio_l20);
        vector.push(snapshot.basic_features.obi_l5);
        vector.push(snapshot.basic_features.obi_l10);
        vector.push(snapshot.basic_features.obi_l20);
        
        // Microstructure features (8)
        vector.push(snapshot.microstructure_features.microprice);
        vector.push(snapshot.microstructure_features.effective_spread);
        vector.push(snapshot.microstructure_features.realized_volatility);
        vector.push(snapshot.microstructure_features.bid_slope);
        vector.push(snapshot.microstructure_features.ask_slope);
        vector.push(snapshot.microstructure_features.price_impact);
        vector.push(snapshot.microstructure_features.liquidity_score);
        vector.push(snapshot.microstructure_features.order_arrival_intensity);
        
        // Order flow features (8)
        vector.push(snapshot.order_flow_features.order_flow_imbalance);
        vector.push(snapshot.order_flow_features.trade_intensity);
        vector.push(snapshot.order_flow_features.volume_acceleration);
        vector.push(snapshot.order_flow_features.depth_pressure_bid);
        vector.push(snapshot.order_flow_features.depth_pressure_ask);
        vector.push(snapshot.order_flow_features.cancellation_rate);
        vector.push(snapshot.order_flow_features.fill_rate_estimate);
        vector.push(snapshot.order_flow_features.aggressive_order_ratio);
        
        // Deltas (6)
        vector.push(snapshot.deltas.price_delta_bps);
        vector.push(snapshot.deltas.volume_delta_pct);
        vector.push(snapshot.deltas.spread_delta_bps);
        vector.push(snapshot.deltas.obi_delta);
        vector.push(snapshot.deltas.depth_delta_pct);
        vector.push(snapshot.deltas.microprice_delta_bps);
        
        // Regime indicators (5)
        vector.push(snapshot.regime_indicators.volatility_regime);
        vector.push(snapshot.regime_indicators.liquidity_regime);
        vector.push(snapshot.regime_indicators.trending_regime);
        vector.push(snapshot.regime_indicators.stress_regime);
        vector.push(snapshot.regime_indicators.momentum_regime);
        
        // LOB tensor (40 for L20: 20 bids + 20 asks)
        vector.extend(&snapshot.lob_tensor);
        
        vector
    }
    
    /// Calculate total feature dimension
    fn calculate_feature_dimension(&self) -> usize {
        9 +    // Basic features
        8 +    // Microstructure features
        8 +    // Order flow features
        6 +    // Deltas
        5 +    // Regime indicators
        40     // LOB tensor (L20)
        // Total: 76 features
    }
}

/// Main LOB time series feature extractor
#[derive(Debug)]
pub struct LobTimeSeriesExtractor {
    /// Configuration
    config: LobTimeSeriesConfig,
    
    /// Base feature extractor
    base_extractor: FeatureExtractor,
    
    /// Time series snapshots buffer
    snapshots: VecDeque<LobTimeSeriesSnapshot>,
    
    /// Previous snapshot for delta calculation
    prev_snapshot: Option<LobTimeSeriesSnapshot>,
    
    /// Statistics
    extraction_count: u64,
    last_extraction: Timestamp,
    last_memory_cleanup: Timestamp,
    
    /// Performance metrics
    avg_extraction_latency_us: f64,
    total_extractions: u64,
    
    /// Symbol for this extractor
    symbol: String,
}

impl LobTimeSeriesExtractor {
    /// Create new LOB time series extractor
    pub fn new(symbol: String, config: LobTimeSeriesConfig) -> Self {
        let window_size = config.max_sequence_length;
        let base_extractor = FeatureExtractor::new(window_size);
        
        Self {
            config,
            base_extractor,
            snapshots: VecDeque::with_capacity(window_size * 2), // 2x for safety
            prev_snapshot: None,
            extraction_count: 0,
            last_extraction: now_micros(),
            last_memory_cleanup: now_micros(),
            avg_extraction_latency_us: 0.0,
            total_extractions: 0,
            symbol,
        }
    }
    
    /// Extract time series snapshot from current orderbook
    pub fn extract_snapshot(
        &mut self,
        orderbook: &OrderBook,
        network_latency_us: u64,
    ) -> Result<LobTimeSeriesSnapshot> {
        let start_time = now_micros();
        
        // First extract base features using existing extractor
        let base_features = self.base_extractor.extract_features(
            orderbook,
            network_latency_us,
            start_time,
        )?;
        
        // Validate base features
        if !validate_features(&base_features) {
            warn!("Base features validation failed for {}", self.symbol);
            return Err(anyhow::anyhow!("Invalid base features"));
        }
        
        // Build LOB time series snapshot
        let snapshot = self.build_snapshot(orderbook, &base_features, start_time)?;
        
        // Update statistics
        self.update_statistics(start_time);
        
        // Store snapshot in buffer
        self.add_snapshot(snapshot.clone());
        
        // Periodic memory cleanup
        self.cleanup_memory_if_needed();
        
        debug!("LOB time series extraction completed for {} in {}μs", 
               self.symbol, now_micros() - start_time);
        
        Ok(snapshot)
    }
    
    /// Build snapshot from orderbook and base features
    fn build_snapshot(
        &mut self,
        orderbook: &OrderBook,
        base_features: &FeatureSet,
        timestamp: Timestamp,
    ) -> Result<LobTimeSeriesSnapshot> {
        // Basic features
        let basic_features = LobBasicFeatures {
            mid_price: base_features.mid_price.0,
            spread_bps: base_features.spread_bps,
            bid_ask_ratio: if base_features.ask_depth_l5 > 0.0 {
                base_features.bid_depth_l5 / base_features.ask_depth_l5
            } else { 1.0 },
            depth_ratio_l5: if base_features.ask_depth_l5 > 0.0 {
                base_features.bid_depth_l5 / base_features.ask_depth_l5
            } else { 1.0 },
            depth_ratio_l10: if base_features.ask_depth_l10 > 0.0 {
                base_features.bid_depth_l10 / base_features.ask_depth_l10
            } else { 1.0 },
            depth_ratio_l20: if base_features.ask_depth_l20 > 0.0 {
                base_features.bid_depth_l20 / base_features.ask_depth_l20
            } else { 1.0 },
            obi_l5: base_features.obi_l5,
            obi_l10: base_features.obi_l10,
            obi_l20: base_features.obi_l20,
        };
        
        // Microstructure features
        let microstructure_features = LobMicrostructureFeatures {
            microprice: base_features.microprice,
            effective_spread: base_features.effective_spread,
            realized_volatility: base_features.realized_volatility,
            bid_slope: base_features.bid_slope,
            ask_slope: base_features.ask_slope,
            price_impact: base_features.market_impact,
            liquidity_score: base_features.liquidity_score,
            order_arrival_intensity: base_features.order_arrival_rate,
        };
        
        // Order flow features
        let order_flow_features = LobOrderFlowFeatures {
            order_flow_imbalance: base_features.order_flow_imbalance,
            trade_intensity: base_features.trade_intensity,
            volume_acceleration: base_features.volume_acceleration,
            depth_pressure_bid: base_features.depth_pressure_bid,
            depth_pressure_ask: base_features.depth_pressure_ask,
            cancellation_rate: base_features.cancellation_rate,
            fill_rate_estimate: self.estimate_fill_rate(orderbook),
            aggressive_order_ratio: self.estimate_aggressive_ratio(orderbook),
        };
        
        // Calculate deltas from previous snapshot
        let deltas = if let Some(ref prev) = self.prev_snapshot {
            self.calculate_deltas(&basic_features, &microstructure_features, prev)
        } else {
            LobDeltas {
                price_delta_bps: 0.0,
                volume_delta_pct: 0.0,
                spread_delta_bps: 0.0,
                obi_delta: 0.0,
                depth_delta_pct: 0.0,
                microprice_delta_bps: 0.0,
            }
        };
        
        // Calculate regime indicators
        let regime_indicators = self.calculate_regime_indicators(orderbook, &basic_features);
        
        // Extract LOB tensor
        let lob_tensor = self.extract_lob_tensor(orderbook)?;
        
        let snapshot = LobTimeSeriesSnapshot {
            timestamp,
            basic_features,
            microstructure_features,
            order_flow_features,
            deltas,
            regime_indicators,
            lob_tensor,
        };
        
        Ok(snapshot)
    }
    
    /// Get DL sequence for trend prediction
    pub fn get_dl_sequence(&self) -> Result<LobTimeSeriesSequence> {
        let lookback_us = self.config.dl_lookback_seconds * 1_000_000;
        let current_time = now_micros();
        let start_time = current_time.saturating_sub(lookback_us);
        
        let relevant_snapshots: VecDeque<_> = self.snapshots
            .iter()
            .filter(|s| s.timestamp >= start_time)
            .cloned()
            .collect();
        
        if relevant_snapshots.is_empty() {
            return Err(anyhow::anyhow!("No snapshots available for DL sequence"));
        }
        
        Ok(LobTimeSeriesSequence {
            start_timestamp: start_time,
            end_timestamp: current_time,
            length: relevant_snapshots.len(),
            snapshots: relevant_snapshots,
            targets: None, // Will be set during training
        })
    }
    
    /// Get RL state for scalping
    pub fn get_rl_state(&self) -> Result<Vec<f64>> {
        let window_us = self.config.rl_state_window_ms * 1_000;
        let current_time = now_micros();
        let start_time = current_time.saturating_sub(window_us);
        
        // Get recent snapshots for RL state
        let recent_snapshots: Vec<_> = self.snapshots
            .iter()
            .filter(|s| s.timestamp >= start_time)
            .take(10) // Limit to last 10 snapshots for RL
            .collect();
        
        if recent_snapshots.is_empty() {
            return Err(anyhow::anyhow!("No recent snapshots for RL state"));
        }
        
        // Build compact state vector for RL
        let mut state = Vec::new();
        
        // Use the most recent snapshot as primary state
        let latest = recent_snapshots.last().unwrap();
        
        // Core microstructure features for RL (12 features)
        state.push(latest.basic_features.spread_bps / 100.0);        // Normalized spread
        state.push(latest.basic_features.obi_l5);                    // L5 OBI
        state.push(latest.basic_features.obi_l10);                   // L10 OBI
        state.push(latest.microstructure_features.microprice / latest.basic_features.mid_price); // Normalized microprice
        state.push(latest.microstructure_features.effective_spread); // Effective spread
        state.push(latest.order_flow_features.order_flow_imbalance); // Order flow imbalance
        state.push(latest.order_flow_features.trade_intensity.min(10.0) / 10.0); // Normalized trade intensity
        state.push(latest.deltas.price_delta_bps / 10.0);           // Recent price movement
        state.push(latest.regime_indicators.volatility_regime);      // Volatility regime
        state.push(latest.regime_indicators.momentum_regime);        // Momentum regime
        
        // LOB shape features (top 5 levels) - 10 features
        let tensor_subset = &latest.lob_tensor[0..10]; // First 5 bids + first 5 asks
        state.extend(tensor_subset.iter().cloned());
        
        // Momentum features from recent history (3 features)
        if recent_snapshots.len() >= 3 {
            let price_momentum = self.calculate_recent_momentum(&recent_snapshots, |s| s.basic_features.mid_price);
            let volume_momentum = self.calculate_recent_momentum(&recent_snapshots, |s| s.basic_features.depth_ratio_l5);
            let obi_momentum = self.calculate_recent_momentum(&recent_snapshots, |s| s.basic_features.obi_l10);
            
            state.push(price_momentum);
            state.push(volume_momentum);
            state.push(obi_momentum);
        } else {
            state.extend(vec![0.0; 3]);
        }
        
        // Total: 12 + 10 + 3 = 25 features for RL state
        Ok(state)
    }
    
    /// Calculate recent momentum for a feature
    fn calculate_recent_momentum<F>(&self, snapshots: &[&LobTimeSeriesSnapshot], extractor: F) -> f64
    where
        F: Fn(&LobTimeSeriesSnapshot) -> f64,
    {
        if snapshots.len() < 2 {
            return 0.0;
        }
        
        let values: Vec<f64> = snapshots.iter().map(|s| extractor(s)).collect();
        let first = values[0];
        let last = values[values.len() - 1];
        
        if first != 0.0 {
            (last - first) / first
        } else {
            0.0
        }
    }
    
    /// Add snapshot to buffer with size management
    fn add_snapshot(&mut self, snapshot: LobTimeSeriesSnapshot) {
        self.snapshots.push_back(snapshot.clone());
        
        // Keep buffer size reasonable
        let max_buffer_size = self.config.max_sequence_length * 3;
        while self.snapshots.len() > max_buffer_size {
            self.snapshots.pop_front();
        }
        
        // Update previous snapshot
        self.prev_snapshot = Some(snapshot);
        self.extraction_count += 1;
        self.last_extraction = now_micros();
    }
    
    /// Calculate deltas from previous snapshot
    fn calculate_deltas(
        &self,
        basic: &LobBasicFeatures,
        micro: &LobMicrostructureFeatures,
        prev: &LobTimeSeriesSnapshot,
    ) -> LobDeltas {
        LobDeltas {
            price_delta_bps: ((basic.mid_price - prev.basic_features.mid_price) / prev.basic_features.mid_price) * 10000.0,
            volume_delta_pct: ((basic.depth_ratio_l5 - prev.basic_features.depth_ratio_l5) / prev.basic_features.depth_ratio_l5.max(0.001)) * 100.0,
            spread_delta_bps: basic.spread_bps - prev.basic_features.spread_bps,
            obi_delta: basic.obi_l10 - prev.basic_features.obi_l10,
            depth_delta_pct: ((basic.depth_ratio_l10 - prev.basic_features.depth_ratio_l10) / prev.basic_features.depth_ratio_l10.max(0.001)) * 100.0,
            microprice_delta_bps: ((micro.microprice - prev.microstructure_features.microprice) / prev.microstructure_features.microprice.max(0.001)) * 10000.0,
        }
    }
    
    /// Calculate market regime indicators
    fn calculate_regime_indicators(
        &self,
        orderbook: &OrderBook,
        basic: &LobBasicFeatures,
    ) -> LobRegimeIndicators {
        // Use recent history to determine regimes
        let recent_count = 10;
        let recent_snapshots: Vec<_> = self.snapshots.iter().rev().take(recent_count).collect();
        
        let volatility_regime = if recent_snapshots.len() >= 5 {
            let price_changes: Vec<f64> = recent_snapshots.windows(2)
                .map(|w| (w[0].basic_features.mid_price - w[1].basic_features.mid_price).abs())
                .collect();
            let avg_change = price_changes.iter().sum::<f64>() / price_changes.len() as f64;
            (avg_change / basic.mid_price * 10000.0).min(1.0) // Normalize to [0,1]
        } else { 0.5 };
        
        let liquidity_regime = basic.spread_bps.max(1.0).recip().min(1.0); // Lower spread = higher liquidity
        
        let trending_regime = if recent_snapshots.len() >= 3 {
            let momentum = (basic.mid_price - recent_snapshots[2].basic_features.mid_price) / recent_snapshots[2].basic_features.mid_price;
            (momentum.abs() * 1000.0).min(1.0)
        } else { 0.5 };
        
        let stress_regime = (basic.spread_bps / 50.0).min(1.0); // High spread indicates stress
        
        let momentum_regime = if basic.obi_l10.abs() > 0.1 { 
            basic.obi_l10.abs() 
        } else { 0.0 };
        
        LobRegimeIndicators {
            volatility_regime,
            liquidity_regime,
            trending_regime,
            stress_regime,
            momentum_regime,
        }
    }
    
    /// Extract LOB tensor for L20 representation
    fn extract_lob_tensor(&self, orderbook: &OrderBook) -> Result<Vec<f64>> {
        let mid_price = orderbook.mid_price().unwrap_or(0.0.to_price()).0;
        if mid_price <= 0.0 {
            return Err(anyhow::anyhow!("Invalid mid price for tensor extraction"));
        }
        
        let mut tensor = vec![0.0; self.config.lob_depth * 2]; // L20 * 2 sides = 40
        
        // Extract bids (first L20)
        for (i, (price, qty)) in orderbook.bids.iter().rev().take(self.config.lob_depth).enumerate() {
            let log_qty = (qty.to_f64().unwrap_or(0.0) + 1.0).ln();
            tensor[i] = log_qty;
        }
        
        // Extract asks (next L20)
        for (i, (price, qty)) in orderbook.asks.iter().take(self.config.lob_depth).enumerate() {
            let log_qty = (qty.to_f64().unwrap_or(0.0) + 1.0).ln();
            tensor[self.config.lob_depth + i] = log_qty;
        }
        
        Ok(tensor)
    }
    
    /// Estimate fill rate for current orderbook
    fn estimate_fill_rate(&self, orderbook: &OrderBook) -> f64 {
        // Simple heuristic: higher depth = higher fill rate
        let total_depth = orderbook.calculate_depth(5, Side::Bid) + orderbook.calculate_depth(5, Side::Ask);
        (total_depth / 10000.0).min(1.0) // Normalize
    }
    
    /// Estimate aggressive order ratio
    fn estimate_aggressive_ratio(&self, orderbook: &OrderBook) -> f64 {
        // Use spread as proxy: wider spread = more aggressive orders needed
        let spread_bps = orderbook.spread_bps().unwrap_or(0.0);
        (spread_bps / 100.0).min(1.0) // Normalize
    }
    
    /// Update performance statistics
    fn update_statistics(&mut self, start_time: Timestamp) {
        let latency = now_micros() - start_time;
        
        if self.total_extractions == 0 {
            self.avg_extraction_latency_us = latency as f64;
        } else {
            let alpha = 0.1;
            self.avg_extraction_latency_us = alpha * latency as f64 + (1.0 - alpha) * self.avg_extraction_latency_us;
        }
        
        self.total_extractions += 1;
    }
    
    /// Cleanup memory if needed
    fn cleanup_memory_if_needed(&mut self) {
        let cleanup_interval_us = self.config.memory_cleanup_interval * 1_000_000;
        if now_micros() - self.last_memory_cleanup > cleanup_interval_us {
            // Trim snapshots buffer
            let target_size = self.config.max_sequence_length;
            while self.snapshots.len() > target_size {
                self.snapshots.pop_front();
            }
            
            // Trigger garbage collection for base extractor
            // (This is Rust, so memory is managed automatically, but we can reset some buffers)
            
            self.last_memory_cleanup = now_micros();
            debug!("Memory cleanup performed for {}", self.symbol);
        }
    }
    
    /// Get extractor statistics
    pub fn get_stats(&self) -> LobTimeSeriesExtractorStats {
        LobTimeSeriesExtractorStats {
            symbol: self.symbol.clone(),
            extraction_count: self.extraction_count,
            total_extractions: self.total_extractions,
            avg_extraction_latency_us: self.avg_extraction_latency_us,
            snapshots_buffered: self.snapshots.len(),
            memory_usage_bytes: self.estimate_memory_usage(),
            last_extraction: self.last_extraction,
        }
    }
    
    /// Estimate memory usage
    fn estimate_memory_usage(&self) -> usize {
        let snapshot_size = std::mem::size_of::<LobTimeSeriesSnapshot>() + 
                           40 * std::mem::size_of::<f64>(); // LOB tensor
        
        std::mem::size_of::<Self>() + 
        (self.snapshots.len() * snapshot_size) +
        self.base_extractor.get_stats().price_history_size * std::mem::size_of::<f64>()
    }
}

/// Extractor statistics
#[derive(Debug, Clone)]
pub struct LobTimeSeriesExtractorStats {
    pub symbol: String,
    pub extraction_count: u64,
    pub total_extractions: u64,
    pub avg_extraction_latency_us: f64,
    pub snapshots_buffered: usize,
    pub memory_usage_bytes: usize,
    pub last_extraction: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::orderbook::OrderBook;
    
    #[test]
    fn test_lob_time_series_extraction() {
        let config = LobTimeSeriesConfig::default();
        let mut extractor = LobTimeSeriesExtractor::new("BTCUSDT".to_string(), config);
        
        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // Initialize orderbook with test data
        let update = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![
                PriceLevel { price: 100.0.to_price(), quantity: "5.0".to_quantity(), side: Side::Bid },
                PriceLevel { price: 99.5.to_price(), quantity: "3.0".to_quantity(), side: Side::Bid },
            ],
            asks: vec![
                PriceLevel { price: 101.0.to_price(), quantity: "4.0".to_quantity(), side: Side::Ask },
                PriceLevel { price: 101.5.to_price(), quantity: "6.0".to_quantity(), side: Side::Ask },
            ],
            timestamp: now_micros(),
            sequence_start: 1,
            sequence_end: 1,
            is_snapshot: true,
        };
        
        orderbook.init_snapshot(update).unwrap();
        
        // Extract snapshot
        let snapshot = extractor.extract_snapshot(&orderbook, 10).unwrap();
        
        // Verify snapshot
        assert_eq!(snapshot.basic_features.mid_price, 100.5);
        assert!(snapshot.basic_features.spread_bps > 0.0);
        assert_eq!(snapshot.lob_tensor.len(), 40); // L20 * 2 sides
        
        // Test sequence generation
        extractor.add_snapshot(snapshot.clone());
        
        // Test RL state
        let rl_state = extractor.get_rl_state().unwrap();
        assert_eq!(rl_state.len(), 23); // Actual RL state dimension: 10 + 10 + 3
    }
    
    #[test]
    fn test_sequence_to_tensor() {
        let config = LobTimeSeriesConfig::default();
        let mut extractor = LobTimeSeriesExtractor::new("BTCUSDT".to_string(), config);
        
        // Create mock snapshots
        let mut snapshots = VecDeque::new();
        for i in 0..5 {
            let snapshot = create_mock_snapshot(100.0 + i as f64, now_micros() + i * 1000);
            snapshots.push_back(snapshot);
        }
        
        let sequence = LobTimeSeriesSequence {
            snapshots,
            start_timestamp: now_micros(),
            end_timestamp: now_micros() + 5000,
            length: 5,
            targets: None,
        };
        
        // Convert to feature matrix
        let matrix = sequence.to_feature_matrix().unwrap();
        
        // Verify matrix dimensions: [5, 76]
        assert_eq!(matrix.len(), 5);
        assert_eq!(matrix[0].len(), 76);
    }
    
    fn create_mock_snapshot(mid_price: f64, timestamp: Timestamp) -> LobTimeSeriesSnapshot {
        LobTimeSeriesSnapshot {
            timestamp,
            basic_features: LobBasicFeatures {
                mid_price,
                spread_bps: 10.0,
                bid_ask_ratio: 1.0,
                depth_ratio_l5: 1.0,
                depth_ratio_l10: 1.0,
                depth_ratio_l20: 1.0,
                obi_l5: 0.0,
                obi_l10: 0.0,
                obi_l20: 0.0,
            },
            microstructure_features: LobMicrostructureFeatures {
                microprice: mid_price,
                effective_spread: 0.01,
                realized_volatility: 0.02,
                bid_slope: 0.1,
                ask_slope: 0.1,
                price_impact: 0.05,
                liquidity_score: 0.8,
                order_arrival_intensity: 1.0,
            },
            order_flow_features: LobOrderFlowFeatures {
                order_flow_imbalance: 0.0,
                trade_intensity: 1.0,
                volume_acceleration: 0.0,
                depth_pressure_bid: 100.0,
                depth_pressure_ask: 100.0,
                cancellation_rate: 0.1,
                fill_rate_estimate: 0.8,
                aggressive_order_ratio: 0.2,
            },
            deltas: LobDeltas {
                price_delta_bps: 0.0,
                volume_delta_pct: 0.0,
                spread_delta_bps: 0.0,
                obi_delta: 0.0,
                depth_delta_pct: 0.0,
                microprice_delta_bps: 0.0,
            },
            regime_indicators: LobRegimeIndicators {
                volatility_regime: 0.5,
                liquidity_regime: 0.8,
                trending_regime: 0.3,
                stress_regime: 0.2,
                momentum_regime: 0.1,
            },
            lob_tensor: vec![0.0; 40], // L20 * 2
        }
    }
}