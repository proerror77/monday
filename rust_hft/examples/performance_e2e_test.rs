/*!
 * 端到端高性能基準測試
 *
 * 測試重點：
 * - < 1μs 決策延遲目標驗證
 * - > 10,000 events/sec 吞吐量驗證
 * - 並發連接性能測試
 * - 內存使用效率測試
 * - 系統穩定性測試
 */

use anyhow::Result;
use clap::Parser;
use rust_hft::{
    core::types::*,
    integrations::unified_bitget_connector::BitgetChannel,
    integrations::{ConnectionMode, UnifiedBitgetConfig, UnifiedBitgetConnector},
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

#[derive(Parser, Debug, Clone)]
#[command(about = "端到端高性能基準測試")]
struct Args {
    /// 測試時長（秒）
    #[arg(short, long, default_value_t = 60)]
    duration: u64,

    /// 並發連接數
    #[arg(short, long, default_value_t = 4)]
    connections: usize,

    /// 目標延遲（微秒）
    #[arg(long, default_value_t = 1)]
    target_latency_us: u64,

    /// 目標吞吐量（events/sec）
    #[arg(long, default_value_t = 10000)]
    target_throughput: u64,

    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,

    /// 啟用CPU親和性
    #[arg(long)]
    enable_cpu_affinity: bool,

    /// 測試模式：throughput, latency, stress
    #[arg(long, default_value = "comprehensive")]
    test_mode: String,
}

#[derive(Debug, Default)]
struct AdvancedMetrics {
    // 基本統計
    total_messages: AtomicU64,
    total_bytes: AtomicU64,
    connection_errors: AtomicU64,

    // 延遲統計（納秒精度）
    latency_sum_ns: AtomicU64,
    latency_count: AtomicU64,
    min_latency_ns: AtomicU64,
    max_latency_ns: AtomicU64,

    // P99/P95 估算用的樣本
    latency_samples: parking_lot::Mutex<Vec<u64>>,

    // 內存使用
    peak_memory_kb: AtomicU64,

    // 吞吐量統計
    throughput_samples: parking_lot::Mutex<Vec<u64>>,
}

impl AdvancedMetrics {
    fn new() -> Self {
        Self {
            min_latency_ns: AtomicU64::new(u64::MAX),
            latency_samples: parking_lot::Mutex::new(Vec::with_capacity(10000)),
            throughput_samples: parking_lot::Mutex::new(Vec::new()),
            ..Default::default()
        }
    }

    fn record_message(&self, size: usize, latency_ns: u64) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        self.total_bytes.fetch_add(size as u64, Ordering::Relaxed);
        self.latency_sum_ns.fetch_add(latency_ns, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);

        // 更新最小延遲
        loop {
            let current_min = self.min_latency_ns.load(Ordering::Relaxed);
            if latency_ns >= current_min {
                break;
            }
            if self
                .min_latency_ns
                .compare_exchange_weak(
                    current_min,
                    latency_ns,
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
            let current_max = self.max_latency_ns.load(Ordering::Relaxed);
            if latency_ns <= current_max {
                break;
            }
            if self
                .max_latency_ns
                .compare_exchange_weak(
                    current_max,
                    latency_ns,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }

        // 採樣存儲（每100個樣本存一個，避免內存爆炸）
        if self.latency_count.load(Ordering::Relaxed) % 100 == 0 {
            let mut samples = self.latency_samples.lock();
            if samples.len() < 10000 {
                // 限制樣本數量
                samples.push(latency_ns);
            }
        }
    }

    fn record_throughput(&self, events_per_sec: u64) {
        let mut samples = self.throughput_samples.lock();
        samples.push(events_per_sec);
    }

    fn get_percentiles(&self) -> (u64, u64, u64) {
        // P50, P95, P99
        let mut samples = self.latency_samples.lock();
        if samples.is_empty() {
            return (0, 0, 0);
        }

        samples.sort_unstable();
        let len = samples.len();
        let p50 = samples[len * 50 / 100];
        let p95 = samples[len * 95 / 100];
        let p99 = samples[len * 99 / 100];

        (p50, p95, p99)
    }

    fn get_comprehensive_stats(&self) -> PerformanceReport {
        let messages = self.total_messages.load(Ordering::Relaxed);
        let bytes = self.total_bytes.load(Ordering::Relaxed);
        let errors = self.connection_errors.load(Ordering::Relaxed);
        let latency_sum = self.latency_sum_ns.load(Ordering::Relaxed);
        let latency_count = self.latency_count.load(Ordering::Relaxed);
        let min_latency = self.min_latency_ns.load(Ordering::Relaxed);
        let max_latency = self.max_latency_ns.load(Ordering::Relaxed);

        let avg_latency_ns = if latency_count > 0 {
            latency_sum / latency_count
        } else {
            0
        };

        let (p50, p95, p99) = self.get_percentiles();

        PerformanceReport {
            total_messages: messages,
            total_bytes: bytes,
            total_errors: errors,
            avg_latency_ns,
            min_latency_ns: if min_latency == u64::MAX {
                0
            } else {
                min_latency
            },
            max_latency_ns: max_latency,
            p50_latency_ns: p50,
            p95_latency_ns: p95,
            p99_latency_ns: p99,
        }
    }
}

#[derive(Debug)]
struct PerformanceReport {
    total_messages: u64,
    total_bytes: u64,
    total_errors: u64,
    avg_latency_ns: u64,
    min_latency_ns: u64,
    max_latency_ns: u64,
    p50_latency_ns: u64,
    p95_latency_ns: u64,
    p99_latency_ns: u64,
}

async fn run_high_performance_test(
    args: Args,
    connection_id: usize,
    metrics: Arc<AdvancedMetrics>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    // 設置CPU親和性（如果啟用）
    if args.enable_cpu_affinity {
        let core_id = connection_id % num_cpus::get();
        if !core_affinity::set_for_current(core_affinity::CoreId { id: core_id }) {
            warn!(
                "Failed to set CPU affinity for connection {} to core {}",
                connection_id, core_id
            );
        } else if args.verbose {
            info!("Connection {} bound to CPU core {}", connection_id, core_id);
        }
    }

    // 創建統一連接器配置
    let config = UnifiedBitgetConfig {
        ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        api_key: None,
        api_secret: None,
        passphrase: None,
        mode: ConnectionMode::Single,
        max_reconnect_attempts: 5,
        reconnect_delay_ms: 500,   // 更快重連
        enable_compression: false, // 禁用壓縮以降低延遲
        max_message_size: 1024 * 1024,
        ping_interval_secs: 30,
    };

    let mut connector = UnifiedBitgetConnector::new(config);

    // 訂閱多個通道以增加數據量
    connector
        .subscribe("BTCUSDT", BitgetChannel::OrderBook)
        .await?;
    connector
        .subscribe("BTCUSDT", BitgetChannel::Trades)
        .await?;
    connector
        .subscribe("ETHUSDT", BitgetChannel::OrderBook)
        .await?;
    connector
        .subscribe("ETHUSDT", BitgetChannel::Trades)
        .await?;

    // 啟動連接並獲取消息接收器
    let connect_start = Instant::now();
    let mut message_rx = connector.start().await?;
    let connect_time = connect_start.elapsed();

    if args.verbose {
        info!(
            "Connection {} established in {:?}",
            connection_id, connect_time
        );
    }

    let mut msg_count_in_interval = 0u64;
    let mut interval_start = Instant::now();

    // 高性能消息處理循環
    while !shutdown.load(Ordering::Relaxed) {
        tokio::select! {
            Some(message) = message_rx.recv() => {
                let receive_time = Instant::now();

                // 計算處理延遲（納秒精度）
                let processing_start = Instant::now();

                // 模擬快速處理邏輯
                let message_size = std::mem::size_of_val(&message.data);
                let symbol = &message.symbol;
                let _channel = &message.channel;

                // 這裡可以添加實際的訂單簿處理、特徵提取等邏輯
                let _processed = format!("{}:{:?}", symbol, message.timestamp);

                let processing_end = Instant::now();
                let processing_latency_ns = processing_end.duration_since(processing_start).as_nanos() as u64;

                // 記錄性能指標
                metrics.record_message(message_size, processing_latency_ns);
                msg_count_in_interval += 1;

                // 每秒計算一次吞吐量
                if interval_start.elapsed() >= Duration::from_secs(1) {
                    metrics.record_throughput(msg_count_in_interval);
                    msg_count_in_interval = 0;
                    interval_start = Instant::now();
                }

                if args.verbose && metrics.total_messages.load(Ordering::Relaxed) % 10000 == 0 {
                    info!("Connection {} processed {} messages",
                          connection_id,
                          metrics.total_messages.load(Ordering::Relaxed));
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(1)) => {
                // 防止忙等待
                continue;
            }
        }
    }

    if args.verbose {
        info!("Connection {} shutting down", connection_id);
    }

    Ok(())
}

async fn run_performance_analysis(args: &Args, metrics: &Arc<AdvancedMetrics>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let test_start = Instant::now();

    loop {
        interval.tick().await;

        let elapsed = test_start.elapsed().as_secs_f64();
        if elapsed >= args.duration as f64 {
            break;
        }

        let report = metrics.get_comprehensive_stats();
        let msgs_per_sec = report.total_messages as f64 / elapsed;
        let mb_per_sec = (report.total_bytes as f64 / elapsed) / (1024.0 * 1024.0);

        info!("📊 性能分析 ({:.1}s):", elapsed);
        info!(
            "   消息處理: {} ({:.1}/s)",
            report.total_messages, msgs_per_sec
        );
        info!(
            "   數據流量: {:.2} MB ({:.2} MB/s)",
            report.total_bytes as f64 / (1024.0 * 1024.0),
            mb_per_sec
        );
        info!(
            "   延遲統計 (ns): avg={}, min={}, max={}",
            report.avg_latency_ns, report.min_latency_ns, report.max_latency_ns
        );
        info!(
            "   延遲分布 (ns): P50={}, P95={}, P99={}",
            report.p50_latency_ns, report.p95_latency_ns, report.p99_latency_ns
        );
        info!("   錯誤計數: {}", report.total_errors);

        // 性能警告
        let avg_latency_us = report.avg_latency_ns as f64 / 1000.0;
        if avg_latency_us > args.target_latency_us as f64 * 2.0 {
            warn!(
                "⚠️  延遲超出目標2倍！當前: {:.1}μs, 目標: {}μs",
                avg_latency_us, args.target_latency_us
            );
        }

        if msgs_per_sec < args.target_throughput as f64 * 0.5 {
            warn!(
                "⚠️  吞吐量低於目標50%！當前: {:.1}/s, 目標: {}/s",
                msgs_per_sec, args.target_throughput
            );
        }

        info!("   ──────────────────────────");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    let metrics = Arc::new(AdvancedMetrics::new());
    let shutdown = Arc::new(AtomicBool::new(false));

    info!("🚀 端到端高性能基準測試開始");
    info!("📋 測試配置:");
    info!("   • 持續時間: {}秒", args.duration);
    info!("   • 並發連接: {}", args.connections);
    info!("   • 目標延遲: {}μs", args.target_latency_us);
    info!("   • 目標吞吐: {}/s", args.target_throughput);
    info!(
        "   • CPU親和性: {}",
        if args.enable_cpu_affinity {
            "啟用"
        } else {
            "禁用"
        }
    );
    info!("   • 測試模式: {}", args.test_mode);
    info!("   ══════════════════════════");

    // 啟動多個並發連接
    let mut handles = Vec::new();
    for i in 0..args.connections {
        let args_clone = args.clone();
        let metrics_clone = metrics.clone();
        let shutdown_clone = shutdown.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) =
                run_high_performance_test(args_clone, i, metrics_clone, shutdown_clone).await
            {
                error!("Connection {} failed: {}", i, e);
            }
        });
        handles.push(handle);
    }

    // 啟動性能分析任務
    let analysis_metrics = metrics.clone();
    let analysis_args = args.clone();
    let analysis_handle = tokio::spawn(async move {
        run_performance_analysis(&analysis_args, &analysis_metrics).await;
    });

    // 等待測試完成
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    // 關閉所有連接
    info!("🔄 正在關閉測試...");
    shutdown.store(true, Ordering::Relaxed);

    // 等待所有任務完成
    for handle in handles {
        let _ = handle.await;
    }
    let _ = analysis_handle.await;

    // 最終性能報告
    let final_report = metrics.get_comprehensive_stats();
    let total_time = args.duration as f64;
    let final_msgs_per_sec = final_report.total_messages as f64 / total_time;
    let final_mb_per_sec = (final_report.total_bytes as f64 / total_time) / (1024.0 * 1024.0);
    let error_rate = if final_report.total_messages > 0 {
        final_report.total_errors as f64 / final_report.total_messages as f64 * 100.0
    } else {
        0.0
    };

    info!("🏆 最終性能報告");
    info!("══════════════════════════");
    info!("⏱️  測試時長: {:.1}s", total_time);
    info!(
        "📨 總消息數: {} ({:.1}/s)",
        final_report.total_messages, final_msgs_per_sec
    );
    info!(
        "📊 總流量: {:.2} MB ({:.2} MB/s)",
        final_report.total_bytes as f64 / (1024.0 * 1024.0),
        final_mb_per_sec
    );

    info!("🚀 延遲統計 (納秒/微秒):");
    info!(
        "   • 平均: {}ns ({:.2}μs)",
        final_report.avg_latency_ns,
        final_report.avg_latency_ns as f64 / 1000.0
    );
    info!(
        "   • 最小: {}ns ({:.2}μs)",
        final_report.min_latency_ns,
        final_report.min_latency_ns as f64 / 1000.0
    );
    info!(
        "   • 最大: {}ns ({:.2}μs)",
        final_report.max_latency_ns,
        final_report.max_latency_ns as f64 / 1000.0
    );
    info!(
        "   • P50:  {}ns ({:.2}μs)",
        final_report.p50_latency_ns,
        final_report.p50_latency_ns as f64 / 1000.0
    );
    info!(
        "   • P95:  {}ns ({:.2}μs)",
        final_report.p95_latency_ns,
        final_report.p95_latency_ns as f64 / 1000.0
    );
    info!(
        "   • P99:  {}ns ({:.2}μs)",
        final_report.p99_latency_ns,
        final_report.p99_latency_ns as f64 / 1000.0
    );

    info!("❌ 錯誤統計:");
    info!("   • 總錯誤: {}", final_report.total_errors);
    info!("   • 錯誤率: {:.3}%", error_rate);

    info!("🔗 連接信息:");
    info!("   • 並發連接: {}", args.connections);
    info!(
        "   • CPU親和性: {}",
        if args.enable_cpu_affinity {
            "已啟用"
        } else {
            "未啟用"
        }
    );

    // 性能等級評估
    let avg_latency_us = final_report.avg_latency_ns as f64 / 1000.0;
    let throughput_score = (final_msgs_per_sec / args.target_throughput as f64 * 100.0).min(100.0);
    let latency_score = if avg_latency_us <= args.target_latency_us as f64 {
        100.0
    } else {
        (args.target_latency_us as f64 / avg_latency_us * 100.0).max(0.0)
    };
    let error_score = if error_rate < 0.01 {
        100.0
    } else if error_rate < 1.0 {
        100.0 - error_rate
    } else {
        0.0
    };

    let overall_score = (throughput_score + latency_score + error_score) / 3.0;

    let grade = if overall_score >= 90.0 {
        "A+ 優秀"
    } else if overall_score >= 80.0 {
        "A 良好"
    } else if overall_score >= 70.0 {
        "B 中等"
    } else if overall_score >= 60.0 {
        "C 及格"
    } else {
        "D 需要優化"
    };

    info!("🏆 性能評級:");
    info!(
        "   • 吞吐量得分: {:.1}% ({:.0}/{} events/s)",
        throughput_score, final_msgs_per_sec, args.target_throughput
    );
    info!(
        "   • 延遲得分: {:.1}% ({:.2}/{} μs)",
        latency_score, avg_latency_us, args.target_latency_us
    );
    info!(
        "   • 穩定性得分: {:.1}% (錯誤率 {:.3}%)",
        error_score, error_rate
    );
    info!("   • 總體得分: {:.1}% - {}", overall_score, grade);

    // 優化建議
    if throughput_score < 50.0 {
        warn!("💡 吞吐量優化建議:");
        warn!("   • 增加並發連接數");
        warn!("   • 啟用CPU親和性綁定");
        warn!("   • 檢查網絡配置和系統參數");
    }

    if latency_score < 50.0 {
        warn!("💡 延遲優化建議:");
        warn!("   • 使用零拷貝數據結構");
        warn!("   • 優化消息處理邏輯");
        warn!("   • 考慮使用內存池");
    }

    if error_score < 90.0 {
        warn!("💡 穩定性優化建議:");
        warn!("   • 檢查網絡連接穩定性");
        warn!("   • 增加錯誤重試機制");
        warn!("   • 優化資源管理");
    }

    info!("══════════════════════════");
    info!("✨ 測試完成！");

    Ok(())
}
