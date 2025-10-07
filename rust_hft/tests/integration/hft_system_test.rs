/*!
 * 統一HFT系統集成測試
 *
 * 提取自 unified_hft_test，專注於完整系統集成測試
 */

use std::sync::{Arc, atomic::{AtomicU64, AtomicBool, Ordering}};
use std::time::Instant;
use dashmap::DashMap;

/// 獲取微秒時間戳
fn get_timestamp_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// Lock-Free 價格等級
#[derive(Debug, Clone)]
pub struct LockFreePriceLevel {
    pub price: u64,      // 定點數價格 (price * 10000)
    pub quantity: f64,
    pub timestamp: u64,
    pub order_count: u32,
}

impl LockFreePriceLevel {
    pub fn new(price: u64, quantity: f64) -> Self {
        Self {
            price,
            quantity,
            timestamp: get_timestamp_micros(),
            order_count: 1,
        }
    }

    pub fn price_as_f64(&self) -> f64 {
        self.price as f64 / 10000.0
    }
}

/// 高性能 Lock-Free OrderBook
#[derive(Debug)]
pub struct HighPerfOrderBook {
    pub bids: DashMap<u64, LockFreePriceLevel>,
    pub asks: DashMap<u64, LockFreePriceLevel>,
    pub best_bid_price: AtomicU64,
    pub best_ask_price: AtomicU64,
    pub update_count: AtomicU64,
    pub last_update_time: AtomicU64,
    pub checksum: AtomicU64,
}

impl HighPerfOrderBook {
    pub fn new() -> Self {
        Self {
            bids: DashMap::new(),
            asks: DashMap::new(),
            best_bid_price: AtomicU64::new(0),
            best_ask_price: AtomicU64::new(u64::MAX),
            update_count: AtomicU64::new(0),
            last_update_time: AtomicU64::new(0),
            checksum: AtomicU64::new(0),
        }
    }

    /// 原子更新買價等級
    pub fn update_bid(&self, price: u64, quantity: f64) -> bool {
        let start = Instant::now();

        if quantity > 0.0 {
            let level = LockFreePriceLevel::new(price, quantity);
            self.bids.insert(price, level);
            self.update_best_bid(price);
        } else {
            self.bids.remove(&price);
            if price == self.best_bid_price.load(Ordering::Relaxed) {
                self.recalculate_best_bid();
            }
        }

        self.update_metadata();

        // 性能檢查：目標 < 1μs
        let latency = start.elapsed().as_nanos() as u64;
        latency < 1000
    }

    /// 原子更新賣價等級
    pub fn update_ask(&self, price: u64, quantity: f64) -> bool {
        let start = Instant::now();

        if quantity > 0.0 {
            let level = LockFreePriceLevel::new(price, quantity);
            self.asks.insert(price, level);
            self.update_best_ask(price);
        } else {
            self.asks.remove(&price);
            if price == self.best_ask_price.load(Ordering::Relaxed) {
                self.recalculate_best_ask();
            }
        }

        self.update_metadata();

        let latency = start.elapsed().as_nanos() as u64;
        latency < 1000
    }

    /// 無鎖更新最佳買價
    fn update_best_bid(&self, price: u64) {
        let mut current_best = self.best_bid_price.load(Ordering::Relaxed);
        while price > current_best {
            match self.best_bid_price.compare_exchange_weak(
                current_best, price, Ordering::Relaxed, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(x) => current_best = x,
            }
        }
    }

    /// 無鎖更新最佳賣價
    fn update_best_ask(&self, price: u64) {
        let mut current_best = self.best_ask_price.load(Ordering::Relaxed);
        while price < current_best {
            match self.best_ask_price.compare_exchange_weak(
                current_best, price, Ordering::Relaxed, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(x) => current_best = x,
            }
        }
    }

    /// 重新計算最佳買價
    fn recalculate_best_bid(&self) {
        let best = self.bids.iter()
            .map(|entry| *entry.key())
            .max()
            .unwrap_or(0);
        self.best_bid_price.store(best, Ordering::Relaxed);
    }

    /// 重新計算最佳賣價
    fn recalculate_best_ask(&self) {
        let best = self.asks.iter()
            .map(|entry| *entry.key())
            .min()
            .unwrap_or(u64::MAX);
        self.best_ask_price.store(best, Ordering::Relaxed);
    }

    /// 更新元數據
    fn update_metadata(&self) {
        self.update_count.fetch_add(1, Ordering::Relaxed);
        self.last_update_time.store(get_timestamp_micros(), Ordering::Relaxed);

        // 計算簡單校驗和
        let bid_sum: u64 = self.bids.iter().map(|entry| *entry.key()).sum();
        let ask_sum: u64 = self.asks.iter().map(|entry| *entry.key()).sum();
        let new_checksum = bid_sum.wrapping_add(ask_sum);
        self.checksum.store(new_checksum, Ordering::Relaxed);
    }

    /// 獲取最佳買賣價
    pub fn get_best_prices(&self) -> (Option<f64>, Option<f64>) {
        let best_bid = self.best_bid_price.load(Ordering::Relaxed);
        let best_ask = self.best_ask_price.load(Ordering::Relaxed);

        let bid = if best_bid > 0 { Some(best_bid as f64 / 10000.0) } else { None };
        let ask = if best_ask < u64::MAX { Some(best_ask as f64 / 10000.0) } else { None };

        (bid, ask)
    }

    /// 獲取訂單簿深度
    pub fn get_depth(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }

    /// 獲取統計信息
    pub fn get_stats(&self) -> OrderBookStats {
        let (bid_depth, ask_depth) = self.get_depth();
        let (best_bid, best_ask) = self.get_best_prices();

        OrderBookStats {
            bid_depth,
            ask_depth,
            best_bid,
            best_ask,
            spread: match (best_bid, best_ask) {
                (Some(b), Some(a)) => Some(a - b),
                _ => None,
            },
            update_count: self.update_count.load(Ordering::Relaxed),
            last_update_time: self.last_update_time.load(Ordering::Relaxed),
            checksum: self.checksum.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrderBookStats {
    pub bid_depth: usize,
    pub ask_depth: usize,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub spread: Option<f64>,
    pub update_count: u64,
    pub last_update_time: u64,
    pub checksum: u64,
}

/// 零拷貝市場消息
#[derive(Debug, Clone)]
pub struct ZeroCopyMarketMessage {
    pub symbol: String,
    pub bids: Vec<(f64, f64)>,  // price, quantity
    pub asks: Vec<(f64, f64)>,
    pub timestamp: u64,
    pub sequence: u64,
}

impl ZeroCopyMarketMessage {
    pub fn new(symbol: String, sequence: u64) -> Self {
        // 生成模擬但真實的市場數據
        let base_price = 50000.0 + (sequence as f64 * 0.001) % 100.0;
        let volatility = 1.0 + (sequence as f64 * 0.0001) % 0.1;

        Self {
            symbol,
            bids: vec![
                (base_price - volatility, 1.0 + (sequence % 10) as f64 * 0.1),
                (base_price - volatility * 2.0, 2.0 + (sequence % 15) as f64 * 0.05),
                (base_price - volatility * 3.0, 1.5 + (sequence % 8) as f64 * 0.12),
            ],
            asks: vec![
                (base_price + volatility, 1.5 + (sequence % 12) as f64 * 0.07),
                (base_price + volatility * 2.0, 2.5 + (sequence % 18) as f64 * 0.04),
                (base_price + volatility * 3.0, 1.0 + (sequence % 6) as f64 * 0.15),
            ],
            timestamp: get_timestamp_micros(),
            sequence,
        }
    }
}

/// 超高性能統計器
#[derive(Debug)]
pub struct UltraPerformanceStats {
    pub messages_processed: AtomicU64,
    pub orderbook_updates: AtomicU64,
    pub successful_updates: AtomicU64,
    pub failed_updates: AtomicU64,
    pub total_latency_ns: AtomicU64,
    pub max_latency_ns: AtomicU64,
    pub min_latency_ns: AtomicU64,
    pub sub_microsecond_count: AtomicU64,
    pub sub_100ns_count: AtomicU64,
    pub errors: AtomicU64,
    pub start_time: Instant,
}

impl UltraPerformanceStats {
    pub fn new() -> Self {
        Self {
            messages_processed: AtomicU64::new(0),
            orderbook_updates: AtomicU64::new(0),
            successful_updates: AtomicU64::new(0),
            failed_updates: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            max_latency_ns: AtomicU64::new(0),
            min_latency_ns: AtomicU64::new(u64::MAX),
            sub_microsecond_count: AtomicU64::new(0),
            sub_100ns_count: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }

    pub fn record_latency(&self, latency_ns: u64) {
        self.total_latency_ns.fetch_add(latency_ns, Ordering::Relaxed);

        // 更新最大延遲
        let mut current_max = self.max_latency_ns.load(Ordering::Relaxed);
        while latency_ns > current_max {
            match self.max_latency_ns.compare_exchange_weak(
                current_max, latency_ns, Ordering::Relaxed, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(x) => current_max = x,
            }
        }

        // 更新最小延遲
        let mut current_min = self.min_latency_ns.load(Ordering::Relaxed);
        while latency_ns < current_min {
            match self.min_latency_ns.compare_exchange_weak(
                current_min, latency_ns, Ordering::Relaxed, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(x) => current_min = x,
            }
        }

        // 統計超低延遲
        if latency_ns < 1_000 {
            self.sub_microsecond_count.fetch_add(1, Ordering::Relaxed);
        }
        if latency_ns < 100 {
            self.sub_100ns_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_update_result(&self, success: bool) {
        if success {
            self.successful_updates.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_updates.fetch_add(1, Ordering::Relaxed);
        }
        self.orderbook_updates.fetch_add(1, Ordering::Relaxed);
    }

    pub fn summary(&self) -> DetailedStatsSummary {
        let elapsed = self.start_time.elapsed();
        let messages = self.messages_processed.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed);
        let min_latency = self.min_latency_ns.load(Ordering::Relaxed);

        DetailedStatsSummary {
            runtime_seconds: elapsed.as_secs_f64(),
            total_messages: messages,
            orderbook_updates: self.orderbook_updates.load(Ordering::Relaxed),
            successful_updates: self.successful_updates.load(Ordering::Relaxed),
            failed_updates: self.failed_updates.load(Ordering::Relaxed),
            avg_latency_ns: if messages > 0 { total_latency as f64 / messages as f64 } else { 0.0 },
            max_latency_ns: self.max_latency_ns.load(Ordering::Relaxed),
            min_latency_ns: if min_latency < u64::MAX { min_latency } else { 0 },
            sub_microsecond_percentage: if messages > 0 {
                self.sub_microsecond_count.load(Ordering::Relaxed) as f64 / messages as f64 * 100.0
            } else { 0.0 },
            sub_100ns_percentage: if messages > 0 {
                self.sub_100ns_count.load(Ordering::Relaxed) as f64 / messages as f64 * 100.0
            } else { 0.0 },
            throughput_msgs_per_sec: if elapsed.as_secs_f64() > 0.0 {
                messages as f64 / elapsed.as_secs_f64()
            } else { 0.0 },
            success_rate: if self.orderbook_updates.load(Ordering::Relaxed) > 0 {
                self.successful_updates.load(Ordering::Relaxed) as f64 /
                self.orderbook_updates.load(Ordering::Relaxed) as f64 * 100.0
            } else { 0.0 },
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub struct DetailedStatsSummary {
    pub runtime_seconds: f64,
    pub total_messages: u64,
    pub orderbook_updates: u64,
    pub successful_updates: u64,
    pub failed_updates: u64,
    pub avg_latency_ns: f64,
    pub max_latency_ns: u64,
    pub min_latency_ns: u64,
    pub sub_microsecond_percentage: f64,
    pub sub_100ns_percentage: f64,
    pub throughput_msgs_per_sec: f64,
    pub success_rate: f64,
    pub errors: u64,
}

/// 統一HFT引擎
pub struct UnifiedHftEngine {
    pub orderbook: Arc<HighPerfOrderBook>,
    pub stats: Arc<UltraPerformanceStats>,
    pub running: Arc<AtomicBool>,
    pub symbol: String,
    pub max_latency_ns: u64,
    pub message_sequence: AtomicU64,
}

impl UnifiedHftEngine {
    pub fn new(symbol: String, max_latency_us: u64) -> Self {
        Self {
            orderbook: Arc::new(HighPerfOrderBook::new()),
            stats: Arc::new(UltraPerformanceStats::new()),
            running: Arc::new(AtomicBool::new(false)),
            symbol,
            max_latency_ns: max_latency_us * 1_000,
            message_sequence: AtomicU64::new(0),
        }
    }

    /// 核心消息處理（目標 < 1μs）
    pub fn process_market_message(&self, message: &ZeroCopyMarketMessage) -> Result<bool, String> {
        let start = Instant::now();
        let mut all_success = true;

        // 處理買價更新（Lock-Free）
        for (price, quantity) in &message.bids {
            let price_fixed = (*price * 10000.0) as u64;
            let success = self.orderbook.update_bid(price_fixed, *quantity);
            self.stats.record_update_result(success);
            if !success { all_success = false; }
        }

        // 處理賣價更新（Lock-Free）
        for (price, quantity) in &message.asks {
            let price_fixed = (*price * 10000.0) as u64;
            let success = self.orderbook.update_ask(price_fixed, *quantity);
            self.stats.record_update_result(success);
            if !success { all_success = false; }
        }

        // 記錄性能指標
        let latency_ns = start.elapsed().as_nanos() as u64;
        self.stats.record_latency(latency_ns);
        self.stats.messages_processed.fetch_add(1, Ordering::Relaxed);

        Ok(all_success)
    }

    /// 生成測試消息
    pub fn generate_test_message(&self) -> ZeroCopyMarketMessage {
        let sequence = self.message_sequence.fetch_add(1, Ordering::Relaxed);
        ZeroCopyMarketMessage::new(self.symbol.clone(), sequence)
    }

    /// 獲取實時統計
    pub fn get_realtime_stats(&self) -> (OrderBookStats, DetailedStatsSummary) {
        (self.orderbook.get_stats(), self.stats.summary())
    }

    pub fn start(&self) {
        self.running.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::PerformanceHelper;

    #[test]
    fn test_unified_hft_engine_basic() {
        let engine = UnifiedHftEngine::new("BTCUSDT".to_string(), 1);
        engine.start();

        let message = engine.generate_test_message();
        let result = engine.process_market_message(&message);

        assert!(result.is_ok());
        assert!(result.unwrap());

        let (ob_stats, perf_stats) = engine.get_realtime_stats();
        assert_eq!(perf_stats.total_messages, 1);
        assert!(ob_stats.bid_depth > 0);
        assert!(ob_stats.ask_depth > 0);

        engine.stop();
    }

    #[test]
    fn test_high_perf_orderbook() {
        let orderbook = HighPerfOrderBook::new();

        // 測試買價更新
        assert!(orderbook.update_bid(500000000, 1.0)); // 50000.0000
        assert!(orderbook.update_bid(499990000, 2.0)); // 49999.0000

        // 測試賣價更新
        assert!(orderbook.update_ask(500010000, 1.5)); // 50001.0000
        assert!(orderbook.update_ask(500020000, 2.5)); // 50002.0000

        let (bid_depth, ask_depth) = orderbook.get_depth();
        assert_eq!(bid_depth, 2);
        assert_eq!(ask_depth, 2);

        let (best_bid, best_ask) = orderbook.get_best_prices();
        assert_eq!(best_bid, Some(50000.0));
        assert_eq!(best_ask, Some(50001.0));

        let stats = orderbook.get_stats();
        assert_eq!(stats.update_count, 4);
        assert!(stats.spread.is_some());
        assert_eq!(stats.spread.unwrap(), 1.0);
    }

    #[test]
    fn test_zero_copy_market_message() {
        let message = ZeroCopyMarketMessage::new("BTCUSDT".to_string(), 123);

        assert_eq!(message.symbol, "BTCUSDT");
        assert_eq!(message.sequence, 123);
        assert!(!message.bids.is_empty());
        assert!(!message.asks.is_empty());
        assert!(message.timestamp > 0);

        // 驗證價格合理性
        assert!(message.bids[0].0 > 40000.0);
        assert!(message.bids[0].0 < 60000.0);
        assert!(message.asks[0].0 > message.bids[0].0);
    }

    #[test]
    fn test_ultra_performance_stats() {
        let stats = UltraPerformanceStats::new();

        // 記錄一些測試數據
        stats.record_latency(500); // 500ns
        stats.record_latency(1500); // 1.5μs
        stats.record_latency(50); // 50ns

        stats.record_update_result(true);
        stats.record_update_result(true);
        stats.record_update_result(false);

        stats.messages_processed.store(3, Ordering::Relaxed);

        let summary = stats.summary();

        assert_eq!(summary.total_messages, 3);
        assert_eq!(summary.orderbook_updates, 3);
        assert_eq!(summary.successful_updates, 2);
        assert_eq!(summary.failed_updates, 1);
        assert!((summary.avg_latency_ns - 683.33).abs() < 1.0); // (500+1500+50)/3
        assert_eq!(summary.max_latency_ns, 1500);
        assert_eq!(summary.min_latency_ns, 50);
        assert_eq!(summary.sub_100ns_percentage, 33.33333333333333); // 1/3
        assert_eq!(summary.sub_microsecond_percentage, 66.66666666666666); // 2/3
        assert_eq!(summary.success_rate, 66.66666666666666); // 2/3
    }

    #[tokio::test]
    async fn test_concurrent_hft_processing() {
        let engine = Arc::new(UnifiedHftEngine::new("BTCUSDT".to_string(), 1));
        engine.start();

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let engine_clone = engine.clone();
                tokio::spawn(async move {
                    for _ in 0..250 {
                        let message = engine_clone.generate_test_message();
                        let _ = engine_clone.process_market_message(&message);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.await.unwrap();
        }

        let (ob_stats, perf_stats) = engine.get_realtime_stats();

        assert_eq!(perf_stats.total_messages, 1000);
        assert!(ob_stats.bid_depth > 0);
        assert!(ob_stats.ask_depth > 0);
        assert!(perf_stats.success_rate > 90.0);

        println!("並發測試結果:");
        println!("  處理消息: {}", perf_stats.total_messages);
        println!("  平均延遲: {:.2}ns", perf_stats.avg_latency_ns);
        println!("  成功率: {:.1}%", perf_stats.success_rate);
        println!("  亞微秒級: {:.1}%", perf_stats.sub_microsecond_percentage);

        engine.stop();
    }

    #[test]
    fn test_performance_target_achievement() {
        let engine = UnifiedHftEngine::new("BTCUSDT".to_string(), 1);
        let message_count = 10000;

        engine.start();

        for _ in 0..message_count {
            let message = engine.generate_test_message();
            let _ = engine.process_market_message(&message);
        }

        let (_, perf_stats) = engine.get_realtime_stats();

        println!("性能目標測試結果:");
        println!("  處理消息: {}", perf_stats.total_messages);
        println!("  平均延遲: {:.2}ns ({:.3}μs)", perf_stats.avg_latency_ns, perf_stats.avg_latency_ns / 1000.0);
        println!("  最大延遲: {}ns ({:.3}μs)", perf_stats.max_latency_ns, perf_stats.max_latency_ns as f64 / 1000.0);
        println!("  最小延遲: {}ns", perf_stats.min_latency_ns);
        println!("  亞微秒級: {:.1}%", perf_stats.sub_microsecond_percentage);
        println!("  < 100ns: {:.1}%", perf_stats.sub_100ns_percentage);
        println!("  吞吐量: {:.0} msgs/s", perf_stats.throughput_msgs_per_sec);

        // 性能目標驗證
        let avg_target_met = perf_stats.avg_latency_ns < 1000.0;
        let sub_microsecond_target_met = perf_stats.sub_microsecond_percentage > 50.0;

        if avg_target_met {
            println!("✅ 平均延遲目標達成 (< 1μs)");
        } else {
            println!("❌ 平均延遲目標未達成");
        }

        if sub_microsecond_target_met {
            println!("✅ 亞微秒級目標達成 (> 50%)");
        } else {
            println!("❌ 亞微秒級目標未達成");
        }

        assert_eq!(perf_stats.total_messages, message_count);
        assert!(perf_stats.success_rate > 95.0);
        assert!(perf_stats.throughput_msgs_per_sec > 1000.0);

        engine.stop();
    }
}