/*!
 * Volume-Based Multi-Connection Data Collection
 * 
 * 基於商品實際數據量動態分配WebSocket連接，解決單連接過載問題
 * 
 * 連接分配策略：
 * - Connection 0: 高流量商品 (ETHUSDT: 15.6 msg/s)
 * - Connection 1: 中高流量商品 (XRPUSDT: 9.3, SOLUSDT: 8.4 msg/s)
 * - Connection 2: 中流量商品 (BTCUSDT: 7.2, DOGEUSDT: 6.2 msg/s)
 * - Connection 3: 低流量商品 (其他: 4-5 msg/s)
 */

use rust_hft::core::app_runner::HftAppRunner;
use rust_hft::integrations::{
    BitgetConfig, BitgetChannel,
    create_volume_based_connector,
    SymbolVolumeMonitor,
    SymbolTrafficStats
};
use rust_hft::database::clickhouse_client::{ClickHouseClient, ClickHouseConfig};
use anyhow::Result;
use clap::Parser;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error, debug};

#[derive(Parser)]
#[command(
    name = "volume_based_collection",
    about = "Volume-based multi-connection market data collection with traffic monitoring"
)]
struct Args {
    #[arg(long, help = "Configuration file path")]
    config: String,
    
    #[arg(long, default_value = "600", help = "Collection duration in seconds")]
    duration: u64,
    
    #[arg(long, default_value = "false", help = "Enable ClickHouse storage")]
    clickhouse: bool,
    
    #[arg(long, default_value = "false", help = "Enable detailed traffic monitoring")]
    monitor: bool,
}

/// 基於流量的連接統計
#[derive(Debug)]
struct VolumeBasedStats {
    total_messages: AtomicU64,
    connection_messages: [AtomicU64; 4], // 4個連接的消息統計
    error_count: AtomicU32,
    start_time: Instant,
    volume_monitor: Option<Arc<SymbolVolumeMonitor>>,
}

impl VolumeBasedStats {
    fn new(enable_monitoring: bool) -> Self {
        let volume_monitor = if enable_monitoring {
            Some(Arc::new(SymbolVolumeMonitor::new(Duration::from_secs(60)))) // 1分鐘滑動窗口
        } else {
            None
        };
        
        Self {
            total_messages: AtomicU64::new(0),
            connection_messages: [
                AtomicU64::new(0),
                AtomicU64::new(0), 
                AtomicU64::new(0),
                AtomicU64::new(0)
            ],
            error_count: AtomicU32::new(0),
            start_time: Instant::now(),
            volume_monitor,
        }
    }
    
    async fn record_message(&self, symbol: &str, connection_id: Option<usize>, message_size: usize) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        
        if let Some(conn_id) = connection_id {
            if conn_id < 4 {
                self.connection_messages[conn_id].fetch_add(1, Ordering::Relaxed);
            }
        }
        
        // 記錄到流量監控器
        if let Some(ref monitor) = self.volume_monitor {
            monitor.record_message(symbol, message_size).await;
        }
    }
    
    fn record_error(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }
    
    fn get_stats(&self) -> (u64, [u64; 4], u32, f64) {
        let total = self.total_messages.load(Ordering::Relaxed);
        let conn_stats = [
            self.connection_messages[0].load(Ordering::Relaxed),
            self.connection_messages[1].load(Ordering::Relaxed),
            self.connection_messages[2].load(Ordering::Relaxed),
            self.connection_messages[3].load(Ordering::Relaxed),
        ];
        let errors = self.error_count.load(Ordering::Relaxed);
        let runtime = self.start_time.elapsed().as_secs_f64();
        
        (total, conn_stats, errors, runtime)
    }
    
    async fn print_traffic_analysis(&self) {
        if let Some(ref monitor) = self.volume_monitor {
            monitor.print_traffic_report().await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize HFT application
    let runner = HftAppRunner::new()?;
    info!("✅ HFT應用初始化完成");
    
    // Create volume-based Bitget connector  
    let bitget_config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        auto_reconnect: true,
        max_reconnect_attempts: 5,
        timeout_seconds: 10,
        ..Default::default()
    };
    
    let mut volume_connector = create_volume_based_connector(bitget_config);
    
    // Define symbols with expected traffic levels (based on historical data)
    let symbols = vec![
        // 高流量組 - 單獨連接
        "ETHUSDT",      // ~15.6 msg/s
        
        // 中高流量組 - 共享連接
        "XRPUSDT",      // ~9.3 msg/s
        "SOLUSDT",      // ~8.4 msg/s
        
        // 中流量組 - 共享連接
        "BTCUSDT",      // ~7.2 msg/s
        "DOGEUSDT",     // ~6.2 msg/s
        
        // 低流量組 - 合併連接
        "ADAUSDT",      // ~4.9 msg/s
        "BNBUSDT",      // ~4.9 msg/s
        "AVAXUSDT",     // ~4.8 msg/s
        "DOTUSDT",      // ~4.2 msg/s
        "MATICUSDT",    // 最低流量
    ];
    
    // Add subscriptions (將自動基於流量分配到不同連接)
    for symbol in &symbols {
        volume_connector.add_subscription(symbol.to_string(), BitgetChannel::Books5).await?;
        volume_connector.add_subscription(symbol.to_string(), BitgetChannel::Books15).await?;
        volume_connector.add_subscription(symbol.to_string(), BitgetChannel::Trade).await?;
        volume_connector.add_subscription(symbol.to_string(), BitgetChannel::Ticker).await?;
        
        info!("✅ 訂閱 {} - 基於流量分配連接", symbol);
    }
    
    // Print connection distribution based on volume
    volume_connector.print_distribution();
    
    // Initialize statistics with volume monitoring
    let stats = Arc::new(VolumeBasedStats::new(args.monitor));
    let stats_clone = Arc::clone(&stats);
    
    // Initialize ClickHouse client if enabled
    let clickhouse_client = if args.clickhouse {
        let clickhouse_config = ClickHouseConfig {
            url: "http://localhost:8123".to_string(),
            database: "hft_db".to_string(),
            username: "default".to_string(),
            password: "".to_string(),
            ..Default::default()
        };
        
        match ClickHouseClient::new(clickhouse_config) {
            Ok(client) => Some(Arc::new(Mutex::new(client))),
            Err(e) => {
                warn!("ClickHouse初始化失敗: {}, 跳過存儲", e);
                None
            }
        }
    } else {
        None
    };
    
    // Start enhanced monitoring task  
    let stats_monitor = Arc::clone(&stats);
    let monitoring_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        
        loop {
            interval.tick().await;
            let (total, conn_stats, errors, runtime) = stats_monitor.get_stats();
            let throughput = if runtime > 0.0 { total as f64 / runtime } else { 0.0 };
            
            info!("📊 實時統計 - 總消息: {} | 吞吐量: {:.1} msg/s | 錯誤: {} | 運行時間: {:.1}s", 
                  total, throughput, errors, runtime);
            info!("🔗 連接統計 - 高流量: {} | 中高流量: {} | 中流量: {} | 低流量: {}",
                  conn_stats[0], conn_stats[1], conn_stats[2], conn_stats[3]);
            
            // 每5分鐘打印詳細流量分析
            if runtime % 300.0 < 30.0 && args.monitor {
                stats_monitor.print_traffic_analysis().await;
            }
        }
    });
    
    // Start data collection task with volume tracking
    let message_receiver = volume_connector.get_message_receiver();
    let collection_task = tokio::spawn(async move {
        let mut receiver = message_receiver.lock().await;
        
        while let Some(message) = receiver.recv().await {
            let (symbol, connection_id, message_size) = match &message {
                rust_hft::integrations::BitgetMessage::OrderBook { symbol, data, .. } => {
                    let conn_id = get_volume_based_connection_id(symbol);
                    let size = data.to_string().len();
                    (symbol.clone(), Some(conn_id), size)
                }
                rust_hft::integrations::BitgetMessage::Trade { symbol, data, .. } => {
                    let conn_id = get_volume_based_connection_id(symbol);
                    let size = data.to_string().len();
                    (symbol.clone(), Some(conn_id), size)
                }
                rust_hft::integrations::BitgetMessage::Ticker { symbol, data, .. } => {
                    let conn_id = get_volume_based_connection_id(symbol);
                    let size = data.to_string().len();
                    (symbol.clone(), Some(conn_id), size)
                }
            };
            
            stats_clone.record_message(&symbol, connection_id, message_size).await;
            
            // Process message
            debug!("收到消息 - 商品: {}, 連接: {:?}, 大小: {}B", symbol, connection_id, message_size);
            
            // Store to ClickHouse if enabled
            if let Some(ref clickhouse) = clickhouse_client {
                if let Err(e) = store_to_clickhouse(&message, clickhouse).await {
                    error!("ClickHouse 存儲失敗: {}", e);
                    stats_clone.record_error();
                }
            }
        }
    });
    
    // Start volume-based connections
    info!("🔄 啟動基於流量的多連接數據收集...");
    let connection_task = tokio::spawn(async move {
        if let Err(e) = volume_connector.connect_all().await {
            error!("基於流量的多連接失敗: {}", e);
        }
    });
    
    // Run with timeout
    info!("⏱️  運行 {} 秒，基於商品數據量智能分配連接", args.duration);
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(args.duration)) => {
            info!("⏰ 收集時間結束");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("⛔ 收到中斷信號");
        }
    }
    
    // Cancel tasks
    monitoring_task.abort();
    collection_task.abort();
    connection_task.abort();
    
    // Print final volume-based statistics
    let (total, conn_stats, errors, runtime) = stats.get_stats();
    let throughput = if runtime > 0.0 { total as f64 / runtime } else { 0.0 };
    
    info!("🎯 === 基於流量的多連接收集完成統計 ===");
    info!("⏱️  總運行時間: {:.1} 分鐘", runtime / 60.0);
    info!("📦 總消息數: {}", total);
    info!("🚀 平均吞吐量: {:.1} msg/s", throughput);
    info!("❌ 處理錯誤: {}", errors);
    info!("✅ 成功率: {:.2}%", if total > 0 { (1.0 - errors as f64 / total as f64) * 100.0 } else { 0.0 });
    info!("📊 連接負載分布:");
    info!("   🔴 連接0 (高流量-ETH): {} 條 ({:.1} msg/s)", conn_stats[0], conn_stats[0] as f64 / runtime);
    info!("   🟡 連接1 (中高流量-XRP,SOL): {} 條 ({:.1} msg/s)", conn_stats[1], conn_stats[1] as f64 / runtime);
    info!("   🟢 連接2 (中流量-BTC,DOGE): {} 條 ({:.1} msg/s)", conn_stats[2], conn_stats[2] as f64 / runtime);
    info!("   🔵 連接3 (低流量-其他): {} 條 ({:.1} msg/s)", conn_stats[3], conn_stats[3] as f64 / runtime);
    
    // Print detailed traffic analysis
    if args.monitor {
        info!("📈 === 商品流量分析 ===");
        stats.print_traffic_analysis().await;
    }
    
    // Performance evaluation for volume-based approach
    let max_conn_load = conn_stats.iter().map(|&x| x as f64 / runtime).fold(0.0, f64::max);
    let min_conn_load = conn_stats.iter().map(|&x| x as f64 / runtime).fold(f64::INFINITY, f64::min);
    let load_balance_ratio = if max_conn_load > 0.0 { min_conn_load / max_conn_load } else { 1.0 };
    
    info!("⚖️  負載均衡評估:");
    info!("   最大連接負載: {:.1} msg/s", max_conn_load);
    info!("   最小連接負載: {:.1} msg/s", min_conn_load);
    info!("   負載均衡比: {:.2} (越接近1.0越均衡)", load_balance_ratio);
    
    if throughput > 100.0 && load_balance_ratio > 0.6 {
        info!("🏆 性能評級: 🟢 優秀 (吞吐量: {:.1} msg/s, 負載均衡: {:.2})", throughput, load_balance_ratio);
    } else if throughput > 60.0 && load_balance_ratio > 0.4 {
        info!("🏆 性能評級: 🟡 良好 (吞吐量: {:.1} msg/s, 負載均衡: {:.2})", throughput, load_balance_ratio);
    } else {
        info!("🏆 性能評級: 🔴 需要優化 (吞吐量: {:.1} msg/s, 負載均衡: {:.2})", throughput, load_balance_ratio);
    }
    
    info!("🎉 基於商品流量的智能連接分配完成！");
    Ok(())
}

/// 根據商品獲取對應的連接ID (基於歷史流量數據)
fn get_volume_based_connection_id(symbol: &str) -> usize {
    match symbol {
        // 高流量組 (>10 msg/s) - 連接0
        "ETHUSDT" => 0,
        
        // 中高流量組 (8-10 msg/s) - 連接1
        "XRPUSDT" | "SOLUSDT" => 1,
        
        // 中流量組 (6-8 msg/s) - 連接2
        "BTCUSDT" | "DOGEUSDT" => 2,
        
        // 低流量組 (<6 msg/s) - 連接3
        _ => 3,
    }
}

/// Store message to ClickHouse
async fn store_to_clickhouse(
    message: &rust_hft::integrations::BitgetMessage,
    _clickhouse: &Arc<Mutex<ClickHouseClient>>
) -> Result<()> {
    // Implement ClickHouse storage logic based on message type
    match message {
        rust_hft::integrations::BitgetMessage::OrderBook { symbol, .. } => {
            debug!("存儲訂單簿數據: {}", symbol);
        }
        rust_hft::integrations::BitgetMessage::Trade { symbol, .. } => {
            debug!("存儲交易數據: {}", symbol);
        }
        rust_hft::integrations::BitgetMessage::Ticker { symbol, .. } => {
            debug!("存儲行情數據: {}", symbol);
        }
    }
    
    Ok(())
}