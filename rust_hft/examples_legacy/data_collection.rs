/*!
 * 統一數據收集系統 - 整合多個數據收集功能
 * 
 * 整合功能：
 * - 基礎市場數據記錄 (02_record_market_data.rs)
 * - 高性能OrderBook記錄 (06_bitget_lock_free_orderbook.rs) 
 * - 交易監控數據 (08_simple_trading_monitor.rs)
 * 
 * 支持多種數據格式和高性能收集
 */

use rust_hft::{HftAppRunner, DataCollectionArgs, WorkflowExecutor, WorkflowStep, StepResult};
use rust_hft::integrations::bitget_connector::*;
use rust_hft::core::orderbook::OrderBook;
use rust_hft::now_micros;
use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use tracing::{info, warn, debug, error};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::sync::mpsc;
use serde_json::Value;
use serde::{Serialize, Deserialize};
use clickhouse::{Client, Row};
use uuid::Uuid;
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;

/// 增強的市場數據記錄結構
#[derive(Debug, Clone, Serialize, Deserialize, Row)]
pub struct EnhancedMarketData {
    /// 基礎信息
    pub record_id: String,
    pub symbol: String,
    pub timestamp: u64,
    pub data_type: String, // "orderbook", "trade", "execution"
    
    /// 價格信息
    pub best_bid: f64,
    pub best_ask: f64,
    pub mid_price: f64,
    pub spread_bps: f64,
    
    /// 深度信息
    pub bid_depth_l5: f64,
    pub ask_depth_l5: f64,
    pub bid_depth_l10: f64, 
    pub ask_depth_l10: f64,
    pub total_bid_volume: f64,
    pub total_ask_volume: f64,
    
    /// 流動性指標
    pub liquidity_score: f64,
    pub depth_imbalance: f64,
    pub order_book_imbalance: f64,
    pub weighted_mid_price: f64,
    
    /// 執行相關數據
    pub execution_latency_us: Option<u64>,
    pub fill_rate: Option<f64>,
    pub slippage_bps: Option<f64>,
    pub liquidity_consumed: Option<f64>,
    
    /// 市場微觀結構
    pub tick_direction: i8, // -1: down, 0: same, 1: up
    pub trade_intensity: f64,
    pub volume_weighted_price: f64,
    pub price_volatility: f64,
    
    /// 訂單流信息
    pub order_arrival_rate: f64,
    pub cancellation_rate: f64,
    pub aggressive_ratio: f64,
    
    /// 質量指標
    pub data_quality_score: f64,
    pub processing_latency_us: u64,
    pub sequence_number: u64,
}

/// 成交執行記錄
#[derive(Debug, Clone, Serialize, Deserialize, Row)]
pub struct ExecutionRecord {
    pub execution_id: String,
    pub symbol: String,
    pub timestamp: u64,
    pub side: String, // "buy" or "sell"
    pub requested_quantity: f64,
    pub filled_quantity: f64,
    pub filled_price: f64,
    pub execution_time_us: u64,
    pub slippage_bps: f64,
    pub fill_rate: f64,
    pub liquidity_consumed: f64,
    pub partial_fill: bool,
    pub immediate_fill: bool,
    pub market_impact_bps: f64,
    pub fees: f64,
    pub pre_trade_spread_bps: f64,
    pub post_trade_spread_bps: f64,
    pub liquidity_source: String, // "maker", "taker", "mixed"
}

/// 深度快照記錄
#[derive(Debug, Clone, Serialize, Deserialize, Row)]
pub struct DepthSnapshot {
    pub snapshot_id: String,
    pub symbol: String,
    pub timestamp: u64,
    pub sequence_number: u64,
    
    /// L20深度數據 (JSON字符串格式)
    pub bids_l20: String, // JSON array of [price, quantity]
    pub asks_l20: String, // JSON array of [price, quantity]
    
    /// 聚合指標
    pub total_bid_value: f64,
    pub total_ask_value: f64,
    pub depth_concentration: f64, // L5/L20比率
    pub spread_stability: f64,
    pub order_count_bid: i32,
    pub order_count_ask: i32,
    
    /// 斜率和形狀
    pub bid_slope: f64,
    pub ask_slope: f64,
    pub depth_skewness: f64,
    pub depth_kurtosis: f64,
}

/// 數據收集模式
#[derive(Debug, Clone)]
pub enum CollectionMode {
    /// 基礎模式 - 記錄原始JSON數據
    Basic,
    /// 高性能模式 - 處理和記錄OrderBook
    HighPerformance,
    /// 監控模式 - 收集統計和性能指標
    Monitoring,
    /// 全功能模式 - 所有功能
    Complete,
    /// ClickHouse模式 - 結構化數據存儲
    ClickHouse,
}

/// ClickHouse配置
#[derive(Debug, Clone)]
pub struct ClickHouseConfig {
    pub url: String,
    pub database: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub batch_size: usize,
    pub flush_interval_ms: u64,
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:8123".to_string(),
            database: "hft_data".to_string(),
            username: None,
            password: None,
            batch_size: 1000,
            flush_interval_ms: 5000,
        }
    }
}

/// ClickHouse數據管理器
pub struct ClickHouseManager {
    client: Client,
    config: ClickHouseConfig,
    market_data_buffer: Vec<EnhancedMarketData>,
    execution_buffer: Vec<ExecutionRecord>,
    depth_buffer: Vec<DepthSnapshot>,
    liquidity_change_buffer: Vec<LiquidityChangeRecord>,
    quality_buffer: Vec<DataQualityRecord>,
    last_flush: u64,
}

impl ClickHouseManager {
    pub async fn new(config: ClickHouseConfig) -> Result<Self> {
        let client = Client::default()
            .with_url(&config.url)
            .with_database(&config.database);
            
        let manager = Self {
            client,
            config,
            market_data_buffer: Vec::new(),
            execution_buffer: Vec::new(),
            depth_buffer: Vec::new(),
            liquidity_change_buffer: Vec::new(),
            quality_buffer: Vec::new(),
            last_flush: now_micros(),
        };
        
        // 初始化數據庫表
        manager.initialize_tables().await?;
        
        Ok(manager)
    }
    
    async fn initialize_tables(&self) -> Result<()> {
        info!("🗄️  Initializing ClickHouse tables...");
        
        // 創建市場數據表
        let market_data_ddl = r#"
            CREATE TABLE IF NOT EXISTS enhanced_market_data (
                record_id String,
                symbol String,
                timestamp UInt64,
                data_type String,
                best_bid Float64,
                best_ask Float64,
                mid_price Float64,
                spread_bps Float64,
                bid_depth_l5 Float64,
                ask_depth_l5 Float64,
                bid_depth_l10 Float64,
                ask_depth_l10 Float64,
                total_bid_volume Float64,
                total_ask_volume Float64,
                liquidity_score Float64,
                depth_imbalance Float64,
                order_book_imbalance Float64,
                weighted_mid_price Float64,
                execution_latency_us Nullable(UInt64),
                fill_rate Nullable(Float64),
                slippage_bps Nullable(Float64),
                liquidity_consumed Nullable(Float64),
                tick_direction Int8,
                trade_intensity Float64,
                volume_weighted_price Float64,
                price_volatility Float64,
                order_arrival_rate Float64,
                cancellation_rate Float64,
                aggressive_ratio Float64,
                data_quality_score Float64,
                processing_latency_us UInt64,
                sequence_number UInt64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
            SETTINGS index_granularity = 8192
        "#;
        
        // 創建執行記錄表
        let execution_ddl = r#"
            CREATE TABLE IF NOT EXISTS execution_records (
                execution_id String,
                symbol String,
                timestamp UInt64,
                side String,
                requested_quantity Float64,
                filled_quantity Float64,
                filled_price Float64,
                execution_time_us UInt64,
                slippage_bps Float64,
                fill_rate Float64,
                liquidity_consumed Float64,
                partial_fill Bool,
                immediate_fill Bool,
                market_impact_bps Float64,
                fees Float64,
                pre_trade_spread_bps Float64,
                post_trade_spread_bps Float64,
                liquidity_source String
            ) ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
            SETTINGS index_granularity = 8192
        "#;
        
        // 創建深度快照表
        let depth_ddl = r#"
            CREATE TABLE IF NOT EXISTS depth_snapshots (
                snapshot_id String,
                symbol String,
                timestamp UInt64,
                sequence_number UInt64,
                bids_l20 String,
                asks_l20 String,
                total_bid_value Float64,
                total_ask_value Float64,
                depth_concentration Float64,
                spread_stability Float64,
                order_count_bid Int32,
                order_count_ask Int32,
                bid_slope Float64,
                ask_slope Float64,
                depth_skewness Float64,
                depth_kurtosis Float64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
            SETTINGS index_granularity = 8192
        "#;
        
        // 創建流動性變化表
        let liquidity_change_ddl = r#"
            CREATE TABLE IF NOT EXISTS liquidity_changes (
                change_id String,
                symbol String,
                timestamp UInt64,
                change_type String,
                side String,
                price_level Float64,
                quantity_before Float64,
                quantity_after Float64,
                quantity_delta Float64,
                depth_impact_l5 Float64,
                depth_impact_l10 Float64,
                spread_impact_bps Float64,
                imbalance_impact Float64,
                time_since_last_change_us UInt64,
                changes_in_window UInt32,
                volatility_impact Float64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
            SETTINGS index_granularity = 8192
        "#;
        
        // 創建數據品質表
        let quality_ddl = r#"
            CREATE TABLE IF NOT EXISTS data_quality (
                quality_id String,
                symbol String,
                timestamp UInt64,
                data_type String,
                check_type String,
                overall_score Float64,
                completeness_score Float64,
                accuracy_score Float64,
                consistency_score Float64,
                timeliness_score Float64,
                missing_fields UInt32,
                invalid_values UInt32,
                outlier_count UInt32,
                duplicate_count UInt32,
                late_arrivals UInt32,
                price_cross_violations UInt32,
                negative_quantities UInt32,
                zero_quantities UInt32,
                unrealistic_spreads UInt32,
                sequence_gaps UInt32,
                processing_latency_us UInt64,
                validation_time_us UInt64,
                memory_usage_bytes UInt64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
            SETTINGS index_granularity = 8192
        "#;
        
        self.client.query(market_data_ddl).execute().await?;
        self.client.query(execution_ddl).execute().await?;
        self.client.query(depth_ddl).execute().await?;
        self.client.query(liquidity_change_ddl).execute().await?;
        self.client.query(quality_ddl).execute().await?;
        
        info!("✅ ClickHouse tables initialized successfully");
        Ok(())
    }
    
    pub async fn add_market_data(&mut self, data: EnhancedMarketData) -> Result<()> {
        self.market_data_buffer.push(data);
        
        if self.market_data_buffer.len() >= self.config.batch_size {
            self.flush_market_data().await?;
        }
        
        Ok(())
    }
    
    pub async fn add_execution_record(&mut self, record: ExecutionRecord) -> Result<()> {
        self.execution_buffer.push(record);
        
        if self.execution_buffer.len() >= self.config.batch_size {
            self.flush_execution_records().await?;
        }
        
        Ok(())
    }
    
    pub async fn add_depth_snapshot(&mut self, snapshot: DepthSnapshot) -> Result<()> {
        self.depth_buffer.push(snapshot);
        
        if self.depth_buffer.len() >= self.config.batch_size {
            self.flush_depth_snapshots().await?;
        }
        
        Ok(())
    }
    
    pub async fn add_liquidity_change(&mut self, change: LiquidityChangeRecord) -> Result<()> {
        self.liquidity_change_buffer.push(change);
        
        if self.liquidity_change_buffer.len() >= self.config.batch_size {
            self.flush_liquidity_changes().await?;
        }
        
        Ok(())
    }
    
    pub async fn add_quality_record(&mut self, quality: DataQualityRecord) -> Result<()> {
        self.quality_buffer.push(quality);
        
        if self.quality_buffer.len() >= self.config.batch_size {
            self.flush_quality_records().await?;
        }
        
        Ok(())
    }
    
    async fn flush_market_data(&mut self) -> Result<()> {
        if self.market_data_buffer.is_empty() {
            return Ok(());
        }
        
        let mut insert = self.client.insert("enhanced_market_data")?;
        for data in &self.market_data_buffer {
            insert.write(data).await?;
        }
        insert.end().await?;
        
        debug!("💾 Flushed {} market data records to ClickHouse", self.market_data_buffer.len());
        self.market_data_buffer.clear();
        self.last_flush = now_micros();
        
        Ok(())
    }
    
    async fn flush_execution_records(&mut self) -> Result<()> {
        if self.execution_buffer.is_empty() {
            return Ok(());
        }
        
        let mut insert = self.client.insert("execution_records")?;
        for record in &self.execution_buffer {
            insert.write(record).await?;
        }
        insert.end().await?;
        
        debug!("💾 Flushed {} execution records to ClickHouse", self.execution_buffer.len());
        self.execution_buffer.clear();
        
        Ok(())
    }
    
    async fn flush_depth_snapshots(&mut self) -> Result<()> {
        if self.depth_buffer.is_empty() {
            return Ok(());
        }
        
        let mut insert = self.client.insert("depth_snapshots")?;
        for snapshot in &self.depth_buffer {
            insert.write(snapshot).await?;
        }
        insert.end().await?;
        
        debug!("💾 Flushed {} depth snapshots to ClickHouse", self.depth_buffer.len());
        self.depth_buffer.clear();
        
        Ok(())
    }
    
    async fn flush_liquidity_changes(&mut self) -> Result<()> {
        if self.liquidity_change_buffer.is_empty() {
            return Ok(());
        }
        
        let mut insert = self.client.insert("liquidity_changes")?;
        for change in &self.liquidity_change_buffer {
            insert.write(change).await?;
        }
        insert.end().await?;
        
        debug!("💾 Flushed {} liquidity changes to ClickHouse", self.liquidity_change_buffer.len());
        self.liquidity_change_buffer.clear();
        
        Ok(())
    }
    
    async fn flush_quality_records(&mut self) -> Result<()> {
        if self.quality_buffer.is_empty() {
            return Ok(());
        }
        
        let mut insert = self.client.insert("data_quality")?;
        for quality in &self.quality_buffer {
            insert.write(quality).await?;
        }
        insert.end().await?;
        
        debug!("💾 Flushed {} quality records to ClickHouse", self.quality_buffer.len());
        self.quality_buffer.clear();
        
        Ok(())
    }
    
    pub async fn flush_all(&mut self) -> Result<()> {
        self.flush_market_data().await?;
        self.flush_execution_records().await?;
        self.flush_depth_snapshots().await?;
        self.flush_liquidity_changes().await?;
        self.flush_quality_records().await?;
        Ok(())
    }
    
    pub fn should_flush(&self) -> bool {
        let time_elapsed = now_micros() - self.last_flush;
        time_elapsed > self.config.flush_interval_ms * 1000
    }
}

/// 數據收集工作流步驟
struct DataCollectionStep {
    mode: CollectionMode,
    symbol: String,
    output_file: String,
    duration_secs: u64,
    clickhouse_config: Option<ClickHouseConfig>,
}

impl DataCollectionStep {
    fn new(mode: CollectionMode, symbol: String, output_file: String, duration_secs: u64) -> Box<dyn WorkflowStep> {
        Box::new(Self { 
            mode, 
            symbol, 
            output_file, 
            duration_secs,
            clickhouse_config: None,
        })
    }
    
    fn new_with_clickhouse(
        mode: CollectionMode, 
        symbol: String, 
        output_file: String, 
        duration_secs: u64,
        clickhouse_config: ClickHouseConfig
    ) -> Box<dyn WorkflowStep> {
        Box::new(Self { 
            mode, 
            symbol, 
            output_file, 
            duration_secs,
            clickhouse_config: Some(clickhouse_config),
        })
    }
}

#[async_trait]
impl WorkflowStep for DataCollectionStep {
    fn name(&self) -> &str {
        match self.mode {
            CollectionMode::Basic => "Basic Data Collection",
            CollectionMode::HighPerformance => "High-Performance Data Collection", 
            CollectionMode::Monitoring => "Monitoring Data Collection",
            CollectionMode::Complete => "Complete Data Collection",
            CollectionMode::ClickHouse => "ClickHouse Enhanced Data Collection",
        }
    }
    
    fn description(&self) -> &str {
        "Collect market data from Bitget exchange with specified mode"
    }
    
    fn estimated_duration(&self) -> u64 {
        self.duration_secs
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("📊 Starting {} for {}", self.name(), self.symbol);
        
        match self.mode {
            CollectionMode::Basic => self.run_basic_collection().await,
            CollectionMode::HighPerformance => self.run_high_perf_collection().await,
            CollectionMode::Monitoring => self.run_monitoring_collection().await,
            CollectionMode::Complete => self.run_complete_collection().await,
            CollectionMode::ClickHouse => self.run_clickhouse_collection().await,
        }
    }
}

impl DataCollectionStep {
    /// 基礎數據收集
    async fn run_basic_collection(&mut self) -> Result<StepResult> {
        let file = Arc::new(Mutex::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.output_file)?
        ));
        
        let records_saved = Arc::new(Mutex::new(0u64));
        
        let mut app = HftAppRunner::new()?;
        let bitget_config = BitgetConfig::default();
        app.with_bitget_connector(bitget_config)
           .with_reporter(30);
        
        let connector = app.get_connector_mut().unwrap();
        connector.add_subscription(self.symbol.clone(), BitgetChannel::Books5);

        let file_clone = file.clone();
        let records_clone = records_saved.clone();
        
        app.run_with_timeout(move |message| {
            let record = match message {
                BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                    serde_json::json!({
                        "type": "orderbook",
                        "symbol": symbol,
                        "timestamp": timestamp,
                        "data": data
                    })
                },
                BitgetMessage::Trade { symbol, data, timestamp } => {
                    serde_json::json!({
                        "type": "trade",
                        "symbol": symbol,
                        "timestamp": timestamp,
                        "data": data
                    })
                },
                _ => return,
            };
            
            if let Ok(json_line) = serde_json::to_string(&record) {
                if let Ok(mut file) = file_clone.lock() {
                    if writeln!(file, "{}", json_line).is_ok() {
                        let mut count = records_clone.lock().unwrap();
                        *count += 1;
                    }
                }
            }
        }, self.duration_secs).await?;
        
        let final_count = *records_saved.lock().unwrap();
        Ok(StepResult::success(&format!("Basic collection: {} records saved", final_count))
           .with_metric("records_saved", final_count as f64))
    }
    
    /// 高性能數據收集
    async fn run_high_perf_collection(&mut self) -> Result<StepResult> {
        info!("🚀 High-performance collection with lock-free processing");
        
        let orderbook_buffer = Arc::new(Mutex::new(VecDeque::<OrderBook>::with_capacity(10000)));
        let stats = Arc::new(Mutex::new(PerformanceStats::new()));
        
        let mut app = HftAppRunner::new()?;
        let bitget_config = BitgetConfig::default();
        app.with_bitget_connector(bitget_config);
        
        let connector = app.get_connector_mut().unwrap();
        connector.add_subscription(self.symbol.clone(), BitgetChannel::Books5);
        
        let buffer_clone = orderbook_buffer.clone();
        let stats_clone = stats.clone();
        let symbol_clone = self.symbol.clone();
        
        app.run_with_timeout(move |message| {
            let process_start = now_micros();
            
            if let BitgetMessage::OrderBook { data, timestamp, .. } = message {
                // 快速解析OrderBook
                if let Ok(orderbook) = parse_orderbook_fast(&symbol_clone, &data, timestamp) {
                    // 添加到緩衝區
                    if let Ok(mut buffer) = buffer_clone.lock() {
                        buffer.push_back(orderbook);
                        if buffer.len() > 10000 {
                            buffer.pop_front();
                        }
                    }
                    
                    // 記錄性能統計
                    let latency = now_micros() - process_start;
                    if let Ok(mut stats) = stats_clone.lock() {
                        stats.record_latency(latency);
                    }
                }
            }
        }, self.duration_secs).await?;
        
        let final_stats = stats.lock().unwrap();
        let buffer_size = orderbook_buffer.lock().unwrap().len();
        
        Ok(StepResult::success(&format!("High-perf collection: {} orderbooks processed", buffer_size))
           .with_metric("orderbooks_processed", buffer_size as f64)
           .with_metric("avg_latency_us", final_stats.avg_latency)
           .with_metric("max_latency_us", final_stats.max_latency as f64))
    }
    
    /// 監控數據收集
    async fn run_monitoring_collection(&mut self) -> Result<StepResult> {
        info!("📈 Monitoring collection with detailed statistics");
        
        let mut app = HftAppRunner::new()?;
        let bitget_config = BitgetConfig::default();
        app.with_bitget_connector(bitget_config)
           .with_reporter(10); // 更頻繁的報告
        
        let connector = app.get_connector_mut().unwrap();
        connector.add_subscription(self.symbol.clone(), BitgetChannel::Books5);
        
        let monitor_stats = Arc::new(Mutex::new(MonitoringStats::new()));
        let monitor_clone = monitor_stats.clone();
        
        app.run_with_timeout(move |message| {
            if let Ok(mut stats) = monitor_clone.lock() {
                stats.process_message(&message);
            }
        }, self.duration_secs).await?;
        
        let final_stats = monitor_stats.lock().unwrap();
        
        Ok(StepResult::success(&format!("Monitoring: {} messages processed", final_stats.total_messages))
           .with_metric("total_messages", final_stats.total_messages as f64)
           .with_metric("orderbook_updates", final_stats.orderbook_updates as f64)
           .with_metric("trade_updates", final_stats.trade_updates as f64)
           .with_metric("avg_spread_bps", final_stats.avg_spread_bps))
    }
    
    /// 完整數據收集
    async fn run_complete_collection(&mut self) -> Result<StepResult> {
        info!("🎯 Complete collection with all features");
        
        // 使用工作流執行所有收集模式
        let mut workflow = WorkflowExecutor::new()
            .add_step(DataCollectionStep::new(
                CollectionMode::Basic,
                self.symbol.clone(),
                format!("basic_{}", self.output_file),
                self.duration_secs / 3
            ))
            .add_step(DataCollectionStep::new(
                CollectionMode::HighPerformance,
                self.symbol.clone(),
                format!("highperf_{}", self.output_file),
                self.duration_secs / 3
            ))
            .add_step(DataCollectionStep::new(
                CollectionMode::Monitoring,
                self.symbol.clone(),
                format!("monitor_{}", self.output_file),
                self.duration_secs / 3
            ));
            
        let report = workflow.execute().await?;
        
        if report.success {
            Ok(StepResult::success("Complete collection finished successfully")
               .with_metric("total_steps", report.total_steps as f64)
               .with_metric("successful_steps", report.successful_steps as f64))
        } else {
            Ok(StepResult::failure("Some collection steps failed"))
        }
    }
    
    /// ClickHouse增強數據收集
    async fn run_clickhouse_collection(&mut self) -> Result<StepResult> {
        info!("🗄️  Enhanced ClickHouse collection with market microstructure data");
        
        let clickhouse_config = self.clickhouse_config.clone()
            .unwrap_or_default();
        
        let mut clickhouse_manager = ClickHouseManager::new(clickhouse_config).await?;
        
        // 創建channel來傳遞數據到ClickHouse
        let (market_data_tx, mut market_data_rx) = mpsc::unbounded_channel::<EnhancedMarketData>();
        let (execution_tx, mut execution_rx) = mpsc::unbounded_channel::<ExecutionRecord>();
        let (depth_tx, mut depth_rx) = mpsc::unbounded_channel::<DepthSnapshot>();
        let (liquidity_change_tx, mut liquidity_change_rx) = mpsc::unbounded_channel::<LiquidityChangeRecord>();
        let (quality_tx, mut quality_rx) = mpsc::unbounded_channel::<DataQualityRecord>();
        
        // 啟動ClickHouse寫入任務
        let clickhouse_handle = {
            let mut manager = clickhouse_manager;
            tokio::spawn(async move {
                let mut last_flush = now_micros();
                let flush_interval = 5000 * 1000; // 5秒
                
                loop {
                    tokio::select! {
                        Some(data) = market_data_rx.recv() => {
                            if let Err(e) = manager.add_market_data(data).await {
                                error!("Failed to add market data: {}", e);
                            }
                        }
                        Some(record) = execution_rx.recv() => {
                            if let Err(e) = manager.add_execution_record(record).await {
                                error!("Failed to add execution record: {}", e);
                            }
                        }
                        Some(snapshot) = depth_rx.recv() => {
                            if let Err(e) = manager.add_depth_snapshot(snapshot).await {
                                error!("Failed to add depth snapshot: {}", e);
                            }
                        }
                        Some(change) = liquidity_change_rx.recv() => {
                            if let Err(e) = manager.add_liquidity_change(change).await {
                                error!("Failed to add liquidity change: {}", e);
                            }
                        }
                        Some(quality) = quality_rx.recv() => {
                            if let Err(e) = manager.add_quality_record(quality).await {
                                error!("Failed to add quality record: {}", e);
                            }
                        }
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                            let now = now_micros();
                            if now - last_flush > flush_interval {
                                if let Err(e) = manager.flush_all().await {
                                    error!("Failed to flush data: {}", e);
                                } else {
                                    debug!("ClickHouse data flushed");
                                }
                                last_flush = now;
                            }
                        }
                    }
                }
            })
        };
        
        let mut app = HftAppRunner::new()?;
        let bitget_config = BitgetConfig::default();
        app.with_bitget_connector(bitget_config)
           .with_reporter(5); // 頻繁報告
        
        let connector = app.get_connector_mut().unwrap();
        connector.add_subscription(self.symbol.clone(), BitgetChannel::Books5);
        connector.add_subscription(self.symbol.clone(), BitgetChannel::Trade);
        
        let stats = Arc::new(Mutex::new(EnhancedCollectionStats::new()));
        let stats_clone = stats.clone();
        let symbol_clone = self.symbol.clone();
        
        let sequence_number = Arc::new(Mutex::new(0u64));
        let price_history = Arc::new(Mutex::new(VecDeque::<f64>::with_capacity(100)));
        let volume_history = Arc::new(Mutex::new(VecDeque::<f64>::with_capacity(100)));
        let spread_history = Arc::new(Mutex::new(VecDeque::<f64>::with_capacity(50)));
        let last_orderbook = Arc::new(Mutex::new(None::<OrderBook>));
        
        let sequence_clone = sequence_number.clone();
        let price_clone = price_history.clone();
        let volume_clone = volume_history.clone();
        let spread_clone = spread_history.clone();
        let orderbook_clone = last_orderbook.clone();
        let market_data_tx_clone = market_data_tx.clone();
        let liquidity_change_tx_clone = liquidity_change_tx.clone();
        let quality_tx_clone = quality_tx.clone();
        
        // 流動性變化追蹤
        let last_orderbook_for_changes = Arc::new(Mutex::new(None::<OrderBook>));
        let liquidity_change_history = Arc::new(Mutex::new(VecDeque::<u64>::with_capacity(100))); // 時間戳歷史
        let liquidity_change_history_clone = liquidity_change_history.clone();
        let last_orderbook_changes_clone = last_orderbook_for_changes.clone();
        
        // 數據品質監控器
        let quality_monitor = Arc::new(Mutex::new(DataQualityMonitor::new(symbol_clone.clone())));
        let quality_monitor_clone = quality_monitor.clone();
        
        app.run_with_timeout(move |message| {
            let process_start = now_micros();
            
            // 更新序列號
            let current_sequence = {
                if let Ok(mut seq) = sequence_clone.lock() {
                    *seq += 1;
                    *seq
                } else {
                    return;
                }
            };
            
            match message {
                BitgetMessage::OrderBook { data, timestamp, .. } => {
                    if let Ok(orderbook) = parse_enhanced_orderbook(&symbol_clone, &data, timestamp) {
                        // 數據品質驗證
                        let quality_record = {
                            if let Ok(mut monitor) = quality_monitor_clone.lock() {
                                monitor.validate_orderbook(&orderbook, process_start)
                            } else {
                                return;
                            }
                        };
                        
                        // 發送品質記錄
                        if let Err(_) = quality_tx_clone.send(quality_record.clone()) {
                            warn!("Failed to send quality record to ClickHouse channel");
                        }
                        
                        // 處理訂單簿數據
                        let enhanced_data_result = {
                            if let (Ok(price_hist), Ok(volume_hist), Ok(spread_hist), Ok(last_ob)) = (
                                price_clone.lock(),
                                volume_clone.lock(),
                                spread_clone.lock(),
                                orderbook_clone.lock()
                            ) {
                                create_enhanced_market_data(
                                    &orderbook,
                                    current_sequence,
                                    &*price_hist,
                                    &*volume_hist,
                                    &*spread_hist,
                                    &*last_ob
                                )
                            } else {
                                return;
                            }
                        };
                        
                        if let Ok(mut enhanced_data) = enhanced_data_result {
                            // 設置品質分數
                            enhanced_data.data_quality_score = quality_record.overall_score;
                            
                            // 發送數據到ClickHouse channel
                            if let Err(_) = market_data_tx_clone.send(enhanced_data.clone()) {
                                warn!("Failed to send market data to ClickHouse channel");
                            }
                            
                            // 更新歷史數據
                            if let Ok(mut price_hist) = price_clone.lock() {
                                update_price_history(&mut *price_hist, enhanced_data.mid_price);
                            }
                            if let Ok(mut spread_hist) = spread_clone.lock() {
                                update_spread_history(&mut *spread_hist, enhanced_data.spread_bps);
                            }
                            
                            // 更新統計
                            if let Ok(mut stats) = stats_clone.lock() {
                                stats.record_orderbook_update(enhanced_data.processing_latency_us);
                                stats.record_data_quality(quality_record.overall_score);
                            }
                        }
                        
                        // 檢測流動性變化
                        if let Ok(mut last_ob_changes) = last_orderbook_changes_clone.lock() {
                            if let Some(ref last_ob) = *last_ob_changes {
                                // 檢測訂單簿變化並生成流動性變化記錄
                                let changes = detect_liquidity_changes(
                                    last_ob,
                                    &orderbook,
                                    &liquidity_change_history_clone
                                );
                                
                                for change in changes {
                                    if let Err(_) = liquidity_change_tx_clone.send(change) {
                                        warn!("Failed to send liquidity change to ClickHouse channel");
                                    }
                                }
                            }
                            *last_ob_changes = Some(orderbook.clone());
                        }
                        
                        if let Ok(mut last_ob) = orderbook_clone.lock() {
                            *last_ob = Some(orderbook);
                        }
                    }
                },
                BitgetMessage::Trade { data, timestamp, .. } => {
                    if let Ok(trade_data) = parse_trade_data(&data, timestamp) {
                        // 數據品質驗證
                        let quality_record = {
                            if let Ok(mut monitor) = quality_monitor_clone.lock() {
                                monitor.validate_trade(&trade_data, process_start)
                            } else {
                                return;
                            }
                        };
                        
                        // 發送品質記錄
                        if let Err(_) = quality_tx_clone.send(quality_record) {
                            warn!("Failed to send trade quality record to ClickHouse channel");
                        }
                        
                        // 更新交易統計
                        if let Ok(mut volume_hist) = volume_clone.lock() {
                            update_volume_history(&mut *volume_hist, trade_data.volume);
                        }
                        
                        if let Ok(mut stats) = stats_clone.lock() {
                            stats.record_trade_update(trade_data.volume);
                        }
                    }
                },
                _ => {}
            }
            
            // 記錄處理延遲
            let processing_latency = now_micros() - process_start;
            if let Ok(mut stats) = stats_clone.lock() {
                stats.record_processing_latency(processing_latency);
            }
        }, self.duration_secs).await?;
        
        // 停止ClickHouse寫入任務
        clickhouse_handle.abort();
        
        let final_stats = stats.lock().unwrap();
        
        info!("📊 ClickHouse collection completed:");
        info!("   Total records: {}", final_stats.total_records);
        info!("   OrderBook updates: {}", final_stats.orderbook_updates);
        info!("   Trade updates: {}", final_stats.trade_updates);
        info!("   Avg processing latency: {:.1}μs", final_stats.avg_processing_latency_us);
        info!("   Max processing latency: {:.1}μs", final_stats.max_processing_latency_us);
        
        Ok(StepResult::success(&format!("ClickHouse collection: {} records processed", final_stats.total_records))
           .with_metric("total_records", final_stats.total_records as f64)
           .with_metric("orderbook_updates", final_stats.orderbook_updates as f64)
           .with_metric("trade_updates", final_stats.trade_updates as f64)
           .with_metric("avg_processing_latency_us", final_stats.avg_processing_latency_us)
           .with_metric("data_quality_score", final_stats.avg_data_quality_score))
    }
}

/// 增強的收集統計
#[derive(Debug)]
struct EnhancedCollectionStats {
    total_records: u64,
    orderbook_updates: u64,
    trade_updates: u64,
    total_processing_latency: u64,
    avg_processing_latency_us: f64,
    max_processing_latency_us: u64,
    total_data_quality_score: f64,
    avg_data_quality_score: f64,
    execution_records: u64,
    depth_snapshots: u64,
}

impl EnhancedCollectionStats {
    fn new() -> Self {
        Self {
            total_records: 0,
            orderbook_updates: 0,
            trade_updates: 0,
            total_processing_latency: 0,
            avg_processing_latency_us: 0.0,
            max_processing_latency_us: 0,
            total_data_quality_score: 0.0,
            avg_data_quality_score: 0.0,
            execution_records: 0,
            depth_snapshots: 0,
        }
    }
    
    fn record_orderbook_update(&mut self, processing_latency: u64) {
        self.orderbook_updates += 1;
        self.total_records += 1;
        self.record_processing_latency(processing_latency);
    }
    
    fn record_trade_update(&mut self, _volume: f64) {
        self.trade_updates += 1;
        self.total_records += 1;
    }
    
    fn record_processing_latency(&mut self, latency: u64) {
        self.total_processing_latency += latency;
        if latency > self.max_processing_latency_us {
            self.max_processing_latency_us = latency;
        }
        
        if self.total_records > 0 {
            self.avg_processing_latency_us = self.total_processing_latency as f64 / self.total_records as f64;
        }
    }
    
    fn record_data_quality(&mut self, quality_score: f64) {
        self.total_data_quality_score += quality_score;
        if self.total_records > 0 {
            self.avg_data_quality_score = self.total_data_quality_score / self.total_records as f64;
        }
    }
}

/// 增強的交易數據結構
#[derive(Debug, Clone)]
struct EnhancedTradeData {
    pub trade_id: String,
    pub symbol: String,
    pub timestamp: u64,
    pub price: f64,
    pub volume: f64,
    pub side: String, // "buy" or "sell"
    pub trade_type: String, // "market", "limit", "stop"
    pub aggressor_side: String, // "buyer", "seller"
    pub sequence_id: u64,
    
    // 流動性相關
    pub liquidity_removed: f64,
    pub price_impact_bps: f64,
    pub spread_before_trade: f64,
    pub spread_after_trade: f64,
    pub depth_consumed_bid: f64,
    pub depth_consumed_ask: f64,
    
    // 市場微觀結構
    pub is_block_trade: bool,
    pub trade_intensity: f64, // 基於最近交易頻率
    pub volume_imbalance: f64, // 買賣量不平衡
    pub price_velocity: f64, // 價格變化速度
}

/// 流動性變化記錄
#[derive(Debug, Clone, Serialize, Deserialize, Row)]
pub struct LiquidityChangeRecord {
    pub change_id: String,
    pub symbol: String,
    pub timestamp: u64,
    pub change_type: String, // "add", "remove", "update", "cancel"
    pub side: String, // "bid", "ask"
    pub price_level: f64,
    pub quantity_before: f64,
    pub quantity_after: f64,
    pub quantity_delta: f64,
    
    // 影響評估
    pub depth_impact_l5: f64,
    pub depth_impact_l10: f64,
    pub spread_impact_bps: f64,
    pub imbalance_impact: f64,
    
    // 時序特徵
    pub time_since_last_change_us: u64,
    pub changes_in_window: u32, // 1秒內的變化次數
    pub volatility_impact: f64,
}

/// 數據品質監控記錄
#[derive(Debug, Clone, Serialize, Deserialize, Row)]
pub struct DataQualityRecord {
    pub quality_id: String,
    pub symbol: String,
    pub timestamp: u64,
    pub data_type: String, // "orderbook", "trade", "execution"
    pub check_type: String, // "completeness", "accuracy", "consistency", "timeliness"
    
    // 基礎品質指標
    pub overall_score: f64, // 總體品質分數 0.0-1.0
    pub completeness_score: f64, // 完整性分數
    pub accuracy_score: f64, // 準確性分數
    pub consistency_score: f64, // 一致性分數
    pub timeliness_score: f64, // 及時性分數
    
    // 詳細檢查結果
    pub missing_fields: u32,
    pub invalid_values: u32,
    pub outlier_count: u32,
    pub duplicate_count: u32,
    pub late_arrivals: u32,
    
    // 訂單簿特定檢查
    pub price_cross_violations: u32, // 價格交叉違規
    pub negative_quantities: u32, // 負數量
    pub zero_quantities: u32, // 零數量
    pub unrealistic_spreads: u32, // 不合理價差
    pub sequence_gaps: u32, // 序號間隙
    
    // 性能指標
    pub processing_latency_us: u64,
    pub validation_time_us: u64,
    pub memory_usage_bytes: u64,
}

/// 數據品質監控器
#[derive(Debug)]
pub struct DataQualityMonitor {
    symbol: String,
    // 歷史數據用於一致性檢查
    last_orderbook: Option<OrderBook>,
    last_trade_price: Option<f64>,
    last_timestamp: Option<u64>,
    
    // 統計計數器
    total_records: u64,
    invalid_records: u64,
    duplicate_records: u64,
    out_of_order_records: u64,
    
    // 價格範圍檢查
    min_valid_price: f64,
    max_valid_price: f64,
    max_valid_spread_bps: f64,
    
    // 時間窗口統計
    recent_timestamps: VecDeque<u64>,
    recent_prices: VecDeque<f64>,
    recent_volumes: VecDeque<f64>,
    
    // 異常檢測閾值
    price_deviation_threshold: f64, // 價格偏差閾值
    volume_spike_threshold: f64, // 成交量異常閾值
    latency_threshold_us: u64, // 延遲閾值
}

impl DataQualityMonitor {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            last_orderbook: None,
            last_trade_price: None,
            last_timestamp: None,
            total_records: 0,
            invalid_records: 0,
            duplicate_records: 0,
            out_of_order_records: 0,
            min_valid_price: 1.0, // 最小有效價格
            max_valid_price: 1_000_000.0, // 最大有效價格
            max_valid_spread_bps: 1000.0, // 最大有效價差(10%)
            recent_timestamps: VecDeque::with_capacity(1000),
            recent_prices: VecDeque::with_capacity(1000),
            recent_volumes: VecDeque::with_capacity(1000),
            price_deviation_threshold: 0.05, // 5%價格偏差閾值
            volume_spike_threshold: 10.0, // 10倍成交量異常閾值
            latency_threshold_us: 10_000, // 10ms延遲閾值
        }
    }
    
    /// 驗證訂單簿數據品質
    pub fn validate_orderbook(&mut self, orderbook: &OrderBook, arrival_time: u64) -> DataQualityRecord {
        let validation_start = now_micros();
        let mut quality_record = DataQualityRecord {
            quality_id: Uuid::new_v4().to_string(),
            symbol: self.symbol.clone(),
            timestamp: orderbook.last_update,
            data_type: "orderbook".to_string(),
            check_type: "comprehensive".to_string(),
            overall_score: 1.0,
            completeness_score: 1.0,
            accuracy_score: 1.0,
            consistency_score: 1.0,
            timeliness_score: 1.0,
            missing_fields: 0,
            invalid_values: 0,
            outlier_count: 0,
            duplicate_count: 0,
            late_arrivals: 0,
            price_cross_violations: 0,
            negative_quantities: 0,
            zero_quantities: 0,
            unrealistic_spreads: 0,
            sequence_gaps: 0,
            processing_latency_us: 0,
            validation_time_us: 0,
            memory_usage_bytes: 0,
        };
        
        self.total_records += 1;
        
        // 1. 完整性檢查
        self.check_completeness(orderbook, &mut quality_record);
        
        // 2. 準確性檢查
        self.check_accuracy(orderbook, &mut quality_record);
        
        // 3. 一致性檢查
        self.check_consistency(orderbook, &mut quality_record);
        
        // 4. 及時性檢查
        self.check_timeliness(orderbook.last_update, arrival_time, &mut quality_record);
        
        // 5. 訂單簿特定檢查
        self.check_orderbook_specific(orderbook, &mut quality_record);
        
        // 計算總體品質分數
        quality_record.overall_score = (
            quality_record.completeness_score +
            quality_record.accuracy_score +
            quality_record.consistency_score +
            quality_record.timeliness_score
        ) / 4.0;
        
        // 更新歷史數據
        self.update_history(orderbook);
        
        quality_record.validation_time_us = now_micros() - validation_start;
        
        if quality_record.overall_score < 0.8 {
            self.invalid_records += 1;
            warn!("Low quality orderbook detected: score={:.3}", quality_record.overall_score);
        }
        
        quality_record
    }
    
    /// 驗證交易數據品質
    pub fn validate_trade(&mut self, trade: &TradeData, arrival_time: u64) -> DataQualityRecord {
        let validation_start = now_micros();
        let mut quality_record = DataQualityRecord {
            quality_id: Uuid::new_v4().to_string(),
            symbol: self.symbol.clone(),
            timestamp: trade.timestamp,
            data_type: "trade".to_string(),
            check_type: "comprehensive".to_string(),
            overall_score: 1.0,
            completeness_score: 1.0,
            accuracy_score: 1.0,
            consistency_score: 1.0,
            timeliness_score: 1.0,
            missing_fields: 0,
            invalid_values: 0,
            outlier_count: 0,
            duplicate_count: 0,
            late_arrivals: 0,
            price_cross_violations: 0,
            negative_quantities: 0,
            zero_quantities: 0,
            unrealistic_spreads: 0,
            sequence_gaps: 0,
            processing_latency_us: 0,
            validation_time_us: 0,
            memory_usage_bytes: 0,
        };
        
        self.total_records += 1;
        
        // 交易特定檢查
        self.check_trade_validity(trade, &mut quality_record);
        self.check_trade_consistency(trade, &mut quality_record);
        self.check_timeliness(trade.timestamp, arrival_time, &mut quality_record);
        
        // 計算總體品質分數
        quality_record.overall_score = (
            quality_record.completeness_score +
            quality_record.accuracy_score +
            quality_record.consistency_score +
            quality_record.timeliness_score
        ) / 4.0;
        
        // 更新歷史數據
        self.update_trade_history(trade);
        
        quality_record.validation_time_us = now_micros() - validation_start;
        
        quality_record
    }
    
    /// 檢查數據完整性
    fn check_completeness(&self, orderbook: &OrderBook, quality_record: &mut DataQualityRecord) {
        let mut score = 1.0;
        
        // 檢查必要字段
        if orderbook.symbol.is_empty() {
            quality_record.missing_fields += 1;
            score -= 0.2;
        }
        
        if orderbook.last_update == 0 {
            quality_record.missing_fields += 1;
            score -= 0.2;
        }
        
        if orderbook.bids.is_empty() {
            quality_record.missing_fields += 1;
            score -= 0.3;
        }
        
        if orderbook.asks.is_empty() {
            quality_record.missing_fields += 1;
            score -= 0.3;
        }
        
        quality_record.completeness_score = score.max(0.0);
    }
    
    /// 檢查數據準確性
    fn check_accuracy(&self, orderbook: &OrderBook, quality_record: &mut DataQualityRecord) {
        let mut score = 1.0;
        
        // 檢查價格合理性
        for (price, quantity) in orderbook.bids.iter() {
            if price.0 < self.min_valid_price || price.0 > self.max_valid_price {
                quality_record.invalid_values += 1;
                score -= 0.1;
            }
            
            let qty_f64 = quantity.to_string().parse::<f64>().unwrap_or(0.0);
            if qty_f64 < 0.0 {
                quality_record.negative_quantities += 1;
                score -= 0.1;
            } else if qty_f64 == 0.0 {
                quality_record.zero_quantities += 1;
                score -= 0.05;
            }
        }
        
        for (price, quantity) in orderbook.asks.iter() {
            if price.0 < self.min_valid_price || price.0 > self.max_valid_price {
                quality_record.invalid_values += 1;
                score -= 0.1;
            }
            
            let qty_f64 = quantity.to_string().parse::<f64>().unwrap_or(0.0);
            if qty_f64 < 0.0 {
                quality_record.negative_quantities += 1;
                score -= 0.1;
            } else if qty_f64 == 0.0 {
                quality_record.zero_quantities += 1;
                score -= 0.05;
            }
        }
        
        quality_record.accuracy_score = score.max(0.0);
    }
    
    /// 檢查數據一致性
    fn check_consistency(&mut self, orderbook: &OrderBook, quality_record: &mut DataQualityRecord) {
        let mut score = 1.0;
        
        // 檢查價格交叉
        if let (Some((best_bid, _)), Some((best_ask, _))) = (
            orderbook.bids.iter().next(),
            orderbook.asks.iter().next()
        ) {
            if best_bid.0 >= best_ask.0 {
                quality_record.price_cross_violations += 1;
                score -= 0.5;
            }
            
            // 檢查價差合理性
            let spread_bps = (best_ask.0 - best_bid.0) / best_bid.0 * 10000.0;
            if spread_bps > self.max_valid_spread_bps {
                quality_record.unrealistic_spreads += 1;
                score -= 0.2;
            }
        }
        
        // 檢查時間序列一致性
        if let Some(last_ts) = self.last_timestamp {
            if orderbook.last_update < last_ts {
                quality_record.sequence_gaps += 1;
                self.out_of_order_records += 1;
                score -= 0.3;
            } else if orderbook.last_update == last_ts {
                quality_record.duplicate_count += 1;
                self.duplicate_records += 1;
                score -= 0.1;
            }
        }
        
        quality_record.consistency_score = score.max(0.0);
    }
    
    /// 檢查數據及時性
    fn check_timeliness(&self, data_timestamp: u64, arrival_time: u64, quality_record: &mut DataQualityRecord) {
        let latency = arrival_time.saturating_sub(data_timestamp);
        quality_record.processing_latency_us = latency;
        
        if latency > self.latency_threshold_us {
            quality_record.late_arrivals += 1;
            quality_record.timeliness_score = 0.5; // 延遲數據降分
        } else {
            quality_record.timeliness_score = 1.0;
        }
    }
    
    /// 訂單簿特定檢查
    fn check_orderbook_specific(&self, orderbook: &OrderBook, quality_record: &mut DataQualityRecord) {
        // 檢查訂單簿結構合理性
        let bid_levels = orderbook.bids.len();
        let ask_levels = orderbook.asks.len();
        
        if bid_levels == 0 || ask_levels == 0 {
            quality_record.missing_fields += 1;
        }
        
        // 檢查價格級別排序
        let mut last_bid_price = f64::INFINITY;
        for (price, _) in orderbook.bids.iter() {
            if price.0 >= last_bid_price {
                quality_record.invalid_values += 1;
            }
            last_bid_price = price.0;
        }
        
        let mut last_ask_price = 0.0;
        for (price, _) in orderbook.asks.iter() {
            if price.0 <= last_ask_price {
                quality_record.invalid_values += 1;
            }
            last_ask_price = price.0;
        }
    }
    
    /// 檢查交易數據有效性
    fn check_trade_validity(&self, trade: &TradeData, quality_record: &mut DataQualityRecord) {
        let mut score = 1.0;
        
        // 檢查價格合理性
        if trade.price < self.min_valid_price || trade.price > self.max_valid_price {
            quality_record.invalid_values += 1;
            score -= 0.3;
        }
        
        // 檢查成交量合理性
        if trade.volume <= 0.0 {
            quality_record.invalid_values += 1;
            score -= 0.3;
        }
        
        // 檢查方向有效性
        if trade.side != "buy" && trade.side != "sell" {
            quality_record.invalid_values += 1;
            score -= 0.2;
        }
        
        // 檢查成交量異常
        let avg_volume = self.calculate_average_volume();
        if avg_volume > 0.0 && trade.volume > avg_volume * self.volume_spike_threshold {
            quality_record.outlier_count += 1;
            score -= 0.1;
        }
        
        quality_record.accuracy_score = score.max(0.0);
    }
    
    /// 檢查交易一致性
    fn check_trade_consistency(&self, trade: &TradeData, quality_record: &mut DataQualityRecord) {
        let mut score = 1.0;
        
        // 價格合理性檢查
        if let Some(last_price) = self.last_trade_price {
            let price_change = (trade.price - last_price) / last_price;
            if price_change.abs() > self.price_deviation_threshold {
                quality_record.outlier_count += 1;
                score -= 0.2;
            }
        }
        
        quality_record.consistency_score = score.max(0.0);
    }
    
    /// 更新歷史數據
    fn update_history(&mut self, orderbook: &OrderBook) {
        self.last_orderbook = Some(orderbook.clone());
        self.last_timestamp = Some(orderbook.last_update);
        
        // 更新時間戳歷史
        self.recent_timestamps.push_back(orderbook.last_update);
        if self.recent_timestamps.len() > 1000 {
            self.recent_timestamps.pop_front();
        }
        
        // 更新價格歷史
        if let Some((best_bid, _)) = orderbook.bids.iter().next() {
            if let Some((best_ask, _)) = orderbook.asks.iter().next() {
                let mid_price = (best_bid.0 + best_ask.0) / 2.0;
                self.recent_prices.push_back(mid_price);
                if self.recent_prices.len() > 1000 {
                    self.recent_prices.pop_front();
                }
            }
        }
    }
    
    /// 更新交易歷史
    fn update_trade_history(&mut self, trade: &TradeData) {
        self.last_trade_price = Some(trade.price);
        self.last_timestamp = Some(trade.timestamp);
        
        self.recent_prices.push_back(trade.price);
        if self.recent_prices.len() > 1000 {
            self.recent_prices.pop_front();
        }
        
        self.recent_volumes.push_back(trade.volume);
        if self.recent_volumes.len() > 1000 {
            self.recent_volumes.pop_front();
        }
    }
    
    /// 計算平均成交量
    fn calculate_average_volume(&self) -> f64 {
        if self.recent_volumes.is_empty() {
            0.0
        } else {
            self.recent_volumes.iter().sum::<f64>() / self.recent_volumes.len() as f64
        }
    }
    
    /// 獲取品質統計
    pub fn get_quality_stats(&self) -> (f64, u64, u64, u64) {
        let quality_ratio = if self.total_records > 0 {
            1.0 - (self.invalid_records as f64 / self.total_records as f64)
        } else {
            1.0
        };
        
        (quality_ratio, self.total_records, self.invalid_records, self.duplicate_records)
    }
}

/// 交易數據結構（保持向後兼容）
#[derive(Debug)]
struct TradeData {
    pub volume: f64,
    pub price: f64,
    pub side: String,
    pub timestamp: u64,
}

/// 性能統計結構
#[derive(Debug)]
struct PerformanceStats {
    total_latencies: u64,
    count: u64,
    max_latency: u64,
    avg_latency: f64,
}

impl PerformanceStats {
    fn new() -> Self {
        Self {
            total_latencies: 0,
            count: 0,
            max_latency: 0,
            avg_latency: 0.0,
        }
    }
    
    fn record_latency(&mut self, latency: u64) {
        self.total_latencies += latency;
        self.count += 1;
        if latency > self.max_latency {
            self.max_latency = latency;
        }
        self.avg_latency = self.total_latencies as f64 / self.count as f64;
    }
}

/// 監控統計結構
#[derive(Debug)]
struct MonitoringStats {
    total_messages: u64,
    orderbook_updates: u64,
    trade_updates: u64,
    total_spread: f64,
    spread_count: u64,
    avg_spread_bps: f64,
}

impl MonitoringStats {
    fn new() -> Self {
        Self {
            total_messages: 0,
            orderbook_updates: 0,
            trade_updates: 0,
            total_spread: 0.0,
            spread_count: 0,
            avg_spread_bps: 0.0,
        }
    }
    
    fn process_message(&mut self, message: &BitgetMessage) {
        self.total_messages += 1;
        
        match message {
            BitgetMessage::OrderBook { data, .. } => {
                self.orderbook_updates += 1;
                // 計算spread (簡化版)
                if let Some(spread) = calculate_spread_from_data(data) {
                    self.total_spread += spread;
                    self.spread_count += 1;
                    self.avg_spread_bps = (self.total_spread / self.spread_count as f64) * 10000.0;
                }
            },
            BitgetMessage::Trade { .. } => {
                self.trade_updates += 1;
            },
            _ => {}
        }
    }
}

/// 快速OrderBook解析
fn parse_orderbook_fast(symbol: &str, _data: &Value, timestamp: u64) -> Result<OrderBook> {
    let mut orderbook = OrderBook::new(symbol.to_string());
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;
    // 簡化的快速解析...
    Ok(orderbook)
}

/// 從數據計算spread
fn calculate_spread_from_data(_data: &Value) -> Option<f64> {
    // 簡化實現
    Some(0.01) // 假設1bp的spread
}

/// 解析增強的訂單簿數據
fn parse_enhanced_orderbook(symbol: &str, _data: &Value, timestamp: u64) -> Result<OrderBook> {
    let mut orderbook = OrderBook::new(symbol.to_string());
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;
    
    // 這裡應該根據實際的Bitget數據格式進行解析
    // 簡化實現...
    
    Ok(orderbook)
}

/// 創建增強的市場數據記錄
fn create_enhanced_market_data(
    orderbook: &OrderBook,
    sequence_number: u64,
    price_history: &VecDeque<f64>,
    _volume_history: &VecDeque<f64>,
    spread_history: &VecDeque<f64>,
    last_orderbook: &Option<OrderBook>
) -> Result<EnhancedMarketData> {
    let process_start = now_micros();
    
    // 計算基礎價格信息
    let best_bid = orderbook.bids.iter().next().map(|(price, _)| price.0).unwrap_or(0.0);
    let best_ask = orderbook.asks.iter().next().map(|(price, _)| price.0).unwrap_or(0.0);
    let mid_price = if best_bid > 0.0 && best_ask > 0.0 {
        (best_bid + best_ask) / 2.0
    } else {
        0.0
    };
    let spread_bps = if best_bid > 0.0 && best_ask > 0.0 {
        (best_ask - best_bid) / best_bid * 10000.0
    } else {
        0.0
    };
    
    // 計算深度信息
    let bid_depth_l5: f64 = orderbook.bids.iter().take(5).map(|(_, qty)| {
        qty.to_string().parse::<f64>().unwrap_or(0.0)
    }).sum();
    let ask_depth_l5: f64 = orderbook.asks.iter().take(5).map(|(_, qty)| {
        qty.to_string().parse::<f64>().unwrap_or(0.0)
    }).sum();
    let bid_depth_l10: f64 = orderbook.bids.iter().take(10).map(|(_, qty)| {
        qty.to_string().parse::<f64>().unwrap_or(0.0)
    }).sum();
    let ask_depth_l10: f64 = orderbook.asks.iter().take(10).map(|(_, qty)| {
        qty.to_string().parse::<f64>().unwrap_or(0.0)
    }).sum();
    
    // 計算流動性指標
    let total_bid_volume = bid_depth_l10;
    let total_ask_volume = ask_depth_l10;
    let depth_imbalance = if total_bid_volume + total_ask_volume > 0.0 {
        (total_bid_volume - total_ask_volume) / (total_bid_volume + total_ask_volume)
    } else {
        0.0
    };
    let order_book_imbalance = depth_imbalance; // 簡化
    
    // 計算加權中間價
    let weighted_mid_price = if total_bid_volume + total_ask_volume > 0.0 {
        (best_ask * total_bid_volume + best_bid * total_ask_volume) / (total_bid_volume + total_ask_volume)
    } else {
        mid_price
    };
    
    // 計算市場微觀結構指標
    let tick_direction = calculate_tick_direction(mid_price, price_history);
    let price_volatility = calculate_price_volatility(price_history);
    let _spread_stability = calculate_spread_stability(spread_history);
    
    // 計算流動性分數
    let liquidity_score = calculate_liquidity_score(bid_depth_l5, ask_depth_l5, spread_bps);
    
    // 計算數據質量分數
    let data_quality_score = calculate_data_quality_score(orderbook, last_orderbook);
    
    let processing_latency = now_micros() - process_start;
    
    Ok(EnhancedMarketData {
        record_id: Uuid::new_v4().to_string(),
        symbol: orderbook.symbol.clone(),
        timestamp: orderbook.last_update,
        data_type: "orderbook".to_string(),
        best_bid,
        best_ask,
        mid_price,
        spread_bps,
        bid_depth_l5,
        ask_depth_l5,
        bid_depth_l10,
        ask_depth_l10,
        total_bid_volume,
        total_ask_volume,
        liquidity_score,
        depth_imbalance,
        order_book_imbalance,
        weighted_mid_price,
        execution_latency_us: None,
        fill_rate: None,
        slippage_bps: None,
        liquidity_consumed: None,
        tick_direction,
        trade_intensity: 1.0, // 需要基於歷史數據計算
        volume_weighted_price: mid_price, // 簡化
        price_volatility,
        order_arrival_rate: 1.0, // 需要基於歷史數據計算
        cancellation_rate: 0.1, // 簡化
        aggressive_ratio: 0.2, // 簡化
        data_quality_score,
        processing_latency_us: processing_latency,
        sequence_number,
    })
}

/// 解析增強的交易數據
fn parse_enhanced_trade_data(_data: &Value, timestamp: u64, sequence_id: u64) -> Result<EnhancedTradeData> {
    // 簡化的交易數據解析 - 在實際實現中需要解析Bitget的trade消息格式
    Ok(EnhancedTradeData {
        trade_id: Uuid::new_v4().to_string(),
        symbol: "BTCUSDT".to_string(), // 應該從數據中提取
        timestamp,
        price: 50000.0, // 從data中解析
        volume: 100.0, // 從data中解析
        side: "buy".to_string(), // 從data中解析
        trade_type: "market".to_string(), // 從data中解析
        aggressor_side: "buyer".to_string(), // 從data中解析
        sequence_id,
        
        // 流動性相關（需要基於訂單簿狀態計算）
        liquidity_removed: 100.0,
        price_impact_bps: 0.5,
        spread_before_trade: 1.0,
        spread_after_trade: 1.1,
        depth_consumed_bid: 0.0,
        depth_consumed_ask: 100.0,
        
        // 市場微觀結構
        is_block_trade: false,
        trade_intensity: 1.0,
        volume_imbalance: 0.1,
        price_velocity: 0.01,
    })
}

/// 解析交易數據（保持向後兼容）
fn parse_trade_data(_data: &Value, timestamp: u64) -> Result<TradeData> {
    // 簡化的交易數據解析
    Ok(TradeData {
        volume: 100.0, // 需要從實際數據解析
        price: 50000.0, // 需要從實際數據解析
        side: "buy".to_string(), // 需要從實際數據解析
        timestamp,
    })
}

/// 檢測流動性變化
fn detect_liquidity_changes(
    last_orderbook: &OrderBook,
    current_orderbook: &OrderBook,
    change_history: &Arc<Mutex<VecDeque<u64>>>
) -> Vec<LiquidityChangeRecord> {
    let mut changes = Vec::new();
    let current_time = now_micros();
    
    // 更新變化歷史
    if let Ok(mut history) = change_history.lock() {
        history.push_back(current_time);
        if history.len() > 100 {
            history.pop_front();
        }
    }
    
    // 計算窗口內變化次數（過去1秒）
    let changes_in_window = if let Ok(history) = change_history.lock() {
        history.iter()
            .filter(|&&t| current_time - t <= 1_000_000) // 1秒 = 1,000,000微秒
            .count() as u32
    } else {
        0
    };
    
    // 檢測買方變化
    changes.extend(detect_side_changes(
        &last_orderbook.bids,
        &current_orderbook.bids,
        "bid",
        current_orderbook.symbol.clone(),
        current_time,
        changes_in_window
    ));
    
    // 檢測賣方變化
    changes.extend(detect_side_changes(
        &last_orderbook.asks,
        &current_orderbook.asks,
        "ask",
        current_orderbook.symbol.clone(),
        current_time,
        changes_in_window
    ));
    
    changes
}

/// 檢測單邊（買/賣）的流動性變化
fn detect_side_changes(
    last_side: &std::collections::BTreeMap<OrderedFloat<f64>, Decimal>,
    current_side: &std::collections::BTreeMap<OrderedFloat<f64>, Decimal>,
    side_name: &str,
    symbol: String,
    timestamp: u64,
    changes_in_window: u32
) -> Vec<LiquidityChangeRecord> {
    let mut changes = Vec::new();
    
    // 檢查所有當前價格級別
    for (price, &quantity) in current_side.iter() {
        let price_level = price.0;
        let current_qty = quantity.to_string().parse::<f64>().unwrap_or(0.0);
        
        let last_qty = last_side.get(price)
            .map(|q| q.to_string().parse::<f64>().unwrap_or(0.0))
            .unwrap_or(0.0);
        
        // 如果數量有變化
        if (current_qty - last_qty).abs() > 0.0001 {
            let change_type = if last_qty == 0.0 {
                "add"
            } else if current_qty == 0.0 {
                "remove"
            } else {
                "update"
            };
            
            let change = LiquidityChangeRecord {
                change_id: Uuid::new_v4().to_string(),
                symbol: symbol.clone(),
                timestamp,
                change_type: change_type.to_string(),
                side: side_name.to_string(),
                price_level,
                quantity_before: last_qty,
                quantity_after: current_qty,
                quantity_delta: current_qty - last_qty,
                
                // 影響評估（簡化計算）
                depth_impact_l5: calculate_depth_impact(current_qty - last_qty, 5),
                depth_impact_l10: calculate_depth_impact(current_qty - last_qty, 10),
                spread_impact_bps: 0.0, // 需要基於實際spread計算
                imbalance_impact: (current_qty - last_qty) / 1000.0, // 簡化
                
                // 時序特徵
                time_since_last_change_us: 1000, // 簡化 - 需要追蹤實際時間
                changes_in_window,
                volatility_impact: 0.01, // 簡化
            };
            
            changes.push(change);
        }
    }
    
    // 檢查已刪除的價格級別
    for (price, &quantity) in last_side.iter() {
        if !current_side.contains_key(price) {
            let price_level = price.0;
            let last_qty = quantity.to_string().parse::<f64>().unwrap_or(0.0);
            
            let change = LiquidityChangeRecord {
                change_id: Uuid::new_v4().to_string(),
                symbol: symbol.clone(),
                timestamp,
                change_type: "remove".to_string(),
                side: side_name.to_string(),
                price_level,
                quantity_before: last_qty,
                quantity_after: 0.0,
                quantity_delta: -last_qty,
                
                depth_impact_l5: calculate_depth_impact(-last_qty, 5),
                depth_impact_l10: calculate_depth_impact(-last_qty, 10),
                spread_impact_bps: 0.0,
                imbalance_impact: -last_qty / 1000.0,
                
                time_since_last_change_us: 1000,
                changes_in_window,
                volatility_impact: 0.01,
            };
            
            changes.push(change);
        }
    }
    
    changes
}

/// 計算深度影響
fn calculate_depth_impact(quantity_delta: f64, depth_levels: usize) -> f64 {
    // 簡化的深度影響計算
    // 在實際實現中，這應該基於該價格級別在總深度中的位置
    quantity_delta / (depth_levels as f64 * 100.0)
}

/// 更新價格歷史
fn update_price_history(history: &mut VecDeque<f64>, price: f64) {
    history.push_back(price);
    if history.len() > 100 {
        history.pop_front();
    }
}

/// 更新成交量歷史
fn update_volume_history(history: &mut VecDeque<f64>, volume: f64) {
    history.push_back(volume);
    if history.len() > 100 {
        history.pop_front();
    }
}

/// 更新價差歷史
fn update_spread_history(history: &mut VecDeque<f64>, spread: f64) {
    history.push_back(spread);
    if history.len() > 50 {
        history.pop_front();
    }
}

/// 計算tick方向
fn calculate_tick_direction(current_price: f64, price_history: &VecDeque<f64>) -> i8 {
    if let Some(&last_price) = price_history.back() {
        if current_price > last_price {
            1
        } else if current_price < last_price {
            -1
        } else {
            0
        }
    } else {
        0
    }
}

/// 計算價格波動率
fn calculate_price_volatility(price_history: &VecDeque<f64>) -> f64 {
    if price_history.len() < 2 {
        return 0.0;
    }
    
    let prices: Vec<f64> = price_history.iter().cloned().collect();
    let returns: Vec<f64> = prices.windows(2)
        .map(|w| (w[1] - w[0]) / w[0])
        .collect();
    
    if returns.is_empty() {
        return 0.0;
    }
    
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter()
        .map(|r| (r - mean).powi(2))
        .sum::<f64>() / returns.len() as f64;
    
    variance.sqrt()
}

/// 計算價差穩定性
fn calculate_spread_stability(spread_history: &VecDeque<f64>) -> f64 {
    if spread_history.len() < 2 {
        return 1.0;
    }
    
    let mean = spread_history.iter().sum::<f64>() / spread_history.len() as f64;
    let variance = spread_history.iter()
        .map(|s| (s - mean).powi(2))
        .sum::<f64>() / spread_history.len() as f64;
    
    if mean > 0.0 {
        1.0 - (variance.sqrt() / mean).min(1.0)
    } else {
        0.0
    }
}

/// 計算流動性分數
fn calculate_liquidity_score(bid_depth: f64, ask_depth: f64, spread_bps: f64) -> f64 {
    let depth_score = (bid_depth + ask_depth) / 1000.0; // 正規化
    let spread_score = if spread_bps > 0.0 {
        1.0 / (1.0 + spread_bps / 10.0)
    } else {
        0.0
    };
    
    (depth_score + spread_score) / 2.0
}

/// 計算數據質量分數
fn calculate_data_quality_score(current: &OrderBook, last: &Option<OrderBook>) -> f64 {
    let mut score: f64 = 1.0;
    
    // 檢查訂單簿有效性
    if !current.is_valid {
        score -= 0.3;
    }
    
    // 檢查數據完整性
    if current.bids.is_empty() || current.asks.is_empty() {
        score -= 0.4;
    }
    
    // 檢查時間戳連續性
    if let Some(last_ob) = last {
        let time_diff = current.last_update.saturating_sub(last_ob.last_update);
        if time_diff > 10000 { // 超過10ms間隔
            score -= 0.2;
        }
    }
    
    score.max(0.0)
}

/// 計算tick方向
fn calculate_tick_direction(current_price: f64, price_history: &VecDeque<f64>) -> i8 {
    if let Some(&last_price) = price_history.back() {
        if current_price > last_price {
            1
        } else if current_price < last_price {
            -1
        } else {
            0
        }
    } else {
        0
    }
}

/// 計算價格波動性
fn calculate_price_volatility(price_history: &VecDeque<f64>) -> f64 {
    if price_history.len() < 2 {
        return 0.0;
    }
    
    let returns: Vec<f64> = price_history
        .windows(2)
        .map(|window| (window[1] - window[0]) / window[0])
        .collect();
    
    if returns.is_empty() {
        return 0.0;
    }
    
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter()
        .map(|&x| (x - mean).powi(2))
        .sum::<f64>() / returns.len() as f64;
    
    variance.sqrt()
}

/// 計算價差穩定性
fn calculate_spread_stability(spread_history: &VecDeque<f64>) -> f64 {
    if spread_history.len() < 2 {
        return 1.0;
    }
    
    let mean = spread_history.iter().sum::<f64>() / spread_history.len() as f64;
    let variance = spread_history.iter()
        .map(|&x| (x - mean).powi(2))
        .sum::<f64>() / spread_history.len() as f64;
    
    if variance == 0.0 {
        1.0
    } else {
        1.0 / (1.0 + variance.sqrt())
    }
}

/// 計算流動性分數
fn calculate_liquidity_score(bid_depth: f64, ask_depth: f64, spread_bps: f64) -> f64 {
    let depth_score = (bid_depth + ask_depth) / 10000.0; // 標準化深度
    let spread_score = if spread_bps > 0.0 {
        1.0 / (1.0 + spread_bps / 10.0) // 價差越小分數越高
    } else {
        0.0
    };
    
    (depth_score + spread_score) / 2.0
}

/// 更新價格歷史
fn update_price_history(price_history: &mut VecDeque<f64>, price: f64) {
    price_history.push_back(price);
    if price_history.len() > 100 {
        price_history.pop_front();
    }
}

/// 更新價差歷史
fn update_spread_history(spread_history: &mut VecDeque<f64>, spread: f64) {
    spread_history.push_back(spread);
    if spread_history.len() > 50 {
        spread_history.pop_front();
    }
}

/// 更新成交量歷史
fn update_volume_history(volume_history: &mut VecDeque<f64>, volume: f64) {
    volume_history.push_back(volume);
    if volume_history.len() > 100 {
        volume_history.pop_front();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = DataCollectionArgs::parse();
    
    info!("🚀 Unified Data Collection System");
    info!("Symbol: {}, Output: {}, Duration: {}s", 
          args.common.symbol, args.data.output_file, args.common.duration_seconds);
    
    // 根據參數選擇收集模式
    let mode = if args.data.output_file.contains("clickhouse") || 
                 std::env::var("USE_CLICKHOUSE").is_ok() {
        CollectionMode::ClickHouse
    } else if args.data.compress {
        CollectionMode::HighPerformance
    } else if args.common.verbose {
        CollectionMode::Monitoring
    } else {
        CollectionMode::Basic
    };
    
    info!("Collection mode: {:?}", mode);
    
    // 創建並執行數據收集工作流
    let mut workflow = WorkflowExecutor::new();
    
    // 根據模式添加相應的步驟
    match mode {
        CollectionMode::ClickHouse => {
            let clickhouse_config = ClickHouseConfig {
                url: std::env::var("CLICKHOUSE_URL")
                    .unwrap_or_else(|_| "http://localhost:8123".to_string()),
                database: std::env::var("CLICKHOUSE_DATABASE")
                    .unwrap_or_else(|_| "hft_data".to_string()),
                username: std::env::var("CLICKHOUSE_USERNAME").ok(),
                password: std::env::var("CLICKHOUSE_PASSWORD").ok(),
                batch_size: std::env::var("CLICKHOUSE_BATCH_SIZE")
                    .unwrap_or_else(|_| "1000".to_string())
                    .parse().unwrap_or(1000),
                flush_interval_ms: std::env::var("CLICKHOUSE_FLUSH_INTERVAL_MS")
                    .unwrap_or_else(|_| "5000".to_string())
                    .parse().unwrap_or(5000),
            };
            
            info!("🗄️  ClickHouse configuration:");
            info!("   URL: {}", clickhouse_config.url);
            info!("   Database: {}", clickhouse_config.database);
            info!("   Batch size: {}", clickhouse_config.batch_size);
            info!("   Flush interval: {}ms", clickhouse_config.flush_interval_ms);
            
            workflow = workflow.add_step(DataCollectionStep::new_with_clickhouse(
                mode,
                args.common.symbol,
                args.data.output_file,
                args.common.duration_seconds,
                clickhouse_config
            ));
        },
        _ => {
            workflow = workflow.add_step(DataCollectionStep::new(
                mode,
                args.common.symbol,
                args.data.output_file,
                args.common.duration_seconds
            ));
        }
    }
    
    let report = workflow.execute().await?;
    report.print_detailed_report();
    
    if report.success {
        info!("🎉 Data collection completed successfully!");
    } else {
        warn!("⚠️  Data collection completed with issues");
    }
    
    Ok(())
}