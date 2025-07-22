/*!
 * 通用測試工具和輔助函數
 */

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

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