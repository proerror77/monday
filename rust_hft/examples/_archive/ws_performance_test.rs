/*!
 * WebSocket 性能測試
 *
 * 測試重點：
 * - 連接建立延遲
 * - 消息接收吞吐量
 * - 網絡延遲分佈
 * - 連接穩定性
 * - 多連接並發處理
 */

use anyhow::Result;
use clap::Parser;
use rust_hft::{core::types::*, integrations::bitget_connector::*};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{info, warn};

// Helper function to get current time in microseconds
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

#[derive(Parser, Debug, Clone)]
#[command(about = "WebSocket 性能測試")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// 測試時長（秒）
    #[arg(short, long, default_value_t = 60)]
    duration: u64,

    /// 並發連接數
    #[arg(short, long, default_value_t = 1)]
    connections: usize,

    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,

    /// 報告間隔（秒）
    #[arg(long, default_value_t = 10)]
    report_interval: u64,
}

#[derive(Debug, Default)]
struct PerformanceMetrics {
    total_messages: AtomicU64,
    total_bytes: AtomicU64,
    connection_errors: AtomicU64,
    latency_sum_us: AtomicU64,
    latency_count: AtomicU64,
    min_latency_us: AtomicU64,
    max_latency_us: AtomicU64,
}

impl PerformanceMetrics {
    fn new() -> Self {
        Self {
            min_latency_us: AtomicU64::new(u64::MAX),
            ..Default::default()
        }
    }

    fn record_message(&self, size: usize, latency_us: u64) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        self.total_bytes.fetch_add(size as u64, Ordering::Relaxed);
        self.latency_sum_us.fetch_add(latency_us, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);

        // 更新最小延遲
        loop {
            let current_min = self.min_latency_us.load(Ordering::Relaxed);
            if latency_us >= current_min {
                break;
            }
            if self
                .min_latency_us
                .compare_exchange_weak(
                    current_min,
                    latency_us,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }

        // 更新最大延遲
        loop {
            let current_max = self.max_latency_us.load(Ordering::Relaxed);
            if latency_us <= current_max {
                break;
            }
            if self
                .max_latency_us
                .compare_exchange_weak(
                    current_max,
                    latency_us,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }
    }

    fn record_error(&self) {
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }

    fn get_stats(&self) -> (u64, u64, u64, f64, u64, u64) {
        let messages = self.total_messages.load(Ordering::Relaxed);
        let bytes = self.total_bytes.load(Ordering::Relaxed);
        let errors = self.connection_errors.load(Ordering::Relaxed);
        let latency_sum = self.latency_sum_us.load(Ordering::Relaxed);
        let latency_count = self.latency_count.load(Ordering::Relaxed);
        let min_latency = self.min_latency_us.load(Ordering::Relaxed);
        let max_latency = self.max_latency_us.load(Ordering::Relaxed);

        let avg_latency = if latency_count > 0 {
            latency_sum as f64 / latency_count as f64
        } else {
            0.0
        };

        (
            messages,
            bytes,
            errors,
            avg_latency,
            min_latency,
            max_latency,
        )
    }
}

async fn run_websocket_test(
    args: Args,
    connection_id: usize,
    metrics: Arc<PerformanceMetrics>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let symbol = args.symbol.clone();
    let verbose = args.verbose;

    let bitget_config = BitgetConfig::default();
    let mut connector = BitgetConnector::new(bitget_config);
    connector.add_subscription(symbol.clone(), BitgetChannel::Books5);
    connector.add_subscription(symbol.clone(), BitgetChannel::Trade);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let tx_clone = tx.clone();

    let message_handler = move |message: BitgetMessage| {
        let _ = tx_clone.send(message);
    };

    // 建立連接
    let connect_start = Instant::now();
    let connector_task = tokio::spawn(async move {
        if let Err(e) = connector.connect_public(message_handler).await {
            warn!("Connection {} failed: {}", connection_id, e);
        }
    });

    let connect_time = connect_start.elapsed();
    if verbose {
        info!(
            "Connection {} established in {:?}",
            connection_id, connect_time
        );
    }

    // 處理消息
    while !shutdown.load(Ordering::Relaxed) {
        tokio::select! {
            Some(message) = rx.recv() => {
                let receive_time = now_micros();

                match message {
                    BitgetMessage::OrderBook { timestamp, data, .. } => {
                        // 計算延遲
                        let latency_us = receive_time - timestamp;
                        let message_size = std::mem::size_of_val(&data);

                        metrics.record_message(message_size, latency_us);

                        if verbose && metrics.total_messages.load(Ordering::Relaxed) % 1000 == 0 {
                            info!("Connection {} processed {} messages",
                                  connection_id,
                                  metrics.total_messages.load(Ordering::Relaxed));
                        }
                    }
                    BitgetMessage::Trade { timestamp, data, .. } => {
                        let latency_us = receive_time - timestamp;
                        let message_size = std::mem::size_of_val(&data);
                        metrics.record_message(message_size, latency_us);
                    }
                    BitgetMessage::Ticker { timestamp, data, .. } => {
                        let latency_us = receive_time - timestamp;
                        let message_size = std::mem::size_of_val(&data);
                        metrics.record_message(message_size, latency_us);
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                // 定期檢查關閉信號
                continue;
            }
        }
    }

    if verbose {
        info!("Connection {} shutting down", connection_id);
    }

    connector_task.abort();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let metrics = Arc::new(PerformanceMetrics::new());
    let shutdown = Arc::new(AtomicBool::new(false));

    info!("🚀 WebSocket 性能測試開始");
    info!(
        "📊 測試參數: {} 連接, {} 秒, 交易對: {}",
        args.connections, args.duration, args.symbol
    );

    // 啟動測試任務
    let mut handles = Vec::new();
    for i in 0..args.connections {
        let args_clone = args.clone();
        let metrics_clone = metrics.clone();
        let shutdown_clone = shutdown.clone();

        let handle = tokio::spawn(async move {
            run_websocket_test(args_clone, i, metrics_clone, shutdown_clone).await
        });
        handles.push(handle);
    }

    // 報告任務
    let report_metrics = metrics.clone();
    let report_shutdown = shutdown.clone();
    let report_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(args.report_interval));
        let start_time = Instant::now();

        while !report_shutdown.load(Ordering::Relaxed) {
            interval.tick().await;

            let (messages, bytes, errors, avg_latency, min_latency, max_latency) =
                report_metrics.get_stats();

            let elapsed = start_time.elapsed().as_secs_f64();
            let msg_per_sec = messages as f64 / elapsed;
            let mb_per_sec = (bytes as f64 / elapsed) / (1024.0 * 1024.0);

            info!("📊 性能統計 ({:.1}s):", elapsed);
            info!("   消息: {} ({:.1}/s)", messages, msg_per_sec);
            info!(
                "   流量: {:.2} MB ({:.2} MB/s)",
                bytes as f64 / (1024.0 * 1024.0),
                mb_per_sec
            );
            info!(
                "   延遲: {:.1}μs (min: {}μs, max: {}μs)",
                avg_latency, min_latency, max_latency
            );
            info!("   錯誤: {}", errors);
            info!("   ──────────────────────────");
        }
    });

    // 等待測試完成
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    // 關閉所有連接
    info!("🔄 正在關閉連接...");
    shutdown.store(true, Ordering::Relaxed);

    // 等待所有任務完成
    for handle in handles {
        let _ = handle.await;
    }
    let _ = report_handle.await;

    // 最終報告
    let (messages, bytes, errors, avg_latency, min_latency, max_latency) = metrics.get_stats();

    let total_time = args.duration as f64;
    let msg_per_sec = messages as f64 / total_time;
    let mb_per_sec = (bytes as f64 / total_time) / (1024.0 * 1024.0);
    let error_rate = errors as f64 / messages as f64 * 100.0;

    info!("🎉 測試完成 - 最終報告:");
    info!("   ⏱️  總時間: {:.1}s", total_time);
    info!("   📨 總消息: {} ({:.1}/s)", messages, msg_per_sec);
    info!(
        "   📊 總流量: {:.2} MB ({:.2} MB/s)",
        bytes as f64 / (1024.0 * 1024.0),
        mb_per_sec
    );
    info!("   🚀 平均延遲: {:.1}μs", avg_latency);
    info!("   ⚡ 最小延遲: {}μs", min_latency);
    info!("   🔥 最大延遲: {}μs", max_latency);
    info!("   ❌ 錯誤率: {:.2}%", error_rate);
    info!("   🔗 連接數: {}", args.connections);

    // 性能評級
    let grade = if avg_latency < 100.0 && error_rate < 0.1 {
        "A+ 優秀"
    } else if avg_latency < 500.0 && error_rate < 1.0 {
        "A 良好"
    } else if avg_latency < 1000.0 && error_rate < 5.0 {
        "B 中等"
    } else {
        "C 需要優化"
    };

    info!("🏆 性能評級: {}", grade);

    Ok(())
}
// Archived legacy example; see grouped examples under 01_data/
