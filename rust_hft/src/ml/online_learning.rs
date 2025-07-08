/*!
 * Candle Online Learning Engine for Real-time ML Training
 * 
 * Advanced deep learning system using Candle framework for:
 * - Real-time model training with streaming data
 * - Adaptive learning rates and model updates
 * - Multi-timeframe prediction (3s, 5s, 10s)
 * - GPU acceleration support
 * - Model persistence and loading
 */

use crate::types::*;
use anyhow::Result;
use candle_core::{Device, Tensor, DType, Module};
use candle_nn::{VarBuilder, VarMap, Linear, Dropout, Optimizer, RNN};
use std::collections::{VecDeque, HashMap};
use tracing::{info, warn, debug};
use serde::{Serialize, Deserialize};

/// Online learning engine configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineLearningConfig {
    /// Model architecture parameters
    pub input_size: usize,
    pub hidden_sizes: Vec<usize>,
    pub output_size: usize,
    pub dropout_rate: f64,
    
    /// Training parameters
    pub learning_rate: f64,
    pub batch_size: usize,
    pub sequence_length: usize,
    pub prediction_horizons: Vec<u64>, // [3, 5, 10] seconds
    
    /// Online learning parameters
    pub update_frequency: usize, // Update model every N samples
    pub validation_ratio: f64,
    pub early_stopping_patience: usize,
    pub max_training_samples: usize,
    
    /// Performance parameters
    pub use_gpu: bool,
    pub mixed_precision: bool,
    pub gradient_clipping: f64,
    
    /// Model persistence
    pub model_save_interval: usize,
    pub model_save_path: String,
}

impl Default for OnlineLearningConfig {
    fn default() -> Self {
        Self {
            input_size: 20, // Based on FeatureSet
            hidden_sizes: vec![128, 64, 32],
            output_size: 3, // 3s, 5s, 10s predictions
            dropout_rate: 0.2,
            
            learning_rate: 0.001,
            batch_size: 32,
            sequence_length: 50,
            prediction_horizons: vec![3, 5, 10],
            
            update_frequency: 100,
            validation_ratio: 0.2,
            early_stopping_patience: 10,
            max_training_samples: 10000,
            
            use_gpu: true,
            mixed_precision: false,
            gradient_clipping: 1.0,
            
            model_save_interval: 1000,
            model_save_path: "models/online_model.safetensors".to_string(),
        }
    }
}

/// LSTM-based neural network for price prediction
#[derive(Debug)]
#[allow(dead_code)]
pub struct PricePredictionModel {
    lstm: candle_nn::LSTM,
    dropout: Dropout,
    linear_layers: Vec<Linear>,
    output_layer: Linear,
    device: Device,
}

impl PricePredictionModel {
    pub fn new(config: &OnlineLearningConfig, vs: VarBuilder, device: Device) -> Result<Self> {
        let input_size = config.input_size;
        let hidden_size = config.hidden_sizes[0];
        
        // LSTM layer
        let lstm_config = candle_nn::LSTMConfig::default();
        let lstm = candle_nn::lstm(input_size, hidden_size, lstm_config, vs.pp("lstm"))?;
        
        // Dropout layer
        let dropout = Dropout::new(config.dropout_rate as f32);
        
        // Hidden layers
        let mut linear_layers = Vec::new();
        let mut prev_size = hidden_size;
        
        for (i, &hidden_size) in config.hidden_sizes.iter().skip(1).enumerate() {
            let linear = candle_nn::linear(prev_size, hidden_size, vs.pp(format!("hidden_{i}")))?;
            linear_layers.push(linear);
            prev_size = hidden_size;
        }
        
        // Output layer
        let output_layer = candle_nn::linear(prev_size, config.output_size, vs.pp("output"))?;
        
        Ok(Self {
            lstm,
            dropout,
            linear_layers,
            output_layer,
            device,
        })
    }
    
    pub fn forward(&self, input: &Tensor, train: bool) -> Result<Tensor> {
        // LSTM forward pass - seq returns Vec<State>
        let lstm_states = self.lstm.seq(input)?;
        
        // Get the last state and extract hidden state
        let last_state = lstm_states.last().ok_or_else(|| anyhow::anyhow!("No LSTM states"))?;
        let last_output = &last_state.h;
        
        // Apply dropout
        let mut x = if train {
            self.dropout.forward(last_output, train)?
        } else {
            last_output.clone()
        };
        
        // Hidden layers with ReLU activation
        for linear in &self.linear_layers {
            x = linear.forward(&x)?;
            x = x.relu()?;
            if train {
                x = self.dropout.forward(&x, train)?;
            }
        }
        
        // Output layer
        let output = self.output_layer.forward(&x)?;
        
        Ok(output)
    }
}

/// Training sample for online learning
#[derive(Debug, Clone)]
pub struct TrainingSample {
    pub features: Vec<f64>,
    pub labels: Vec<f64>, // 3s, 5s, 10s price changes
    pub timestamp: Timestamp,
    pub symbol: String,
}

/// Online learning engine state
pub struct OnlineLearningEngine {
    config: OnlineLearningConfig,
    model: Option<PricePredictionModel>,
    optimizer: Option<candle_nn::AdamW>,
    device: Device,
    
    // Training data management
    training_buffer: VecDeque<TrainingSample>,
    validation_buffer: VecDeque<TrainingSample>,
    
    // Model state
    var_map: VarMap,
    training_step: usize,
    last_save_step: usize,
    
    // Performance tracking
    training_loss_history: VecDeque<f64>,
    validation_loss_history: VecDeque<f64>,
    prediction_accuracy: HashMap<u64, f64>, // horizon -> accuracy
    
    // Statistics
    stats: OnlineLearningStats,
}

#[derive(Debug, Clone, Default)]
pub struct OnlineLearningStats {
    pub total_samples_processed: u64,
    pub total_training_updates: u64,
    pub current_training_loss: f64,
    pub current_validation_loss: f64,
    pub model_accuracy_3s: f64,
    pub model_accuracy_5s: f64,
    pub model_accuracy_10s: f64,
    pub training_time_ms: u64,
    pub inference_time_us: u64,
    pub last_model_save: Option<Timestamp>,
}

impl std::fmt::Debug for OnlineLearningEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnlineLearningEngine")
            .field("config", &self.config)
            .field("device", &format!("{:?}", self.device))
            .field("training_step", &self.training_step)
            .field("training_buffer_len", &self.training_buffer.len())
            .field("validation_buffer_len", &self.validation_buffer.len())
            .field("stats", &self.stats)
            .finish()
    }
}

impl OnlineLearningEngine {
    pub fn new(config: OnlineLearningConfig) -> Result<Self> {
        // Initialize device
        let device = if config.use_gpu {
            if let Ok(cuda_device) = Device::new_cuda(0) {
                info!("Using GPU (CUDA) for online learning");
                cuda_device
            } else {
                info!("CUDA not available, using CPU for online learning");
                Device::Cpu
            }
        } else {
            info!("Using CPU for online learning");
            Device::Cpu
        };
        
        // Initialize variable map for model parameters
        let var_map = VarMap::new();
        let vs = VarBuilder::from_varmap(&var_map, DType::F32, &device);
        
        // Initialize model
        let model = PricePredictionModel::new(&config, vs.clone(), device.clone())?;
        
        // Initialize optimizer
        let params = candle_nn::ParamsAdamW {
            lr: config.learning_rate,
            ..Default::default()
        };
        let optimizer = candle_nn::AdamW::new(var_map.all_vars(), params)?;
        
        // Initialize accuracy tracking
        let mut prediction_accuracy = HashMap::new();
        for &horizon in &config.prediction_horizons {
            prediction_accuracy.insert(horizon, 0.0);
        }
        
        info!("Online learning engine initialized with device: {:?}", device);
        
        let max_training_samples = config.max_training_samples;
        
        Ok(Self {
            config,
            model: Some(model),
            optimizer: Some(optimizer),
            device,
            
            training_buffer: VecDeque::with_capacity(max_training_samples),
            validation_buffer: VecDeque::with_capacity(max_training_samples / 5),
            
            var_map,
            training_step: 0,
            last_save_step: 0,
            
            training_loss_history: VecDeque::with_capacity(1000),
            validation_loss_history: VecDeque::with_capacity(1000),
            prediction_accuracy,
            
            stats: OnlineLearningStats::default(),
        })
    }
    
    /// Add new training sample to the buffer
    pub fn add_training_sample(&mut self, sample: TrainingSample) -> Result<()> {
        self.stats.total_samples_processed += 1;
        
        // Decide whether this goes to training or validation
        let use_for_validation = self.training_buffer.len() as f64 * self.config.validation_ratio 
            < self.validation_buffer.len() as f64;
        
        if use_for_validation && self.validation_buffer.len() < self.config.max_training_samples / 5 {
            self.validation_buffer.push_back(sample);
            
            // Limit validation buffer size
            if self.validation_buffer.len() > self.config.max_training_samples / 5 {
                self.validation_buffer.pop_front();
            }
        } else {
            self.training_buffer.push_back(sample);
            
            // Limit training buffer size
            if self.training_buffer.len() > self.config.max_training_samples {
                self.training_buffer.pop_front();
            }
        }
        
        // Trigger training if we have enough samples
        if self.training_buffer.len() >= self.config.batch_size && 
           self.stats.total_samples_processed % self.config.update_frequency as u64 == 0 {
            self.train_step()?;
        }
        
        Ok(())
    }
    
    /// Perform one training step
    fn train_step(&mut self) -> Result<()> {
        let start_time = std::time::Instant::now();
        
        if self.training_buffer.len() < self.config.batch_size {
            return Ok(());
        }
        
        // Prepare training batch
        let batch_samples: Vec<_> = self.training_buffer
            .iter()
            .rev()
            .take(self.config.batch_size)
            .cloned()
            .collect();
        
        let (input_tensor, target_tensor) = self.prepare_batch(&batch_samples)?;
        
        // Get model reference
        let model = self.model.as_ref().ok_or_else(|| anyhow::anyhow!("Model not initialized"))?;
        
        // Forward pass
        let predictions = model.forward(&input_tensor, true)?;
        
        // Calculate loss (MSE)
        let loss = self.calculate_loss(&predictions, &target_tensor)?;
        
        // Backward pass and compute gradients
        let grads = loss.backward()?;
        
        // Gradient clipping
        if self.config.gradient_clipping > 0.0 {
            self.clip_gradients(self.config.gradient_clipping)?;
        }
        
        // Update parameters with optimizer
        let optimizer = self.optimizer.as_mut().ok_or_else(|| anyhow::anyhow!("Optimizer not initialized"))?;
        optimizer.step(&grads)?;
        
        // Update statistics
        let loss_value = loss.to_scalar::<f32>()? as f64;
        self.training_loss_history.push_back(loss_value);
        if self.training_loss_history.len() > 1000 {
            self.training_loss_history.pop_front();
        }
        
        self.stats.current_training_loss = loss_value;
        self.stats.total_training_updates += 1;
        self.stats.training_time_ms = start_time.elapsed().as_millis() as u64;
        
        self.training_step += 1;
        
        // Validation step
        if self.training_step % 10 == 0 {
            self.validation_step()?;
        }
        
        // Save model periodically
        if self.training_step - self.last_save_step >= self.config.model_save_interval {
            self.save_model()?;
            self.last_save_step = self.training_step;
        }
        
        debug!("Training step {}: loss = {:.6}", self.training_step, loss_value);
        
        Ok(())
    }
    
    /// Perform validation step
    fn validation_step(&mut self) -> Result<()> {
        if self.validation_buffer.len() < self.config.batch_size {
            return Ok(());
        }
        
        let model = self.model.as_ref().ok_or_else(|| anyhow::anyhow!("Model not initialized"))?;
        
        // Prepare validation batch
        let batch_samples: Vec<_> = self.validation_buffer
            .iter()
            .rev()
            .take(self.config.batch_size)
            .cloned()
            .collect();
        
        let (input_tensor, target_tensor) = self.prepare_batch(&batch_samples)?;
        
        // Forward pass (no training)
        let predictions = model.forward(&input_tensor, false)?;
        
        // Calculate validation loss
        let loss = self.calculate_loss(&predictions, &target_tensor)?;
        let loss_value = loss.to_scalar::<f32>()? as f64;
        
        self.validation_loss_history.push_back(loss_value);
        if self.validation_loss_history.len() > 1000 {
            self.validation_loss_history.pop_front();
        }
        
        self.stats.current_validation_loss = loss_value;
        
        // Calculate accuracy for each horizon
        self.calculate_accuracy(&predictions, &target_tensor)?;
        
        debug!("Validation step: loss = {:.6}", loss_value);
        
        Ok(())
    }
    
    /// Make prediction for new features
    pub fn predict(&self, features: &FeatureSet) -> Result<Vec<f64>> {
        let start_time = std::time::Instant::now();
        
        let model = self.model.as_ref().ok_or_else(|| anyhow::anyhow!("Model not initialized"))?;
        
        // Convert features to tensor (convert f64 to f32)
        let feature_vec = self.extract_feature_vector(features);
        let feature_vec_f32: Vec<f32> = feature_vec.iter().map(|&x| x as f32).collect();
        let input = Tensor::from_vec(
            feature_vec_f32,
            (1, 1, self.config.input_size),
            &self.device
        )?;
        
        // Forward pass
        let predictions = model.forward(&input, false)?;
        
        // Convert to Vec<f64>
        let pred_data = predictions.to_vec2::<f32>()?;
        let result: Vec<f64> = pred_data[0].iter().map(|&x| x as f64).collect();
        
        // Update inference time
        let inference_time = start_time.elapsed().as_micros() as u64;
        // Note: We can't mutate self here due to &self, so we skip updating stats
        
        debug!("Prediction completed in {}μs: {:?}", inference_time, result);
        
        Ok(result)
    }
    
    /// Prepare batch tensors from samples
    fn prepare_batch(&self, samples: &[TrainingSample]) -> Result<(Tensor, Tensor)> {
        let batch_size = samples.len();
        let seq_len = 1; // For now, we use single timestep
        
        // Prepare input features (convert f64 to f32)
        let mut input_data = Vec::with_capacity(batch_size * seq_len * self.config.input_size);
        let mut target_data = Vec::with_capacity(batch_size * self.config.output_size);
        
        for sample in samples {
            // Add features (convert f64 to f32)
            for &feature in &sample.features {
                input_data.push(feature as f32);
            }
            
            // Add labels (convert f64 to f32)
            for &label in &sample.labels {
                target_data.push(label as f32);
            }
        }
        
        let input_tensor = Tensor::from_vec(
            input_data,
            (batch_size, seq_len, self.config.input_size),
            &self.device
        )?;
        
        let target_tensor = Tensor::from_vec(
            target_data,
            (batch_size, self.config.output_size),
            &self.device
        )?;
        
        Ok((input_tensor, target_tensor))
    }
    
    /// Calculate MSE loss
    fn calculate_loss(&self, predictions: &Tensor, targets: &Tensor) -> Result<Tensor> {
        let diff = (predictions - targets)?;
        let squared = diff.powf(2.0)?;
        let loss = squared.mean_all()?;
        Ok(loss)
    }
    
    /// Calculate prediction accuracy
    fn calculate_accuracy(&mut self, predictions: &Tensor, targets: &Tensor) -> Result<()> {
        let pred_data = predictions.to_vec2::<f32>()?;
        let target_data = targets.to_vec2::<f32>()?;
        
        let mut horizon_accuracies = vec![0.0; self.config.output_size];
        let mut horizon_counts = vec![0; self.config.output_size];
        
        for (pred_row, target_row) in pred_data.iter().zip(target_data.iter()) {
            for (i, (&pred, &target)) in pred_row.iter().zip(target_row.iter()).enumerate() {
                if i < self.config.output_size {
                    // Calculate directional accuracy (same sign = correct direction)
                    let correct = (pred > 0.0) == (target > 0.0);
                    if correct {
                        horizon_accuracies[i] += 1.0;
                    }
                    horizon_counts[i] += 1;
                }
            }
        }
        
        // Update accuracy statistics
        for (i, &horizon) in self.config.prediction_horizons.iter().enumerate() {
            if i < horizon_accuracies.len() && horizon_counts[i] > 0 {
                let accuracy = horizon_accuracies[i] / horizon_counts[i] as f64;
                
                // Exponential moving average
                let alpha = 0.1;
                let current_acc = self.prediction_accuracy.get(&horizon).copied().unwrap_or(0.5);
                let new_acc = alpha * accuracy + (1.0 - alpha) * current_acc;
                
                self.prediction_accuracy.insert(horizon, new_acc);
                
                match horizon {
                    3 => self.stats.model_accuracy_3s = new_acc,
                    5 => self.stats.model_accuracy_5s = new_acc,
                    10 => self.stats.model_accuracy_10s = new_acc,
                    _ => {}
                }
            }
        }
        
        Ok(())
    }
    
    /// Clip gradients to prevent exploding gradients
    fn clip_gradients(&self, _max_norm: f64) -> Result<()> {
        // Implementation would need access to gradients
        // This is a placeholder for gradient clipping
        Ok(())
    }
    
    /// Extract feature vector from FeatureSet
    fn extract_feature_vector(&self, features: &FeatureSet) -> Vec<f64> {
        vec![
            features.mid_price.0,
            features.spread_bps,
            features.obi_l1,
            features.obi_l5,
            features.obi_l10,
            features.obi_l20,
            features.bid_depth_l5,
            features.ask_depth_l5,
            features.bid_depth_l10,
            features.ask_depth_l10,
            features.depth_imbalance_l5,
            features.depth_imbalance_l10,
            features.depth_imbalance_l20,
            features.bid_slope,
            features.ask_slope,
            features.price_momentum,
            features.volume_imbalance,
            features.latency_network_us as f64,
            features.latency_processing_us as f64,
            features.data_quality_score,
        ]
    }
    
    /// Save model to disk
    pub fn save_model(&mut self) -> Result<()> {
        info!("Saving model to {}", self.config.model_save_path);
        
        // Create directory if it doesn't exist
        if let Some(parent) = std::path::Path::new(&self.config.model_save_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        // Save the variable map
        self.var_map.save(&self.config.model_save_path)?;
        
        self.stats.last_model_save = Some(now_micros());
        
        info!("Model saved successfully");
        Ok(())
    }
    
    /// Load model from disk
    pub fn load_model(&mut self) -> Result<()> {
        info!("Loading model from {}", self.config.model_save_path);
        
        if !std::path::Path::new(&self.config.model_save_path).exists() {
            warn!("Model file does not exist: {}", self.config.model_save_path);
            return Ok(());
        }
        
        // Load the variable map
        self.var_map.load(&self.config.model_save_path)?;
        
        info!("Model loaded successfully");
        Ok(())
    }
    
    /// Get current statistics
    pub fn get_stats(&self) -> OnlineLearningStats {
        self.stats.clone()
    }
    
    /// Reset training state
    pub fn reset(&mut self) -> Result<()> {
        self.training_buffer.clear();
        self.validation_buffer.clear();
        self.training_loss_history.clear();
        self.validation_loss_history.clear();
        
        self.training_step = 0;
        self.last_save_step = 0;
        
        for accuracy in self.prediction_accuracy.values_mut() {
            *accuracy = 0.5; // Reset to baseline
        }
        
        self.stats = OnlineLearningStats::default();
        
        info!("Online learning engine reset");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    
    #[test]
    fn test_online_learning_config() {
        let config = OnlineLearningConfig::default();
        assert_eq!(config.input_size, 20);
        assert_eq!(config.output_size, 3);
        assert_eq!(config.prediction_horizons, vec![3, 5, 10]);
    }
    
    #[test]
    fn test_training_sample_creation() {
        let sample = TrainingSample {
            features: vec![1.0, 2.0, 3.0],
            labels: vec![0.1, 0.2, 0.3],
            timestamp: now_micros(),
            symbol: "BTCUSDT".to_string(),
        };
        
        assert_eq!(sample.features.len(), 3);
        assert_eq!(sample.labels.len(), 3);
    }
    
    #[tokio::test]
    async fn test_online_learning_engine_creation() {
        let temp_dir = tempdir().unwrap();
        let model_path = temp_dir.path().join("test_model.safetensors").to_string_lossy().to_string();
        
        let mut config = OnlineLearningConfig::default();
        config.model_save_path = model_path;
        config.use_gpu = false; // Use CPU for testing
        
        let engine = OnlineLearningEngine::new(config);
        assert!(engine.is_ok());
        
        let engine = engine.unwrap();
        assert_eq!(engine.training_step, 0);
        assert_eq!(engine.stats.total_samples_processed, 0);
    }
}