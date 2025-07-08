/*!
 * 真實端到端延遲測量系統
 * 
 * 這個範例展示如何測量從交易所到應用程序的真實延遲，包括：
 * - 與交易所時間服務器的 NTP 同步
 * - 網絡層延遲測量 (ping, traceroute)
 * - WebSocket 數據包到達時間戳
 * - 多層延遲分解分析
 * - 實時延遲監控和異常檢測
 * 
 * 執行方式：
 * cargo run --example real_latency_measurement -- --symbol BTCUSDT --enable-ntp-sync
 */

use rust_hft::{
    integrations::bitget_connector::*,
    core::types::*,
    utils::{
        precise_timing::*,
        network_latency::*,
    },
};
use anyhow::Result;
use tracing::{info, warn, error, debug};
use std::sync::Arc;
use std::time::Duration;
use clap::Parser;
use serde_json::Value;
use tokio::time::interval;

#[derive(Parser)]
#[command(name = "real_latency_measurement")]
#[command(about = "Real End-to-End Latency Measurement System")]
struct Args {
    #[arg(short, long, default_value_t = String::from("BTCUSDT"))]
    symbol: String,
    
    #[arg(long, default_value_t = false)]
    enable_ntp_sync: bool,
    
    #[arg(long, default_value_t = false)]
    enable_network_monitoring: bool,
    
    #[arg(long, default_value_t = 60)]
    report_interval_sec: u64,
    
    #[arg(long, default_value_t = 10000)]
    high_latency_threshold_us: u64,
}

/// 真實延遲測量主控器
pub struct RealLatencyMeasurementSystem {
    args: Args,
    time_sync: Arc<PreciseTimeSync>,
    latency_measurer: Arc<RealLatencyMeasurer>,
    
    /// 統計數據
    total_messages: Arc<std::sync::atomic::AtomicU64>,
    high_latency_messages: Arc<std::sync::atomic::AtomicU64>,
    
    /// 延遲記錄
    recent_measurements: Arc<parking_lot::RwLock<Vec<LatencyMeasurement>>>,
}

impl RealLatencyMeasurementSystem {
    /// 創建新的真實延遲測量系統
    pub async fn new(args: Args) -> Result<Self> {
        info!("🚀 初始化真實端到端延遲測量系統");
        
        // 配置時間同步
        let time_sync_config = TimeSyncConfig {
            ntp_servers: vec![
                "time.google.com".to_string(),
                "time.cloudflare.com".to_string(),
                "pool.ntp.org".to_string(),
            ],
            sync_interval_sec: 300,
            max_drift_us: 1000,
            timeout_ms: 5000,
            high_precision: true,
        };
        
        // 配置網絡監控
        let network_config = NetworkLatencyConfig {
            enable_kernel_timestamps: args.enable_network_monitoring,
            exchange_hosts: vec![
                "ws.bitget.com".to_string(),
                "api.bitget.com".to_string(),
            ],
            ping_interval_sec: 30,
            capture_buffer_size: 1024 * 1024,
            max_latency_records: 1000,
            anomaly_threshold_us: args.high_latency_threshold_us,
        };
        
        // 創建組件
        let time_sync = Arc::new(PreciseTimeSync::new(time_sync_config));
        let latency_measurer = Arc::new(RealLatencyMeasurer::new(
            time_sync.clone(),
            network_config,
        ));
        
        // 啟動時間同步 (如果啟用)
        if args.enable_ntp_sync {
            time_sync.start().await?;
            info!("✅ NTP 時間同步已啟動");
        } else {
            warn!("⚠️  NTP 同步已禁用，延遲測量可能不準確");
        }
        
        // 啟動網絡監控 (如果啟用)
        if args.enable_network_monitoring {
            latency_measurer.start().await?;
            info!("✅ 網絡延遲監控已啟動");
        }
        
        Ok(Self {
            args,
            time_sync,
            latency_measurer,
            total_messages: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            high_latency_messages: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            recent_measurements: Arc::new(parking_lot::RwLock::new(Vec::new())),
        })
    }
    
    /// 啟動延遲測量系統
    pub async fn run(&self) -> Result<()> {
        info!("🎯 啟動真實延遲測量系統");
        info!("配置: Symbol={}, NTP={}, Network={}", 
              self.args.symbol, self.args.enable_ntp_sync, self.args.enable_network_monitoring);
        
        // 啟動定期報告
        self.start_periodic_reporting().await;
        
        // 啟動延遲異常檢測
        self.start_anomaly_detection().await;
        
        // 配置 Bitget WebSocket
        let config = BitgetConfig {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 5,
        };
        
        let mut connector = BitgetConnector::new(config);
        connector.add_subscription(self.args.symbol.clone(), BitgetChannel::Books5);
        
        // 設置消息處理器
        let message_handler = self.create_message_handler();
        
        // 啟動 WebSocket 連接
        info!("🔌 連接到 Bitget WebSocket: {}", self.args.symbol);
        tokio::select! {
            result = connector.connect_public(message_handler) => {
                if let Err(e) = result {
                    error!("WebSocket 連接失敗: {}", e);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("📊 收到關閉信號，停止測量...");
                self.print_final_report().await;
            }
        }
        
        Ok(())
    }
    
    /// 創建消息處理器
    fn create_message_handler(&self) -> impl Fn(BitgetMessage) + Send + 'static {
        let time_sync = self.time_sync.clone();
        let latency_measurer = self.latency_measurer.clone();
        let total_messages = self.total_messages.clone();
        let high_latency_messages = self.high_latency_messages.clone();
        let recent_measurements = self.recent_measurements.clone();
        let high_latency_threshold = self.args.high_latency_threshold_us;
        
        move |message: BitgetMessage| {
            let processing_start = time_sync.now_synced_micros();
            
            match message {
                BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                    // 模擬處理延遲
                    let _ = Self::process_orderbook_data(&data);
                    let processing_end = time_sync.now_synced_micros();
                    
                    // 測量真實端到端延遲
                    let packet_source: std::net::SocketAddr = "185.199.109.153:443".parse().unwrap(); // Bitget IP
                    let packet_size = serde_json::to_string(&data)
                        .map(|s| s.len())
                        .unwrap_or(1024);
                    
                    match latency_measurer.measure_websocket_latency(
                        &timestamp.to_string(),
                        packet_source,
                        packet_size,
                        processing_start,
                        processing_end,
                    ) {
                        Ok(measurement) => {
                            total_messages.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            
                            let total_latency = measurement.latency_breakdown.total_latency_us;
                            
                            // 記錄測量結果
                            {
                                let mut measurements = recent_measurements.write();
                                if measurements.len() >= 1000 {
                                    measurements.remove(0);
                                }
                                measurements.push(measurement.clone());
                            }
                            
                            // 檢查高延遲
                            if total_latency > high_latency_threshold {
                                high_latency_messages.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                warn!("🚨 高延遲警告: {}μs - 網絡: {}μs ({}%), 處理: {}μs ({}%)",
                                      total_latency,
                                      measurement.latency_breakdown.network_latency_us,
                                      (measurement.latency_breakdown.network_ratio * 100.0) as u32,
                                      measurement.latency_breakdown.processing_latency_us,
                                      (measurement.latency_breakdown.processing_ratio * 100.0) as u32);
                            }
                            
                            // 詳細日誌 (每 100 條消息)
                            if total_messages.load(std::sync::atomic::Ordering::Relaxed) % 100 == 0 {
                                info!("💬 {} - 總延遲: {}μs, 網絡: {}μs, 內核: {}μs, 處理: {}μs",
                                      symbol,
                                      total_latency,
                                      measurement.latency_breakdown.network_latency_us,
                                      measurement.latency_breakdown.kernel_latency_us,
                                      measurement.latency_breakdown.processing_latency_us);
                            }
                        },
                        Err(e) => {
                            error!("延遲測量失敗: {}", e);
                        }
                    }
                },
                _ => {}
            }
        }
    }
    
    /// 處理 OrderBook 數據 (模擬實際處理)
    fn process_orderbook_data(data: &Value) -> Result<()> {
        // 模擬解析和處理 OrderBook 數據
        let _array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        // 模擬一些計算工作
        let mut sum = 0.0;
        for i in 0..100 {
            sum += (i as f64).sin().cos();
        }
        
        // 確保編譯器不會優化掉計算
        if sum > 1e6 {
            debug!("Unlikely computation result: {}", sum);
        }
        
        Ok(())
    }
    
    /// 啟動定期報告
    async fn start_periodic_reporting(&self) {
        let latency_measurer = self.latency_measurer.clone();
        let total_messages = self.total_messages.clone();
        let high_latency_messages = self.high_latency_messages.clone();
        let recent_measurements = self.recent_measurements.clone();
        let report_interval = self.args.report_interval_sec;
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(report_interval));
            
            loop {
                interval.tick().await;
                
                let total = total_messages.load(std::sync::atomic::Ordering::Relaxed);
                let high_latency = high_latency_messages.load(std::sync::atomic::Ordering::Relaxed);
                
                if total == 0 {
                    info!("📊 === 延遲測量報告 ===");
                    info!("📈 尚無數據");
                    continue;
                }
                
                // 獲取綜合統計
                let comprehensive_stats = latency_measurer.get_comprehensive_stats();
                
                // 計算最近測量的統計
                let recent_stats = {
                    let measurements = recent_measurements.read();
                    Self::calculate_recent_stats(&measurements)
                };
                
                // 打印詳細報告
                info!("📊 === 真實端到端延遲測量報告 ===");
                info!("📈 消息統計:");
                info!("   └─ 總消息數: {}", total);
                info!("   └─ 高延遲消息: {} ({:.1}%)", 
                      high_latency, (high_latency as f64 / total as f64) * 100.0);
                
                info!("⚡ 延遲分解 (最近 {} 條消息):", recent_stats.sample_count);
                info!("   └─ 平均總延遲: {:.1}μs", recent_stats.avg_total_latency_us);
                info!("   └─ 平均網絡延遲: {:.1}μs ({:.1}%)", 
                      recent_stats.avg_network_latency_us,
                      recent_stats.avg_network_ratio * 100.0);
                info!("   └─ 平均內核延遲: {:.1}μs ({:.1}%)", 
                      recent_stats.avg_kernel_latency_us,
                      recent_stats.avg_kernel_ratio * 100.0);
                info!("   └─ 平均處理延遲: {:.1}μs ({:.1}%)", 
                      recent_stats.avg_processing_latency_us,
                      recent_stats.avg_processing_ratio * 100.0);
                
                info!("📊 延遲百分位數:");
                info!("   └─ P50: {}μs", recent_stats.p50_latency_us);
                info!("   └─ P95: {}μs", recent_stats.p95_latency_us);
                info!("   └─ P99: {}μs", recent_stats.p99_latency_us);
                info!("   └─ 最大: {}μs", recent_stats.max_latency_us);
                
                info!("🌐 網絡統計:");
                info!("   └─ 總測量次數: {}", comprehensive_stats.network_stats.total_measurements);
                info!("   └─ 異常次數: {}", comprehensive_stats.network_stats.anomaly_count);
                info!("   └─ 平均網絡 RTT: {:.1}μs", comprehensive_stats.network_stats.overall_avg_latency_us);
                
                info!("🕐 時間同步狀態:");
                info!("   └─ 同步狀態: {}", 
                      if comprehensive_stats.time_sync_status.is_synchronized { "✅ 已同步" } else { "❌ 未同步" });
                if comprehensive_stats.time_sync_status.is_synchronized {
                    info!("   └─ 時間偏差: {}μs", comprehensive_stats.time_sync_status.offset_us);
                    info!("   └─ 使用服務器: {}", comprehensive_stats.time_sync_status.active_server);
                }
                
                info!("==========================================");
            }
        });
    }
    
    /// 啟動異常檢測
    async fn start_anomaly_detection(&self) {
        let recent_measurements = self.recent_measurements.clone();
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(10));
            
            loop {
                interval.tick().await;
                
                let measurements = recent_measurements.read();
                if measurements.len() < 50 {
                    continue;
                }
                
                // 檢測延遲突刺
                Self::detect_latency_spikes(&measurements);
                
                // 檢測網絡質量下降
                Self::detect_network_degradation(&measurements);
            }
        });
    }
    
    /// 檢測延遲突刺
    fn detect_latency_spikes(measurements: &[LatencyMeasurement]) {
        let recent_50: Vec<_> = measurements.iter().rev().take(50).collect();
        
        let avg_latency: f64 = recent_50
            .iter()
            .map(|m| m.latency_breakdown.total_latency_us as f64)
            .sum::<f64>() / recent_50.len() as f64;
        
        let std_dev = {
            let variance: f64 = recent_50
                .iter()
                .map(|m| {
                    let diff = m.latency_breakdown.total_latency_us as f64 - avg_latency;
                    diff * diff
                })
                .sum::<f64>() / recent_50.len() as f64;
            variance.sqrt()
        };
        
        // 檢查最近的測量是否是異常值 (超過 3 標準差)
        if let Some(latest) = recent_50.first() {
            let latest_latency = latest.latency_breakdown.total_latency_us as f64;
            if latest_latency > avg_latency + 3.0 * std_dev {
                warn!("🔥 檢測到延遲突刺: {:.0}μs (平均: {:.0}μs, 標準差: {:.0}μs)",
                      latest_latency, avg_latency, std_dev);
            }
        }
    }
    
    /// 檢測網絡質量下降
    fn detect_network_degradation(measurements: &[LatencyMeasurement]) {
        if measurements.len() < 100 {
            return;
        }
        
        let recent_50: Vec<_> = measurements.iter().rev().take(50).collect();
        let older_50: Vec<_> = measurements.iter().rev().skip(50).take(50).collect();
        
        let recent_avg_network: f64 = recent_50
            .iter()
            .map(|m| m.latency_breakdown.network_latency_us as f64)
            .sum::<f64>() / recent_50.len() as f64;
        
        let older_avg_network: f64 = older_50
            .iter()
            .map(|m| m.latency_breakdown.network_latency_us as f64)
            .sum::<f64>() / older_50.len() as f64;
        
        if recent_avg_network > older_avg_network * 1.5 {
            warn!("📡 檢測到網絡質量下降: {:.0}μs → {:.0}μs (+{:.1}%)",
                  older_avg_network, recent_avg_network,
                  ((recent_avg_network - older_avg_network) / older_avg_network) * 100.0);
        }
    }
    
    /// 計算最近測量的統計
    fn calculate_recent_stats(measurements: &[LatencyMeasurement]) -> RecentLatencyStats {
        if measurements.is_empty() {
            return RecentLatencyStats::default();
        }
        
        let recent: Vec<_> = measurements.iter().rev().take(100).collect();
        let count = recent.len();
        
        let total_latencies: Vec<u64> = recent.iter().map(|m| m.latency_breakdown.total_latency_us).collect();
        let network_latencies: Vec<u64> = recent.iter().map(|m| m.latency_breakdown.network_latency_us).collect();
        let kernel_latencies: Vec<u64> = recent.iter().map(|m| m.latency_breakdown.kernel_latency_us).collect();
        let processing_latencies: Vec<u64> = recent.iter().map(|m| m.latency_breakdown.processing_latency_us).collect();
        
        let avg_total = total_latencies.iter().sum::<u64>() as f64 / count as f64;
        let avg_network = network_latencies.iter().sum::<u64>() as f64 / count as f64;
        let avg_kernel = kernel_latencies.iter().sum::<u64>() as f64 / count as f64;
        let avg_processing = processing_latencies.iter().sum::<u64>() as f64 / count as f64;
        
        let avg_network_ratio = recent.iter().map(|m| m.latency_breakdown.network_ratio).sum::<f64>() / count as f64;
        let avg_kernel_ratio = recent.iter().map(|m| m.latency_breakdown.kernel_ratio).sum::<f64>() / count as f64;
        let avg_processing_ratio = recent.iter().map(|m| m.latency_breakdown.processing_ratio).sum::<f64>() / count as f64;
        
        let mut sorted_total = total_latencies.clone();
        sorted_total.sort_unstable();
        
        RecentLatencyStats {
            sample_count: count,
            avg_total_latency_us: avg_total,
            avg_network_latency_us: avg_network,
            avg_kernel_latency_us: avg_kernel,
            avg_processing_latency_us: avg_processing,
            avg_network_ratio,
            avg_kernel_ratio,
            avg_processing_ratio,
            p50_latency_us: Self::percentile(&sorted_total, 50.0),
            p95_latency_us: Self::percentile(&sorted_total, 95.0),
            p99_latency_us: Self::percentile(&sorted_total, 99.0),
            max_latency_us: sorted_total.iter().max().copied().unwrap_or(0),
        }
    }
    
    fn percentile(sorted_values: &[u64], percentile: f64) -> u64 {
        if sorted_values.is_empty() {
            return 0;
        }
        let index = ((percentile / 100.0) * (sorted_values.len() - 1) as f64).round() as usize;
        sorted_values[index.min(sorted_values.len() - 1)]
    }
    
    /// 打印最終報告
    async fn print_final_report(&self) {
        info!("📋 === 最終延遲測量報告 ===");
        
        let total = self.total_messages.load(std::sync::atomic::Ordering::Relaxed);
        let high_latency = self.high_latency_messages.load(std::sync::atomic::Ordering::Relaxed);
        
        info!("🔢 總體統計:");
        info!("   └─ 處理的消息總數: {}", total);
        info!("   └─ 高延遲消息數: {} ({:.1}%)", 
              high_latency, if total > 0 { (high_latency as f64 / total as f64) * 100.0 } else { 0.0 });
        
        if total > 0 {
            let comprehensive_stats = self.latency_measurer.get_comprehensive_stats();
            
            info!("📊 最終延遲統計:");
            info!("   └─ 平均總延遲: {:.1}μs", comprehensive_stats.latency_stats.avg_total_latency_us);
            info!("   └─ 平均網絡延遲: {:.1}μs", comprehensive_stats.latency_stats.avg_network_latency_us);
            info!("   └─ P99 延遲: {}μs", comprehensive_stats.latency_stats.p99_total_latency_us);
            info!("   └─ 最大延遲: {}μs", comprehensive_stats.latency_stats.max_total_latency_us);
            info!("   └─ 網絡延遲占比: {:.1}%", comprehensive_stats.latency_stats.avg_network_ratio * 100.0);
            
            info!("🌐 網絡質量:");
            info!("   └─ 網絡測量次數: {}", comprehensive_stats.network_stats.total_measurements);
            info!("   └─ 網絡異常次數: {}", comprehensive_stats.network_stats.anomaly_count);
            info!("   └─ 平均 RTT: {:.1}μs", comprehensive_stats.network_stats.overall_avg_latency_us);
        }
        
        info!("====================================");
    }
}

/// 最近延遲統計
#[derive(Debug, Default)]
struct RecentLatencyStats {
    sample_count: usize,
    avg_total_latency_us: f64,
    avg_network_latency_us: f64,
    avg_kernel_latency_us: f64,
    avg_processing_latency_us: f64,
    avg_network_ratio: f64,
    avg_kernel_ratio: f64,
    avg_processing_ratio: f64,
    p50_latency_us: u64,
    p95_latency_us: u64,
    p99_latency_us: u64,
    max_latency_us: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    
    info!("🚀 啟動真實端到端延遲測量系統");
    info!("⚙️  配置:");
    info!("   Symbol: {}", args.symbol);
    info!("   NTP 同步: {}", args.enable_ntp_sync);
    info!("   網絡監控: {}", args.enable_network_monitoring);
    info!("   報告間隔: {}秒", args.report_interval_sec);
    info!("   高延遲閾值: {}μs", args.high_latency_threshold_us);
    
    let system = RealLatencyMeasurementSystem::new(args).await?;
    system.run().await?;
    
    Ok(())
}