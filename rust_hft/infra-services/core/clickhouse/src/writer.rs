//! ClickHouse RowBinary 寫入器
//!
//! RowBinary 格式比 JSONEachRow 更高效：
//! - 無 JSON 解析開銷
//! - 更緊湊的二進制表示
//! - 原生支援 Nullable 和複雜類型

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// 寫入錯誤類型
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("HTTP 錯誤: {0}")]
    Http(String),
    #[error("序列化錯誤: {0}")]
    Serialization(String),
    #[error("連接錯誤: {0}")]
    Connection(String),
    #[error("通道已關閉")]
    ChannelClosed,
}

/// ClickHouse 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHouseConfig {
    /// HTTP URL (如 http://localhost:8123)
    pub url: String,
    /// 資料庫名稱
    pub database: String,
    /// 用戶名
    pub user: String,
    /// 密碼
    pub password: String,
    /// 批量大小（多少行後自動發送）
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// 刷新間隔（毫秒）
    #[serde(default = "default_flush_interval_ms")]
    pub flush_interval_ms: u64,
    /// 連接超時（秒）
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    /// 請求超時（秒）
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// 是否啟用 LZ4 壓縮
    #[serde(default = "default_compression")]
    pub enable_compression: bool,
}

fn default_batch_size() -> usize {
    1000
}
fn default_flush_interval_ms() -> u64 {
    1000
}
fn default_connect_timeout_secs() -> u64 {
    5
}
fn default_request_timeout_secs() -> u64 {
    10
}
fn default_compression() -> bool {
    true
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:8123".to_string(),
            database: "default".to_string(),
            user: "default".to_string(),
            password: String::new(),
            batch_size: default_batch_size(),
            flush_interval_ms: default_flush_interval_ms(),
            connect_timeout_secs: default_connect_timeout_secs(),
            request_timeout_secs: default_request_timeout_secs(),
            enable_compression: default_compression(),
        }
    }
}

impl ClickHouseConfig {
    /// 從環境變數建立配置
    pub fn from_env() -> Self {
        Self {
            url: std::env::var("CLICKHOUSE_URL")
                .unwrap_or_else(|_| "http://localhost:8123".to_string()),
            database: std::env::var("CLICKHOUSE_DB").unwrap_or_else(|_| "default".to_string()),
            user: std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_string()),
            password: std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_default(),
            batch_size: std::env::var("CLICKHOUSE_BATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default_batch_size()),
            flush_interval_ms: std::env::var("CLICKHOUSE_FLUSH_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default_flush_interval_ms()),
            ..Default::default()
        }
    }
}

/// RowBinary 值類型
#[derive(Debug, Clone)]
pub enum RowBinaryValue {
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    UInt64(u64),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    Float32(f32),
    Float64(f64),
    String(String),
    /// DateTime64(3) - 毫秒精度時間戳
    DateTime64Millis(i64),
    /// DateTime64(6) - 微秒精度時間戳
    DateTime64Micros(i64),
    /// Nullable 包裝
    Nullable(Option<Box<RowBinaryValue>>),
    /// Decimal64(precision)
    Decimal64(i64, u8),
}

impl RowBinaryValue {
    /// 序列化為 RowBinary 格式
    pub fn write_to<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        match self {
            RowBinaryValue::UInt8(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::UInt16(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::UInt32(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::UInt64(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::Int8(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::Int16(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::Int32(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::Int64(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::Float32(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::Float64(v) => writer.write_all(&v.to_le_bytes()),
            RowBinaryValue::String(s) => {
                // ClickHouse String: varint length + bytes
                write_varint(writer, s.len() as u64)?;
                writer.write_all(s.as_bytes())
            }
            RowBinaryValue::DateTime64Millis(v) | RowBinaryValue::DateTime64Micros(v) => {
                writer.write_all(&v.to_le_bytes())
            }
            RowBinaryValue::Nullable(opt) => match opt {
                None => writer.write_all(&[1u8]), // NULL marker
                Some(inner) => {
                    writer.write_all(&[0u8])?; // NOT NULL marker
                    inner.write_to(writer)
                }
            },
            RowBinaryValue::Decimal64(v, _precision) => writer.write_all(&v.to_le_bytes()),
        }
    }
}

/// 寫入 varint (用於 String 長度)
fn write_varint<W: Write>(writer: &mut W, mut value: u64) -> std::io::Result<()> {
    while value >= 0x80 {
        writer.write_all(&[(value as u8) | 0x80])?;
        value >>= 7;
    }
    writer.write_all(&[value as u8])
}

/// RowBinary 行寫入器
pub struct RowBinaryWriter {
    buffer: Vec<u8>,
    row_count: usize,
}

impl Default for RowBinaryWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl RowBinaryWriter {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(64 * 1024), // 64KB 初始緩衝
            row_count: 0,
        }
    }

    /// 寫入一行數據
    pub fn write_row(&mut self, values: &[RowBinaryValue]) -> Result<(), WriteError> {
        for value in values {
            value
                .write_to(&mut self.buffer)
                .map_err(|e| WriteError::Serialization(e.to_string()))?;
        }
        self.row_count += 1;
        Ok(())
    }

    /// 獲取緩衝區內容
    pub fn take_buffer(&mut self) -> (Vec<u8>, usize) {
        let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(64 * 1024));
        let count = self.row_count;
        self.row_count = 0;
        (buffer, count)
    }

    /// 當前行數
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// 緩衝區大小
    pub fn buffer_size(&self) -> usize {
        self.buffer.len()
    }

    /// 清空緩衝區
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.row_count = 0;
    }
}

/// 批量寫入請求
struct BatchRequest {
    table: String,
    data: Vec<u8>,
    row_count: usize,
}

/// ClickHouse 異步寫入器
pub struct ClickHouseWriter {
    config: ClickHouseConfig,
    http_client: reqwest::Client,
    /// 批量發送通道
    sender: mpsc::Sender<BatchRequest>,
    /// 背景任務句柄
    _worker_handle: tokio::task::JoinHandle<()>,
}

impl ClickHouseWriter {
    /// 建立新的寫入器
    pub fn new(config: ClickHouseConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(10)
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .connect_timeout(std::time::Duration::from_secs(config.connect_timeout_secs))
            .timeout(std::time::Duration::from_secs(config.request_timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let (sender, receiver) = mpsc::channel::<BatchRequest>(1024);

        let worker_config = config.clone();
        let worker_client = http_client.clone();

        let worker_handle = tokio::spawn(async move {
            Self::background_worker(worker_config, worker_client, receiver).await;
        });

        Self {
            config,
            http_client,
            sender,
            _worker_handle: worker_handle,
        }
    }

    /// 建立共享寫入器
    pub fn new_shared(config: ClickHouseConfig) -> Arc<Self> {
        Arc::new(Self::new(config))
    }

    /// 異步寫入一批數據（非阻塞，返回後數據進入發送隊列）
    pub async fn write_batch(
        &self,
        table: &str,
        writer: &mut RowBinaryWriter,
    ) -> Result<(), WriteError> {
        let (data, row_count) = writer.take_buffer();
        if row_count == 0 {
            return Ok(());
        }

        let request = BatchRequest {
            table: format!("{}.{}", self.config.database, table),
            data,
            row_count,
        };

        self.sender
            .send(request)
            .await
            .map_err(|_| WriteError::ChannelClosed)?;

        Ok(())
    }

    /// 同步寫入（阻塞直到完成）
    pub async fn write_batch_sync(
        &self,
        table: &str,
        writer: &mut RowBinaryWriter,
    ) -> Result<usize, WriteError> {
        let (data, row_count) = writer.take_buffer();
        if row_count == 0 {
            return Ok(0);
        }

        let table_full = format!("{}.{}", self.config.database, table);
        self.send_batch(&table_full, data, row_count).await?;
        Ok(row_count)
    }

    /// 背景工作執行緒
    async fn background_worker(
        config: ClickHouseConfig,
        client: reqwest::Client,
        mut receiver: mpsc::Receiver<BatchRequest>,
    ) {
        info!("ClickHouse 背景寫入器啟動");

        while let Some(request) = receiver.recv().await {
            let url = format!(
                "{}/?query={}",
                config.url,
                urlencoding::encode(&format!("INSERT INTO {} FORMAT RowBinary", request.table))
            );

            let body = if config.enable_compression {
                match Self::compress_lz4(&request.data) {
                    Ok(compressed) => compressed,
                    Err(e) => {
                        warn!("LZ4 壓縮失敗，使用原始數據: {}", e);
                        request.data
                    }
                }
            } else {
                request.data
            };

            let mut req_builder = client.post(&url);

            if config.enable_compression {
                req_builder = req_builder.header("Content-Encoding", "lz4");
            }

            if !config.password.is_empty() {
                req_builder = req_builder.basic_auth(&config.user, Some(&config.password));
            } else {
                req_builder = req_builder.basic_auth(&config.user, None::<&str>);
            }

            match req_builder.body(body).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        debug!(
                            "成功寫入 {} 行到 {}",
                            request.row_count, request.table
                        );
                    } else {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        error!(
                            "寫入失敗 {} 行到 {}: HTTP {} - {}",
                            request.row_count, request.table, status, body
                        );
                    }
                }
                Err(e) => {
                    error!(
                        "寫入請求失敗 {} 行到 {}: {}",
                        request.row_count, request.table, e
                    );
                }
            }
        }

        info!("ClickHouse 背景寫入器停止");
    }

    /// 直接發送批量數據
    async fn send_batch(
        &self,
        table: &str,
        data: Vec<u8>,
        row_count: usize,
    ) -> Result<(), WriteError> {
        let url = format!(
            "{}/?query={}",
            self.config.url,
            urlencoding::encode(&format!("INSERT INTO {} FORMAT RowBinary", table))
        );

        let body = if self.config.enable_compression {
            Self::compress_lz4(&data).unwrap_or(data)
        } else {
            data
        };

        let mut req_builder = self.http_client.post(&url);

        if self.config.enable_compression {
            req_builder = req_builder.header("Content-Encoding", "lz4");
        }

        if !self.config.password.is_empty() {
            req_builder =
                req_builder.basic_auth(&self.config.user, Some(&self.config.password));
        } else {
            req_builder = req_builder.basic_auth(&self.config.user, None::<&str>);
        }

        let resp = req_builder
            .body(body)
            .send()
            .await
            .map_err(|e| WriteError::Http(e.to_string()))?;

        if resp.status().is_success() {
            debug!("成功寫入 {} 行到 {}", row_count, table);
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(WriteError::Http(format!(
                "HTTP {} - {}",
                status, body
            )))
        }
    }

    /// LZ4 壓縮
    fn compress_lz4(data: &[u8]) -> Result<Vec<u8>, WriteError> {
        use std::io::Write as _;
        let mut encoder = lz4::EncoderBuilder::new()
            .level(1)
            .build(Vec::new())
            .map_err(|e| WriteError::Serialization(e.to_string()))?;
        encoder
            .write_all(data)
            .map_err(|e| WriteError::Serialization(e.to_string()))?;
        let (compressed, result) = encoder.finish();
        result.map_err(|e| WriteError::Serialization(e.to_string()))?;
        Ok(compressed)
    }

    /// 獲取配置引用
    pub fn config(&self) -> &ClickHouseConfig {
        &self.config
    }
}

/// 市場數據行（示例結構）
pub struct MarketDataRow {
    pub timestamp: i64,  // DateTime64(6)
    pub symbol: String,  // String
    pub bid_price: f64,  // Float64
    pub ask_price: f64,  // Float64
    pub bid_qty: f64,    // Float64
    pub ask_qty: f64,    // Float64
    pub venue: String,   // LowCardinality(String)
}

impl MarketDataRow {
    /// 轉換為 RowBinary 值列表
    pub fn to_row_binary(&self) -> Vec<RowBinaryValue> {
        vec![
            RowBinaryValue::DateTime64Micros(self.timestamp),
            RowBinaryValue::String(self.symbol.clone()),
            RowBinaryValue::Float64(self.bid_price),
            RowBinaryValue::Float64(self.ask_price),
            RowBinaryValue::Float64(self.bid_qty),
            RowBinaryValue::Float64(self.ask_qty),
            RowBinaryValue::String(self.venue.clone()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_row_binary_value_uint8() {
        let value = RowBinaryValue::UInt8(42);
        let mut buffer = Vec::new();
        value.write_to(&mut buffer).unwrap();
        assert_eq!(buffer, vec![42]);
    }

    #[test]
    fn test_row_binary_value_uint64() {
        let value = RowBinaryValue::UInt64(0x0102030405060708);
        let mut buffer = Vec::new();
        value.write_to(&mut buffer).unwrap();
        assert_eq!(buffer, vec![0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn test_row_binary_value_string() {
        let value = RowBinaryValue::String("hello".to_string());
        let mut buffer = Vec::new();
        value.write_to(&mut buffer).unwrap();
        // varint 5 = 0x05, then "hello"
        assert_eq!(buffer, vec![5, b'h', b'e', b'l', b'l', b'o']);
    }

    #[test]
    fn test_row_binary_value_nullable_null() {
        let value = RowBinaryValue::Nullable(None);
        let mut buffer = Vec::new();
        value.write_to(&mut buffer).unwrap();
        assert_eq!(buffer, vec![1]); // NULL marker
    }

    #[test]
    fn test_row_binary_value_nullable_some() {
        let value = RowBinaryValue::Nullable(Some(Box::new(RowBinaryValue::UInt32(100))));
        let mut buffer = Vec::new();
        value.write_to(&mut buffer).unwrap();
        assert_eq!(buffer, vec![0, 100, 0, 0, 0]); // NOT NULL + value
    }

    #[test]
    fn test_row_binary_writer() {
        let mut writer = RowBinaryWriter::new();

        // Write a row with multiple values
        writer
            .write_row(&[
                RowBinaryValue::UInt64(1000000),
                RowBinaryValue::String("BTCUSDT".to_string()),
                RowBinaryValue::Float64(67000.5),
            ])
            .unwrap();

        assert_eq!(writer.row_count(), 1);
        assert!(writer.buffer_size() > 0);

        let (buffer, count) = writer.take_buffer();
        assert_eq!(count, 1);
        assert!(!buffer.is_empty());
        assert_eq!(writer.row_count(), 0);
    }

    #[test]
    fn test_varint_small() {
        let mut buffer = Vec::new();
        write_varint(&mut buffer, 5).unwrap();
        assert_eq!(buffer, vec![5]);
    }

    #[test]
    fn test_varint_large() {
        let mut buffer = Vec::new();
        write_varint(&mut buffer, 300).unwrap();
        // 300 = 0b100101100 = [0xAC, 0x02]
        assert_eq!(buffer, vec![0xAC, 0x02]);
    }

    #[test]
    fn test_config_default() {
        let config = ClickHouseConfig::default();
        assert_eq!(config.url, "http://localhost:8123");
        assert_eq!(config.batch_size, 1000);
        assert!(config.enable_compression);
    }

    #[test]
    fn test_market_data_row_to_binary() {
        let row = MarketDataRow {
            timestamp: 1700000000000000, // microseconds
            symbol: "BTCUSDT".to_string(),
            bid_price: 67000.0,
            ask_price: 67001.0,
            bid_qty: 1.5,
            ask_qty: 2.0,
            venue: "BINANCE".to_string(),
        };

        let values = row.to_row_binary();
        assert_eq!(values.len(), 7);

        // Verify first value is timestamp
        match &values[0] {
            RowBinaryValue::DateTime64Micros(ts) => assert_eq!(*ts, 1700000000000000),
            _ => panic!("Expected DateTime64Micros"),
        }

        // Verify symbol
        match &values[1] {
            RowBinaryValue::String(s) => assert_eq!(s, "BTCUSDT"),
            _ => panic!("Expected String"),
        }
    }
}
