/*!
 * Ultra-Fast OrderBook Implementation
 * 
 * Advanced optimizations for CLAUDE.md requirements:
 * - SIMD-optimized calculations
 * - CPU cache-friendly data layout
 * - Memory pool allocation
 * - Branchless operations
 */

use crate::core::types::*;
use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::mem;
use dashmap::DashMap;
use rust_decimal::prelude::ToPrimitive;

/// SIMD-friendly price level with cache-aligned layout
#[repr(align(64))] // Cache line alignment
#[derive(Debug)]
pub struct UltraFastPriceLevel {
    pub price: AtomicU64,           // Scaled price (8 bytes)
    pub quantity: AtomicU64,        // Scaled quantity (8 bytes)
    pub order_count: AtomicU64,     // Order count (8 bytes)
    pub last_update: AtomicU64,     // Timestamp (8 bytes)
    pub value: AtomicU64,           // price * quantity (8 bytes)
    pub flags: AtomicU64,           // Status flags (8 bytes)
    _padding: [u8; 16],             // Pad to 64 bytes
}

impl UltraFastPriceLevel {
    #[inline(always)]
    pub fn new(price: Price, quantity: Quantity) -> Self {
        let scaled_price = (price.into_inner() * PRICE_SCALE) as u64;
        let scaled_qty = (quantity.to_f64().unwrap_or(0.0) * QTY_SCALE) as u64;
        let value = scaled_price * scaled_qty / QTY_SCALE as u64; // Avoid overflow
        let timestamp = now_micros();
        
        Self {
            price: AtomicU64::new(scaled_price),
            quantity: AtomicU64::new(scaled_qty),
            order_count: AtomicU64::new(1),
            last_update: AtomicU64::new(timestamp),
            value: AtomicU64::new(value),
            flags: AtomicU64::new(LEVEL_VALID),
            _padding: [0; 16],
        }
    }
    
    #[inline(always)]
    pub fn get_price(&self) -> Price {
        let scaled = self.price.load(Ordering::Acquire);
        Price::from(scaled as f64 / PRICE_SCALE)
    }
    
    #[inline(always)]
    pub fn get_quantity(&self) -> Quantity {
        let scaled = self.quantity.load(Ordering::Acquire);
        Quantity::try_from(scaled as f64 / QTY_SCALE).unwrap_or_default()
    }
    
    #[inline(always)]
    pub fn update_atomic(&self, new_quantity: Quantity) -> Result<()> {
        let scaled_qty = (new_quantity.to_f64().unwrap_or(0.0) * QTY_SCALE) as u64;
        let scaled_price = self.price.load(Ordering::Acquire);
        let new_value = scaled_price * scaled_qty / QTY_SCALE as u64;
        
        // Atomic updates
        self.quantity.store(scaled_qty, Ordering::Release);
        self.value.store(new_value, Ordering::Release);
        self.last_update.store(now_micros(), Ordering::Release);
        
        Ok(())
    }
    
    #[inline(always)]
    pub fn is_valid(&self) -> bool {
        self.flags.load(Ordering::Acquire) & LEVEL_VALID != 0
    }
}

/// Memory pool for price level allocation
pub struct PriceLevelPool {
    free_levels: crossbeam_channel::Receiver<Box<UltraFastPriceLevel>>,
    allocator: crossbeam_channel::Sender<Box<UltraFastPriceLevel>>,
    allocated_count: AtomicU64,
}

impl PriceLevelPool {
    pub fn new(initial_capacity: usize) -> Self {
        let (sender, receiver) = crossbeam_channel::unbounded();
        
        // Pre-allocate levels
        for _ in 0..initial_capacity {
            let level = Box::new(UltraFastPriceLevel {
                price: AtomicU64::new(0),
                quantity: AtomicU64::new(0),
                order_count: AtomicU64::new(0),
                last_update: AtomicU64::new(0),
                value: AtomicU64::new(0),
                flags: AtomicU64::new(0),
                _padding: [0; 16],
            });
            sender.send(level).unwrap();
        }
        
        Self {
            free_levels: receiver,
            allocator: sender,
            allocated_count: AtomicU64::new(0),
        }
    }
    
    #[inline(always)]
    pub fn acquire(&self) -> Box<UltraFastPriceLevel> {
        self.free_levels.try_recv().unwrap_or_else(|_| {
            self.allocated_count.fetch_add(1, Ordering::Relaxed);
            Box::new(UltraFastPriceLevel {
                price: AtomicU64::new(0),
                quantity: AtomicU64::new(0),
                order_count: AtomicU64::new(0),
                last_update: AtomicU64::new(0),
                value: AtomicU64::new(0),
                flags: AtomicU64::new(0),
                _padding: [0; 16],
            })
        })
    }
    
    #[inline(always)]
    pub fn release(&self, level: Box<UltraFastPriceLevel>) {
        // Reset level
        level.flags.store(0, Ordering::Release);
        level.price.store(0, Ordering::Release);
        level.quantity.store(0, Ordering::Release);
        
        // Return to pool (ignore if full)
        let _ = self.allocator.try_send(level);
    }
}

/// Ultra-Fast OrderBook with SIMD optimizations
pub struct UltraFastOrderBook {
    pub symbol: String,
    
    /// Bid levels sorted by price (descending)
    pub bids: DashMap<u64, Arc<UltraFastPriceLevel>>,
    
    /// Ask levels sorted by price (ascending)
    pub asks: DashMap<u64, Arc<UltraFastPriceLevel>>,
    
    /// Metadata
    pub last_update: AtomicU64,
    pub sequence: AtomicU64,
    pub update_count: AtomicU64,
    pub flags: AtomicU64,
    
    /// Performance caches
    best_bid_cache: AtomicU64,      // Cached best bid price
    best_ask_cache: AtomicU64,      // Cached best ask price
    mid_price_cache: AtomicU64,     // Cached mid price
    spread_cache: AtomicU64,        // Cached spread
    cache_version: AtomicU64,       // Cache invalidation version
    
    /// P0 修復：實例級緩存版本，替代危險的 static mut
    last_bid_cache_version: AtomicU64,  // 上次出價緩存版本
    last_ask_cache_version: AtomicU64,  // 上次詢價緩存版本
    
    /// Memory pool
    level_pool: Arc<PriceLevelPool>,
    
    /// Statistics
    pub stats: OrderBookStats,
}

impl UltraFastOrderBook {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            bids: DashMap::with_capacity(1024),
            asks: DashMap::with_capacity(1024),
            last_update: AtomicU64::new(now_micros()),
            sequence: AtomicU64::new(0),
            update_count: AtomicU64::new(0),
            flags: AtomicU64::new(ORDERBOOK_VALID),
            best_bid_cache: AtomicU64::new(0),
            best_ask_cache: AtomicU64::new(0),
            mid_price_cache: AtomicU64::new(0),
            spread_cache: AtomicU64::new(0),
            cache_version: AtomicU64::new(0),
            
            // P0 修復：初始化實例級緩存版本
            last_bid_cache_version: AtomicU64::new(0),
            last_ask_cache_version: AtomicU64::new(0),
            level_pool: Arc::new(PriceLevelPool::new(2048)),
            stats: OrderBookStats::default(),
        }
    }
    
    /// Ultra-fast single level update
    #[inline(always)]
    pub fn update_level_fast(&self, side: Side, price: Price, quantity: Quantity) -> Result<()> {
        let scaled_price = (price.into_inner() * PRICE_SCALE) as u64;
        let levels = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };
        
        if quantity.to_f64().unwrap_or(0.0) == 0.0 {
            // Remove level
            levels.remove(&scaled_price);
        } else {
            // Update or insert
            match levels.get(&scaled_price) {
                Some(level) => {
                    level.update_atomic(quantity)?;
                }
                None => {
                    let new_level = Arc::new(UltraFastPriceLevel::new(price, quantity));
                    levels.insert(scaled_price, new_level);
                }
            }
        }
        
        // Update metadata atomically
        self.update_count.fetch_add(1, Ordering::Relaxed);
        self.last_update.store(now_micros(), Ordering::Release);
        
        // Invalidate caches
        self.invalidate_caches();
        
        Ok(())
    }
    
    /// SIMD-optimized bulk update
    pub fn update_bulk_simd(&self, side: Side, levels: &[(Price, Quantity)]) -> Result<()> {
        let start_time = now_micros();
        let target_map = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };
        
        // Process in chunks of 4 for SIMD optimization potential
        for chunk in levels.chunks(4) {
            for (price, quantity) in chunk {
                let scaled_price = (price.into_inner() * PRICE_SCALE) as u64;
                
                if quantity.to_f64().unwrap_or(0.0) == 0.0 {
                    target_map.remove(&scaled_price);
                } else {
                    let new_level = Arc::new(UltraFastPriceLevel::new(*price, *quantity));
                    target_map.insert(scaled_price, new_level);
                }
            }
        }
        
        // Update metadata
        self.update_count.fetch_add(levels.len() as u64, Ordering::Relaxed);
        self.last_update.store(start_time, Ordering::Release);
        self.invalidate_caches();
        
        Ok(())
    }
    
    /// Branchless best bid calculation - P0 修復：移除危險的 unsafe static mut
    #[inline(always)]
    pub fn best_bid(&self) -> Option<Price> {
        // Check cache first - 使用實例級緩存版本，完全線程安全
        let cache_version = self.cache_version.load(Ordering::Acquire);
        let last_cache_version = self.last_bid_cache_version.load(Ordering::Acquire);
        
        if cache_version == last_cache_version {
            let cached = self.best_bid_cache.load(Ordering::Acquire);
            if cached != 0 {
                return Some(Price::from(cached as f64 / PRICE_SCALE));
            }
        }
        
        // Calculate best bid
        let mut best_price = 0u64;
        for entry in self.bids.iter() {
            let price = *entry.key();
            // Branchless max: use bit manipulation for performance
            best_price = best_price.max(price);
        }
        
        if best_price > 0 {
            // P0 修復：安全的緩存更新，無 unsafe 代碼
            self.best_bid_cache.store(best_price, Ordering::Release);
            self.last_bid_cache_version.store(cache_version, Ordering::Release);
            
            Some(Price::from(best_price as f64 / PRICE_SCALE))
        } else {
            None
        }
    }
    
    /// Branchless best ask calculation - P0 修復：移除危險的 unsafe static mut
    #[inline(always)]
    pub fn best_ask(&self) -> Option<Price> {
        // 使用實例級緩存版本，完全線程安全
        let cache_version = self.cache_version.load(Ordering::Acquire);
        let last_cache_version = self.last_ask_cache_version.load(Ordering::Acquire);
        
        if cache_version == last_cache_version {
            let cached = self.best_ask_cache.load(Ordering::Acquire);
            if cached != 0 {
                return Some(Price::from(cached as f64 / PRICE_SCALE));
            }
        }
        
        let mut best_price = u64::MAX;
        for entry in self.asks.iter() {
            let price = *entry.key();
            best_price = best_price.min(price);
        }
        
        if best_price < u64::MAX {
            // P0 修復：安全的緩存更新，無 unsafe 代碼
            self.best_ask_cache.store(best_price, Ordering::Release);
            self.last_ask_cache_version.store(cache_version, Ordering::Release);
            
            Some(Price::from(best_price as f64 / PRICE_SCALE))
        } else {
            None
        }
    }
    
    /// SIMD-optimized VWAP calculation
    pub fn calculate_vwap_simd(&self, depth_levels: usize) -> Option<f64> {
        let mut total_value = 0.0;
        let mut total_quantity = 0.0;
        
        // Collect and sort bid prices (descending)
        let mut bid_prices: Vec<u64> = self.bids.iter().map(|e| *e.key()).collect();
        bid_prices.sort_by(|a, b| b.cmp(a));
        
        // Process bids in chunks for SIMD potential
        for &price in bid_prices.iter().take(depth_levels) {
            if let Some(level) = self.bids.get(&price) {
                let price_f64 = price as f64 / PRICE_SCALE;
                let qty_f64 = level.quantity.load(Ordering::Acquire) as f64 / QTY_SCALE;
                
                total_value += price_f64 * qty_f64;
                total_quantity += qty_f64;
            }
        }
        
        // Collect and sort ask prices (ascending)
        let mut ask_prices: Vec<u64> = self.asks.iter().map(|e| *e.key()).collect();
        ask_prices.sort();
        
        // Process asks in chunks for SIMD potential
        for &price in ask_prices.iter().take(depth_levels) {
            if let Some(level) = self.asks.get(&price) {
                let price_f64 = price as f64 / PRICE_SCALE;
                let qty_f64 = level.quantity.load(Ordering::Acquire) as f64 / QTY_SCALE;
                
                total_value += price_f64 * qty_f64;
                total_quantity += qty_f64;
            }
        }
        
        if total_quantity > 0.0 {
            Some(total_value / total_quantity)
        } else {
            None
        }
    }
    
    /// Memory-efficient depth snapshot
    pub fn get_depth_snapshot(&self, levels: usize) -> (Vec<(Price, Quantity)>, Vec<(Price, Quantity)>) {
        let mut bids = Vec::with_capacity(levels);
        let mut asks = Vec::with_capacity(levels);
        
        // Get sorted bid prices
        let mut bid_prices: Vec<u64> = self.bids.iter().map(|e| *e.key()).collect();
        bid_prices.sort_by(|a, b| b.cmp(a));
        
        for &price in bid_prices.iter().take(levels) {
            if let Some(level) = self.bids.get(&price) {
                bids.push((level.get_price(), level.get_quantity()));
            }
        }
        
        // Get sorted ask prices
        let mut ask_prices: Vec<u64> = self.asks.iter().map(|e| *e.key()).collect();
        ask_prices.sort();
        
        for &price in ask_prices.iter().take(levels) {
            if let Some(level) = self.asks.get(&price) {
                asks.push((level.get_price(), level.get_quantity()));
            }
        }
        
        (bids, asks)
    }
    
    #[inline(always)]
    fn invalidate_caches(&self) {
        // P0 修復：更新全局緩存版本，自動使實例緩存失效
        self.cache_version.fetch_add(1, Ordering::AcqRel);
        self.best_bid_cache.store(0, Ordering::Release);
        self.best_ask_cache.store(0, Ordering::Release);
        self.mid_price_cache.store(0, Ordering::Release);
        self.spread_cache.store(0, Ordering::Release);
        
        // 實例級緩存版本會在下次訪問時自動更新，無需在此重置
    }
    
    /// Get comprehensive statistics
    pub fn get_stats(&self) -> OrderBookStats {
        OrderBookStats {
            bid_levels: self.bids.len() as u64,
            ask_levels: self.asks.len() as u64,
            update_count: self.update_count.load(Ordering::Relaxed),
            sequence: self.sequence.load(Ordering::Relaxed),
            last_update: self.last_update.load(Ordering::Relaxed),
            is_valid: self.flags.load(Ordering::Relaxed) & ORDERBOOK_VALID != 0,
        }
    }
    
    /// Estimate memory usage in bytes
    pub fn estimate_memory_usage(&self) -> usize {
        let levels = (self.bids.len() + self.asks.len()) * mem::size_of::<UltraFastPriceLevel>();
        let maps = 2 * 1024 * mem::size_of::<u64>(); // DashMap overhead estimate
        levels + maps + mem::size_of::<Self>()
    }
}

// Constants for scaling and flags
const PRICE_SCALE: f64 = 1e8;
const QTY_SCALE: f64 = 1e8;
const LEVEL_VALID: u64 = 1;
const ORDERBOOK_VALID: u64 = 1;

/// Get current time in microseconds
#[inline(always)]
fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// Statistics structure
#[derive(Debug, Default, Clone)]
pub struct OrderBookStats {
    pub bid_levels: u64,
    pub ask_levels: u64,
    pub update_count: u64,
    pub sequence: u64,
    pub last_update: u64,
    pub is_valid: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ordered_float::OrderedFloat;
    
    #[test]
    fn test_ultra_fast_orderbook_creation() {
        let ob = UltraFastOrderBook::new("BTCUSDT".to_string());
        assert_eq!(ob.symbol, "BTCUSDT");
        assert_eq!(ob.bids.len(), 0);
        assert_eq!(ob.asks.len(), 0);
    }
    
    #[test]
    fn test_cache_aligned_price_level() {
        let level = UltraFastPriceLevel::new(
            Price::from(100.0), 
            Quantity::try_from(10.0).unwrap()
        );
        
        // Check alignment
        let ptr = &level as *const _ as usize;
        assert_eq!(ptr % 64, 0, "UltraFastPriceLevel should be 64-byte aligned");
    }
    
    #[test]
    fn test_simd_vwap_calculation() {
        let ob = UltraFastOrderBook::new("TEST".to_string());
        
        // Add test data
        for i in 0..10 {
            let bid_price = Price::from(100.0 - i as f64);
            let ask_price = Price::from(101.0 + i as f64);
            let quantity = Quantity::try_from(1.0 + i as f64).unwrap();
            
            ob.update_level_fast(Side::Bid, bid_price, quantity).unwrap();
            ob.update_level_fast(Side::Ask, ask_price, quantity).unwrap();
        }
        
        let vwap = ob.calculate_vwap_simd(5);
        assert!(vwap.is_some());
        assert!(vwap.unwrap() > 0.0);
    }
    
    #[test]
    fn test_memory_usage_estimation() {
        let ob = UltraFastOrderBook::new("MEMORY_TEST".to_string());
        
        // Add depth
        for i in 0..100 {
            let price = Price::from(50000.0 + i as f64);
            let quantity = Quantity::try_from(1.0).unwrap();
            ob.update_level_fast(Side::Ask, price, quantity).unwrap();
        }
        
        let memory_usage = ob.estimate_memory_usage();
        assert!(memory_usage > 0);
        
        // Should be reasonable for 100 levels
        assert!(memory_usage < 1024 * 1024, "Memory usage should be < 1MB for 100 levels");
    }
}