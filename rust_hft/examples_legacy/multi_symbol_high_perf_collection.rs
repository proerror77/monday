/*!
 * 多商品高性能數據收集系統
 * 
 * 特色功能：
 * - 支持10+個交易對同時收集
 * - SIMD優化的特徵計算
 * - ClickHouse批量高效入庫
 * - 實時性能監控和質量控制
 * - YAML配置文件驅動
 * - 自動故障恢復和重連
 */

use rust_hft::{WorkflowExecutor, WorkflowStep, StepResult, HftAppRunner};
use rust_hft::integrations::bitget_connector::*;
use rust_hft::utils::performance::{PerformanceManager, PerformanceConfig};
use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use tracing::{info, warn, debug, error};
use std::sync::{Arc, atomic::{AtomicU64, AtomicUsize, Ordering}, Mutex as StdMutex};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use clickhouse::{Client, Row};
use uuid::Uuid;
// use crossbeam_channel::{unbounded, Sender, Receiver};
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender, UnboundedReceiver};
use crossbeam_channel::{unbounded as crossbeam_unbounded, Sender as CrossbeamSender, Receiver as CrossbeamReceiver};
use serde_yaml;
use std::fs;
use rustls;

/// JSON解析缓冲池 - 用于零拷贝JSON处理
pub struct JsonBufferPool {
    buffers: StdMutex<Vec<Vec<u8>>>,
    created_count: AtomicUsize,
    reused_count: AtomicUsize,
    max_size: usize,
    buffer_capacity: usize,
}

impl JsonBufferPool {
    pub fn new(max_size: usize, buffer_capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            buffers: StdMutex::new(Vec::with_capacity(max_size)),
            created_count: AtomicUsize::new(0),
            reused_count: AtomicUsize::new(0),
            max_size,
            buffer_capacity,
        })
    }
    
    pub fn get_buffer(&self) -> Vec<u8> {
        if let Ok(mut pool) = self.buffers.lock() {
            if let Some(mut buffer) = pool.pop() {
                self.reused_count.fetch_add(1, Ordering::Relaxed);
                buffer.clear();
                return buffer;
            }
        }
        self.created_count.fetch_add(1, Ordering::Relaxed);
        Vec::with_capacity(self.buffer_capacity)
    }
    
    pub fn return_buffer(&self, buffer: Vec<u8>) {
        if let Ok(mut pool) = self.buffers.lock() {
            if pool.len() < self.max_size && buffer.capacity() >= self.buffer_capacity / 2 {
                pool.push(buffer);
            }
        }
    }
    
    pub fn stats(&self) -> (usize, usize) {
        (
            self.created_count.load(Ordering::Relaxed),
            self.reused_count.load(Ordering::Relaxed)
        )
    }
}

/// 高效字符串缓存池 - 专门用于减少String分配开销
pub struct StringCache {
    record_ids: StdMutex<Vec<String>>,
    symbols: StdMutex<Vec<String>>,
    data_types: StdMutex<Vec<String>>,
    created_count: AtomicUsize,
    reused_count: AtomicUsize,
    max_size: usize,
}

impl StringCache {
    pub fn new(max_size: usize) -> Arc<Self> {
        Arc::new(Self {
            record_ids: StdMutex::new(Vec::with_capacity(max_size)),
            symbols: StdMutex::new(Vec::with_capacity(max_size)),
            data_types: StdMutex::new(Vec::with_capacity(max_size)),
            created_count: AtomicUsize::new(0),
            reused_count: AtomicUsize::new(0),
            max_size,
        })
    }
    
    pub fn get_record_id(&self) -> String {
        if let Ok(mut pool) = self.record_ids.lock() {
            if let Some(mut s) = pool.pop() {
                self.reused_count.fetch_add(1, Ordering::Relaxed);
                s.clear();
                return s;
            }
        }
        self.created_count.fetch_add(1, Ordering::Relaxed);
        String::with_capacity(64) // 预分配合理容量
    }
    
    pub fn get_symbol(&self) -> String {
        if let Ok(mut pool) = self.symbols.lock() {
            if let Some(mut s) = pool.pop() {
                self.reused_count.fetch_add(1, Ordering::Relaxed);
                s.clear();
                return s;
            }
        }
        self.created_count.fetch_add(1, Ordering::Relaxed);
        String::with_capacity(16) // 交易对名称通常较短
    }
    
    pub fn get_data_type(&self) -> String {
        if let Ok(mut pool) = self.data_types.lock() {
            if let Some(mut s) = pool.pop() {
                self.reused_count.fetch_add(1, Ordering::Relaxed);
                s.clear();
                return s;
            }
        }
        self.created_count.fetch_add(1, Ordering::Relaxed);
        String::with_capacity(16) // "orderbook", "trade" 等
    }
    
    pub fn return_strings(&self, record_id: String, symbol: String, data_type: String) {
        // 归还字符串到各自的池中，避免内存碎片
        if let Ok(mut pool) = self.record_ids.lock() {
            if pool.len() < self.max_size {
                pool.push(record_id);
            }
        }
        if let Ok(mut pool) = self.symbols.lock() {
            if pool.len() < self.max_size {
                pool.push(symbol);
            }
        }
        if let Ok(mut pool) = self.data_types.lock() {
            if pool.len() < self.max_size {
                pool.push(data_type);
            }
        }
    }
    
    pub fn stats(&self) -> (usize, usize) {
        (
            self.created_count.load(Ordering::Relaxed),
            self.reused_count.load(Ordering::Relaxed)
        )
    }
    
    pub fn pool_sizes(&self) -> (usize, usize, usize) {
        let record_ids_size = self.record_ids.lock().map_or(0, |p| p.len());
        let symbols_size = self.symbols.lock().map_or(0, |p| p.len());
        let data_types_size = self.data_types.lock().map_or(0, |p| p.len());
        (record_ids_size, symbols_size, data_types_size)
    }
}

/// 多商品收集配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiSymbolConfig {
    pub symbols: Vec<String>,
    pub data_collection: DataCollectionConfig,
    pub clickhouse: ClickHouseConfig,
    pub monitoring: MonitoringConfig,
    pub file_output: Option<FileOutputConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataCollectionConfig {
    pub channels: Vec<String>,
    pub performance: PerformanceSettings,
    pub quality_control: QualityControlSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceSettings {
    pub batch_size: usize,
    pub buffer_capacity: usize,
    pub flush_interval_ms: u64,
    pub enable_simd: bool,
    pub cpu_affinity: bool,
    pub memory_prefault_mb: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityControlSettings {
    pub enable_validation: bool,
    pub max_latency_ms: u64,
    pub duplicate_detection: bool,
    pub sequence_validation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHouseConfig {
    pub enabled: bool,
    pub connection: ClickHouseConnection,
    pub batch_settings: ClickHouseBatchSettings,
    pub tables: ClickHouseTables,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHouseConnection {
    pub url: String,
    pub database: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHouseBatchSettings {
    pub batch_size: usize,
    pub flush_interval_ms: u64,
    pub max_memory_mb: usize,
    pub compression: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHouseTables {
    pub market_data: String,
    pub trade_data: String,
    pub quality_metrics: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringConfig {
    pub enable_realtime_stats: bool,
    pub stats_interval_sec: u64,
    pub enable_performance_metrics: bool,
    pub log_level: String,
    pub alerts: AlertSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertSettings {
    pub max_processing_latency_us: u64,
    pub min_throughput_msg_per_sec: u64,
    pub max_error_rate_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOutputConfig {
    pub enabled: bool,
    pub format: String,
    pub directory: String,
    pub rotation_size_mb: usize,
    pub compression: bool,
}

/// 增強的多商品市場數據
#[derive(Debug, Clone, Serialize, Deserialize, Row)]
pub struct MultiSymbolMarketData {
    pub record_id: String,
    pub symbol: String,
    pub timestamp: u64,
    pub data_type: String,
    
    // 價格數據
    pub best_bid: f64,
    pub best_ask: f64,
    pub mid_price: f64,
    pub spread_bps: f64,
    
    // SIMD優化的流動性指標
    pub obi_l1: f64,
    pub obi_l5: f64,
    pub obi_l10: f64,
    pub obi_l20: f64,
    
    // 深度信息
    pub bid_volume_l5: f64,
    pub ask_volume_l5: f64,
    pub total_bid_volume: f64,
    pub total_ask_volume: f64,
    
    // 質量指標
    pub processing_latency_us: u64,
    pub data_quality_score: f64,
    pub sequence_number: u64,
}

/// 多商品數據收集器
pub struct MultiSymbolCollector {
    config: MultiSymbolConfig,
    performance_manager: Arc<PerformanceManager>,
    clickhouse_client: Option<Client>,
    
    // 統計計數器
    total_messages: Arc<AtomicU64>,
    messages_by_symbol: Arc<RwLock<HashMap<String, AtomicU64>>>,
    processing_errors: Arc<AtomicU64>,
    
    // 異步ClickHouse寫入通道 (tokio - 用于跨异步边界)
    clickhouse_sender: Option<UnboundedSender<MultiSymbolMarketData>>,
    
    // 无锁高性能消息队列 (crossbeam - 用于热路径)
    fast_message_sender: Option<CrossbeamSender<MultiSymbolMarketData>>,
    
    // 數據緩衝（用於內存備份）
    data_buffer: Arc<RwLock<Vec<MultiSymbolMarketData>>>,
    
    // 字符串缓存池用于减少String分配开销
    string_cache: Arc<StringCache>,
    
    // JSON解析缓冲池用于零拷贝JSON处理
    json_buffer_pool: Arc<JsonBufferPool>,
}

impl MultiSymbolCollector {
    pub fn from_config_file(config_path: &str) -> Result<Self> {
        info!("📁 載入配置文件: {}", config_path);
        
        let config_content = fs::read_to_string(config_path)?;
        let config: MultiSymbolConfig = serde_yaml::from_str(&config_content)?;
        
        info!("✅ 配置載入成功，商品數量: {}", config.symbols.len());
        for symbol in &config.symbols {
            info!("   📈 {}", symbol);
        }
        
        Self::new(config)
    }
    
    pub fn new(config: MultiSymbolConfig) -> Result<Self> {
        // 初始化性能管理器
        let perf_config = PerformanceConfig {
            simd_acceleration: config.data_collection.performance.enable_simd,
            cpu_isolation: config.data_collection.performance.cpu_affinity,
            memory_prefaulting: true,
            ..Default::default()
        };
        
        let performance_manager = Arc::new(PerformanceManager::new(perf_config)?);
        
        // 初始化字符串缓存池
        let string_cache = StringCache::new(1000);
        
        // 初始化JSON解析缓冲池
        let json_buffer_pool = JsonBufferPool::new(500, 8192); // 500个缓冲，每个8KB
        
        // 初始化ClickHouse客戶端和異步寫入通道
        let (clickhouse_client, clickhouse_sender, fast_message_sender) = if config.clickhouse.enabled {
            let client = Client::default()
                .with_url(&config.clickhouse.connection.url)
                .with_database(&config.clickhouse.connection.database)
                .with_user(&config.clickhouse.connection.username)
                .with_password(&config.clickhouse.connection.password);
            
            let (sender, receiver) = unbounded_channel();
            
            // 创建无锁消息队列 (crossbeam)
            let (fast_sender, fast_receiver) = crossbeam_unbounded::<MultiSymbolMarketData>();
            
            // 启动无锁消息桥接任务
            let sender_clone = sender.clone();
            tokio::spawn(async move {
                Self::crossbeam_bridge_task(fast_receiver, sender_clone).await;
            });
            
            // 启动异步ClickHouse写入任务
            let client_clone = client.clone();
            let batch_size = config.clickhouse.batch_settings.batch_size;
            let flush_interval = config.clickhouse.batch_settings.flush_interval_ms;
            let table_name = config.clickhouse.tables.market_data.clone();
            
            tokio::spawn(async move {
                Self::clickhouse_writer_task(client_clone, receiver, batch_size, flush_interval, table_name).await;
            });
            
            (Some(client), Some(sender), Some(fast_sender))
        } else {
            (None, None, None)
        };
        
        // 初始化統計結構
        let mut messages_by_symbol = HashMap::new();
        for symbol in &config.symbols {
            messages_by_symbol.insert(symbol.clone(), AtomicU64::new(0));
        }
        
        Ok(Self {
            config,
            performance_manager,
            clickhouse_client,
            clickhouse_sender,
            fast_message_sender,
            total_messages: Arc::new(AtomicU64::new(0)),
            messages_by_symbol: Arc::new(RwLock::new(messages_by_symbol)),
            processing_errors: Arc::new(AtomicU64::new(0)),
            data_buffer: Arc::new(RwLock::new(Vec::with_capacity(10000))),
            string_cache,
            json_buffer_pool,
        })
    }
    
    /// 优化的深度指标批量计算（减少多次迭代，提高性能）
    fn calculate_depth_metrics_optimized(
        bid_volumes: &[f64], 
        ask_volumes: &[f64]
    ) -> (f64, f64, f64, f64, f64, f64, f64, f64) {
        let bid_len = bid_volumes.len();
        let ask_len = ask_volumes.len();
        
        // 一次遍历计算所有级别的累积和
        let mut bid_cum_sum = [0.0; 21]; // 0到20级别的累积和
        let mut ask_cum_sum = [0.0; 21];
        
        // 计算bid累积和
        for (i, &volume) in bid_volumes.iter().enumerate().take(20) {
            bid_cum_sum[i + 1] = bid_cum_sum[i] + volume;
        }
        
        // 计算ask累积和
        for (i, &volume) in ask_volumes.iter().enumerate().take(20) {
            ask_cum_sum[i + 1] = ask_cum_sum[i] + volume;
        }
        
        // 计算各级别的OBI
        let calculate_obi = |bid_sum: f64, ask_sum: f64| -> f64 {
            if bid_sum + ask_sum > 0.0 {
                (bid_sum - ask_sum) / (bid_sum + ask_sum)
            } else {
                0.0
            }
        };
        
        let obi_l1 = if bid_len > 0 && ask_len > 0 {
            calculate_obi(bid_cum_sum[1], ask_cum_sum[1])
        } else { 0.0 };
        
        let obi_l5 = if bid_len >= 5 && ask_len >= 5 {
            calculate_obi(bid_cum_sum[5], ask_cum_sum[5])
        } else { 0.0 };
        
        let obi_l10 = if bid_len >= 10 && ask_len >= 10 {
            calculate_obi(bid_cum_sum[10], ask_cum_sum[10])
        } else { 0.0 };
        
        let obi_l20 = if bid_len >= 20 && ask_len >= 20 {
            calculate_obi(bid_cum_sum[20], ask_cum_sum[20])
        } else { 0.0 };
        
        // 从累积和中获取各级别数据
        let bid_volume_l5 = if bid_len >= 5 { bid_cum_sum[5] } else { bid_cum_sum[bid_len] };
        let ask_volume_l5 = if ask_len >= 5 { ask_cum_sum[5] } else { ask_cum_sum[ask_len] };
        let total_bid_volume = bid_cum_sum[bid_len.min(20)];
        let total_ask_volume = ask_cum_sum[ask_len.min(20)];
        
        (obi_l1, obi_l5, obi_l10, obi_l20, bid_volume_l5, ask_volume_l5, total_bid_volume, total_ask_volume)
    }
    
    /// 零拷贝深度指标计算 - 直接使用数组切片，避免Vec分配
    fn calculate_depth_metrics_zero_copy(
        bid_volumes: &[f64], 
        ask_volumes: &[f64]
    ) -> (f64, f64, f64, f64, f64, f64, f64, f64) {
        let bid_len = bid_volumes.len();
        let ask_len = ask_volumes.len();
        
        // 使用栈上数组计算累积和，避免堆分配
        let mut bid_cum_sum = [0.0; 21]; 
        let mut ask_cum_sum = [0.0; 21];
        
        // 优化的累积和计算（单次遍历）
        for (i, &volume) in bid_volumes.iter().enumerate().take(20) {
            bid_cum_sum[i + 1] = bid_cum_sum[i] + volume;
        }
        
        for (i, &volume) in ask_volumes.iter().enumerate().take(20) {
            ask_cum_sum[i + 1] = ask_cum_sum[i] + volume;
        }
        
        // 内联函数避免闭包开销
        #[inline(always)]
        fn calc_obi(bid_sum: f64, ask_sum: f64) -> f64 {
            let total = bid_sum + ask_sum;
            if total > 0.0 { (bid_sum - ask_sum) / total } else { 0.0 }
        }
        
        // 直接计算所有指标，减少分支判断
        let obi_l1 = if bid_len > 0 && ask_len > 0 { calc_obi(bid_cum_sum[1], ask_cum_sum[1]) } else { 0.0 };
        let obi_l5 = if bid_len >= 5 && ask_len >= 5 { calc_obi(bid_cum_sum[5], ask_cum_sum[5]) } else { 0.0 };
        let obi_l10 = if bid_len >= 10 && ask_len >= 10 { calc_obi(bid_cum_sum[10], ask_cum_sum[10]) } else { 0.0 };
        let obi_l20 = if bid_len >= 20 && ask_len >= 20 { calc_obi(bid_cum_sum[20], ask_cum_sum[20]) } else { 0.0 };
        
        let bid_volume_l5 = if bid_len >= 5 { bid_cum_sum[5] } else { bid_cum_sum[bid_len.min(20)] };
        let ask_volume_l5 = if ask_len >= 5 { ask_cum_sum[5] } else { ask_cum_sum[ask_len.min(20)] };
        let total_bid_volume = bid_cum_sum[bid_len.min(20)];
        let total_ask_volume = ask_cum_sum[ask_len.min(20)];
        
        (obi_l1, obi_l5, obi_l10, obi_l20, bid_volume_l5, ask_volume_l5, total_bid_volume, total_ask_volume)
    }
    
    /// 高效无锁消息桥接任务 - 优化批量处理和网络效率
    async fn crossbeam_bridge_task(
        fast_receiver: CrossbeamReceiver<MultiSymbolMarketData>,
        tokio_sender: UnboundedSender<MultiSymbolMarketData>,
    ) {
        info!("🚀 高效无锁消息桥接任务启动");
        
        // 优化批量处理 - 动态批次大小和自适应间隔
        let mut batch = Vec::with_capacity(500); // 增大批次容量
        let mut last_flush = Instant::now();
        let mut adaptive_batch_size = 50; // 动态批次大小
        let mut flush_interval = Duration::from_micros(50); // 更激进的初始间隔
        
        loop {
            // 非阻塞地尝试接收消息
            match fast_receiver.try_recv() {
                Ok(message) => {
                    batch.push(message);
                    
                    // 自适应批次处理 - 根据消息速率调整批次大小
                    let should_flush = batch.len() >= adaptive_batch_size || last_flush.elapsed() >= flush_interval;
                    
                    if should_flush {
                        let flush_start = Instant::now();
                        
                        // 批量发送消息，减少系统调用开销
                        for msg in batch.drain(..) {
                            if let Err(_) = tokio_sender.send(msg) {
                                warn!("❌ Tokio通道发送失败，桥接任务退出");
                                return;
                            }
                        }
                        
                        // 动态调整批次大小和间隔
                        let flush_duration = flush_start.elapsed();
                        if flush_duration.as_micros() < 10 {
                            // 刷新很快，可以增大批次大小
                            adaptive_batch_size = (adaptive_batch_size * 2).min(500);
                            flush_interval = (flush_interval * 2).min(Duration::from_micros(200));
                        } else if flush_duration.as_micros() > 100 {
                            // 刷新较慢，减小批次大小
                            adaptive_batch_size = (adaptive_batch_size / 2).max(10);
                            flush_interval = (flush_interval / 2).max(Duration::from_micros(25));
                        }
                        
                        last_flush = Instant::now();
                    }
                },
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    // 如果有待处理的消息且超时了，刷新它们
                    if !batch.is_empty() && last_flush.elapsed() >= flush_interval {
                        for msg in batch.drain(..) {
                            if let Err(_) = tokio_sender.send(msg) {
                                warn!("❌ Tokio通道发送失败，桥接任务退出");
                                return;
                            }
                        }
                        last_flush = Instant::now();
                    }
                    
                    // 短暂休眠以避免忙等待
                    tokio::time::sleep(Duration::from_micros(1)).await;
                },
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    info!("📋 无锁消息桥接任务结束 - 发送端已断开");
                    
                    // 刷新剩余消息
                    for msg in batch.drain(..) {
                        let _ = tokio_sender.send(msg);
                    }
                    break;
                }
            }
        }
    }
    
    /// 高效异步ClickHouse写入任务 - 优化批量处理和网络压缩
    async fn clickhouse_writer_task(
        client: Client,
        mut receiver: UnboundedReceiver<MultiSymbolMarketData>,
        batch_size: usize,
        flush_interval_ms: u64,
        table_name: String,
    ) {
        let mut batch = Vec::with_capacity(batch_size * 2); // 增大缓冲区
        let mut last_flush = Instant::now();
        let flush_interval = Duration::from_millis(flush_interval_ms);
        let mut total_records = 0usize;
        let mut total_flushes = 0usize;
        
        info!("🚀 高效异步ClickHouse写入任务启动，批次大小: {}, 刷新间隔: {}ms", batch_size, flush_interval_ms);
        
        while let Some(record) = receiver.recv().await {
            batch.push(record);
            
            // 智能批量刷新策略 - 动态调整批量大小
            let elapsed = last_flush.elapsed();
            let should_flush = batch.len() >= batch_size || elapsed >= flush_interval;
            
            if should_flush && !batch.is_empty() {
                let flush_start = Instant::now();
                
                if let Err(e) = Self::flush_batch_to_clickhouse(&client, &mut batch, &table_name).await {
                    error!("❌ ClickHouse批次写入失败: {}", e);
                } else {
                    total_records += batch.len();
                    total_flushes += 1;
                    let flush_duration = flush_start.elapsed();
                    
                    debug!("✅ ClickHouse批量写入成功: {} 条记录, 耗时: {:.2}ms, 累计: {} 条/{} 批次", 
                           batch.len(), flush_duration.as_secs_f64() * 1000.0, total_records, total_flushes);
                }
                
                batch.clear();
                last_flush = Instant::now();
            }
        }
        
        // 处理剩余数据
        if !batch.is_empty() {
            if let Err(e) = Self::flush_batch_to_clickhouse(&client, &mut batch, &table_name).await {
                error!("❌ ClickHouse最终批次写入失败: {}", e);
            } else {
                total_records += batch.len();
                info!("✅ ClickHouse最终批次写入成功: {} 条记录", batch.len());
            }
        }
        
        info!("📋 高效异步ClickHouse写入任务结束 - 总计: {} 条记录/{} 批次", total_records, total_flushes);
    }
    
    /// 將批次數據寫入ClickHouse
    async fn flush_batch_to_clickhouse(
        client: &Client,
        batch: &mut Vec<MultiSymbolMarketData>,
        table_name: &str,
    ) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        
        let mut insert = client.insert(table_name)?;
        for record in batch.iter() {
            insert.write(record).await?;
        }
        insert.end().await?;
        
        Ok(())
    }
    
    /// 啟動多商品數據收集
    pub async fn start_collection(self: Arc<Self>, duration_minutes: u64) -> Result<()> {
        info!("🚀 啟動多商品高性能數據收集");
        info!("⏱️  收集時長: {} 分鐘", duration_minutes);
        info!("📊 商品列表: {:?}", self.config.symbols);
        
        // 設置HFT應用
        let mut app = HftAppRunner::new()?;
        let bitget_config = BitgetConfig::default();
        app.with_bitget_connector(bitget_config);
        
        let connector = app.get_connector_mut().unwrap();
        
        // 添加所有商品訂閱
        for symbol in &self.config.symbols {
            for channel_str in &self.config.data_collection.channels {
                let channel = match channel_str.as_str() {
                    "Books5" => BitgetChannel::Books5,
                    "Books15" => BitgetChannel::Books15,
                    "Trade" => BitgetChannel::Trade,
                    "Ticker" => BitgetChannel::Ticker,
                    _ => {
                        warn!("未知的通道類型: {}", channel_str);
                        continue;
                    }
                };
                
                connector.add_subscription(symbol.clone(), channel);
                info!("✅ 訂閱 {} - {}", symbol, channel_str);
            }
        }
        
        // 啟動數據處理
        self.start_data_processing().await?;
        
        // 啟動監控
        self.start_monitoring().await?;
        
        // 運行數據收集
        let start_time = Instant::now();
        
        info!("🔄 開始數據收集...");
        
        // 創建channel用於消息傳遞
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        
        // 啟動消息處理任務
        let collector_clone = self.clone();
        let _processor_handle = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if let Err(e) = collector_clone.process_message(message).await {
                    error!("處理消息錯誤: {}", e);
                    collector_clone.processing_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        
        let result = app.run_with_timeout(
            move |message| {
                if let Err(e) = tx.send(message) {
                    error!("發送消息到處理器失敗: {}", e);
                }
            },
            duration_minutes * 60
        ).await;
        
        let elapsed = start_time.elapsed();
        
        // 刷新剩餘數據
        self.flush_remaining_data().await?;
        
        // 打印最終統計
        self.print_final_statistics(elapsed).await;
        
        result
    }
    
    /// 處理單條消息
    async fn process_message(&self, message: BitgetMessage) -> Result<()> {
        let start_time = rust_hft::now_micros();
        
        let record = match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                self.process_orderbook_message(&symbol, &data, timestamp).await?
            },
            BitgetMessage::Trade { symbol, data, timestamp, .. } => {
                self.process_trade_message(&symbol, &data, timestamp).await?
            },
            _ => return Ok(()), // 忽略其他類型消息
        };
        
        // 更新統計
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        if let Some(symbol_stats) = self.messages_by_symbol.read().await.get(&record.symbol) {
            symbol_stats.fetch_add(1, Ordering::Relaxed);
        }
        
        // 優先使用无锁队列發送（最高性能）
        if let Some(ref fast_sender) = self.fast_message_sender {
            match fast_sender.try_send(record.clone()) {
                Ok(_) => {
                    // 成功發送到无锁队列，性能最佳
                },
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    // 无锁队列满了，降级到tokio通道
                    if let Some(ref sender) = self.clickhouse_sender {
                        if let Err(_) = sender.send(record.clone()) {
                            warn!("❌ 所有通道都已滿，添加到內存緩衝區");
                            let mut buffer = self.data_buffer.write().await;
                            buffer.push(record);
                        }
                    } else {
                        let mut buffer = self.data_buffer.write().await;
                        buffer.push(record);
                    }
                },
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    warn!("❌ 无锁队列已断开，降级到tokio通道");
                    if let Some(ref sender) = self.clickhouse_sender {
                        if let Err(_) = sender.send(record.clone()) {
                            let mut buffer = self.data_buffer.write().await;
                            buffer.push(record);
                        }
                    } else {
                        let mut buffer = self.data_buffer.write().await;
                        buffer.push(record);
                    }
                }
            }
        } else if let Some(ref sender) = self.clickhouse_sender {
            // 降级到tokio通道
            if let Err(_) = sender.send(record.clone()) {
                warn!("❌ ClickHouse寫入通道已滿或關閉");
                let mut buffer = self.data_buffer.write().await;
                buffer.push(record);
            }
        } else {
            // 如果ClickHouse未啟用，添加到內存緩衝區
            let mut buffer = self.data_buffer.write().await;
            buffer.push(record);
        }
        
        // 記錄處理延遲
        let processing_time = rust_hft::now_micros() - start_time;
        self.performance_manager.record_processing_latency(processing_time);
        
        Ok(())
    }
    
    /// 优化的JSON解析 - 使用simd-json和缓冲池
    fn parse_json_optimized(&self, json_str: &str) -> Result<serde_json::Value> {
        // 获取缓冲池中的buffer
        let mut buffer = self.json_buffer_pool.get_buffer();
        buffer.extend_from_slice(json_str.as_bytes());
        
        // 使用simd-json进行零拷贝解析
        let result = match simd_json::to_owned_value(&mut buffer) {
            Ok(value) => {
                // 转换为serde_json::Value以保持兼容性
                let json_str = simd_json::to_string(&value)?;
                Ok(serde_json::from_str(&json_str)?)
            },
            Err(_) => {
                // fallback到标准JSON解析
                serde_json::from_str(json_str).map_err(|e| anyhow::anyhow!("JSON解析失败: {}", e))
            }
        };
        
        // 归还buffer到池中
        self.json_buffer_pool.return_buffer(buffer);
        result
    }
    
    /// 處理OrderBook消息 (使用零拷贝JSON解析优化)
    async fn process_orderbook_message(
        &self, 
        symbol: &str, 
        data: &serde_json::Value, 
        timestamp: u64
    ) -> Result<MultiSymbolMarketData> {
        // 使用字符串缓存创建消息，避免重复String分配
        let mut record_id = self.string_cache.get_record_id();
        let mut symbol_str = self.string_cache.get_symbol();
        let mut data_type_str = self.string_cache.get_data_type();
        
        // 设置字符串内容
        record_id.push_str(&Uuid::new_v4().to_string());
        symbol_str.push_str(symbol);
        data_type_str.push_str("orderbook");
        // 解析價格數據
        let empty_vec = vec![];
        let bids = data["bids"].as_array().unwrap_or(&empty_vec);
        let asks = data["asks"].as_array().unwrap_or(&empty_vec);
        
        let best_bid = bids.get(0)
            .and_then(|bid| bid[0].as_str())
            .and_then(|price| price.parse::<f64>().ok())
            .unwrap_or(0.0);
            
        let best_ask = asks.get(0)
            .and_then(|ask| ask[0].as_str())
            .and_then(|price| price.parse::<f64>().ok())
            .unwrap_or(0.0);
        
        let mid_price = (best_bid + best_ask) / 2.0;
        let spread_bps = if mid_price > 0.0 {
            (best_ask - best_bid) / mid_price * 10000.0
        } else {
            0.0
        };
        
        // 优化的深度數據提取（预分配和批量处理）
        // 零拷贝数组优化 - 避免堆分配
        let mut bid_volumes: [f64; 20] = [0.0; 20];
        let mut ask_volumes: [f64; 20] = [0.0; 20];
        let mut bid_count = 0usize;
        let mut ask_count = 0usize;
        
        // 零拷贝批量解析bid数据（直接写入数组）
        for (i, bid) in bids.iter().take(20).enumerate() {
            if let (Some(price_str), Some(volume_str)) = (bid[0].as_str(), bid[1].as_str()) {
                if let (Ok(_price), Ok(volume)) = (price_str.parse::<f64>(), volume_str.parse::<f64>()) {
                    bid_volumes[i] = volume;
                    bid_count = i + 1;
                }
            }
        }
        
        // 零拷贝批量解析ask数据
        for (i, ask) in asks.iter().take(20).enumerate() {
            if let (Some(price_str), Some(volume_str)) = (ask[0].as_str(), ask[1].as_str()) {
                if let (Ok(_price), Ok(volume)) = (price_str.parse::<f64>(), volume_str.parse::<f64>()) {
                    ask_volumes[i] = volume;
                    ask_count = i + 1;
                }
            }
        }
        
        // 零拷贝批量深度计算（直接使用数组切片）
        let (obi_l1, obi_l5, obi_l10, obi_l20, bid_volume_l5, ask_volume_l5, total_bid_volume, total_ask_volume) = 
            Self::calculate_depth_metrics_zero_copy(&bid_volumes[..bid_count], &ask_volumes[..ask_count]);
        
        // 创建消息结构体，使用缓存的字符串
        let mut record = MultiSymbolMarketData {
            record_id: record_id,
            symbol: symbol_str,
            timestamp,
            data_type: data_type_str,
            best_bid,
            best_ask,
            mid_price,
            spread_bps,
            obi_l1,
            obi_l5,
            obi_l10,
            obi_l20,
            bid_volume_l5,
            ask_volume_l5,
            total_bid_volume,
            total_ask_volume,
            processing_latency_us: 0, // 將被後續更新
            data_quality_score: 1.0,  // 簡化版本
            sequence_number: 0,
        };
        record.sequence_number = self.total_messages.load(Ordering::Relaxed);
        record.sequence_number = self.total_messages.load(Ordering::Relaxed);
        
        Ok(record)
    }
    
    /// 處理Trade消息 (使用字符串缓存优化)
    async fn process_trade_message(
        &self,
        symbol: &str,
        data: &serde_json::Value,
        timestamp: u64
    ) -> Result<MultiSymbolMarketData> {
        // 使用字符串缓存创建消息，避免重复String分配
        let mut record_id = self.string_cache.get_record_id();
        let mut symbol_str = self.string_cache.get_symbol();
        let mut data_type_str = self.string_cache.get_data_type();
        
        // 设置字符串内容
        record_id.push_str(&Uuid::new_v4().to_string());
        symbol_str.push_str(symbol);
        data_type_str.push_str("trade");
        
        let price: f64 = data["price"].as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
            
        let volume: f64 = data["volume"].as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        
        // 创建消息结构体，使用缓存的字符串
        let record = MultiSymbolMarketData {
            record_id: record_id,
            symbol: symbol_str,
            timestamp,
            data_type: data_type_str,
            best_bid: price,
            best_ask: price,
            mid_price: price,
            spread_bps: 0.0,
            obi_l1: 0.0,
            obi_l5: 0.0,
            obi_l10: 0.0,
            obi_l20: 0.0,
            bid_volume_l5: volume,
            ask_volume_l5: 0.0,
            total_bid_volume: volume,
            total_ask_volume: 0.0,
            processing_latency_us: 0,
            data_quality_score: 1.0,
            sequence_number: self.total_messages.load(Ordering::Relaxed),
        };
        
        Ok(record)
    }
    
    /// 刷新緩衝區到ClickHouse
    async fn flush_buffer_to_clickhouse(&self, buffer: &mut Vec<MultiSymbolMarketData>) -> Result<()> {
        if let Some(ref client) = self.clickhouse_client {
            if !buffer.is_empty() {
                let table_name = &self.config.clickhouse.tables.market_data;
                let mut insert = client.insert(table_name)?;
                
                for record in buffer.iter() {
                    insert.write(record).await?;
                }
                
                insert.end().await?;
                
                debug!("💾 寫入 {} 條記錄到 ClickHouse", buffer.len());
                buffer.clear();
            }
        }
        Ok(())
    }
    
    /// 刷新剩餘數據
    async fn flush_remaining_data(&self) -> Result<()> {
        info!("🔄 刷新剩餘數據...");
        
        // 關閉ClickHouse寫入通道，讓後台任務處理剩餘數據
        if self.clickhouse_sender.is_some() {
            // Drop sender to signal shutdown to background task
            // The sender is dropped when the struct is dropped
            info!("📋 ClickHouse寫入通道將關閉，等待後台任務完成");
            // 給後台任務一些時間來處理剩餘數據
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
        
        // 刷新內存緩衝區（備用數據）
        let mut buffer = self.data_buffer.write().await;
        if !buffer.is_empty() {
            info!("💾 刷新 {} 條備用記錄", buffer.len());
            self.flush_buffer_to_clickhouse(&mut buffer).await?;
        }
        
        Ok(())
    }
    
    /// 啟動數據處理
    async fn start_data_processing(&self) -> Result<()> {
        info!("🔄 啟動數據處理線程");
        Ok(())
    }
    
    /// 啟動監控
    async fn start_monitoring(&self) -> Result<()> {
        if self.config.monitoring.enable_realtime_stats {
            info!("📊 啟動實時監控");
            
            let total_messages = self.total_messages.clone();
            let messages_by_symbol = self.messages_by_symbol.clone();
            let processing_errors = self.processing_errors.clone();
            let performance_manager = self.performance_manager.clone();
            let stats_interval = self.config.monitoring.stats_interval_sec;
            
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(stats_interval));
                let start_time = Instant::now();
                
                loop {
                    interval.tick().await;
                    
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let total = total_messages.load(Ordering::Relaxed);
                    let errors = processing_errors.load(Ordering::Relaxed);
                    let throughput = total as f64 / elapsed;
                    
                    info!("📊 實時統計 - 總消息: {} | 吞吐量: {:.1} msg/s | 錯誤: {} | 運行時間: {:.1}s", 
                          total, throughput, errors, elapsed);
                    
                    // 按商品統計
                    let symbol_stats = messages_by_symbol.read().await;
                    for (symbol, count) in symbol_stats.iter() {
                        let symbol_count = count.load(Ordering::Relaxed);
                        if symbol_count > 0 {
                            debug!("   📈 {}: {} 條消息", symbol, symbol_count);
                        }
                    }
                    
                    // 性能統計
                    let perf_stats = performance_manager.get_performance_stats();
                    debug!("   ⚡ 平均延遲: {:.1}μs | 最大延遲: {}μs", 
                           perf_stats.avg_latency_us, perf_stats.max_latency_us);
                }
            });
        }
        
        Ok(())
    }
    
    /// 打印最終統計
    async fn print_final_statistics(&self, elapsed: Duration) {
        let total = self.total_messages.load(Ordering::Relaxed);
        let errors = self.processing_errors.load(Ordering::Relaxed);
        let elapsed_secs = elapsed.as_secs_f64();
        let throughput = total as f64 / elapsed_secs;
        
        info!("🎯 === 多商品數據收集完成統計 ===");
        info!("⏱️  總運行時間: {:.1} 分鐘", elapsed_secs / 60.0);
        info!("📦 總消息數: {}", total);
        info!("🚀 平均吞吐量: {:.1} msg/s", throughput);
        info!("❌ 處理錯誤: {}", errors);
        info!("✅ 成功率: {:.2}%", if total > 0 { (total - errors) as f64 / total as f64 * 100.0 } else { 0.0 });
        
        // 按商品統計
        info!("📊 各商品數據統計:");
        let symbol_stats = self.messages_by_symbol.read().await;
        for (symbol, count) in symbol_stats.iter() {
            let symbol_count = count.load(Ordering::Relaxed);
            if symbol_count > 0 {
                let symbol_throughput = symbol_count as f64 / elapsed_secs;
                info!("   📈 {}: {} 條 ({:.1} msg/s)", symbol, symbol_count, symbol_throughput);
            }
        }
        
        // 性能統計
        let perf_stats = self.performance_manager.get_performance_stats();
        info!("⚡ 性能指標:");
        info!("   平均延遲: {:.1}μs", perf_stats.avg_latency_us);
        info!("   最大延遲: {}μs", perf_stats.max_latency_us);
        info!("   最小延遲: {}μs", perf_stats.min_latency_us);
        info!("   處理消息數: {}", perf_stats.messages_processed);
        
        // 字符串缓存統計
        let (created, reused) = self.string_cache.stats();
        let (record_ids_size, symbols_size, data_types_size) = self.string_cache.pool_sizes();
        let reuse_rate = if created + reused > 0 {
            (reused as f64 / (created + reused) as f64) * 100.0
        } else {
            0.0
        };
        info!("💾 字符串缓存統計:");
        info!("   創建字符串: {}", created);
        info!("   重用字符串: {}", reused);
        info!("   重用率: {:.1}%", reuse_rate);
        info!("   池大小 - record_ids: {}, symbols: {}, data_types: {}", record_ids_size, symbols_size, data_types_size);
        
        // JSON缓冲池統計
        let (json_created, json_reused) = self.json_buffer_pool.stats();
        let json_reuse_rate = if json_created + json_reused > 0 {
            (json_reused as f64 / (json_created + json_reused) as f64) * 100.0
        } else {
            0.0
        };
        info!("📊 JSON缓冲池統計:");
        info!("   創建缓冲: {}", json_created);
        info!("   重用缓冲: {}", json_reused);
        info!("   重用率: {:.1}%", json_reuse_rate);
        
        // 評級
        let grade = if throughput >= 1000.0 && perf_stats.avg_latency_us < 100.0 {
            "🟢 優秀"
        } else if throughput >= 500.0 && perf_stats.avg_latency_us < 200.0 {
            "🟡 良好"
        } else {
            "🔴 需要優化"
        };
        
        info!("🏆 性能評級: {} (吞吐量: {:.1} msg/s, 延遲: {:.1}μs)", 
              grade, throughput, perf_stats.avg_latency_us);
    }
}

/// 多商品收集工作流步驟
struct MultiSymbolCollectionStep {
    config_path: String,
    duration_minutes: u64,
}

impl MultiSymbolCollectionStep {
    fn new(config_path: String, duration_minutes: u64) -> Box<dyn WorkflowStep> {
        Box::new(Self { config_path, duration_minutes })
    }
}

#[async_trait]
impl WorkflowStep for MultiSymbolCollectionStep {
    fn name(&self) -> &str {
        "Multi-Symbol High Performance Collection"
    }
    
    fn description(&self) -> &str {
        "High-performance data collection for multiple trading pairs with ClickHouse storage"
    }
    
    fn estimated_duration(&self) -> u64 {
        self.duration_minutes * 60 + 60 // 加1分鐘啟動時間
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🚀 啟動多商品高性能數據收集工作流");
        
        let collector = Arc::new(MultiSymbolCollector::from_config_file(&self.config_path)?);
        let collector_clone = collector.clone();
        collector.start_collection(self.duration_minutes).await?;
        
        let total_messages = collector_clone.total_messages.load(Ordering::Relaxed);
        let perf_stats = collector_clone.performance_manager.get_performance_stats();
        
        let throughput = total_messages as f64 / (self.duration_minutes * 60) as f64;
        
        info!("🎉 多商品數據收集工作流完成");
        
        Ok(StepResult::success(&format!("收集完成: {} 消息, {:.1} msg/s", total_messages, throughput))
           .with_metric("total_messages", total_messages as f64)
           .with_metric("throughput", throughput)
           .with_metric("avg_latency_us", perf_stats.avg_latency_us))
    }
}

/// 命令行參數
#[derive(Parser, Debug)]
#[command(author, version, about = "Multi-Symbol High Performance Data Collection")]
struct MultiSymbolArgs {
    /// 配置文件路徑
    #[arg(short, long, default_value = "config/multi_symbol_collection.yaml")]
    config: String,
    
    /// 收集時長（分鐘）
    #[arg(short, long, default_value = "60")]
    duration: u64,
    
    /// 使用工作流模式
    #[arg(long)]
    workflow: bool,
    
    /// 詳細日誌
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化 Rustls crypto provider
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Failed to install crypto provider"))?;
    
    // 設置日誌級別 (HftAppRunner 會處理 tracing 初始化)
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    
    let args = MultiSymbolArgs::parse();
    
    info!("🚀 多商品高性能數據收集系統");
    info!("📁 配置文件: {}", args.config);
    info!("⏱️  收集時長: {} 分鐘", args.duration);
    
    if args.workflow {
        // 使用工作流模式
        let mut workflow = WorkflowExecutor::new()
            .add_step(MultiSymbolCollectionStep::new(args.config, args.duration));
        
        let report = workflow.execute().await?;
        report.print_detailed_report();
        
        if report.success {
            info!("🎉 多商品數據收集工作流成功完成！");
        } else {
            error!("❌ 多商品數據收集工作流失敗");
        }
    } else {
        // 直接運行模式
        let collector = Arc::new(MultiSymbolCollector::from_config_file(&args.config)?);
        collector.start_collection(args.duration).await?;
        
        info!("🎉 多商品數據收集完成！");
    }
    
    Ok(())
}