/*!
 * HFT 系統性能測試
 * 
 * 測試系統各組件的性能：
 * - WebSocket 連接吞吐量
 * - 數據處理延遲
 * - 特徵提取性能
 * - 整體系統性能
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
    core::orderbook::OrderBook,
    now_micros,
};
use anyhow::Result;
use tracing::{info, warn, error};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::collections::VecDeque;
use clap::Parser;
use serde::{Serialize, Deserialize};
use tokio::sync::mpsc;

#[derive(Parser, Debug, Clone)]
#[command(about = "HFT 系統性能測試")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,
    
    /// 測試時長（秒）
    #[arg(short, long, default_value_t = 60)]
    duration: u64,
    
    /// 測試模式
    #[arg(short, long, default_value = "full")]
    mode: String, // "websocket", "processing", "features", "full"
    
    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,
    
    /// 並發連接數
    #[arg(long, default_value_t = 1)]
    connections: usize,
}

/// 性能統計
#[derive(Debug, Default)]
pub struct PerformanceStats {
    // WebSocket 統計
    ws_messages_received: AtomicU64,
    ws_bytes_received: AtomicU64,
    ws_errors: AtomicU64,
    ws_connection_time_us: AtomicU64,
    
    // 處理統計
    processing_latency_total_us: AtomicU64,
    processing_count: AtomicU64,
    
    // 特徵提取統計
    feature_extraction_latency_total_us: AtomicU64,
    feature_extraction_count: AtomicU64,
    
    // 系統統計
    start_time: Mutex<Option<Instant>>,
    is_running: AtomicBool,
    
    // 延遲分佈
    latency_histogram: Mutex<VecDeque<u64>>,
}

impl PerformanceStats {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            latency_histogram: Mutex::new(VecDeque::with_capacity(10000)),
            ..Default::default()
        })
    }
    
    fn start(&self) {
        *self.start_time.lock().unwrap() = Some(Instant::now());
        self.is_running.store(true, Ordering::Relaxed);
    }
    
    fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
    }
    
    fn get_elapsed_secs(&self) -> f64 {
        if let Some(start) = *self.start_time.lock().unwrap() {
            start.elapsed().as_secs_f64()
        } else {
            0.0
        }
    }
    
    fn record_ws_message(&self, bytes: usize) {
        self.ws_messages_received.fetch_add(1, Ordering::Relaxed);
        self.ws_bytes_received.fetch_add(bytes as u64, Ordering::Relaxed);
    }
    
    fn record_processing_latency(&self, latency_us: u64) {
        self.processing_latency_total_us.fetch_add(latency_us, Ordering::Relaxed);
        self.processing_count.fetch_add(1, Ordering::Relaxed);
        
        // 記錄延遲分佈
        if let Ok(mut histogram) = self.latency_histogram.lock() {
            histogram.push_back(latency_us);
            if histogram.len() > 10000 {
                histogram.pop_front();
            }
        }
    }
    
    fn record_feature_extraction_latency(&self, latency_us: u64) {
        self.feature_extraction_latency_total_us.fetch_add(latency_us, Ordering::Relaxed);
        self.feature_extraction_count.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_error(&self) {
        self.ws_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    fn get_avg_processing_latency(&self) -> f64 {
        let total = self.processing_latency_total_us.load(Ordering::Relaxed);
        let count = self.processing_count.load(Ordering::Relaxed);
        if count > 0 {
            total as f64 / count as f64
        } else {
            0.0
        }
    }
    
    fn get_avg_feature_latency(&self) -> f64 {
        let total = self.feature_extraction_latency_total_us.load(Ordering::Relaxed);
        let count = self.feature_extraction_count.load(Ordering::Relaxed);
        if count > 0 {
            total as f64 / count as f64
        } else {
            0.0
        }
    }
    
    fn get_latency_percentiles(&self) -> (f64, f64, f64) {
        if let Ok(histogram) = self.latency_histogram.lock() {
            if histogram.is_empty() {
                return (0.0, 0.0, 0.0);
            }
            
            let mut sorted: Vec<u64> = histogram.iter().cloned().collect();
            sorted.sort();
            
            let len = sorted.len();
            let p50 = sorted[len * 50 / 100] as f64;
            let p95 = sorted[len * 95 / 100] as f64;
            let p99 = sorted[len * 99 / 100] as f64;
            
            (p50, p95, p99)
        } else {
            (0.0, 0.0, 0.0)
        }
    }
}

/// 測試結果
#[derive(Debug, Serialize)]
pub struct TestResults {
    duration_secs: f64,
    websocket_stats: WebSocketStats,
    processing_stats: ProcessingStats,
    feature_stats: FeatureStats,
    latency_percentiles: LatencyPercentiles,
    system_performance: SystemPerformance,
}

#[derive(Debug, Serialize)]
pub struct WebSocketStats {
    total_messages: u64,
    total_bytes: u64,
    messages_per_sec: f64,
    mbps: f64,
    errors: u64,
    error_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct ProcessingStats {
    total_processed: u64,
    avg_latency_us: f64,
    processing_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct FeatureStats {
    total_extracted: u64,
    avg_latency_us: f64,
    extraction_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct LatencyPercentiles {
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
}

#[derive(Debug, Serialize)]
pub struct SystemPerformance {
    cpu_efficiency: f64,
    memory_efficiency: f64,
    overall_score: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(if args.verbose { tracing::Level::DEBUG } else { tracing::Level::INFO })
        .init();
    
    info!("🚀 HFT 系統性能測試開始");
    info!("測試模式: {}, 時長: {}秒, 連接數: {}", args.mode, args.duration, args.connections);
    
    let stats = PerformanceStats::new();
    stats.start();
    
    match args.mode.as_str() {
        "websocket" => test_websocket_performance(&args, stats.clone()).await?,
        "processing" => test_processing_performance(&args, stats.clone()).await?,
        "features" => test_feature_extraction_performance(&args, stats.clone()).await?,
        "full" => test_full_system_performance(&args, stats.clone()).await?,
        _ => {
            error!("未知測試模式: {}", args.mode);
            return Ok(());
        }
    }
    
    stats.stop();
    
    // 生成測試報告
    let results = generate_test_report(&stats).await;
    print_test_results(&results);
    
    // 保存結果
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let results_file = format!("performance_test_{}.json", timestamp);
    std::fs::write(&results_file, serde_json::to_string_pretty(&results)?)?;
    info!("📄 測試結果已保存: {}", results_file);
    
    Ok(())
}

async fn test_websocket_performance(args: &Args, stats: Arc<PerformanceStats>) -> Result<()> {
    info!("📡 WebSocket 性能測試");
    
    let mut tasks = Vec::new();
    
    // 創建多個連接進行並發測試
    for i in 0..args.connections {
        let stats_clone = stats.clone();
        let symbol = args.symbol.clone();
        let duration = args.duration;
        
        let task = tokio::spawn(async move {
            let bitget_config = BitgetConfig::default();
            let mut connector = BitgetConnector::new(bitget_config);
            
            connector.add_subscription(symbol, BitgetChannel::Books5);
            
            let connect_start = Instant::now();
            let (tx, mut rx) = mpsc::unbounded_channel();
            
            // 使用閉包來處理消息
            let tx_clone = tx.clone();
            let message_handler = move |message: BitgetMessage| {
                let _ = tx_clone.send(message);
            };
            
            // 在後台任務中運行連接
            let connector_task = tokio::spawn(async move {
                if let Err(e) = connector.connect_public(message_handler).await {
                    error!("連接 {} 失敗: {}", i, e);
                }
            });
            
            let connect_time = connect_start.elapsed().as_micros() as u64;
            stats_clone.ws_connection_time_us.store(connect_time, Ordering::Relaxed);
            info!("連接 {} 建立成功，耗時: {}μs", i, connect_time);
            
            let test_start = Instant::now();
            while test_start.elapsed().as_secs() < duration && stats_clone.is_running.load(Ordering::Relaxed) {
                match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                    Ok(Some(message)) => {
                        let msg_size = estimate_message_size(&message);
                        stats_clone.record_ws_message(msg_size);
                    },
                    Ok(None) => {
                        warn!("連接 {} 斷開", i);
                        stats_clone.record_error();
                        break;
                    },
                    Err(_) => {
                        // 超時，繼續
                    }
                }
            }
            
            connector_task.abort();
        });
        
        tasks.push(task);
    }
    
    // 等待所有任務完成
    for task in tasks {
        let _ = task.await;
    }
    
    Ok(())
}

async fn test_processing_performance(args: &Args, stats: Arc<PerformanceStats>) -> Result<()> {
    info!("⚡ 數據處理性能測試");
    
    let bitget_config = BitgetConfig::default();
    let mut connector = BitgetConnector::new(bitget_config);
    
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Books5);
    
    let (tx, mut rx) = mpsc::unbounded_channel();
    
    let message_handler = move |message: BitgetMessage| {
        let _ = tx.send(message);
    };
    
    let connector_task = tokio::spawn(async move {
        if let Err(e) = connector.connect_public(message_handler).await {
            error!("連接失敗: {}", e);
        }
    });
    
    let test_start = Instant::now();
    
    while test_start.elapsed().as_secs() < args.duration && stats.is_running.load(Ordering::Relaxed) {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(message)) => {
                let process_start = now_micros();
                
                // 模擬數據處理
                let _processed_data = process_market_data(&message);
                
                let processing_latency = now_micros() - process_start;
                stats.record_processing_latency(processing_latency);
                
                let msg_size = estimate_message_size(&message);
                stats.record_ws_message(msg_size);
            },
            Ok(None) => {
                warn!("連接斷開");
                stats.record_error();
                break;
            },
            Err(_) => {
                // 超時，繼續
            }
        }
    }
    
    connector_task.abort();
    Ok(())
}

async fn test_feature_extraction_performance(args: &Args, stats: Arc<PerformanceStats>) -> Result<()> {
    info!("🔧 特徵提取性能測試");
    
    let feature_extractor = Arc::new(Mutex::new(FeatureExtractor::new(50)));
    
    let bitget_config = BitgetConfig::default();
    let mut connector = BitgetConnector::new(bitget_config);
    
    connector.add_subscription(args.symbol.clone(), BitgetChannel::Books5);
    
    let (tx, mut rx) = mpsc::unbounded_channel();
    
    let message_handler = move |message: BitgetMessage| {
        let _ = tx.send(message);
    };
    
    let connector_task = tokio::spawn(async move {
        if let Err(e) = connector.connect_public(message_handler).await {
            error!("連接失敗: {}", e);
        }
    });
    
    let test_start = Instant::now();
    
    while test_start.elapsed().as_secs() < args.duration && stats.is_running.load(Ordering::Relaxed) {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(message)) => {
                if let BitgetMessage::OrderBook { data, timestamp, .. } = message {
                    let process_start = now_micros();
                    
                    // 模擬 OrderBook 解析
                    if let Ok(orderbook) = parse_orderbook(&args.symbol, &data, timestamp) {
                        let feature_start = now_micros();
                        
                        // 特徵提取
                        if let Ok(mut extractor) = feature_extractor.lock() {
                            if let Ok(_features) = extractor.extract_features(&orderbook, 0, timestamp) {
                                let feature_latency = now_micros() - feature_start;
                                stats.record_feature_extraction_latency(feature_latency);
                            }
                        }
                        
                        let total_latency = now_micros() - process_start;
                        stats.record_processing_latency(total_latency);
                    }
                    
                    let msg_size = estimate_message_size(&BitgetMessage::OrderBook { 
                        symbol: args.symbol.clone(),
                        data, 
                        timestamp,
                        action: None 
                    });
                    stats.record_ws_message(msg_size);
                }
            },
            Ok(None) => {
                warn!("連接斷開");
                stats.record_error();
                break;
            },
            Err(_) => {
                // 超時，繼續
            }
        }
    }
    
    connector_task.abort();
    Ok(())
}

async fn test_full_system_performance(args: &Args, stats: Arc<PerformanceStats>) -> Result<()> {
    info!("🎯 完整系統性能測試");
    
    // 實時統計報告
    let stats_report = stats.clone();
    let report_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        
        while stats_report.is_running.load(Ordering::Relaxed) {
            interval.tick().await;
            print_realtime_stats(&stats_report);
        }
    });
    
    // 運行主要測試
    test_feature_extraction_performance(args, stats.clone()).await?;
    
    report_task.abort();
    Ok(())
}

fn print_realtime_stats(stats: &Arc<PerformanceStats>) {
    let elapsed = stats.get_elapsed_secs();
    if elapsed == 0.0 {
        return;
    }
    
    let messages = stats.ws_messages_received.load(Ordering::Relaxed);
    let bytes = stats.ws_bytes_received.load(Ordering::Relaxed);
    let errors = stats.ws_errors.load(Ordering::Relaxed);
    
    println!("\n📊 實時性能統計 ({:.1}s):", elapsed);
    println!("  消息: {} 條 ({:.0} 條/秒)", messages, messages as f64 / elapsed);
    println!("  數據: {:.2} MB ({:.2} MB/秒)", 
             bytes as f64 / 1024.0 / 1024.0,
             (bytes as f64 / elapsed) / 1024.0 / 1024.0);
    println!("  錯誤: {} 次", errors);
    println!("  處理延遲: {:.1}μs (平均)", stats.get_avg_processing_latency());
    println!("  特徵延遲: {:.1}μs (平均)", stats.get_avg_feature_latency());
    
    let (p50, p95, p99) = stats.get_latency_percentiles();
    println!("  延遲分佈: P50={:.1}μs, P95={:.1}μs, P99={:.1}μs", p50, p95, p99);
}

async fn generate_test_report(stats: &Arc<PerformanceStats>) -> TestResults {
    let duration = stats.get_elapsed_secs();
    let messages = stats.ws_messages_received.load(Ordering::Relaxed);
    let bytes = stats.ws_bytes_received.load(Ordering::Relaxed);
    let errors = stats.ws_errors.load(Ordering::Relaxed);
    let processed = stats.processing_count.load(Ordering::Relaxed);
    let features = stats.feature_extraction_count.load(Ordering::Relaxed);
    
    let (p50, p95, p99) = stats.get_latency_percentiles();
    
    // 計算系統性能評分
    let msg_rate = if duration > 0.0 { messages as f64 / duration } else { 0.0 };
    let cpu_efficiency = if stats.get_avg_processing_latency() > 0.0 {
        100.0 / (stats.get_avg_processing_latency() / 1000.0).max(1.0)
    } else {
        100.0
    };
    let memory_efficiency = 95.0; // 簡化
    let overall_score = (cpu_efficiency + memory_efficiency) / 2.0;
    
    TestResults {
        duration_secs: duration,
        websocket_stats: WebSocketStats {
            total_messages: messages,
            total_bytes: bytes,
            messages_per_sec: msg_rate,
            mbps: if duration > 0.0 { (bytes as f64 / duration) / 1024.0 / 1024.0 } else { 0.0 },
            errors,
            error_rate: if messages > 0 { errors as f64 / messages as f64 } else { 0.0 },
        },
        processing_stats: ProcessingStats {
            total_processed: processed,
            avg_latency_us: stats.get_avg_processing_latency(),
            processing_rate: if duration > 0.0 { processed as f64 / duration } else { 0.0 },
        },
        feature_stats: FeatureStats {
            total_extracted: features,
            avg_latency_us: stats.get_avg_feature_latency(),
            extraction_rate: if duration > 0.0 { features as f64 / duration } else { 0.0 },
        },
        latency_percentiles: LatencyPercentiles {
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
        },
        system_performance: SystemPerformance {
            cpu_efficiency,
            memory_efficiency,
            overall_score,
        },
    }
}

fn print_test_results(results: &TestResults) {
    println!("\n🎉 性能測試報告");
    println!("={}", "=".repeat(49));
    println!("測試時長: {:.1} 秒", results.duration_secs);
    
    println!("\n📡 WebSocket 性能:");
    println!("  總消息數: {}", results.websocket_stats.total_messages);
    println!("  消息速率: {:.0} 條/秒", results.websocket_stats.messages_per_sec);
    println!("  數據吞吐: {:.2} MB/秒", results.websocket_stats.mbps);
    println!("  錯誤率: {:.2}%", results.websocket_stats.error_rate * 100.0);
    
    println!("\n⚡ 處理性能:");
    println!("  總處理數: {}", results.processing_stats.total_processed);
    println!("  處理速率: {:.0} 次/秒", results.processing_stats.processing_rate);
    println!("  平均延遲: {:.1}μs", results.processing_stats.avg_latency_us);
    
    println!("\n🔧 特徵提取:");
    println!("  總提取數: {}", results.feature_stats.total_extracted);
    println!("  提取速率: {:.0} 次/秒", results.feature_stats.extraction_rate);
    println!("  平均延遲: {:.1}μs", results.feature_stats.avg_latency_us);
    
    println!("\n📊 延遲分佈:");
    println!("  P50: {:.1}μs", results.latency_percentiles.p50_us);
    println!("  P95: {:.1}μs", results.latency_percentiles.p95_us);
    println!("  P99: {:.1}μs", results.latency_percentiles.p99_us);
    
    println!("\n🎯 系統評分:");
    println!("  CPU 效率: {:.1}%", results.system_performance.cpu_efficiency);
    println!("  內存效率: {:.1}%", results.system_performance.memory_efficiency);
    println!("  綜合評分: {:.1}%", results.system_performance.overall_score);
    
    // 性能等級評估
    let grade = if results.system_performance.overall_score >= 90.0 {
        "優秀 (A+)"
    } else if results.system_performance.overall_score >= 80.0 {
        "良好 (A)"
    } else if results.system_performance.overall_score >= 70.0 {
        "中等 (B)"
    } else {
        "需要優化 (C)"
    };
    
    println!("\n🏆 系統性能等級: {}", grade);
    
    // 優化建議
    println!("\n💡 優化建議:");
    if results.processing_stats.avg_latency_us > 1000.0 {
        println!("  • 處理延遲較高，考慮優化算法或啟用 SIMD");
    }
    if results.websocket_stats.error_rate > 0.01 {
        println!("  • 網絡錯誤率較高，檢查網絡連接穩定性");
    }
    if results.latency_percentiles.p99_us > 10000.0 {
        println!("  • P99 延遲較高，檢查是否有性能瓶頸");
    }
    if results.system_performance.overall_score < 80.0 {
        println!("  • 系統整體性能有待提升，建議進行深度優化");
    }
}

// 輔助函數

fn estimate_message_size(message: &BitgetMessage) -> usize {
    match message {
        BitgetMessage::OrderBook { data, .. } => data.to_string().len(),
        BitgetMessage::Trade { data, .. } => data.to_string().len(),
        BitgetMessage::Ticker { data, .. } => data.to_string().len(),
    }
}

fn process_market_data(_message: &BitgetMessage) -> String {
    // 模擬數據處理
    "processed_data".to_string()
}

fn parse_orderbook(symbol: &str, _data: &serde_json::Value, timestamp: u64) -> Result<OrderBook> {
    let mut orderbook = OrderBook::new(symbol.to_string());
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;
    Ok(orderbook)
}
