/*!
 * Multi-Connection High Performance Data Collection
 * 
 * Solves the single WebSocket connection overload issue by distributing
 * 40 subscriptions across multiple connections based on data type frequency.
 * 
 * Architecture:
 * - Connection 0: High-frequency orderbook data (books5, books15)
 * - Connection 1: Medium-frequency trade data  
 * - Connection 2: Low-frequency ticker data
 */

use rust_hft::core::app_runner::HftAppRunner;
use rust_hft::integrations::{
    BitgetConfig, BitgetChannel,
    create_optimized_multi_connector,
    MultiConnectionBitgetConnector
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
    name = "multi_connection_collection",
    about = "Multi-connection high performance market data collection"
)]
struct Args {
    #[arg(long, help = "Configuration file path")]
    config: String,
    
    #[arg(long, default_value = "600", help = "Collection duration in seconds")]
    duration: u64,
    
    #[arg(long, default_value = "false", help = "Enable ClickHouse storage")]
    clickhouse: bool,
}

/// Multi-connection statistics tracker
#[derive(Debug)]
struct MultiConnectionStats {
    total_messages: AtomicU64,
    connection_messages: [AtomicU64; 3], // Track messages per connection
    error_count: AtomicU32,
    start_time: Instant,
    last_report_time: AtomicU64,
}

impl MultiConnectionStats {
    fn new() -> Self {
        Self {
            total_messages: AtomicU64::new(0),
            connection_messages: [
                AtomicU64::new(0),
                AtomicU64::new(0), 
                AtomicU64::new(0)
            ],
            error_count: AtomicU32::new(0),
            start_time: Instant::now(),
            last_report_time: AtomicU64::new(0),
        }
    }
    
    fn record_message(&self, connection_id: Option<usize>) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        if let Some(conn_id) = connection_id {
            if conn_id < 3 {
                self.connection_messages[conn_id].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    
    fn record_error(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }
    
    fn get_stats(&self) -> (u64, [u64; 3], u32, f64) {
        let total = self.total_messages.load(Ordering::Relaxed);
        let conn_stats = [
            self.connection_messages[0].load(Ordering::Relaxed),
            self.connection_messages[1].load(Ordering::Relaxed),
            self.connection_messages[2].load(Ordering::Relaxed),
        ];
        let errors = self.error_count.load(Ordering::Relaxed);
        let runtime = self.start_time.elapsed().as_secs_f64();
        
        (total, conn_stats, errors, runtime)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize HFT application
    let runner = HftAppRunner::new()?;
    info!("✅ HFT應用初始化完成");
    
    // Load configuration from runner (it already loads config)
    let config = &runner.config;
    info!("✅ 配置加載完成: {}", args.config);
    
    // Create multi-connection Bitget connector
    let bitget_config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        auto_reconnect: true,
        max_reconnect_attempts: 5,
        timeout_seconds: 10,
        ..Default::default()
    };
    
    let mut multi_connector = create_optimized_multi_connector(bitget_config);
    
    // Define symbols to collect
    let symbols = vec![
        "BTCUSDT", "ETHUSDT", "SOLUSDT", "ADAUSDT", "BNBUSDT",
        "DOGEUSDT", "XRPUSDT", "DOTUSDT", "MATICUSDT", "AVAXUSDT"
    ];
    
    // Add subscriptions (will be automatically distributed across connections)
    for symbol in &symbols {
        // High-frequency data (books) -> Connection 0
        multi_connector.add_subscription(symbol.to_string(), BitgetChannel::Books5).await?;
        multi_connector.add_subscription(symbol.to_string(), BitgetChannel::Books15).await?;
        
        // Medium-frequency data (trade) -> Connection 1
        multi_connector.add_subscription(symbol.to_string(), BitgetChannel::Trade).await?;
        
        // Low-frequency data (ticker) -> Connection 2  
        multi_connector.add_subscription(symbol.to_string(), BitgetChannel::Ticker).await?;
        
        info!("✅ 訂閱 {} - Books5, Books15, Trade, Ticker", symbol);
    }
    
    // Print connection distribution
    multi_connector.print_distribution();
    
    // Initialize statistics
    let stats = Arc::new(MultiConnectionStats::new());
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
    
    // Start real-time monitoring task
    let stats_monitor = Arc::clone(&stats);
    let monitoring_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        
        loop {
            interval.tick().await;
            let (total, conn_stats, errors, runtime) = stats_monitor.get_stats();
            let throughput = if runtime > 0.0 { total as f64 / runtime } else { 0.0 };
            
            info!("📊 實時統計 - 總消息: {} | 吞吐量: {:.1} msg/s | 錯誤: {} | 運行時間: {:.1}s", 
                  total, throughput, errors, runtime);
            info!("📈 連接統計 - Books: {} | Trade: {} | Ticker: {}", 
                  conn_stats[0], conn_stats[1], conn_stats[2]);
        }
    });
    
    // Start data collection task
    let message_receiver = multi_connector.get_message_receiver();
    let collection_task = tokio::spawn(async move {
        let mut receiver = message_receiver.lock().await;
        
        while let Some(message) = receiver.recv().await {
            // Determine connection based on message type for statistics
            let connection_id = match &message {
                rust_hft::integrations::BitgetMessage::OrderBook { .. } => Some(0),
                rust_hft::integrations::BitgetMessage::Trade { .. } => Some(1),  
                rust_hft::integrations::BitgetMessage::Ticker { .. } => Some(2),
            };
            
            stats_clone.record_message(connection_id);
            
            // Process message (implement your data processing logic here)
            debug!("收到消息: {:?}", message);
            
            // Store to ClickHouse if enabled
            if let Some(ref clickhouse) = clickhouse_client {
                if let Err(e) = store_to_clickhouse(&message, clickhouse).await {
                    error!("ClickHouse 存儲失敗: {}", e);
                    stats_clone.record_error();
                }
            }
        }
    });
    
    // Start all connections
    info!("🔄 啟動多連接數據收集...");
    let connection_task = tokio::spawn(async move {
        if let Err(e) = multi_connector.connect_all().await {
            error!("多連接失敗: {}", e);
        }
    });
    
    // Run with timeout
    info!("⏱️  運行 {} 秒", args.duration);
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
    
    // Print final statistics
    let (total, conn_stats, errors, runtime) = stats.get_stats();
    let throughput = if runtime > 0.0 { total as f64 / runtime } else { 0.0 };
    
    info!("🎯 === 多連接數據收集完成統計 ===");
    info!("⏱️  總運行時間: {:.1} 分鐘", runtime / 60.0);
    info!("📦 總消息數: {}", total);
    info!("🚀 平均吞吐量: {:.1} msg/s", throughput);
    info!("❌ 處理錯誤: {}", errors);
    info!("✅ 成功率: {:.2}%", if total > 0 { (1.0 - errors as f64 / total as f64) * 100.0 } else { 0.0 });
    info!("📊 連接分布統計:");
    info!("   📈 連接0 (Books): {} 條 ({:.1} msg/s)", conn_stats[0], conn_stats[0] as f64 / runtime);
    info!("   📈 連接1 (Trade): {} 條 ({:.1} msg/s)", conn_stats[1], conn_stats[1] as f64 / runtime);
    info!("   📈 連接2 (Ticker): {} 條 ({:.1} msg/s)", conn_stats[2], conn_stats[2] as f64 / runtime);
    
    // Performance evaluation
    if throughput > 100.0 {
        info!("🏆 性能評級: 🟢 優秀 (吞吐量: {:.1} msg/s)", throughput);
    } else if throughput > 50.0 {
        info!("🏆 性能評級: 🟡 良好 (吞吐量: {:.1} msg/s)", throughput);
    } else {
        info!("🏆 性能評級: 🔴 需要優化 (吞吐量: {:.1} msg/s)", throughput);
    }
    
    info!("🎉 多連接數據收集完成！");
    Ok(())
}

/// Store message to ClickHouse
async fn store_to_clickhouse(
    message: &rust_hft::integrations::BitgetMessage,
    _clickhouse: &Arc<Mutex<ClickHouseClient>>
) -> Result<()> {
    // Implement ClickHouse storage logic based on message type
    // This is a placeholder implementation
    match message {
        rust_hft::integrations::BitgetMessage::OrderBook { symbol, .. } => {
            debug!("存儲訂單簿數據: {} ", symbol);
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