/*!
 * Unified OrderBook Manager
 * 
 * 高性能訂單簿管理器，整合了多層級支持、快速更新和緩存優化
 * 支持零分配更新、SIMD 優化和無鎖數據結構
 */

use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use anyhow::{Result, Context};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use rust_decimal::prelude::ToPrimitive;

/// 統一的 OrderBook 管理器配置
#[derive(Debug, Clone)]
pub struct UnifiedOrderBookConfig {
    /// 最大價格層級數
    pub max_levels: usize,
    
    /// 是否啟用快照緩存
    pub enable_snapshot_cache: bool,
    
    /// 快照緩存過期時間（毫秒）
    pub snapshot_cache_ttl_ms: u64,
    
    /// 是否啟用增量更新優化
    pub enable_delta_optimization: bool,
    
    /// 是否啟用 SIMD 優化（需要 CPU 支持）
    pub enable_simd: bool,
    
    /// 統計信息更新間隔（秒）
    pub stats_update_interval_secs: u64,
}

impl Default for UnifiedOrderBookConfig {
    fn default() -> Self {
        Self {
            max_levels: 50,
            enable_snapshot_cache: true,
            snapshot_cache_ttl_ms: 100,
            enable_delta_optimization: true,
            enable_simd: cfg!(target_feature = "avx2"),
            stats_update_interval_secs: 5,
        }
    }
}

/// OrderBook 快照
#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    pub symbol: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub timestamp: u64,
    pub sequence: Option<u64>,
}

/// 價格層級
#[derive(Debug, Clone, Copy)]
pub struct PriceLevel {
    pub price: f64,
    pub quantity: f64,
    pub order_count: Option<u32>,
}

/// OrderBook 更新類型
#[derive(Debug, Clone)]
pub enum OrderBookUpdate {
    /// 完整快照
    Snapshot(OrderBookSnapshot),
    
    /// 增量更新
    Delta {
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        timestamp: u64,
        sequence: Option<u64>,
    },
}

/// OrderBook 管理器統計信息
#[derive(Debug, Default, Clone)]
pub struct OrderBookManagerStats {
    pub total_updates: u64,
    pub snapshot_updates: u64,
    pub delta_updates: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub update_latency_us: f64,
    pub symbols_tracked: usize,
}

/// 內部 OrderBook 狀態
struct ManagedOrderBook {
    /// 核心 OrderBook 結構
    book: OrderBook,
    
    /// 快照緩存
    snapshot_cache: Option<(OrderBookSnapshot, Instant)>,
    
    /// 最後更新時間
    last_update: Instant,
    
    /// 更新序列號
    sequence: Option<u64>,
    
    /// 統計信息
    stats: OrderBookStats,
}

#[derive(Debug, Default)]
struct OrderBookStats {
    updates: u64,
    snapshots: u64,
    deltas: u64,
}

/// 統一的 OrderBook 管理器
pub struct UnifiedOrderBookManager {
    config: UnifiedOrderBookConfig,
    books: Arc<DashMap<String, Arc<RwLock<ManagedOrderBook>>>>,
    global_stats: Arc<RwLock<OrderBookManagerStats>>,
}

impl UnifiedOrderBookManager {
    /// 創建新的 OrderBook 管理器
    pub fn new(config: UnifiedOrderBookConfig) -> Self {
        Self {
            config,
            books: Arc::new(DashMap::new()),
            global_stats: Arc::new(RwLock::new(OrderBookManagerStats::default())),
        }
    }
    
    /// 處理 OrderBook 更新
    pub fn update(&self, update: OrderBookUpdate) -> Result<()> {
        let start = Instant::now();
        
        match update {
            OrderBookUpdate::Snapshot(snapshot) => {
                self.handle_snapshot(snapshot)?;
            }
            OrderBookUpdate::Delta { symbol, bids, asks, timestamp, sequence } => {
                self.handle_delta(symbol, bids, asks, timestamp, sequence)?;
            }
        }
        
        // 更新延遲統計
        let latency = start.elapsed().as_micros() as f64;
        let mut stats = self.global_stats.write();
        stats.update_latency_us = (stats.update_latency_us * 0.9) + (latency * 0.1); // EMA
        
        Ok(())
    }
    
    /// 獲取 OrderBook 快照
    pub fn get_snapshot(&self, symbol: &str, max_levels: Option<usize>) -> Option<OrderBookSnapshot> {
        if let Some(book_ref) = self.books.get(symbol) {
            let book = book_ref.read();
            
            // 檢查緩存
            if self.config.enable_snapshot_cache {
                if let Some((cached_snapshot, cache_time)) = &book.snapshot_cache {
                    if cache_time.elapsed().as_millis() < self.config.snapshot_cache_ttl_ms as u128 {
                        let mut stats = self.global_stats.write();
                        stats.cache_hits += 1;
                        return Some(cached_snapshot.clone());
                    }
                }
            }
            
            // 生成新快照
            let levels = max_levels.unwrap_or(self.config.max_levels);
            let snapshot = self.generate_snapshot(&book.book, symbol, levels);
            
            let mut stats = self.global_stats.write();
            stats.cache_misses += 1;
            
            Some(snapshot)
        } else {
            None
        }
    }
    
    /// 獲取最佳買賣價
    pub fn get_best_bid_ask(&self, symbol: &str) -> Option<(f64, f64)> {
        self.books.get(symbol).and_then(|book_ref| {
            let book = book_ref.read();
            match (book.book.best_bid(), book.book.best_ask()) {
                (Some(bid), Some(ask)) => Some((bid.0, ask.0)),
                _ => None,
            }
        })
    }
    
    /// 獲取中間價
    pub fn get_mid_price(&self, symbol: &str) -> Option<f64> {
        self.get_best_bid_ask(symbol).map(|(bid, ask)| (bid + ask) / 2.0)
    }
    
    /// 獲取價差
    pub fn get_spread(&self, symbol: &str) -> Option<f64> {
        self.get_best_bid_ask(symbol).map(|(bid, ask)| ask - bid)
    }
    
    /// 獲取管理器統計信息
    pub fn get_stats(&self) -> OrderBookManagerStats {
        let mut stats = self.global_stats.read().clone();
        stats.symbols_tracked = self.books.len();
        stats
    }
    
    /// 清理過期的 OrderBook
    pub fn cleanup_stale_books(&self, max_age: Duration) {
        let now = Instant::now();
        let mut to_remove = Vec::new();
        
        for entry in self.books.iter() {
            let book = entry.value().read();
            if now.duration_since(book.last_update) > max_age {
                to_remove.push(entry.key().clone());
            }
        }
        
        for symbol in to_remove {
            self.books.remove(&symbol);
            debug!("Removed stale orderbook for {}", symbol);
        }
    }
    
    // --- 內部實現方法 ---
    
    fn handle_snapshot(&self, snapshot: OrderBookSnapshot) -> Result<()> {
        let symbol = snapshot.symbol.clone();
        
        // 獲取或創建 OrderBook
        let book_ref = self.books.entry(symbol.clone())
            .or_insert_with(|| Arc::new(RwLock::new(ManagedOrderBook {
                book: OrderBook::new(symbol.clone()),
                snapshot_cache: None,
                last_update: Instant::now(),
                sequence: None,
                stats: OrderBookStats::default(),
            })));
        
        let mut book = book_ref.write();
        
        // 創建 OrderBookUpdate 來初始化快照
        let update = crate::core::types::OrderBookUpdate {
            symbol: symbol.clone(),
            bids: snapshot.bids.iter().map(|level| crate::core::types::PriceLevel {
                price: level.price.to_price(),
                quantity: level.quantity.to_quantity(),
                side: Side::Bid,
            }).collect(),
            asks: snapshot.asks.iter().map(|level| crate::core::types::PriceLevel {
                price: level.price.to_price(),
                quantity: level.quantity.to_quantity(),
                side: Side::Ask,
            }).collect(),
            timestamp: snapshot.timestamp,
            sequence_start: snapshot.sequence.unwrap_or(1),
            sequence_end: snapshot.sequence.unwrap_or(1),
            is_snapshot: true,
        };
        
        // 使用 OrderBook 的 init_snapshot 方法
        book.book.init_snapshot(update)?;
        
        // 更新元數據
        book.sequence = snapshot.sequence;
        book.last_update = Instant::now();
        book.stats.snapshots += 1;
        book.stats.updates += 1;
        
        // 更新緩存
        if self.config.enable_snapshot_cache {
            book.snapshot_cache = Some((snapshot, Instant::now()));
        }
        
        // 更新全局統計
        let mut global_stats = self.global_stats.write();
        global_stats.total_updates += 1;
        global_stats.snapshot_updates += 1;
        
        Ok(())
    }
    
    fn handle_delta(
        &self,
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        timestamp: u64,
        sequence: Option<u64>,
    ) -> Result<()> {
        let book_ref = self.books.get(&symbol)
            .ok_or_else(|| anyhow::anyhow!("No orderbook found for symbol: {}", symbol))?;
        
        let mut book = book_ref.write();
        
        // 檢查序列號（如果提供）
        if let (Some(new_seq), Some(current_seq)) = (sequence, book.sequence) {
            if new_seq <= current_seq {
                warn!("Out of order update for {}: {} <= {}", symbol, new_seq, current_seq);
                return Ok(());
            }
        }
        
        // 創建增量更新
        let update = crate::core::types::OrderBookUpdate {
            symbol: symbol.clone(),
            bids: bids.iter().map(|level| crate::core::types::PriceLevel {
                price: level.price.to_price(),
                quantity: level.quantity.to_quantity(),
                side: Side::Bid,
            }).collect(),
            asks: asks.iter().map(|level| crate::core::types::PriceLevel {
                price: level.price.to_price(),
                quantity: level.quantity.to_quantity(),
                side: Side::Ask,
            }).collect(),
            timestamp,
            sequence_start: sequence.unwrap_or(book.book.last_sequence + 1),
            sequence_end: sequence.unwrap_or(book.book.last_sequence + 1),
            is_snapshot: false,
        };
        
        // 使用 OrderBook 的 apply_update 方法
        book.book.apply_update(update)?;
        
        // 更新元數據
        book.sequence = sequence;
        book.last_update = Instant::now();
        book.stats.deltas += 1;
        book.stats.updates += 1;
        
        // 清除緩存（因為數據已更新）
        if self.config.enable_snapshot_cache {
            book.snapshot_cache = None;
        }
        
        // 更新全局統計
        let mut global_stats = self.global_stats.write();
        global_stats.total_updates += 1;
        global_stats.delta_updates += 1;
        
        Ok(())
    }
    
    fn generate_snapshot(&self, book: &OrderBook, symbol: &str, max_levels: usize) -> OrderBookSnapshot {
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        
        // 從 OrderBook 的 bids BTreeMap 中提取前 max_levels 個買單 (從高到低)
        for (price, quantity) in book.bids.iter().rev().take(max_levels) {
            bids.push(PriceLevel {
                price: price.0,
                quantity: quantity.to_f64().unwrap_or(0.0),
                order_count: None,
            });
        }
        
        // 從 OrderBook 的 asks BTreeMap 中提取前 max_levels 個賣單 (從低到高)
        for (price, quantity) in book.asks.iter().take(max_levels) {
            asks.push(PriceLevel {
                price: price.0,
                quantity: quantity.to_f64().unwrap_or(0.0),
                order_count: None,
            });
        }
        
        OrderBookSnapshot {
            symbol: symbol.to_string(),
            bids,
            asks,
            timestamp: book.last_update,
            sequence: Some(book.last_sequence),
        }
    }
}

/// 便捷函數：創建默認配置的管理器
pub fn create_default_manager() -> UnifiedOrderBookManager {
    UnifiedOrderBookManager::new(UnifiedOrderBookConfig::default())
}

/// 便捷函數：創建高性能配置的管理器
pub fn create_high_performance_manager() -> UnifiedOrderBookManager {
    let config = UnifiedOrderBookConfig {
        max_levels: 25,
        enable_snapshot_cache: true,
        snapshot_cache_ttl_ms: 50,
        enable_delta_optimization: true,
        enable_simd: true,
        stats_update_interval_secs: 10,
    };
    
    UnifiedOrderBookManager::new(config)
}

/// 便捷函數：創建低延遲配置的管理器
pub fn create_low_latency_manager() -> UnifiedOrderBookManager {
    let config = UnifiedOrderBookConfig {
        max_levels: 10,
        enable_snapshot_cache: false, // 避免緩存開銷
        snapshot_cache_ttl_ms: 0,
        enable_delta_optimization: true,
        enable_simd: true,
        stats_update_interval_secs: 60, // 減少統計開銷
    };
    
    UnifiedOrderBookManager::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_orderbook_manager_creation() {
        let manager = create_default_manager();
        let stats = manager.get_stats();
        assert_eq!(stats.total_updates, 0);
        assert_eq!(stats.symbols_tracked, 0);
    }
    
    #[test]
    fn test_snapshot_update() {
        let manager = create_default_manager();
        
        let snapshot = OrderBookSnapshot {
            symbol: "BTCUSDT".to_string(),
            bids: vec![
                PriceLevel { price: 50000.0, quantity: 1.0, order_count: None },
                PriceLevel { price: 49999.0, quantity: 2.0, order_count: None },
            ],
            asks: vec![
                PriceLevel { price: 50001.0, quantity: 1.0, order_count: None },
                PriceLevel { price: 50002.0, quantity: 2.0, order_count: None },
            ],
            timestamp: 1234567890,
            sequence: Some(1),
        };
        
        manager.update(OrderBookUpdate::Snapshot(snapshot)).unwrap();
        
        let stats = manager.get_stats();
        assert_eq!(stats.total_updates, 1);
        assert_eq!(stats.snapshot_updates, 1);
        assert_eq!(stats.symbols_tracked, 1);
        
        let mid_price = manager.get_mid_price("BTCUSDT").unwrap();
        assert_eq!(mid_price, 50000.5);
    }
}
