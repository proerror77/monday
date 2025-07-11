use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use clap::Parser;
use reqwest::Client;
use rust_hft::database::{ClickHouseClient, ClickHouseConfig, LobDepthRow, TradeDataRow};
use rust_hft::core::{OrderBookUpdate, PriceLevel, Side, Price, Quantity, ConfigLoader, ResolvedConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};
use zip::ZipArchive;
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;
use calamine::{Reader, Xlsx, open_workbook, Data};

/// Bitget 歷史資料下載器
/// 
/// 支持下載深度數據和成交數據，自動解壓縮和格式轉換
/// 可選直接導入到 ClickHouse 資料庫，包含數據驗證和清理功能
/// 
/// 使用示例：
/// ```bash
/// # 僅下載到文件
/// cargo run --example download_historical_lob -- \
///   --symbol SOLUSDT --start-date 2024-01-01 --end-date 2024-01-31 \
///   --data-type depth --output-dir ./historical_data
///
/// # 下載並直接導入 ClickHouse
/// cargo run --example download_historical_lob -- \
///   --symbol SOLUSDT --start-date 2024-01-01 --end-date 2024-01-31 \
///   --data-type depth \
///   --clickhouse-url http://localhost:8123
///
/// # 使用配置文件並進行完整的數據管道（下載→入庫→驗證→清理）
/// cargo run --example download_historical_lob -- \
///   -c clickhouse/config/historical_data_config.yaml \
///   -p solusdt_30days_depth \
///   --verify --cleanup-after-verify
///
/// # 下載多種數據類型並顯示統計
/// cargo run --example download_historical_lob -- \
///   -c config.yaml -p profile_name \
///   --show-stats --verify
/// ```
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// 配置文件路徑 (YAML 格式)
    #[arg(short = 'c', long)]
    config: Option<String>,

    /// 配置 profile 名稱
    #[arg(short = 'p', long, default_value = "default")]
    profile: String,

    /// 列出配置文件中的所有 profiles
    #[arg(long)]
    list_profiles: bool,

    /// 創建默認配置文件
    #[arg(long)]
    create_config: Option<String>,

    // === 以下為命令行覆蓋參數 (可選，會覆蓋配置文件中的設置) ===
    
    /// 交易對符號 (例如: SOLUSDT, BTCUSDT)
    #[arg(short, long)]
    symbol: Option<String>,

    /// 開始日期 (YYYY-MM-DD)
    #[arg(long)]
    start_date: Option<String>,

    /// 結束日期 (YYYY-MM-DD)
    #[arg(long)]
    end_date: Option<String>,

    /// 資料類型 (depth, trade, all)
    #[arg(short = 't', long)]
    data_type: Option<String>,

    /// 輸出目錄
    #[arg(short, long)]
    output_dir: Option<String>,

    /// 最大並行下載數
    #[arg(short, long)]
    max_concurrent: Option<usize>,

    /// 是否跳過已存在的文件
    #[arg(long)]
    skip_existing: Option<bool>,

    /// 是否刪除原始 ZIP 文件
    #[arg(long)]
    cleanup_zip: Option<bool>,

    /// ClickHouse 連接 URL (如果提供，將直接導入資料庫)
    #[arg(long)]
    clickhouse_url: Option<String>,

    /// ClickHouse 資料庫名稱
    #[arg(long)]
    clickhouse_db: Option<String>,

    /// ClickHouse 使用者名稱
    #[arg(long)]
    clickhouse_user: Option<String>,

    /// ClickHouse 密碼
    #[arg(long)]
    clickhouse_password: Option<String>,

    /// 批量插入大小
    #[arg(long)]
    batch_size: Option<usize>,

    /// 重試次數
    #[arg(long)]
    max_retries: Option<usize>,

    /// 下載完成後驗證數據質量
    #[arg(long)]
    verify: bool,

    /// 驗證後清理下載的原始文件（節省空間）
    #[arg(long)]
    cleanup_after_verify: bool,

    /// 顯示數據庫統計信息
    #[arg(long)]
    show_stats: bool,
}

/// Bitget 深度資料結構
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BitgetDepthData {
    /// 時間戳 (毫秒)
    #[serde(alias = "ts")]
    timestamp: u64,
    
    /// 買盤深度 [[price, quantity], ...]
    #[serde(alias = "bids")]
    bids: Vec<[String; 2]>,
    
    /// 賣盤深度 [[price, quantity], ...]
    #[serde(alias = "asks")]
    asks: Vec<[String; 2]>,
    
    /// 序列號
    #[serde(alias = "seqId")]
    sequence: Option<u64>,
    
    /// 校驗和
    #[serde(alias = "checksum")]
    checksum: Option<u32>,
}

/// Bitget 成交資料結構
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BitgetTradeData {
    /// 時間戳 (毫秒)
    #[serde(alias = "ts")]
    timestamp: u64,
    
    /// 成交ID
    #[serde(alias = "tradeId")]
    trade_id: String,
    
    /// 價格
    #[serde(alias = "price")]
    price: String,
    
    /// 數量
    #[serde(alias = "size")]
    quantity: String,
    
    /// 買賣方向
    #[serde(alias = "side")]
    side: String,
}

/// 統一的歷史資料結構
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnifiedHistoricalData {
    /// 時間戳 (微秒)
    timestamp: u64,
    
    /// 交易對
    symbol: String,
    
    /// 資料類型
    data_type: String,
    
    /// 原始資料 (JSON 字符串)
    raw_data: String,
    
    /// 處理後的資料
    processed_data: serde_json::Value,
}

/// 歷史資料下載器
struct HistoricalDownloader {
    client: Client,
    base_url: String,
    output_dir: PathBuf,
    max_concurrent: usize,
    skip_existing: bool,
    cleanup_zip: bool,
    max_retries: usize,
    clickhouse_client: Option<Arc<ClickHouseClient>>,
    batch_size: usize,
}

impl HistoricalDownloader {

    /// 生成下載 URL
    fn generate_download_url(&self, symbol: &str, data_type: &str, date: &NaiveDate) -> String {
        let date_str = date.format("%Y%m%d").to_string();
        match data_type {
            "depth" => format!("{}/depth/{}/1/{}.zip", self.base_url, symbol, date_str),
            "trade" => {
                // 成交數據的 URL 格式: SPBL/SOLUSDT/20250618_007.zip
                let year_month_day = date.format("%Y%m%d").to_string();
                format!("{}/trades/SPBL/{}/{}_007.zip", self.base_url, symbol, year_month_day)
            },
            _ => panic!("Unsupported data type: {}", data_type),
        }
    }

    /// 下載單個文件
    async fn download_file(&self, url: &str, output_path: &Path) -> Result<()> {
        let mut retries = 0;
        
        loop {
            match self.client.get(url).send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        let content = response.bytes().await?;
                        
                        // 確保輸出目錄存在
                        if let Some(parent) = output_path.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        
                        // 寫入文件
                        let mut file = File::create(output_path)?;
                        file.write_all(&content)?;
                        
                        info!("Downloaded: {} -> {}", url, output_path.display());
                        return Ok(());
                    } else if response.status().as_u16() == 404 {
                        warn!("File not found: {}", url);
                        return Err(anyhow::anyhow!("File not found: {}", url));
                    } else {
                        error!("HTTP error {}: {}", response.status(), url);
                        return Err(anyhow::anyhow!("HTTP error {}: {}", response.status(), url));
                    }
                }
                Err(e) => {
                    retries += 1;
                    if retries > self.max_retries {
                        error!("Failed to download {} after {} retries: {}", url, self.max_retries, e);
                        return Err(anyhow::anyhow!("Failed to download {}: {}", url, e));
                    }
                    
                    warn!("Retry {}/{} for {}: {}", retries, self.max_retries, url, e);
                    tokio::time::sleep(Duration::from_secs(retries as u64 * 2)).await;
                }
            }
        }
    }

    /// 解壓縮 ZIP 文件
    fn extract_zip(&self, zip_path: &Path, output_dir: &Path) -> Result<Vec<PathBuf>> {
        let file = File::open(zip_path)?;
        let mut archive = ZipArchive::new(file)?;
        let mut extracted_files = Vec::new();

        fs::create_dir_all(output_dir)?;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let outpath = output_dir.join(file.mangled_name());

            if file.is_file() {
                let mut outfile = File::create(&outpath)?;
                std::io::copy(&mut file, &mut outfile)?;
                debug!("Extracted: {}", outpath.display());
                extracted_files.push(outpath);
            }
        }

        Ok(extracted_files)
    }

    /// 解析深度資料 (支持 Excel 和 JSON 格式)
    fn parse_depth_data(&self, file_path: &Path, symbol: &str) -> Result<Vec<UnifiedHistoricalData>> {
        let file_extension = file_path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");

        match file_extension.to_lowercase().as_str() {
            "xlsx" | "xls" => self.parse_excel_depth_data(file_path, symbol),
            _ => self.parse_json_depth_data(file_path, symbol),
        }
    }

    /// 解析 Excel 格式的深度資料
    fn parse_excel_depth_data(&self, file_path: &Path, symbol: &str) -> Result<Vec<UnifiedHistoricalData>> {
        let mut workbook: Xlsx<_> = open_workbook(file_path)?;
        let mut results = Vec::new();

        // 獲取第一個工作表
        if let Some(Ok(range)) = workbook.worksheet_range_at(0) {
            debug!("Excel file dimensions: {} rows x {} columns", range.height(), range.width());
            
            // 先檢查前幾行的內容來理解格式
            debug!("First 5 rows content:");
            for row_idx in 0..std::cmp::min(5, range.height()) {
                if let Some(row) = range.rows().nth(row_idx) {
                    let row_content: Vec<String> = row.iter()
                        .map(|cell| format!("{:?}", cell))
                        .collect();
                    debug!("Row {}: {:?}", row_idx, row_content);
                }
            }
            
            // 跳過標題行，從第二行開始讀取
            for row_idx in 1..range.height() {
                if let Some(row) = range.rows().nth(row_idx) {
                    // Bitget Excel 格式: timestamp, ask_price, bid_price, ask_volume, bid_volume
                    if row.len() >= 5 {
                        if let (Some(timestamp_cell), Some(ask_price_cell), Some(bid_price_cell), 
                                Some(ask_volume_cell), Some(bid_volume_cell)) = 
                            (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4)) {
                            
                            // 解析時間戳
                            if let Some(timestamp_ms) = Self::parse_excel_timestamp(timestamp_cell) {
                                // 解析價格和數量
                                if let (Some(ask_price), Some(bid_price), Some(ask_volume), Some(bid_volume)) = (
                                    Self::parse_excel_number(ask_price_cell),
                                    Self::parse_excel_number(bid_price_cell),
                                    Self::parse_excel_number(ask_volume_cell),
                                    Self::parse_excel_number(bid_volume_cell)
                                ) {
                                    // 構建標準的訂單簿格式
                                    let bids = vec![[bid_price.to_string(), bid_volume.to_string()]];
                                    let asks = vec![[ask_price.to_string(), ask_volume.to_string()]];
                                    
                                    let unified = UnifiedHistoricalData {
                                        timestamp: timestamp_ms * 1000, // 轉換為微秒
                                        symbol: symbol.to_string(),
                                        data_type: "depth".to_string(),
                                        raw_data: format!("Excel Row {}: ts={}, ask={}@{}, bid={}@{}", 
                                                         row_idx + 1, timestamp_ms, ask_price, ask_volume, bid_price, bid_volume),
                                        processed_data: serde_json::json!({
                                            "bids": bids,
                                            "asks": asks,
                                            "mid_price": (ask_price + bid_price) / 2.0,
                                            "spread": ask_price - bid_price,
                                        }),
                                    };
                                    results.push(unified);
                                }
                            }
                        }
                    }
                }
            }
        }

        info!("Parsed {} depth records from Excel file: {}", results.len(), file_path.display());
        Ok(results)
    }

    /// 解析 JSON 格式的深度資料
    fn parse_json_depth_data(&self, file_path: &Path, symbol: &str) -> Result<Vec<UnifiedHistoricalData>> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        let mut results = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<BitgetDepthData>(&line) {
                Ok(depth_data) => {
                    let unified = UnifiedHistoricalData {
                        timestamp: depth_data.timestamp * 1000, // 轉換為微秒
                        symbol: symbol.to_string(),
                        data_type: "depth".to_string(),
                        raw_data: line.clone(),
                        processed_data: serde_json::json!({
                            "bids": depth_data.bids,
                            "asks": depth_data.asks,
                            "sequence": depth_data.sequence,
                            "checksum": depth_data.checksum,
                            "mid_price": calculate_mid_price(&depth_data.bids, &depth_data.asks),
                            "spread": calculate_spread(&depth_data.bids, &depth_data.asks),
                        }),
                    };
                    results.push(unified);
                }
                Err(e) => {
                    warn!("Failed to parse depth data line: {} - Error: {}", line, e);
                }
            }
        }

        Ok(results)
    }

    /// 解析成交資料
    fn parse_trade_data(&self, file_path: &Path, symbol: &str) -> Result<Vec<UnifiedHistoricalData>> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        let mut results = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<BitgetTradeData>(&line) {
                Ok(trade_data) => {
                    let unified = UnifiedHistoricalData {
                        timestamp: trade_data.timestamp * 1000, // 轉換為微秒
                        symbol: symbol.to_string(),
                        data_type: "trade".to_string(),
                        raw_data: line.clone(),
                        processed_data: serde_json::json!({
                            "trade_id": trade_data.trade_id,
                            "price": trade_data.price,
                            "quantity": trade_data.quantity,
                            "side": trade_data.side,
                            "notional": calculate_notional(&trade_data.price, &trade_data.quantity),
                        }),
                    };
                    results.push(unified);
                }
                Err(e) => {
                    warn!("Failed to parse trade data line: {} - Error: {}", line, e);
                }
            }
        }

        Ok(results)
    }

    /// 轉換為 ClickHouse LOB 行
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
                    if price > 0.0 && qty > 0.0 {
                        bid_prices.push(price);
                        bid_quantities.push(qty);
                    }
                }
            }
        }

        // 處理 asks
        for ask in asks {
            if let Some(level) = ask.as_array() {
                if level.len() >= 2 {
                    let price: f64 = level[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    let qty: f64 = level[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                    if price > 0.0 && qty > 0.0 {
                        ask_prices.push(price);
                        ask_quantities.push(qty);
                    }
                }
            }
        }

        // 資料驗證
        if bid_prices.is_empty() || ask_prices.is_empty() {
            return Ok(None);
        }

        // 原始數據是毫秒時間戳，需要轉換為秒
        let timestamp = DateTime::from_timestamp(data.timestamp as i64 / 1000, 0)
            .ok_or_else(|| anyhow::anyhow!("Invalid timestamp: {}", data.timestamp))?;

        let mid_price = processed["mid_price"].as_f64().unwrap_or_else(|| {
            (bid_prices[0] + ask_prices[0]) / 2.0
        });
        let spread = processed["spread"].as_f64().unwrap_or_else(|| {
            ask_prices[0] - bid_prices[0]
        });
        let sequence = processed["sequence"].as_u64().unwrap_or(0);

        let bid_levels = bid_prices.len() as u16;
        let ask_levels = ask_prices.len() as u16;

        // 使用正確的 DateTime64(6) 微秒精度轉換
        let timestamp_micros = timestamp.timestamp_micros();
        let created_at_secs = Utc::now().timestamp() as u32; // DateTime 為秒精度
        
        Ok(Some(LobDepthRow {
            timestamp: timestamp_micros,
            symbol: data.symbol.clone(),
            exchange: "bitget".to_string(),
            sequence,
            is_snapshot: 0, // 歷史資料通常是增量更新
            bid_prices,
            bid_quantities,
            ask_prices,
            ask_quantities,
            bid_levels,
            ask_levels,
            spread,
            mid_price,
            data_source: "historical".to_string(),
            created_at: created_at_secs,
        }))
    }

    /// 轉換為 ClickHouse 成交行
    fn convert_to_trade_row(&self, data: &UnifiedHistoricalData) -> Result<Option<TradeDataRow>> {
        let processed = &data.processed_data;
        
        let price: f64 = processed["price"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
        let quantity: f64 = processed["quantity"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
        let side = processed["side"].as_str().unwrap_or("unknown").to_string();
        let trade_id = processed["trade_id"].as_str().unwrap_or("").to_string();
        let notional = processed["notional"].as_f64().unwrap_or(price * quantity);

        // 資料驗證
        if price <= 0.0 || quantity <= 0.0 {
            return Ok(None);
        }

        // 原始數據是毫秒時間戳，需要轉換為秒
        let timestamp = DateTime::from_timestamp(data.timestamp as i64 / 1000, 0)
            .ok_or_else(|| anyhow::anyhow!("Invalid timestamp: {}", data.timestamp))?;

        // 使用正確的 DateTime64(6) 微秒精度轉換
        let timestamp_micros = timestamp.timestamp_micros();
        let created_at_secs = Utc::now().timestamp() as u32; // DateTime 為秒精度
        
        Ok(Some(TradeDataRow {
            timestamp: timestamp_micros,
            symbol: data.symbol.clone(),
            exchange: "bitget".to_string(),
            trade_id,
            price,
            quantity,
            side,
            notional,
            data_source: "historical".to_string(),
            created_at: created_at_secs,
        }))
    }

    /// 批量導入到 ClickHouse
    async fn batch_import_to_clickhouse(&self, data: &[UnifiedHistoricalData]) -> Result<usize> {
        if let Some(ref client) = self.clickhouse_client {
            let mut lob_rows = Vec::new();
            let mut trade_rows = Vec::new();

            for item in data {
                match item.data_type.as_str() {
                    "depth" => {
                        if let Some(row) = self.convert_to_lob_row(item)? {
                            lob_rows.push(row);
                        }
                    }
                    "trade" => {
                        if let Some(row) = self.convert_to_trade_row(item)? {
                            trade_rows.push(row);
                        }
                    }
                    _ => {
                        warn!("Unknown data type: {}", item.data_type);
                    }
                }
            }

            let mut imported_count = 0;

            // 批量插入 LOB 資料
            if !lob_rows.is_empty() {
                let lob_count = lob_rows.len();
                client.batch_insert_lob_depth(lob_rows).await?;
                imported_count += lob_count;
                debug!("Imported {} LOB depth records", lob_count);
            }

            // 批量插入成交資料
            if !trade_rows.is_empty() {
                let trade_count = trade_rows.len();
                client.batch_insert_trade_data(trade_rows).await?;
                imported_count += trade_count;
                debug!("Imported {} trade records", trade_count);
            }

            Ok(imported_count)
        } else {
            Ok(0)
        }
    }

    /// 處理單個日期的資料
    async fn process_single_date(
        &self,
        symbol: &str,
        data_type: &str,
        date: NaiveDate,
        semaphore: Arc<Semaphore>,
    ) -> Result<()> {
        let _permit = semaphore.acquire().await?;
        
        let url = self.generate_download_url(symbol, data_type, &date);
        let date_str = date.format("%Y%m%d").to_string();
        let zip_filename = format!("{}_{}_{}.zip", symbol, data_type, date_str);
        let zip_path = self.output_dir.join(&zip_filename);

        // 📅 開始處理日期日志
        info!("📅 開始處理 {} {} 數據 (日期: {})", symbol, data_type, date_str);
        info!("🔗 下載 URL: {}", url);

        // 檢查是否跳過已存在的文件
        if self.skip_existing && zip_path.exists() {
            info!("⏭️  跳過已存在文件: {}", zip_path.display());
            return Ok(());
        }

        // 下載 ZIP 文件
        info!("⬇️  開始下載 {}", zip_filename);
        if let Err(e) = self.download_file(&url, &zip_path).await {
            if e.to_string().contains("File not found") || e.to_string().contains("403") {
                warn!("❌ {} 日期 {} 數據不可用 (403/404)", symbol, date_str);
                return Ok(());
            }
            error!("❌ 下載失敗 {} 日期 {}: {}", symbol, date_str, e);
            return Err(e);
        }
        
        info!("✅ 下載完成: {}", zip_filename);

        // 解壓縮
        info!("📦 開始解壓縮到: {}_{}_{}/", symbol, data_type, date_str);
        let extract_dir = self.output_dir.join(format!("{}_{}_{}", symbol, data_type, date_str));
        let extracted_files = self.extract_zip(&zip_path, &extract_dir)?;
        info!("📂 解壓完成，發現 {} 個文件", extracted_files.len());

        // 解析和轉換資料
        let mut total_records = 0;
        let mut total_imported = 0;

        for (i, file_path) in extracted_files.iter().enumerate() {
            info!("📊 處理文件 {}/{}: {}", i + 1, extracted_files.len(), file_path.file_name().unwrap().to_string_lossy());
            
            let data = match data_type {
                "depth" => self.parse_depth_data(&file_path, symbol)?,
                "trade" => self.parse_trade_data(&file_path, symbol)?,
                _ => return Err(anyhow::anyhow!("Unsupported data type: {}", data_type)),
            };

            info!("📈 解析得到 {} 條記錄", data.len());
            total_records += data.len();

            // 如果有 ClickHouse 客戶端，直接導入資料庫
            if self.clickhouse_client.is_some() {
                info!("💾 開始導入 ClickHouse (批次大小: {})", self.batch_size);
                // 分批導入以避免內存問題
                for (batch_idx, chunk) in data.chunks(self.batch_size).enumerate() {
                    info!("   批次 {}: 導入 {} 條記錄...", batch_idx + 1, chunk.len());
                    let imported = self.batch_import_to_clickhouse(chunk).await?;
                    total_imported += imported;
                    info!("   ✅ 批次 {} 完成，成功導入 {} 條", batch_idx + 1, imported);
                }
            } else {
                // 否則儲存到文件
                let output_file = self.output_dir.join(format!("{}_{}_{}.jsonl", symbol, data_type, date_str));
                info!("💾 保存到文件: {}", output_file.display());
                let mut output = File::create(&output_file)?;
                
                for record in data {
                    let json_line = serde_json::to_string(&record)?;
                    writeln!(output, "{}", json_line)?;
                }
                info!("✅ 文件保存完成");
            }
        }

        if self.clickhouse_client.is_some() {
            info!("🎉 {} 日期 {} 處理完成: {} 條記錄，{} 條成功導入 ClickHouse", 
                  symbol, date_str, total_records, total_imported);
        } else {
            info!("🎉 {} 日期 {} 處理完成: {} 條記錄保存到文件", 
                  symbol, date_str, total_records);
        }

        // 清理 ZIP 文件
        if self.cleanup_zip {
            info!("🧹 清理臨時文件...");
            fs::remove_file(&zip_path)?;
            fs::remove_dir_all(&extract_dir)?;
            info!("✅ 清理完成");
        }

        Ok(())
    }

    /// 批量下載歷史資料
    async fn download_historical_data(
        &self,
        symbol: &str,
        data_type: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<()> {
        info!("🚀 開始歷史數據下載任務");
        info!("   交易對: {}", symbol);
        info!("   數據類型: {}", data_type);
        info!("   日期範圍: {} 到 {}", start_date, end_date);
        info!("   輸出目錄: {}", self.output_dir.display());
        info!("   最大並行數: {}", self.max_concurrent);

        // 確保輸出目錄存在
        fs::create_dir_all(&self.output_dir)?;

        // 生成日期範圍
        let mut dates = Vec::new();
        let mut current_date = start_date;
        while current_date <= end_date {
            dates.push(current_date);
            current_date += chrono::Duration::days(1);
        }

        let total_tasks = dates.len();
        info!("📋 總共需要處理 {} 個日期", total_tasks);

        // 使用信號量控制並行下載數
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));
        let mut join_set = JoinSet::new();

        for date in dates {
            let downloader = self.clone();
            let symbol = symbol.to_string();
            let data_type = data_type.to_string();
            let semaphore = semaphore.clone();

            join_set.spawn(async move {
                downloader.process_single_date(&symbol, &data_type, date, semaphore).await
            });
        }

        // 等待所有任務完成
        let mut success_count = 0;
        let mut error_count = 0;
        let mut processed = 0;

        info!("⏳ 等待所有下載任務完成...");

        while let Some(result) = join_set.join_next().await {
            processed += 1;
            match result {
                Ok(Ok(())) => {
                    success_count += 1;
                    info!("✅ 進度: {}/{} ({:.1}%)", processed, total_tasks, 
                          (processed as f64 / total_tasks as f64) * 100.0);
                },
                Ok(Err(e)) => {
                    error_count += 1;
                    error!("❌ 任務失敗 ({}/{}): {}", processed, total_tasks, e);
                }
                Err(e) => {
                    error_count += 1;
                    error!("❌ 執行錯誤 ({}/{}): {}", processed, total_tasks, e);
                }
            }
        }

        info!("🏁 下載任務全部完成!");
        info!("📊 最終統計:");
        info!("   ✅ 成功: {} 個日期", success_count);
        info!("   ❌ 失敗: {} 個日期", error_count);
        info!("   📈 成功率: {:.1}%", (success_count as f64 / total_tasks as f64) * 100.0);
        
        Ok(())
    }

    /// 解析 Excel 單元格中的時間戳
    fn parse_excel_timestamp(cell: &Data) -> Option<u64> {
        match cell {
            Data::Int(i) => Some(*i as u64),
            Data::Float(f) => Some(*f as u64),
            Data::String(s) => s.parse::<u64>().ok(),
            _ => None,
        }
    }

    /// 解析 Excel 單元格中的數字
    fn parse_excel_number(cell: &Data) -> Option<f64> {
        match cell {
            Data::Int(i) => Some(*i as f64),
            Data::Float(f) => Some(*f),
            Data::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    /// 解析 Excel 單元格中的訂單簿一側資料
    fn parse_excel_orderbook_side(cell: &Data) -> Vec<[String; 2]> {
        match cell {
            Data::String(s) => {
                // 嘗試解析為 JSON 格式的價格數組
                if let Ok(parsed) = serde_json::from_str::<Vec<[String; 2]>>(s) {
                    parsed
                } else {
                    // 或者嘗試解析為簡單的 "price,quantity;price,quantity" 格式
                    s.split(';')
                        .filter_map(|pair| {
                            let parts: Vec<&str> = pair.split(',').collect();
                            if parts.len() == 2 {
                                Some([parts[0].trim().to_string(), parts[1].trim().to_string()])
                            } else {
                                None
                            }
                        })
                        .collect()
                }
            }
            _ => Vec::new(),
        }
    }

    /// 從數組計算中間價
    fn calculate_mid_price_from_arrays(bids: &[[String; 2]], asks: &[[String; 2]]) -> Option<f64> {
        if bids.is_empty() || asks.is_empty() {
            return None;
        }

        let best_bid: f64 = bids[0][0].parse().ok()?;
        let best_ask: f64 = asks[0][0].parse().ok()?;
        
        Some((best_bid + best_ask) / 2.0)
    }

    /// 從數組計算價差
    fn calculate_spread_from_arrays(bids: &[[String; 2]], asks: &[[String; 2]]) -> Option<f64> {
        if bids.is_empty() || asks.is_empty() {
            return None;
        }

        let best_bid: f64 = bids[0][0].parse().ok()?;
        let best_ask: f64 = asks[0][0].parse().ok()?;
        
        Some(best_ask - best_bid)
    }
}

impl Clone for HistoricalDownloader {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            output_dir: self.output_dir.clone(),
            max_concurrent: self.max_concurrent,
            skip_existing: self.skip_existing,
            cleanup_zip: self.cleanup_zip,
            max_retries: self.max_retries,
            clickhouse_client: self.clickhouse_client.clone(),
            batch_size: self.batch_size,
        }
    }
}

/// 計算中間價
fn calculate_mid_price(bids: &[[String; 2]], asks: &[[String; 2]]) -> Option<f64> {
    if bids.is_empty() || asks.is_empty() {
        return None;
    }

    let best_bid: f64 = bids[0][0].parse().ok()?;
    let best_ask: f64 = asks[0][0].parse().ok()?;
    
    Some((best_bid + best_ask) / 2.0)
}

/// 計算價差
fn calculate_spread(bids: &[[String; 2]], asks: &[[String; 2]]) -> Option<f64> {
    if bids.is_empty() || asks.is_empty() {
        return None;
    }

    let best_bid: f64 = bids[0][0].parse().ok()?;
    let best_ask: f64 = asks[0][0].parse().ok()?;
    
    Some(best_ask - best_bid)
}

/// 計算成交金額
fn calculate_notional(price: &str, quantity: &str) -> Option<f64> {
    let price_f: f64 = price.parse().ok()?;
    let qty_f: f64 = quantity.parse().ok()?;
    Some(price_f * qty_f)
}

/// 解析配置（從文件或命令行參數）
fn parse_config(args: &Args) -> Result<ResolvedConfig> {
    let mut config = if let Some(config_path) = &args.config {
        // 從 YAML 文件加載配置
        ConfigLoader::load_from_file(config_path, Some(&args.profile))?
    } else {
        // 使用默認配置文件（如果存在）
        let default_config_path = "config/historical_data_config.yaml";
        if std::path::Path::new(default_config_path).exists() {
            info!("Using default config file: {}", default_config_path);
            ConfigLoader::load_from_file(default_config_path, Some(&args.profile))?
        } else {
            // 沒有配置文件，必須提供命令行參數
            if args.symbol.is_none() || args.start_date.is_none() || args.end_date.is_none() {
                return Err(anyhow::anyhow!(
                    "Either provide --config file or specify --symbol, --start-date, and --end-date"
                ));
            }
            
            // 從命令行參數創建配置
            ResolvedConfig {
                symbols: vec![args.symbol.as_ref().unwrap().clone()],
                data_type: args.data_type.as_ref().unwrap_or(&"depth".to_string()).clone(),
                start_date: parse_date(args.start_date.as_ref().unwrap())?,
                end_date: parse_date(args.end_date.as_ref().unwrap())?,
                output_dir: args.output_dir.as_ref().unwrap_or(&"./historical_data".to_string()).clone(),
                cleanup_zip: args.cleanup_zip.unwrap_or(true),
                skip_existing: args.skip_existing.unwrap_or(true),
                max_concurrent: args.max_concurrent.unwrap_or(5),
                batch_size: args.batch_size.unwrap_or(5000),
                max_retries: args.max_retries.unwrap_or(3),
                clickhouse_enabled: args.clickhouse_url.is_some(),
                clickhouse_url: args.clickhouse_url.clone(),
                clickhouse_db: args.clickhouse_db.as_ref().unwrap_or(&"hft_db".to_string()).clone(),
                clickhouse_user: args.clickhouse_user.as_ref().unwrap_or(&"hft_user".to_string()).clone(),
                clickhouse_password: args.clickhouse_password.as_ref().unwrap_or(&"hft_password".to_string()).clone(),
                training_enabled: false,
                training_config: None,
            }
        }
    };

    // 命令行參數覆蓋配置文件設置
    if let Some(symbol) = &args.symbol {
        config.symbols = vec![symbol.clone()];
    }
    if let Some(data_type) = &args.data_type {
        config.data_type = data_type.clone();
    }
    if let Some(start_date) = &args.start_date {
        config.start_date = parse_date(start_date)?;
    }
    if let Some(end_date) = &args.end_date {
        config.end_date = parse_date(end_date)?;
    }
    if let Some(output_dir) = &args.output_dir {
        config.output_dir = output_dir.clone();
    }
    if let Some(max_concurrent) = args.max_concurrent {
        config.max_concurrent = max_concurrent;
    }
    if let Some(batch_size) = args.batch_size {
        config.batch_size = batch_size;
    }
    if let Some(max_retries) = args.max_retries {
        config.max_retries = max_retries;
    }
    if let Some(cleanup_zip) = args.cleanup_zip {
        config.cleanup_zip = cleanup_zip;
    }
    if let Some(skip_existing) = args.skip_existing {
        config.skip_existing = skip_existing;
    }
    if let Some(clickhouse_url) = &args.clickhouse_url {
        config.clickhouse_enabled = true;
        config.clickhouse_url = Some(clickhouse_url.clone());
    }
    if let Some(clickhouse_db) = &args.clickhouse_db {
        config.clickhouse_db = clickhouse_db.clone();
    }
    if let Some(clickhouse_user) = &args.clickhouse_user {
        config.clickhouse_user = clickhouse_user.clone();
    }
    if let Some(clickhouse_password) = &args.clickhouse_password {
        config.clickhouse_password = clickhouse_password.clone();
    }

    Ok(config)
}

/// 解析日期字符串
fn parse_date(date_str: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .context("Invalid date format. Use YYYY-MM-DD")
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt::init();

    // 初始化 rustls crypto provider
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args = Args::parse();

    // 處理特殊命令
    if args.list_profiles {
        let config_path = args.config.as_deref().unwrap_or("config/historical_data_config.yaml");
        match ConfigLoader::list_profiles(config_path) {
            Ok(profiles) => {
                info!("Available profiles in {}:", config_path);
                for profile in profiles {
                    info!("  - {}", profile);
                }
                return Ok(());
            }
            Err(e) => {
                error!("Failed to list profiles: {}", e);
                return Err(e);
            }
        }
    }

    if let Some(config_path) = &args.create_config {
        ConfigLoader::create_default_config(config_path)?;
        info!("Default config created at: {}", config_path);
        return Ok(());
    }

    // 解析配置
    let config = parse_config(&args)?;
    
    // 驗證配置
    ConfigLoader::validate_config(&config)?;

    info!("=== 配置摘要 ===");
    info!("Profile: {}", args.profile);
    info!("Symbols: {:?}", config.symbols);
    info!("Data type: {}", config.data_type);
    info!("Date range: {} to {}", config.start_date, config.end_date);
    info!("ClickHouse enabled: {}", config.clickhouse_enabled);
    info!("Max concurrent: {}", config.max_concurrent);
    info!("Batch size: {}", config.batch_size);

    // 為每個交易對執行下載
    for symbol in &config.symbols {
        info!("Processing symbol: {}", symbol);

        // 創建下載器（為每個符號創建新的實例以避免狀態衝突）
        let downloader = create_downloader_from_config(&config).await?;

        // 開始下載
        downloader
            .download_historical_data(symbol, &config.data_type, config.start_date, config.end_date)
            .await?;

        // 如果使用了 ClickHouse，顯示統計資訊
        if let Some(ref client) = downloader.clickhouse_client {
            client.flush_all().await?;
            
            if args.show_stats {
                let stats = client.get_statistics().await?;
                info!("=== {} 資料庫統計 ===", symbol);
                for (table, count) in stats {
                    if table.contains("lob_depth") || table.contains("trade_data") {
                        info!("{}: {} 條記錄", table, count);
                    }
                }
            }
        }
    }

    // 數據驗證
    if args.verify || args.cleanup_after_verify {
        info!("=== 開始數據驗證 ===");
        
        // 檢查是否有 ClickHouse 客戶端
        if config.clickhouse_enabled {
            let ch_config = ClickHouseConfig {
                url: config.clickhouse_url.as_ref().unwrap().clone(),
                database: config.clickhouse_db.clone(),
                username: config.clickhouse_user.clone(),
                password: config.clickhouse_password.clone(),
                batch_size: config.batch_size,
                max_connections: config.max_concurrent,
                flush_interval_ms: 5000,
            };
            
            let ch_client = Arc::new(ClickHouseClient::new(ch_config)?);
            
            match verify_database_data(&ch_client).await {
                Ok(_) => {
                    info!("✅ 數據驗證通過");
                    
                    // 如果驗證通過且要求清理，則清理下載文件
                    if args.cleanup_after_verify {
                        cleanup_download_files(&config)?;
                    }
                }
                Err(e) => {
                    error!("❌ 數據驗證失敗: {}", e);
                    if args.cleanup_after_verify {
                        warn!("由於驗證失敗，跳過文件清理");
                    }
                    return Err(e);
                }
            }
        } else {
            warn!("未啟用 ClickHouse，跳過數據驗證");
        }
    }

    info!("🎉 All symbols processed successfully!");
    
    // 顯示後續建議
    if config.clickhouse_enabled {
        info!("📋 下一步建議：");
        info!("1. 特徵工程：從 LOB 數據提取 OBI、WAP、Spread 等特徵");
        info!("2. 標籤生成：基於未來中間價變動方向創建訓練標籤"); 
        info!("3. 模型訓練：使用 LightGBM/XGBoost 進行訓練");
        info!("4. 回測驗證：在歷史數據上驗證策略效果");
    }
    
    Ok(())
}

/// 驗證資料庫數據完整性和一致性
async fn verify_database_data(client: &ClickHouseClient) -> Result<()> {
    info!("🔍 開始數據驗證過程...");
    
    // 獲取統計資訊
    info!("📊 正在收集數據統計資訊...");
    let stats = client.get_statistics().await?;
    
    let lob_count = stats.get("lob_depth").copied().unwrap_or(0);
    let trade_count = stats.get("trade_data").copied().unwrap_or(0);
    
    info!("📈 數據庫統計:");
    info!("   🔹 LOB 深度記錄: {} 條", lob_count);
    info!("   🔹 成交記錄: {} 條", trade_count);
    
    if let Some(db_size) = stats.get("database_size_bytes") {
        let size_mb = db_size / (1024 * 1024);
        info!("   🔹 數據庫大小: {} MB", size_mb);
    }
    
    // 檢查基本數據存在性
    if lob_count == 0 && trade_count == 0 {
        return Err(anyhow::anyhow!("沒有發現任何數據記錄"));
    }
    
    // 驗證 LOB 數據完整性
    if lob_count > 0 {
        info!("🔍 驗證 LOB 數據完整性...");
        
        // 1. 檢查重複記錄
        info!("   🔄 檢查重複記錄...");
        let duplicate_count = client.query_tuple::<u64>(
            "SELECT count() - count(DISTINCT (timestamp, symbol, sequence)) FROM hft_db.lob_depth"
        ).await?;
        
        if duplicate_count > 0 {
            warn!("   ⚠️  發現 {} 條重複的 LOB 記錄 (這是正常的，表示同一時刻有多個相同快照)", duplicate_count);
        } else {
            info!("   ✅ 沒有發現重複的 LOB 記錄");
        }
        
        // 2. 檢查數據完整性（價格和數量陣列長度是否匹配）
        let invalid_lob_count = client.query_tuple::<u64>(
            "SELECT count() FROM hft_db.lob_depth WHERE 
             length(bid_prices) != length(bid_quantities) OR 
             length(ask_prices) != length(ask_quantities) OR
             length(bid_prices) != bid_levels OR
             length(ask_prices) != ask_levels"
        ).await?;
        
        if invalid_lob_count > 0 {
            return Err(anyhow::anyhow!("發現 {} 條 LOB 數據結構不一致", invalid_lob_count));
        } else {
            info!("✅ LOB 數據結構完整一致");
        }
        
        // 3. 檢查時間戳合理性（簡化版本）
        let min_timestamp = client.query_tuple::<i64>("SELECT min(timestamp) FROM hft_db.lob_depth").await?;
        let max_timestamp = client.query_tuple::<i64>("SELECT max(timestamp) FROM hft_db.lob_depth").await?;
        
        // 檢查時間戳是否在合理範圍內（2020年到未來1年）
        let year_2020_micros = 1577836800_000000i64; // 2020-01-01 in microseconds
        let future_limit_micros = chrono::Utc::now().timestamp_micros() + 365 * 24 * 3600 * 1000000; // 未來1年
        
        if min_timestamp < year_2020_micros {
            return Err(anyhow::anyhow!("發現過舊的時間戳記錄: {}", min_timestamp));
        }
        
        if max_timestamp > future_limit_micros {
            return Err(anyhow::anyhow!("發現未來的時間戳記錄: {}", max_timestamp));
        }
        
        info!("✅ LOB 時間戳範圍正常 ({} 到 {})", min_timestamp, max_timestamp);
        
        // 4. 檢查價格合理性
        let invalid_price_count = client.query_tuple::<u64>(
            "SELECT count() FROM hft_db.lob_depth WHERE 
             mid_price <= 0 OR spread < 0"
        ).await?;
        
        if invalid_price_count > 0 {
            return Err(anyhow::anyhow!("發現 {} 條價格數據異常", invalid_price_count));
        } else {
            info!("✅ LOB 價格數據正常");
        }
        
        // 5. 統計數據覆蓋範圍
        let unique_dates = client.query_tuple::<u64>("SELECT count(DISTINCT toDate(timestamp)) FROM hft_db.lob_depth").await?;
        let min_date = client.query_tuple::<String>("SELECT toString(toDate(min(timestamp))) FROM hft_db.lob_depth").await?;
        let max_date = client.query_tuple::<String>("SELECT toString(toDate(max(timestamp))) FROM hft_db.lob_depth").await?;
        let avg_records_per_day = lob_count / unique_dates.max(1);
        
        info!("📊 LOB 數據覆蓋分析:");
        info!("  - 時間範圍: {} 到 {}", min_date, max_date);
        info!("  - 覆蓋天數: {} 天", unique_dates);
        info!("  - 平均每日記錄數: {} 條", avg_records_per_day);
        
        // 檢查數據密度是否合理（每天至少應該有一定數量的記錄）
        if avg_records_per_day < 100 {
            warn!("⚠️  平均每日記錄數較少，可能存在數據缺失");
        }
    }
    
    // 驗證成交數據完整性
    if trade_count > 0 {
        info!("🔍 正在驗證成交數據完整性...");
        
        // 1. 檢查重複記錄
        let trade_duplicate_count = client.query_tuple::<u64>(
            "SELECT count() - count(DISTINCT (timestamp, symbol, trade_id)) FROM hft_db.trade_data"
        ).await?;
        
        if trade_duplicate_count > 0 {
            warn!("⚠️  發現 {} 條重複的成交記錄", trade_duplicate_count);
        } else {
            info!("✅ 沒有發現重複的成交記錄");
        }
        
        // 2. 檢查價格和數量合理性
        let invalid_trade_count = client.query_tuple::<u64>(
            "SELECT count() FROM hft_db.trade_data WHERE 
             price <= 0 OR quantity <= 0 OR notional <= 0"
        ).await?;
        
        if invalid_trade_count > 0 {
            return Err(anyhow::anyhow!("發現 {} 條成交數據異常", invalid_trade_count));
        } else {
            info!("✅ 成交數據價格和數量正常");
        }
        
        // 3. 檢查計算一致性（notional = price * quantity）
        let calculation_error_count = client.query_tuple::<u64>(
            "SELECT count() FROM hft_db.trade_data WHERE 
             abs(notional - (price * quantity)) > 0.01"
        ).await?;
        
        if calculation_error_count > 0 {
            warn!("⚠️  發現 {} 條成交金額計算不一致", calculation_error_count);
        } else {
            info!("✅ 成交金額計算一致");
        }
        
        let trade_min_date = client.query_tuple::<String>("SELECT toString(toDate(min(timestamp))) FROM hft_db.trade_data").await?;
        let trade_max_date = client.query_tuple::<String>("SELECT toString(toDate(max(timestamp))) FROM hft_db.trade_data").await?;
        
        info!("📊 成交數據覆蓋分析:");
        info!("  - 時間範圍: {} 到 {}", trade_min_date, trade_max_date);
        info!("  - 總成交記錄: {} 條", trade_count);
    }
    
    // 如果有 LOB 和成交數據，檢查時間對齊
    if lob_count > 0 && trade_count > 0 {
        info!("🔍 正在檢查 LOB 和成交數據時間對齊...");
        
        let lob_time_range = client.query_tuple::<(String, String)>(
            "SELECT toString(toDate(min(timestamp))), toString(toDate(max(timestamp))) FROM hft_db.lob_depth"
        ).await?;
        
        let trade_time_range = client.query_tuple::<(String, String)>(
            "SELECT toString(toDate(min(timestamp))), toString(toDate(max(timestamp))) FROM hft_db.trade_data"
        ).await?;
        
        info!("時間範圍對比:");
        info!("  - LOB 數據: {} 到 {}", lob_time_range.0, lob_time_range.1);
        info!("  - 成交數據: {} 到 {}", trade_time_range.0, trade_time_range.1);
        
        if lob_time_range.0 == trade_time_range.0 && lob_time_range.1 == trade_time_range.1 {
            info!("✅ LOB 和成交數據時間範圍完全對齊");
        } else {
            warn!("⚠️  LOB 和成交數據時間範圍不完全對齊");
        }
    }
    
    info!("🎉 數據完整性驗證完成");
    Ok(())
}

/// 清理下載的原始文件
fn cleanup_download_files(config: &ResolvedConfig) -> Result<()> {
    info!("正在清理下載的原始文件...");
    
    let mut cleaned_count = 0;
    let mut total_size = 0u64;
    
    // 清理輸出目錄
    if std::path::Path::new(&config.output_dir).exists() {
        let dir_size = calculate_dir_size(&config.output_dir)?;
        total_size += dir_size;
        
        std::fs::remove_dir_all(&config.output_dir)?;
        info!("已刪除目錄: {} ({} MB)", config.output_dir, dir_size / 1024 / 1024);
        cleaned_count += 1;
    }
    
    // 查找並清理其他可能的下載目錄
    let patterns = vec![
        "solusdt_*days_*",
        "trade_data*",
        "test_data*", 
        "historical_data*",
        "*_depth*",
        "*_trade*"
    ];
    
    for pattern in patterns {
        // 簡單的模式匹配，查找當前目錄下匹配的目錄
        if let Ok(entries) = std::fs::read_dir(".") {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        let dir_name = entry.file_name().to_string_lossy().to_string();
                        if pattern_matches(&dir_name, pattern) {
                            let dir_size = calculate_dir_size(&dir_name)?;
                            total_size += dir_size;
                            
                            std::fs::remove_dir_all(&dir_name)?;
                            info!("已刪除目錄: {} ({} MB)", dir_name, dir_size / 1024 / 1024);
                            cleaned_count += 1;
                        }
                    }
                }
            }
        }
    }
    
    // 清理 ZIP 文件
    if let Ok(entries) = std::fs::read_dir(".") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("zip") {
                if let Ok(metadata) = entry.metadata() {
                    total_size += metadata.len();
                    std::fs::remove_file(&path)?;
                    info!("已刪除文件: {} ({} MB)", 
                          path.display(), 
                          metadata.len() / 1024 / 1024);
                    cleaned_count += 1;
                }
            }
        }
    }
    
    if cleaned_count > 0 {
        info!("✅ 清理完成：刪除了 {} 個文件/目錄，節省了 {} MB 存儲空間", 
              cleaned_count, total_size / 1024 / 1024);
    } else {
        info!("沒有發現需要清理的文件");
    }
    
    Ok(())
}

/// 簡單的模式匹配函數
fn pattern_matches(text: &str, pattern: &str) -> bool {
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            text.starts_with(parts[0]) && text.ends_with(parts[1])
        } else {
            false
        }
    } else {
        text == pattern
    }
}

/// 計算目錄大小
fn calculate_dir_size(dir: &str) -> Result<u64> {
    let mut size = 0;
    
    fn visit_dir(dir: &std::path::Path, size: &mut u64) -> Result<()> {
        if dir.is_dir() {
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    visit_dir(&path, size)?;
                } else {
                    *size += entry.metadata()?.len();
                }
            }
        }
        Ok(())
    }
    
    visit_dir(std::path::Path::new(dir), &mut size)?;
    Ok(size)
}

/// 從配置創建下載器
async fn create_downloader_from_config(config: &ResolvedConfig) -> Result<HistoricalDownloader> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create HTTP client")?;

    // 初始化 ClickHouse 客戶端（如果啟用）
    let clickhouse_client = if config.clickhouse_enabled {
        let ch_config = ClickHouseConfig {
            url: config.clickhouse_url.as_ref().unwrap().clone(),
            database: config.clickhouse_db.clone(),
            username: config.clickhouse_user.clone(),
            password: config.clickhouse_password.clone(),
            batch_size: config.batch_size,
            max_connections: config.max_concurrent,
            flush_interval_ms: 5000,
        };

        let ch_client = Arc::new(ClickHouseClient::new(ch_config)?);
        
        // 測試連接並創建表
        ch_client.test_connection().await?;
        ch_client.create_tables().await?;
        info!("ClickHouse 連接已建立，將直接導入資料庫");
        
        Some(ch_client)
    } else {
        info!("未啟用 ClickHouse，將保存到文件");
        None
    };

    Ok(HistoricalDownloader {
        client,
        base_url: "https://img.bitgetimg.com/online".to_string(),
        output_dir: PathBuf::from(&config.output_dir),
        max_concurrent: config.max_concurrent,
        skip_existing: config.skip_existing,
        cleanup_zip: config.cleanup_zip,
        max_retries: config.max_retries,
        clickhouse_client,
        batch_size: config.batch_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_mid_price() {
        let bids = vec![["100.0".to_string(), "1.0".to_string()]];
        let asks = vec![["101.0".to_string(), "1.0".to_string()]];
        
        let mid_price = calculate_mid_price(&bids, &asks);
        assert_eq!(mid_price, Some(100.5));
    }

    #[test]
    fn test_calculate_spread() {
        let bids = vec![["100.0".to_string(), "1.0".to_string()]];
        let asks = vec![["101.0".to_string(), "1.0".to_string()]];
        
        let spread = calculate_spread(&bids, &asks);
        assert_eq!(spread, Some(1.0));
    }

    #[test]
    fn test_calculate_notional() {
        let notional = calculate_notional("100.5", "2.0");
        assert_eq!(notional, Some(201.0));
    }

    #[test]
    fn test_parse_date() {
        let date = parse_date("2024-01-01").unwrap();
        assert_eq!(date.year(), 2024);
        assert_eq!(date.month(), 1);
        assert_eq!(date.day(), 1);
    }
}