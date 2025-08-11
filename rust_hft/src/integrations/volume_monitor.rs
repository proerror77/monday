/*!
 * Symbol Volume Monitor - 商品數據量監控器
 * 
 * 實時監控每個交易對的消息流量，為動態連接分配提供數據支持
 */

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// 時間戳類型
pub type Timestamp = u64;

/// 商品流量統計
#[derive(Debug, Clone)]
pub struct SymbolTrafficStats {
    pub symbol: String,
    pub message_count: u64,
    pub messages_per_second: f64,
    pub average_message_size: f64,
    pub peak_rate: f64,
    pub last_update: Instant,
}

/// 商品數據量監控器
#[derive(Debug)]
pub struct SymbolVolumeMonitor {
    /// 滑動窗口內的消息計數 (symbol -> timestamps)
    message_windows: Arc<RwLock<HashMap<String, VecDeque<Instant>>>>,
    
    /// 統計窗口大小
    window_duration: Duration,
    
    /// 累計統計信息
    cumulative_stats: Arc<RwLock<HashMap<String, SymbolTrafficStats>>>,
    
    /// 監控開始時間
    start_time: Instant,
}

impl SymbolVolumeMonitor {
    /// 創建新的監控器
    pub fn new(window_duration: Duration) -> Self {
        Self {
            message_windows: Arc::new(RwLock::new(HashMap::new())),
            window_duration,
            cumulative_stats: Arc::new(RwLock::new(HashMap::new())),
            start_time: Instant::now(),
        }
    }
    
    /// 記錄消息
    pub async fn record_message(&self, symbol: &str, message_size: usize) {
        let now = Instant::now();
        
        // 更新滑動窗口
        {
            let mut windows = self.message_windows.write().await;
            let window = windows.entry(symbol.to_string()).or_insert_with(VecDeque::new);
            
            // 添加當前時間戳
            window.push_back(now);
            
            // 清理過期的時間戳
            let cutoff = now - self.window_duration;
            while let Some(&front_time) = window.front() {
                if front_time < cutoff {
                    window.pop_front();
                } else {
                    break;
                }
            }
        }
        
        // 更新累計統計
        {
            let mut stats = self.cumulative_stats.write().await;
            let symbol_stats = stats.entry(symbol.to_string()).or_insert_with(|| {
                SymbolTrafficStats {
                    symbol: symbol.to_string(),
                    message_count: 0,
                    messages_per_second: 0.0,
                    average_message_size: 0.0,
                    peak_rate: 0.0,
                    last_update: now,
                }
            });
            
            // 更新計數和平均大小
            let prev_count = symbol_stats.message_count;
            let prev_avg_size = symbol_stats.average_message_size;
            
            symbol_stats.message_count += 1;
            symbol_stats.average_message_size = 
                (prev_avg_size * prev_count as f64 + message_size as f64) / symbol_stats.message_count as f64;
            symbol_stats.last_update = now;
            
            // 計算當前速率
            let current_rate = self.calculate_current_rate_internal(symbol).await;
            symbol_stats.messages_per_second = current_rate;
            
            // 更新峰值速率
            if current_rate > symbol_stats.peak_rate {
                symbol_stats.peak_rate = current_rate;
            }
        }
    }
    
    /// 獲取符號的當前消息速率 (msg/s)
    pub async fn get_current_rate(&self, symbol: &str) -> f64 {
        self.calculate_current_rate_internal(symbol).await
    }
    
    /// 內部計算當前速率
    async fn calculate_current_rate_internal(&self, symbol: &str) -> f64 {
        let windows = self.message_windows.read().await;
        if let Some(window) = windows.get(symbol) {
            if window.is_empty() {
                return 0.0;
            }
            
            let count = window.len() as f64;
            let duration_secs = self.window_duration.as_secs_f64();
            count / duration_secs
        } else {
            0.0
        }
    }
    
    /// 獲取所有符號的統計信息
    pub async fn get_all_stats(&self) -> HashMap<String, SymbolTrafficStats> {
        let stats = self.cumulative_stats.read().await;
        stats.clone()
    }
    
    /// 獲取指定符號的統計信息
    pub async fn get_symbol_stats(&self, symbol: &str) -> Option<SymbolTrafficStats> {
        let stats = self.cumulative_stats.read().await;
        stats.get(symbol).cloned()
    }
    
    /// 獲取符號按流量排序的列表
    pub async fn get_symbols_by_traffic(&self) -> Vec<(String, f64)> {
        let stats = self.cumulative_stats.read().await;
        let mut symbol_rates: Vec<(String, f64)> = stats
            .iter()
            .map(|(symbol, stats)| (symbol.clone(), stats.messages_per_second))
            .collect();
        
        // 按消息速率降序排序
        symbol_rates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        symbol_rates
    }
    
    /// 計算流量分組
    pub async fn calculate_traffic_groups(
        &self,
        high_threshold: f64,
        medium_threshold: f64,
    ) -> (Vec<String>, Vec<String>, Vec<String>) {
        let symbol_rates = self.get_symbols_by_traffic().await;
        
        let mut high_traffic = Vec::new();
        let mut medium_traffic = Vec::new();
        let mut low_traffic = Vec::new();
        
        for (symbol, rate) in symbol_rates {
            if rate >= high_threshold {
                high_traffic.push(symbol);
            } else if rate >= medium_threshold {
                medium_traffic.push(symbol);
            } else {
                low_traffic.push(symbol);
            }
        }
        
        (high_traffic, medium_traffic, low_traffic)
    }
    
    /// 打印流量統計報告
    pub async fn print_traffic_report(&self) {
        let stats = self.get_all_stats().await;
        let total_runtime = self.start_time.elapsed().as_secs_f64();
        
        info!("=== 商品流量統計報告 ===");
        info!("監控時間: {:.1} 分鐘", total_runtime / 60.0);
        
        let mut sorted_stats: Vec<_> = stats.values().collect();
        sorted_stats.sort_by(|a, b| b.messages_per_second.partial_cmp(&a.messages_per_second).unwrap_or(std::cmp::Ordering::Equal));
        
        for stat in sorted_stats {
            info!(
                "📊 {}: {:.1} msg/s | 總數: {} | 峰值: {:.1} msg/s | 平均大小: {:.1}B",
                stat.symbol,
                stat.messages_per_second,
                stat.message_count,
                stat.peak_rate,
                stat.average_message_size
            );
        }
        
        // 計算建議的分組
        let (high, medium, low) = self.calculate_traffic_groups(10.0, 5.0).await;
        info!("💡 建議連接分組:");
        info!("   🔴 高流量 (>10 msg/s): {:?}", high);
        info!("   🟡 中流量 (5-10 msg/s): {:?}", medium);
        info!("   🟢 低流量 (<5 msg/s): {:?}", low);
        info!("========================");
    }
    
    /// 清理過期數據
    pub async fn cleanup_expired_data(&self) {
        let now = Instant::now();
        let cutoff = now - self.window_duration;
        
        let mut windows = self.message_windows.write().await;
        for window in windows.values_mut() {
            while let Some(&front_time) = window.front() {
                if front_time < cutoff {
                    window.pop_front();
                } else {
                    break;
                }
            }
        }
    }
}

/// 動態負載均衡器
#[derive(Debug)]
pub struct DynamicLoadBalancer {
    monitor: Arc<SymbolVolumeMonitor>,
    connection_loads: Vec<f64>,
    target_load: f64,
    rebalance_threshold: f64,
    last_rebalance: Instant,
    rebalance_interval: Duration,
}

impl DynamicLoadBalancer {
    /// 創建新的負載均衡器
    pub fn new(
        monitor: Arc<SymbolVolumeMonitor>,
        num_connections: usize,
        target_load: f64,
        rebalance_threshold: f64,
        rebalance_interval: Duration,
    ) -> Self {
        Self {
            monitor,
            connection_loads: vec![0.0; num_connections],
            target_load,
            rebalance_threshold,
            last_rebalance: Instant::now(),
            rebalance_interval,
        }
    }
    
    /// 檢查是否需要重平衡
    pub async fn should_rebalance(&self) -> bool {
        // 檢查時間間隔
        if self.last_rebalance.elapsed() < self.rebalance_interval {
            return false;
        }
        
        // 檢查負載不平衡
        let max_load = self.connection_loads.iter().fold(0.0f64, |a, &b| a.max(b));
        let min_load = self.connection_loads.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        
        (max_load - min_load) > self.rebalance_threshold
    }
    
    /// 計算最佳符號分配
    pub async fn calculate_optimal_distribution(&self, symbols: &[String]) -> HashMap<String, usize> {
        let mut distribution = HashMap::new();
        let mut loads = vec![0.0; self.connection_loads.len()];
        
        // 獲取符號的流量信息並按流量降序排序
        let mut symbol_rates = Vec::new();
        for symbol in symbols {
            let rate = self.monitor.get_current_rate(symbol).await;
            symbol_rates.push((symbol.clone(), rate));
        }
        symbol_rates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        
        // 使用貪心算法分配符號到負載最小的連接
        for (symbol, rate) in symbol_rates {
            let min_load_idx = loads
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            
            distribution.insert(symbol, min_load_idx);
            loads[min_load_idx] += rate;
        }
        
        distribution
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;
    
    #[tokio::test]
    async fn test_volume_monitor() {
        let monitor = SymbolVolumeMonitor::new(Duration::from_secs(5));
        
        // 模擬不同符號的消息
        monitor.record_message("BTCUSDT", 100).await;
        monitor.record_message("ETHUSDT", 120).await;
        monitor.record_message("ETHUSDT", 110).await;
        
        sleep(Duration::from_millis(100)).await;
        
        let btc_rate = monitor.get_current_rate("BTCUSDT").await;
        let eth_rate = monitor.get_current_rate("ETHUSDT").await;
        
        assert!(btc_rate > 0.0);
        assert!(eth_rate > btc_rate); // ETH should have higher rate
        
        let stats = monitor.get_all_stats().await;
        assert_eq!(stats.len(), 2);
        assert!(stats.contains_key("BTCUSDT"));
        assert!(stats.contains_key("ETHUSDT"));
    }
    
    #[tokio::test]
    async fn test_traffic_groups() {
        let monitor = SymbolVolumeMonitor::new(Duration::from_secs(1));
        
        // 模擬不同流量的符號
        for _ in 0..20 {
            monitor.record_message("HIGH_TRAFFIC", 100).await;
        }
        for _ in 0..10 {
            monitor.record_message("MEDIUM_TRAFFIC", 100).await;
        }
        for _ in 0..2 {
            monitor.record_message("LOW_TRAFFIC", 100).await;
        }
        
        sleep(Duration::from_millis(100)).await;
        
        let (high, medium, low) = monitor.calculate_traffic_groups(15.0, 5.0).await;
        
        assert!(high.contains(&"HIGH_TRAFFIC".to_string()));
        assert!(medium.contains(&"MEDIUM_TRAFFIC".to_string()));
        assert!(low.contains(&"LOW_TRAFFIC".to_string()));
    }
}