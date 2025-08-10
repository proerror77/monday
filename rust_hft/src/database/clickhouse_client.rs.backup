use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clickhouse::{Client, Row};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// ClickHouse 客戶端配置
#[derive(Debug, Clone)]
pub struct ClickHouseConfig {
    pub url: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub max_connections: usize,
    pub batch_size: usize,
    pub flush_interval_ms: u64,
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:8123".to_string(),
            database: "hft_db".to_string(),
            username: "hft_user".to_string(),
            password: "hft_password".to_string(),
            max_connections: 10,
            batch_size: 1000,
            flush_interval_ms: 5000,
        }
    }
}

/// LOB 深度資料行結構 (使用正確的 ClickHouse 類型映射)
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
pub struct LobDepthRow {
    pub timestamp: i64, // DateTime64(6) 映射為 i64 微秒
    pub symbol: String,
    pub exchange: String,
    pub sequence: u64,
    pub is_snapshot: u8,
    pub bid_prices: Vec<f64>,
    pub bid_quantities: Vec<f64>,
    pub ask_prices: Vec<f64>,
    pub ask_quantities: Vec<f64>,
    pub bid_levels: u16,
    pub ask_levels: u16,
    pub spread: f64,
    pub mid_price: f64,
    pub data_source: String,
    pub created_at: u32, // DateTime 映射為 u32 秒
}


/// 成交資料行結構 (使用正確的 ClickHouse 類型映射)
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
pub struct TradeDataRow {
    pub timestamp: i64, // DateTime64(6) 映射為 i64 微秒
    pub symbol: String,
    pub exchange: String,
    pub trade_id: String,
    pub price: f64,
    pub quantity: f64,
    pub side: String,
    pub notional: f64,
    pub data_source: String,
    pub created_at: u32, // DateTime 映射為 u32 秒
}


/// 機器學習特徵行結構 (使用正確的 ClickHouse 類型映射)
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
pub struct MlFeaturesRow {
    pub timestamp: i64, // DateTime64(6) 映射為 i64 微秒
    pub symbol: String,
    pub mid_price: f64,
    pub weighted_mid_price: f64,
    pub spread: f64,
    pub relative_spread: f64,
    pub order_book_imbalance: f64,
    pub bid_ask_ratio: f64,
    pub depth_ratio: f64,
    pub volume_5s: f64,
    pub volume_10s: f64,
    pub volume_30s: f64,
    pub price_volatility: f64,
    pub volume_volatility: f64,
    pub price_trend_5s: f64,
    pub price_trend_10s: f64,
    pub rsi: f64,
    pub macd: f64,
    pub bollinger_upper: f64,
    pub bollinger_lower: f64,
    pub price_change_3s: f64,
    pub price_change_5s: f64,
    pub price_change_10s: f64,
    pub created_at: u32, // DateTime 映射為 u32 秒
}

/// 系統日誌行結構 (使用正確的 ClickHouse 類型映射)
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
pub struct SystemLogRow {
    pub timestamp: i64, // DateTime64(6) 映射為 i64 微秒
    pub level: String,
    pub component: String,
    pub message: String,
    pub metadata: String,
    pub created_at: u32, // DateTime 映射為 u32 秒
}

/// 批量插入緩衝區
#[derive(Debug)]
struct BatchBuffer<T> {
    data: Vec<T>,
    max_size: usize,
    last_flush: std::time::Instant,
    flush_interval: std::time::Duration,
}

impl<T> BatchBuffer<T> {
    fn new(max_size: usize, flush_interval_ms: u64) -> Self {
        Self {
            data: Vec::with_capacity(max_size),
            max_size,
            last_flush: std::time::Instant::now(),
            flush_interval: std::time::Duration::from_millis(flush_interval_ms),
        }
    }

    fn push(&mut self, item: T) -> bool {
        self.data.push(item);
        self.should_flush()
    }

    fn should_flush(&self) -> bool {
        self.data.len() >= self.max_size || self.last_flush.elapsed() >= self.flush_interval
    }

    fn take(&mut self) -> Vec<T> {
        self.last_flush = std::time::Instant::now();
        std::mem::take(&mut self.data)
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// ClickHouse 客戶端
pub struct ClickHouseClient {
    client: Client,
    config: ClickHouseConfig,
    
    // 批量插入緩衝區
    lob_buffer: Arc<Mutex<BatchBuffer<LobDepthRow>>>,
    trade_buffer: Arc<Mutex<BatchBuffer<TradeDataRow>>>,
    ml_features_buffer: Arc<Mutex<BatchBuffer<MlFeaturesRow>>>,
    system_log_buffer: Arc<Mutex<BatchBuffer<SystemLogRow>>>,
}

impl ClickHouseClient {
    /// 創建新的 ClickHouse 客戶端
    pub fn new(config: ClickHouseConfig) -> Result<Self> {
        let client = Client::default()
            .with_url(&config.url)
            .with_database(&config.database)
            .with_user(&config.username)
            .with_password(&config.password);

        let lob_buffer = Arc::new(Mutex::new(BatchBuffer::new(
            config.batch_size,
            config.flush_interval_ms,
        )));
        let trade_buffer = Arc::new(Mutex::new(BatchBuffer::new(
            config.batch_size,
            config.flush_interval_ms,
        )));
        let ml_features_buffer = Arc::new(Mutex::new(BatchBuffer::new(
            config.batch_size,
            config.flush_interval_ms,
        )));
        let system_log_buffer = Arc::new(Mutex::new(BatchBuffer::new(
            config.batch_size,
            config.flush_interval_ms,
        )));

        Ok(Self {
            client,
            config,
            lob_buffer,
            trade_buffer,
            ml_features_buffer,
            system_log_buffer,
        })
    }

    /// 測試資料庫連接
    pub async fn test_connection(&self) -> Result<()> {
        let result = self
            .client
            .query("SELECT 1 as test")
            .fetch_one::<u8>()
            .await
            .context("Failed to test ClickHouse connection")?;
        
        info!("ClickHouse connection test successful: {}", result);
        Ok(())
    }

    /// 創建所有表格 (如果不存在)
    pub async fn create_tables(&self) -> Result<()> {
        let tables = vec![
            include_str!("../../clickhouse/init.sql"),
        ];

        for table_sql in tables {
            // 分割 SQL 語句
            let statements: Vec<&str> = table_sql
                .split(';')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && !s.starts_with("--"))
                .collect();

            for statement in statements {
                if !statement.is_empty() {
                    self.client
                        .query(statement)
                        .execute()
                        .await
                        .context(format!("Failed to execute SQL: {}", statement))?;
                }
            }
        }

        info!("All tables created successfully");
        Ok(())
    }

    /// 插入 LOB 深度資料
    pub async fn insert_lob_depth(&self, row: LobDepthRow) -> Result<()> {
        let mut buffer = self.lob_buffer.lock().await;
        let should_flush = buffer.push(row);
        
        if should_flush {
            let data = buffer.take();
            drop(buffer); // 釋放鎖
            
            self.flush_lob_depth_data(data).await?;
        }

        Ok(())
    }

    /// 插入成交資料
    pub async fn insert_trade_data(&self, row: TradeDataRow) -> Result<()> {
        let mut buffer = self.trade_buffer.lock().await;
        let should_flush = buffer.push(row);
        
        if should_flush {
            let data = buffer.take();
            drop(buffer); // 釋放鎖
            
            self.flush_trade_data(data).await?;
        }

        Ok(())
    }

    /// 插入機器學習特徵
    pub async fn insert_ml_features(&self, row: MlFeaturesRow) -> Result<()> {
        let mut buffer = self.ml_features_buffer.lock().await;
        let should_flush = buffer.push(row);
        
        if should_flush {
            let data = buffer.take();
            drop(buffer); // 釋放鎖
            
            self.flush_ml_features(data).await?;
        }

        Ok(())
    }

    /// 插入系統日誌
    pub async fn insert_system_log(&self, row: SystemLogRow) -> Result<()> {
        let mut buffer = self.system_log_buffer.lock().await;
        let should_flush = buffer.push(row);
        
        if should_flush {
            let data = buffer.take();
            drop(buffer); // 釋放鎖
            
            self.flush_system_log(data).await?;
        }

        Ok(())
    }

    /// 批量插入 LOB 深度資料 (帶詳細調試)
    pub async fn batch_insert_lob_depth(&self, rows: Vec<LobDepthRow>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        info!("=== ClickHouse Batch Insert Debug ===");
        info!("Attempting to insert {} LOB depth rows", rows.len());
        
        // 詳細數據統計
        let total_bid_elements: usize = rows.iter().map(|r| r.bid_prices.len()).sum();
        let total_ask_elements: usize = rows.iter().map(|r| r.ask_prices.len()).sum();
        let avg_bid_levels = total_bid_elements as f64 / rows.len() as f64;
        let avg_ask_levels = total_ask_elements as f64 / rows.len() as f64;
        
        info!("Data statistics:");
        info!("  - Total bid elements: {}", total_bid_elements);
        info!("  - Total ask elements: {}", total_ask_elements);
        info!("  - Average bid levels: {:.2}", avg_bid_levels);
        info!("  - Average ask levels: {:.2}", avg_ask_levels);
        
        // 驗證所有記錄並收集統計
        let mut validation_errors = 0;
        for (i, row) in rows.iter().enumerate() {
            match self.validate_lob_row(row) {
                Ok(_) => {
                    if i < 3 { // 只打印前3行的詳細信息
                        info!("Row {}: symbol={}, timestamp={}, bid_prices=[{}], ask_prices=[{}]", 
                              i, row.symbol, row.timestamp,
                              row.bid_prices.iter().take(3).map(|x| format!("{:.2}", x)).collect::<Vec<_>>().join(","),
                              row.ask_prices.iter().take(3).map(|x| format!("{:.2}", x)).collect::<Vec<_>>().join(","));
                    }
                },
                Err(e) => {
                    error!("Row {} validation failed: {}", i, e);
                    validation_errors += 1;
                }
            }
        }
        
        if validation_errors > 0 {
            return Err(anyhow::anyhow!("Found {} validation errors", validation_errors));
        }
        
        info!("All {} rows passed validation", rows.len());
        
        // 嘗試序列化測試
        info!("Testing serialization for first row...");
        let test_row = &rows[0];
        match serde_json::to_string(test_row) {
            Ok(json) => {
                info!("First row serializes to {} bytes: {}", json.len(), 
                      if json.len() > 200 { format!("{}...", &json[..200]) } else { json });
            },
            Err(e) => {
                error!("Serialization test failed: {}", e);
                return Err(anyhow::anyhow!("Serialization test failed: {}", e));
            }
        }
        
        // 使用標準的 insert API 並添加詳細錯誤處理
        info!("Starting ClickHouse insert operation...");
        let mut insert = self.client.insert("lob_depth")
            .context("Failed to create insert statement")?;
        
        for (i, row) in rows.iter().enumerate() {
            if i % 100 == 0 {
                info!("Writing batch progress: {}/{}", i, rows.len());
            }
            
            match insert.write(row).await {
                Ok(_) => {
                    if i < 5 { // 詳細日誌前5行
                        debug!("Successfully wrote row {}: symbol={}, bid_levels={}, ask_levels={}", 
                               i, row.symbol, row.bid_levels, row.ask_levels);
                    }
                },
                Err(e) => {
                    error!("Failed to write row {}: symbol={}, error={}", i, row.symbol, e);
                    return Err(anyhow::anyhow!("Failed to write row {}: {}", i, e));
                }
            }
        }
        
        info!("All rows written, calling insert.end()...");
        match insert.end().await {
            Ok(_) => {
                info!("✅ Successfully inserted {} LOB depth rows", rows.len());
                Ok(())
            },
            Err(e) => {
                error!("❌ Failed to end batch insert: {}", e);
                error!("Error details: {:?}", e);
                Err(anyhow::anyhow!("Failed to end batch insert: {}", e))
            }
        }
    }
    
    /// 游攻的批量插入方法，在出錯時減小批次大小
    pub async fn batch_insert_lob_depth_with_recovery(&self, rows: Vec<LobDepthRow>) -> Result<()> {
        const MAX_BATCH_SIZE: usize = 1000;
        const MIN_BATCH_SIZE: usize = 100;
        
        let mut batch_size = MAX_BATCH_SIZE.min(rows.len());
        let mut start_idx = 0;
        
        while start_idx < rows.len() {
            let end_idx = (start_idx + batch_size).min(rows.len());
            let batch = rows[start_idx..end_idx].to_vec();
            
            match self.batch_insert_lob_depth(batch).await {
                Ok(_) => {
                    info!("Successfully inserted batch [{}-{}]", start_idx, end_idx);
                    start_idx = end_idx;
                    // 成功後重置批次大小
                    batch_size = MAX_BATCH_SIZE.min(rows.len() - start_idx);
                }
                Err(e) if batch_size > MIN_BATCH_SIZE => {
                    warn!("Batch insert failed, reducing batch size from {} to {}: {}", 
                          batch_size, batch_size / 2, e);
                    batch_size /= 2;
                    // 不推進 start_idx，以較小批次重試
                }
                Err(e) => {
                    error!("Cannot insert even small batch: {}", e);
                    return Err(e);
                }
            }
        }
        
        Ok(())
    }
    
    /// 驗證 LOB 記錄的數據一致性
    fn validate_lob_row(&self, row: &LobDepthRow) -> Result<()> {
        // 確保陣列大小匹配
        if row.bid_prices.len() != row.bid_quantities.len() {
            return Err(anyhow::anyhow!(
                "Bid prices and quantities array length mismatch: {} vs {}", 
                row.bid_prices.len(), row.bid_quantities.len()
            ));
        }
        
        if row.ask_prices.len() != row.ask_quantities.len() {
            return Err(anyhow::anyhow!(
                "Ask prices and quantities array length mismatch: {} vs {}", 
                row.ask_prices.len(), row.ask_quantities.len()
            ));
        }
        
        // 驗證檔數與陣列大小匹配
        if row.bid_levels != row.bid_prices.len() as u16 {
            return Err(anyhow::anyhow!(
                "Bid levels mismatch: {} vs {}", 
                row.bid_levels, row.bid_prices.len()
            ));
        }
        
        if row.ask_levels != row.ask_prices.len() as u16 {
            return Err(anyhow::anyhow!(
                "Ask levels mismatch: {} vs {}", 
                row.ask_levels, row.ask_prices.len()
            ));
        }
        
        // 驗證數据有效性
        if row.bid_prices.is_empty() || row.ask_prices.is_empty() {
            return Err(anyhow::anyhow!("Empty price arrays"));
        }
        
        // 驗證價格和數量為正數
        for (i, &price) in row.bid_prices.iter().enumerate() {
            if price <= 0.0 || !price.is_finite() {
                return Err(anyhow::anyhow!("Invalid bid price at index {}: {}", i, price));
            }
        }
        
        for (i, &qty) in row.bid_quantities.iter().enumerate() {
            if qty <= 0.0 || !qty.is_finite() {
                return Err(anyhow::anyhow!("Invalid bid quantity at index {}: {}", i, qty));
            }
        }
        
        for (i, &price) in row.ask_prices.iter().enumerate() {
            if price <= 0.0 || !price.is_finite() {
                return Err(anyhow::anyhow!("Invalid ask price at index {}: {}", i, price));
            }
        }
        
        for (i, &qty) in row.ask_quantities.iter().enumerate() {
            if qty <= 0.0 || !qty.is_finite() {
                return Err(anyhow::anyhow!("Invalid ask quantity at index {}: {}", i, qty));
            }
        }
        
        Ok(())
    }

    /// 批量插入成交資料
    pub async fn batch_insert_trade_data(&self, rows: Vec<TradeDataRow>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let mut insert = self.client.insert("trade_data")?;
        for row in rows {
            insert.write(&row).await?;
        }
        insert.end().await?;

        Ok(())
    }

    /// 批量插入機器學習特徵
    pub async fn batch_insert_ml_features(&self, rows: Vec<MlFeaturesRow>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let mut insert = self.client.insert("ml_features")?;
        for row in rows {
            insert.write(&row).await?;
        }
        insert.end().await?;

        Ok(())
    }

    /// 強制刷新所有緩衝區
    pub async fn flush_all(&self) -> Result<()> {
        // 刷新 LOB 深度資料
        {
            let mut buffer = self.lob_buffer.lock().await;
            if !buffer.is_empty() {
                let data = buffer.take();
                drop(buffer);
                self.flush_lob_depth_data(data).await?;
            }
        }

        // 刷新成交資料
        {
            let mut buffer = self.trade_buffer.lock().await;
            if !buffer.is_empty() {
                let data = buffer.take();
                drop(buffer);
                self.flush_trade_data(data).await?;
            }
        }

        // 刷新機器學習特徵
        {
            let mut buffer = self.ml_features_buffer.lock().await;
            if !buffer.is_empty() {
                let data = buffer.take();
                drop(buffer);
                self.flush_ml_features(data).await?;
            }
        }

        // 刷新系統日誌
        {
            let mut buffer = self.system_log_buffer.lock().await;
            if !buffer.is_empty() {
                let data = buffer.take();
                drop(buffer);
                self.flush_system_log(data).await?;
            }
        }

        Ok(())
    }

    /// 查詢 LOB 深度資料
    pub async fn query_lob_depth(
        &self,
        symbol: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        limit: Option<usize>,
    ) -> Result<Vec<LobDepthRow>> {
        let mut query = format!(
            "SELECT * FROM lob_depth WHERE symbol = '{}' AND timestamp >= '{}' AND timestamp <= '{}'",
            symbol, start_time.format("%Y-%m-%d %H:%M:%S"), end_time.format("%Y-%m-%d %H:%M:%S")
        );

        if let Some(limit) = limit {
            query.push_str(&format!(" LIMIT {}", limit));
        }

        let rows = self
            .client
            .query(&query)
            .fetch_all::<LobDepthRow>()
            .await
            .context("Failed to query LOB depth data")?;

        Ok(rows)
    }

    /// 查詢成交資料
    pub async fn query_trade_data(
        &self,
        symbol: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        limit: Option<usize>,
    ) -> Result<Vec<TradeDataRow>> {
        let mut query = format!(
            "SELECT * FROM trade_data WHERE symbol = '{}' AND timestamp >= '{}' AND timestamp <= '{}'",
            symbol, start_time.format("%Y-%m-%d %H:%M:%S"), end_time.format("%Y-%m-%d %H:%M:%S")
        );

        if let Some(limit) = limit {
            query.push_str(&format!(" LIMIT {}", limit));
        }

        let rows = self
            .client
            .query(&query)
            .fetch_all::<TradeDataRow>()
            .await
            .context("Failed to query trade data")?;

        Ok(rows)
    }

    /// 查詢機器學習特徵
    pub async fn query_ml_features(
        &self,
        symbol: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        limit: Option<usize>,
    ) -> Result<Vec<MlFeaturesRow>> {
        let mut query = format!(
            "SELECT * FROM ml_features WHERE symbol = '{}' AND timestamp >= '{}' AND timestamp <= '{}'",
            symbol, start_time.format("%Y-%m-%d %H:%M:%S"), end_time.format("%Y-%m-%d %H:%M:%S")
        );

        if let Some(limit) = limit {
            query.push_str(&format!(" LIMIT {}", limit));
        }

        let rows = self
            .client
            .query(&query)
            .fetch_all::<MlFeaturesRow>()
            .await
            .context("Failed to query ML features")?;

        Ok(rows)
    }

    /// 執行自定義查詢並返回元組
    pub async fn query_tuple<T>(&self, sql: &str) -> Result<T> 
    where
        T: for<'de> serde::Deserialize<'de> + clickhouse::Row,
    {
        self.client
            .query(sql)
            .fetch_one()
            .await
            .context("Failed to execute custom query")
    }

    /// 獲取資料庫統計資訊
    pub async fn get_statistics(&self) -> Result<HashMap<String, u64>> {
        let mut stats = HashMap::new();

        // 查詢各表的資料量
        let tables = vec!["lob_depth", "trade_data", "ml_features", "system_logs"];
        
        for table in tables {
            let count: u64 = self
                .client
                .query(&format!("SELECT count() FROM {}", table))
                .fetch_one()
                .await
                .context(format!("Failed to get count for table {}", table))?;
            
            stats.insert(table.to_string(), count);
        }

        // 查詢資料庫大小
        let db_size: u64 = self
            .client
            .query("SELECT sum(bytes_on_disk) FROM system.parts WHERE database = 'hft_db'")
            .fetch_one()
            .await
            .context("Failed to get database size")?;
        
        stats.insert("database_size_bytes".to_string(), db_size);

        Ok(stats)
    }

    /// 內部方法：刷新 LOB 深度資料
    async fn flush_lob_depth_data(&self, data: Vec<LobDepthRow>) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        let count = data.len();
        // 使用游攻的批量插入方法
        self.batch_insert_lob_depth_with_recovery(data).await?;
        debug!("Flushed {} LOB depth records", count);
        Ok(())
    }

    /// 內部方法：刷新成交資料
    async fn flush_trade_data(&self, data: Vec<TradeDataRow>) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        let count = data.len();
        self.batch_insert_trade_data(data).await?;
        debug!("Flushed {} trade records", count);
        Ok(())
    }

    /// 內部方法：刷新機器學習特徵
    async fn flush_ml_features(&self, data: Vec<MlFeaturesRow>) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        let count = data.len();
        self.batch_insert_ml_features(data).await?;
        debug!("Flushed {} ML feature records", count);
        Ok(())
    }

    /// 內部方法：刷新系統日誌
    async fn flush_system_log(&self, data: Vec<SystemLogRow>) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        let mut insert = self.client.insert("system_logs")?;
        for row in data.iter() {
            insert.write(row).await?;
        }
        insert.end().await?;

        debug!("Flushed {} system log records", data.len());
        Ok(())
    }
}

/// 啟動後台刷新任務
pub async fn start_background_flusher(client: Arc<ClickHouseClient>) -> Result<()> {
    let flush_interval = std::time::Duration::from_millis(client.config.flush_interval_ms);
    
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(flush_interval);
        
        loop {
            interval.tick().await;
            
            if let Err(e) = client.flush_all().await {
                error!("Background flush failed: {}", e);
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn test_batch_buffer() {
        let mut buffer = BatchBuffer::new(3, 1000);
        
        assert!(!buffer.push(1));
        assert!(!buffer.push(2));
        assert!(buffer.push(3)); // 應該觸發刷新
        
        let data = buffer.take();
        assert_eq!(data.len(), 3);
        assert!(buffer.is_empty());
    }

    #[tokio::test]
    async fn test_clickhouse_config() {
        let config = ClickHouseConfig::default();
        assert_eq!(config.url, "http://localhost:8123");
        assert_eq!(config.database, "hft_db");
        assert_eq!(config.username, "hft_user");
        assert_eq!(config.password, "hft_password");
    }
}