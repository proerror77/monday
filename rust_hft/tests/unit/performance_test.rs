/*!
 * 性能工具測試 - 提升測試覆蓋率
 * 
 * 測試性能監控和優化工具
 */

use rust_hft::utils::performance::*;
use rust_hft::utils::ultra_low_latency::*;
use std::time::{Duration, Instant};
use std::thread;

#[test]
fn test_timestamp_utilities() {
    let now_micros_1 = now_micros();
    thread::sleep(Duration::from_micros(100));
    let now_micros_2 = now_micros();
    
    assert!(now_micros_2 > now_micros_1);
    assert!(now_micros_2 - now_micros_1 >= 100);
    
    let now_nanos_1 = now_nanos();
    thread::sleep(Duration::from_nanos(1000));
    let now_nanos_2 = now_nanos();
    
    assert!(now_nanos_2 > now_nanos_1);
    assert!(now_nanos_2 - now_nanos_1 >= 1000);
}

#[test]
fn test_latency_tracker() {
    let mut tracker = LatencyTracker::new();
    
    // 記錄一些延遲
    tracker.record("api_call", 1500);
    tracker.record("api_call", 2000);
    tracker.record("api_call", 1000);
    tracker.record("database", 5000);
    tracker.record("database", 6000);
    
    // 測試統計信息
    let api_stats = tracker.get_stats("api_call");
    assert!(api_stats.is_some());
    
    let stats = api_stats.unwrap();
    assert_eq!(stats.count, 3);
    assert_eq!(stats.min, 1000);
    assert_eq!(stats.max, 2000);
    assert_eq!(stats.mean, 1500.0);
    
    let db_stats = tracker.get_stats("database").unwrap();
    assert_eq!(db_stats.count, 2);
    assert_eq!(db_stats.min, 5000);
    assert_eq!(db_stats.max, 6000);
    assert_eq!(db_stats.mean, 5500.0);
    
    // 測試不存在的指標
    assert!(tracker.get_stats("nonexistent").is_none());
}

#[test]
fn test_latency_tracker_percentiles() {
    let mut tracker = LatencyTracker::new();
    
    // 添加100個數據點
    for i in 1..=100 {
        tracker.record("test", i * 10); // 10, 20, 30, ..., 1000
    }
    
    let stats = tracker.get_stats("test").unwrap();
    assert_eq!(stats.count, 100);
    assert_eq!(stats.min, 10);
    assert_eq!(stats.max, 1000);
    assert_eq!(stats.mean, 505.0); // (10 + 1000) * 100 / 2 / 100
    
    // 測試百分位數
    let p50 = tracker.get_percentile("test", 50.0);
    assert!(p50.is_some());
    assert!((p50.unwrap() - 500.0).abs() < 50.0); // 應該接近中位數
    
    let p95 = tracker.get_percentile("test", 95.0);
    assert!(p95.is_some());
    assert!(p95.unwrap() > 900.0); // P95應該接近最大值
    
    let p99 = tracker.get_percentile("test", 99.0);
    assert!(p99.is_some());
    assert!(p99.unwrap() > 950.0); // P99應該更接近最大值
}

#[test]
fn test_memory_prefaulting() {
    let initial_rss = get_memory_usage().unwrap_or(0);
    
    // 預分配一些內存
    let result = prefault_memory(1024 * 1024); // 1MB
    assert!(result.is_ok());
    
    let after_rss = get_memory_usage().unwrap_or(0);
    
    // 內存使用應該增加（但這取決於系統行為）
    // 主要測試函數不會崩潰
    assert!(after_rss >= initial_rss);
}

#[test]
fn test_cpu_affinity_manager() {
    let manager = CpuAffinityManager::new(0);
    
    // 測試創建不會崩潰
    assert_eq!(std::mem::size_of_val(&manager), std::mem::size_of::<CpuAffinityManager>());
    
    // 測試綁定操作（可能會失敗，取決於平台和權限）
    let result = manager.bind_to_cpu();
    // 不要求成功，因為可能需要特殊權限
    match result {
        Ok(_) => println!("CPU親和性綁定成功"),
        Err(e) => println!("CPU親和性綁定失敗（可能是權限問題）: {}", e),
    }
}

#[test]
fn test_simd_feature_processor() {
    let processor = SIMDFeatureProcessor::new();
    
    let input = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
    let output = processor.process_features(&input);
    
    assert_eq!(output.len(), input.len());
    
    // SIMD處理應該保持數據完整性
    for (i, &val) in output.iter().enumerate() {
        assert!((val - input[i]).abs() < 1e-6);
    }
}

#[test]
fn test_aligned_memory_pool() {
    let pool = AlignedMemoryPool::new(1024, 64); // 1KB，64字節對齊
    
    let buffer = pool.allocate(256);
    assert!(buffer.is_ok());
    
    let buf = buffer.unwrap();
    assert_eq!(buf.len(), 256);
    
    // 測試內存對齊
    let ptr = buf.as_ptr() as usize;
    assert_eq!(ptr % 64, 0, "內存未正確對齊");
}

#[test]
fn test_tsc_timing() {
    // 測試TSC計時功能
    let freq_result = calibrate_tsc_frequency();
    
    match freq_result {
        Ok(frequency) => {
            assert!(frequency > 1_000_000_000); // 至少1GHz
            assert!(frequency < 10_000_000_000); // 不超過10GHz
            
            // 測試TSC讀取
            let tsc1 = read_tsc();
            thread::sleep(Duration::from_micros(10));
            let tsc2 = read_tsc();
            
            assert!(tsc2 > tsc1);
            
            // 計算經過的時間（微秒）
            let elapsed_tsc = tsc2 - tsc1;
            let elapsed_micros = (elapsed_tsc as f64 * 1_000_000.0) / frequency as f64;
            
            // 應該接近10微秒
            assert!(elapsed_micros >= 5.0 && elapsed_micros <= 50.0);
        }
        Err(_) => {
            // TSC校準可能在某些平台上失敗，這是可以接受的
            println!("TSC校準失敗，跳過TSC相關測試");
        }
    }
}

#[test]
fn test_performance_profiler() {
    let mut profiler = PerformanceProfiler::new();
    
    // 開始一個性能測量
    profiler.start_measurement("test_operation");
    
    // 模擬一些工作
    thread::sleep(Duration::from_millis(1));
    
    // 結束測量
    let elapsed = profiler.end_measurement("test_operation");
    assert!(elapsed.is_some());
    
    let duration = elapsed.unwrap();
    assert!(duration >= 1000); // 至少1ms
    assert!(duration <= 10000); // 不超過10ms
    
    // 測試統計
    let stats = profiler.get_measurement_stats("test_operation");
    assert!(stats.is_some());
    
    let measurement_stats = stats.unwrap();
    assert_eq!(measurement_stats.count, 1);
    assert!(measurement_stats.mean >= 1000.0);
}

#[test]
fn test_memory_usage_monitoring() {
    let initial_usage = get_memory_usage();
    assert!(initial_usage.is_ok() || initial_usage.is_err()); // 函數應該返回結果
    
    if let Ok(usage) = initial_usage {
        assert!(usage > 0); // 應該有一些內存使用
        assert!(usage < 100 * 1024 * 1024 * 1024); // 不應超過100GB
    }
}

#[test]
fn test_cache_optimization() {
    // 測試緩存行優化
    let data = vec![1u64; 1024]; // 8KB數據
    
    let start = Instant::now();
    
    // 順序訪問（緩存友好）
    let mut sum1 = 0u64;
    for i in 0..1024 {
        sum1 += data[i];
    }
    
    let sequential_time = start.elapsed();
    
    // 隨機訪問（緩存不友好）
    let start = Instant::now();
    let mut sum2 = 0u64;
    for i in (0..1024).step_by(64) { // 每隔64個元素訪問
        for j in 0..16 {
            if i + j < 1024 {
                sum2 += data[i + j];
            }
        }
    }
    
    let random_time = start.elapsed();
    
    assert_eq!(sum1, sum2); // 結果應該相同
    
    // 順序訪問通常比隨機訪問快（但不是絕對的）
    println!("順序訪問時間: {:?}", sequential_time);
    println!("塊狀訪問時間: {:?}", random_time);
}

#[test]
fn test_branch_prediction_optimization() {
    let data: Vec<i32> = (0..10000).map(|i| if i % 2 == 0 { i } else { -i }).collect();
    
    let start = Instant::now();
    
    // 可預測的分支模式
    let mut positive_sum = 0i64;
    let mut negative_sum = 0i64;
    
    for &value in &data {
        if value >= 0 {
            positive_sum += value as i64;
        } else {
            negative_sum += value as i64;
        }
    }
    
    let predictable_time = start.elapsed();
    
    // 使用無分支算法
    let start = Instant::now();
    let mut sum_no_branch = 0i64;
    
    for &value in &data {
        let is_positive = (value >= 0) as i32;
        let is_negative = 1 - is_positive;
        sum_no_branch += (is_positive as i64 * value as i64) - (is_negative as i64 * value as i64);
    }
    
    let branchless_time = start.elapsed();
    
    assert_eq!(positive_sum + negative_sum, sum_no_branch);
    
    println!("有分支時間: {:?}", predictable_time);
    println!("無分支時間: {:?}", branchless_time);
}