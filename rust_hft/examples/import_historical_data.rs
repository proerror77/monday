use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use rust_hft::database::{ClickHouseClient, ClickHouseConfig, LobDepthRow, TradeDataRow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

/// 歷史資料導入工具
/// 
/// 將下載的歷史資料文件導入到 ClickHouse 資料庫
/// 
/// 使用示例：
/// ```bash
/// # 啟動 ClickHouse
/// docker-compose up -d
/// 
/// # 導入資料
/// cargo run --example import_historical_data -- \
///   --input-dir ./historical_data --symbol SOLUSDT \
///   --clickhouse-url http://localhost:8123
/// ```
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 輸入目錄 (包含 .jsonl 文件)
    #[arg(short, long)]
    input_dir: String,

    /// 交易對符號 (例如: SOLUSDT, BTCUSDT)
    #[arg(short, long)]
    symbol: String,

    /// ClickHouse 連接 URL
    #[arg(long, default_value = "http://localhost:8123")]
    clickhouse_url: String,

    /// 資料庫名稱
    #[arg(long, default_value = "hft_db")]
    database: String,

    /// 使用者名稱
    #[arg(long, default_value = "hft_user")]
    username: String,

    /// 密碼
    #[arg(long, default_value = "hft_password")]
    password: String,

    /// 批量大小
    #[arg(long, default_value = "10000")]
    batch_size: usize,

    /// 最大並行處理數
    #[arg(long, default_value = "4")]
    max_concurrent: usize,

    /// 是否跳過已存在的資料
    #[arg(long, default_value = "false")]
    skip_existing: bool,

    /// 資料類型過濾 (depth, trade, 或 all)
    #[arg(long, default_value = "all")]
    data_type: String,

    /// 是否進行資料驗證
    #[arg(long, default_value = "true")]
    validate_data: bool,
}

/// 統一的歷史資料結構 (與下載器匹配)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnifiedHistoricalData {
    timestamp: u64,
    symbol: String,
    data_type: String,
    raw_data: String,
    processed_data: serde_json::Value,
}

/// 資料導入統計
#[derive(Debug, Default)]
struct ImportStatistics {
    total_files: usize,
    total_records: usize,
    successful_records: usize,
    failed_records: usize,
    lob_depth_records: usize,
    trade_records: usize,
    processing_time: std::time::Duration,
}

/// 歷史資料導入器
struct HistoricalDataImporter {
    client: Arc<ClickHouseClient>,
    config: ClickHouseConfig,
    batch_size: usize,
    max_concurrent: usize,
    skip_existing: bool,
    validate_data: bool,
}

impl HistoricalDataImporter {
    /// 創建新的導入器
    async fn new(args: &Args) -> Result<Self> {
        let config = ClickHouseConfig {
            url: args.clickhouse_url.clone(),
            database: args.database.clone(),
            username: args.username.clone(),
            password: args.password.clone(),
            batch_size: args.batch_size,
            max_connections: args.max_concurrent,
            flush_interval_ms: 5000,
        };

        let client = Arc::new(ClickHouseClient::new(config.clone())?);
        
        // 測試連接
        client.test_connection().await?;
        info!("ClickHouse connection established successfully");

        // 創建表格
        client.create_tables().await?;
        info!("Database tables ready");

        Ok(Self {
            client,
            config,
            batch_size: args.batch_size,
            max_concurrent: args.max_concurrent,
            skip_existing: args.skip_existing,
            validate_data: args.validate_data,
        })
    }

    /// 掃描輸入目錄並找到所有 .jsonl 文件
    fn scan_input_files(&self, input_dir: &str, symbol: &str, data_type: &str) -> Result<Vec<PathBuf>> {
        let dir = PathBuf::from(input_dir);
        if !dir.exists() {
            return Err(anyhow::anyhow!("Input directory does not exist: {}", input_dir));
        }

        let mut files = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_file() {
                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                    // 檢查文件名模式: {symbol}_{data_type}_{date}.jsonl
                    if filename.ends_with(".jsonl") && filename.starts_with(symbol) {
                        let should_include = match data_type {
                            "all" => true,
                            "depth" => filename.contains("_depth_"),
                            "trade" => filename.contains("_trade_"),
                            _ => false,
                        };
                        
                        if should_include {
                            files.push(path);
                        }
                    }
                }
            }
        }

        files.sort();
        info!("Found {} files to process", files.len());
        Ok(files)
    }

    /// 處理單個文件
    async fn process_file(&self, file_path: &PathBuf, semaphore: Arc<Semaphore>) -> Result<ImportStatistics> {
        let _permit = semaphore.acquire().await?;
        
        let start_time = Instant::now();
        let mut stats = ImportStatistics::default();
        stats.total_files = 1;

        info!("Processing file: {}", file_path.display());

        let file = File::open(file_path)?;
        let reader = BufReader::new(file);

        let mut lob_batch = Vec::new();
        let mut trade_batch = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            stats.total_records += 1;

            match serde_json::from_str::<UnifiedHistoricalData>(&line) {
                Ok(data) => {
                    match data.data_type.as_str() {
                        "depth" => {
                            if let Some(lob_row) = self.convert_to_lob_row(&data)? {
                                lob_batch.push(lob_row);
                                stats.lob_depth_records += 1;
                            }
                        }
                        "trade" => {
                            if let Some(trade_row) = self.convert_to_trade_row(&data)? {
                                trade_batch.push(trade_row);
                                stats.trade_records += 1;
                            }
                        }
                        _ => {
                            warn!("Unknown data type: {}", data.data_type);
                        }
                    }

                    // 批量插入
                    if lob_batch.len() >= self.batch_size {
                        self.client.batch_insert_lob_depth(lob_batch.clone()).await?;
                        stats.successful_records += lob_batch.len();
                        lob_batch.clear();
                    }

                    if trade_batch.len() >= self.batch_size {
                        self.client.batch_insert_trade_data(trade_batch.clone()).await?;
                        stats.successful_records += trade_batch.len();
                        trade_batch.clear();
                    }
                }
                Err(e) => {
                    stats.failed_records += 1;
                    warn!("Failed to parse line: {} - Error: {}", line, e);
                }
            }
        }

        // 插入剩餘的資料
        if !lob_batch.is_empty() {
            self.client.batch_insert_lob_depth(lob_batch.clone()).await?;
            stats.successful_records += lob_batch.len();
        }

        if !trade_batch.is_empty() {
            self.client.batch_insert_trade_data(trade_batch.clone()).await?;
            stats.successful_records += trade_batch.len();
        }

        stats.processing_time = start_time.elapsed();
        info!(
            "File processed: {} - {} records in {:?}",
            file_path.display(),
            stats.total_records,
            stats.processing_time
        );

        Ok(stats)
    }

    /// 轉換為 LOB 深度行
    fn convert_to_lob_row(&self, data: &UnifiedHistoricalData) -> Result<Option<LobDepthRow>> {
        let processed = &data.processed_data;
        
        // 解析 bids 和 asks
        let bids = processed["bids"].as_array().ok_or_else(|| {
            anyhow::anyhow!("Missing bids array")
        })?;
        
        let asks = processed["asks"].as_array().ok_or_else(|| {
            anyhow::anyhow!("Missing asks array")
        })?;

        let mut bid_prices = Vec::new();
        let mut bid_quantities = Vec::new();
        let mut ask_prices = Vec::new();
        let mut ask_quantities = Vec::new();

        // 處理 bids
        for bid in bids {
            if let Some(level) = bid.as_array() {
                if level.len() >= 2 {
                    let price: f64 = level[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    let qty: f64 = level[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    bid_prices.push(price);
                    bid_quantities.push(qty);
                }
            }
        }

        // 處理 asks
        for ask in asks {
            if let Some(level) = ask.as_array() {
                if level.len() >= 2 {
                    let price: f64 = level[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    let qty: f64 = level[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    ask_prices.push(price);
                    ask_quantities.push(qty);
                }
            }
        }

        // 資料驗證
        if self.validate_data {
            if bid_prices.is_empty() || ask_prices.is_empty() {
                return Ok(None);
            }
        }

        let timestamp = DateTime::from_timestamp_micros(data.timestamp as i64)
            .ok_or_else(|| anyhow::anyhow!("Invalid timestamp"))?;

        let mid_price = processed["mid_price"].as_f64().unwrap_or(0.0);
        let spread = processed["spread"].as_f64().unwrap_or(0.0);
        let sequence = processed["sequence"].as_u64().unwrap_or(0);

        Ok(Some(LobDepthRow {
            timestamp,
            symbol: data.symbol.clone(),
            exchange: "bitget".to_string(),
            sequence,
            is_snapshot: 0, // 歷史資料通常是增量更新
            bid_prices,
            bid_quantities,
            ask_prices,
            ask_quantities,
            bid_levels: bid_prices.len() as u16,
            ask_levels: ask_prices.len() as u16,
            spread,
            mid_price,
            data_source: "historical".to_string(),
            created_at: Utc::now(),
        }))
    }

    /// 轉換為成交行
    fn convert_to_trade_row(&self, data: &UnifiedHistoricalData) -> Result<Option<TradeDataRow>> {
        let processed = &data.processed_data;
        
        let price: f64 = processed["price"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
        let quantity: f64 = processed["quantity"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
        let side = processed["side"].as_str().unwrap_or("unknown").to_string();
        let trade_id = processed["trade_id"].as_str().unwrap_or("").to_string();
        let notional = processed["notional"].as_f64().unwrap_or(price * quantity);

        // 資料驗證
        if self.validate_data {
            if price <= 0.0 || quantity <= 0.0 {
                return Ok(None);
            }
        }

        let timestamp = DateTime::from_timestamp_micros(data.timestamp as i64)
            .ok_or_else(|| anyhow::anyhow!("Invalid timestamp"))?;

        Ok(Some(TradeDataRow {
            timestamp,
            symbol: data.symbol.clone(),
            exchange: "bitget".to_string(),
            trade_id,
            price,
            quantity,
            side,
            notional,
            data_source: "historical".to_string(),
            created_at: Utc::now(),
        }))
    }

    /// 導入歷史資料
    async fn import_historical_data(
        &self,
        input_dir: &str,
        symbol: &str,
        data_type: &str,
    ) -> Result<ImportStatistics> {
        let files = self.scan_input_files(input_dir, symbol, data_type)?;
        
        if files.is_empty() {
            return Err(anyhow::anyhow!("No files found to import"));
        }

        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));
        let mut join_set = JoinSet::new();

        // 並行處理文件
        for file_path in files {
            let importer = self.clone();
            let semaphore = semaphore.clone();
            
            join_set.spawn(async move {
                importer.process_file(&file_path, semaphore).await
            });
        }

        // 聚合統計結果
        let mut total_stats = ImportStatistics::default();
        let start_time = Instant::now();

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(stats)) => {
                    total_stats.total_files += stats.total_files;
                    total_stats.total_records += stats.total_records;
                    total_stats.successful_records += stats.successful_records;
                    total_stats.failed_records += stats.failed_records;
                    total_stats.lob_depth_records += stats.lob_depth_records;
                    total_stats.trade_records += stats.trade_records;
                }
                Ok(Err(e)) => {
                    error!("File processing failed: {}", e);
                    total_stats.failed_records += 1;
                }
                Err(e) => {
                    error!("Join error: {}", e);
                    total_stats.failed_records += 1;
                }
            }
        }

        total_stats.processing_time = start_time.elapsed();

        // 強制刷新所有緩衝區
        self.client.flush_all().await?;

        Ok(total_stats)
    }
}

impl Clone for HistoricalDataImporter {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            config: self.config.clone(),
            batch_size: self.batch_size,
            max_concurrent: self.max_concurrent,
            skip_existing: self.skip_existing,
            validate_data: self.validate_data,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    // 驗證輸入
    if !["depth", "trade", "all"].contains(&args.data_type.as_str()) {
        return Err(anyhow::anyhow!("Data type must be 'depth', 'trade', or 'all'"));
    }

    info!("Starting historical data import...");
    info!("Input directory: {}", args.input_dir);
    info!("Symbol: {}", args.symbol);
    info!("Data type: {}", args.data_type);
    info!("ClickHouse URL: {}", args.clickhouse_url);
    info!("Batch size: {}", args.batch_size);
    info!("Max concurrent: {}", args.max_concurrent);

    // 創建導入器
    let importer = HistoricalDataImporter::new(&args).await?;

    // 執行導入
    let stats = importer
        .import_historical_data(&args.input_dir, &args.symbol, &args.data_type)
        .await?;

    // 顯示統計結果
    info!("Import completed successfully!");
    info!("=== Import Statistics ===");
    info!("Total files processed: {}", stats.total_files);
    info!("Total records: {}", stats.total_records);
    info!("Successful records: {}", stats.successful_records);
    info!("Failed records: {}", stats.failed_records);
    info!("LOB depth records: {}", stats.lob_depth_records);
    info!("Trade records: {}", stats.trade_records);
    info!("Processing time: {:?}", stats.processing_time);
    info!("Average records/sec: {:.2}", stats.total_records as f64 / stats.processing_time.as_secs_f64());

    // 獲取資料庫統計
    let db_stats = importer.client.get_statistics().await?;
    info!("=== Database Statistics ===");
    for (table, count) in db_stats {
        info!("{}: {}", table, count);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_import_statistics() {
        let stats = ImportStatistics::default();
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_records, 0);
        assert_eq!(stats.successful_records, 0);
        assert_eq!(stats.failed_records, 0);
    }
}