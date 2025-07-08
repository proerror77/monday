/*!
 * Ultra Low Latency Benchmark for BTCUSDT DL Trading
 * 
 * 基準測試驗證 UltraThink 優化效果
 * 目標：P99 ≤ 1ms，P50 ≤ 300μs
 */

use rust_hft::utils::ultra_low_latency::*;
use rust_hft::ml::*;
use rust_hft::engine::*;
use anyhow::Result;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();
    
    info!("🚀 Ultra Low Latency Benchmark - BTCUSDT DL Trading");
    info!("目標：P99 ≤ 1ms，P50 ≤ 300μs");
    
    // 初始化執行器（綁定到 CPU 核心 2）
    let mut executor = UltraLowLatencyExecutor::new(2)?;
    info!("✅ 執行器初始化完成，綁定到 CPU 核心 2");
    
    // 預熱階段
    info!("🔥 開始預熱階段...");
    warmup_phase(&mut executor).await?;
    
    // 基準測試階段
    info!("📊 開始基準測試...");
    
    // 測試1：純 SIMD 特徵提取
    test_simd_feature_extraction(&mut executor).await?;
    
    // 測試2：端到端推理延遲
    test_end_to_end_inference(&mut executor).await?;
    
    // 測試3：高負載穩定性測試
    test_high_load_stability(&mut executor).await?;
    
    // 測試4：延遲分佈分析
    test_latency_distribution(&mut executor).await?;
    
    // 最終延遲統計
    let final_stats = executor.get_latency_stats();
    info!("📈 === 最終延遲統計 ===");
    info!("{}", final_stats.format());
    
    // 目標達成驗證
    if final_stats.meets_targets() {
        info!("🎯 ✅ 延遲目標達成！");
        info!("   P50: {}μs ≤ 300μs ✅", final_stats.p50_micros);
        info!("   P99: {}μs ≤ 1000μs ✅", final_stats.p99_micros);
    } else {
        warn!("⚠️ 延遲目標未達成");
        warn!("   P50: {}μs (目標: ≤300μs)", final_stats.p50_micros);
        warn!("   P99: {}μs (目標: ≤1000μs)", final_stats.p99_micros);
    }
    
    // 性能評分
    let performance_score = calculate_performance_score(&final_stats);
    info!("🏆 性能評分: {:.1}/10.0", performance_score);
    
    if performance_score >= 9.0 {
        info!("🥇 優秀！達到生產環境標準");
    } else if performance_score >= 7.0 {
        info!("🥈 良好！適用於大部分 HFT 場景");
    } else if performance_score >= 5.0 {
        info!("🥉 及格！需要進一步優化");
    } else {
        warn!("❌ 不及格！需要重大優化");
    }
    
    info!("✅ Ultra Low Latency Benchmark 完成");
    
    Ok(())
}

/// 預熱階段
async fn warmup_phase(executor: &mut UltraLowLatencyExecutor) -> Result<()> {
    let warmup_iterations = 10000;
    let test_data = generate_test_lob_data();
    
    info!("執行 {} 次預熱迭代...", warmup_iterations);
    
    for i in 0..warmup_iterations {
        let _ = executor.execute_ultra_fast_inference(&test_data)?;
        
        if i % 1000 == 0 {
            info!("預熱進度: {}/{}", i, warmup_iterations);
        }
    }
    
    // 清除預熱數據的延遲統計
    let _ = executor.get_latency_stats();
    
    info!("✅ 預熱完成");
    Ok(())
}

/// 測試 SIMD 特徵提取
async fn test_simd_feature_extraction(executor: &mut UltraLowLatencyExecutor) -> Result<()> {
    info!("🧪 測試1: SIMD 特徵提取性能");
    
    let test_data = generate_test_lob_data();
    let iterations = 50000;
    
    let start_time = Instant::now();
    for _ in 0..iterations {
        let _ = executor.execute_ultra_fast_inference(&test_data)?;
    }
    let elapsed = start_time.elapsed();
    
    let avg_latency_ns = elapsed.as_nanos() / iterations as u128;
    let avg_latency_us = avg_latency_ns / 1000;
    
    info!("SIMD 特徵提取結果:");
    info!("  - 總迭代: {}", iterations);
    info!("  - 總耗時: {:?}", elapsed);
    info!("  - 平均延遲: {}μs", avg_latency_us);
    info!("  - 吞吐量: {:.0} ops/sec", iterations as f64 / elapsed.as_secs_f64());
    
    if avg_latency_us <= 100 {
        info!("  ✅ SIMD 優化效果優秀");
    } else if avg_latency_us <= 200 {
        info!("  ✅ SIMD 優化效果良好");
    } else {
        warn!("  ⚠️ SIMD 優化效果一般");
    }
    
    Ok(())
}

/// 測試端到端推理延遲
async fn test_end_to_end_inference(executor: &mut UltraLowLatencyExecutor) -> Result<()> {
    info!("🧪 測試2: 端到端推理延遲");
    
    let test_data = generate_test_lob_data();
    let iterations = 100000;
    
    for i in 0..iterations {
        let _ = executor.execute_ultra_fast_inference(&test_data)?;
        
        if i % 10000 == 0 && i > 0 {
            let stats = executor.get_latency_stats();
            info!("進度 {}/{}: P50={}μs, P99={}μs", 
                  i, iterations, stats.p50_micros, stats.p99_micros);
        }
    }
    
    let stats = executor.get_latency_stats();
    info!("端到端推理結果:");
    info!("  - 總推理次數: {}", iterations);
    info!("  - P50延遲: {}μs (目標: ≤300μs)", stats.p50_micros);
    info!("  - P95延遲: {}μs", stats.p95_micros);
    info!("  - P99延遲: {}μs (目標: ≤1000μs)", stats.p99_micros);
    info!("  - 最大延遲: {}μs", stats.max_micros);
    
    Ok(())
}

/// 測試高負載穩定性
async fn test_high_load_stability(executor: &mut UltraLowLatencyExecutor) -> Result<()> {
    info!("🧪 測試3: 高負載穩定性測試");
    
    let test_data = generate_test_lob_data();
    let duration = Duration::from_secs(30);
    let start_time = Instant::now();
    let mut iteration_count = 0;
    
    while start_time.elapsed() < duration {
        let _ = executor.execute_ultra_fast_inference(&test_data)?;
        iteration_count += 1;
        
        // 每秒報告一次狀態
        if iteration_count % 100000 == 0 {
            let elapsed = start_time.elapsed().as_secs_f64();
            let throughput = iteration_count as f64 / elapsed;
            info!("高負載測試進行中: {:.0} ops/sec", throughput);
        }
    }
    
    let elapsed = start_time.elapsed();
    let throughput = iteration_count as f64 / elapsed.as_secs_f64();
    let stats = executor.get_latency_stats();
    
    info!("高負載穩定性結果:");
    info!("  - 測試時長: {:?}", elapsed);
    info!("  - 總推理次數: {}", iteration_count);
    info!("  - 平均吞吐量: {:.0} ops/sec", throughput);
    info!("  - 穩定性P99延遲: {}μs", stats.p99_micros);
    
    if throughput >= 1000000.0 {
        info!("  ✅ 高負載性能優秀 (>1M ops/sec)");
    } else if throughput >= 500000.0 {
        info!("  ✅ 高負載性能良好 (>500K ops/sec)");
    } else {
        warn!("  ⚠️ 高負載性能需要改進");
    }
    
    Ok(())
}

/// 測試延遲分佈
async fn test_latency_distribution(executor: &mut UltraLowLatencyExecutor) -> Result<()> {
    info!("🧪 測試4: 延遲分佈分析");
    
    let test_data = generate_test_lob_data();
    let iterations = 200000;
    let mut latencies = Vec::with_capacity(iterations);
    
    for _ in 0..iterations {
        let start = std::time::Instant::now();
        let _ = executor.execute_ultra_fast_inference(&test_data)?;
        let latency = start.elapsed().as_micros() as u64;
        latencies.push(latency);
    }
    
    latencies.sort_unstable();
    
    let len = latencies.len();
    let p1 = latencies[len * 1 / 100];
    let p5 = latencies[len * 5 / 100];
    let p10 = latencies[len * 10 / 100];
    let p25 = latencies[len * 25 / 100];
    let p50 = latencies[len * 50 / 100];
    let p75 = latencies[len * 75 / 100];
    let p90 = latencies[len * 90 / 100];
    let p95 = latencies[len * 95 / 100];
    let p99 = latencies[len * 99 / 100];
    let p999 = latencies[len * 999 / 1000];
    
    info!("延遲分佈分析 ({}次測量):", iterations);
    info!("  P1:   {}μs", p1);
    info!("  P5:   {}μs", p5);
    info!("  P10:  {}μs", p10);
    info!("  P25:  {}μs", p25);
    info!("  P50:  {}μs ← 目標: ≤300μs", p50);
    info!("  P75:  {}μs", p75);
    info!("  P90:  {}μs", p90);
    info!("  P95:  {}μs", p95);
    info!("  P99:  {}μs ← 目標: ≤1000μs", p99);
    info!("  P99.9:{}μs", p999);
    
    // 分析分佈特性
    let mean: f64 = latencies.iter().map(|&x| x as f64).sum::<f64>() / len as f64;
    let variance: f64 = latencies.iter()
        .map(|&x| (x as f64 - mean).powi(2))
        .sum::<f64>() / len as f64;
    let std_dev = variance.sqrt();
    
    info!("分佈統計:");
    info!("  - 均值: {:.1}μs", mean);
    info!("  - 標準差: {:.1}μs", std_dev);
    info!("  - 變異係數: {:.3}", std_dev / mean);
    
    // 尾部延遲分析
    let tail_threshold = 1000; // 1ms
    let tail_count = latencies.iter().filter(|&&x| x > tail_threshold).count();
    let tail_ratio = tail_count as f64 / len as f64 * 100.0;
    
    info!("尾部延遲分析:");
    info!("  - >1ms 次數: {} ({:.3}%)", tail_count, tail_ratio);
    
    if tail_ratio < 0.1 {
        info!("  ✅ 尾部延遲控制優秀");
    } else if tail_ratio < 1.0 {
        info!("  ✅ 尾部延遲控制良好");
    } else {
        warn!("  ⚠️ 尾部延遲需要改進");
    }
    
    Ok(())
}

/// 生成測試 LOB 數據
fn generate_test_lob_data() -> Vec<f64> {
    let mut data = Vec::with_capacity(64);
    
    // 模擬 L8 LOB 數據（16個價位 × 4個屬性）
    for i in 0..64 {
        let base_value = 50000.0 + (i as f64 * 10.0);
        let noise = (i as f64 * 0.123).sin() * 5.0;
        data.push(base_value + noise);
    }
    
    data
}

/// 計算性能評分
fn calculate_performance_score(stats: &LatencyStats) -> f64 {
    let mut score = 0.0;
    
    // P50 延遲評分 (30%)
    if stats.p50_micros <= 200 {
        score += 3.0;
    } else if stats.p50_micros <= 300 {
        score += 2.5;
    } else if stats.p50_micros <= 500 {
        score += 2.0;
    } else if stats.p50_micros <= 800 {
        score += 1.0;
    }
    
    // P99 延遲評分 (40%)
    if stats.p99_micros <= 800 {
        score += 4.0;
    } else if stats.p99_micros <= 1000 {
        score += 3.5;
    } else if stats.p99_micros <= 1500 {
        score += 2.5;
    } else if stats.p99_micros <= 2000 {
        score += 1.5;
    } else if stats.p99_micros <= 3000 {
        score += 1.0;
    }
    
    // 最大延遲評分 (20%)
    if stats.max_micros <= 2000 {
        score += 2.0;
    } else if stats.max_micros <= 5000 {
        score += 1.5;
    } else if stats.max_micros <= 10000 {
        score += 1.0;
    }
    
    // 延遲穩定性評分 (10%)
    let stability_ratio = stats.p99_micros as f64 / stats.p50_micros as f64;
    if stability_ratio <= 2.0 {
        score += 1.0;
    } else if stability_ratio <= 3.0 {
        score += 0.7;
    } else if stability_ratio <= 4.0 {
        score += 0.5;
    } else if stability_ratio <= 5.0 {
        score += 0.3;
    }
    
    score
}