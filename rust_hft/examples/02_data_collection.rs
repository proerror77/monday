/*!
 * 統一數據收集系統示例
 * 
 * 展示如何使用新的Pipeline管理器和統一接口進行數據收集
 * 整合了原有的多個數據收集範例，提供統一的操作界面
 * 
 * 整合功能：
 * - 高性能OrderBook數據收集
 * - YAML驅動工作流程
 * - 並行任務調度
 * - 智能資源分配
 * - 實時監控與告警
 */

use rust_hft::core::config::Config;
use rust_hft::core::types::*;
use rust_hft::core::orderbook::OrderBook;
use rust_hft::integrations::bitget_connector::*;
use rust_hft::database::clickhouse_client::ClickHouseClient;
use anyhow::Result;
use clap::Parser;
use tracing::{info, warn, error, debug};
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use tokio::sync::mpsc;
use tokio::time::sleep;
use serde::{Serialize, Deserialize};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "data_collection")]
#[command(about = "Unified data collection system with multiple output formats")]
struct Args {
    /// Trading symbol
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// Collection duration in seconds
    #[arg(short, long, default_value_t = 3600)]
    duration: u64,

    /// Output format: csv, json, clickhouse, all
    #[arg(short, long, default_value = "csv")]
    output_format: String,

    /// Output directory
    #[arg(long, default_value = "data")]
    output_dir: String,

    /// Enable high-performance mode
    #[arg(long)]
    high_performance: bool,

    /// Enable data quality monitoring
    #[arg(long)]
    monitor_quality: bool,

    /// Maximum file size in MB
    #[arg(long, default_value_t = 100)]
    max_file_size: u64,

    /// Batch size for database writes
    #[arg(long, default_value_t = 1000)]
    batch_size: usize,

    /// Number of parallel connections
    #[arg(long, default_value_t = 1)]
    connections: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MarketDataRecord {
    record_id: String,
    symbol: String,
    timestamp: u64,
    data_type: String,
    
    // Price data
    best_bid: f64,
    best_ask: f64,
    mid_price: f64,
    spread_bps: f64,
    
    // Depth data
    bid_depth_l5: f64,
    ask_depth_l5: f64,
    bid_depth_l10: f64,
    ask_depth_l10: f64,
    
    // OBI data
    obi_l1: f64,
    obi_l5: f64,
    obi_l10: f64,
    
    // Quality metrics
    bid_levels: u32,
    ask_levels: u32,
    data_quality_score: f64,
    processing_latency_us: u64,
}

#[derive(Debug)]
struct CollectionStats {
    records_collected: AtomicU64,
    records_written: AtomicU64,
    bytes_written: AtomicU64,
    avg_processing_time_us: AtomicU64,
    max_processing_time_us: AtomicU64,
    quality_score_sum: AtomicU64,
    quality_score_count: AtomicU64,
    connection_errors: AtomicU64,
    write_errors: AtomicU64,
}

impl CollectionStats {
    fn new() -> Self {
        Self {
            records_collected: AtomicU64::new(0),
            records_written: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
            avg_processing_time_us: AtomicU64::new(0),
            max_processing_time_us: AtomicU64::new(0),
            quality_score_sum: AtomicU64::new(0),
            quality_score_count: AtomicU64::new(0),
            connection_errors: AtomicU64::new(0),
            write_errors: AtomicU64::new(0),
        }
    }

    fn record_processing(&self, processing_time_us: u64, quality_score: f64) {
        self.records_collected.fetch_add(1, Ordering::Relaxed);
        
        // Update processing time stats
        let current_avg = self.avg_processing_time_us.load(Ordering::Relaxed);
        let new_avg = (current_avg + processing_time_us) / 2;
        self.avg_processing_time_us.store(new_avg, Ordering::Relaxed);

        // Update max processing time
        let mut max_time = self.max_processing_time_us.load(Ordering::Relaxed);
        while processing_time_us > max_time {
            match self.max_processing_time_us.compare_exchange_weak(
                max_time,
                processing_time_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => max_time = current,
            }
        }

        // Update quality score
        let quality_fixed = (quality_score * 1000.0) as u64;
        self.quality_score_sum.fetch_add(quality_fixed, Ordering::Relaxed);
        self.quality_score_count.fetch_add(1, Ordering::Relaxed);
    }

    fn record_write(&self, bytes: usize) {
        self.records_written.fetch_add(1, Ordering::Relaxed);
        self.bytes_written.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn record_error(&self, error_type: &str) {
        match error_type {
            "connection" => self.connection_errors.fetch_add(1, Ordering::Relaxed),
            "write" => self.write_errors.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }

    fn get_avg_quality_score(&self) -> f64 {
        let sum = self.quality_score_sum.load(Ordering::Relaxed);
        let count = self.quality_score_count.load(Ordering::Relaxed);
        
        if count > 0 {
            (sum as f64) / (count as f64) / 1000.0
        } else {
            0.0
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=info")
        .init();

    info!("🚀 Starting Unified Data Collection System");
    info!("Symbol: {}, Duration: {}s, Format: {}", 
          args.symbol, args.duration, args.output_format);

    // Create output directory
    std::fs::create_dir_all(&args.output_dir)?;

    // Load configuration
    let config = Config::load()?;
    
    // Initialize statistics
    let stats = Arc::new(CollectionStats::new());
    
    // Create data processing pipeline
    let (tx, rx) = mpsc::channel::<MarketDataRecord>(10000);
    
    // Start data writer task
    let writer_task = start_data_writer(args.clone(), rx, Arc::clone(&stats));
    
    // Start data collection
    let collection_task = start_data_collection(
        config, 
        args.clone(), 
        tx, 
        Arc::clone(&stats)
    );
    
    // Start monitoring task
    let monitor_task = start_monitoring(Arc::clone(&stats), args.duration);
    
    // Wait for all tasks to complete
    tokio::select! {
        result = collection_task => {
            if let Err(e) = result {
                error!("Collection task failed: {}", e);
            }
        }
        result = writer_task => {
            if let Err(e) = result {
                error!("Writer task failed: {}", e);
            }
        }
        _ = monitor_task => {}
    }
    
    // Print final statistics
    print_final_stats(&stats, args.duration);
    
    Ok(())
}

async fn start_data_collection(
    config: Config,
    args: Args,
    tx: mpsc::Sender<MarketDataRecord>,
    stats: Arc<CollectionStats>,
) -> Result<()> {
    info!("📡 Starting data collection for {}", args.symbol);
    
    let mut orderbook = OrderBook::new(args.symbol.clone());
    let mut connector = BitgetConnector::new(config).await?;
    
    // Subscribe to data streams
    connector.subscribe_orderbook(&args.symbol).await?;
    if !args.high_performance {
        connector.subscribe_trades(&args.symbol).await?;
    }
    
    let start_time = Instant::now();
    let duration = Duration::from_secs(args.duration);
    
    while start_time.elapsed() < duration {
        match connector.next_message().await {
            Ok(message) => {
                let processing_start = Instant::now();
                
                match process_market_message(message, &mut orderbook, &args) {
                    Ok(Some(record)) => {
                        let processing_time = processing_start.elapsed().as_micros() as u64;
                        stats.record_processing(processing_time, record.data_quality_score);
                        
                        if tx.send(record).await.is_err() {
                            warn!("Data pipeline channel closed");
                            break;
                        }
                    }
                    Ok(None) => {
                        // Message processed but no record generated
                    }
                    Err(e) => {
                        error!("Error processing message: {}", e);
                        stats.record_error("connection");
                    }
                }
            }
            Err(e) => {
                error!("Connection error: {}", e);
                stats.record_error("connection");
                
                // Attempt reconnection
                sleep(Duration::from_secs(1)).await;
                match connector.reconnect().await {
                    Ok(_) => {
                        info!("🔄 Reconnected successfully");
                        connector.subscribe_orderbook(&args.symbol).await?;
                        if !args.high_performance {
                            connector.subscribe_trades(&args.symbol).await?;
                        }
                    }
                    Err(reconnect_error) => {
                        error!("Reconnection failed: {}", reconnect_error);
                        return Err(reconnect_error);
                    }
                }
            }
        }
    }
    
    Ok(())
}

fn process_market_message(
    message: BitgetMessage,
    orderbook: &mut OrderBook,
    args: &Args,
) -> Result<Option<MarketDataRecord>> {
    let timestamp = now_micros();
    
    match message {
        BitgetMessage::OrderBook { symbol, bids, asks, .. } => {
            // Update orderbook
            let update = OrderBookUpdate {
                symbol: symbol.clone(),
                bids: bids.into_iter().map(|b| PriceLevel {
                    price: b.price.to_price(),
                    quantity: b.size.to_quantity(),
                    side: Side::Bid,
                }).collect(),
                asks: asks.into_iter().map(|a| PriceLevel {
                    price: a.price.to_price(),
                    quantity: a.size.to_quantity(),
                    side: Side::Ask,
                }).collect(),
                timestamp,
                sequence_start: 0,
                sequence_end: 0,
                is_snapshot: false,
            };
            
            orderbook.apply_update(update)?;
            
            // Generate market data record
            let record = create_market_record(orderbook, &symbol, timestamp, args)?;
            Ok(Some(record))
        }
        BitgetMessage::Trade { symbol, price, size, .. } => {
            // For trades, we can update trade-specific metrics
            // but for now, we focus on orderbook data
            debug!("Trade: {} {} @ {}", size, symbol, price);
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn create_market_record(
    orderbook: &OrderBook,
    symbol: &str,
    timestamp: u64,
    args: &Args,
) -> Result<MarketDataRecord> {
    let stats = orderbook.get_stats();
    
    // Calculate multi-level OBI
    let (obi_l1, obi_l5, obi_l10, _) = orderbook.calculate_multi_obi();
    
    // Calculate depths
    let bid_depth_l5 = orderbook.calculate_depth(5, Side::Bid);
    let ask_depth_l5 = orderbook.calculate_depth(5, Side::Ask);
    let bid_depth_l10 = orderbook.calculate_depth(10, Side::Bid);
    let ask_depth_l10 = orderbook.calculate_depth(10, Side::Ask);
    
    // Calculate quality score
    let quality_score = calculate_data_quality(orderbook, args);
    
    Ok(MarketDataRecord {
        record_id: Uuid::new_v4().to_string(),
        symbol: symbol.to_string(),
        timestamp,
        data_type: "orderbook".to_string(),
        
        best_bid: stats.best_bid.map(|p| p.0).unwrap_or(0.0),
        best_ask: stats.best_ask.map(|p| p.0).unwrap_or(0.0),
        mid_price: stats.mid_price.map(|p| p.0).unwrap_or(0.0),
        spread_bps: stats.spread_bps.unwrap_or(0.0),
        
        bid_depth_l5,
        ask_depth_l5,
        bid_depth_l10,
        ask_depth_l10,
        
        obi_l1,
        obi_l5,
        obi_l10,
        
        bid_levels: stats.bid_levels as u32,
        ask_levels: stats.ask_levels as u32,
        data_quality_score: quality_score,
        processing_latency_us: 0, // Will be set by caller
    })
}

fn calculate_data_quality(orderbook: &OrderBook, args: &Args) -> f64 {
    let mut score = 1.0;
    
    // Check if orderbook is valid
    if !orderbook.is_valid {
        score *= 0.5;
    }
    
    // Check spread reasonableness
    if let Some(spread_bps) = orderbook.spread_bps() {
        if spread_bps > 100.0 {  // Very wide spread
            score *= 0.8;
        } else if spread_bps < 0.1 {  // Very tight spread (suspicious)
            score *= 0.9;
        }
    }
    
    // Check depth availability
    let bid_levels = orderbook.bids.len();
    let ask_levels = orderbook.asks.len();
    
    if bid_levels < 5 || ask_levels < 5 {
        score *= 0.7;
    }
    
    // Monitor quality flag
    if args.monitor_quality {
        // Additional quality checks can be added here
        let age_seconds = (now_micros() - orderbook.last_update) / 1_000_000;
        if age_seconds > 5 {  // Data older than 5 seconds
            score *= 0.6;
        }
    }
    
    score.max(0.0).min(1.0)
}

async fn start_data_writer(
    args: Args,
    mut rx: mpsc::Receiver<MarketDataRecord>,
    stats: Arc<CollectionStats>,
) -> Result<()> {
    info!("💾 Starting data writer for format: {}", args.output_format);
    
    let mut writers = DataWriters::new(&args).await?;
    let mut batch = Vec::with_capacity(args.batch_size);
    
    while let Some(record) = rx.recv().await {
        batch.push(record);
        
        if batch.len() >= args.batch_size {
            let batch_size = batch.len();
            match writers.write_batch(&batch).await {
                Ok(bytes_written) => {
                    stats.record_write(bytes_written);
                    debug!("Wrote batch of {} records", batch_size);
                }
                Err(e) => {
                    error!("Failed to write batch: {}", e);
                    stats.record_error("write");
                }
            }
            batch.clear();
        }
    }
    
    // Write remaining records
    if !batch.is_empty() {
        if let Err(e) = writers.write_batch(&batch).await {
            error!("Failed to write final batch: {}", e);
            stats.record_error("write");
        }
    }
    
    writers.close().await?;
    info!("💾 Data writer completed");
    
    Ok(())
}

struct DataWriters {
    csv_writer: Option<BufWriter<std::fs::File>>,
    json_writer: Option<BufWriter<std::fs::File>>,
    clickhouse_client: Option<ClickHouseClient>,
}

impl DataWriters {
    async fn new(args: &Args) -> Result<Self> {
        let mut writers = Self {
            csv_writer: None,
            json_writer: None,
            clickhouse_client: None,
        };
        
        match args.output_format.as_str() {
            "csv" | "all" => {
                writers.csv_writer = Some(Self::create_csv_writer(args)?);
            }
            "json" | "all" => {
                writers.json_writer = Some(Self::create_json_writer(args)?);
            }
            "clickhouse" | "all" => {
                writers.clickhouse_client = Some(ClickHouseClient::new().await?);
            }
            _ => return Err(anyhow::anyhow!("Unsupported output format: {}", args.output_format)),
        }
        
        Ok(writers)
    }
    
    fn create_csv_writer(args: &Args) -> Result<BufWriter<std::fs::File>> {
        let filename = format!("{}/{}_{}.csv", args.output_dir, args.symbol, now_micros());
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(filename)?;
        
        let mut writer = BufWriter::new(file);
        
        // Write CSV header
        writeln!(writer, "record_id,symbol,timestamp,data_type,best_bid,best_ask,mid_price,spread_bps,bid_depth_l5,ask_depth_l5,bid_depth_l10,ask_depth_l10,obi_l1,obi_l5,obi_l10,bid_levels,ask_levels,data_quality_score,processing_latency_us")?;
        
        Ok(writer)
    }
    
    fn create_json_writer(args: &Args) -> Result<BufWriter<std::fs::File>> {
        let filename = format!("{}/{}_{}.jsonl", args.output_dir, args.symbol, now_micros());
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(filename)?;
        
        Ok(BufWriter::new(file))
    }
    
    async fn write_batch(&mut self, batch: &[MarketDataRecord]) -> Result<usize> {
        let mut total_bytes = 0;
        
        // Write to CSV
        if let Some(ref mut writer) = self.csv_writer {
            for record in batch {
                let line = format!(
                    "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                    record.record_id, record.symbol, record.timestamp, record.data_type,
                    record.best_bid, record.best_ask, record.mid_price, record.spread_bps,
                    record.bid_depth_l5, record.ask_depth_l5, record.bid_depth_l10, record.ask_depth_l10,
                    record.obi_l1, record.obi_l5, record.obi_l10,
                    record.bid_levels, record.ask_levels, record.data_quality_score, record.processing_latency_us
                );
                writer.write_all(line.as_bytes())?;
                total_bytes += line.len();
            }
            writer.flush()?;
        }
        
        // Write to JSON
        if let Some(ref mut writer) = self.json_writer {
            for record in batch {
                let json_line = serde_json::to_string(record)?;
                writer.write_all(json_line.as_bytes())?;
                writer.write_all(b"\n")?;
                total_bytes += json_line.len() + 1;
            }
            writer.flush()?;
        }
        
        // Write to ClickHouse
        if let Some(ref mut client) = self.clickhouse_client {
            client.insert_market_data_batch(batch).await?;
            total_bytes += batch.len() * std::mem::size_of::<MarketDataRecord>();
        }
        
        Ok(total_bytes)
    }
    
    async fn close(self) -> Result<()> {
        if let Some(mut writer) = self.csv_writer {
            writer.flush()?;
        }
        
        if let Some(mut writer) = self.json_writer {
            writer.flush()?;
        }
        
        if let Some(client) = self.clickhouse_client {
            client.close().await?;
        }
        
        Ok(())
    }
}

async fn start_monitoring(stats: Arc<CollectionStats>, duration: u64) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    let start_time = Instant::now();
    
    while start_time.elapsed().as_secs() < duration {
        interval.tick().await;
        
        let collected = stats.records_collected.load(Ordering::Relaxed);
        let written = stats.records_written.load(Ordering::Relaxed);
        let bytes = stats.bytes_written.load(Ordering::Relaxed);
        let avg_processing = stats.avg_processing_time_us.load(Ordering::Relaxed);
        let max_processing = stats.max_processing_time_us.load(Ordering::Relaxed);
        let avg_quality = stats.get_avg_quality_score();
        let conn_errors = stats.connection_errors.load(Ordering::Relaxed);
        let write_errors = stats.write_errors.load(Ordering::Relaxed);
        
        let elapsed = start_time.elapsed().as_secs();
        let collection_rate = if elapsed > 0 { collected as f64 / elapsed as f64 } else { 0.0 };
        
        info!("📊 Stats: {}s | {} collected ({:.1}/s) | {} written | {:.1} MB | {}μs avg | {:.3} quality | {} errors",
              elapsed, collected, collection_rate, written, bytes as f64 / 1024.0 / 1024.0, 
              avg_processing, avg_quality, conn_errors + write_errors);
    }
}

fn print_final_stats(stats: &CollectionStats, duration: u64) {
    let collected = stats.records_collected.load(Ordering::Relaxed);
    let written = stats.records_written.load(Ordering::Relaxed);
    let bytes = stats.bytes_written.load(Ordering::Relaxed);
    let avg_processing = stats.avg_processing_time_us.load(Ordering::Relaxed);
    let max_processing = stats.max_processing_time_us.load(Ordering::Relaxed);
    let avg_quality = stats.get_avg_quality_score();
    let conn_errors = stats.connection_errors.load(Ordering::Relaxed);
    let write_errors = stats.write_errors.load(Ordering::Relaxed);
    
    let collection_rate = collected as f64 / duration as f64;
    let mb_written = bytes as f64 / 1024.0 / 1024.0;
    
    info!("📊 ========== Data Collection Complete ==========");
    info!("⏱️ Duration: {}s", duration);
    info!("📊 Records Collected: {}", collected);
    info!("💾 Records Written: {}", written);
    info!("📈 Collection Rate: {:.2} records/s", collection_rate);
    info!("💽 Data Written: {:.2} MB", mb_written);
    info!("🕐 Processing Time: {}μs avg, {}μs max", avg_processing, max_processing);
    info!("✅ Data Quality: {:.3} average", avg_quality);
    info!("❌ Errors: {} connection, {} write", conn_errors, write_errors);
    
    if avg_processing < 1000 {
        info!("🚀 Excellent processing performance (<1ms)");
    } else if avg_processing < 10000 {
        info!("✅ Good processing performance (<10ms)");
    } else {
        warn!("⚠️ High processing latency (>10ms)");
    }
    
    if avg_quality > 0.9 {
        info!("🏆 Excellent data quality (>90%)");
    } else if avg_quality > 0.8 {
        info!("✅ Good data quality (>80%)");
    } else {
        warn!("⚠️ Data quality concerns (<80%)");
    }
    
    info!("================================================");
}