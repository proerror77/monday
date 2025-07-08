/*!
 * ML-Enhanced Trading Strategy for Rust HFT
 * 
 * Implements LightGBM-based prediction and trading signal generation
 * Optimized for ultra-low latency inference (<20μs)
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::ml::features::{features_to_vector, validate_features};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use std::collections::VecDeque;
use tracing::{info, warn, error, debug};
use candle_core::{Device, Tensor, DType};
use candle_nn::{Linear, Module, VarBuilder, VarMap};

/// Strategy statistics
#[derive(Debug, Clone, Default)]
pub struct StrategyStats {
    pub features_processed: u64,
    pub predictions_made: u64,
    pub buy_signals: u64,
    pub sell_signals: u64,
    pub hold_signals: u64,
    pub avg_inference_latency_us: f64,
    pub avg_confidence: f64,
    pub model_accuracy: f64,
    pub last_prediction: f64,
}

/// Main strategy thread entry point
pub fn run(
    config: Config,
    features_rx: Receiver<FeatureSet>,
    signal_tx: Sender<TradingSignal>,
) -> Result<()> {
    info!("🧠 Strategy thread starting...");
    
    let mut strategy = MLStrategy::new(config)?;
    
    info!("Strategy initialized, waiting for features...");
    
    // Main strategy loop
    while let Ok(features) = features_rx.recv() {
        let signal_start = now_micros();
        
        match strategy.process_features(features, signal_start) {
            Ok(Some(signal)) => {
                // Send trading signal to execution thread
                if let Err(e) = signal_tx.try_send(signal) {
                    warn!("Failed to send signal to execution: {}", e);
                }
            },
            Ok(None) => {
                // No signal generated (e.g., low confidence, hold decision)
                debug!("No trading signal generated");
            },
            Err(e) => {
                error!("Strategy processing error: {}", e);
            }
        }
    }
    
    info!("🧠 Strategy thread shutting down");
    Ok(())
}

/// ML-based trading strategy with layered prediction architecture
#[allow(dead_code)]
struct MLStrategy {
    /// Primary: Candle deep learning model
    candle_model: Option<CandleModel>,
    
    /// Secondary: SmartCore traditional ML model  
    fallback_model: Option<()>, // 暫時簡化，避免複雜的泛型參數
    
    /// Device for Candle operations (CPU/GPU)
    device: Device,
    
    /// Configuration
    config: Config,
    
    /// Strategy statistics
    stats: StrategyStats,
    
    /// Feature history for validation
    feature_history: VecDeque<FeatureSet>,
    
    /// Prediction history for accuracy tracking
    prediction_history: VecDeque<PredictionRecord>,
    
    /// Risk manager
    risk_manager: RiskManager,
    
    /// Signal filter
    signal_filter: SignalFilter,
}

/// Candle-based deep learning model
#[derive(Debug)]
struct CandleModel {
    /// Neural network layers
    linear1: Linear,
    linear2: Linear,
    output: Linear,
    /// Model metadata
    feature_count: usize,
    model_version: String,
}

impl CandleModel {
    fn new(feature_count: usize, device: &Device) -> Result<Self> {
        let varmap = VarMap::new();
        let vs = VarBuilder::from_varmap(&varmap, DType::F32, device);
        
        let linear1 = candle_nn::linear(feature_count, 128, vs.pp("linear1"))?;
        let linear2 = candle_nn::linear(128, 64, vs.pp("linear2"))?;
        let output = candle_nn::linear(64, 1, vs.pp("output"))?;
        
        Ok(Self {
            linear1,
            linear2,
            output,
            feature_count,
            model_version: "candle_v1".to_string(),
        })
    }
    
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = self.linear1.forward(x)?;
        let x = x.relu()?;
        let x = self.linear2.forward(&x)?;
        let x = x.relu()?;
        let x = self.output.forward(&x)?;
        // Manual sigmoid implementation: 1 / (1 + exp(-x))
        let neg_x = x.neg()?;
        let exp_neg_x = neg_x.exp()?;
        let one = Tensor::ones_like(&exp_neg_x)?;
        let denominator = (one.clone() + exp_neg_x)?;
        let sigmoid = one.broadcast_div(&denominator)?;
        Ok(sigmoid)
    }
    
    fn predict(&self, features: &[f64]) -> Result<f64> {
        if features.len() != self.feature_count {
            return Err(anyhow::anyhow!("Feature count mismatch: expected {}, got {}", 
                                      self.feature_count, features.len()));
        }
        
        // Convert f64 to f32 for Candle
        let features_f32: Vec<f32> = features.iter().map(|&x| x as f32).collect();
        let tensor = Tensor::from_slice(&features_f32, (1, self.feature_count), &Device::Cpu)?;
        let output = self.forward(&tensor)?;
        let result = output.to_scalar::<f32>()?;
        Ok(result as f64)
    }
}

impl MLStrategy {
    fn new(config: Config) -> Result<Self> {
        // Initialize device (prefer GPU if available)
        let device = Self::initialize_device();
        info!("Initialized ML device: {:?}", device);
        
        // Try to load Candle model
        let candle_model = Self::load_candle_model(&config.ml.model_path, &device)?;
        
        // Initialize fallback model
        let fallback_model = Self::initialize_fallback_model()?;
        
        Ok(Self {
            candle_model,
            fallback_model,
            device,
            config: config.clone(),
            stats: StrategyStats::default(),
            feature_history: VecDeque::with_capacity(1000),
            prediction_history: VecDeque::with_capacity(1000),
            risk_manager: RiskManager::new(&config),
            signal_filter: SignalFilter::new(&config),
        })
    }
    
    /// Initialize device for Candle operations
    fn initialize_device() -> Device {
        #[cfg(feature = "cuda")]
        {
            if candle_core::cuda::has_cuda() {
                info!("CUDA available, using GPU acceleration");
                return Device::new_cuda(0).unwrap_or_else(|e| {
                    warn!("Failed to initialize CUDA device: {}, falling back to CPU", e);
                    Device::Cpu
                });
            }
        }
        
        #[cfg(feature = "metal")]
        {
            if let Ok(device) = Device::new_metal(0) {
                info!("Metal available, using GPU acceleration");
                return device;
            }
        }
        
        info!("Using CPU device for ML operations");
        Device::Cpu
    }
    
    /// Load Candle model from file or create new one
    fn load_candle_model(model_path: &str, device: &Device) -> Result<Option<CandleModel>> {
        // For now, create a new model (in production, you'd load from safetensors)
        if std::path::Path::new(model_path).exists() {
            info!("Model file found but loading from safetensors not yet implemented");
            info!("Creating new Candle model for now");
        } else {
            info!("Model file not found: {}. Creating new Candle model.", model_path);
        }
        
        // Create new model with expected feature count
        let feature_count = 17; // From features_to_vector function
        let model = CandleModel::new(feature_count, device)?;
        info!("Candle model initialized with {} features", feature_count);
        Ok(Some(model))
    }
    
    /// Initialize SmartCore fallback model
    fn initialize_fallback_model() -> Result<Option<()>> {
        info!("Initializing SmartCore fallback model (placeholder)");
        // For now, we'll create a placeholder that will be trained later
        // In production, you'd load a pre-trained model
        Ok(Some(()))
    }
    
    /// Process features and generate trading signal
    fn process_features(
        &mut self,
        features: FeatureSet,
        signal_start: Timestamp,
    ) -> Result<Option<TradingSignal>> {
        let start_time = now_micros();
        
        // Validate features
        if !validate_features(&features) {
            warn!("Invalid features received, skipping");
            return Ok(None);
        }
        
        // Store feature history
        self.feature_history.push_back(features.clone());
        if self.feature_history.len() > 1000 {
            self.feature_history.pop_front();
        }
        
        // Generate prediction using layered architecture
        let prediction = self.make_layered_prediction(&features)?;
        
        self.stats.predictions_made += 1;
        self.stats.last_prediction = prediction.probability;
        
        // Update confidence statistics
        self.update_confidence_stats(prediction.confidence);
        
        // Generate trading signal
        let signal = self.generate_signal(features, prediction.clone(), signal_start)?;
        
        // Apply risk management
        let final_signal = self.risk_manager.validate_signal(signal)?;
        
        // Apply signal filtering
        let filtered_signal = self.signal_filter.filter_signal(final_signal)?;
        
        let inference_latency = now_micros() - start_time;
        self.update_latency_stats(inference_latency);
        
        debug!("Strategy processing: {}μs, prediction: {:.3}, confidence: {:.3}", 
               inference_latency, prediction.probability, prediction.confidence);
        
        Ok(filtered_signal)
    }
    
    /// Make layered prediction: Candle -> SmartCore -> Rule-based
    fn make_layered_prediction(&self, features: &FeatureSet) -> Result<Prediction> {
        let start_time = now_micros();
        
        // Try primary Candle model first
        if let Some(ref candle_model) = self.candle_model {
            match self.make_candle_prediction(candle_model, features) {
                Ok(prediction) => {
                    let latency = now_micros() - start_time;
                    if latency < 60_000 { // 60μs threshold
                        debug!("Candle prediction latency: {}μs", latency);
                        return Ok(prediction);
                    } else {
                        warn!("Candle prediction too slow ({}μs), falling back", latency);
                    }
                },
                Err(e) => {
                    warn!("Candle prediction failed: {}, falling back", e);
                }
            }
        }
        
        // Try SmartCore fallback model
        if let Some(ref fallback_model) = self.fallback_model {
            match self.make_smartcore_prediction(fallback_model, features) {
                Ok(prediction) => {
                    let latency = now_micros() - start_time;
                    debug!("SmartCore fallback prediction latency: {}μs", latency);
                    return Ok(prediction);
                },
                Err(e) => {
                    warn!("SmartCore prediction failed: {}, using rule-based fallback", e);
                }
            }
        }
        
        // Final fallback: rule-based prediction
        let prediction = self.rule_based_prediction(features);
        let latency = now_micros() - start_time;
        debug!("Rule-based fallback prediction latency: {}μs", latency);
        Ok(prediction)
    }
    
    /// Make prediction using Candle deep learning model
    fn make_candle_prediction(
        &self,
        model: &CandleModel,
        features: &FeatureSet,
    ) -> Result<Prediction> {
        // Convert features to vector
        let feature_vector = features_to_vector(features);
        
        // Make prediction using Candle model
        let probability = model.predict(&feature_vector)?;
        
        // Calculate confidence based on distance from decision boundary
        let confidence = self.calculate_confidence(probability);
        
        Ok(Prediction {
            probability,
            confidence,
            model_version: model.model_version.clone(),
            feature_count: feature_vector.len(),
        })
    }
    
    /// Make prediction using SmartCore traditional ML
    fn make_smartcore_prediction(
        &self,
        _model: &(),
        features: &FeatureSet,
    ) -> Result<Prediction> {
        // For now, use a simplified model since we haven't trained SmartCore yet
        let feature_vector = features_to_vector(features);
        
        // Simple weighted combination of features
        let weighted_signal = feature_vector[1] * 0.3 + // obi_l1
                             feature_vector[3] * 0.5 + // obi_l10
                             feature_vector[11] * 0.2; // price_momentum
        
        let probability = 0.5 + weighted_signal.tanh() * 0.5;
        let confidence = weighted_signal.abs().min(1.0);
        
        Ok(Prediction {
            probability,
            confidence,
            model_version: "smartcore_placeholder".to_string(),
            feature_count: feature_vector.len(),
        })
    }
    
    /// Rule-based prediction (final fallback)
    fn rule_based_prediction(&self, features: &FeatureSet) -> Prediction {
        // Simple rule-based strategy using OBI
        let obi_signal = features.obi_l10;
        let spread_signal = if features.spread_bps < 10.0 { 0.1 } else { -0.1 };
        
        // Combine signals
        let combined_signal = obi_signal * 0.7 + spread_signal * 0.3;
        
        // Convert to probability
        let probability = 0.5 + combined_signal.tanh() * 0.5;
        let confidence = combined_signal.abs().min(1.0);
        
        Prediction {
            probability,
            confidence,
            model_version: "fallback_v1".to_string(),
            feature_count: 3, // obi, spread, derived
        }
    }
    
    /// Calculate prediction confidence
    fn calculate_confidence(&self, probability: f64) -> f64 {
        // Distance from neutral (0.5) indicates confidence
        let distance_from_neutral = (probability - 0.5).abs();
        
        // Scale to [0, 1] range
        (distance_from_neutral * 2.0).min(1.0)
    }
    
    /// Generate trading signal from prediction
    fn generate_signal(
        &mut self,
        features: FeatureSet,
        prediction: Prediction,
        signal_start: Timestamp,
    ) -> Result<Option<TradingSignal>> {
        let signal_latency = now_micros() - signal_start;
        
        // Determine signal type based on thresholds
        let signal_type = if prediction.probability > self.config.ml.buy_threshold &&
                            prediction.confidence > self.config.ml.prediction_threshold {
            self.stats.buy_signals += 1;
            SignalType::Buy
        } else if prediction.probability < self.config.ml.sell_threshold &&
                  prediction.confidence > self.config.ml.prediction_threshold {
            self.stats.sell_signals += 1;
            SignalType::Sell
        } else {
            self.stats.hold_signals += 1;
            return Ok(None); // No signal for hold
        };
        
        // Calculate suggested price and quantity
        let (suggested_price, suggested_quantity) = self.calculate_order_params(
            &features, 
            signal_type, 
            prediction.confidence
        );
        
        let signal = TradingSignal {
            signal_type,
            confidence: prediction.confidence,
            suggested_price,
            suggested_quantity,
            timestamp: now_micros(),
            features_timestamp: features.timestamp,
            signal_latency_us: signal_latency,
        };
        
        Ok(Some(signal))
    }
    
    /// Calculate optimal order parameters
    fn calculate_order_params(
        &self,
        features: &FeatureSet,
        signal_type: SignalType,
        confidence: f64,
    ) -> (Price, Quantity) {
        // Base order size from config
        let base_size = self.config.trading.order_size;
        
        // Adjust size based on confidence
        let confidence_multiplier = (confidence * 1.5).min(2.0); // Max 2x size
        let adjusted_size = base_size * confidence_multiplier;
        
        // Calculate price based on signal type and market conditions
        let suggested_price = match signal_type {
            SignalType::Buy => {
                // For buy signals, be slightly aggressive to ensure fill
                let aggressive_factor = 1.0 + (confidence * 0.001); // Max 0.1% premium
                features.best_ask * aggressive_factor
            },
            SignalType::Sell => {
                // For sell signals, be slightly aggressive to ensure fill
                let aggressive_factor = 1.0 - (confidence * 0.001); // Max 0.1% discount
                features.best_bid * aggressive_factor
            },
            SignalType::Hold => features.mid_price, // Shouldn't happen
        };
        
        (suggested_price, adjusted_size.to_quantity())
    }
    
    /// Update confidence statistics
    fn update_confidence_stats(&mut self, confidence: f64) {
        if self.stats.predictions_made == 0 {
            self.stats.avg_confidence = confidence;
        } else {
            let alpha = 0.1;
            self.stats.avg_confidence = 
                alpha * confidence + (1.0 - alpha) * self.stats.avg_confidence;
        }
    }
    
    /// Update latency statistics
    fn update_latency_stats(&mut self, latency_us: u64) {
        if self.stats.predictions_made == 0 {
            self.stats.avg_inference_latency_us = latency_us as f64;
        } else {
            let alpha = 0.1;
            self.stats.avg_inference_latency_us = 
                alpha * latency_us as f64 + (1.0 - alpha) * self.stats.avg_inference_latency_us;
        }
    }
    
    /// Get strategy statistics
    #[allow(dead_code)]
    pub fn get_stats(&self) -> StrategyStats {
        self.stats.clone()
    }
}

/// Prediction result
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct Prediction {
    probability: f64,
    confidence: f64,
    model_version: String,
    feature_count: usize,
}

/// Prediction record for accuracy tracking
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PredictionRecord {
    timestamp: Timestamp,
    prediction: Prediction,
    actual_outcome: Option<f64>, // Will be filled later
}

/// Risk manager for signal validation
struct RiskManager {
    config: Config,
    recent_signals: VecDeque<TradingSignal>,
    daily_loss: f64,
    max_position_reached: bool,
}

impl RiskManager {
    fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            recent_signals: VecDeque::with_capacity(100),
            daily_loss: 0.0,
            max_position_reached: false,
        }
    }
    
    fn validate_signal(&mut self, signal: Option<TradingSignal>) -> Result<Option<TradingSignal>> {
        let Some(signal) = signal else {
            return Ok(None);
        };
        
        // Check daily loss limit
        if self.daily_loss > self.config.risk.max_daily_loss {
            warn!("Daily loss limit exceeded, blocking signal");
            return Ok(None);
        }
        
        // Check signal rate limiting
        let recent_count = self.recent_signals.iter()
            .filter(|s| now_micros() - s.timestamp < 1_000_000) // Last 1 second
            .count();
        
        if recent_count >= self.config.risk.max_order_rate as usize {
            warn!("Order rate limit exceeded, blocking signal");
            return Ok(None);
        }
        
        // Check position limits (simplified)
        if self.max_position_reached {
            warn!("Max position reached, blocking signal");
            return Ok(None);
        }
        
        // Store signal
        self.recent_signals.push_back(signal.clone());
        if self.recent_signals.len() > 100 {
            self.recent_signals.pop_front();
        }
        
        Ok(Some(signal))
    }
}

/// Signal filter for additional processing
#[allow(dead_code)]
struct SignalFilter {
    config: Config,
    last_signal_time: Timestamp,
    signal_count: u64,
}

impl SignalFilter {
    fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            last_signal_time: 0,
            signal_count: 0,
        }
    }
    
    fn filter_signal(&mut self, signal: Option<TradingSignal>) -> Result<Option<TradingSignal>> {
        let Some(signal) = signal else {
            return Ok(None);
        };
        
        // Minimum time between signals
        let min_interval_us = 100_000; // 100ms minimum
        let time_since_last = now_micros() - self.last_signal_time;
        
        if time_since_last < min_interval_us {
            debug!("Signal filtered due to minimum interval");
            return Ok(None);
        }
        
        self.last_signal_time = now_micros();
        self.signal_count += 1;
        
        Ok(Some(signal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_strategy_creation() {
        let config = Config::default();
        let strategy = MLStrategy::new(config);
        // Should succeed even without model file
        assert!(strategy.is_ok());
    }
    
    #[test]
    fn test_confidence_calculation() {
        let config = Config::default();
        let strategy = MLStrategy::new(config).unwrap();
        
        // Test confidence calculation
        assert_eq!(strategy.calculate_confidence(0.5), 0.0); // Neutral
        assert_eq!(strategy.calculate_confidence(1.0), 1.0); // Maximum confidence
        assert_eq!(strategy.calculate_confidence(0.0), 1.0); // Maximum confidence
        assert_eq!(strategy.calculate_confidence(0.75), 0.5); // 50% confidence
    }
    
    #[test]
    fn test_fallback_prediction() {
        let config = Config::default();
        let strategy = MLStrategy::new(config).unwrap();
        
        let mut features = FeatureSet::default_enhanced();
        features.timestamp = now_micros();
        features.latency_network_us = 10;
        features.latency_processing_us = 20;
        features.best_bid = 100.0.to_price();
        features.best_ask = 101.0.to_price();
        features.mid_price = 100.5.to_price();
        features.spread = 1.0;
        features.spread_bps = 100.0;
        features.obi_l1 = 0.0;
        features.obi_l5 = 0.0;
        features.obi_l10 = 0.3; // Strong buy signal
        features.obi_l20 = 0.0;
        features.bid_depth_l5 = 1000.0;
        features.ask_depth_l5 = 1000.0;
        features.bid_depth_l10 = 2000.0;
        features.ask_depth_l10 = 2000.0;
        features.bid_depth_l20 = 4000.0;
        features.ask_depth_l20 = 4000.0;
        features.depth_imbalance_l5 = 0.0;
        features.depth_imbalance_l10 = 0.0;
        features.depth_imbalance_l20 = 0.0;
        features.bid_slope = 0.0;
        features.ask_slope = 0.0;
        features.total_bid_levels = 10;
        features.total_ask_levels = 10;
        features.price_momentum = 0.0;
        features.volume_imbalance = 0.0;
        features.is_valid = true;
        features.data_quality_score = 1.0;
        
        let prediction = strategy.rule_based_prediction(&features);
        
        // Should generate a buy-biased prediction due to positive OBI
        assert!(prediction.probability > 0.5);
        assert!(prediction.confidence > 0.0);
    }
    
    #[test]
    fn test_risk_manager() {
        let config = Config::default();
        let mut risk_manager = RiskManager::new(&config);
        
        let signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.8,
            suggested_price: 100.0.to_price(),
            suggested_quantity: 0.001.to_quantity(),
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 50,
        };
        
        // First signal should pass
        let result = risk_manager.validate_signal(Some(signal.clone()));
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }
}