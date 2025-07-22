/*!
 * TorchScript 模型推理性能測試
 * 
 * 測試重點：
 * - 模型加載時間
 * - 推理延遲分佈
 * - 批量推理性能
 * - 內存使用效率
 * - 多模型並發推理
 */

use rust_hft::{
    ml::{FeatureExtractor, FeatureSet},
    core::types::*,
    utils::performance::now_micros,
};
use anyhow::Result;
use clap::Parser;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn, error};
use tokio::sync::mpsc;

#[derive(Parser, Debug, Clone)]
#[command(about = "TorchScript 模型推理性能測試")]
struct Args {
    /// 模型文件路径
    #[arg(short, long, default_value = "models/btc_model.pt")]
    model_path: String,
    
    /// 測試時長（秒）
    #[arg(short, long, default_value_t = 60)]
    duration: u64,
    
    /// 並發推理數
    #[arg(short, long, default_value_t = 1)]
    concurrency: usize,
    
    /// 批量大小
    #[arg(short, long, default_value_t = 1)]
    batch_size: usize,
    
    /// 推理頻率（次/秒）
    #[arg(short, long, default_value_t = 1000)]
    rate: usize,
    
    /// 特徵維度
    #[arg(long, default_value_t = 128)]
    feature_dim: usize,
    
    /// 預熱輪數
    #[arg(long, default_value_t = 100)]
    warmup_rounds: usize,
    
    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Default)]
struct InferenceMetrics {
    total_inferences: AtomicU64,
    total_batches: AtomicU64,
    inference_errors: AtomicU64,
    inference_latency_sum_us: AtomicU64,
    inference_latency_count: AtomicU64,
    min_latency_us: AtomicU64,
    max_latency_us: AtomicU64,
    feature_extraction_time_us: AtomicU64,
    model_loading_time_us: AtomicU64,
}

impl InferenceMetrics {
    fn new() -> Self {
        Self {
            min_latency_us: AtomicU64::new(u64::MAX),
            ..Default::default()
        }
    }
    
    fn record_inference(&self, batch_size: usize, latency_us: u64) {
        self.total_inferences.fetch_add(batch_size as u64, Ordering::Relaxed);
        self.total_batches.fetch_add(1, Ordering::Relaxed);
        self.inference_latency_sum_us.fetch_add(latency_us, Ordering::Relaxed);
        self.inference_latency_count.fetch_add(1, Ordering::Relaxed);
        
        // 更新最小延遲
        loop {
            let current_min = self.min_latency_us.load(Ordering::Relaxed);
            if latency_us >= current_min {
                break;
            }
            if self.min_latency_us.compare_exchange_weak(
                current_min, latency_us, Ordering::Relaxed, Ordering::Relaxed
            ).is_ok() {
                break;
            }
        }
        
        // 更新最大延遲
        loop {
            let current_max = self.max_latency_us.load(Ordering::Relaxed);
            if latency_us <= current_max {
                break;
            }
            if self.max_latency_us.compare_exchange_weak(
                current_max, latency_us, Ordering::Relaxed, Ordering::Relaxed
            ).is_ok() {
                break;
            }
        }
    }
    
    fn record_error(&self) {
        self.inference_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_feature_extraction(&self, time_us: u64) {
        self.feature_extraction_time_us.fetch_add(time_us, Ordering::Relaxed);
    }
    
    fn record_model_loading(&self, time_us: u64) {
        self.model_loading_time_us.store(time_us, Ordering::Relaxed);
    }
    
    fn get_stats(&self) -> (u64, u64, u64, f64, u64, u64, u64, u64) {
        let inferences = self.total_inferences.load(Ordering::Relaxed);
        let batches = self.total_batches.load(Ordering::Relaxed);
        let errors = self.inference_errors.load(Ordering::Relaxed);
        let latency_sum = self.inference_latency_sum_us.load(Ordering::Relaxed);
        let latency_count = self.inference_latency_count.load(Ordering::Relaxed);
        let min_latency = self.min_latency_us.load(Ordering::Relaxed);
        let max_latency = self.max_latency_us.load(Ordering::Relaxed);
        let feature_time = self.feature_extraction_time_us.load(Ordering::Relaxed);
        let loading_time = self.model_loading_time_us.load(Ordering::Relaxed);
        
        let avg_latency = if latency_count > 0 {
            latency_sum as f64 / latency_count as f64
        } else {
            0.0
        };
        
        (inferences, batches, errors, avg_latency, min_latency, max_latency, feature_time, loading_time)
    }
}

// 生成模擬訂單簿數據
fn generate_mock_orderbook(_symbol: &str) -> OrderBookSnapshot {
    use rust_hft::core::types::ToPrice;
    
    let base_price = 50000.0;
    let timestamp = now_micros();
    
    OrderBookSnapshot {
        timestamp,
        best_bid: (base_price - 1.0).to_price(),
        best_ask: (base_price + 1.0).to_price(),
        total_bid_volume: 10.5, // Sum of bid volumes
        total_ask_volume: 10.5, // Sum of ask volumes
        bid_levels: 5,
        ask_levels: 5,
        spread: 2.0, // ask - bid
    }
}

// 生成模擬特徵向量
fn generate_mock_features(dim: usize) -> Vec<f32> {
    (0..dim).map(|i| (i as f32).sin() * 0.1).collect()
}

async fn inference_worker(
    worker_id: usize,
    args: Args,
    metrics: Arc<InferenceMetrics>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    info!("Worker {}: 開始模型加載", worker_id);
    
    // 加載模型
    let load_start = Instant::now();
    let model_loader = ModelLoader::new(&args.model_path)?;
    let load_time = load_start.elapsed().as_micros() as u64;
    metrics.record_model_loading(load_time);
    
    if args.verbose {
        info!("Worker {}: 模型加載完成，耗時 {}μs", worker_id, load_time);
    }
    
    // 特徵提取器
    let mut feature_extractor = FeatureExtractor::new(100); // 使用100作為窗口大小
    
    // 預熱
    info!("Worker {}: 開始預熱 {} 輪", worker_id, args.warmup_rounds);
    for _ in 0..args.warmup_rounds {
        let mock_features = generate_mock_features(args.feature_dim);
        let _ = model_loader.predict_batch(&[mock_features])?;
    }
    
    // 計算推理間隔
    let inference_interval = Duration::from_micros(1_000_000 / args.rate as u64);
    let mut interval = tokio::time::interval(inference_interval);
    
    // 批量緩衝區
    let mut batch_buffer = Vec::with_capacity(args.batch_size);
    
    while !shutdown.load(Ordering::Relaxed) {
        interval.tick().await;
        
        // 生成模擬數據
        let orderbook = generate_mock_orderbook("BTCUSDT");
        
        // 特徵提取
        let feature_start = Instant::now();
        let features = match feature_extractor.extract_features(&orderbook, 0, now_micros()) {
            Ok(features) => features,
            Err(e) => {
                error!("Worker {}: 特徵提取失敗: {}", worker_id, e);
                metrics.record_error();
                continue;
            }
        };
        let feature_time = feature_start.elapsed().as_micros() as u64;
        metrics.record_feature_extraction(feature_time);
        
        // 添加到批次
        batch_buffer.push(features);
        
        // 批量推理
        if batch_buffer.len() >= args.batch_size {
            let inference_start = Instant::now();
            
            match model_loader.predict_batch(&batch_buffer) {
                Ok(predictions) => {
                    let inference_time = inference_start.elapsed().as_micros() as u64;
                    metrics.record_inference(batch_buffer.len(), inference_time);
                    
                    if args.verbose && metrics.total_inferences.load(Ordering::Relaxed) % 10000 == 0 {
                        info!("Worker {}: 完成 {} 次推理, 預測數量: {}", 
                              worker_id, 
                              metrics.total_inferences.load(Ordering::Relaxed),
                              predictions.len());
                    }
                }
                Err(e) => {
                    error!("Worker {}: 推理失敗: {}", worker_id, e);
                    metrics.record_error();
                }
            }
            
            batch_buffer.clear();
        }
    }
    
    // 處理剩餘批次
    if !batch_buffer.is_empty() {
        let inference_start = Instant::now();
        match model_loader.predict_batch(&batch_buffer) {
            Ok(_) => {
                let inference_time = inference_start.elapsed().as_micros() as u64;
                metrics.record_inference(batch_buffer.len(), inference_time);
            }
            Err(e) => {
                error!("Worker {}: 最終推理失敗: {}", worker_id, e);
                metrics.record_error();
            }
        }
    }
    
    info!("Worker {}: 推理工作完成", worker_id);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    let args = Args::parse();
    let metrics = Arc::new(InferenceMetrics::new());
    let shutdown = Arc::new(AtomicBool::new(false));
    
    info!("🚀 TorchScript 模型推理性能測試開始");
    info!("📊 測試參數:");
    info!("   模型路徑: {}", args.model_path);
    info!("   併發數: {}", args.concurrency);
    info!("   批量大小: {}", args.batch_size);
    info!("   推理頻率: {}/s", args.rate);
    info!("   特徵維度: {}", args.feature_dim);
    info!("   測試時長: {}s", args.duration);
    info!("   預熱輪數: {}", args.warmup_rounds);
    
    // 啟動推理工作線程
    let mut handles = Vec::new();
    for i in 0..args.concurrency {
        let args_clone = args.clone();
        let metrics_clone = metrics.clone();
        let shutdown_clone = shutdown.clone();
        
        let handle = tokio::spawn(async move {
            inference_worker(i, args_clone, metrics_clone, shutdown_clone).await
        });
        handles.push(handle);
    }
    
    // 統計報告任務
    let report_metrics = metrics.clone();
    let report_shutdown = shutdown.clone();
    let report_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let start_time = Instant::now();
        
        while !report_shutdown.load(Ordering::Relaxed) {
            interval.tick().await;
            
            let (inferences, batches, errors, avg_latency, min_latency, max_latency, feature_time, loading_time) = 
                report_metrics.get_stats();
            
            let elapsed = start_time.elapsed().as_secs_f64();
            let inferences_per_sec = inferences as f64 / elapsed;
            let batches_per_sec = batches as f64 / elapsed;
            let avg_feature_time = if inferences > 0 { feature_time as f64 / inferences as f64 } else { 0.0 };
            
            info!("📊 推理統計 ({:.1}s):", elapsed);
            info!("   推理: {} ({:.1}/s)", inferences, inferences_per_sec);
            info!("   批次: {} ({:.1}/s)", batches, batches_per_sec);
            info!("   延遲: {:.1}μs (min: {}μs, max: {}μs)", avg_latency, min_latency, max_latency);
            info!("   特徵: {:.1}μs", avg_feature_time);
            info!("   加載: {}μs", loading_time);
            info!("   錯誤: {}", errors);
            info!("   ──────────────────────────");
        }
    });
    
    // 等待測試完成
    tokio::time::sleep(Duration::from_secs(args.duration)).await;
    
    // 關閉所有工作線程
    info!("🔄 正在關閉推理工作線程...");
    shutdown.store(true, Ordering::Relaxed);
    
    // 等待所有任務完成
    for handle in handles {
        let _ = handle.await;
    }
    let _ = report_handle.await;
    
    // 最終統計報告
    let (inferences, batches, errors, avg_latency, min_latency, max_latency, feature_time, loading_time) = 
        metrics.get_stats();
    
    let total_time = args.duration as f64;
    let inferences_per_sec = inferences as f64 / total_time;
    let batches_per_sec = batches as f64 / total_time;
    let error_rate = if inferences > 0 { errors as f64 / inferences as f64 * 100.0 } else { 0.0 };
    let avg_feature_time = if inferences > 0 { feature_time as f64 / inferences as f64 } else { 0.0 };
    let throughput_per_worker = inferences_per_sec / args.concurrency as f64;
    
    info!("🎉 測試完成 - 最終報告:");
    info!("   ⏱️  總時間: {:.1}s", total_time);
    info!("   🔍 總推理: {} ({:.1}/s)", inferences, inferences_per_sec);
    info!("   📦 總批次: {} ({:.1}/s)", batches, batches_per_sec);
    info!("   🚀 平均延遲: {:.1}μs", avg_latency);
    info!("   ⚡ 最小延遲: {}μs", min_latency);
    info!("   🔥 最大延遲: {}μs", max_latency);
    info!("   🎯 特徵提取: {:.1}μs", avg_feature_time);
    info!("   📚 模型加載: {}μs", loading_time);
    info!("   ❌ 錯誤率: {:.2}%", error_rate);
    info!("   🔗 併發數: {}", args.concurrency);
    info!("   📏 批量大小: {}", args.batch_size);
    info!("   ⚙️  單工作線程吞吐量: {:.1}/s", throughput_per_worker);
    
    // 計算效率指標
    let total_processing_time = avg_latency + avg_feature_time;
    let efficiency = if total_processing_time > 0.0 { 
        (avg_latency / total_processing_time) * 100.0 
    } else { 
        0.0 
    };
    
    info!("   📈 推理效率: {:.1}%", efficiency);
    
    // 性能評級
    let grade = if avg_latency < 1000.0 && inferences_per_sec > 5000.0 && error_rate < 0.1 {
        "A+ 優秀"
    } else if avg_latency < 5000.0 && inferences_per_sec > 2000.0 && error_rate < 1.0 {
        "A 良好"
    } else if avg_latency < 10000.0 && inferences_per_sec > 1000.0 && error_rate < 5.0 {
        "B 中等"
    } else {
        "C 需要優化"
    };
    
    info!("🏆 性能評級: {}", grade);
    
    // 優化建議
    if avg_latency > 5000.0 {
        info!("💡 建議: 推理延遲較高，考慮使用更小的模型或量化優化");
    }
    if error_rate > 1.0 {
        info!("💡 建議: 錯誤率較高，檢查輸入數據格式和模型兼容性");
    }
    if efficiency < 80.0 {
        info!("💡 建議: 推理效率不足，優化特徵提取或增加批量大小");
    }
    
    Ok(())
}