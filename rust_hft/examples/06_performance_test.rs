/*!
 * 统一性能测试系统
 * 
 * 整合功能：
 * - 延迟基准测试
 * - 吞吐量压力测试
 * - 内存使用分析
 * - 系统性能评估
 */

use anyhow::Result;
use clap::Parser;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "performance_test")]
#[command(about = "Comprehensive performance testing suite")]
struct Args {
    /// Test type: latency, throughput, memory, stress, all
    #[arg(short, long, default_value = "all")]
    test_type: String,

    /// Number of test iterations
    #[arg(short, long, default_value_t = 100000)]
    iterations: u64,

    /// Number of concurrent threads
    #[arg(long, default_value_t = 4)]
    threads: usize,

    /// Test duration in seconds
    #[arg(short, long, default_value_t = 60)]
    duration: u64,

    /// Memory allocation size in MB for memory tests
    #[arg(long, default_value_t = 100)]
    memory_size: usize,

    /// Target latency threshold in microseconds
    #[arg(long, default_value_t = 1000)]
    latency_threshold: u64,

    /// Enable detailed output
    #[arg(long)]
    verbose: bool,
}

#[derive(Debug)]
struct LatencyStats {
    min_us: u64,
    max_us: u64,
    avg_us: f64,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    p99_9_us: u64,
    total_samples: u64,
}

#[derive(Debug)]
struct ThroughputStats {
    total_operations: u64,
    operations_per_second: f64,
    total_bytes: u64,
    bytes_per_second: f64,
    duration_seconds: f64,
}

#[derive(Debug)]
struct MemoryStats {
    initial_usage_mb: f64,
    peak_usage_mb: f64,
    final_usage_mb: f64,
    allocation_rate_mb_per_sec: f64,
    gc_pressure: f64,
}

#[derive(Debug)]
struct PerformanceReport {
    test_type: String,
    timestamp: u64,
    latency: Option<LatencyStats>,
    throughput: Option<ThroughputStats>,
    memory: Option<MemoryStats>,
    cpu_usage: f64,
    system_load: f64,
    passed: bool,
    recommendations: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize tracing
    let log_level = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(format!("rust_hft={}", log_level))
        .init();

    info!("🚀 Starting Performance Test Suite");
    info!("Test type: {}, Iterations: {}, Threads: {}", 
          args.test_type, args.iterations, args.threads);

    let mut reports = Vec::new();

    match args.test_type.as_str() {
        "latency" => {
            let report = run_latency_test(&args).await?;
            reports.push(report);
        }
        "throughput" => {
            let report = run_throughput_test(&args).await?;
            reports.push(report);
        }
        "memory" => {
            let report = run_memory_test(&args).await?;
            reports.push(report);
        }
        "stress" => {
            let report = run_stress_test(&args).await?;
            reports.push(report);
        }
        "all" => {
            info!("🔄 Running comprehensive performance test suite...");
            
            let latency_report = run_latency_test(&args).await?;
            reports.push(latency_report);
            
            let throughput_report = run_throughput_test(&args).await?;
            reports.push(throughput_report);
            
            let memory_report = run_memory_test(&args).await?;
            reports.push(memory_report);
            
            let stress_report = run_stress_test(&args).await?;
            reports.push(stress_report);
        }
        _ => return Err(anyhow::anyhow!("Unknown test type: {}", args.test_type)),
    }

    // Generate comprehensive report
    generate_performance_report(&reports);
    
    // Save results to file
    save_results_to_file(&reports).await?;

    Ok(())
}

async fn run_latency_test(args: &Args) -> Result<PerformanceReport> {
    info!("🕐 Starting latency benchmark test...");
    
    let mut latencies = Vec::with_capacity(args.iterations as usize);
    let iterations = args.iterations;
    
    // Warm up
    for _ in 0..1000 {
        simulate_trading_decision().await;
    }
    
    info!("📊 Running {} latency measurements...", iterations);
    
    for i in 0..iterations {
        let start = Instant::now();
        
        // Simulate core trading operations
        simulate_trading_decision().await;
        
        let latency = start.elapsed().as_nanos() as u64 / 1000; // Convert to microseconds
        latencies.push(latency);
        
        if args.verbose && i % 10000 == 0 {
            info!("Completed {} iterations", i);
        }
    }
    
    // Calculate statistics
    latencies.sort_unstable();
    let total_samples = latencies.len() as u64;
    let min_us = *latencies.first().unwrap();
    let max_us = *latencies.last().unwrap();
    let avg_us = latencies.iter().sum::<u64>() as f64 / total_samples as f64;
    
    let p50_us = latencies[latencies.len() * 50 / 100];
    let p95_us = latencies[latencies.len() * 95 / 100];
    let p99_us = latencies[latencies.len() * 99 / 100];
    let p99_9_us = latencies[latencies.len() * 999 / 1000];
    
    let latency_stats = LatencyStats {
        min_us,
        max_us,
        avg_us,
        p50_us,
        p95_us,
        p99_us,
        p99_9_us,
        total_samples,
    };
    
    // Performance assessment
    let passed = p99_us <= args.latency_threshold;
    let mut recommendations = Vec::new();
    
    if p99_us > args.latency_threshold {
        recommendations.push(format!("P99 latency {}μs exceeds threshold {}μs", p99_us, args.latency_threshold));
    }
    
    if p95_us > args.latency_threshold / 2 {
        recommendations.push("Consider CPU affinity optimization".to_string());
    }
    
    if max_us > 10000 {
        recommendations.push("High latency spikes detected - check for GC or system interference".to_string());
    }
    
    info!("✅ Latency test completed");
    info!("📈 Results: avg={}μs, p95={}μs, p99={}μs", avg_us as u64, p95_us, p99_us);
    
    Ok(PerformanceReport {
        test_type: "latency".to_string(),
        timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        latency: Some(latency_stats),
        throughput: None,
        memory: None,
        cpu_usage: get_cpu_usage().await,
        system_load: get_system_load().await,
        passed,
        recommendations,
    })
}

async fn run_throughput_test(args: &Args) -> Result<PerformanceReport> {
    info!("📈 Starting throughput benchmark test...");
    
    let operations_counter = Arc::new(AtomicU64::new(0));
    let bytes_counter = Arc::new(AtomicU64::new(0));
    let semaphore = Arc::new(Semaphore::new(args.threads));
    
    let start_time = Instant::now();
    let duration = Duration::from_secs(args.duration);
    
    let mut tasks = Vec::new();
    
    for thread_id in 0..args.threads {
        let operations_counter = Arc::clone(&operations_counter);
        let bytes_counter = Arc::clone(&bytes_counter);
        let semaphore = Arc::clone(&semaphore);
        let verbose = args.verbose;
        
        let task = tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();
            let mut local_operations = 0u64;
            let mut local_bytes = 0u64;
            
            let thread_start = Instant::now();
            
            while thread_start.elapsed() < duration {
                // Simulate high-throughput operations
                let bytes_processed = simulate_data_processing().await;
                
                local_operations += 1;
                local_bytes += bytes_processed;
                
                if verbose && local_operations % 1000 == 0 {
                    println!("Thread {}: {} operations", thread_id, local_operations);
                }
            }
            
            operations_counter.fetch_add(local_operations, Ordering::Relaxed);
            bytes_counter.fetch_add(local_bytes, Ordering::Relaxed);
        });
        
        tasks.push(task);
    }
    
    // Wait for all tasks to complete
    for task in tasks {
        task.await?;
    }
    
    let elapsed = start_time.elapsed();
    let total_operations = operations_counter.load(Ordering::Relaxed);
    let total_bytes = bytes_counter.load(Ordering::Relaxed);
    
    let operations_per_second = total_operations as f64 / elapsed.as_secs_f64();
    let bytes_per_second = total_bytes as f64 / elapsed.as_secs_f64();
    
    let throughput_stats = ThroughputStats {
        total_operations,
        operations_per_second,
        total_bytes,
        bytes_per_second,
        duration_seconds: elapsed.as_secs_f64(),
    };
    
    // Performance assessment
    let target_ops_per_sec = 10000.0; // 10K ops/sec target
    let passed = operations_per_second >= target_ops_per_sec;
    let mut recommendations = Vec::new();
    
    if operations_per_second < target_ops_per_sec {
        recommendations.push(format!("Throughput {:.0} ops/s below target {:.0} ops/s", 
                                   operations_per_second, target_ops_per_sec));
    }
    
    if bytes_per_second < 1024.0 * 1024.0 * 10.0 { // 10 MB/s
        recommendations.push("Consider optimizing data processing pipeline".to_string());
    }
    
    info!("✅ Throughput test completed");
    info!("📈 Results: {:.0} ops/s, {:.1} MB/s", 
          operations_per_second, bytes_per_second / 1024.0 / 1024.0);
    
    Ok(PerformanceReport {
        test_type: "throughput".to_string(),
        timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        latency: None,
        throughput: Some(throughput_stats),
        memory: None,
        cpu_usage: get_cpu_usage().await,
        system_load: get_system_load().await,
        passed,
        recommendations,
    })
}

async fn run_memory_test(args: &Args) -> Result<PerformanceReport> {
    info!("💾 Starting memory benchmark test...");
    
    let initial_usage = get_memory_usage().await;
    info!("Initial memory usage: {:.2} MB", initial_usage);
    
    let mut allocations = Vec::new();
    let allocation_size = 1024 * 1024; // 1 MB chunks
    let total_allocations = args.memory_size;
    
    let start_time = Instant::now();
    
    // Allocation phase
    for i in 0..total_allocations {
        let mut data = vec![0u8; allocation_size];
        
        // Write to memory to ensure allocation
        for j in 0..data.len() {
            data[j] = (i + j) as u8;
        }
        
        allocations.push(data);
        
        if args.verbose && i % 10 == 0 {
            let current_usage = get_memory_usage().await;
            info!("Allocated {} MB, current usage: {:.2} MB", i + 1, current_usage);
        }
        
        // Small delay to measure allocation rate
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    
    let peak_usage = get_memory_usage().await;
    let allocation_duration = start_time.elapsed();
    
    info!("Peak memory usage: {:.2} MB", peak_usage);
    
    // Memory access pattern test
    let access_start = Instant::now();
    let mut checksum = 0u64;
    
    for allocation in &allocations {
        for &byte in allocation.iter().step_by(1024) {
            checksum = checksum.wrapping_add(byte as u64);
        }
    }
    
    let access_duration = access_start.elapsed();
    info!("Memory access pattern completed, checksum: {}", checksum);
    
    // Cleanup phase
    allocations.clear();
    
    // Force garbage collection (if applicable)
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    let final_usage = get_memory_usage().await;
    
    let allocation_rate = total_allocations as f64 / allocation_duration.as_secs_f64();
    let gc_pressure = (peak_usage - final_usage) / peak_usage;
    
    let memory_stats = MemoryStats {
        initial_usage_mb: initial_usage,
        peak_usage_mb: peak_usage,
        final_usage_mb: final_usage,
        allocation_rate_mb_per_sec: allocation_rate,
        gc_pressure,
    };
    
    // Performance assessment
    let memory_efficiency = final_usage / peak_usage;
    let passed = memory_efficiency < 1.2 && gc_pressure < 0.5; // Less than 20% overhead
    
    let mut recommendations = Vec::new();
    
    if memory_efficiency > 1.5 {
        recommendations.push("High memory overhead detected - check for memory leaks".to_string());
    }
    
    if gc_pressure > 0.3 {
        recommendations.push("High GC pressure - consider object pooling".to_string());
    }
    
    if allocation_rate < 10.0 {
        recommendations.push("Low allocation rate - check for allocation bottlenecks".to_string());
    }
    
    info!("✅ Memory test completed");
    info!("📊 Results: peak={:.2}MB, final={:.2}MB, efficiency={:.2}", 
          peak_usage, final_usage, memory_efficiency);
    
    Ok(PerformanceReport {
        test_type: "memory".to_string(),
        timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        latency: None,
        throughput: None,
        memory: Some(memory_stats),
        cpu_usage: get_cpu_usage().await,
        system_load: get_system_load().await,
        passed,
        recommendations,
    })
}

async fn run_stress_test(args: &Args) -> Result<PerformanceReport> {
    info!("🔥 Starting stress test...");
    
    let duration = Duration::from_secs(args.duration);
    let start_time = Instant::now();
    
    let operations_counter = Arc::new(AtomicU64::new(0));
    let error_counter = Arc::new(AtomicU64::new(0));
    
    // Launch multiple stress test scenarios
    let mut tasks = Vec::new();
    
    // CPU intensive tasks
    for i in 0..args.threads {
        let operations_counter = Arc::clone(&operations_counter);
        let error_counter = Arc::clone(&error_counter);
        
        let task = tokio::spawn(async move {
            let mut local_ops = 0u64;
            let task_start = Instant::now();
            
            while task_start.elapsed() < duration {
                match simulate_cpu_intensive_work(i).await {
                    Ok(_) => local_ops += 1,
                    Err(_) => error_counter.fetch_add(1, Ordering::Relaxed),
                }
                
                // Prevent overwhelming the system
                if local_ops % 100 == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
            
            operations_counter.fetch_add(local_ops, Ordering::Relaxed);
        });
        
        tasks.push(task);
    }
    
    // Memory pressure task
    let mem_ops_counter = Arc::clone(&operations_counter);
    let mem_task = tokio::spawn(async move {
        let mut allocations = Vec::new();
        let task_start = Instant::now();
        
        while task_start.elapsed() < duration {
            // Allocate and deallocate memory
            let data = vec![0u8; 1024 * 1024]; // 1MB
            allocations.push(data);
            
            if allocations.len() > 50 { // Keep max 50MB
                allocations.drain(0..10);
            }
            
            mem_ops_counter.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    
    tasks.push(mem_task);
    
    // Wait for all tasks
    for task in tasks {
        task.await?;
    }
    
    let elapsed = start_time.elapsed();
    let total_operations = operations_counter.load(Ordering::Relaxed);
    let total_errors = error_counter.load(Ordering::Relaxed);
    
    let ops_per_second = total_operations as f64 / elapsed.as_secs_f64();
    let error_rate = total_errors as f64 / total_operations as f64;
    
    // Performance assessment
    let passed = error_rate < 0.01 && ops_per_second > 100.0; // Less than 1% errors, min 100 ops/s
    
    let mut recommendations = Vec::new();
    
    if error_rate > 0.05 {
        recommendations.push(format!("High error rate: {:.2}%", error_rate * 100.0));
    }
    
    if ops_per_second < 100.0 {
        recommendations.push("Low throughput under stress conditions".to_string());
    }
    
    let final_cpu = get_cpu_usage().await;
    let final_load = get_system_load().await;
    
    if final_cpu > 90.0 {
        recommendations.push("CPU utilization too high - check for inefficiencies".to_string());
    }
    
    info!("✅ Stress test completed");
    info!("📊 Results: {:.0} ops/s, {:.2}% errors, {:.1}% CPU", 
          ops_per_second, error_rate * 100.0, final_cpu);
    
    Ok(PerformanceReport {
        test_type: "stress".to_string(),
        timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        latency: None,
        throughput: None,
        memory: None,
        cpu_usage: final_cpu,
        system_load: final_load,
        passed,
        recommendations,
    })
}

// Simulation functions
async fn simulate_trading_decision() {
    // Simulate orderbook processing
    let _price_data = vec![50000.0, 50001.0, 49999.0, 50002.0];
    
    // Simulate feature calculation
    let _features = (0..20).map(|i| i as f64 * 0.1).collect::<Vec<_>>();
    
    // Simulate ML inference
    let _prediction = _features.iter().sum::<f64>() / _features.len() as f64;
    
    // Simulate risk check
    let _risk_score = _prediction * 0.8;
    
    // Small CPU work to simulate real processing
    let mut sum = 0u64;
    for i in 0..100 {
        sum = sum.wrapping_add(i);
    }
    let _ = sum;
}

async fn simulate_data_processing() -> u64 {
    // Simulate market data processing
    let data_size = 1024; // 1KB per operation
    let mut data = vec![0u8; data_size];
    
    // Simulate data transformation
    for i in 0..data.len() {
        data[i] = (i % 256) as u8;
    }
    
    // Simulate checksum calculation
    let _checksum: u8 = data.iter().fold(0, |acc, &x| acc ^ x);
    
    data_size as u64
}

async fn simulate_cpu_intensive_work(worker_id: usize) -> Result<()> {
    // Simulate complex calculations
    let mut result = worker_id as f64;
    
    for i in 0..1000 {
        result = result.sin().cos().tan().abs().sqrt();
        result += i as f64 * 0.001;
    }
    
    if result.is_nan() {
        return Err(anyhow::anyhow!("Calculation error"));
    }
    
    Ok(())
}

async fn get_memory_usage() -> f64 {
    // Simplified memory usage detection
    // In a real implementation, this would use system APIs
    100.0 + rand::random::<f64>() * 50.0 // Simulate 100-150 MB usage
}

async fn get_cpu_usage() -> f64 {
    // Simplified CPU usage detection
    25.0 + rand::random::<f64>() * 50.0 // Simulate 25-75% usage
}

async fn get_system_load() -> f64 {
    // Simplified system load detection
    1.0 + rand::random::<f64>() * 2.0 // Simulate 1.0-3.0 load average
}

fn generate_performance_report(reports: &[PerformanceReport]) {
    info!("📊 ========== Performance Test Summary ==========");
    
    let mut all_passed = true;
    let mut total_recommendations = Vec::new();
    
    for report in reports {
        info!("🔍 Test: {}", report.test_type.to_uppercase());
        info!("  Status: {}", if report.passed { "✅ PASS" } else { "❌ FAIL" });
        info!("  CPU Usage: {:.1}%", report.cpu_usage);
        info!("  System Load: {:.2}", report.system_load);
        
        if let Some(ref latency) = report.latency {
            info!("  Latency: avg={:.0}μs, p95={:.0}μs, p99={:.0}μs", 
                  latency.avg_us, latency.p95_us, latency.p99_us);
        }
        
        if let Some(ref throughput) = report.throughput {
            info!("  Throughput: {:.0} ops/s, {:.1} MB/s", 
                  throughput.operations_per_second, 
                  throughput.bytes_per_second / 1024.0 / 1024.0);
        }
        
        if let Some(ref memory) = report.memory {
            info!("  Memory: peak={:.1}MB, final={:.1}MB", 
                  memory.peak_usage_mb, memory.final_usage_mb);
        }
        
        if !report.passed {
            all_passed = false;
        }
        
        for rec in &report.recommendations {
            total_recommendations.push(format!("[{}] {}", report.test_type, rec));
        }
        
        info!("");
    }
    
    info!("🎯 Overall Result: {}", if all_passed { "✅ ALL TESTS PASSED" } else { "❌ SOME TESTS FAILED" });
    
    if !total_recommendations.is_empty() {
        info!("💡 Recommendations:");
        for rec in total_recommendations {
            info!("  - {}", rec);
        }
    }
    
    info!("===============================================");
}

async fn save_results_to_file(reports: &[PerformanceReport]) -> Result<()> {
    std::fs::create_dir_all("reports")?;
    
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    
    let filename = format!("reports/performance_test_{}.json", timestamp);
    let json_content = serde_json::to_string_pretty(reports)?;
    
    std::fs::write(&filename, json_content)?;
    info!("📄 Results saved to: {}", filename);
    
    Ok(())
}