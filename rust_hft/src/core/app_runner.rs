/*!
 * 統一應用啟動器 - 消除重複的初始化代碼
 * 
 * 提供標準化的HFT應用啟動模板，包含：
 * - 日誌初始化
 * - 性能優化
 * - WebSocket連接管理  
 * - 錯誤處理
 * - 優雅關閉
 */

use crate::{
    core::{config::Config, types::*},
    integrations::{UnifiedBitgetConnector, UnifiedBitgetConfig, UnifiedBitgetMessage, UnifiedBitgetChannel, ConnectionMode},
    utils::performance::*,
};
use anyhow::Result;
use tracing::{info, error};
use tokio::time::Duration;
use std::sync::Arc;

/// 統一的HFT應用啟動器
pub struct HftAppRunner {
    pub config: Config,
    pub performance_manager: PerformanceManager,
    connector: Option<UnifiedBitgetConnector>,
    reporter: Option<Arc<UnifiedReporter>>,
}

impl HftAppRunner {
    /// 創建新的應用啟動器，包含所有標準初始化
    pub fn new() -> Result<Self> {
        // 1. 初始化日誌系統 (安全初始化，避免重複設置)
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .try_init();
        
        info!("🚀 Initializing HFT Application");
        
        // 2. 加載配置
        let config = Config::load()?;
        
        // 3. 性能優化初始化
        let performance_config = PerformanceConfig::default();
        let mut performance_manager = PerformanceManager::new(performance_config)?;
        
        // 4. 檢測硬件能力
        let capabilities = detect_hardware_capabilities();
        info!("Hardware: {} cores, AVX2: {}, AVX512: {}", 
              capabilities.cpu_count, capabilities.has_avx2, capabilities.has_avx512);
        
        // 5. 應用性能優化
        performance_manager.optimize_for_trading()?;
        
        info!("✅ HFT Application initialized successfully");
        
        Ok(Self {
            config,
            performance_manager,
            connector: None,
            reporter: None,
        })
    }
    
    /// 使用自定義配置創建應用
    pub fn with_config(config: Config) -> Result<Self> {
        let mut app = Self::new()?;
        app.config = config;
        Ok(app)
    }
    
    /// 設置Bitget連接器
    pub fn with_bitget_connector(&mut self, bitget_config: UnifiedBitgetConfig) -> &mut Self {
        self.connector = Some(UnifiedBitgetConnector::new(bitget_config));
        self
    }
    
    /// 設置統計報告器
    pub fn with_reporter(&mut self, interval_secs: u64) -> &mut Self {
        self.reporter = Some(Arc::new(UnifiedReporter::new(interval_secs)));
        self
    }
    
    /// 獲取連接器的可變引用
    pub fn get_connector_mut(&mut self) -> Option<&mut UnifiedBitgetConnector> {
        self.connector.as_mut()
    }
    
    /// 設置連接器
    pub fn set_connector(&mut self, connector: Option<UnifiedBitgetConnector>) {
        self.connector = connector;
    }
    
    /// 運行應用 - 標準模式
    pub async fn run<F>(&mut self, message_handler: F) -> Result<()>
    where
        F: Fn(UnifiedBitgetMessage) + Send + Sync + 'static,
    {
        let connector = self.connector.take()
            .ok_or_else(|| anyhow::anyhow!("Bitget connector not configured"))?;
        
        self.run_with_connector(connector, message_handler).await
    }
    
    /// 運行應用 - 帶超時
    pub async fn run_with_timeout<F>(&mut self, 
                                     message_handler: F, 
                                     timeout_secs: u64) -> Result<()>
    where
        F: Fn(UnifiedBitgetMessage) + Send + Sync + 'static,
    {
        let connector = self.connector.take()
            .ok_or_else(|| anyhow::anyhow!("Bitget connector not configured"))?;
        
        info!("⏱️  Running with {} second timeout", timeout_secs);
        
        tokio::select! {
            result = self.run_with_connector(connector, message_handler) => result,
            _ = tokio::time::sleep(Duration::from_secs(timeout_secs)) => {
                info!("⏰ Application timeout reached");
                Ok(())
            }
        }
    }
    
    /// 運行應用 - 帶計數限制
    pub async fn run_with_message_limit<F>(&mut self, 
                                           message_handler: F, 
                                           max_messages: u64) -> Result<()>
    where
        F: Fn(UnifiedBitgetMessage) + Send + Sync + 'static,
    {
        let connector = self.connector.take()
            .ok_or_else(|| anyhow::anyhow!("Bitget connector not configured"))?;
        
        info!("📊 Running with {} message limit", max_messages);
        
        let message_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let message_count_clone = message_count.clone();
        
        let wrapped_handler = move |msg: UnifiedBitgetMessage| {
            let count = message_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count < max_messages {
                message_handler(msg);
            }
        };
        
        tokio::select! {
            result = self.run_with_connector(connector, wrapped_handler) => result,
            _ = async {
                loop {
                    if message_count.load(std::sync::atomic::Ordering::Relaxed) >= max_messages {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            } => {
                info!("📈 Message limit reached");
                Ok(())
            }
        }
    }
    
    /// 核心運行邏輯
    async fn run_with_connector<F>(&mut self,
                                   connector: UnifiedBitgetConnector,
                                   message_handler: F) -> Result<()>
    where
        F: Fn(UnifiedBitgetMessage) + Send + Sync + 'static,
    {
        // 啟動統計報告器
        let reporter_task = if let Some(reporter) = &self.reporter {
            let reporter_clone = reporter.clone();
            Some(tokio::spawn(async move {
                reporter_clone.start_periodic_reporting().await;
            }))
        } else {
            None
        };
        
        // 創建增強的消息處理器（帶性能監控）
        let reporter = self.reporter.clone();
        
        let enhanced_handler = move |message: UnifiedBitgetMessage| {
            let start_time = now_micros();
            
            // 調用用戶處理器
            message_handler(message);
            
            // 記錄性能指標
            let latency = now_micros() - start_time;
            
            if let Some(ref reporter) = reporter {
                reporter.record_latency("message_processing", latency);
                reporter.record_count("messages_processed");
            }
        };
        
        info!("🔌 Starting WebSocket connection...");

        // 啟動連接並獲取消息接收端
        let mut rx = match connector.start().await {
            Ok(rx) => rx,
            Err(e) => {
                error!("❌ Connection failed: {}", e);
                // 結束前清理 reporter
                if let Some(task) = reporter_task { task.abort(); }
                return Err(e);
            }
        };

        // 消息處理循環與關閉信號監聽
        tokio::select! {
            _ = async {
                while let Some(msg) = rx.recv().await {
                    enhanced_handler(msg);
                }
            } => {}
            _ = tokio::signal::ctrl_c() => {
                info!("📊 Shutdown signal received");
            }
        }

        // 嘗試停止連接器
        let _ = connector.stop().await;

        // 清理
        if let Some(task) = reporter_task { task.abort(); }

        // 生成最終報告
        self.generate_final_report();

        Ok(())
    }
    
    /// 獲取性能統計
    pub fn get_performance_stats(&self) -> PerformanceStats {
        self.performance_manager.get_performance_stats()
    }
    
    /// 生成最終報告
    fn generate_final_report(&self) {
        info!("📋 === Final Application Report ===");
        
        let stats = self.get_performance_stats();
        info!("Messages Processed: {}", stats.messages_processed);
        info!("Average Latency: {:.2}μs", stats.avg_latency_us);
        info!("Max Latency: {}μs", stats.max_latency_us);
        info!("Min Latency: {}μs", stats.min_latency_us);
        info!("Memory Pool - Created: {}, Reused: {}", 
              stats.memory_pool_created, stats.memory_pool_reused);
        
        if let Some(ref reporter) = self.reporter {
            reporter.generate_final_report();
        }
        
        info!("=====================================");
    }
}

/// 統一的統計報告器
pub struct UnifiedReporter {
    stats: std::sync::Mutex<std::collections::HashMap<String, f64>>,
    counters: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    start_time: std::time::Instant,
    interval_secs: u64,
}

impl UnifiedReporter {
    pub fn new(interval_secs: u64) -> Self {
        Self {
            stats: std::sync::Mutex::new(std::collections::HashMap::new()),
            counters: std::sync::Mutex::new(std::collections::HashMap::new()),
            start_time: std::time::Instant::now(),
            interval_secs,
        }
    }
    
    pub fn record_latency(&self, metric: &str, latency_us: u64) {
        let mut stats = self.stats.lock().unwrap();
        let current = *stats.get(metric).unwrap_or(&0.0);
        stats.insert(metric.to_string(), current + latency_us as f64);
    }
    
    pub fn record_count(&self, metric: &str) {
        let mut counters = self.counters.lock().unwrap();
        let current = *counters.get(metric).unwrap_or(&0);
        counters.insert(metric.to_string(), current + 1);
    }
    
    pub async fn start_periodic_reporting(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(self.interval_secs));
        
        loop {
            interval.tick().await;
            self.print_report();
        }
    }
    
    fn print_report(&self) {
        let stats = self.stats.lock().unwrap();
        let counters = self.counters.lock().unwrap();
        
        info!("📊 === Periodic Report ===");
        for (metric, count) in counters.iter() {
            info!("{}: {}", metric, count);
        }
        
        for (metric, total_latency) in stats.iter() {
            if let Some(count) = counters.get(&format!("{}_count", metric.replace("_latency", ""))) {
                if *count > 0 {
                    let avg = total_latency / *count as f64;
                    info!("{}: {:.2}μs avg", metric, avg);
                }
            }
        }
        info!("========================");
    }
    
    pub fn generate_final_report(&self) {
        info!("📋 === Final Reporter Statistics ===");
        let elapsed = self.start_time.elapsed();
        info!("Total Runtime: {:?}", elapsed);
        
        let counters = self.counters.lock().unwrap();
        for (metric, count) in counters.iter() {
            let rate = *count as f64 / elapsed.as_secs() as f64;
            info!("{}: {} ({:.2}/sec)", metric, count, rate);
        }
        info!("===================================");
    }
}

/// 快速創建應用的便利函數
pub async fn run_simple_app<F>(symbol: &str, handler: F) -> Result<()>
where
    F: Fn(UnifiedBitgetMessage) + Send + Sync + 'static,
{
    let mut app = HftAppRunner::new()?;
    
    let bitget_config = UnifiedBitgetConfig {
        ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        api_key: None,
        api_secret: None,
        passphrase: None,
        mode: crate::integrations::ConnectionMode::Single,
        max_reconnect_attempts: 5,
        reconnect_delay_ms: 1000,
        enable_compression: true,
        max_message_size: 1024 * 1024,
        ping_interval_secs: 30,
        connection_timeout_secs: 10,
    };
    app.with_bitget_connector(bitget_config);
    
    let connector = app.connector.take().unwrap();
    let _ = connector.subscribe(&symbol, crate::integrations::UnifiedBitgetChannel::OrderBook5).await;
    app.connector = Some(connector);
    
    app.run(handler).await
}

/// 快速創建帶超時的應用
pub async fn run_timed_app<F>(symbol: &str, timeout_secs: u64, handler: F) -> Result<()>
where
    F: Fn(UnifiedBitgetMessage) + Send + Sync + 'static,
{
    let mut app = HftAppRunner::new()?;
    
    let bitget_config = UnifiedBitgetConfig {
        ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        api_key: None,
        api_secret: None,
        passphrase: None,
        mode: ConnectionMode::Single,
        max_reconnect_attempts: 5,
        reconnect_delay_ms: 1000,
        enable_compression: true,
        max_message_size: 1024 * 1024,
        ping_interval_secs: 30,
        connection_timeout_secs: 10,
    };
    app.with_bitget_connector(bitget_config)
       .with_reporter(30); // 30秒報告間隔
    
    let connector = app.connector.take().unwrap();
    let _ = connector.subscribe(&symbol, UnifiedBitgetChannel::OrderBook5).await;
    app.connector = Some(connector);
    
    app.run_with_timeout(handler, timeout_secs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_app_runner_creation() {
        let app = HftAppRunner::new();
        assert!(app.is_ok());
    }
    
    #[test]
    fn test_unified_reporter() {
        let reporter = UnifiedReporter::new(1);
        reporter.record_latency("test", 100);
        reporter.record_count("messages");
        
        // Should not panic
        reporter.generate_final_report();
    }
}
