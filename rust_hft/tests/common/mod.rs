#![cfg(feature = "legacy-sdk")]

/*!
 * 通用測試工具和輔助函數
 */

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use std::collections::HashMap;
use tokio::sync::{mpsc, Mutex};
use std::sync::Arc;
use anyhow::Result;
use tracing_subscriber::{EnvFilter, fmt};

/// 初始化测试日志
pub fn init_test_logging() {
    let _ = fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("rust_hft=debug".parse().unwrap()))
        .with_test_writer()
        .try_init();
}

/// 延迟跟踪器
#[derive(Debug)]
pub struct LatencyTracker {
    samples: Arc<Mutex<Vec<f64>>>,
}

impl LatencyTracker {
    pub fn new() -> Self {
        Self {
            samples: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn record(&self, latency_ms: f64) {
        let mut samples = self.samples.lock().await;
        samples.push(latency_ms);
    }

    pub async fn get_stats(&self) -> (f64, f64, f64) {
        let samples = self.samples.lock().await;
        if samples.is_empty() {
            return (0.0, 0.0, 0.0);
        }

        let sum: f64 = samples.iter().sum();
        let avg = sum / samples.len() as f64;

        let mut sorted = samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let p99_index = ((samples.len() as f64) * 0.99) as usize;
        let p99 = sorted.get(p99_index.min(sorted.len() - 1)).copied().unwrap_or(0.0);

        let max = sorted.last().copied().unwrap_or(0.0);

        (avg, p99, max)
    }
}

/// 性能測試助手
#[derive(Debug)]
pub struct PerformanceHelper {
    pub start_time: Instant,
    pub operation_count: AtomicU64,
    pub total_latency_ns: AtomicU64,
}

impl PerformanceHelper {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            operation_count: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
        }
    }

    pub fn record_operation(&self, latency_ns: u64) {
        self.operation_count.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns.fetch_add(latency_ns, Ordering::Relaxed);
    }

    pub fn get_stats(&self) -> (u64, f64, f64) {
        let count = self.operation_count.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed);
        let elapsed = self.start_time.elapsed().as_secs_f64();

        let avg_latency = if count > 0 { total_latency as f64 / count as f64 } else { 0.0 };
        let throughput = if elapsed > 0.0 { count as f64 / elapsed } else { 0.0 };

        (count, avg_latency, throughput)
    }
}

/// 測試數據生成器
pub mod test_data {
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 生成模擬訂單簿數據
    pub fn generate_orderbook_levels(count: usize, base_price: f64) -> Vec<(f64, f64)> {
        (0..count)
            .map(|i| {
                let price = base_price + i as f64 * 0.01;
                let quantity = 1.0 + (i % 10) as f64 * 0.1;
                (price, quantity)
            })
            .collect()
    }

    /// 生成模擬市場消息
    pub fn generate_market_message(symbol: &str, sequence: u64) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        format!(
            r#"{{"channel":"books5","instId":"{}","data":[{{"asks":[["50000.{}","1.0"]],"bids":[["49999.{}","1.5"]],"ts":"{}","checksum":{}}}]}}"#,
            symbol,
            sequence % 100,
            sequence % 100,
            timestamp,
            123456 + sequence
        )
    }

    /// 生成測試用的二進制數據
    pub fn generate_binary_data(size: usize, pattern: u8) -> Vec<u8> {
        let mut data = vec![pattern; size];
        // 添加一些變化
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = (*byte).wrapping_add(i as u8);
        }
        data
    }
}

/// 模擬 WebSocket 助手
pub mod mock_websocket {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    pub fn create_mock_stream(messages: Vec<String>) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            for message in messages {
                tx.send(message).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        });

        rx
    }
}

/// Mock Redis Connection for testing
#[derive(Clone)]
pub struct MockRedis {
    published_messages: Arc<Mutex<Vec<(String, String)>>>,
}

impl MockRedis {
    pub fn new() -> Self {
        Self {
            published_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn publish(&self, channel: &str, message: &str) -> Result<()> {
        let mut messages = self.published_messages.lock().await;
        messages.push((channel.to_string(), message.to_string()));
        Ok(())
    }

    pub async fn get_published_messages(&self) -> Vec<(String, String)> {
        self.published_messages.lock().await.clone()
    }

    pub async fn clear_messages(&self) {
        self.published_messages.lock().await.clear();
    }
}

/// Test Exchange Connector for mocking exchange connections
pub struct TestExchangeConnector {
    pub name: String,
    pub connected: bool,
    pub subscribed_symbols: Vec<String>,
    pub message_sender: Option<mpsc::UnboundedSender<String>>,
}

impl TestExchangeConnector {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            connected: false,
            subscribed_symbols: Vec::new(),
            message_sender: None,
        }
    }

    pub fn set_message_sender(&mut self, sender: mpsc::UnboundedSender<String>) {
        self.message_sender = Some(sender);
    }

    pub fn simulate_market_data(&self, symbol: &str, sequence: u64) -> Result<()> {
        if let Some(sender) = &self.message_sender {
            let message = test_data::generate_market_message(symbol, sequence);
            sender.send(message).map_err(|e| anyhow::anyhow!("Send error: {}", e))?;
        }
        Ok(())
    }
}

/// Test Configuration Builder
pub struct TestConfigBuilder {
    symbols: Vec<String>,
    exchanges: Vec<String>,
    redis_url: String,
}

impl TestConfigBuilder {
    pub fn new() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            exchanges: vec!["binance".to_string()],
            redis_url: "redis://localhost:6379".to_string(),
        }
    }

    pub fn with_symbols(mut self, symbols: Vec<&str>) -> Self {
        self.symbols = symbols.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_exchanges(mut self, exchanges: Vec<&str>) -> Self {
        self.exchanges = exchanges.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn build(self) -> rust_hft::integrations::multi_exchange_manager::MultiExchangeConfig {
        rust_hft::integrations::multi_exchange_manager::MultiExchangeConfig {
            enabled_exchanges: self.exchanges,
            symbols: self.symbols,
            redis_url: self.redis_url,
            auto_reconnect: true,
            reconnect_interval_secs: 1, // Fast reconnect for tests
        }
    }
}

// 移除重复的 LatencyTracker 定义 - 使用上面已定义的版本

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_performance_helper() {
        let helper = PerformanceHelper::new();
        helper.record_operation(1000);
        helper.record_operation(2000);

        let (count, avg_latency, _throughput) = helper.get_stats();
        assert_eq!(count, 2);
        assert_eq!(avg_latency, 1500.0);
    }

    #[test]
    fn test_generate_orderbook_levels() {
        let levels = test_data::generate_orderbook_levels(5, 50000.0);
        assert_eq!(levels.len(), 5);
        assert_eq!(levels[0].0, 50000.0);
        assert_eq!(levels[4].0, 50000.04);
    }

    #[test]
    fn test_generate_market_message() {
        let message = test_data::generate_market_message("BTCUSDT", 123);
        assert!(message.contains("BTCUSDT"));
        assert!(message.contains("books5"));
    }
}