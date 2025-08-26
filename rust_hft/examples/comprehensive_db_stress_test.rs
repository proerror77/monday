/*!
 * 20+商品 × 15分钟完整数据库压力测试
 *
 * 测试目标：
 * - 20+热门交易对的真实WebSocket数据收集
 * - 15分钟持续不间断数据写入ClickHouse
 * - 详细性能监控和分析
 * - 自动化优化建议生成
 * - 完整的压力测试报告
 */

use anyhow::Result;
use clickhouse::{Client, Row};
use rust_hft::integrations::{
    unified_bitget_connector::BitgetMessage, ConnectionMode, UnifiedBitgetChannel,
    UnifiedBitgetConfig, UnifiedBitgetConnector,
};
use rustls::crypto::aws_lc_rs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use tracing::{error, info, warn};

/// 测试配置
#[derive(Debug, Clone)]
struct ComprehensiveTestConfig {
    /// 测试时长（分钟）
    pub test_duration_minutes: u64,
    /// 目标商品数量
    pub target_symbols_count: usize,
    /// ClickHouse配置
    pub clickhouse_url: String,
    pub database_name: String,
    pub table_name: String,
    /// 批量写入配置
    pub batch_size: usize,
    pub batch_timeout_secs: u64,
    /// 并发写入器数量
    pub writer_count: usize,
    /// 性能监控间隔（秒）
    pub monitoring_interval_secs: u64,
}

impl Default for ComprehensiveTestConfig {
    fn default() -> Self {
        Self {
            test_duration_minutes: 15,
            target_symbols_count: 25,
            clickhouse_url: "http://localhost:8123".to_string(),
            database_name: "hft".to_string(),
            table_name: "market_data_15min".to_string(),
            batch_size: 500,
            batch_timeout_secs: 2,
            writer_count: 4,
            monitoring_interval_secs: 30,
        }
    }
}

/// ClickHouse数据记录结构
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
struct MarketDataRecord {
    /// 时间戳（微秒）
    timestamp: i64,
    /// 交易对符号
    symbol: String,
    /// 数据通道类型
    channel: String,
    /// 最佳买价
    best_bid: f64,
    /// 最佳卖价
    best_ask: f64,
    /// 中间价
    mid_price: f64,
    /// 价差
    spread: f64,
    /// 买单总量
    bid_volume: f64,
    /// 卖单总量
    ask_volume: f64,
    /// 最新成交价
    last_price: f64,
    /// 24小时成交量
    volume_24h: f64,
    /// 原始数据大小（字节）
    raw_data_size: i32,
    /// 处理延迟（微秒）
    processing_latency_us: i64,
}

/// 实时性能统计
#[derive(Debug)]
struct RealTimePerformanceStats {
    // 连接统计
    active_connections: AtomicU64,
    failed_connections: AtomicU64,
    reconnection_count: AtomicU64,

    // 数据流统计
    total_messages_received: AtomicU64,
    total_records_written: AtomicU64,
    total_batches_written: AtomicU64,
    total_bytes_written: AtomicU64,

    // 性能指标
    min_processing_latency_us: AtomicU64,
    max_processing_latency_us: AtomicU64,
    total_processing_latency_us: AtomicU64,

    // 数据库性能
    db_write_success_count: AtomicU64,
    db_write_error_count: AtomicU64,
    total_db_write_latency_us: AtomicU64,

    // 队列统计
    current_queue_size: AtomicU64,
    max_queue_size: AtomicU64,
    queue_overflow_count: AtomicU64,

    // 按商品统计
    symbol_stats: Arc<RwLock<HashMap<String, SymbolStats>>>,

    // 测试控制
    test_start_time: Arc<RwLock<Option<Instant>>>,
    is_running: AtomicBool,
}

#[derive(Debug, Clone)]
struct SymbolStats {
    message_count: u64,
    last_update: Instant,
    connection_errors: u64,
    avg_latency_us: f64,
}

impl Default for SymbolStats {
    fn default() -> Self {
        Self {
            message_count: 0,
            last_update: Instant::now(),
            connection_errors: 0,
            avg_latency_us: 0.0,
        }
    }
}

impl RealTimePerformanceStats {
    fn new() -> Self {
        Self {
            active_connections: AtomicU64::new(0),
            failed_connections: AtomicU64::new(0),
            reconnection_count: AtomicU64::new(0),
            total_messages_received: AtomicU64::new(0),
            total_records_written: AtomicU64::new(0),
            total_batches_written: AtomicU64::new(0),
            total_bytes_written: AtomicU64::new(0),
            min_processing_latency_us: AtomicU64::new(u64::MAX),
            max_processing_latency_us: AtomicU64::new(0),
            total_processing_latency_us: AtomicU64::new(0),
            db_write_success_count: AtomicU64::new(0),
            db_write_error_count: AtomicU64::new(0),
            total_db_write_latency_us: AtomicU64::new(0),
            current_queue_size: AtomicU64::new(0),
            max_queue_size: AtomicU64::new(0),
            queue_overflow_count: AtomicU64::new(0),
            symbol_stats: Arc::new(RwLock::new(HashMap::new())),
            test_start_time: Arc::new(RwLock::new(None)),
            is_running: AtomicBool::new(false),
        }
    }

    async fn start_test(&self) {
        self.is_running.store(true, Ordering::Relaxed);
        *self.test_start_time.write().await = Some(Instant::now());
    }

    fn stop_test(&self) {
        self.is_running.store(false, Ordering::Relaxed);
    }

    fn is_test_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    fn record_connection_success(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    fn record_connection_failure(&self) {
        self.failed_connections.fetch_add(1, Ordering::Relaxed);
    }

    fn record_message_received(&self, symbol: &str, latency_us: u64) {
        self.total_messages_received.fetch_add(1, Ordering::Relaxed);
        self.total_processing_latency_us
            .fetch_add(latency_us, Ordering::Relaxed);

        // 更新最小延迟
        let mut current_min = self.min_processing_latency_us.load(Ordering::Relaxed);
        while latency_us < current_min {
            match self.min_processing_latency_us.compare_exchange_weak(
                current_min,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(new_min) => current_min = new_min,
            }
        }

        // 更新最大延迟
        let mut current_max = self.max_processing_latency_us.load(Ordering::Relaxed);
        while latency_us > current_max {
            match self.max_processing_latency_us.compare_exchange_weak(
                current_max,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(new_max) => current_max = new_max,
            }
        }

        // 更新按商品统计 - 修复生命周期问题
        let symbol_stats = self.symbol_stats.clone();
        let symbol_owned = symbol.to_string(); // 转换为拥有的String
        tokio::spawn(async move {
            let mut stats = symbol_stats.write().await;
            let entry = stats
                .entry(symbol_owned)
                .or_insert_with(SymbolStats::default);
            entry.message_count += 1;
            entry.last_update = Instant::now();
            entry.avg_latency_us = (entry.avg_latency_us * (entry.message_count - 1) as f64
                + latency_us as f64)
                / entry.message_count as f64;
        });
    }

    fn record_db_write_success(&self, batch_size: usize, bytes: usize, latency_us: u64) {
        self.total_records_written
            .fetch_add(batch_size as u64, Ordering::Relaxed);
        self.total_batches_written.fetch_add(1, Ordering::Relaxed);
        self.total_bytes_written
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.db_write_success_count.fetch_add(1, Ordering::Relaxed);
        self.total_db_write_latency_us
            .fetch_add(latency_us, Ordering::Relaxed);
    }

    fn record_db_write_error(&self) {
        self.db_write_error_count.fetch_add(1, Ordering::Relaxed);
    }

    fn update_queue_size(&self, size: usize) {
        let size_u64 = size as u64;
        self.current_queue_size.store(size_u64, Ordering::Relaxed);

        let mut current_max = self.max_queue_size.load(Ordering::Relaxed);
        while size_u64 > current_max {
            match self.max_queue_size.compare_exchange_weak(
                current_max,
                size_u64,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(new_max) => current_max = new_max,
            }
        }
    }

    async fn get_test_duration(&self) -> Duration {
        if let Some(start) = *self.test_start_time.read().await {
            start.elapsed()
        } else {
            Duration::from_secs(0)
        }
    }

    async fn print_detailed_stats(&self) {
        let duration = self.get_test_duration().await;
        let total_messages = self.total_messages_received.load(Ordering::Relaxed);
        let total_written = self.total_records_written.load(Ordering::Relaxed);
        let total_bytes = self.total_bytes_written.load(Ordering::Relaxed);

        let msg_rate = if duration.as_secs_f64() > 0.0 {
            total_messages as f64 / duration.as_secs_f64()
        } else {
            0.0
        };

        let write_rate = if duration.as_secs_f64() > 0.0 {
            total_written as f64 / duration.as_secs_f64()
        } else {
            0.0
        };

        let throughput_mbps = if duration.as_secs_f64() > 0.0 {
            (total_bytes as f64 / duration.as_secs_f64()) / (1024.0 * 1024.0)
        } else {
            0.0
        };

        let avg_processing_latency = if total_messages > 0 {
            self.total_processing_latency_us.load(Ordering::Relaxed) as f64 / total_messages as f64
        } else {
            0.0
        };

        let avg_db_latency = if self.db_write_success_count.load(Ordering::Relaxed) > 0 {
            self.total_db_write_latency_us.load(Ordering::Relaxed) as f64
                / self.db_write_success_count.load(Ordering::Relaxed) as f64
        } else {
            0.0
        };

        info!("📊 === 详细性能统计 [运行时间: {:?}] ===", duration);
        info!("🔗 连接状态:");
        info!(
            "   活跃连接: {}",
            self.active_connections.load(Ordering::Relaxed)
        );
        info!(
            "   失败连接: {}",
            self.failed_connections.load(Ordering::Relaxed)
        );
        info!(
            "   重连次数: {}",
            self.reconnection_count.load(Ordering::Relaxed)
        );

        info!("📡 数据流性能:");
        info!("   接收消息: {} ({:.1} msg/s)", total_messages, msg_rate);
        info!("   写入记录: {} ({:.1} rec/s)", total_written, write_rate);
        info!(
            "   写入批次: {}",
            self.total_batches_written.load(Ordering::Relaxed)
        );
        info!(
            "   数据吞吐: {:.2} MB ({:.2} MB/s)",
            total_bytes as f64 / (1024.0 * 1024.0),
            throughput_mbps
        );

        info!("⚡ 延迟性能:");
        info!(
            "   处理延迟: {:.1}μs 平均, {}μs 最小, {}μs 最大",
            avg_processing_latency,
            self.min_processing_latency_us.load(Ordering::Relaxed),
            self.max_processing_latency_us.load(Ordering::Relaxed)
        );
        info!("   数据库延迟: {:.1}μs 平均", avg_db_latency);

        info!("🗄️  数据库性能:");
        info!(
            "   写入成功: {}",
            self.db_write_success_count.load(Ordering::Relaxed)
        );
        info!(
            "   写入错误: {}",
            self.db_write_error_count.load(Ordering::Relaxed)
        );
        info!(
            "   当前队列: {}",
            self.current_queue_size.load(Ordering::Relaxed)
        );
        info!(
            "   最大队列: {}",
            self.max_queue_size.load(Ordering::Relaxed)
        );
        info!(
            "   队列溢出: {}",
            self.queue_overflow_count.load(Ordering::Relaxed)
        );

        // 按商品统计
        let symbol_stats = self.symbol_stats.read().await;
        if !symbol_stats.is_empty() {
            info!("📈 按商品统计 (前10个最活跃):");
            let mut sorted_symbols: Vec<_> = symbol_stats.iter().collect();
            sorted_symbols.sort_by(|a, b| b.1.message_count.cmp(&a.1.message_count));

            for (symbol, stats) in sorted_symbols.iter().take(10) {
                info!(
                    "   {}: {} msgs, {:.1}μs avg",
                    symbol, stats.message_count, stats.avg_latency_us
                );
            }
        }

        info!("════════════════════════════════════");
    }
}

/// 获取25个热门交易对
fn get_target_symbols() -> Vec<String> {
    vec![
        "BTCUSDT",
        "ETHUSDT",
        "BNBUSDT",
        "XRPUSDT",
        "ADAUSDT",
        "SOLUSDT",
        "DOGEUSDT",
        "MATICUSDT",
        "LINKUSDT",
        "UNIUSDT",
        "LTCUSDT",
        "BCHUSDT",
        "XLMUSDT",
        "VETUSDT",
        "TRXUSDT",
        "EOSUSDT",
        "ATOMUSDT",
        "DOTUSDT",
        "AVAXUSDT",
        "SHIBUSDT",
        "FTMUSDT",
        "NEARUSDT",
        "ALGOUSDT",
        "KSMUSDT",
        "WAVESUSDT",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect()
}

/// 获取测试通道
fn get_test_channels() -> Vec<UnifiedBitgetChannel> {
    vec![
        UnifiedBitgetChannel::OrderBook5,
        UnifiedBitgetChannel::Trades,
        UnifiedBitgetChannel::Ticker,
    ]
}

/// 单个连接的数据收集器
async fn run_symbol_data_collector(
    symbol: String,
    channels: Vec<UnifiedBitgetChannel>,
    tx: mpsc::Sender<MarketDataRecord>,
    stats: Arc<RealTimePerformanceStats>,
) -> Result<()> {
    info!("🚀 启动 {} 数据收集器", symbol);

    let config = UnifiedBitgetConfig {
        ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        api_key: None,
        api_secret: None,
        passphrase: None,
        mode: ConnectionMode::Single,
        max_reconnect_attempts: 5,
        reconnect_delay_ms: 1000,
        enable_compression: true,
        max_message_size: 2 * 1024 * 1024,
        ping_interval_secs: 30,
        connection_timeout_secs: 15,
    };

    let connector = UnifiedBitgetConnector::new(config);

    // 添加所有通道订阅
    for channel in &channels {
        if let Err(e) = connector.subscribe(&symbol, channel.clone()).await {
            error!("{} 订阅 {:?} 失败: {}", symbol, channel, e);
            stats.record_connection_failure();
            return Err(e);
        }
    }

    info!("{} 添加了 {} 个订阅", symbol, channels.len());

    // 启动连接
    let mut receiver = match timeout(Duration::from_secs(15), connector.start()).await {
        Ok(Ok(rx)) => {
            info!("✅ {} 连接成功", symbol);
            stats.record_connection_success();
            rx
        }
        Ok(Err(e)) => {
            error!("❌ {} 连接失败: {}", symbol, e);
            stats.record_connection_failure();
            return Err(e);
        }
        Err(_) => {
            error!("⏰ {} 连接超时", symbol);
            stats.record_connection_failure();
            return Err(anyhow::anyhow!("连接超时"));
        }
    };

    let mut local_message_count = 0u64;
    let collector_start = Instant::now();

    // 持续接收数据
    while stats.is_test_running() {
        match timeout(Duration::from_millis(1000), receiver.recv()).await {
            Ok(Some(message)) => {
                let receive_start = Instant::now();
                local_message_count += 1;

                // 转换为数据库记录
                if let Ok(record) = convert_message_to_record(&symbol, &message, receive_start) {
                    let processing_latency = receive_start.elapsed().as_micros() as u64;
                    stats.record_message_received(&symbol, processing_latency);

                    // 发送到数据库写入队列
                    if tx.send(record).await.is_err() {
                        warn!("{} 数据发送失败，队列可能已满", symbol);
                        break;
                    }
                }

                // 定期报告进度
                if local_message_count % 2000 == 0 {
                    let elapsed = collector_start.elapsed();
                    let rate = local_message_count as f64 / elapsed.as_secs_f64();
                    info!(
                        "📡 {} 高频接收: {} 消息 ({:.1} msg/s)",
                        symbol, local_message_count, rate
                    );
                }
            }
            Ok(None) => {
                warn!("📡 {} 接收器关闭", symbol);
                break;
            }
            Err(_) => {
                // 超时继续
                if !stats.is_test_running() {
                    break;
                }
                continue;
            }
        }
    }

    let final_elapsed = collector_start.elapsed();
    let final_rate = local_message_count as f64 / final_elapsed.as_secs_f64();
    info!(
        "🏁 {} 收集完成: {} 消息, {:.1} msg/s, 耗时: {:?}",
        symbol, local_message_count, final_rate, final_elapsed
    );

    Ok(())
}

/// 转换消息为数据库记录
fn convert_message_to_record(
    symbol: &str,
    message: &BitgetMessage,
    receive_time: Instant,
) -> Result<MarketDataRecord> {
    let now_us = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros() as i64;

    let processing_latency_us = receive_time.elapsed().as_micros() as i64;

    // 简化的数据提取（实际应该根据具体消息类型解析）
    let record = MarketDataRecord {
        timestamp: now_us,
        symbol: symbol.to_string(),
        channel: format!("{:?}", message.channel),
        best_bid: 0.0, // 应该从实际数据中提取
        best_ask: 0.0, // 应该从实际数据中提取
        mid_price: 0.0,
        spread: 0.0,
        bid_volume: 0.0,
        ask_volume: 0.0,
        last_price: 0.0,
        volume_24h: 0.0,
        raw_data_size: serde_json::to_string(&message)?.len() as i32,
        processing_latency_us,
    };

    Ok(record)
}

/// 批量数据库写入器（broadcast版本）
async fn batch_database_writer_broadcast(
    mut rx: tokio::sync::broadcast::Receiver<MarketDataRecord>,
    client: Arc<Client>,
    config: ComprehensiveTestConfig,
    stats: Arc<RealTimePerformanceStats>,
    writer_id: usize,
) -> Result<()> {
    let mut batch = Vec::with_capacity(config.batch_size);
    let mut last_flush = Instant::now();
    let flush_interval = Duration::from_secs(config.batch_timeout_secs);

    info!("💾 数据库写入器 {} 启动", writer_id);

    while stats.is_test_running() {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(record) => {
                        batch.push(record);
                        stats.update_queue_size(batch.len());
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("写入器 {} 消息滞后 {} 条", writer_id, n);
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("写入器 {} 通道关闭", writer_id);
                        break;
                    }
                }

                // 达到批量大小或超时则刷新
                if batch.len() >= config.batch_size || last_flush.elapsed() >= flush_interval {
                    if let Err(e) = flush_batch_to_clickhouse(
                        &client,
                        &mut batch,
                        &config,
                        &stats,
                        writer_id
                    ).await {
                        error!("写入器 {} 批量刷新失败: {}", writer_id, e);
                        stats.record_db_write_error();
                    }
                    last_flush = Instant::now();
                }
            }
            _ = tokio::time::sleep(flush_interval) => {
                if !batch.is_empty() {
                    if let Err(e) = flush_batch_to_clickhouse(
                        &client,
                        &mut batch,
                        &config,
                        &stats,
                        writer_id
                    ).await {
                        error!("写入器 {} 定时刷新失败: {}", writer_id, e);
                        stats.record_db_write_error();
                    }
                    last_flush = Instant::now();
                }
            }
        }
    }

    // 最终刷新
    if !batch.is_empty() {
        if let Err(e) =
            flush_batch_to_clickhouse(&client, &mut batch, &config, &stats, writer_id).await
        {
            error!("写入器 {} 最终刷新失败: {}", writer_id, e);
            stats.record_db_write_error();
        }
    }

    info!("💾 数据库写入器 {} 完成", writer_id);
    Ok(())
}

/// 刷新批量数据到ClickHouse
async fn flush_batch_to_clickhouse(
    client: &Client,
    batch: &mut Vec<MarketDataRecord>,
    config: &ComprehensiveTestConfig,
    stats: &RealTimePerformanceStats,
    writer_id: usize,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }

    let start_time = Instant::now();
    let record_count = batch.len();
    let table_name = format!("{}.{}", config.database_name, config.table_name);

    // 执行批量插入
    let mut insert = client.insert(&table_name)?;
    for record in batch.iter() {
        insert.write(record).await?;
    }
    insert.end().await?;

    let latency_us = start_time.elapsed().as_micros() as u64;
    let data_size = record_count * std::mem::size_of::<MarketDataRecord>();

    stats.record_db_write_success(record_count, data_size, latency_us);

    info!(
        "💾 写入器 {}: {} 记录写入完成 ({}μs)",
        writer_id, record_count, latency_us
    );

    batch.clear();
    Ok(())
}

/// 设置ClickHouse数据库和表
async fn setup_clickhouse_database(
    client: &Client,
    config: &ComprehensiveTestConfig,
) -> Result<()> {
    // 创建数据库
    let create_db_sql = format!("CREATE DATABASE IF NOT EXISTS {}", config.database_name);
    client.query(&create_db_sql).execute().await?;

    // 创建表
    let create_table_sql = format!(
        r#"
        CREATE TABLE IF NOT EXISTS {}.{} (
            timestamp Int64,
            symbol String,
            channel String,
            best_bid Float64,
            best_ask Float64,
            mid_price Float64,
            spread Float64,
            bid_volume Float64,
            ask_volume Float64,
            last_price Float64,
            volume_24h Float64,
            raw_data_size Int32,
            processing_latency_us Int64
        ) ENGINE = MergeTree()
        ORDER BY (symbol, timestamp)
        PARTITION BY toYYYYMMDD(toDateTime(timestamp / 1000000))
        SETTINGS index_granularity = 8192
        "#,
        config.database_name, config.table_name
    );

    client.query(&create_table_sql).execute().await?;

    info!(
        "✅ ClickHouse 数据库 {}.{} 设置完成",
        config.database_name, config.table_name
    );
    Ok(())
}

/// 性能监控任务
async fn performance_monitoring_task(
    stats: Arc<RealTimePerformanceStats>,
    config: ComprehensiveTestConfig,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(config.monitoring_interval_secs));

    while stats.is_test_running() {
        interval.tick().await;
        stats.print_detailed_stats().await;
    }

    // 最终报告
    info!("📋 === 最终性能报告 ===");
    stats.print_detailed_stats().await;
    generate_optimization_suggestions(&stats, &config).await;
}

/// 生成优化建议
async fn generate_optimization_suggestions(
    stats: &RealTimePerformanceStats,
    config: &ComprehensiveTestConfig,
) {
    let total_messages = stats.total_messages_received.load(Ordering::Relaxed);
    let total_written = stats.total_records_written.load(Ordering::Relaxed);
    let db_errors = stats.db_write_error_count.load(Ordering::Relaxed);
    let max_queue = stats.max_queue_size.load(Ordering::Relaxed);
    let duration = stats.get_test_duration().await;

    let message_rate = if duration.as_secs_f64() > 0.0 {
        total_messages as f64 / duration.as_secs_f64()
    } else {
        0.0
    };

    let write_success_rate = if total_messages > 0 {
        (total_written as f64 / total_messages as f64) * 100.0
    } else {
        0.0
    };

    let error_rate = if total_written > 0 {
        (db_errors as f64 / total_written as f64) * 100.0
    } else {
        0.0
    };

    info!("🔍 === 自动化性能分析和优化建议 ===");

    // 吞吐量分析
    if message_rate > 50000.0 {
        info!("🏆 优秀: 消息处理率 {:.1} msg/s 表现优异", message_rate);
    } else if message_rate > 20000.0 {
        info!("✅ 良好: 消息处理率 {:.1} msg/s 表现良好", message_rate);
    } else if message_rate > 5000.0 {
        info!(
            "⚠️  中等: 消息处理率 {:.1} msg/s，建议优化网络或解析性能",
            message_rate
        );
    } else {
        info!("❌ 需要优化: 消息处理率 {:.1} msg/s 过低", message_rate);
        info!("   建议:");
        info!("   - 检查网络连接质量");
        info!("   - 优化消息解析逻辑");
        info!("   - 增加连接并发数");
    }

    // 写入性能分析
    if write_success_rate > 95.0 {
        info!("🏆 优秀: 写入成功率 {:.1}% 表现优异", write_success_rate);
    } else if write_success_rate > 90.0 {
        info!("✅ 良好: 写入成功率 {:.1}% 表现良好", write_success_rate);
    } else {
        info!("❌ 需要优化: 写入成功率 {:.1}% 过低", write_success_rate);
        info!("   建议:");
        info!("   - 增加批量写入大小");
        info!("   - 优化ClickHouse配置");
        info!("   - 检查磁盘I/O性能");
    }

    // 错误率分析
    if error_rate < 0.1 {
        info!("🏆 优秀: 错误率 {:.2}% 极低", error_rate);
    } else if error_rate < 1.0 {
        info!("✅ 良好: 错误率 {:.2}% 在可接受范围", error_rate);
    } else {
        info!("❌ 需要优化: 错误率 {:.2}% 过高", error_rate);
        info!("   建议:");
        info!("   - 检查数据库连接池配置");
        info!("   - 增加重试机制");
        info!("   - 监控数据库资源使用");
    }

    // 队列管理分析
    if max_queue < config.batch_size as u64 * 2 {
        info!("🏆 优秀: 最大队列长度 {} 控制良好", max_queue);
    } else if max_queue < config.batch_size as u64 * 5 {
        info!("⚠️  注意: 最大队列长度 {} 较高，建议优化", max_queue);
        info!("   建议:");
        info!("   - 增加写入器数量");
        info!("   - 减少批量超时时间");
    } else {
        info!("❌ 需要优化: 最大队列长度 {} 过高", max_queue);
        info!("   建议:");
        info!("   - 显著增加写入器数量");
        info!("   - 优化批量写入策略");
        info!("   - 考虑增加内存缓冲");
    }

    // 系统建议
    info!("🛠️  系统级优化建议:");

    if config.writer_count < 8 && message_rate > 30000.0 {
        info!("   - 建议增加数据库写入器数量到 8-12 个");
    }

    if config.batch_size < 1000 && error_rate < 1.0 {
        info!("   - 可以尝试增加批量大小到 1000-2000");
    }

    if config.target_symbols_count > 30 && stats.failed_connections.load(Ordering::Relaxed) > 2 {
        info!("   - 考虑分批建立连接，避免同时连接过多");
    }

    info!("════════════════════════════════════");
}

/// 主测试函数
pub async fn run_comprehensive_database_stress_test(config: ComprehensiveTestConfig) -> Result<()> {
    info!("🚀 启动 20+商品 × 15分钟完整数据库压力测试");
    info!("📊 测试配置: {:?}", config);

    let stats = Arc::new(RealTimePerformanceStats::new());
    stats.start_test().await;

    // 初始化ClickHouse
    let client = Arc::new(Client::default().with_url(&config.clickhouse_url));
    setup_clickhouse_database(&client, &config).await?;

    // 获取目标商品和通道
    let symbols = get_target_symbols();
    let channels = get_test_channels();

    info!("🎯 目标商品: {} 个", symbols.len());
    info!("📡 测试通道: {:?}", channels);

    // 创建数据管道 - 使用broadcast channel以便多个消费者
    let (broadcast_tx, _) =
        tokio::sync::broadcast::channel(config.batch_size * config.writer_count * 3);
    let (tx, mut main_rx) = mpsc::channel(config.batch_size * config.writer_count * 3);

    // 启动性能监控
    let monitoring_stats = stats.clone();
    let monitoring_config = config.clone();
    let monitoring_handle = tokio::spawn(async move {
        performance_monitoring_task(monitoring_stats, monitoring_config).await;
    });

    // 启动数据库写入器
    let mut writer_handles = Vec::new();
    for i in 0..config.writer_count {
        let writer_rx = broadcast_tx.subscribe();
        let client_clone = client.clone();
        let config_clone = config.clone();
        let stats_clone = stats.clone();

        let handle = tokio::spawn(async move {
            batch_database_writer_broadcast(writer_rx, client_clone, config_clone, stats_clone, i)
                .await
        });
        writer_handles.push(handle);
    }

    // 启动数据转发器（从main_rx转发到broadcast_tx）
    let forwarder_tx = broadcast_tx.clone();
    let forwarder_handle = tokio::spawn(async move {
        while let Some(record) = main_rx.recv().await {
            // 忽略发送错误（如果没有接收器）
            let _ = forwarder_tx.send(record);
        }
    });

    // 启动所有商品的数据收集器
    let mut collector_handles = Vec::new();
    for symbol in symbols {
        let tx_clone = tx.clone();
        let channels_clone = channels.clone();
        let stats_clone = stats.clone();

        let handle = tokio::spawn(async move {
            run_symbol_data_collector(symbol, channels_clone, tx_clone, stats_clone).await
        });
        collector_handles.push(handle);

        // 错开启动时间，避免同时建立过多连接
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    info!("⏳ 测试将持续 {} 分钟...", config.test_duration_minutes);

    // 等待测试完成
    tokio::time::sleep(Duration::from_secs(config.test_duration_minutes * 60)).await;

    // 停止测试
    info!("🛑 测试时间结束，正在收集最终结果...");
    stats.stop_test();

    // 等待所有任务完成
    for handle in collector_handles {
        let _ = handle.await;
    }

    drop(tx); // 关闭发送端，让写入器完成剩余数据

    for handle in writer_handles {
        let _ = handle.await;
    }

    monitoring_handle.abort();

    info!("🎉 20+商品 × 15分钟数据库压力测试完成！");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化默认crypto provider
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install default crypto provider");

    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let config = ComprehensiveTestConfig::default();

    info!("🚀 准备启动完整数据库压力测试");
    info!("📋 测试参数:");
    info!("   测试时长: {} 分钟", config.test_duration_minutes);
    info!("   目标商品: {} 个", config.target_symbols_count);
    info!(
        "   ClickHouse: {}/{}.{}",
        config.clickhouse_url, config.database_name, config.table_name
    );
    info!("   批量大小: {}", config.batch_size);
    info!("   写入器数: {}", config.writer_count);

    match run_comprehensive_database_stress_test(config).await {
        Ok(()) => {
            info!("✅ 测试成功完成！");
        }
        Err(e) => {
            error!("❌ 测试失败: {}", e);
        }
    }

    Ok(())
}
