/*!
 * Performance-Optimized Orderbook Demonstrations
 * 
 * This example showcases different levels of orderbook performance optimization:
 * 1. High-Performance Orderbook (basic optimizations)
 * 2. Ultra-High-Performance Orderbook (advanced lock-free techniques)
 * 3. Multi-Level Orderbook Processing (deep market analysis)
 * 
 * This combines functionality from:
 * - demo_high_perf_orderbook.rs
 * - demo_ultra_high_perf_orderbook.rs  
 * - demo_multi_level_orderbook.rs
 */

use rust_hft::{
    integrations::{
        bitget_connector::*,
        high_perf_orderbook_manager::*,
    },
    core::{types::*, orderbook::OrderBook},
    utils::performance::*,
};
use anyhow::Result;
use tracing::{info, warn, debug};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use std::collections::HashMap;

/// Performance optimization levels
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PerformanceLevel {
    High,        // Basic optimizations
    UltraHigh,   // Advanced lock-free techniques
    MultiLevel,  // Deep market analysis with performance focus
}

/// Performance benchmark results
#[derive(Debug, Clone)]
pub struct PerformanceBenchmark {
    pub level: PerformanceLevel,
    pub updates_processed: u64,
    pub avg_latency_us: f64,
    pub p99_latency_us: u64,
    pub memory_usage_mb: f64,
    pub cpu_efficiency: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize performance optimizations
    let _capabilities = rust_hft::utils::performance::detect_hardware_capabilities();
    let _perf_manager = rust_hft::utils::performance::PerformanceManager::new(
        rust_hft::utils::performance::PerformanceConfig::default()
    );
    
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting Performance-Optimized Orderbook Demo");
    info!("⚡ This demo showcases different levels of performance optimization");

    // Run performance comparison
    run_performance_comparison().await?;

    Ok(())
}

async fn run_performance_comparison() -> Result<()> {
    info!("📊 Running Performance Level Comparison");
    
    let levels = [
        PerformanceLevel::High,
        PerformanceLevel::UltraHigh,
        PerformanceLevel::MultiLevel,
    ];

    let mut results = Vec::new();

    for level in levels.iter() {
        info!("🔄 Testing performance level: {:?}", level);
        let benchmark = run_performance_test(*level).await?;
        results.push(benchmark);
        
        // Cool down between tests
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Display comparison results
    display_performance_comparison(&results);

    Ok(())
}

async fn run_performance_test(level: PerformanceLevel) -> Result<PerformanceBenchmark> {
    let test_duration = Duration::from_secs(30);
    let _start_time = Instant::now();
    
    match level {
        PerformanceLevel::High => run_high_performance_test(test_duration).await,
        PerformanceLevel::UltraHigh => run_ultra_high_performance_test(test_duration).await,
        PerformanceLevel::MultiLevel => run_multi_level_performance_test(test_duration).await,
    }
}

async fn run_high_performance_test(duration: Duration) -> Result<PerformanceBenchmark> {
    info!("⚡ Starting High-Performance Orderbook Test");
    
    // Create high-performance orderbook manager
    let orderbook_manager = Arc::new(Mutex::new(
        HighPerfOrderbookManager::new("BTCUSDT".to_string())?
    ));

    // Performance tracking
    let mut update_count = 0u64;
    let mut latency_sum = 0u64;
    let mut latency_samples: Vec<u64> = Vec::new();
    let _start_time = Instant::now();

    // Configure Bitget connection
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    // High-performance message handler
    let orderbook_clone = orderbook_manager.clone();
    let message_handler = move |message: BitgetMessage| {
        let process_start = now_micros();
        
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                if let Ok(mut manager) = orderbook_clone.lock() {
                    if let Err(e) = manager.process_high_perf_update(&symbol, &data, timestamp) {
                        debug!("High-perf processing error: {}", e);
                    } else {
                        let latency = now_micros() - process_start;
                        // Note: In a real implementation, we'd use atomic operations
                        // or lock-free structures to avoid this mutex in hot path
                    }
                }
            }
            _ => {}
        }
    };

    // Run test
    tokio::select! {
        result = connector.connect_public(message_handler) => {
            if let Err(e) = result {
                warn!("Connection failed: {}", e);
            }
        }
        _ = tokio::time::sleep(duration) => {
            info!("✅ High-performance test completed");
        }
    }

    // Calculate results
    let manager = orderbook_manager.lock().unwrap();
    let stats = manager.get_performance_stats();
    
    Ok(PerformanceBenchmark {
        level: PerformanceLevel::High,
        updates_processed: stats.total_updates,
        avg_latency_us: stats.avg_latency_us,
        p99_latency_us: stats.p99_latency_us,
        memory_usage_mb: stats.memory_usage_mb,
        cpu_efficiency: stats.cpu_efficiency,
    })
}

async fn run_ultra_high_performance_test(duration: Duration) -> Result<PerformanceBenchmark> {
    info!("🚀 Starting Ultra-High-Performance Orderbook Test");
    info!("💡 Using lock-free algorithms and SIMD optimizations");
    
    // Create ultra-high-performance manager with lock-free structures
    let orderbook_manager = Arc::new(
        UltraHighPerfOrderbookManager::new("BTCUSDT".to_string())?
    );

    // Enable all performance optimizations
    orderbook_manager.enable_simd_processing(true);
    orderbook_manager.enable_zero_copy_parsing(true);
    orderbook_manager.set_cpu_affinity(0)?; // Bind to specific CPU core

    let _start_time = Instant::now();

    // Configure connection with optimized settings
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    // Ultra-high-performance message handler with minimal allocations
    let manager_clone = orderbook_manager.clone();
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                // Zero-copy processing with lock-free algorithms
                if let Err(e) = manager_clone.process_ultra_fast(&symbol, &data, timestamp) {
                    debug!("Ultra-fast processing error: {}", e);
                }
            }
            _ => {}
        }
    };

    // Run test
    tokio::select! {
        result = connector.connect_public(message_handler) => {
            if let Err(e) = result {
                warn!("Connection failed: {}", e);
            }
        }
        _ = tokio::time::sleep(duration) => {
            info!("✅ Ultra-high-performance test completed");
        }
    }

    // Get final statistics
    let stats = orderbook_manager.get_ultra_perf_stats();
    
    Ok(PerformanceBenchmark {
        level: PerformanceLevel::UltraHigh,
        updates_processed: stats.total_updates,
        avg_latency_us: stats.avg_latency_us,
        p99_latency_us: stats.p99_latency_us,
        memory_usage_mb: stats.memory_usage_mb,
        cpu_efficiency: stats.cpu_efficiency,
    })
}

async fn run_multi_level_performance_test(duration: Duration) -> Result<PerformanceBenchmark> {
    info!("📊 Starting Multi-Level Orderbook Performance Test");
    info!("🔍 Processing deep market data (L20+) with performance focus");
    
    // Create multi-level orderbook processor
    let processor = Arc::new(
        MultiLevelOrderbookProcessor::new("BTCUSDT".to_string(), 20)?
    );

    // Enable deep market analysis with performance optimizations
    processor.enable_deep_analysis(true);
    processor.set_max_levels(20);
    processor.enable_microstructure_features(true);

    let _start_time = Instant::now();

    // Configure connection for deep market data
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    // Multi-level processing handler
    let processor_clone = processor.clone();
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                // Process all 20 levels with performance optimizations
                if let Err(e) = processor_clone.process_multi_level(&symbol, &data, timestamp) {
                    debug!("Multi-level processing error: {}", e);
                }
            }
            _ => {}
        }
    };

    // Run test
    tokio::select! {
        result = connector.connect_public(message_handler) => {
            if let Err(e) = result {
                warn!("Connection failed: {}", e);
            }
        }
        _ = tokio::time::sleep(duration) => {
            info!("✅ Multi-level performance test completed");
        }
    }

    // Get comprehensive statistics
    let stats = processor.get_comprehensive_stats();
    
    Ok(PerformanceBenchmark {
        level: PerformanceLevel::MultiLevel,
        updates_processed: stats.total_updates,
        avg_latency_us: stats.avg_latency_us,
        p99_latency_us: stats.p99_latency_us,
        memory_usage_mb: stats.memory_usage_mb,
        cpu_efficiency: stats.cpu_efficiency,
    })
}

fn display_performance_comparison(results: &[PerformanceBenchmark]) {
    info!("📊 Performance Comparison Results");
    info!("=====================================");
    
    for result in results {
        info!("🔹 Performance Level: {:?}", result.level);
        info!("   Updates Processed: {}", result.updates_processed);
        info!("   Avg Latency: {:.1}μs", result.avg_latency_us);
        info!("   P99 Latency: {}μs", result.p99_latency_us);
        info!("   Memory Usage: {:.1}MB", result.memory_usage_mb);
        info!("   CPU Efficiency: {:.1}%", result.cpu_efficiency * 100.0);
        info!("   ───────────────────────────");
    }

    // Find best performing configuration
    let best_latency = results.iter()
        .min_by(|a, b| a.avg_latency_us.partial_cmp(&b.avg_latency_us).unwrap())
        .unwrap();
    
    let best_throughput = results.iter()
        .max_by_key(|r| r.updates_processed)
        .unwrap();

    info!("🏆 Performance Winners:");
    info!("   Best Latency: {:?} ({:.1}μs avg)", best_latency.level, best_latency.avg_latency_us);
    info!("   Best Throughput: {:?} ({} updates)", best_throughput.level, best_throughput.updates_processed);
    
    // Performance recommendations
    info!("💡 Recommendations:");
    if best_latency.level == PerformanceLevel::UltraHigh {
        info!("   ⚡ Use Ultra-High Performance for latency-critical applications");
    }
    if best_throughput.level == PerformanceLevel::MultiLevel {
        info!("   📊 Use Multi-Level for comprehensive market analysis");
    }
}

// Mock implementations for compilation
// In the real system, these would be complete implementations

struct HighPerfOrderbookManager {
    symbol: String,
}

impl HighPerfOrderbookManager {
    pub fn new(symbol: String) -> Result<Self> {
        Ok(Self { symbol })
    }
    
    pub fn process_high_perf_update(&mut self, _symbol: &str, _data: &serde_json::Value, _timestamp: u64) -> Result<()> {
        Ok(())
    }
    
    pub fn get_performance_stats(&self) -> PerformanceStats {
        PerformanceStats {
            total_updates: 1000,
            avg_latency_us: 50.0,
            p99_latency_us: 200,
            memory_usage_mb: 100.0,
            cpu_efficiency: 0.85,
        }
    }
}

struct UltraHighPerfOrderbookManager {
    symbol: String,
}

impl UltraHighPerfOrderbookManager {
    pub fn new(symbol: String) -> Result<Self> {
        Ok(Self { symbol })
    }
    
    pub fn enable_simd_processing(&self, _enabled: bool) {}
    pub fn enable_zero_copy_parsing(&self, _enabled: bool) {}
    pub fn set_cpu_affinity(&self, _core: u32) -> Result<()> { Ok(()) }
    
    pub fn process_ultra_fast(&self, _symbol: &str, _data: &serde_json::Value, _timestamp: u64) -> Result<()> {
        Ok(())
    }
    
    pub fn get_ultra_perf_stats(&self) -> PerformanceStats {
        PerformanceStats {
            total_updates: 1500,
            avg_latency_us: 25.0,
            p99_latency_us: 100,
            memory_usage_mb: 80.0,
            cpu_efficiency: 0.95,
        }
    }
}

struct MultiLevelOrderbookProcessor {
    symbol: String,
    max_levels: usize,
}

impl MultiLevelOrderbookProcessor {
    pub fn new(symbol: String, levels: usize) -> Result<Self> {
        Ok(Self { symbol, max_levels: levels })
    }
    
    pub fn enable_deep_analysis(&self, _enabled: bool) {}
    pub fn set_max_levels(&self, _levels: usize) {}
    pub fn enable_microstructure_features(&self, _enabled: bool) {}
    
    pub fn process_multi_level(&self, _symbol: &str, _data: &serde_json::Value, _timestamp: u64) -> Result<()> {
        Ok(())
    }
    
    pub fn get_comprehensive_stats(&self) -> PerformanceStats {
        PerformanceStats {
            total_updates: 800,
            avg_latency_us: 75.0,
            p99_latency_us: 300,
            memory_usage_mb: 150.0,
            cpu_efficiency: 0.80,
        }
    }
}

struct PerformanceStats {
    pub total_updates: u64,
    pub avg_latency_us: f64,
    pub p99_latency_us: u64,
    pub memory_usage_mb: f64,
    pub cpu_efficiency: f64,
}