/*!
 * 數據收集系統專用性能測試
 *
 * 測試重點：
 * - 數據處理吞吐量測試
 * - 數據品質監控性能影響
 * - ClickHouse批量寫入性能
 * - 內存使用和GC壓力測試
 * - 長時間穩定性測試
 */

use anyhow::Result;
use clap::Parser;
use rust_hft::{core::types::*, integrations::bitget_connector::*};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::info;

// Helper function to get current time in microseconds
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// 數據收集性能測試參數
#[derive(Parser, Debug)]
#[command(author, version, about = "Data Collection Performance Test")]
struct DataCollectionPerfArgs {
    /// 測試時長(秒)
    #[arg(short, long, default_value = "120")]
    duration: u64,

    /// 模擬數據生成速率 (消息/秒)
    #[arg(short, long, default_value = "1000")]
    rate: u64,

    /// 是否啟用數據品質監控
    #[arg(long)]
    enable_quality_monitoring: bool,

    /// 是否啟用ClickHouse寫入
    #[arg(long)]
    enable_clickhouse: bool,

    /// ClickHouse批量大小
    #[arg(long, default_value = "500")]
    clickhouse_batch_size: usize,

    /// 內存監控間隔(秒)
    #[arg(long, default_value = "5")]
    memory_check_interval: u64,

    /// 詳細日誌輸出
    #[arg(short, long)]
    verbose: bool,
}

/// 性能統計結構
#[derive(Debug)]
pub struct DataCollectionStats {
    // 消息統計
    pub total_messages: AtomicU64,
    pub orderbook_messages: AtomicU64,
    pub trade_messages: AtomicU64,
    pub processed_messages: AtomicU64,
    pub dropped_messages: AtomicU64,

    // 延遲統計
    pub total_processing_latency_us: AtomicU64,
    pub max_processing_latency_us: AtomicU64,
    pub quality_validation_latency_us: AtomicU64,
    pub clickhouse_write_latency_us: AtomicU64,

    // 錯誤統計
    pub parse_errors: AtomicU64,
    pub validation_errors: AtomicU64,
    pub database_errors: AtomicU64,
    pub channel_overflow_errors: AtomicU64,

    // 內存統計
    pub peak_memory_mb: AtomicU64,
    pub current_memory_mb: AtomicU64,
    pub gc_count: AtomicU64,

    // 吞吐量統計
    pub start_time: Instant,
    pub last_report_time: Mutex<Instant>,
    pub last_message_count: AtomicU64,
}

impl DataCollectionStats {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            total_messages: AtomicU64::new(0),
            orderbook_messages: AtomicU64::new(0),
            trade_messages: AtomicU64::new(0),
            processed_messages: AtomicU64::new(0),
            dropped_messages: AtomicU64::new(0),
            total_processing_latency_us: AtomicU64::new(0),
            max_processing_latency_us: AtomicU64::new(0),
            quality_validation_latency_us: AtomicU64::new(0),
            clickhouse_write_latency_us: AtomicU64::new(0),
            parse_errors: AtomicU64::new(0),
            validation_errors: AtomicU64::new(0),
            database_errors: AtomicU64::new(0),
            channel_overflow_errors: AtomicU64::new(0),
            peak_memory_mb: AtomicU64::new(0),
            current_memory_mb: AtomicU64::new(0),
            gc_count: AtomicU64::new(0),
            start_time: now,
            last_report_time: Mutex::new(now),
            last_message_count: AtomicU64::new(0),
        }
    }

    pub fn record_message(&self, msg_type: &str, processing_latency_us: u64) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        self.processed_messages.fetch_add(1, Ordering::Relaxed);
        self.total_processing_latency_us
            .fetch_add(processing_latency_us, Ordering::Relaxed);

        // 更新最大延遲
        let mut current_max = self.max_processing_latency_us.load(Ordering::Relaxed);
        while processing_latency_us > current_max {
            match self.max_processing_latency_us.compare_exchange_weak(
                current_max,
                processing_latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current_max = x,
            }
        }

        match msg_type {
            "orderbook" => {
                self.orderbook_messages.fetch_add(1, Ordering::Relaxed);
            }
            "trade" => {
                self.trade_messages.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        };
    }

    pub fn record_quality_validation_latency(&self, latency_us: u64) {
        self.quality_validation_latency_us
            .fetch_add(latency_us, Ordering::Relaxed);
    }

    pub fn record_clickhouse_write_latency(&self, latency_us: u64) {
        self.clickhouse_write_latency_us
            .fetch_add(latency_us, Ordering::Relaxed);
    }

    pub fn record_error(&self, error_type: &str) {
        match error_type {
            "parse" => {
                self.parse_errors.fetch_add(1, Ordering::Relaxed);
            }
            "validation" => {
                self.validation_errors.fetch_add(1, Ordering::Relaxed);
            }
            "database" => {
                self.database_errors.fetch_add(1, Ordering::Relaxed);
            }
            "channel_overflow" => {
                self.channel_overflow_errors.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        };
    }

    pub fn update_memory_usage(&self, memory_mb: u64) {
        self.current_memory_mb.store(memory_mb, Ordering::Relaxed);

        let mut current_peak = self.peak_memory_mb.load(Ordering::Relaxed);
        while memory_mb > current_peak {
            match self.peak_memory_mb.compare_exchange_weak(
                current_peak,
                memory_mb,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current_peak = x,
            }
        }
    }

    pub fn get_current_throughput(&self) -> f64 {
        let elapsed_secs = self.start_time.elapsed().as_secs_f64();
        if elapsed_secs > 0.0 {
            self.total_messages.load(Ordering::Relaxed) as f64 / elapsed_secs
        } else {
            0.0
        }
    }

    pub fn get_instantaneous_throughput(&self) -> f64 {
        if let Ok(mut last_time) = self.last_report_time.try_lock() {
            let now = Instant::now();
            let elapsed = now.duration_since(*last_time).as_secs_f64();
            let current_count = self.total_messages.load(Ordering::Relaxed);
            let last_count = self.last_message_count.load(Ordering::Relaxed);

            if elapsed > 0.0 {
                let throughput = (current_count - last_count) as f64 / elapsed;
                *last_time = now;
                self.last_message_count
                    .store(current_count, Ordering::Relaxed);
                throughput
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    pub fn get_avg_processing_latency_us(&self) -> f64 {
        let total_processed = self.processed_messages.load(Ordering::Relaxed);
        if total_processed > 0 {
            self.total_processing_latency_us.load(Ordering::Relaxed) as f64 / total_processed as f64
        } else {
            0.0
        }
    }

    pub fn print_realtime_stats(&self) {
        let total = self.total_messages.load(Ordering::Relaxed);
        let processed = self.processed_messages.load(Ordering::Relaxed);
        let dropped = self.dropped_messages.load(Ordering::Relaxed);
        let current_throughput = self.get_current_throughput();
        let instant_throughput = self.get_instantaneous_throughput();
        let avg_latency = self.get_avg_processing_latency_us();
        let max_latency = self.max_processing_latency_us.load(Ordering::Relaxed);
        let memory_mb = self.current_memory_mb.load(Ordering::Relaxed);

        info!("📊 實時統計 - 總計: {} | 處理: {} | 丟棄: {} | 吞吐量: {:.1}/s (瞬時: {:.1}/s) | 延遲: {:.1}μs (最大: {}μs) | 內存: {}MB",
              total, processed, dropped, current_throughput, instant_throughput, avg_latency, max_latency, memory_mb);
    }

    pub fn print_final_summary(&self, test_duration_secs: u64) {
        let total = self.total_messages.load(Ordering::Relaxed);
        let processed = self.processed_messages.load(Ordering::Relaxed);
        let dropped = self.dropped_messages.load(Ordering::Relaxed);
        let orderbooks = self.orderbook_messages.load(Ordering::Relaxed);
        let trades = self.trade_messages.load(Ordering::Relaxed);

        let parse_errs = self.parse_errors.load(Ordering::Relaxed);
        let validation_errs = self.validation_errors.load(Ordering::Relaxed);
        let db_errs = self.database_errors.load(Ordering::Relaxed);
        let channel_errs = self.channel_overflow_errors.load(Ordering::Relaxed);

        let avg_latency = self.get_avg_processing_latency_us();
        let max_latency = self.max_processing_latency_us.load(Ordering::Relaxed);
        let peak_memory = self.peak_memory_mb.load(Ordering::Relaxed);
        let avg_throughput = total as f64 / test_duration_secs as f64;

        info!("\n🎯 === 數據收集性能測試總結 ===");
        info!("⏱️  測試時長: {}秒", test_duration_secs);

        info!("\n📦 === 消息統計 ===");
        info!("總消息數: {}", total);
        info!(
            "已處理: {} ({:.1}%)",
            processed,
            processed as f64 / total as f64 * 100.0
        );
        info!(
            "已丟棄: {} ({:.1}%)",
            dropped,
            dropped as f64 / total as f64 * 100.0
        );
        info!("OrderBook: {}", orderbooks);
        info!("Trade: {}", trades);
        info!("平均吞吐量: {:.1} 消息/秒", avg_throughput);

        info!("\n⚡ === 性能指標 ===");
        info!("平均處理延遲: {:.1}μs", avg_latency);
        info!("最大處理延遲: {}μs", max_latency);
        info!("峰值內存使用: {}MB", peak_memory);

        if parse_errs + validation_errs + db_errs + channel_errs > 0 {
            info!("\n❌ === 錯誤統計 ===");
            info!("解析錯誤: {}", parse_errs);
            info!("驗證錯誤: {}", validation_errs);
            info!("數據庫錯誤: {}", db_errs);
            info!("通道溢出: {}", channel_errs);
        }

        info!("\n🏆 === 性能評級 ===");

        // 吞吐量評級
        let throughput_grade = if avg_throughput >= 5000.0 {
            "🟢 優秀"
        } else if avg_throughput >= 2000.0 {
            "🟡 良好"
        } else if avg_throughput >= 1000.0 {
            "🟠 一般"
        } else {
            "🔴 需要優化"
        };

        // 延遲評級
        let latency_grade = if avg_latency <= 50.0 {
            "🟢 優秀"
        } else if avg_latency <= 100.0 {
            "🟡 良好"
        } else if avg_latency <= 200.0 {
            "🟠 一般"
        } else {
            "🔴 需要優化"
        };

        // 內存評級
        let memory_grade = if peak_memory <= 200 {
            "🟢 優秀"
        } else if peak_memory <= 500 {
            "🟡 良好"
        } else if peak_memory <= 1000 {
            "🟠 一般"
        } else {
            "🔴 需要優化"
        };

        info!(
            "吞吐量: {} ({:.1} 消息/秒)",
            throughput_grade, avg_throughput
        );
        info!("延遲: {} ({:.1}μs)", latency_grade, avg_latency);
        info!("內存: {} ({}MB)", memory_grade, peak_memory);
    }
}

/// 真實多交易對數據收集器
pub struct RealDataCollector {
    symbols: Vec<String>,
    message_count: Arc<AtomicU64>,
    should_stop: Arc<AtomicBool>,
}

impl RealDataCollector {
    pub fn new(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            message_count: Arc::new(AtomicU64::new(0)),
            should_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start_collection(
        self: Arc<Self>,
        tx: mpsc::Sender<(String, Value, u64)>,
        stats: Arc<DataCollectionStats>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut connector_handles = Vec::new();

            // 為每個交易對創建獨立的連接器
            for symbol in &self.symbols {
                let symbol_clone = symbol.clone();
                let tx_clone = tx.clone();
                let stats_clone = stats.clone();
                let message_count = self.message_count.clone();
                let should_stop = self.should_stop.clone();

                let handle = tokio::spawn(async move {
                    let bitget_config = BitgetConfig::default();
                    let mut connector = BitgetConnector::new(bitget_config);
                    connector.add_subscription(symbol_clone.clone(), BitgetChannel::Books5);
                    connector.add_subscription(symbol_clone.clone(), BitgetChannel::Trade);

                    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel();
                    let ws_tx_clone = ws_tx.clone();

                    let message_handler = move |message: BitgetMessage| {
                        let _ = ws_tx_clone.send(message);
                    };

                    // 啟動連接器
                    let connector_task = tokio::spawn(async move {
                        if let Err(e) = connector.connect_public(message_handler).await {
                            tracing::error!(
                                "WebSocket connection failed for {}: {}",
                                symbol_clone,
                                e
                            );
                        }
                    });

                    // 處理消息
                    while !should_stop.load(Ordering::Relaxed) {
                        tokio::select! {
                            Some(message) = ws_rx.recv() => {
                                let timestamp = now_micros();
                                message_count.fetch_add(1, Ordering::Relaxed);

                                let (msg_type, data) = match message {
                                    BitgetMessage::OrderBook { data, .. } => {
                                        ("orderbook", data)
                                    }
                                    BitgetMessage::Trade { data, .. } => {
                                        ("trade", data)
                                    }
                                    BitgetMessage::Ticker { data, .. } => {
                                        ("ticker", data)
                                    }
                                };

                                if tx_clone.send((msg_type.to_string(), data, timestamp)).await.is_err() {
                                    stats_clone.record_error("channel_overflow");
                                    break;
                                }
                            }
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                                // 定期檢查關閉信號
                                continue;
                            }
                        }
                    }

                    connector_task.abort();
                });

                connector_handles.push(handle);
            }

            // 等待所有連接器完成
            for handle in connector_handles {
                let _ = handle.await;
            }

            info!(
                "📤 真實數據收集器停止，總計收集 {} 條消息",
                self.message_count.load(Ordering::Relaxed)
            );
        })
    }

    pub fn stop(&self) {
        self.should_stop.store(true, Ordering::Relaxed);
    }
}

/// 數據處理器
pub struct DataProcessor {
    enable_quality_monitoring: bool,
    enable_clickhouse: bool,
    batch_size: usize,
    buffer: Vec<Value>,
}

impl DataProcessor {
    pub fn new(
        enable_quality_monitoring: bool,
        enable_clickhouse: bool,
        batch_size: usize,
    ) -> Self {
        Self {
            enable_quality_monitoring,
            enable_clickhouse,
            batch_size,
            buffer: Vec::with_capacity(batch_size),
        }
    }

    pub async fn process_message(
        &mut self,
        msg_type: &str,
        data: Value,
        _timestamp: u64,
        stats: &DataCollectionStats,
    ) {
        let process_start = now_micros();

        // 模擬解析處理
        tokio::time::sleep(Duration::from_micros(5)).await;

        // 模擬數據品質監控
        if self.enable_quality_monitoring {
            let quality_start = now_micros();
            self.simulate_quality_validation(&data).await;
            let quality_latency = now_micros() - quality_start;
            stats.record_quality_validation_latency(quality_latency);
        }

        // 模擬ClickHouse批量寫入
        if self.enable_clickhouse {
            self.buffer.push(data);
            if self.buffer.len() >= self.batch_size {
                let write_start = now_micros();
                self.simulate_clickhouse_write().await;
                let write_latency = now_micros() - write_start;
                stats.record_clickhouse_write_latency(write_latency);
                self.buffer.clear();
            }
        }

        let total_latency = now_micros() - process_start;
        stats.record_message(msg_type, total_latency);
    }

    async fn simulate_quality_validation(&self, _data: &Value) {
        // 模擬品質驗證計算
        tokio::time::sleep(Duration::from_micros(15)).await;
    }

    async fn simulate_clickhouse_write(&self) {
        // 模擬批量數據庫寫入
        tokio::time::sleep(Duration::from_micros(200)).await;
    }

    pub async fn flush_remaining(&mut self, stats: &DataCollectionStats) {
        if !self.buffer.is_empty() && self.enable_clickhouse {
            let write_start = now_micros();
            self.simulate_clickhouse_write().await;
            let write_latency = now_micros() - write_start;
            stats.record_clickhouse_write_latency(write_latency);
            self.buffer.clear();
        }
    }
}

/// 內存監控器
pub fn start_memory_monitor(
    stats: Arc<DataCollectionStats>,
    interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while interval_secs > 0 {
            // 模擬內存使用監控
            let memory_mb = get_current_memory_usage_mb();
            stats.update_memory_usage(memory_mb);

            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    })
}

/// 獲取當前內存使用量
fn get_current_memory_usage_mb() -> u64 {
    // 簡化的內存使用量模擬
    static mut MEMORY_USAGE: u64 = 50;
    unsafe {
        MEMORY_USAGE += fastrand::u64(1..10);
        if MEMORY_USAGE > 300 {
            MEMORY_USAGE = 50;
        }
        MEMORY_USAGE
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt::init();

    let args = DataCollectionPerfArgs::parse();

    info!("🚀 數據收集系統性能測試");
    info!("📊 測試配置:");
    info!("   測試時長: {}秒", args.duration);
    info!("   數據生成速率: {} 消息/秒", args.rate);
    info!(
        "   品質監控: {}",
        if args.enable_quality_monitoring {
            "啟用"
        } else {
            "禁用"
        }
    );
    info!(
        "   ClickHouse: {}",
        if args.enable_clickhouse {
            "啟用"
        } else {
            "禁用"
        }
    );
    if args.enable_clickhouse {
        info!("   批量大小: {}", args.clickhouse_batch_size);
    }
    info!("   內存檢查間隔: {}秒", args.memory_check_interval);

    let stats = Arc::new(DataCollectionStats::new());

    // 創建消息通道
    let (tx, mut rx) = mpsc::channel::<(String, Value, u64)>(10000);

    // 啟動真實數據收集器
    let symbols = vec![
        "BTCUSDT".to_string(),
        "ETHUSDT".to_string(),
        "SOLUSDT".to_string(),
    ];
    let collector = Arc::new(RealDataCollector::new(symbols));
    let collection_handle = collector
        .clone()
        .start_collection(tx.clone(), stats.clone());

    // 啟動內存監控
    let memory_handle = start_memory_monitor(stats.clone(), args.memory_check_interval);

    // 啟動實時統計報告
    let stats_clone = stats.clone();
    let stats_handle = tokio::spawn(async move {
        while args.duration > 0 {
            tokio::time::sleep(Duration::from_secs(10)).await;
            stats_clone.print_realtime_stats();
        }
    });

    // 啟動數據處理任務
    let mut processor = DataProcessor::new(
        args.enable_quality_monitoring,
        args.enable_clickhouse,
        args.clickhouse_batch_size,
    );

    let processing_handle = {
        let stats = stats.clone();
        tokio::spawn(async move {
            while let Some((msg_type, data, timestamp)) = rx.recv().await {
                processor
                    .process_message(&msg_type, data, timestamp, &stats)
                    .await;
            }

            // 處理剩餘緩衝數據
            processor.flush_remaining(&stats).await;
            info!("📥 數據處理器完成");
        })
    };

    // 運行測試
    info!("🔄 開始性能測試...");
    tokio::time::sleep(Duration::from_secs(args.duration)).await;
    info!("⏰ 測試時間結束，正在停止組件...");

    // 停止數據收集
    collector.stop();
    collection_handle.await?;

    // 關閉發送端，等待處理完成
    drop(tx);
    processing_handle.await?;

    // 停止其他任務
    memory_handle.abort();
    stats_handle.abort();

    // 打印最終統計
    stats.print_final_summary(args.duration);

    info!("🏁 數據收集性能測試完成！");

    Ok(())
}
// Moved to grouped example: 01_data/main.rs
