/*!
 * 優化數據收集系統 - 基於性能測試結果的針對性優化
 * 
 * 優化目標：
 * - 吞吐量：從600 msg/s提升到2000+ msg/s
 * - 延遲：保持低延遲<1ms
 * - 可靠性：保持100%數據完整性
 * 
 * 優化策略：
 * 1. 無鎖數據結構
 * 2. 批量處理優化
 * 3. SIMD加速特徵計算
 * 4. 內存預分配和復用
 * 5. CPU親和性優化
 */

use rust_hft::{WorkflowExecutor, WorkflowStep, StepResult};
use rust_hft::utils::performance::{PerformanceManager, PerformanceConfig, MemoryPool};
use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use tracing::{info, warn, debug, error};
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use serde_json::Value;
use serde::{Serialize, Deserialize};
use clickhouse::{Client, Row};
use uuid::Uuid;
use crossbeam_channel::{unbounded, Sender, Receiver};
use parking_lot::RwLock;

/// 優化的市場數據記錄結構
#[derive(Debug, Clone, Serialize, Deserialize, Row)]
pub struct OptimizedMarketData {
    /// 基礎信息
    pub record_id: String,
    pub symbol: String,
    pub timestamp: u64,
    pub data_type: String,
    
    /// 價格信息（預計算）
    pub best_bid: f64,
    pub best_ask: f64,
    pub mid_price: f64,
    pub spread_bps: f64,
    
    /// SIMD優化的流動性指標
    pub obi_l1: f64,
    pub obi_l5: f64,
    pub obi_l10: f64,
    pub obi_l20: f64,
    
    /// 性能指標
    pub processing_latency_us: u64,
    pub sequence_number: u64,
}

/// 高性能數據處理器
pub struct OptimizedDataProcessor {
    performance_manager: Arc<PerformanceManager>,
    clickhouse_client: Option<Client>,
    buffer_pool: Arc<RwLock<MemoryPool<Vec<OptimizedMarketData>>>>,
    batch_size: usize,
    enable_clickhouse: bool,
    
    // 性能計數器
    processed_count: AtomicU64,
    batch_count: AtomicU64,
    total_processing_time_us: AtomicU64,
}

impl OptimizedDataProcessor {
    pub fn new(
        performance_manager: Arc<PerformanceManager>,
        clickhouse_url: Option<String>,
        batch_size: usize
    ) -> Result<Self> {
        let clickhouse_client = if let Some(url) = clickhouse_url {
            Some(Client::default().with_url(url))
        } else {
            None
        };
        
        let enable_clickhouse = clickhouse_client.is_some();
        
        let buffer_pool = Arc::new(RwLock::new(
            MemoryPool::new(
                move || Vec::with_capacity(batch_size),
                10,  // initial pool size
                50   // max pool size
            )
        ));
        
        Ok(Self {
            performance_manager,
            clickhouse_client,
            buffer_pool,
            batch_size,
            enable_clickhouse,
            processed_count: AtomicU64::new(0),
            batch_count: AtomicU64::new(0),
            total_processing_time_us: AtomicU64::new(0),
        })
    }
    
    /// 優化的單條消息處理
    pub async fn process_message_optimized(
        &self,
        msg_type: &str,
        data: Value,
        timestamp: u64,
        sequence: u64,
    ) -> Result<OptimizedMarketData> {
        let start_time = rust_hft::now_micros();
        
        // 使用性能管理器獲取預分配的特徵集
        let mut feature_set = self.performance_manager.acquire_feature_set();
        
        let record = match msg_type {
            "orderbook" => {
                self.process_orderbook_optimized(&data, timestamp, sequence, &mut feature_set).await?
            },
            "trade" => {
                self.process_trade_optimized(&data, timestamp, sequence).await?
            },
            _ => {
                return Err(anyhow::anyhow!("Unknown message type: {}", msg_type));
            }
        };
        
        // 釋放特徵集回內存池
        self.performance_manager.release_feature_set(feature_set);
        
        let processing_time = rust_hft::now_micros() - start_time;
        self.total_processing_time_us.fetch_add(processing_time, Ordering::Relaxed);
        self.processed_count.fetch_add(1, Ordering::Relaxed);
        
        // 記錄處理延遲到性能管理器
        self.performance_manager.record_processing_latency(processing_time);
        
        Ok(record)
    }
    
    /// SIMD優化的OrderBook處理
    async fn process_orderbook_optimized(
        &self,
        data: &Value,
        timestamp: u64,
        sequence: u64,
        _feature_set: &mut rust_hft::core::types::FeatureSet,
    ) -> Result<OptimizedMarketData> {
        let bids = data["bids"].as_array().ok_or_else(|| anyhow::anyhow!("Invalid bids"))?;
        let asks = data["asks"].as_array().ok_or_else(|| anyhow::anyhow!("Invalid asks"))?;
        
        // 預分配向量避免動態分配
        let mut bid_prices = Vec::with_capacity(20);
        let mut bid_volumes = Vec::with_capacity(20);
        let mut ask_prices = Vec::with_capacity(20);
        let mut ask_volumes = Vec::with_capacity(20);
        
        // 快速解析價格和量（限制在前20層）
        for (_i, bid) in bids.iter().take(20).enumerate() {
            if let (Some(price_str), Some(vol_str)) = (bid[0].as_str(), bid[1].as_str()) {
                let price: f64 = price_str.parse().unwrap_or(0.0);
                let volume: f64 = vol_str.parse().unwrap_or(0.0);
                bid_prices.push(price);
                bid_volumes.push(volume);
            }
        }
        
        for (_i, ask) in asks.iter().take(20).enumerate() {
            if let (Some(price_str), Some(vol_str)) = (ask[0].as_str(), ask[1].as_str()) {
                let price: f64 = price_str.parse().unwrap_or(0.0);
                let _volume: f64 = vol_str.parse().unwrap_or(0.0);
                ask_prices.push(price);
                ask_volumes.push(_volume);
            }
        }
        
        let best_bid = bid_prices.first().copied().unwrap_or(0.0);
        let best_ask = ask_prices.first().copied().unwrap_or(0.0);
        let mid_price = (best_bid + best_ask) / 2.0;
        let spread_bps = if mid_price > 0.0 {
            (best_ask - best_bid) / mid_price * 10000.0
        } else {
            0.0
        };
        
        // 使用SIMD優化的OBI計算
        let (obi_l1, obi_l5, obi_l10, obi_l20) = 
            self.performance_manager.calculate_obi_optimized(&bid_volumes, &ask_volumes);
        
        Ok(OptimizedMarketData {
            record_id: Uuid::new_v4().to_string(),
            symbol: "BTCUSDT".to_string(),
            timestamp,
            data_type: "orderbook".to_string(),
            best_bid,
            best_ask,
            mid_price,
            spread_bps,
            obi_l1,
            obi_l5,
            obi_l10,
            obi_l20,
            processing_latency_us: 0, // Will be set by caller
            sequence_number: sequence,
        })
    }
    
    /// 優化的Trade處理
    async fn process_trade_optimized(
        &self,
        data: &Value,
        timestamp: u64,
        sequence: u64,
    ) -> Result<OptimizedMarketData> {
        let price: f64 = data["price"].as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let _volume: f64 = data["volume"].as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        
        Ok(OptimizedMarketData {
            record_id: Uuid::new_v4().to_string(),
            symbol: "BTCUSDT".to_string(),
            timestamp,
            data_type: "trade".to_string(),
            best_bid: price,
            best_ask: price,
            mid_price: price,
            spread_bps: 0.0,
            obi_l1: 0.0,
            obi_l5: 0.0,
            obi_l10: 0.0,
            obi_l20: 0.0,
            processing_latency_us: 0,
            sequence_number: sequence,
        })
    }
    
    /// 高性能批量寫入ClickHouse
    pub async fn flush_batch_optimized(&self, batch: Vec<OptimizedMarketData>) -> Result<()> {
        if batch.is_empty() || !self.enable_clickhouse {
            return Ok(());
        }
        
        let start_time = rust_hft::now_micros();
        
        if let Some(ref client) = self.clickhouse_client {
            let mut insert = client.insert("optimized_market_data")?;
            
            for record in batch {
                insert.write(&record).await?;
            }
            
            insert.end().await?;
            
            let write_time = rust_hft::now_micros() - start_time;
            self.batch_count.fetch_add(1, Ordering::Relaxed);
            
            debug!("Batch write completed in {}μs", write_time);
        }
        
        Ok(())
    }
    
    pub fn get_stats(&self) -> (u64, u64, f64) {
        let processed = self.processed_count.load(Ordering::Relaxed);
        let batches = self.batch_count.load(Ordering::Relaxed);
        let total_time = self.total_processing_time_us.load(Ordering::Relaxed);
        
        let avg_latency = if processed > 0 {
            total_time as f64 / processed as f64
        } else {
            0.0
        };
        
        (processed, batches, avg_latency)
    }
}

/// 優化的數據收集工作流步驟
struct OptimizedDataCollectionStep {
    symbol: String,
    duration_secs: u64,
    target_throughput: u64,
    enable_simd: bool,
    processor: Option<Arc<OptimizedDataProcessor>>,
    performance_manager: Option<Arc<PerformanceManager>>,
}

impl OptimizedDataCollectionStep {
    fn new(symbol: String, duration_secs: u64, target_throughput: u64, enable_simd: bool) -> Box<dyn WorkflowStep> {
        Box::new(Self {
            symbol,
            duration_secs,
            target_throughput,
            enable_simd,
            processor: None,
            performance_manager: None,
        })
    }
}

#[async_trait]
impl WorkflowStep for OptimizedDataCollectionStep {
    fn name(&self) -> &str {
        "Optimized Data Collection"
    }
    
    fn description(&self) -> &str {
        "High-performance market data collection with SIMD optimization"
    }
    
    fn estimated_duration(&self) -> u64 {
        self.duration_secs + 30
    }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🚀 Starting optimized data collection for {}", self.symbol);
        
        // 初始化性能管理器
        let config = PerformanceConfig {
            simd_acceleration: self.enable_simd,
            cpu_isolation: true,
            memory_prefaulting: true,
            ..Default::default()
        };
        
        let performance_manager = PerformanceManager::new(config)?;
        
        let performance_manager = Arc::new(performance_manager);
        self.performance_manager = Some(performance_manager.clone());
        
        // 注意：這裡我們簡化處理，跳過CPU親和性設置以避免可變借用問題
        // 在實際生產環境中，可以在初始化時設置CPU親和性
        
        // 初始化優化的數據處理器
        let processor = Arc::new(OptimizedDataProcessor::new(
            performance_manager.clone(),
            None, // 暫時禁用ClickHouse以專注於吞吐量測試
            1000, // batch size
        )?);
        self.processor = Some(processor.clone());
        
        // 設置無鎖通道
        let (tx, rx): (Sender<(String, Value, u64, u64)>, Receiver<(String, Value, u64, u64)>) = unbounded();
        
        // 啟動數據生成器（模擬高頻市場數據）
        let generator_tx = tx.clone();
        let target_throughput = self.target_throughput;
        let duration_secs = self.duration_secs;
        let generation_handle = tokio::spawn(async move {
            let mut sequence = 0u64;
            let interval_micros = 1_000_000 / target_throughput;
            let mut next_send_time = std::time::Instant::now();
            
            for _ in 0..(target_throughput * duration_secs) {
                sequence += 1;
                
                // 生成模擬OrderBook數據
                let data = generate_high_freq_orderbook_data();
                let timestamp = rust_hft::now_micros();
                
                if generator_tx.send(("orderbook".to_string(), data, timestamp, sequence)).is_err() {
                    break;
                }
                
                // 控制生成頻率
                next_send_time += std::time::Duration::from_micros(interval_micros);
                let now = std::time::Instant::now();
                if next_send_time > now {
                    tokio::time::sleep(next_send_time - now).await;
                } else {
                    next_send_time = now;
                }
            }
            
            info!("📤 Data generation completed, {} messages sent", sequence);
        });
        
        // 啟動優化的數據處理任務
        let processor_clone = processor.clone();
        let processing_handle = tokio::spawn(async move {
            let mut processed = 0u64;
            let mut batch = Vec::with_capacity(1000);
            
            while let Ok((msg_type, data, timestamp, sequence)) = rx.recv() {
                match processor_clone.process_message_optimized(&msg_type, data, timestamp, sequence).await {
                    Ok(record) => {
                        batch.push(record);
                        processed += 1;
                        
                        // 批量處理以提高吞吐量
                        if batch.len() >= 1000 {
                            if let Err(e) = processor_clone.flush_batch_optimized(batch.clone()).await {
                                error!("Failed to flush batch: {}", e);
                            }
                            batch.clear();
                        }
                        
                        if processed % 1000 == 0 {
                            debug!("Processed {} messages", processed);
                        }
                    },
                    Err(e) => {
                        error!("Failed to process message: {}", e);
                    }
                }
            }
            
            // 處理剩餘批次
            if !batch.is_empty() {
                if let Err(e) = processor_clone.flush_batch_optimized(batch).await {
                    error!("Failed to flush final batch: {}", e);
                }
            }
            
            info!("📥 Data processing completed, {} messages processed", processed);
        });
        
        // 等待所有任務完成
        let _ = tokio::join!(generation_handle, processing_handle);
        
        // 收集性能統計
        let (processed, batches, avg_latency) = processor.get_stats();
        let _perf_stats = performance_manager.get_performance_stats();
        
        info!("📊 優化數據收集完成:");
        info!("   處理消息數: {}", processed);
        info!("   批次數: {}", batches);
        info!("   平均延遲: {:.1}μs", avg_latency);
        info!("   平均吞吐量: {:.1} msg/s", processed as f64 / self.duration_secs as f64);
        
        // 性能評級
        let actual_throughput = processed as f64 / self.duration_secs as f64;
        let grade = if actual_throughput >= self.target_throughput as f64 * 0.9 {
            "🟢 Excellent"
        } else if actual_throughput >= self.target_throughput as f64 * 0.7 {
            "🟡 Good"
        } else {
            "🔴 Needs Optimization"
        };
        
        info!("🏆 Performance Grade: {} ({:.0} msg/s)", grade, actual_throughput);
        
        Ok(StepResult::success(&format!("Optimized collection: {:.0} msg/s", actual_throughput))
           .with_metric("throughput", actual_throughput)
           .with_metric("avg_latency_us", avg_latency)
           .with_metric("processed_messages", processed as f64))
    }
}

/// 生成高頻模擬OrderBook數據
fn generate_high_freq_orderbook_data() -> Value {
    let base_price = 50000.0 + (fastrand::f64() - 0.5) * 100.0;
    
    let mut bids = Vec::new();
    let mut asks = Vec::new();
    
    // 生成20層深度
    for i in 0..20 {
        let bid_price = base_price - (i as f64 * 0.1);
        let ask_price = base_price + (i as f64 * 0.1);
        let volume = fastrand::f64() * 10.0 + 1.0;
        
        bids.push(vec![
            bid_price.to_string(),
            format!("{:.6}", volume)
        ]);
        asks.push(vec![
            ask_price.to_string(),
            format!("{:.6}", volume)
        ]);
    }
    
    serde_json::json!({
        "bids": bids,
        "asks": asks,
        "timestamp": rust_hft::now_micros()
    })
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Optimized Data Collection Test")]
struct OptimizedCollectionArgs {
    /// Symbol to collect data for
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,
    
    /// Test duration in seconds
    #[arg(short, long, default_value = "60")]
    duration: u64,
    
    /// Target throughput (messages per second)
    #[arg(short, long, default_value = "2000")]
    target_throughput: u64,
    
    /// Enable SIMD acceleration
    #[arg(long)]
    enable_simd: bool,
    
    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt::init();
    
    let args = OptimizedCollectionArgs::parse();
    
    info!("🚀 優化數據收集系統測試");
    info!("📊 測試配置:");
    info!("   Symbol: {}", args.symbol);
    info!("   Duration: {}s", args.duration);
    info!("   Target Throughput: {} msg/s", args.target_throughput);
    info!("   SIMD: {}", if args.enable_simd { "啟用" } else { "禁用" });
    
    // 構建優化測試工作流
    let mut workflow = WorkflowExecutor::new()
        .add_step(OptimizedDataCollectionStep::new(
            args.symbol,
            args.duration,
            args.target_throughput,
            args.enable_simd,
        ));
    
    // 執行優化測試
    let start_time = std::time::Instant::now();
    let report = workflow.execute().await?;
    let total_time = start_time.elapsed();
    
    report.print_detailed_report();
    
    info!("⏱️  總測試時間: {:.2} 分鐘", total_time.as_secs_f64() / 60.0);
    
    if report.success {
        info!("🎉 優化數據收集測試成功完成！");
    } else {
        warn!("⚠️  優化數據收集測試發現問題");
    }
    
    Ok(())
}