/*!
 * Demo: Online Learning Engine with Candle Framework
 * 
 * Demonstrates real-time ML training and prediction using:
 * - Streaming feature data
 * - Multi-timeframe predictions (3s, 5s, 10s)
 * - Model persistence and loading
 * - Performance monitoring
 */

use rust_hft::ml::online_learning::*;
use rust_hft::core::types::*;
use rust_hft::utils::feature_store::{FeatureStore, FeatureStoreConfig};
use tracing::{info, warn, error};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use rand::Rng;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🧠 Starting Online Learning Engine Demo");
    
    // Create configuration
    let mut config = OnlineLearningConfig::default();
    config.use_gpu = false; // Use CPU for demo
    config.batch_size = 16;
    config.update_frequency = 50;
    config.model_save_interval = 200;
    config.model_save_path = "demo_models/online_model.safetensors".to_string();
    
    info!("📋 Configuration:");
    info!("   Input size: {}", config.input_size);
    info!("   Hidden layers: {:?}", config.hidden_sizes);
    info!("   Output size: {} ({}s predictions)", config.output_size, config.prediction_horizons.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "));
    info!("   Learning rate: {}", config.learning_rate);
    info!("   Batch size: {}", config.batch_size);
    
    // Initialize online learning engine
    let mut engine = OnlineLearningEngine::new(config.clone())?;
    info!("✅ Online learning engine initialized");
    
    // Try to load existing model
    if let Err(e) = engine.load_model() {
        warn!("Could not load existing model: {}", e);
        info!("Starting with fresh model");
    } else {
        info!("✅ Loaded existing model");
    }
    
    // Initialize feature store for demonstration
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("demo_features.redb").to_string_lossy().to_string();
    
    let store_config = FeatureStoreConfig {
        db_path,
        ..Default::default()
    };
    
    let feature_store = Arc::new(Mutex::new(FeatureStore::new(store_config)?));
    info!("✅ Feature store initialized");
    
    // Simulate streaming training data
    info!("🔄 Starting training simulation...");
    
    let mut rng = rand::rng();
    let total_samples = 500;
    let mut prediction_results = Vec::new();
    
    for i in 0..total_samples {
        // Generate synthetic market features
        let features = generate_synthetic_features(&mut rng, i);
        
        // Generate corresponding labels (price changes)
        let labels = generate_synthetic_labels(&mut rng, &features);
        
        // Create training sample
        let training_sample = TrainingSample {
            features: extract_feature_vector(&features),
            labels,
            timestamp: now_micros(),
            symbol: "BTCUSDT".to_string(),
        };
        
        // Add to training
        engine.add_training_sample(training_sample)?;
        
        // Store features for demonstration
        {
            let mut store = feature_store.lock().unwrap();
            store.store_features(&features)?;
        }
        
        // Make prediction every 10 samples
        if i % 10 == 0 {
            match engine.predict(&features) {
                Ok(predictions) => {
                    prediction_results.push(predictions.clone());
                    info!("📈 Sample {}: Predictions = [{:.4}, {:.4}, {:.4}] (3s, 5s, 10s)", 
                          i, predictions[0], predictions[1], predictions[2]);
                }
                Err(e) => {
                    warn!("Prediction failed: {}", e);
                }
            }
        }
        
        // Show training progress
        if i % 100 == 0 && i > 0 {
            let stats = engine.get_stats();
            info!("🔄 Training Progress:");
            info!("   Samples processed: {}", stats.total_samples_processed);
            info!("   Training updates: {}", stats.total_training_updates);
            info!("   Training loss: {:.6}", stats.current_training_loss);
            info!("   Validation loss: {:.6}", stats.current_validation_loss);
            info!("   Accuracy (3s): {:.1}%", stats.model_accuracy_3s * 100.0);
            info!("   Accuracy (5s): {:.1}%", stats.model_accuracy_5s * 100.0);
            info!("   Accuracy (10s): {:.1}%", stats.model_accuracy_10s * 100.0);
            info!("   Training time: {}ms", stats.training_time_ms);
        }
    }
    
    // Final statistics
    let final_stats = engine.get_stats();
    info!("🎯 Final Training Results:");
    info!("   Total samples: {}", final_stats.total_samples_processed);
    info!("   Total updates: {}", final_stats.total_training_updates);
    info!("   Final training loss: {:.6}", final_stats.current_training_loss);
    info!("   Final validation loss: {:.6}", final_stats.current_validation_loss);
    info!("   Final accuracy (3s): {:.1}%", final_stats.model_accuracy_3s * 100.0);
    info!("   Final accuracy (5s): {:.1}%", final_stats.model_accuracy_5s * 100.0);
    info!("   Final accuracy (10s): {:.1}%", final_stats.model_accuracy_10s * 100.0);
    
    // Test prediction performance
    info!("⚡ Testing prediction performance...");
    let test_features = generate_synthetic_features(&mut rng, 1000);
    
    let start_time = std::time::Instant::now();
    let num_predictions = 100;
    
    for _ in 0..num_predictions {
        let _prediction = engine.predict(&test_features)?;
    }
    
    let total_time = start_time.elapsed();
    let avg_time_us = total_time.as_micros() as f64 / num_predictions as f64;
    
    info!("📊 Prediction Performance:");
    info!("   {} predictions in {:.2}ms", num_predictions, total_time.as_millis());
    info!("   Average time per prediction: {:.1}μs", avg_time_us);
    info!("   Throughput: {:.0} predictions/second", 1_000_000.0 / avg_time_us);
    
    // Save final model
    match engine.save_model() {
        Ok(_) => info!("💾 Model saved successfully"),
        Err(e) => error!("Failed to save model: {}", e),
    }
    
    // Analyze prediction patterns
    if !prediction_results.is_empty() {
        info!("📈 Prediction Analysis:");
        
        let mut avg_predictions = vec![0.0; 3];
        for pred in &prediction_results {
            for (i, &val) in pred.iter().enumerate() {
                if i < avg_predictions.len() {
                    avg_predictions[i] += val;
                }
            }
        }
        
        for val in &mut avg_predictions {
            *val /= prediction_results.len() as f64;
        }
        
        info!("   Average predictions: [{:.4}, {:.4}, {:.4}]", 
              avg_predictions[0], avg_predictions[1], avg_predictions[2]);
        
        // Calculate prediction volatility
        let mut volatilities = vec![0.0; 3];
        for pred in &prediction_results {
            for (i, &val) in pred.iter().enumerate() {
                if i < volatilities.len() {
                    let diff = val - avg_predictions[i];
                    volatilities[i] += diff * diff;
                }
            }
        }
        
        for vol in &mut volatilities {
            *vol = (*vol / prediction_results.len() as f64).sqrt() as f64;
        }
        
        info!("   Prediction volatility: [{:.4}, {:.4}, {:.4}]", 
              volatilities[0], volatilities[1], volatilities[2]);
    }
    
    info!("🏁 Online Learning Demo completed successfully!");
    
    Ok(())
}

/// Generate synthetic market features for demonstration
fn generate_synthetic_features(rng: &mut impl Rng, step: usize) -> FeatureSet {
    let base_price = 50000.0 + (step as f64 * 0.1) + rng.random_range(-100.0..100.0);
    let spread = rng.random_range(1.0..20.0);
    let spread_bps = spread / base_price * 10000.0;
    
    let best_bid = (base_price - spread / 2.0).to_price();
    let best_ask = (base_price + spread / 2.0).to_price();
    
    let mut features = FeatureSet::default_enhanced();
    features.timestamp = now_micros();
    features.best_bid = best_bid;
    features.best_ask = best_ask;
    features.mid_price = base_price.to_price();
    features.spread = spread;
    features.spread_bps = spread_bps;
    
    features.latency_network_us = rng.random_range(10..100);
    features.latency_processing_us = rng.random_range(5..50);
    
    // Order book imbalance features
    features.obi_l1 = rng.random_range(-0.3..0.3);
    features.obi_l5 = rng.random_range(-0.2..0.2);
    features.obi_l10 = rng.random_range(-0.15..0.15);
    features.obi_l20 = rng.random_range(-0.1..0.1);
    
    // Depth features
    features.bid_depth_l5 = rng.random_range(1000.0..5000.0);
    features.ask_depth_l5 = rng.random_range(1000.0..5000.0);
    features.bid_depth_l10 = rng.random_range(2000.0..10000.0);
    features.ask_depth_l10 = rng.random_range(2000.0..10000.0);
    features.bid_depth_l20 = rng.random_range(4000.0..20000.0);
    features.ask_depth_l20 = rng.random_range(4000.0..20000.0);
    
    // Imbalance features
    features.depth_imbalance_l5 = rng.random_range(-0.2..0.2);
    features.depth_imbalance_l10 = rng.random_range(-0.15..0.15);
    features.depth_imbalance_l20 = rng.random_range(-0.1..0.1);
    
    // Slope features
    features.bid_slope = rng.random_range(0.1..1.0);
    features.ask_slope = rng.random_range(0.1..1.0);
    
    // Counts
    features.total_bid_levels = rng.random_range(10..50);
    features.total_ask_levels = rng.random_range(10..50);
    
    // Momentum and imbalance
    features.price_momentum = rng.random_range(-0.01..0.01);
    features.volume_imbalance = rng.random_range(-0.3..0.3);
    
    // Quality
    features.is_valid = true;
    features.data_quality_score = rng.random_range(0.9..1.0);
    
    features
}


/// Generate synthetic labels (price changes) for demonstration
fn generate_synthetic_labels(rng: &mut impl Rng, features: &FeatureSet) -> Vec<f64> {
    // Create realistic price changes based on features
    // This is a simplified model for demonstration
    
    let base_change = features.obi_l1 * 0.001 + features.price_momentum * 0.5;
    let volatility = features.spread_bps / 10000.0;
    
    // 3s, 5s, 10s predictions with increasing uncertainty
    let change_3s = base_change + rng.random_range(-volatility..volatility) * 0.5;
    let change_5s = base_change * 1.2 + rng.random_range(-volatility..volatility) * 0.7;
    let change_10s = base_change * 1.5 + rng.random_range(-volatility..volatility) * 1.0;
    
    vec![change_3s, change_5s, change_10s]
}

/// Extract feature vector from FeatureSet (matching the engine implementation)
fn extract_feature_vector(features: &FeatureSet) -> Vec<f64> {
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