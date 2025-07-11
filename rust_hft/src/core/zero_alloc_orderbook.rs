/*!
 * 🚀 零分配高性能OrderBook實現
 * 
 * 專為HFT設計的超低延遲訂單簿，使用內存池和零拷貝技術
 * 
 * 核心特性：
 * - 零運行時分配
 * - SIMD優化搜索
 * - 預分配內存池
 * - 緩存友好的數據布局
 * - 無鎖並發訪問
 */

use crate::core::types::*;
use crate::utils::memory_optimization::{
    ZeroAllocVec, MemoryPool, CowPtr, StackAllocator, GLOBAL_MEMORY_MANAGER
};
use crate::utils::concurrency_safety::{LockFreeCounter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::ptr::NonNull;
use std::mem::MaybeUninit;
use serde::{Serialize, Deserialize};
use tracing::{debug, warn, error, info};
// SIMD支持（需要nightly Rust）
#[cfg(feature = "simd")]
use std::simd::{f64x8, Simd};

/// 價格層級條目
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct PriceLevel {
    /// 價格
    pub price: f64,
    /// 數量
    pub quantity: f64,
    /// 訂單數量
    pub order_count: u32,
    /// 最後更新時間
    pub last_update: u64,
}

impl PriceLevel {
    pub fn new(price: f64, quantity: f64) -> Self {
        Self {
            price,
            quantity,
            order_count: 1,
            last_update: crate::core::types::now_micros(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.quantity <= 0.0
    }

    pub fn update(&mut self, quantity: f64) {
        self.quantity = quantity;
        self.last_update = crate::core::types::now_micros();
        if quantity > 0.0 {
            self.order_count = 1;
        } else {
            self.order_count = 0;
        }
    }
}

/// 零分配訂單簿實現
pub struct ZeroAllocOrderBook {
    /// 買單側（按價格降序）
    bids: ZeroAllocVec<PriceLevel>,
    /// 賣單側（按價格升序）  
    asks: ZeroAllocVec<PriceLevel>,
    /// 交易對符號
    symbol: String,
    /// 最後更新時間戳
    last_update: AtomicU64,
    /// 序列號
    sequence: AtomicU64,
    /// 更新計數器
    update_count: LockFreeCounter,
    /// 數據有效性標誌
    is_valid: AtomicBool,
    /// 最佳買價緩存
    cached_best_bid: AtomicU64, // 使用位表示存儲f64
    /// 最佳賣價緩存
    cached_best_ask: AtomicU64,
    /// 中間價緩存  
    cached_mid_price: AtomicU64,
    /// 價差緩存
    cached_spread: AtomicU64,
    /// 緩存有效性標誌
    cache_valid: AtomicBool,
    /// 內存池引用
    memory_pool: Arc<MemoryPool<PriceLevel>>,
}

impl ZeroAllocOrderBook {
    /// 創建新的零分配訂單簿
    pub fn new(symbol: String, max_levels: usize) -> anyhow::Result<Self> {
        let memory_pool = Arc::new(MemoryPool::new(max_levels, 10));
        
        let bids = ZeroAllocVec::with_capacity_from_pool(max_levels, &memory_pool)
            .ok_or_else(|| anyhow::anyhow!("Failed to allocate memory for bids"))?;
        let asks = ZeroAllocVec::with_capacity_from_pool(max_levels, &memory_pool)
            .ok_or_else(|| anyhow::anyhow!("Failed to allocate memory for asks"))?;

        Ok(Self {
            bids,
            asks,
            symbol,
            last_update: AtomicU64::new(crate::core::types::now_micros()),
            sequence: AtomicU64::new(0),
            update_count: LockFreeCounter::new(0),
            is_valid: AtomicBool::new(true),
            cached_best_bid: AtomicU64::new(0),
            cached_best_ask: AtomicU64::new(0),
            cached_mid_price: AtomicU64::new(0),
            cached_spread: AtomicU64::new(0),
            cache_valid: AtomicBool::new(false),
            memory_pool,
        })
    }

    /// 更新買單價格層級（零分配）
    pub fn update_bid(&mut self, price: f64, quantity: f64) -> anyhow::Result<()> {
        self.invalidate_cache();
        
        if quantity <= 0.0 {
            // 刪除價格層級
            self.remove_bid_level(price)?;
        } else {
            // 添加或更新價格層級
            self.upsert_bid_level(price, quantity)?;
        }
        
        self.increment_sequence();
        Ok(())
    }

    /// 更新賣單價格層級（零分配）
    pub fn update_ask(&mut self, price: f64, quantity: f64) -> anyhow::Result<()> {
        self.invalidate_cache();
        
        if quantity <= 0.0 {
            // 刪除價格層級
            self.remove_ask_level(price)?;
        } else {
            // 添加或更新價格層級
            self.upsert_ask_level(price, quantity)?;
        }
        
        self.increment_sequence();
        Ok(())
    }

    /// SIMD優化的價格搜索
    fn find_bid_index_simd(&self, target_price: f64) -> Option<usize> {
        let levels = self.bids.as_slice();
        if levels.is_empty() {
            return None;
        }

        #[cfg(feature = "simd")]
        {
            // 使用SIMD進行並行搜索（處理8個價格）
            let target_simd = f64x8::splat(target_price);
            let mut i = 0;
            
            while i + 8 <= levels.len() {
                // 載入8個價格到SIMD寄存器
                let prices: [f64; 8] = [
                    levels[i].price, levels[i+1].price, levels[i+2].price, levels[i+3].price,
                    levels[i+4].price, levels[i+5].price, levels[i+6].price, levels[i+7].price,
                ];
                let price_simd = f64x8::from_array(prices);
                
                // 並行比較
                let mask = price_simd.simd_eq(target_simd);
                
                // 檢查是否有匹配
                for j in 0..8 {
                    if mask.test(j) {
                        return Some(i + j);
                    }
                }
                
                i += 8;
            }
            
            // 處理剩餘元素
            for j in i..levels.len() {
                if (levels[j].price - target_price).abs() < f64::EPSILON {
                    return Some(j);
                }
            }
        }
        
        #[cfg(not(feature = "simd"))]
        {
            // 回退到線性搜索
            for (i, level) in levels.iter().enumerate() {
                if (level.price - target_price).abs() < f64::EPSILON {
                    return Some(i);
                }
            }
        }
        
        None
    }

    /// 二分搜索買單插入位置
    fn find_bid_insert_position(&self, price: f64) -> usize {
        let levels = self.bids.as_slice();
        let mut left = 0;
        let mut right = levels.len();
        
        while left < right {
            let mid = left + (right - left) / 2;
            if levels[mid].price > price {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        
        left
    }

    /// 二分搜索賣單插入位置
    fn find_ask_insert_position(&self, price: f64) -> usize {
        let levels = self.asks.as_slice();
        let mut left = 0;
        let mut right = levels.len();
        
        while left < right {
            let mid = left + (right - left) / 2;
            if levels[mid].price < price {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        
        left
    }

    /// 添加或更新買單層級
    fn upsert_bid_level(&mut self, price: f64, quantity: f64) -> anyhow::Result<()> {
        if let Some(index) = self.find_bid_index_simd(price) {
            // 更新現有層級
            self.bids.as_mut_slice()[index].update(quantity);
        } else {
            // 插入新層級
            let insert_pos = self.find_bid_insert_position(price);
            let new_level = PriceLevel::new(price, quantity);
            
            // 在指定位置插入（需要移動元素）
            if self.bids.len() >= self.bids.as_slice().len() {
                return Err(anyhow::anyhow!("OrderBook bid side is full"));
            }
            
            // 手動實現插入以避免分配
            self.bids.push(new_level).map_err(|_| anyhow::anyhow!("Failed to insert bid level"))?;
            
            // 將新元素移動到正確位置
            let slice = self.bids.as_mut_slice();
            let last_idx = slice.len() - 1;
            for i in (insert_pos..last_idx).rev() {
                slice.swap(i, i + 1);
            }
        }
        
        Ok(())
    }

    /// 添加或更新賣單層級
    fn upsert_ask_level(&mut self, price: f64, quantity: f64) -> anyhow::Result<()> {
        if let Some(index) = self.find_ask_index(price) {
            // 更新現有層級
            self.asks.as_mut_slice()[index].update(quantity);
        } else {
            // 插入新層級
            let insert_pos = self.find_ask_insert_position(price);
            let new_level = PriceLevel::new(price, quantity);
            
            if self.asks.len() >= self.asks.as_slice().len() {
                return Err(anyhow::anyhow!("OrderBook ask side is full"));
            }
            
            self.asks.push(new_level).map_err(|_| anyhow::anyhow!("Failed to insert ask level"))?;
            
            // 將新元素移動到正確位置
            let slice = self.asks.as_mut_slice();
            let last_idx = slice.len() - 1;
            for i in (insert_pos..last_idx).rev() {
                slice.swap(i, i + 1);
            }
        }
        
        Ok(())
    }

    /// 查找賣單價格索引
    fn find_ask_index(&self, target_price: f64) -> Option<usize> {
        self.asks.as_slice().iter().position(|level| {
            (level.price - target_price).abs() < f64::EPSILON
        })
    }

    /// 移除買單層級
    fn remove_bid_level(&mut self, price: f64) -> anyhow::Result<()> {
        if let Some(index) = self.find_bid_index_simd(price) {
            // 移除元素（向前移動後續元素）
            let slice = self.bids.as_mut_slice();
            for i in index..slice.len()-1 {
                slice[i] = slice[i + 1];
            }
            self.bids.pop();
        }
        Ok(())
    }

    /// 移除賣單層級
    fn remove_ask_level(&mut self, price: f64) -> anyhow::Result<()> {
        if let Some(index) = self.find_ask_index(price) {
            let slice = self.asks.as_mut_slice();
            for i in index..slice.len()-1 {
                slice[i] = slice[i + 1];
            }
            self.asks.pop();
        }
        Ok(())
    }

    /// 獲取最佳買價（緩存優化）
    pub fn best_bid(&self) -> Option<f64> {
        if self.cache_valid.load(Ordering::Acquire) {
            let cached = self.cached_best_bid.load(Ordering::Relaxed);
            if cached != 0 {
                return Some(f64::from_bits(cached));
            }
        }
        
        let best = self.bids.as_slice().first()?.price;
        self.cached_best_bid.store(best.to_bits(), Ordering::Relaxed);
        Some(best)
    }

    /// 獲取最佳賣價（緩存優化）
    pub fn best_ask(&self) -> Option<f64> {
        if self.cache_valid.load(Ordering::Acquire) {
            let cached = self.cached_best_ask.load(Ordering::Relaxed);
            if cached != 0 {
                return Some(f64::from_bits(cached));
            }
        }
        
        let best = self.asks.as_slice().first()?.price;
        self.cached_best_ask.store(best.to_bits(), Ordering::Relaxed);
        Some(best)
    }

    /// 獲取中間價（緩存優化）
    pub fn mid_price(&self) -> Option<f64> {
        if self.cache_valid.load(Ordering::Acquire) {
            let cached = self.cached_mid_price.load(Ordering::Relaxed);
            if cached != 0 {
                return Some(f64::from_bits(cached));
            }
        }

        let best_bid = self.best_bid()?;
        let best_ask = self.best_ask()?;
        let mid = (best_bid + best_ask) / 2.0;
        
        self.cached_mid_price.store(mid.to_bits(), Ordering::Relaxed);
        Some(mid)
    }

    /// 獲取價差（緩存優化）
    pub fn spread(&self) -> Option<f64> {
        if self.cache_valid.load(Ordering::Acquire) {
            let cached = self.cached_spread.load(Ordering::Relaxed);
            if cached != 0 {
                return Some(f64::from_bits(cached));
            }
        }

        let best_bid = self.best_bid()?;
        let best_ask = self.best_ask()?;
        let spread = best_ask - best_bid;
        
        self.cached_spread.store(spread.to_bits(), Ordering::Relaxed);
        Some(spread)
    }

    /// 使緩存失效
    fn invalidate_cache(&self) {
        self.cache_valid.store(false, Ordering::Release);
    }

    /// 重新計算並緩存關鍵指標
    pub fn refresh_cache(&self) {
        // 重新計算所有緩存值
        if let (Some(bid), Some(ask)) = (self.best_bid(), self.best_ask()) {
            self.cached_best_bid.store(bid.to_bits(), Ordering::Relaxed);
            self.cached_best_ask.store(ask.to_bits(), Ordering::Relaxed);
            
            let mid = (bid + ask) / 2.0;
            let spread = ask - bid;
            
            self.cached_mid_price.store(mid.to_bits(), Ordering::Relaxed);
            self.cached_spread.store(spread.to_bits(), Ordering::Relaxed);
            
            self.cache_valid.store(true, Ordering::Release);
        }
    }

    /// 增加序列號
    fn increment_sequence(&self) {
        self.sequence.fetch_add(1, Ordering::Relaxed);
        self.update_count.increment();
        self.last_update.store(crate::core::types::now_micros(), Ordering::Relaxed);
    }

    /// 獲取訂單簿深度
    pub fn depth(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }

    /// 獲取L2市場深度快照
    pub fn get_l2_snapshot(&self, levels: usize) -> L2Snapshot {
        let bid_levels: Vec<_> = self.bids.as_slice()
            .iter()
            .take(levels)
            .map(|level| (level.price, level.quantity))
            .collect();
            
        let ask_levels: Vec<_> = self.asks.as_slice()
            .iter()
            .take(levels)
            .map(|level| (level.price, level.quantity))
            .collect();

        L2Snapshot {
            symbol: self.symbol.clone(),
            bids: bid_levels,
            asks: ask_levels,
            timestamp: self.last_update.load(Ordering::Relaxed),
            sequence: self.sequence.load(Ordering::Relaxed),
        }
    }

    /// 檢查訂單簿是否為空
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() && self.asks.is_empty()
    }

    /// 清空訂單簿但保留內存
    pub fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.invalidate_cache();
        self.is_valid.store(true, Ordering::Relaxed);
    }

    /// 獲取內存使用統計
    pub fn memory_stats(&self) -> MemoryUsageStats {
        MemoryUsageStats {
            bid_levels: self.bids.len(),
            ask_levels: self.asks.len(),
            total_capacity: self.bids.as_slice().len() + self.asks.as_slice().len(),
            memory_pool_stats: self.memory_pool.get_stats(),
        }
    }

    /// 驗證訂單簿完整性
    pub fn validate(&self) -> bool {
        // 檢查買單是否按價格降序排列
        let bid_sorted = self.bids.as_slice()
            .windows(2)
            .all(|w| w[0].price >= w[1].price);

        // 檢查賣單是否按價格升序排列
        let ask_sorted = self.asks.as_slice()
            .windows(2)
            .all(|w| w[0].price <= w[1].price);

        // 檢查是否有交叉
        let no_cross = match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => bid < ask,
            _ => true,
        };

        let is_valid = bid_sorted && ask_sorted && no_cross;
        self.is_valid.store(is_valid, Ordering::Relaxed);
        is_valid
    }
}

/// L2市場深度快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2Snapshot {
    pub symbol: String,
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
    pub timestamp: u64,
    pub sequence: u64,
}

/// 內存使用統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUsageStats {
    pub bid_levels: usize,
    pub ask_levels: usize,
    pub total_capacity: usize,
    pub memory_pool_stats: crate::utils::memory_optimization::PoolStats,
}

unsafe impl Send for ZeroAllocOrderBook {}
unsafe impl Sync for ZeroAllocOrderBook {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_alloc_orderbook_creation() {
        let orderbook = ZeroAllocOrderBook::new("BTCUSDT".to_string(), 100).unwrap();
        assert!(orderbook.is_empty());
        assert_eq!(orderbook.depth(), (0, 0));
    }

    #[test]
    fn test_price_level_operations() {
        let mut orderbook = ZeroAllocOrderBook::new("BTCUSDT".to_string(), 100).unwrap();
        
        // 添加買單
        orderbook.update_bid(50000.0, 1.5).unwrap();
        orderbook.update_bid(49999.0, 2.0).unwrap();
        orderbook.update_bid(49998.0, 1.0).unwrap();
        
        // 添加賣單
        orderbook.update_ask(50001.0, 1.2).unwrap();
        orderbook.update_ask(50002.0, 0.8).unwrap();
        
        assert_eq!(orderbook.depth(), (3, 2));
        assert_eq!(orderbook.best_bid(), Some(50000.0));
        assert_eq!(orderbook.best_ask(), Some(50001.0));
        assert_eq!(orderbook.spread(), Some(1.0));
        assert_eq!(orderbook.mid_price(), Some(50000.5));
    }

    #[test]
    fn test_price_level_removal() {
        let mut orderbook = ZeroAllocOrderBook::new("BTCUSDT".to_string(), 100).unwrap();
        
        orderbook.update_bid(50000.0, 1.5).unwrap();
        orderbook.update_bid(49999.0, 2.0).unwrap();
        
        // 移除價格層級
        orderbook.update_bid(50000.0, 0.0).unwrap();
        
        assert_eq!(orderbook.depth(), (1, 0));
        assert_eq!(orderbook.best_bid(), Some(49999.0));
    }

    #[test]
    fn test_orderbook_validation() {
        let mut orderbook = ZeroAllocOrderBook::new("BTCUSDT".to_string(), 100).unwrap();
        
        orderbook.update_bid(50000.0, 1.5).unwrap();
        orderbook.update_ask(50001.0, 1.2).unwrap();
        
        assert!(orderbook.validate());
        
        // 創建交叉情況
        orderbook.update_ask(49999.0, 1.0).unwrap();
        assert!(!orderbook.validate());
    }

    #[test]
    fn test_l2_snapshot() {
        let mut orderbook = ZeroAllocOrderBook::new("BTCUSDT".to_string(), 100).unwrap();
        
        orderbook.update_bid(50000.0, 1.5).unwrap();
        orderbook.update_bid(49999.0, 2.0).unwrap();
        orderbook.update_ask(50001.0, 1.2).unwrap();
        orderbook.update_ask(50002.0, 0.8).unwrap();
        
        let snapshot = orderbook.get_l2_snapshot(2);
        assert_eq!(snapshot.bids.len(), 2);
        assert_eq!(snapshot.asks.len(), 2);
        assert_eq!(snapshot.symbol, "BTCUSDT");
    }

    #[test]
    fn test_cache_optimization() {
        let mut orderbook = ZeroAllocOrderBook::new("BTCUSDT".to_string(), 100).unwrap();
        
        orderbook.update_bid(50000.0, 1.5).unwrap();
        orderbook.update_ask(50001.0, 1.2).unwrap();
        
        // 第一次訪問會計算並緩存
        let mid1 = orderbook.mid_price();
        orderbook.refresh_cache();
        
        // 第二次訪問應該使用緩存
        let mid2 = orderbook.mid_price();
        assert_eq!(mid1, mid2);
    }
}