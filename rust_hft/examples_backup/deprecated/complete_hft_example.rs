/*!
 * Complete HFT System Example
 * 
 * Demonstrates the full HFT pipeline:
 * - Feature store with real data
 * - Performance optimization
 * - Machine learning model training
 * - Backtesting framework
 * - Live trading simulation
 */

use rust_hft::core::types::*;
use rust_hft::utils::feature_store::{FeatureStore, FeatureStoreConfig};
use rust_hft::utils::performance::{PerformanceConfig, MemoryPool};
use rust_hft::utils::backtesting::{BacktestConfig, BacktestEngine, BacktestStrategy, BacktestPortfolio};
use rust_hft::ml::model_training_simple::SimplifiedPredictor;
use rust_hft::ml::online_learning::{OnlineLearningEngine, OnlineLearningConfig, TrainingSample};
use anyhow::Result;
use tracing::{info, warn};
use rand::Rng;
use tempfile::tempdir;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🚀 Starting Complete HFT System Example");
    
    // 1. Hardware and Performance Optimization
    info!("🔧 Initializing performance optimizations...");
    let perf_config = PerformanceConfig::default();
    info!("Performance config: CPU isolation={}, SIMD={}", 
          perf_config.cpu_isolation, perf_config.simd_acceleration);
    
    // Initialize memory pools for zero-allocation performance
    let mut feature_pool = MemoryPool::new(
        || FeatureSet::default_enhanced(),
        100,  // initial size
        1000  // max size
    );
    info!("✅ Memory pools initialized");
    
    // 2. Feature Store Setup
    info!("📊 Setting up feature store...");
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("hft_features.redb").to_string_lossy().to_string();
    
    let store_config = FeatureStoreConfig {
        db_path: db_path.clone(),
        write_buffer_size: 1000,
        compression_enabled: true,
        cache_size_mb: 128,
        ..Default::default()
    };
    
    let mut feature_store = FeatureStore::new(store_config)?;
    info!("✅ Feature store initialized at: {}", db_path);
    
    // 3. Generate and Store Training Data
    info!("🎯 Generating synthetic market data...");
    let training_data = generate_synthetic_market_data(5000)?;
    info!("Generated {} training samples", training_data.len());
    
    // Store features in feature store
    for (i, features) in training_data.iter().enumerate() {
        feature_store.store_features(features)?;
        
        if i % 1000 == 0 {
            info!("Stored {} features...", i);
        }
    }
    feature_store.flush_buffer()?;
    info!("✅ All features stored in database");
    
    // 4. Traditional ML Model Training
    info!("🧠 Training simplified ML model...");
    let mut trained_model = SimplifiedPredictor::new();
    
    // Simple training loop with first 80% of data
    let train_size = (training_data.len() as f64 * 0.8) as usize;
    let learning_rate = 0.01;
    
    for features in &training_data[..train_size] {
        // Generate a simple target based on features
        let target = if features.obi_l10 > 0.1 { 1.0 } else if features.obi_l10 < -0.1 { -1.0 } else { 0.0 };
        trained_model.update_weights(features, target, learning_rate)?;
    }
    
    info!("✅ Simplified ML model trained with {} samples", train_size);
    
    // 5. Online Learning Engine Setup
    info!("🔄 Setting up online learning engine...");
    let mut online_config = OnlineLearningConfig::default();
    online_config.use_gpu = false; // Use CPU for this example
    online_config.batch_size = 16;
    online_config.update_frequency = 50;
    
    let model_path = temp_dir.path().join("online_model.safetensors").to_string_lossy().to_string();
    online_config.model_save_path = model_path;
    
    let mut online_engine = OnlineLearningEngine::new(online_config.clone())?;
    info!("✅ Online learning engine initialized");
    
    // 6. Feature Store Query Performance Test
    info!("⚡ Testing feature store performance...");
    let query_start = now_micros();
    let recent_features = feature_store.get_latest_features(100)?;
    let query_latency = now_micros() - query_start;
    
    info!("✅ Query performance: {} features in {}μs", recent_features.len(), query_latency);
    
    // 7. Memory Pool Performance Test
    info!("🏊 Testing memory pool performance...");
    let pool_start = now_micros();
    let mut acquired_features = Vec::new();
    
    for _ in 0..100 {
        acquired_features.push(feature_pool.acquire());
    }
    
    for feature in acquired_features {
        feature_pool.release(feature);
    }
    
    let pool_latency = now_micros() - pool_start;
    let (created, reused, _allocated) = feature_pool.stats();
    info!("✅ Memory pool performance: 100 acquire/release cycles in {}μs", pool_latency);
    info!("   Created: {}, Reused: {}", created, reused);
    
    // 8. Online Training with Synthetic Stream
    info!("📡 Simulating online training with streaming data...");
    let mut rng = rand::rng();
    
    for i in 0..200 {
        // Generate new market data
        let features = generate_synthetic_features(&mut rng, i);
        let labels = generate_synthetic_labels(&mut rng, &features);
        
        // Create training sample
        let training_sample = TrainingSample {
            features: extract_feature_vector(&features),
            labels,
            timestamp: now_micros(),
            symbol: "BTCUSDT".to_string(),
        };
        
        // Add to online training
        online_engine.add_training_sample(training_sample)?;
        
        // Make predictions occasionally
        if i % 20 == 0 {
            match online_engine.predict(&features) {
                Ok(predictions) => {
                    info!("📈 Online prediction {}: [{:.4}, {:.4}, {:.4}] (3s, 5s, 10s)", 
                          i, predictions[0], predictions[1], predictions[2]);
                }
                Err(e) => warn!("Prediction failed: {}", e),
            }
        }
        
        // Show progress
        if i % 50 == 0 && i > 0 {
            let stats = online_engine.get_stats();
            info!("🔄 Online training progress: {} samples, {} updates, loss: {:.6}", 
                  stats.total_samples_processed, 
                  stats.total_training_updates,
                  stats.current_training_loss);
        }
    }
    
    // 9. Backtesting Framework
    info!("📊 Running comprehensive backtest...");
    let backtest_config = BacktestConfig {
        start_time: 0,
        end_time: now_micros(),
        initial_balance: 100000.0,
        commission_rate: 0.001,
        slippage_bps: 2.0,
        max_position_size: 10000.0,
        max_daily_trades: 100,
        benchmark_symbol: "BTCUSDT".to_string(),
        risk_free_rate: 0.02,
        output_path: temp_dir.path().join("backtest_results").to_string_lossy().to_string(),
    };
    
    let mut backtest_engine = BacktestEngine::new(backtest_config);
    let mut backtest_strategy = SimpleMLBacktestStrategy::new(trained_model.clone());
    
    let backtest_results = backtest_engine.run_backtest(&training_data, &mut backtest_strategy)?;
    
    info!("✅ Backtest completed:");
    info!("   Total Return: {:.2}%", backtest_results.total_return * 100.0);
    info!("   Max Drawdown: {:.2}%", backtest_results.max_drawdown * 100.0);
    info!("   Sharpe Ratio: {:.2}", backtest_results.sharpe_ratio);
    info!("   Total Trades: {}", backtest_results.total_trades);
    info!("   Win Rate: {:.2}%", backtest_results.win_rate * 100.0);
    
    // 10. Performance Benchmarking
    info!("⏱️ Running performance benchmarks...");
    
    // ML Inference Latency Test
    let inference_iterations = 1000;
    let mut inference_latencies = Vec::with_capacity(inference_iterations);
    
    for _ in 0..inference_iterations {
        let features = generate_synthetic_features(&mut rng, 0);
        let start = now_micros();
        let _prediction = trained_model.predict(&features)?;
        let latency = now_micros() - start;
        inference_latencies.push(latency);
    }
    
    inference_latencies.sort();
    let inference_p50 = inference_latencies[inference_iterations / 2];
    let inference_p95 = inference_latencies[inference_iterations * 95 / 100];
    let inference_p99 = inference_latencies[inference_iterations * 99 / 100];
    
    info!("✅ ML Inference Performance:");
    info!("   P50: {}μs, P95: {}μs, P99: {}μs", inference_p50, inference_p95, inference_p99);
    
    // Feature Extraction Latency Test
    let mut extraction_latencies = Vec::with_capacity(inference_iterations);
    
    for _ in 0..inference_iterations {
        let features = feature_pool.acquire();
        let start = now_micros();
        let _vector = extract_feature_vector(&features);
        let latency = now_micros() - start;
        extraction_latencies.push(latency);
        feature_pool.release(features);
    }
    
    extraction_latencies.sort();
    let extraction_p99 = extraction_latencies[inference_iterations * 99 / 100];
    
    info!("✅ Feature Extraction Performance:");
    info!("   P99: {}μs", extraction_p99);
    
    // 11. System Statistics Summary
    let store_stats = feature_store.get_stats();
    let final_online_stats = online_engine.get_stats();
    
    info!("📈 Final System Statistics:");
    info!("Feature Store:");
    info!("   Total features: {}", store_stats.total_features_stored);
    info!("   Avg write latency: {:.2}μs", store_stats.avg_write_latency_us);
    info!("   Avg read latency: {:.2}μs", store_stats.avg_read_latency_us);
    info!("   Storage size: {:.2} MB", store_stats.storage_size_bytes as f64 / 1024.0 / 1024.0);
    
    info!("Online Learning:");
    info!("   Samples processed: {}", final_online_stats.total_samples_processed);
    info!("   Training updates: {}", final_online_stats.total_training_updates);
    info!("   Final training loss: {:.6}", final_online_stats.current_training_loss);
    info!("   Model accuracy: {:.1}% / {:.1}% / {:.1}%", 
          final_online_stats.model_accuracy_3s * 100.0,
          final_online_stats.model_accuracy_5s * 100.0,
          final_online_stats.model_accuracy_10s * 100.0);
    
    // 12. Performance Target Validation
    let target_inference_latency = 50; // μs
    let target_extraction_latency = 10; // μs
    
    info!("🎯 Performance Target Validation:");
    if inference_p99 <= target_inference_latency {
        info!("✅ ML inference target achieved (P99 ≤ {}μs)", target_inference_latency);
    } else {
        warn!("⚠️  ML inference target missed (P99 {}μs > {}μs)", 
              inference_p99, target_inference_latency);
    }
    
    if extraction_p99 <= target_extraction_latency {
        info!("✅ Feature extraction target achieved (P99 ≤ {}μs)", target_extraction_latency);
    } else {
        warn!("⚠️  Feature extraction target missed (P99 {}μs > {}μs)", 
              extraction_p99, target_extraction_latency);
    }
    
    // Save model and cleanup
    online_engine.save_model()?;
    info!("💾 Online learning model saved");
    
    info!("🏁 Complete HFT System Example finished successfully!");
    info!("📁 Temporary files stored in: {}", temp_dir.path().display());
    
    Ok(())
}

/// Generate synthetic market data for training
fn generate_synthetic_market_data(count: usize) -> Result<Vec<FeatureSet>> {
    let mut rng = rand::rng();
    let mut data = Vec::with_capacity(count);
    
    for i in 0..count {
        let features = generate_synthetic_features(&mut rng, i);
        data.push(features);
    }
    
    Ok(data)
}

/// Generate synthetic market features
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
    
    // Imbalance and momentum
    features.depth_imbalance_l5 = rng.random_range(-0.2..0.2);
    features.depth_imbalance_l10 = rng.random_range(-0.15..0.15);
    features.depth_imbalance_l20 = rng.random_range(-0.1..0.1);
    features.price_momentum = rng.random_range(-0.01..0.01);
    features.volume_imbalance = rng.random_range(-0.3..0.3);
    
    // Quality and validation
    features.is_valid = true;
    features.data_quality_score = rng.random_range(0.9..1.0);
    
    features
}

/// Generate synthetic labels for training
fn generate_synthetic_labels(rng: &mut impl Rng, features: &FeatureSet) -> Vec<f64> {
    let base_change = features.obi_l1 * 0.001 + features.price_momentum * 0.5;
    let volatility = features.spread_bps / 10000.0;
    
    // 3s, 5s, 10s predictions with increasing uncertainty
    let change_3s = base_change + rng.random_range(-volatility..volatility) * 0.5;
    let change_5s = base_change * 1.2 + rng.random_range(-volatility..volatility) * 0.7;
    let change_10s = base_change * 1.5 + rng.random_range(-volatility..volatility) * 1.0;
    
    vec![change_3s, change_5s, change_10s]
}

/// Extract feature vector from FeatureSet
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

/// Simple ML-based backtest strategy
struct SimpleMLBacktestStrategy {
    model: SimplifiedPredictor,
    confidence_threshold: f64,
}

impl SimpleMLBacktestStrategy {
    fn new(model: SimplifiedPredictor) -> Self {
        Self {
            model,
            confidence_threshold: 0.3,
        }
    }
}

impl BacktestStrategy for SimpleMLBacktestStrategy {
    fn generate_signal(&mut self, features: &FeatureSet, _portfolio: &BacktestPortfolio) -> Result<Option<TradingSignal>> {
        let prediction = self.model.predict(features)?;
        
        // Convert prediction to trading signal
        if prediction > self.confidence_threshold {
            Ok(Some(TradingSignal {
                signal_type: SignalType::Buy,
                confidence: prediction.abs(),
                suggested_price: features.best_ask,
                suggested_quantity: 0.001.to_quantity(),
                timestamp: features.timestamp,
                features_timestamp: features.timestamp,
                signal_latency_us: 0,
            }))
        } else if prediction < -self.confidence_threshold {
            Ok(Some(TradingSignal {
                signal_type: SignalType::Sell,
                confidence: prediction.abs(),
                suggested_price: features.best_bid,
                suggested_quantity: 0.001.to_quantity(),
                timestamp: features.timestamp,
                features_timestamp: features.timestamp,
                signal_latency_us: 0,
            }))
        } else {
            Ok(None) // Hold
        }
    }
}