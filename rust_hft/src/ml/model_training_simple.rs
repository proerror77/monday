/*!
 * Simplified ML Models for Initial Implementation
 */

use crate::types::*;
use anyhow::Result;
use tracing::info;

/// Simplified ML predictor using basic rules
#[derive(Debug, Clone)]
pub struct SimplifiedPredictor {
    /// Feature weights learned from data
    pub weights: Vec<f64>,
    /// Prediction confidence threshold
    pub confidence_threshold: f64,
}

impl Default for SimplifiedPredictor {
    fn default() -> Self {
        Self::new()
    }
}

impl SimplifiedPredictor {
    pub fn new() -> Self {
        // Simple initial weights for key features
        let weights = vec![
            0.3,  // OBI L1
            0.25, // OBI L5
            0.2,  // OBI L10
            0.15, // Spread BPS
            0.1,  // Price momentum
        ];
        
        Self {
            weights,
            confidence_threshold: 0.6,
        }
    }
    
    /// Make prediction based on features
    pub fn predict(&self, features: &FeatureSet) -> Result<f64> {
        let feature_values = [
            features.obi_l1,
            features.obi_l5,
            features.obi_l10,
            features.spread_bps / 10000.0, // Normalize
            features.price_momentum,
        ];
        
        let prediction: f64 = feature_values.iter()
            .zip(&self.weights)
            .map(|(feature, weight)| feature * weight)
            .sum();
        
        Ok(prediction.tanh()) // Squash to [-1, 1]
    }
    
    /// Update weights based on new data (simple online learning)
    pub fn update_weights(&mut self, features: &FeatureSet, target: f64, learning_rate: f64) -> Result<()> {
        let prediction = self.predict(features)?;
        let error = target - prediction;
        
        let feature_values = [
            features.obi_l1,
            features.obi_l5,
            features.obi_l10,
            features.spread_bps / 10000.0,
            features.price_momentum,
        ];
        
        // Simple gradient descent
        for (i, &feature_val) in feature_values.iter().enumerate() {
            if i < self.weights.len() {
                self.weights[i] += learning_rate * error * feature_val;
            }
        }
        
        Ok(())
    }
}

/// Generate synthetic training data for testing
pub fn generate_synthetic_data(num_samples: usize, sequence_length: usize) -> Result<TrainingData> {
    info!("Generating {} synthetic samples with sequence length {}", num_samples, sequence_length);
    
    let mut features = Vec::new();
    let mut labels = Vec::new();
    let mut rng = rand::rng();
    
    use rand::Rng;
    
    for _i in 0..num_samples {
        let timestamp = now_micros();
        
        // Generate realistic market features
        let base_price = 50000.0 + rng.random_range(-5000.0..5000.0);
        let spread_bps = rng.random_range(1.0..50.0);
        
        let mut feature_set = FeatureSet::default_enhanced();
        feature_set.timestamp = timestamp;
        feature_set.latency_network_us = rng.random_range(10..100);
        feature_set.latency_processing_us = rng.random_range(5..50);
        feature_set.best_bid = (base_price - spread_bps/2.0).to_price();
        feature_set.best_ask = (base_price + spread_bps/2.0).to_price();
        feature_set.mid_price = base_price.to_price();
        feature_set.spread = spread_bps;
        feature_set.spread_bps = spread_bps;
        feature_set.obi_l1 = rng.random_range(-0.5..0.5);
        feature_set.obi_l5 = rng.random_range(-0.3..0.3);
        feature_set.obi_l10 = rng.random_range(-0.2..0.2);
        feature_set.obi_l20 = rng.random_range(-0.1..0.1);
        feature_set.bid_depth_l5 = rng.random_range(1000.0..10000.0);
        feature_set.ask_depth_l5 = rng.random_range(1000.0..10000.0);
        feature_set.bid_depth_l10 = rng.random_range(2000.0..20000.0);
        feature_set.ask_depth_l10 = rng.random_range(2000.0..20000.0);
        feature_set.bid_depth_l20 = rng.random_range(4000.0..40000.0);
        feature_set.ask_depth_l20 = rng.random_range(4000.0..40000.0);
        feature_set.depth_imbalance_l5 = rng.random_range(-0.2..0.2);
        feature_set.depth_imbalance_l10 = rng.random_range(-0.15..0.15);
        feature_set.depth_imbalance_l20 = rng.random_range(-0.1..0.1);
        feature_set.bid_slope = rng.random_range(0.05..0.5);
        feature_set.ask_slope = rng.random_range(0.05..0.5);
        feature_set.total_bid_levels = rng.random_range(5..25);
        feature_set.total_ask_levels = rng.random_range(5..25);
        feature_set.price_momentum = rng.random_range(-0.01..0.01);
        feature_set.volume_imbalance = rng.random_range(-0.3..0.3);
        feature_set.is_valid = true;
        feature_set.data_quality_score = rng.random_range(0.8..1.0);
        
        // Add some enhanced features for more realism
        feature_set.microprice = base_price + rng.random_range(-1.0..1.0);
        feature_set.vwap = base_price + rng.random_range(-0.5..0.5);
        feature_set.realized_volatility = rng.random_range(0.01..0.1);
        feature_set.effective_spread = feature_set.spread_bps * rng.random_range(0.8..1.2) / 10000.0;
        feature_set.trade_intensity = rng.random_range(1.0..20.0);
        feature_set.liquidity_score = rng.random_range(0.3..1.0);
        
        // Generate synthetic label (price change prediction)
        let price_change = rng.random_range(-0.01..0.01);
        
        features.push(feature_set);
        labels.push(price_change);
    }
    
    Ok(TrainingData {
        features,
        labels,
    })
}

#[derive(Debug, Clone)]
pub struct TrainingData {
    pub features: Vec<FeatureSet>,
    pub labels: Vec<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_simplified_predictor() {
        let mut predictor = SimplifiedPredictor::new();
        
        // Create test features
        let mut features = FeatureSet::default_enhanced();
        features.timestamp = now_micros();
        features.latency_network_us = 10;
        features.latency_processing_us = 20;
        features.best_bid = 100.0.to_price();
        features.best_ask = 101.0.to_price();
        features.mid_price = 100.5.to_price();
        features.spread = 1.0;
        features.spread_bps = 100.0;
        features.obi_l1 = 0.1;
        features.obi_l5 = 0.05;
        features.obi_l10 = 0.02;
        features.obi_l20 = 0.01;
        features.bid_depth_l5 = 1000.0;
        features.ask_depth_l5 = 1100.0;
        features.bid_depth_l10 = 2000.0;
        features.ask_depth_l10 = 2200.0;
        features.bid_depth_l20 = 4000.0;
        features.ask_depth_l20 = 4400.0;
        features.depth_imbalance_l5 = -0.05;
        features.depth_imbalance_l10 = -0.05;
        features.depth_imbalance_l20 = -0.05;
        features.bid_slope = 0.1;
        features.ask_slope = 0.1;
        features.total_bid_levels = 10;
        features.total_ask_levels = 12;
        features.price_momentum = 0.001;
        features.volume_imbalance = 0.05;
        features.is_valid = true;
        features.data_quality_score = 0.95;
        
        let prediction = predictor.predict(&features).unwrap();
        assert!(prediction >= -1.0 && prediction <= 1.0);
        
        // Test weight update
        predictor.update_weights(&features, 0.5, 0.01).unwrap();
    }
    
    #[test]
    fn test_synthetic_data_generation() {
        let data = generate_synthetic_data(10, 5).unwrap();
        assert_eq!(data.features.len(), 10);
        assert_eq!(data.labels.len(), 10);
        
        for feature in &data.features {
            assert!(feature.is_valid);
            assert!(feature.data_quality_score > 0.0);
        }
    }
}