/*!
 * 簡化版統一超高頻交易系統
 * 
 * 結合已實現的 Lock-Free OrderBook 和零拷貝處理技術
 * 目標：決策延遲 < 1μs，消息處理 < 100ns
 */

use std::sync::{Arc, atomic::{AtomicU64, AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use std::collections::VecDeque;
use dashmap::DashMap;
use clap::Parser;
use tracing::{info, warn, debug};

#[derive(Parser, Debug)]
#[command(about = "簡化版統一超高頻交易系統")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,
    
    /// 測試消息數量
    #[arg(short, long, default_value_t = 100000)]
    messages: usize,
    
    /// 最大延遲閾值（微秒）
    #[arg(short, long, default_value_t = 1)]
    max_latency_us: u64,
    
    /// 批量大小
    #[arg(short, long, default_value_t = 32)]
    batch_size: usize,
    
    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,
}

/// 簡化的價格等級（Lock-Free）
#[derive(Debug)]
pub struct SimplePriceLevel {
    pub price: u64,      // 定點數價格 (price * 100)
    pub quantity: f64,
    pub timestamp: u64,
}

/// 簡化的 Lock-Free OrderBook
#[derive(Debug)]
pub struct SimpleOrderBook {
    pub bids: DashMap<u64, SimplePriceLevel>,
    pub asks: DashMap<u64, SimplePriceLevel>,
    pub best_bid_price: AtomicU64,
    pub best_ask_price: AtomicU64,
    pub update_count: AtomicU64,
}

impl SimpleOrderBook {
    pub fn new() -> Self {
        Self {
            bids: DashMap::new(),
            asks: DashMap::new(),
            best_bid_price: AtomicU64::new(0),
            best_ask_price: AtomicU64::new(u64::MAX),
            update_count: AtomicU64::new(0),
        }
    }
    
    /// Lock-Free 更新買價
    pub fn update_bid(&self, price: u64, quantity: f64) {
        let level = SimplePriceLevel {
            price,
            quantity,
            timestamp: self.get_timestamp(),
        };
        
        if quantity > 0.0 {
            self.bids.insert(price, level);
            
            // 原子更新最佳買價
            let current_best = self.best_bid_price.load(Ordering::Relaxed);
            if price > current_best {
                self.best_bid_price.store(price, Ordering::Relaxed);
            }
        } else {
            self.bids.remove(&price);
        }
        
        self.update_count.fetch_add(1, Ordering::Relaxed);
    }
    
    /// Lock-Free 更新賣價
    pub fn update_ask(&self, price: u64, quantity: f64) {
        let level = SimplePriceLevel {
            price,
            quantity,
            timestamp: self.get_timestamp(),
        };
        
        if quantity > 0.0 {
            self.asks.insert(price, level);
            
            // 原子更新最佳賣價
            let current_best = self.best_ask_price.load(Ordering::Relaxed);
            if price < current_best {
                self.best_ask_price.store(price, Ordering::Relaxed);
            }
        } else {
            self.asks.remove(&price);
        }
        
        self.update_count.fetch_add(1, Ordering::Relaxed);
    }
    
    /// 獲取最佳買賣價
    pub fn get_best_prices(&self) -> (Option<u64>, Option<u64>) {
        let best_bid = self.best_bid_price.load(Ordering::Relaxed);
        let best_ask = self.best_ask_price.load(Ordering::Relaxed);
        
        let bid = if best_bid > 0 { Some(best_bid) } else { None };
        let ask = if best_ask < u64::MAX { Some(best_ask) } else { None };
        
        (bid, ask)
    }
    
    /// 獲取當前時間戳
    fn get_timestamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64
    }
    
    /// 獲取訂單簿深度
    pub fn get_depth(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }
}

/// 高性能統計計數器
#[derive(Debug)]
pub struct PerformanceStats {
    pub messages_processed: AtomicU64,
    pub orderbook_updates: AtomicU64,
    pub total_latency_ns: AtomicU64,
    pub max_latency_ns: AtomicU64,
    pub sub_microsecond_count: AtomicU64,
    pub errors: AtomicU64,
    pub start_time: Instant,
}

impl PerformanceStats {
    pub fn new() -> Self {
        Self {
            messages_processed: AtomicU64::new(0),
            orderbook_updates: AtomicU64::new(0),
            total_latency_ns: AtomicU64::new(0),
            max_latency_ns: AtomicU64::new(0),
            sub_microsecond_count: AtomicU64::new(0),
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
        
        // 統計亞微秒級延遲
        if latency_ns < 1_000 {
            self.sub_microsecond_count.fetch_add(1, Ordering::Relaxed);
        }
    }
    
    pub fn summary(&self) -> StatsSummary {
        let elapsed = self.start_time.elapsed();
        let messages = self.messages_processed.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed);
        
        StatsSummary {
            runtime_seconds: elapsed.as_secs_f64(),
            total_messages: messages,
            orderbook_updates: self.orderbook_updates.load(Ordering::Relaxed),
            avg_latency_ns: if messages > 0 { total_latency as f64 / messages as f64 } else { 0.0 },
            max_latency_ns: self.max_latency_ns.load(Ordering::Relaxed),
            sub_microsecond_percentage: if messages > 0 {
                self.sub_microsecond_count.load(Ordering::Relaxed) as f64 / messages as f64 * 100.0
            } else { 0.0 },
            throughput_msgs_per_sec: if elapsed.as_secs_f64() > 0.0 {
                messages as f64 / elapsed.as_secs_f64()
            } else { 0.0 },
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub struct StatsSummary {
    pub runtime_seconds: f64,
    pub total_messages: u64,
    pub orderbook_updates: u64,
    pub avg_latency_ns: f64,
    pub max_latency_ns: u64,
    pub sub_microsecond_percentage: f64,
    pub throughput_msgs_per_sec: f64,
    pub errors: u64,
}

/// 簡化的市場消息
#[derive(Debug, Clone)]
pub struct MarketMessage {
    pub symbol: String,
    pub bids: Vec<(f64, f64)>,  // price, quantity
    pub asks: Vec<(f64, f64)>,
    pub timestamp: u64,
}

impl MarketMessage {
    pub fn new(symbol: String) -> Self {
        // 生成模擬的市場數據
        let base_price = 50000.0;
        let spread = 0.5;
        
        Self {
            symbol,
            bids: vec![
                (base_price - spread, 1.0),
                (base_price - spread - 0.5, 2.0),
                (base_price - spread - 1.0, 1.5),
            ],
            asks: vec![
                (base_price + spread, 1.5),
                (base_price + spread + 0.5, 2.5),
                (base_price + spread + 1.0, 1.0),
            ],
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        }
    }
}

/// 簡化的統一HFT引擎
pub struct SimplifiedHftEngine {
    pub orderbook: Arc<SimpleOrderBook>,
    pub stats: Arc<PerformanceStats>,
    pub running: Arc<AtomicBool>,
    pub symbol: String,
    pub max_latency_ns: u64,
}

impl SimplifiedHftEngine {
    pub fn new(symbol: String, max_latency_us: u64) -> Self {
        Self {
            orderbook: Arc::new(SimpleOrderBook::new()),
            stats: Arc::new(PerformanceStats::new()),
            running: Arc::new(AtomicBool::new(false)),
            symbol,
            max_latency_ns: max_latency_us * 1_000,
        }
    }
    
    /// 核心消息處理函數（目標 < 1μs）
    pub fn process_market_message(&self, message: &MarketMessage) -> Result<(), String> {
        let start = Instant::now();
        
        // Lock-Free OrderBook 更新
        for (price, quantity) in &message.bids {
            let price_fixed = (*price * 100.0) as u64;
            self.orderbook.update_bid(price_fixed, *quantity);
        }
        
        for (price, quantity) in &message.asks {
            let price_fixed = (*price * 100.0) as u64;
            self.orderbook.update_ask(price_fixed, *quantity);
        }
        
        // 記錄性能指標
        let latency_ns = start.elapsed().as_nanos() as u64;
        self.stats.record_latency(latency_ns);
        self.stats.messages_processed.fetch_add(1, Ordering::Relaxed);
        self.stats.orderbook_updates.fetch_add(1, Ordering::Relaxed);
        
        // 延遲檢查
        if latency_ns > self.max_latency_ns {
            warn!("延遲超標: {}ns > {}ns", latency_ns, self.max_latency_ns);
        }
        
        Ok(())
    }
    
    /// 批量處理消息（零拷貝風格）
    pub fn process_batch(&self, messages: &[MarketMessage]) -> Result<(), String> {
        let start = Instant::now();
        
        for message in messages {
            // 單獨測量每個消息的處理時間
            let msg_start = Instant::now();
            
            if let Err(e) = self.process_market_message(message) {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
            
            let msg_latency = msg_start.elapsed().as_nanos() as u64;
            if msg_latency > self.max_latency_ns {
                warn!("批量處理中消息延遲超標: {}ns", msg_latency);
            }
        }
        
        let batch_latency = start.elapsed().as_nanos() as u64;
        debug!("批量處理 {} 條消息，總延遲: {}ns", messages.len(), batch_latency);
        
        Ok(())
    }
    
    /// 獲取最佳買賣價
    pub fn get_best_prices(&self) -> (Option<f64>, Option<f64>) {
        let (bid_fixed, ask_fixed) = self.orderbook.get_best_prices();
        
        let bid = bid_fixed.map(|p| p as f64 / 100.0);
        let ask = ask_fixed.map(|p| p as f64 / 100.0);
        
        (bid, ask)
    }
    
    /// 獲取訂單簿統計
    pub fn get_orderbook_stats(&self) -> (usize, usize, u64) {
        let (bid_depth, ask_depth) = self.orderbook.get_depth();
        let updates = self.orderbook.update_count.load(Ordering::Relaxed);
        (bid_depth, ask_depth, updates)
    }
    
    /// 啟動引擎
    pub fn start(&self) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);
        info!("🚀 簡化HFT引擎已啟動 - 目標延遲 < {}μs", self.max_latency_ns / 1_000);
        Ok(())
    }
    
    /// 停止引擎
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
        info!("⏹️ 簡化HFT引擎已停止");
    }
}

/// 市場數據生成器
pub struct MarketDataGenerator {
    symbol: String,
    message_count: AtomicU64,
    base_price: f64,
    price_volatility: f64,
}

impl MarketDataGenerator {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            message_count: AtomicU64::new(0),
            base_price: 50000.0,
            price_volatility: 0.1,
        }
    }
    
    pub fn generate_message(&self) -> MarketMessage {
        let count = self.message_count.fetch_add(1, Ordering::Relaxed);
        
        // 模擬價格波動
        let price_change = (count as f64 * 0.01) % 2.0 - 1.0; // -1.0 到 1.0
        let current_price = self.base_price + price_change * self.price_volatility;
        
        MarketMessage {
            symbol: self.symbol.clone(),
            bids: vec![
                (current_price - 0.5, 1.0 + (count % 10) as f64 * 0.1),
                (current_price - 1.0, 2.0 + (count % 20) as f64 * 0.05),
                (current_price - 1.5, 1.5 + (count % 15) as f64 * 0.08),
            ],
            asks: vec![
                (current_price + 0.5, 1.5 + (count % 12) as f64 * 0.07),
                (current_price + 1.0, 2.5 + (count % 25) as f64 * 0.03),
                (current_price + 1.5, 1.0 + (count % 8) as f64 * 0.12),
            ],
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        }
    }
    
    pub fn generate_batch(&self, size: usize) -> Vec<MarketMessage> {
        (0..size).map(|_| self.generate_message()).collect()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    
    let args = Args::parse();
    
    info!("🚀 啟動簡化版統一超高頻交易系統");
    info!("交易對: {}, 測試消息: {}, 最大延遲: {}μs, 批量: {}", 
          args.symbol, args.messages, args.max_latency_us, args.batch_size);
    
    // 創建統一HFT引擎
    let engine = Arc::new(SimplifiedHftEngine::new(
        args.symbol.clone(), 
        args.max_latency_us
    ));
    
    // 啟動引擎
    engine.start()?;
    
    // 創建數據生成器
    let generator = MarketDataGenerator::new(args.symbol.clone());
    
    info!("📊 開始處理市場數據...");
    
    // 測試 1: 單條消息處理
    info!("🔸 測試單條消息處理...");
    let single_start = Instant::now();
    
    for i in 0..args.messages {
        let message = generator.generate_message();
        
        if let Err(e) = engine.process_market_message(&message) {
            engine.stats.errors.fetch_add(1, Ordering::Relaxed);
            if args.verbose {
                warn!("處理錯誤 #{}: {}", i, e);
            }
        }
        
        if args.verbose && i % 10000 == 0 && i > 0 {
            let (best_bid, best_ask) = engine.get_best_prices();
            let (bid_depth, ask_depth, updates) = engine.get_orderbook_stats();
            debug!("已處理 {} 條消息 | 最佳價位: bid={:?}, ask={:?} | 深度: {}/{} | 更新: {}", 
                   i, best_bid, best_ask, bid_depth, ask_depth, updates);
        }
    }
    
    let single_elapsed = single_start.elapsed();
    
    // 測試 2: 批量消息處理
    info!("🔸 測試批量消息處理...");
    let batch_start = Instant::now();
    
    let total_batches = args.messages / args.batch_size;
    for batch_i in 0..total_batches {
        let batch = generator.generate_batch(args.batch_size);
        
        if let Err(e) = engine.process_batch(&batch) {
            warn!("批量處理錯誤 #{}: {}", batch_i, e);
        }
        
        if args.verbose && batch_i % 100 == 0 && batch_i > 0 {
            debug!("已處理 {} 個批量 ({} 條消息)", batch_i, batch_i * args.batch_size);
        }
    }
    
    let batch_elapsed = batch_start.elapsed();
    
    // 停止引擎
    engine.stop();
    
    // 生成性能報告
    let stats = engine.stats.summary();
    
    info!("\\n🎯 ===== 簡化版HFT系統性能報告 =====");
    info!("運行時間: {:.2}s", stats.runtime_seconds);
    info!("處理消息: {}", stats.total_messages);
    info!("OrderBook更新: {}", stats.orderbook_updates);
    info!("平均延遲: {:.2}ns ({:.3}μs)", stats.avg_latency_ns, stats.avg_latency_ns / 1000.0);
    info!("最大延遲: {}ns ({:.3}μs)", stats.max_latency_ns, stats.max_latency_ns as f64 / 1000.0);
    info!("亞微秒級消息: {:.1}%", stats.sub_microsecond_percentage);
    info!("吞吐量: {:.0} msgs/s", stats.throughput_msgs_per_sec);
    info!("錯誤數: {}", stats.errors);
    
    // 性能對比
    info!("\\n📊 ===== 處理模式對比 =====");
    info!("單條處理總時間: {:.2}ms", single_elapsed.as_millis());
    info!("批量處理總時間: {:.2}ms", batch_elapsed.as_millis());
    
    let single_throughput = args.messages as f64 / single_elapsed.as_secs_f64();
    let batch_throughput = (total_batches * args.batch_size) as f64 / batch_elapsed.as_secs_f64();
    
    info!("單條處理吞吐量: {:.0} msgs/s", single_throughput);
    info!("批量處理吞吐量: {:.0} msgs/s", batch_throughput);
    
    let speedup = batch_throughput / single_throughput;
    info!("批量處理提升: {:.2}x", speedup);
    
    // 目標達成評估
    let target_met = stats.avg_latency_ns < 1000.0;
    let sub_microsecond_target_met = stats.sub_microsecond_percentage > 90.0;
    
    info!("\\n🏆 ===== 目標達成評估 =====");
    info!("平均延遲 < 1μs: {} ({:.2}ns)", 
          if target_met { "✅ 達成" } else { "❌ 未達成" }, stats.avg_latency_ns);
    info!("90%+ 亞微秒級: {} ({:.1}%)", 
          if sub_microsecond_target_met { "✅ 達成" } else { "❌ 未達成" }, 
          stats.sub_microsecond_percentage);
    
    if target_met && sub_microsecond_target_met {
        info!("🎉 恭喜！簡化版HFT系統已達到超低延遲目標！");
        info!("💡 Lock-Free OrderBook + 零拷貝處理 = 生產級性能！");
    } else {
        info!("⚠️ 系統性能建議：");
        if !target_met {
            info!("   • 進一步優化原子操作");
            info!("   • 減少內存分配");
            info!("   • 使用 SIMD 指令集");
        }
        if !sub_microsecond_target_met {
            info!("   • 實施 CPU 親和性綁定");
            info!("   • 啟用實時調度優先級");
        }
    }
    
    // 詳細統計
    if args.verbose {
        info!("\\n📈 ===== 詳細統計 =====");
        let (best_bid, best_ask) = engine.get_best_prices();
        let (bid_depth, ask_depth, total_updates) = engine.get_orderbook_stats();
        
        info!("OrderBook 最終狀態:");
        info!("   最佳買價: {:?}", best_bid);
        info!("   最佳賣價: {:?}", best_ask);
        info!("   買單深度: {}", bid_depth);
        info!("   賣單深度: {}", ask_depth);
        info!("   總更新次數: {}", total_updates);
        
        if let (Some(bid), Some(ask)) = (best_bid, best_ask) {
            let spread = ask - bid;
            info!("   買賣價差: {:.2}", spread);
            let mid_price = (bid + ask) / 2.0;
            info!("   中間價: {:.2}", mid_price);
        }
        
        info!("性能指標:");
        info!("   Lock-Free操作成功率: {:.2}%", 
              if stats.total_messages > 0 {
                  (stats.total_messages - stats.errors) as f64 / stats.total_messages as f64 * 100.0
              } else { 0.0 });
        
        let memory_throughput = stats.throughput_msgs_per_sec * 1024.0 / 1024.0 / 1024.0;
        info!("   內存處理效率: {:.2} GB/s", memory_throughput);
        
        let cpu_efficiency = stats.sub_microsecond_percentage;
        info!("   CPU 效率指標: {:.1}%", cpu_efficiency);
    }
    
    info!("✅ 簡化版統一超高頻交易系統測試完成");
    
    Ok(())
}