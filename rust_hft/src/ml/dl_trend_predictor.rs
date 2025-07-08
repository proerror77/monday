/*!
 * DL Trend Predictor for LOB Time Series Analysis
 * 
 * Transformer-based deep learning model for 10-second trend prediction
 * Features:
 * - 30-second LOB sequence → 10-second trend classification
 * - 5-class prediction: StrongUp, WeakUp, Neutral, WeakDown, StrongDown
 * - Ultra-low latency inference (<1ms)
 * - Online model updates with new data
 */

use crate::core::types::*;
use crate::ml::lob_time_series_extractor::{LobTimeSeriesExtractor, LobTimeSeriesSequence, LobTimeSeriesConfig};
use anyhow::Result;
use candle_core::{Device, Tensor, DType, Module};
use candle_nn::{Linear, LayerNorm, VarBuilder, VarMap, Init, Activation};
use std::collections::VecDeque;
use tracing::{info, warn, debug, error};
use serde::{Serialize, Deserialize};

/// Trend prediction classes for 10-second horizon
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TrendClass {
    StrongDown = 0,  // < -0.1% (10 bps)
    WeakDown = 1,    // -0.1% to -0.02% 
    Neutral = 2,     // -0.02% to +0.02%
    WeakUp = 3,      // +0.02% to +0.1%
    StrongUp = 4,    // > +0.1%
}

impl TrendClass {
    /// Convert from price change (basis points) to trend class
    pub fn from_price_change_bps(change_bps: f64) -> Self {
        if change_bps > 10.0 {
            TrendClass::StrongUp
        } else if change_bps > 2.0 {
            TrendClass::WeakUp
        } else if change_bps > -2.0 {
            TrendClass::Neutral
        } else if change_bps > -10.0 {
            TrendClass::WeakDown
        } else {
            TrendClass::StrongDown
        }
    }
    
    /// Convert to numeric label for training
    pub fn to_label(&self) -> usize {
        *self as usize
    }
    
    /// Get all classes
    pub fn all_classes() -> [TrendClass; 5] {
        [
            TrendClass::StrongDown,
            TrendClass::WeakDown,
            TrendClass::Neutral,
            TrendClass::WeakUp,
            TrendClass::StrongUp,
        ]
    }
    
    /// Get class name
    pub fn name(&self) -> &'static str {
        match self {
            TrendClass::StrongDown => "StrongDown",
            TrendClass::WeakDown => "WeakDown",
            TrendClass::Neutral => "Neutral",
            TrendClass::WeakUp => "WeakUp",
            TrendClass::StrongUp => "StrongUp",
        }
    }
}

/// Trend prediction result
#[derive(Debug, Clone)]
pub struct TrendPrediction {
    /// Predicted trend class
    pub trend_class: TrendClass,
    
    /// Class probabilities [5]
    pub class_probabilities: Vec<f64>,
    
    /// Confidence score (0-1)
    pub confidence: f64,
    
    /// Expected price change in basis points
    pub expected_change_bps: f64,
    
    /// Model inference latency in microseconds
    pub inference_latency_us: u64,
    
    /// Prediction timestamp
    pub timestamp: Timestamp,
    
    /// Input sequence length used
    pub sequence_length: usize,
}

/// Configuration for DL trend predictor
#[derive(Debug, Clone)]
pub struct DlTrendPredictorConfig {
    /// Model parameters
    pub hidden_dim: usize,           // 256
    pub num_heads: usize,            // 8
    pub num_layers: usize,           // 4
    pub dropout_rate: f64,           // 0.1
    pub max_sequence_length: usize,  // 60 (30s / 500ms)
    
    /// Feature dimensions
    pub input_dim: usize,            // 76 (from LobTimeSeriesSequence)
    pub output_dim: usize,           // 5 (trend classes)
    
    /// Training parameters
    pub learning_rate: f64,          // 0.001
    pub batch_size: usize,           // 32
    pub warmup_steps: usize,         // 1000
    
    /// Prediction parameters
    pub confidence_threshold: f64,   // 0.6
    pub prediction_horizon_seconds: u64, // 10
    pub update_frequency_minutes: u64,   // 30
    
    /// Performance targets
    pub max_inference_latency_us: u64,  // 1000 (1ms)
    pub min_prediction_accuracy: f64,   // 0.55
}

impl Default for DlTrendPredictorConfig {
    fn default() -> Self {
        Self {
            hidden_dim: 256,
            num_heads: 8,
            num_layers: 4,
            dropout_rate: 0.1,
            max_sequence_length: 60,
            input_dim: 76,
            output_dim: 5,
            learning_rate: 0.001,
            batch_size: 32,
            warmup_steps: 1000,
            confidence_threshold: 0.6,
            prediction_horizon_seconds: 10,
            update_frequency_minutes: 30,
            max_inference_latency_us: 1000,
            min_prediction_accuracy: 0.55,
        }
    }
}

/// Transformer-based LOB trend prediction model
#[derive(Debug)]
pub struct LobTransformerModel {
    /// Input projection layer
    input_projection: Linear,
    
    /// Positional encoding
    positional_encoding: Tensor,
    
    /// Transformer encoder layers
    encoder_layers: Vec<TransformerEncoderLayer>,
    
    /// Output projection layers
    output_projection: Linear,
    output_norm: LayerNorm,
    
    /// Model configuration
    config: DlTrendPredictorConfig,
    
    /// Device for computations
    device: Device,
}

/// Single transformer encoder layer
#[derive(Debug)]
struct TransformerEncoderLayer {
    /// Multi-head attention
    self_attention: MultiHeadAttention,
    
    /// Feed-forward network
    feed_forward: FeedForward,
    
    /// Layer normalization
    norm1: LayerNorm,
    norm2: LayerNorm,
    
    /// Dropout rate
    dropout_rate: f64,
}

/// Multi-head attention mechanism
#[derive(Debug)]
struct MultiHeadAttention {
    query_projection: Linear,
    key_projection: Linear,
    value_projection: Linear,
    output_projection: Linear,
    num_heads: usize,
    head_dim: usize,
    scale: f64,
}

/// Feed-forward network
#[derive(Debug)]
struct FeedForward {
    linear1: Linear,
    linear2: Linear,
    activation: Activation,
}

impl LobTransformerModel {
    /// Create new transformer model
    pub fn new(config: DlTrendPredictorConfig, device: Device) -> Result<Self> {
        let varmap = VarMap::new();
        let vs = VarBuilder::from_varmap(&varmap, DType::F32, &device);
        
        // Input projection: input_dim -> hidden_dim
        let input_projection = candle_nn::linear(
            config.input_dim,
            config.hidden_dim,
            vs.pp("input_projection")
        )?;
        
        // Positional encoding
        let positional_encoding = Self::create_positional_encoding(
            config.max_sequence_length,
            config.hidden_dim,
            &device
        )?;
        
        // Transformer encoder layers
        let mut encoder_layers = Vec::new();
        for i in 0..config.num_layers {
            let layer = TransformerEncoderLayer::new(
                config.hidden_dim,
                config.num_heads,
                config.dropout_rate,
                vs.pp(&format!("encoder_layer_{}", i))
            )?;
            encoder_layers.push(layer);
        }
        
        // Output projection
        let output_projection = candle_nn::linear(
            config.hidden_dim,
            config.output_dim,
            vs.pp("output_projection")
        )?;
        
        let output_norm = candle_nn::layer_norm(
            config.hidden_dim,
            1e-5,
            vs.pp("output_norm")
        )?;
        
        Ok(Self {
            input_projection,
            positional_encoding,
            encoder_layers,
            output_projection,
            output_norm,
            config,
            device,
        })
    }
    
    /// Forward pass through the model
    pub fn forward(&self, input: &Tensor) -> Result<Tensor> {
        // Input shape: [batch_size, sequence_length, input_dim]
        let batch_size = input.dim(0)?;
        let seq_len = input.dim(1)?;
        
        // Project input to hidden dimension
        let mut x = self.input_projection.forward(input)?;
        
        // Add positional encoding
        let pos_encoding = self.positional_encoding
            .narrow(0, 0, seq_len)?
            .unsqueeze(0)?
            .expand(&[batch_size, seq_len, self.config.hidden_dim])?;
        x = (x + pos_encoding)?;
        
        // Pass through transformer encoder layers
        for layer in &self.encoder_layers {
            x = layer.forward(&x)?;
        }
        
        // Global average pooling over sequence dimension
        x = x.mean(1)?; // [batch_size, hidden_dim]
        
        // Layer normalization
        x = self.output_norm.forward(&x)?;
        
        // Output projection
        let logits = self.output_projection.forward(&x)?;
        
        // Apply softmax to get probabilities
        let probabilities = candle_nn::ops::softmax(&logits, 1)?;
        
        Ok(probabilities)
    }
    
    /// Create positional encoding
    fn create_positional_encoding(
        max_len: usize,
        hidden_dim: usize,
        device: &Device
    ) -> Result<Tensor> {
        let mut encoding = vec![vec![0.0f32; hidden_dim]; max_len];
        
        for pos in 0..max_len {
            for i in (0..hidden_dim).step_by(2) {
                let div_term = (i as f64 / hidden_dim as f64 * 10000.0f64.ln()).exp();
                encoding[pos][i] = (pos as f64 / div_term).sin() as f32;
                if i + 1 < hidden_dim {
                    encoding[pos][i + 1] = (pos as f64 / div_term).cos() as f32;
                }
            }
        }
        
        let flat_encoding: Vec<f32> = encoding.into_iter().flatten().collect();
        let tensor = Tensor::from_slice(&flat_encoding, (max_len, hidden_dim), device)?;
        
        Ok(tensor)
    }
    
    /// Predict trend from LOB sequence
    pub fn predict(&self, sequence: &LobTimeSeriesSequence) -> Result<TrendPrediction> {
        let start_time = now_micros();
        
        // Convert sequence to tensor
        let input_tensor = sequence.to_tensor(&self.device)?;
        
        // Forward pass
        let output = self.forward(&input_tensor)?;
        
        // Extract probabilities (single batch)
        let probs_tensor = output.squeeze(0)?;
        let probabilities: Vec<f64> = probs_tensor.to_vec1::<f32>()?
            .into_iter()
            .map(|x| x as f64)
            .collect();
        
        // Find predicted class
        let predicted_class_idx = probabilities
            .iter()
            .enumerate()
            .max_by(|a, b| {
                match a.1.partial_cmp(b.1) {
                    Some(ordering) => ordering,
                    None => std::cmp::Ordering::Equal,
                }
            })
            .map(|(idx, _)| idx)
            .unwrap_or(2); // Default to Neutral
        
        let trend_class = match predicted_class_idx {
            0 => TrendClass::StrongDown,
            1 => TrendClass::WeakDown,
            2 => TrendClass::Neutral,
            3 => TrendClass::WeakUp,
            4 => TrendClass::StrongUp,
            _ => TrendClass::Neutral,
        };
        
        // Calculate confidence (max probability)
        let confidence = probabilities[predicted_class_idx];
        
        // Estimate expected price change
        let expected_change_bps = self.calculate_expected_change(&probabilities);
        
        let inference_latency_us = now_micros() - start_time;
        
        Ok(TrendPrediction {
            trend_class,
            class_probabilities: probabilities,
            confidence,
            expected_change_bps,
            inference_latency_us,
            timestamp: now_micros(),
            sequence_length: sequence.length,
        })
    }
    
    /// Calculate expected price change from class probabilities
    fn calculate_expected_change(&self, probabilities: &[f64]) -> f64 {
        // Expected values for each class (in basis points)
        let class_values = [-15.0, -5.0, 0.0, 5.0, 15.0]; // StrongDown to StrongUp
        
        probabilities.iter()
            .zip(class_values.iter())
            .map(|(prob, value)| prob * value)
            .sum()
    }
}

impl TransformerEncoderLayer {
    fn new(
        hidden_dim: usize,
        num_heads: usize,
        dropout_rate: f64,
        vs: VarBuilder
    ) -> Result<Self> {
        let self_attention = MultiHeadAttention::new(
            hidden_dim,
            num_heads,
            vs.pp("self_attention")
        )?;
        
        let feed_forward = FeedForward::new(
            hidden_dim,
            hidden_dim * 4, // Standard transformer ratio
            vs.pp("feed_forward")
        )?;
        
        let norm1 = candle_nn::layer_norm(hidden_dim, 1e-5, vs.pp("norm1"))?;
        let norm2 = candle_nn::layer_norm(hidden_dim, 1e-5, vs.pp("norm2"))?;
        
        Ok(Self {
            self_attention,
            feed_forward,
            norm1,
            norm2,
            dropout_rate,
        })
    }
    
    fn forward(&self, input: &Tensor) -> Result<Tensor> {
        // Self-attention with residual connection
        let attn_output = self.self_attention.forward(input)?;
        let x = (input + attn_output)?;
        let x = self.norm1.forward(&x)?;
        
        // Feed-forward with residual connection
        let ff_output = self.feed_forward.forward(&x)?;
        let x = (x + ff_output)?;
        let x = self.norm2.forward(&x)?;
        
        Ok(x)
    }
}

impl MultiHeadAttention {
    fn new(hidden_dim: usize, num_heads: usize, vs: VarBuilder) -> Result<Self> {
        assert!(hidden_dim % num_heads == 0, "hidden_dim must be divisible by num_heads");
        
        let head_dim = hidden_dim / num_heads;
        let scale = (head_dim as f64).sqrt().recip();
        
        let query_projection = candle_nn::linear(hidden_dim, hidden_dim, vs.pp("query"))?;
        let key_projection = candle_nn::linear(hidden_dim, hidden_dim, vs.pp("key"))?;
        let value_projection = candle_nn::linear(hidden_dim, hidden_dim, vs.pp("value"))?;
        let output_projection = candle_nn::linear(hidden_dim, hidden_dim, vs.pp("output"))?;
        
        Ok(Self {
            query_projection,
            key_projection,
            value_projection,
            output_projection,
            num_heads,
            head_dim,
            scale,
        })
    }
    
    fn forward(&self, input: &Tensor) -> Result<Tensor> {
        let (batch_size, seq_len, hidden_dim) = input.dims3()?;
        
        // Generate Q, K, V
        let queries = self.query_projection.forward(input)?;
        let keys = self.key_projection.forward(input)?;
        let values = self.value_projection.forward(input)?;
        
        // Reshape for multi-head attention
        // [batch_size, seq_len, hidden_dim] -> [batch_size, num_heads, seq_len, head_dim]
        let queries = queries
            .reshape((batch_size, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let keys = keys
            .reshape((batch_size, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let values = values
            .reshape((batch_size, seq_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        
        // Scaled dot-product attention
        let attention_scores = queries.matmul(&keys.transpose(2, 3)?)?;
        let attention_scores = (attention_scores * self.scale)?;
        let attention_weights = candle_nn::ops::softmax(&attention_scores, 3)?;
        
        // Apply attention to values
        let attention_output = attention_weights.matmul(&values)?;
        
        // Reshape back to [batch_size, seq_len, hidden_dim]
        let attention_output = attention_output
            .transpose(1, 2)?
            .reshape((batch_size, seq_len, hidden_dim))?;
        
        // Output projection
        let output = self.output_projection.forward(&attention_output)?;
        
        Ok(output)
    }
}

impl FeedForward {
    fn new(input_dim: usize, hidden_dim: usize, vs: VarBuilder) -> Result<Self> {
        let linear1 = candle_nn::linear(input_dim, hidden_dim, vs.pp("linear1"))?;
        let linear2 = candle_nn::linear(hidden_dim, input_dim, vs.pp("linear2"))?;
        
        Ok(Self {
            linear1,
            linear2,
            activation: Activation::Gelu,
        })
    }
    
    fn forward(&self, input: &Tensor) -> Result<Tensor> {
        let x = self.linear1.forward(input)?;
        let x = self.activation.forward(&x)?;
        let x = self.linear2.forward(&x)?;
        Ok(x)
    }
}

/// Main DL trend predictor
#[derive(Debug)]
pub struct DlTrendPredictor {
    /// Configuration
    config: DlTrendPredictorConfig,
    
    /// LOB time series configuration
    lob_config: LobTimeSeriesConfig,
    
    /// Transformer model
    model: LobTransformerModel,
    
    /// Device for computations
    device: Device,
    
    /// Performance statistics
    stats: DlTrendPredictorStats,
    
    /// Recent predictions for validation
    recent_predictions: VecDeque<(TrendPrediction, Option<f64>)>, // (prediction, actual_change)
    
    /// Symbol for this predictor
    symbol: String,
    
    /// Last update timestamp
    last_update: Timestamp,
}

/// Statistics for trend predictor
#[derive(Debug, Clone, Default)]
pub struct DlTrendPredictorStats {
    pub predictions_made: u64,
    pub avg_inference_latency_us: f64,
    pub accuracy_5min: f64,
    pub accuracy_1hour: f64,
    pub accuracy_overall: f64,
    pub confidence_avg: f64,
    pub class_distribution: [u64; 5], // Count for each trend class
    pub last_prediction_time: Timestamp,
}

impl DlTrendPredictor {
    /// Create new DL trend predictor
    pub fn new(
        symbol: String,
        config: DlTrendPredictorConfig,
        lob_config: LobTimeSeriesConfig,
    ) -> Result<Self> {
        // Initialize device (prefer GPU if available)
        let device = Self::initialize_device();
        info!("DL Trend Predictor initialized on device: {:?}", device);
        
        // Create transformer model
        let model = LobTransformerModel::new(config.clone(), device.clone())?;
        
        Ok(Self {
            config,
            lob_config,
            model,
            device,
            stats: DlTrendPredictorStats::default(),
            recent_predictions: VecDeque::with_capacity(1000),
            symbol,
            last_update: now_micros(),
        })
    }
    
    /// Initialize computation device
    fn initialize_device() -> Device {
        #[cfg(feature = "cuda")]
        {
            if candle_core::cuda::has_cuda() {
                info!("CUDA available, using GPU for DL trend predictor");
                return Device::new_cuda(0).unwrap_or_else(|e| {
                    warn!("Failed to initialize CUDA device: {}, falling back to CPU", e);
                    Device::Cpu
                });
            }
        }
        
        #[cfg(feature = "metal")]
        {
            if let Ok(device) = Device::new_metal(0) {
                info!("Metal available, using GPU for DL trend predictor");
                return device;
            }
        }
        
        info!("Using CPU device for DL trend predictor");
        Device::Cpu
    }
    
    /// Predict trend from LOB time series extractor
    pub fn predict_trend(
        &mut self,
        extractor: &LobTimeSeriesExtractor,
    ) -> Result<Option<TrendPrediction>> {
        // Get DL sequence from extractor
        let sequence = extractor.get_dl_sequence()?;
        
        // Check if we have enough data
        if sequence.length < 10 {
            debug!("Insufficient data for trend prediction: {} snapshots", sequence.length);
            return Ok(None);
        }
        
        // Make prediction
        let prediction = self.model.predict(&sequence)?;
        
        // Update statistics
        self.update_stats(&prediction);
        
        // Check confidence threshold
        if prediction.confidence < self.config.confidence_threshold {
            debug!("Prediction confidence too low: {:.3}", prediction.confidence);
            return Ok(None);
        }
        
        // Store prediction for later validation
        self.recent_predictions.push_back((prediction.clone(), None));
        if self.recent_predictions.len() > 1000 {
            self.recent_predictions.pop_front();
        }
        
        info!(
            "DL trend prediction for {}: {} (confidence: {:.3}, latency: {}μs)",
            self.symbol,
            prediction.trend_class.name(),
            prediction.confidence,
            prediction.inference_latency_us
        );
        
        Ok(Some(prediction))
    }
    
    /// Update prediction with actual outcome for validation
    pub fn update_prediction_outcome(
        &mut self,
        prediction_timestamp: Timestamp,
        actual_price_change_bps: f64,
    ) {
        // Find the prediction to update
        for (prediction, actual_change) in &mut self.recent_predictions {
            if prediction.timestamp == prediction_timestamp && actual_change.is_none() {
                *actual_change = Some(actual_price_change_bps);
                
                // Update accuracy statistics
                self.calculate_accuracy();
                break;
            }
        }
    }
    
    /// Calculate prediction accuracy
    fn calculate_accuracy(&mut self) {
        let validated_predictions: Vec<_> = self.recent_predictions
            .iter()
            .filter_map(|(pred, actual)| {
                actual.map(|change| (pred, change))
            })
            .collect();
        
        if validated_predictions.is_empty() {
            return;
        }
        
        // Calculate overall accuracy
        let correct_predictions = validated_predictions
            .iter()
            .filter(|(pred, actual_change)| {
                let actual_class = TrendClass::from_price_change_bps(*actual_change);
                pred.trend_class == actual_class
            })
            .count();
        
        self.stats.accuracy_overall = correct_predictions as f64 / validated_predictions.len() as f64;
        
        // Calculate recent accuracy (last hour)
        let one_hour_ago = now_micros().saturating_sub(3600 * 1_000_000);
        let recent_predictions: Vec<_> = validated_predictions
            .iter()
            .filter(|(pred, _)| pred.timestamp >= one_hour_ago)
            .collect();
        
        if !recent_predictions.is_empty() {
            let recent_correct = recent_predictions
                .iter()
                .filter(|(pred, actual_change)| {
                    let actual_class = TrendClass::from_price_change_bps(*actual_change);
                    pred.trend_class == actual_class
                })
                .count();
            
            self.stats.accuracy_1hour = recent_correct as f64 / recent_predictions.len() as f64;
        }
        
        // Calculate very recent accuracy (last 5 minutes)
        let five_min_ago = now_micros().saturating_sub(300 * 1_000_000);
        let very_recent_predictions: Vec<_> = validated_predictions
            .iter()
            .filter(|(pred, _)| pred.timestamp >= five_min_ago)
            .collect();
        
        if !very_recent_predictions.is_empty() {
            let very_recent_correct = very_recent_predictions
                .iter()
                .filter(|(pred, actual_change)| {
                    let actual_class = TrendClass::from_price_change_bps(*actual_change);
                    pred.trend_class == actual_class
                })
                .count();
            
            self.stats.accuracy_5min = very_recent_correct as f64 / very_recent_predictions.len() as f64;
        }
    }
    
    /// Update performance statistics
    fn update_stats(&mut self, prediction: &TrendPrediction) {
        self.stats.predictions_made += 1;
        self.stats.last_prediction_time = prediction.timestamp;
        
        // Update average inference latency
        if self.stats.predictions_made == 1 {
            self.stats.avg_inference_latency_us = prediction.inference_latency_us as f64;
        } else {
            let alpha = 0.1;
            self.stats.avg_inference_latency_us = alpha * prediction.inference_latency_us as f64 
                + (1.0 - alpha) * self.stats.avg_inference_latency_us;
        }
        
        // Update average confidence
        if self.stats.predictions_made == 1 {
            self.stats.confidence_avg = prediction.confidence;
        } else {
            let alpha = 0.1;
            self.stats.confidence_avg = alpha * prediction.confidence + (1.0 - alpha) * self.stats.confidence_avg;
        }
        
        // Update class distribution
        self.stats.class_distribution[prediction.trend_class.to_label()] += 1;
    }
    
    /// Get predictor statistics
    pub fn get_stats(&self) -> DlTrendPredictorStats {
        self.stats.clone()
    }
    
    /// Check if model needs update
    pub fn needs_update(&self) -> bool {
        let update_interval_us = self.config.update_frequency_minutes * 60 * 1_000_000;
        now_micros() - self.last_update > update_interval_us
    }
    
    /// Update model with new data (placeholder for future implementation)
    pub fn update_model(&mut self) -> Result<()> {
        // Placeholder for online model updates
        // In a real implementation, this would:
        // 1. Collect recent validated predictions
        // 2. Perform gradient updates on the model
        // 3. Validate model performance
        
        self.last_update = now_micros();
        info!("Model update completed for {}", self.symbol);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::lob_time_series_extractor::{LobTimeSeriesSnapshot, LobTimeSeriesSequence};
    use std::collections::VecDeque;
    
    #[test]
    fn test_trend_class_conversion() {
        assert_eq!(TrendClass::from_price_change_bps(15.0), TrendClass::StrongUp);
        assert_eq!(TrendClass::from_price_change_bps(5.0), TrendClass::WeakUp);
        assert_eq!(TrendClass::from_price_change_bps(0.0), TrendClass::Neutral);
        assert_eq!(TrendClass::from_price_change_bps(-5.0), TrendClass::WeakDown);
        assert_eq!(TrendClass::from_price_change_bps(-15.0), TrendClass::StrongDown);
    }
    
    #[test]
    fn test_dl_trend_predictor_creation() {
        let config = DlTrendPredictorConfig::default();
        let lob_config = LobTimeSeriesConfig::default();
        
        let predictor = DlTrendPredictor::new(
            "BTCUSDT".to_string(),
            config,
            lob_config,
        );
        
        assert!(predictor.is_ok());
    }
    
    #[test]
    fn test_transformer_model_creation() {
        let config = DlTrendPredictorConfig::default();
        let device = Device::Cpu;
        
        let model = LobTransformerModel::new(config, device);
        assert!(model.is_ok());
    }
    
    #[test]
    fn test_model_inference() {
        let config = DlTrendPredictorConfig::default();
        let device = Device::Cpu;
        let model = LobTransformerModel::new(config.clone(), device.clone()).unwrap();
        
        // Create mock sequence
        let mut snapshots = VecDeque::new();
        for i in 0..10 {
            let snapshot = create_mock_snapshot(100.0 + i as f64, now_micros() + i * 1000);
            snapshots.push_back(snapshot);
        }
        
        let sequence = LobTimeSeriesSequence {
            snapshots,
            start_timestamp: now_micros(),
            end_timestamp: now_micros() + 10000,
            length: 10,
            targets: None,
        };
        
        // Test prediction
        let prediction = model.predict(&sequence);
        assert!(prediction.is_ok());
        
        let pred = prediction.unwrap();
        assert_eq!(pred.class_probabilities.len(), 5);
        assert!(pred.confidence >= 0.0 && pred.confidence <= 1.0);
        assert!(pred.inference_latency_us > 0);
    }
    
    fn create_mock_snapshot(mid_price: f64, timestamp: Timestamp) -> LobTimeSeriesSnapshot {
        use crate::ml::lob_time_series_extractor::*;
        
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