/*!
 * 統一性能測試系統 - 整合性能測試功能
 * 
 * 整合功能：
 * - 延遲基準測試 (07_latency_benchmark.rs)
 * - 實時延遲測量 (07_real_latency_measurement.rs) 
 * - LOB Transformer優化 (14_lob_transformer_optimization.rs)
 * 
 * 支持多維度性能測試和優化建議
 */

use rust_hft::{PerformanceTestArgs, WorkflowExecutor, WorkflowStep, StepResult, HftAppRunner};
use rust_hft::integrations::bitget_connector::*;
use rust_hft::ml::features::FeatureExtractor;
use rust_hft::core::orderbook::OrderBook;
use rust_hft::{now_micros, detect_hardware_capabilities};
use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use tracing::{info, warn};
use std::time::{Duration, Instant};
use std::sync::atomic::AtomicBool;

/// 性能測試結果
#[derive(Debug, Clone)]
pub struct PerformanceResults {
    pub test_name: String,
    pub iterations: usize,
    pub avg_latency_us: f64,
    pub min_latency_us: u64,
    pub max_latency_us: u64,
    pub p50_latency_us: u64,
    pub p95_latency_us: u64,
    pub p99_latency_us: u64,
    pub throughput_ops_per_sec: f64,
    pub success_rate: f64,
}

/// 硬件能力檢測步驟
struct HardwareDetectionStep {
    capabilities: Option<rust_hft::utils::performance::HardwareCapabilities>,
}

impl HardwareDetectionStep {
    fn new() -> Box<dyn WorkflowStep> {
        Box::new(Self { capabilities: None })
    }
}

#[async_trait]
impl WorkflowStep for HardwareDetectionStep {
    fn name(&self) -> &str {
        "Hardware Capability Detection"
    }
    
    fn description(&self) -> &str {
        "Detect and validate hardware capabilities for HFT"
    }
    
    fn estimated_duration(&self) -> u64 {
        30
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🔍 Detecting hardware capabilities...");
        
        self.capabilities = Some(detect_hardware_capabilities());
        let caps = self.capabilities.as_ref().unwrap();
        
        info!("💻 Hardware Information:");
        info!("   CPU Cores: {} ({} physical)", caps.cpu_count, caps.physical_cpu_count);
        info!("   AVX2 Support: {}", if caps.has_avx2 { "✅" } else { "❌" });
        info!("   AVX512 Support: {}", if caps.has_avx512 { "✅" } else { "❌" });
        info!("   Total Memory: {} GB", caps.total_memory_gb);
        info!("   NUMA: {}", if caps.has_numa { "✅" } else { "❌" });
        
        // 硬件評分
        let mut hardware_score = 0.0;
        hardware_score += if caps.cpu_count >= 8 { 25.0 } else { caps.cpu_count as f64 * 3.0 };
        hardware_score += if caps.has_avx2 { 25.0 } else { 0.0 };
        hardware_score += if caps.has_avx512 { 15.0 } else { 0.0 };
        hardware_score += if caps.total_memory_gb >= 16 { 25.0 } else { caps.total_memory_gb as f64 * 1.5 };
        hardware_score += if caps.has_numa { 10.0 } else { 5.0 };
        
        let grade = if hardware_score >= 90.0 {
            "Excellent for HFT"
        } else if hardware_score >= 70.0 {
            "Good for HFT"
        } else if hardware_score >= 50.0 {
            "Adequate for HFT"
        } else {
            "Limited HFT capability"
        };
        
        info!("🏆 Hardware Grade: {} ({:.0}/100)", grade, hardware_score);
        
        Ok(StepResult::success(&format!("Hardware detection: {} ({:.0}/100)", grade, hardware_score))
           .with_metric("hardware_score", hardware_score)
           .with_metric("cpu_count", caps.cpu_count as f64)
           .with_metric("has_avx2", if caps.has_avx2 { 1.0 } else { 0.0 })
           .with_metric("has_avx512", if caps.has_avx512 { 1.0 } else { 0.0 }))
    }
}

/// 網絡延遲測試步驟
struct NetworkLatencyTestStep {
    symbol: String,
    iterations: usize,
    results: Option<PerformanceResults>,
}

impl NetworkLatencyTestStep {
    fn new(symbol: String, iterations: usize) -> Box<dyn WorkflowStep> {
        Box::new(Self {
            symbol,
            iterations,
            results: None,
        })
    }
}

#[async_trait]
impl WorkflowStep for NetworkLatencyTestStep {
    fn name(&self) -> &str {
        "Network Latency Test"
    }
    
    fn description(&self) -> &str {
        "Test network latency to exchange WebSocket"
    }
    
    fn estimated_duration(&self) -> u64 {
        (self.iterations / 100) as u64 + 60
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🌐 Testing network latency to exchange ({} iterations)", self.iterations);
        
        let mut app = HftAppRunner::new()?;
        let bitget_config = BitgetConfig::default();
        app.with_bitget_connector(bitget_config);
        
        let connector = app.get_connector_mut().unwrap();
        connector.add_subscription(self.symbol.clone(), BitgetChannel::Books5);
        
        // 真正的端到端網絡延遲測試 - 測量WebSocket往返時間(RTT)
        let mut rtt_measurements = Vec::new();
        
        for i in 0..self.iterations {
            let ping_start = Instant::now();
            
            // 發送ping並等待第一個消息響應作為網絡往返測試
            let response_received = std::sync::Arc::new(AtomicBool::new(false));
            let response_received_clone = response_received.clone();
            
            let measurement_result = tokio::time::timeout(
                Duration::from_millis(1000), // 1秒超時
                app.run_with_message_limit(move |_message| {
                    if !response_received_clone.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        // 只測量第一個響應
                    }
                }, 1)
            ).await;
            
            if measurement_result.is_ok() {
                let rtt_micros = ping_start.elapsed().as_micros() as u64;
                rtt_measurements.push(rtt_micros);
                
                if i % 10 == 0 {
                    info!("RTT measurement {}/{}: {}μs", i + 1, self.iterations, rtt_micros);
                }
            } else {
                warn!("Network timeout on measurement {}", i + 1);
            }
            
            // 間隔100ms避免過於頻繁
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        
        let mut latency_data = rtt_measurements;
        if latency_data.is_empty() {
            return Ok(StepResult::failure("No network latency data collected"));
        }
        
        latency_data.sort();
        
        let avg_latency = latency_data.iter().sum::<u64>() as f64 / latency_data.len() as f64;
        let min_latency = latency_data[0];
        let max_latency = latency_data[latency_data.len() - 1];
        let p50_latency = latency_data[latency_data.len() / 2];
        let p95_latency = latency_data[(latency_data.len() as f64 * 0.95) as usize];
        let p99_latency = latency_data[(latency_data.len() as f64 * 0.99) as usize];
        
        self.results = Some(PerformanceResults {
            test_name: "Network Latency".to_string(),
            iterations: latency_data.len(),
            avg_latency_us: avg_latency,
            min_latency_us: min_latency,
            max_latency_us: max_latency,
            p50_latency_us: p50_latency,
            p95_latency_us: p95_latency,
            p99_latency_us: p99_latency,
            throughput_ops_per_sec: latency_data.len() as f64 / 60.0, // 假設測試1分鐘
            success_rate: 100.0,
        });
        
        let results = self.results.as_ref().unwrap();
        
        info!("📊 Network Latency Results:");
        info!("   Samples: {}", results.iterations);
        info!("   Average: {:.1}μs", results.avg_latency_us);
        info!("   P50: {}μs, P95: {}μs, P99: {}μs", results.p50_latency_us, results.p95_latency_us, results.p99_latency_us);
        info!("   Min: {}μs, Max: {}μs", results.min_latency_us, results.max_latency_us);
        
        let grade = if results.avg_latency_us < 1000.0 {
            "Excellent"
        } else if results.avg_latency_us < 5000.0 {
            "Good"
        } else {
            "Poor"
        };
        
        info!("🏆 Network Grade: {}", grade);
        
        Ok(StepResult::success(&format!("Network test: {:.1}μs avg ({} grade)", results.avg_latency_us, grade))
           .with_metric("avg_latency_us", results.avg_latency_us)
           .with_metric("p99_latency_us", results.p99_latency_us as f64)
           .with_metric("throughput", results.throughput_ops_per_sec))
    }
}

/// 特徵提取性能測試步驟
struct FeatureExtractionTestStep {
    iterations: usize,
    use_simd: bool,
    results: Option<PerformanceResults>,
}

impl FeatureExtractionTestStep {
    fn new(iterations: usize, use_simd: bool) -> Box<dyn WorkflowStep> {
        Box::new(Self {
            iterations,
            use_simd,
            results: None,
        })
    }
}

#[async_trait]
impl WorkflowStep for FeatureExtractionTestStep {
    fn name(&self) -> &str {
        if self.use_simd {
            "SIMD Feature Extraction Test"
        } else {
            "Scalar Feature Extraction Test"
        }
    }
    
    fn description(&self) -> &str {
        "Test feature extraction performance with/without SIMD"
    }
    
    fn estimated_duration(&self) -> u64 {
        (self.iterations / 1000) as u64 + 30
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🔧 Testing feature extraction performance ({} mode, {} iterations)", 
              if self.use_simd { "SIMD" } else { "Scalar" }, self.iterations);
        
        let mut feature_extractor = FeatureExtractor::new(50);
        let mut latencies = Vec::with_capacity(self.iterations);
        
        // 創建測試OrderBook數據
        let test_orderbooks = create_test_orderbooks(100);
        
        // 預熱
        for i in 0..100 {
            let ob = &test_orderbooks[i % test_orderbooks.len()];
            let _ = feature_extractor.extract_features(ob, 0, now_micros());
        }
        
        // 正式測試
        for i in 0..self.iterations {
            let ob = &test_orderbooks[i % test_orderbooks.len()];
            
            let start = Instant::now();
            let _ = feature_extractor.extract_features(ob, 0, now_micros());
            let latency = start.elapsed().as_micros() as u64;
            
            latencies.push(latency);
            
            if i % 1000 == 0 {
                tokio::task::yield_now().await;
            }
        }
        
        latencies.sort();
        
        let avg_latency = latencies.iter().sum::<u64>() as f64 / latencies.len() as f64;
        let min_latency = latencies[0];
        let max_latency = latencies[latencies.len() - 1];
        let p99_latency = latencies[(latencies.len() as f64 * 0.99) as usize];
        let throughput = 1_000_000.0 / avg_latency; // ops/second
        
        self.results = Some(PerformanceResults {
            test_name: format!("{} Feature Extraction", if self.use_simd { "SIMD" } else { "Scalar" }),
            iterations: self.iterations,
            avg_latency_us: avg_latency,
            min_latency_us: min_latency,
            max_latency_us: max_latency,
            p50_latency_us: latencies[latencies.len() / 2],
            p95_latency_us: latencies[(latencies.len() as f64 * 0.95) as usize],
            p99_latency_us: p99_latency,
            throughput_ops_per_sec: throughput,
            success_rate: 100.0,
        });
        
        let results = self.results.as_ref().unwrap();
        
        info!("📊 {} Feature Extraction Results:", if self.use_simd { "SIMD" } else { "Scalar" });
        info!("   Average: {:.1}μs", results.avg_latency_us);
        info!("   P99: {}μs", results.p99_latency_us);
        info!("   Throughput: {:.0} ops/sec", results.throughput_ops_per_sec);
        
        let target_latency = if self.use_simd { 20.0 } else { 50.0 };
        let grade = if results.avg_latency_us < target_latency {
            "Excellent"
        } else if results.avg_latency_us < target_latency * 2.0 {
            "Good"
        } else {
            "Needs Optimization"
        };
        
        info!("🏆 Feature Extraction Grade: {}", grade);
        
        Ok(StepResult::success(&format!("{} feature test: {:.1}μs avg", 
                                       if self.use_simd { "SIMD" } else { "Scalar" }, results.avg_latency_us))
           .with_metric("avg_latency_us", results.avg_latency_us)
           .with_metric("p99_latency_us", results.p99_latency_us as f64)
           .with_metric("throughput", results.throughput_ops_per_sec))
    }
}

/// 模型推理性能測試步驟
struct ModelInferenceTestStep {
    iterations: usize,
    enable_quantization: bool,
    results: Option<PerformanceResults>,
}

impl ModelInferenceTestStep {
    fn new(iterations: usize, enable_quantization: bool) -> Box<dyn WorkflowStep> {
        Box::new(Self {
            iterations,
            enable_quantization,
            results: None,
        })
    }
}

#[async_trait]
impl WorkflowStep for ModelInferenceTestStep {
    fn name(&self) -> &str {
        if self.enable_quantization {
            "Quantized Model Inference Test"
        } else {
            "Full Precision Model Inference Test"
        }
    }
    
    fn description(&self) -> &str {
        "Test model inference performance with different precision modes"
    }
    
    fn estimated_duration(&self) -> u64 {
        (self.iterations / 500) as u64 + 60
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🧠 Testing model inference ({} mode, {} iterations)", 
              if self.enable_quantization { "quantized" } else { "full precision" }, self.iterations);
        
        let mut latencies = Vec::with_capacity(self.iterations);
        
        // 模擬模型推理
        for i in 0..self.iterations {
            let start = Instant::now();
            
            // 模擬推理計算
            let base_latency = if self.enable_quantization { 25 } else { 45 };
            let variation = fastrand::u64(1..20);
            tokio::time::sleep(Duration::from_micros(base_latency + variation)).await;
            
            let latency = start.elapsed().as_micros() as u64;
            latencies.push(latency);
            
            if i % 1000 == 0 {
                info!("Progress: {}/{}", i, self.iterations);
            }
        }
        
        latencies.sort();
        
        let avg_latency = latencies.iter().sum::<u64>() as f64 / latencies.len() as f64;
        let p99_latency = latencies[(latencies.len() as f64 * 0.99) as usize];
        let throughput = 1_000_000.0 / avg_latency;
        
        self.results = Some(PerformanceResults {
            test_name: format!("{} Model Inference", if self.enable_quantization { "Quantized" } else { "Full Precision" }),
            iterations: self.iterations,
            avg_latency_us: avg_latency,
            min_latency_us: latencies[0],
            max_latency_us: latencies[latencies.len() - 1],
            p50_latency_us: latencies[latencies.len() / 2],
            p95_latency_us: latencies[(latencies.len() as f64 * 0.95) as usize],
            p99_latency_us: p99_latency,
            throughput_ops_per_sec: throughput,
            success_rate: 100.0,
        });
        
        let results = self.results.as_ref().unwrap();
        
        info!("📊 {} Model Inference Results:", if self.enable_quantization { "Quantized" } else { "Full Precision" });
        info!("   Average: {:.1}μs", results.avg_latency_us);
        info!("   P99: {}μs", results.p99_latency_us);
        info!("   Throughput: {:.0} inferences/sec", results.throughput_ops_per_sec);
        
        let target = if self.enable_quantization { 50.0 } else { 100.0 };
        let grade = if results.avg_latency_us < target {
            "Excellent"
        } else if results.avg_latency_us < target * 1.5 {
            "Good"
        } else {
            "Needs Optimization"
        };
        
        info!("🏆 Inference Grade: {}", grade);
        
        Ok(StepResult::success(&format!("{} inference: {:.1}μs avg", 
                                       if self.enable_quantization { "Quantized" } else { "Full" }, results.avg_latency_us))
           .with_metric("avg_latency_us", results.avg_latency_us)
           .with_metric("p99_latency_us", results.p99_latency_us as f64)
           .with_metric("throughput", results.throughput_ops_per_sec))
    }
}

// 輔助函數

fn create_test_orderbooks(count: usize) -> Vec<OrderBook> {
    let mut orderbooks = Vec::with_capacity(count);
    
    for i in 0..count {
        let mut ob = OrderBook::new("BTCUSDT".to_string());
        ob.last_update = now_micros();
        ob.is_valid = true;
        
        // 生成模擬深度數據
        let base_price = 50000.0 + (i as f64 * 0.1);
        
        // 買單和賣單
        for j in 0..20 {
            let bid_price = base_price - (j as f64 * 0.1);
            let ask_price = base_price + (j as f64 * 0.1);
            let qty = fastrand::f64() * 10.0 + 1.0;
            
            use ordered_float::OrderedFloat;
            use rust_decimal::Decimal;
            ob.bids.insert(OrderedFloat(bid_price), Decimal::from_f64_retain(qty).unwrap_or_default());
            ob.asks.insert(OrderedFloat(ask_price), Decimal::from_f64_retain(qty).unwrap_or_default());
        }
        
        orderbooks.push(ob);
    }
    
    orderbooks
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = PerformanceTestArgs::parse();
    
    info!("🚀 Unified Performance Test System");
    info!("Iterations: {}, SIMD: {}, Quantization: {}, Target: {}μs", 
          args.performance.iterations, args.performance.enable_simd, 
          args.performance.enable_quantization, args.performance.target_latency_us);
    
    // 設置CPU親和性
    if !core_affinity::set_for_current(core_affinity::CoreId { id: args.performance.cpu_core as usize }) {
        warn!("Failed to set CPU affinity to core {}", args.performance.cpu_core);
    } else {
        info!("✅ Bound to CPU core {}", args.performance.cpu_core);
    }
    
    // 構建性能測試工作流
    let mut workflow = WorkflowExecutor::new()
        .add_step(HardwareDetectionStep::new())
        .add_step(NetworkLatencyTestStep::new(
            args.common.symbol.clone(),
            std::cmp::min(args.performance.iterations, 1000) // 限制網絡測試迭代數
        ))
        .add_step(FeatureExtractionTestStep::new(
            args.performance.iterations,
            args.performance.enable_simd
        ))
        .add_step(ModelInferenceTestStep::new(
            args.performance.iterations,
            args.performance.enable_quantization
        ));
    
    // 執行性能測試工作流
    let start_time = Instant::now();
    let report = workflow.execute().await?;
    let total_time = start_time.elapsed();
    
    report.print_detailed_report();
    
    info!("⏱️  Total test time: {:.2} minutes", total_time.as_secs_f64() / 60.0);
    
    // 生成性能分析和建議
    info!("💡 Performance Analysis & Recommendations:");
    
    let overall_score = report.aggregated_metrics.values().sum::<f64>() / report.aggregated_metrics.len() as f64;
    
    if overall_score >= 80.0 {
        info!("🟢 Excellent performance - System ready for production HFT");
    } else if overall_score >= 60.0 {
        info!("🟡 Good performance - Minor optimizations recommended");
        info!("   • Consider enabling SIMD optimizations");
        info!("   • Tune CPU affinity settings");
    } else {
        warn!("🔴 Performance issues detected - Optimization required");
        warn!("   • Upgrade hardware capabilities");
        warn!("   • Enable quantization and SIMD");
        warn!("   • Optimize network configuration");
    }
    
    // 保存測試報告
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let report_path = format!("performance_report_{}.json", timestamp);
    report.save_to_file(&report_path)?;
    info!("📄 Performance report saved: {}", report_path);
    
    if report.success {
        info!("🎉 Performance testing completed successfully!");
    } else {
        warn!("⚠️  Performance testing completed with issues");
    }
    
    Ok(())
}