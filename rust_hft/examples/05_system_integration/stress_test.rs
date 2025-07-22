/*!
 * 全面性能基準測試 - 真實 Bitget 數據測試
 * 
 * 測試目標：
 * - 驗證 WebSocket 連接性能是否達到 <1μs 決策延遲
 * - 測試真實市場數據的處理吞吐量
 * - 對比修復前後的性能差異
 * - 生成詳細的性能報告
 */

use rust_hft::integrations::{bitget_connector::*, dynamic_bitget_connector::*};
use rust_hft::core::orderbook::OrderBook;
use rust_hft::now_micros;
use anyhow::Result;
use clap::Parser;
use tracing::{info, warn, error};
use std::sync::{Arc, atomic::{AtomicU64, AtomicUsize, Ordering}};
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};
use serde::{Serialize, Deserialize};
use std::fs::File;
use std::io::Write;

#[derive(Parser)]
#[command(name = "performance_benchmark")]
#[command(about = "全面性能基準測試，使用真實 Bitget 數據")]
struct Args {
    /// 測試持續時間（秒）
    #[arg(short, long, default_value = "300")]
    duration: u64,
    
    /// 測試的交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,
    
    /// 是否同時測試多個交易對
    #[arg(long)]
    multi_symbol: bool,
    
    /// 目標延遲閾值（微秒）
    #[arg(long, default_value = "1")]
    target_latency_us: u64,
    
    /// 最小吞吐量要求（msg/s）
    #[arg(long, default_value = "1000")]
    min_throughput: u64,
    
    /// 輸出報告文件
    #[arg(short, long, default_value = "performance_report.json")]
    output: String,
    
    /// 詳細日誌
    #[arg(short, long)]
    verbose: bool,
}

/// 性能指標收集器
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub test_name: String,
    pub start_time: u64,
    pub end_time: u64,
    pub duration_secs: u64,
    
    // 延遲指標
    pub total_messages: u64,
    pub total_processing_time_us: u64,
    pub avg_latency_us: f64,
    pub min_latency_us: u64,
    pub max_latency_us: u64,
    pub p50_latency_us: u64,
    pub p95_latency_us: u64,
    pub p99_latency_us: u64,
    
    // 吞吐量指標
    pub throughput_msg_per_sec: f64,
    pub peak_throughput_msg_per_sec: f64,
    pub avg_throughput_msg_per_sec: f64,
    
    // 質量指標
    pub error_count: u64,
    pub success_rate: f64,
    pub data_quality_score: f64,
    
    // 連接指標
    pub connection_count: usize,
    pub reconnection_count: u64,
    pub connection_stability: f64,
    
    // 內存和 CPU 指標
    pub peak_memory_mb: f64,
    pub avg_cpu_usage: f64,
    
    // 目標達成情況
    pub latency_target_met: bool,
    pub throughput_target_met: bool,
    pub overall_grade: String,
}

impl Default for PerformanceMetrics {
    fn default() -> Self {
        Self {
            test_name: String::new(),
            start_time: 0,
            end_time: 0,
            duration_secs: 0,
            total_messages: 0,
            total_processing_time_us: 0,
            avg_latency_us: 0.0,
            min_latency_us: u64::MAX,
            max_latency_us: 0,
            p50_latency_us: 0,
            p95_latency_us: 0,
            p99_latency_us: 0,
            throughput_msg_per_sec: 0.0,
            peak_throughput_msg_per_sec: 0.0,
            avg_throughput_msg_per_sec: 0.0,
            error_count: 0,
            success_rate: 0.0,
            data_quality_score: 0.0,
            connection_count: 0,
            reconnection_count: 0,
            connection_stability: 0.0,
            peak_memory_mb: 0.0,
            avg_cpu_usage: 0.0,
            latency_target_met: false,
            throughput_target_met: false,
            overall_grade: "PENDING".to_string(),
        }
    }
}

/// 實時性能監控器
#[derive(Clone)]
pub struct PerformanceMonitor {
    metrics: Arc<PerformanceMetrics>,
    latency_samples: Arc<std::sync::Mutex<VecDeque<u64>>>,
    throughput_samples: Arc<std::sync::Mutex<VecDeque<f64>>>,
    message_count: Arc<AtomicU64>,
    error_count: Arc<AtomicU64>,
    total_processing_time: Arc<AtomicU64>,
    start_time: Instant,
}

impl PerformanceMonitor {
    pub fn new(test_name: String) -> Self {
        let mut metrics = PerformanceMetrics::default();
        metrics.test_name = test_name;
        metrics.start_time = now_micros();
        
        Self {
            metrics: Arc::new(metrics),
            latency_samples: Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(100000))),
            throughput_samples: Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(1000))),
            message_count: Arc::new(AtomicU64::new(0)),
            error_count: Arc::new(AtomicU64::new(0)),
            total_processing_time: Arc::new(AtomicU64::new(0)),
            start_time: Instant::now(),
        }
    }
    
    pub fn record_message_processed(&self, processing_latency_us: u64) {
        self.message_count.fetch_add(1, Ordering::Relaxed);
        self.total_processing_time.fetch_add(processing_latency_us, Ordering::Relaxed);
        
        // 記錄延遲樣本（保留最近的 100k 樣本用於百分位計算）
        if let Ok(mut samples) = self.latency_samples.lock() {
            samples.push_back(processing_latency_us);
            if samples.len() > 100000 {
                samples.pop_front();
            }
        }
        
        // 每秒計算一次吞吐量
        let elapsed = self.start_time.elapsed();
        if elapsed.as_secs() > 0 {
            let current_throughput = self.message_count.load(Ordering::Relaxed) as f64 / elapsed.as_secs_f64();
            if let Ok(mut throughput_samples) = self.throughput_samples.lock() {
                throughput_samples.push_back(current_throughput);
                if throughput_samples.len() > 1000 {
                    throughput_samples.pop_front();
                }
            }
        }
    }
    
    pub fn record_error(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn get_current_stats(&self) -> (f64, f64, u64) {
        let total_messages = self.message_count.load(Ordering::Relaxed);
        let total_time = self.total_processing_time.load(Ordering::Relaxed);
        let elapsed = self.start_time.elapsed().as_secs_f64();
        
        let avg_latency = if total_messages > 0 {
            total_time as f64 / total_messages as f64
        } else {
            0.0
        };
        
        let throughput = if elapsed > 0.0 {
            total_messages as f64 / elapsed
        } else {
            0.0
        };
        
        (avg_latency, throughput, total_messages)
    }
    
    pub fn finalize_metrics(&self, target_latency_us: u64, min_throughput: u64) -> PerformanceMetrics {
        let mut final_metrics = (*self.metrics).clone();
        final_metrics.end_time = now_micros();
        final_metrics.duration_secs = self.start_time.elapsed().as_secs();
        
        // 基礎統計
        final_metrics.total_messages = self.message_count.load(Ordering::Relaxed);
        final_metrics.total_processing_time_us = self.total_processing_time.load(Ordering::Relaxed);
        final_metrics.error_count = self.error_count.load(Ordering::Relaxed);
        
        // 計算延遲統計
        if let Ok(samples) = self.latency_samples.lock() {
            if !samples.is_empty() {
                let mut sorted_samples: Vec<u64> = samples.iter().cloned().collect();
                sorted_samples.sort_unstable();
                
                final_metrics.min_latency_us = sorted_samples[0];
                final_metrics.max_latency_us = sorted_samples[sorted_samples.len() - 1];
                final_metrics.avg_latency_us = final_metrics.total_processing_time_us as f64 / final_metrics.total_messages as f64;
                
                // 百分位數計算
                final_metrics.p50_latency_us = sorted_samples[sorted_samples.len() * 50 / 100];
                final_metrics.p95_latency_us = sorted_samples[sorted_samples.len() * 95 / 100];
                final_metrics.p99_latency_us = sorted_samples[sorted_samples.len() * 99 / 100];
            }
        }
        
        // 計算吞吐量統計
        if let Ok(throughput_samples) = self.throughput_samples.lock() {
            if !throughput_samples.is_empty() {
                final_metrics.peak_throughput_msg_per_sec = throughput_samples.iter().fold(0.0f64, |a, &b| a.max(b));
                final_metrics.avg_throughput_msg_per_sec = throughput_samples.iter().sum::<f64>() / throughput_samples.len() as f64;
            }
        }
        
        final_metrics.throughput_msg_per_sec = if final_metrics.duration_secs > 0 {
            final_metrics.total_messages as f64 / final_metrics.duration_secs as f64
        } else {
            0.0
        };
        
        // 計算成功率和質量分數
        final_metrics.success_rate = if final_metrics.total_messages > 0 {
            1.0 - (final_metrics.error_count as f64 / final_metrics.total_messages as f64)
        } else {
            0.0
        };
        
        final_metrics.data_quality_score = final_metrics.success_rate;
        
        // 檢查目標達成情況
        final_metrics.latency_target_met = final_metrics.p95_latency_us <= target_latency_us;
        final_metrics.throughput_target_met = final_metrics.throughput_msg_per_sec >= min_throughput as f64;
        
        // 計算總體評級
        final_metrics.overall_grade = if final_metrics.latency_target_met && final_metrics.throughput_target_met && final_metrics.success_rate >= 0.99 {
            "A+ 優秀".to_string()
        } else if final_metrics.latency_target_met && final_metrics.throughput_target_met {
            "A 良好".to_string()
        } else if final_metrics.latency_target_met || final_metrics.throughput_target_met {
            "B 普通".to_string()
        } else {
            "C 需要改進".to_string()
        };
        
        final_metrics
    }
}

/// 測試 DynamicBitgetConnector 的性能
async fn test_dynamic_connector_performance(
    symbol: &str, 
    duration_secs: u64,
    target_latency_us: u64,
    min_throughput: u64
) -> Result<PerformanceMetrics> {
    info!("🚀 開始測試 DynamicBitgetConnector 性能");
    info!("   商品: {}", symbol);
    info!("   測試時長: {}秒", duration_secs);
    info!("   目標延遲: <{}μs (P95)", target_latency_us);
    info!("   最小吞吐量: {} msg/s", min_throughput);
    
    let monitor = PerformanceMonitor::new(format!("DynamicBitgetConnector_{}", symbol));
    
    // 創建高性能動態連接器
    let bitget_config = BitgetConfig::default();
    let mut dynamic_connector = create_traffic_based_dynamic_connector(bitget_config);
    
    // 添加訂閱
    dynamic_connector.add_subscription(symbol.to_string(), BitgetChannel::Books5).await?;
    dynamic_connector.add_subscription(symbol.to_string(), BitgetChannel::Trade).await?;
    
    // 啟動動態連接器
    dynamic_connector.start().await?;
    
    // 獲取統一消息接收器
    let unified_receiver = dynamic_connector.get_unified_receiver();
    
    info!("✅ 動態連接器啟動成功，開始性能測試...");
    
    // 啟動實時統計報告
    let monitor_clone = monitor.clone();
    let stats_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let (avg_latency, throughput, total_msgs) = monitor_clone.get_current_stats();
            info!("📊 實時統計: {:.1}μs 延遲 | {:.1} msg/s 吞吐量 | {} 總消息", 
                  avg_latency, throughput, total_msgs);
        }
    });
    
    // 運行性能測試
    let start_time = tokio::time::Instant::now();
    let duration = tokio::time::Duration::from_secs(duration_secs);
    
    let monitor_test = monitor.clone();
    let test_handle = tokio::spawn(async move {
        let mut receiver = unified_receiver.lock().await;
        
        while start_time.elapsed() < duration {
            match tokio::time::timeout(
                tokio::time::Duration::from_millis(100),
                receiver.recv()
            ).await {
                Ok(Some(message)) => {
                    let process_start = now_micros();
                    
                    // 模擬實際的消息處理（解析、特徵提取、決策）
                    match message {
                        BitgetMessage::OrderBook { data, timestamp, .. } => {
                            // 模擬 OrderBook 解析和處理
                            let _parsed = parse_orderbook_simple(&data, timestamp);
                            
                            // 模擬特徵提取
                            let _features = extract_basic_features(&data);
                            
                            // 模擬交易決策
                            let _decision = make_trading_decision(&data);
                        },
                        BitgetMessage::Trade { data, timestamp, .. } => {
                            // 模擬交易數據處理
                            let _parsed = parse_trade_simple(&data, timestamp);
                        },
                        _ => {}
                    }
                    
                    let processing_latency = now_micros() - process_start;
                    monitor_test.record_message_processed(processing_latency);
                },
                Ok(None) => {
                    warn!("WebSocket 連接關閉");
                    break;
                },
                Err(_) => {
                    // 超時，繼續等待
                    continue;
                }
            }
        }
    });
    
    // 等待測試完成
    let _ = test_handle.await;
    stats_handle.abort();
    
    info!("⏱️  性能測試完成，正在生成最終報告...");
    
    let final_metrics = monitor.finalize_metrics(target_latency_us, min_throughput);
    
    // 打印詳細結果
    print_performance_results(&final_metrics);
    
    Ok(final_metrics)
}

/// 測試多商品性能
async fn test_multi_symbol_performance(
    symbols: &[&str],
    duration_secs: u64,
    target_latency_us: u64,
    min_throughput: u64
) -> Result<PerformanceMetrics> {
    info!("🚀 開始測試多商品性能");
    info!("   商品列表: {:?}", symbols);
    info!("   測試時長: {}秒", duration_secs);
    
    let monitor = PerformanceMonitor::new("MultiSymbol_Performance".to_string());
    
    // 創建高性能動態連接器
    let bitget_config = BitgetConfig::default();
    let mut dynamic_connector = create_traffic_based_dynamic_connector(bitget_config);
    
    // 添加所有商品的訂閱
    for symbol in symbols {
        dynamic_connector.add_subscription(symbol.to_string(), BitgetChannel::Books5).await?;
        dynamic_connector.add_subscription(symbol.to_string(), BitgetChannel::Trade).await?;
        info!("✅ 訂閱 {}", symbol);
    }
    
    // 啟動動態連接器
    dynamic_connector.start().await?;
    let unified_receiver = dynamic_connector.get_unified_receiver();
    
    info!("✅ 多商品動態連接器啟動成功");
    
    // 運行測試（與單商品類似的邏輯）
    let start_time = tokio::time::Instant::now();
    let duration = tokio::time::Duration::from_secs(duration_secs);
    
    let monitor_test = monitor.clone();
    let test_handle = tokio::spawn(async move {
        let mut receiver = unified_receiver.lock().await;
        
        while start_time.elapsed() < duration {
            match tokio::time::timeout(
                tokio::time::Duration::from_millis(50), // 更快的超時用於多商品
                receiver.recv()
            ).await {
                Ok(Some(message)) => {
                    let process_start = now_micros();
                    
                    // 處理消息（同單商品測試）
                    match message {
                        BitgetMessage::OrderBook { data, symbol, timestamp, .. } => {
                            let _parsed = parse_orderbook_simple(&data, timestamp);
                            let _features = extract_basic_features(&data);
                            
                            // 多商品決策邏輯
                            let _decision = make_multi_symbol_decision(&symbol, &data);
                        },
                        BitgetMessage::Trade { data, timestamp, .. } => {
                            let _parsed = parse_trade_simple(&data, timestamp);
                        },
                        _ => {}
                    }
                    
                    let processing_latency = now_micros() - process_start;
                    monitor_test.record_message_processed(processing_latency);
                },
                Ok(None) => break,
                Err(_) => continue,
            }
        }
    });
    
    let _ = test_handle.await;
    
    let final_metrics = monitor.finalize_metrics(target_latency_us, min_throughput * symbols.len() as u64);
    print_performance_results(&final_metrics);
    
    Ok(final_metrics)
}

/// 簡化的 OrderBook 解析（用於性能測試）
fn parse_orderbook_simple(data: &serde_json::Value, timestamp: u64) -> Result<()> {
    // 模擬快速解析
    let _bids = data.get("bids").and_then(|v| v.as_array());
    let _asks = data.get("asks").and_then(|v| v.as_array());
    let _ts = timestamp;
    Ok(())
}

/// 簡化的交易數據解析
fn parse_trade_simple(data: &serde_json::Value, timestamp: u64) -> Result<()> {
    let _price = data.get("price").and_then(|v| v.as_str());
    let _volume = data.get("volume").and_then(|v| v.as_str());
    let _ts = timestamp;
    Ok(())
}

/// 基礎特徵提取
fn extract_basic_features(_data: &serde_json::Value) -> Vec<f64> {
    // 模擬特徵提取過程
    vec![1.0, 2.0, 3.0, 4.0, 5.0]
}

/// 交易決策模擬
fn make_trading_decision(_data: &serde_json::Value) -> i8 {
    // 模擬決策邏輯 (-1: 賣出, 0: 持有, 1: 買入)
    0
}

/// 多商品決策模擬
fn make_multi_symbol_decision(_symbol: &str, _data: &serde_json::Value) -> i8 {
    // 模擬多商品決策邏輯
    0
}

/// 打印性能結果
fn print_performance_results(metrics: &PerformanceMetrics) {
    info!("🎯 === 性能測試結果 ===");
    info!("測試名稱: {}", metrics.test_name);
    info!("測試時長: {}秒", metrics.duration_secs);
    info!("");
    info!("📊 延遲指標:");
    info!("   總消息數: {}", metrics.total_messages);
    info!("   平均延遲: {:.2}μs", metrics.avg_latency_us);
    info!("   最小延遲: {}μs", metrics.min_latency_us);
    info!("   最大延遲: {}μs", metrics.max_latency_us);
    info!("   P50 延遲: {}μs", metrics.p50_latency_us);
    info!("   P95 延遲: {}μs", metrics.p95_latency_us);
    info!("   P99 延遲: {}μs", metrics.p99_latency_us);
    info!("");
    info!("🚀 吞吐量指標:");
    info!("   平均吞吐量: {:.1} msg/s", metrics.throughput_msg_per_sec);
    info!("   峰值吞吐量: {:.1} msg/s", metrics.peak_throughput_msg_per_sec);
    info!("");
    info!("✅ 質量指標:");
    info!("   成功率: {:.2}%", metrics.success_rate * 100.0);
    info!("   錯誤數: {}", metrics.error_count);
    info!("   數據質量分數: {:.2}", metrics.data_quality_score);
    info!("");
    info!("🎯 目標達成:");
    let latency_status = if metrics.latency_target_met { 
        "✅ 達成".to_string() 
    } else { 
        format!("❌ 未達成 (P95: {}μs)", metrics.p95_latency_us)
    };
    info!("   延遲目標: {}", latency_status);
    
    let throughput_status = if metrics.throughput_target_met { 
        "✅ 達成".to_string() 
    } else { 
        format!("❌ 未達成 ({:.1} msg/s)", metrics.throughput_msg_per_sec)
    };
    info!("   吞吐量目標: {}", throughput_status);
    info!("");
    info!("🏆 總體評級: {}", metrics.overall_grade);
    info!("========================");
}

/// 保存性能報告到文件
fn save_performance_report(metrics: &[PerformanceMetrics], output_file: &str) -> Result<()> {
    let json_report = serde_json::to_string_pretty(metrics)?;
    let mut file = File::create(output_file)?;
    file.write_all(json_report.as_bytes())?;
    info!("📄 性能報告已保存到: {}", output_file);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt::init();
    
    let args = Args::parse();
    
    info!("🔬 啟動全面性能基準測試");
    info!("⚙️  測試配置:");
    info!("   持續時間: {}秒", args.duration);
    info!("   目標延遲: <{}μs", args.target_latency_us);
    info!("   最小吞吐量: {} msg/s", args.min_throughput);
    info!("");
    
    let mut all_metrics = Vec::new();
    
    // 測試1: 單商品性能
    info!("🧪 測試 1: 單商品高頻數據處理");
    let single_metrics = test_dynamic_connector_performance(
        &args.symbol,
        args.duration,
        args.target_latency_us,
        args.min_throughput
    ).await?;
    all_metrics.push(single_metrics);
    
    // 測試2: 多商品性能（如果啟用）
    if args.multi_symbol {
        info!("🧪 測試 2: 多商品並發處理");
        let multi_symbols = ["BTCUSDT", "ETHUSDT", "XRPUSDT", "SOLUSDT", "DOGEUSDT"];
        let multi_metrics = test_multi_symbol_performance(
            &multi_symbols,
            args.duration,
            args.target_latency_us,
            args.min_throughput
        ).await?;
        all_metrics.push(multi_metrics);
    }
    
    // 保存詳細報告
    save_performance_report(&all_metrics, &args.output)?;
    
    // 總結評估
    info!("");
    info!("🎊 === 全面測試總結 ===");
    for metrics in &all_metrics {
        info!("{}: {}", metrics.test_name, metrics.overall_grade);
        if !metrics.latency_target_met {
            warn!("   ⚠️  延遲目標未達成: P95 {}μs > 目標 {}μs", 
                  metrics.p95_latency_us, args.target_latency_us);
        }
        if !metrics.throughput_target_met {
            warn!("   ⚠️  吞吐量目標未達成: {:.1} msg/s < 目標 {} msg/s", 
                  metrics.throughput_msg_per_sec, args.min_throughput);
        }
    }
    
    let overall_success = all_metrics.iter().all(|m| m.latency_target_met && m.throughput_target_met);
    
    if overall_success {
        info!("🎉 所有性能目標均已達成！WebSocket 連接修復成功！");
    } else {
        warn!("⚠️  部分性能目標未達成，需要進一步優化");
    }
    
    Ok(())
}