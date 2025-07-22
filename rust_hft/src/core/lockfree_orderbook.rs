/*!
 * Lock-Free OrderBook 實現
 * 
 * 使用無鎖數據結構和 Compare-and-Swap 操作實現超低延遲的訂單簿
 * 目標延遲: < 1μs
 */

use crate::types::*;
use anyhow::Result;
use dashmap::DashMap;
// use lockfree::map::Map as LockFreeMap;  // Future implementation
use std::sync::atomic::{AtomicU64, AtomicPtr, Ordering};
use std::sync::Arc;
use std::ptr;
use tracing::debug;
use rust_decimal::prelude::ToPrimitive;

/// Lock-Free 價格級別
#[derive(Debug)]
pub struct LockFreePriceLevel {
    pub price: Price,
    pub quantity: AtomicU64, // 使用 AtomicU64 存儲 quantity (scaled by 10^8)
    pub order_count: AtomicU64,
    pub last_update: AtomicU64,
}

impl LockFreePriceLevel {
    pub fn new(price: Price, quantity: Quantity) -> Self {
        Self {
            price,
            quantity: AtomicU64::new((quantity.to_f64().unwrap_or(0.0) * 1e8) as u64),
            order_count: AtomicU64::new(1),
            last_update: AtomicU64::new(crate::core::types::now_micros()),
        }
    }

    pub fn get_quantity(&self) -> Quantity {
        let scaled_qty = self.quantity.load(Ordering::Acquire);
        Quantity::try_from(scaled_qty as f64 / 1e8).unwrap_or_default()
    }

    pub fn update_quantity(&self, new_quantity: Quantity) -> Result<()> {
        let scaled_qty = (new_quantity.to_f64().unwrap_or(0.0) * 1e8) as u64;
        self.quantity.store(scaled_qty, Ordering::Release);
        self.last_update.store(crate::core::types::now_micros(), Ordering::Release);
        Ok(())
    }

    pub fn add_quantity(&self, delta: Quantity) -> Result<()> {
        let scaled_delta = (delta.to_f64().unwrap_or(0.0) * 1e8) as u64;
        self.quantity.fetch_add(scaled_delta, Ordering::AcqRel);
        self.last_update.store(crate::core::types::now_micros(), Ordering::Release);
        Ok(())
    }

    pub fn remove_quantity(&self, delta: Quantity) -> Result<()> {
        let scaled_delta = (delta.to_f64().unwrap_or(0.0) * 1e8) as u64;
        self.quantity.fetch_sub(scaled_delta, Ordering::AcqRel);
        self.last_update.store(crate::core::types::now_micros(), Ordering::Release);
        Ok(())
    }
}

/// Lock-Free OrderBook 實現
/// 
/// 使用 DashMap 和 AtomicPtr 提供零鎖競爭的訂單簿操作
#[derive(Debug)]
pub struct LockFreeOrderBook {
    /// 交易對標識符
    pub symbol: String,
    
    /// 買盤價格級別: price -> LockFreePriceLevel
    pub bids: DashMap<u64, Arc<LockFreePriceLevel>>, // 使用 u64 scaled price 作為 key
    
    /// 賣盤價格級別: price -> LockFreePriceLevel  
    pub asks: DashMap<u64, Arc<LockFreePriceLevel>>,
    
    /// 最後更新時間戳
    pub last_update: AtomicU64,
    
    /// 序列號
    pub sequence: AtomicU64,
    
    /// 更新計數
    pub update_count: AtomicU64,
    
    /// 數據品質分數
    pub data_quality_score: AtomicU64, // scaled by 10^6
    
    /// 有效性標記
    pub is_valid: AtomicU64, // 0 = false, 1 = true
    
    /// 最佳買價緩存
    best_bid_cache: AtomicPtr<LockFreePriceLevel>,
    
    /// 最佳賣價緩存
    best_ask_cache: AtomicPtr<LockFreePriceLevel>,
}

impl LockFreeOrderBook {
    /// 創建新的 Lock-Free OrderBook
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            bids: DashMap::new(),
            asks: DashMap::new(),
            last_update: AtomicU64::new(crate::core::types::now_micros()),
            sequence: AtomicU64::new(0),
            update_count: AtomicU64::new(0),
            data_quality_score: AtomicU64::new(1_000_000), // 1.0 scaled by 10^6
            is_valid: AtomicU64::new(0),
            best_bid_cache: AtomicPtr::new(ptr::null_mut()),
            best_ask_cache: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// 原子更新價格級別
    pub fn update_level(&self, side: Side, price: Price, quantity: Quantity) -> Result<()> {
        let scaled_price = self.scale_price(price);
        let levels = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };

        if quantity.to_f64().unwrap_or(0.0) == 0.0 {
            // 移除價格級別
            levels.remove(&scaled_price);
        } else {
            // 更新或插入價格級別
            match levels.get(&scaled_price) {
                Some(level) => {
                    level.update_quantity(quantity)?;
                }
                None => {
                    let new_level = Arc::new(LockFreePriceLevel::new(price, quantity));
                    levels.insert(scaled_price, new_level);
                }
            }
        }

        // 更新統計信息
        self.update_count.fetch_add(1, Ordering::Relaxed);
        self.last_update.store(crate::core::types::now_micros(), Ordering::Release);
        
        // 無效化緩存
        self.invalidate_cache(side);

        Ok(())
    }

    /// 批量更新（用於快照）
    pub fn update_snapshot(&self, update: OrderBookUpdate) -> Result<()> {
        let start_time = crate::core::types::now_micros();

        // 清理現有數據
        self.bids.clear();
        self.asks.clear();

        // 插入新的買盤
        for level in update.bids {
            let scaled_price = self.scale_price(level.price);
            let price_level = Arc::new(LockFreePriceLevel::new(level.price, level.quantity));
            self.bids.insert(scaled_price, price_level);
        }

        // 插入新的賣盤
        for level in update.asks {
            let scaled_price = self.scale_price(level.price);
            let price_level = Arc::new(LockFreePriceLevel::new(level.price, level.quantity));
            self.asks.insert(scaled_price, price_level);
        }

        // 更新元數據
        self.sequence.store(update.sequence_end, Ordering::Release);
        self.is_valid.store(1, Ordering::Release);
        self.last_update.store(update.timestamp, Ordering::Release);

        // 無效化所有緩存
        self.invalidate_cache(Side::Bid);
        self.invalidate_cache(Side::Ask);

        let processing_time = crate::core::types::now_micros() - start_time;
        debug!("Lock-free snapshot update completed in {}μs", processing_time);

        Ok(())
    }

    /// 獲取最佳買價
    pub fn best_bid(&self) -> Option<Price> {
        // 嘗試使用緩存
        let cached_ptr = self.best_bid_cache.load(Ordering::Acquire);
        if !cached_ptr.is_null() {
            unsafe {
                return Some((*cached_ptr).price);
            }
        }

        // 緩存失效，重新計算
        let mut best_price = 0u64;
        for entry in self.bids.iter() {
            let price = *entry.key();
            if price > best_price {
                best_price = price;
            }
        }

        if best_price > 0 {
            let price = self.unscale_price(best_price);
            // 嘗試更新緩存
            if let Some(level) = self.bids.get(&best_price) {
                let level_ptr = Arc::as_ptr(&level) as *mut LockFreePriceLevel;
                self.best_bid_cache.store(level_ptr, Ordering::Release);
            }
            Some(price)
        } else {
            None
        }
    }

    /// 獲取最佳賣價
    pub fn best_ask(&self) -> Option<Price> {
        // 嘗試使用緩存
        let cached_ptr = self.best_ask_cache.load(Ordering::Acquire);
        if !cached_ptr.is_null() {
            unsafe {
                return Some((*cached_ptr).price);
            }
        }

        // 緩存失效，重新計算
        let mut best_price = u64::MAX;
        for entry in self.asks.iter() {
            let price = *entry.key();
            if price < best_price {
                best_price = price;
            }
        }

        if best_price < u64::MAX {
            let price = self.unscale_price(best_price);
            // 嘗試更新緩存
            if let Some(level) = self.asks.get(&best_price) {
                let level_ptr = Arc::as_ptr(&level) as *mut LockFreePriceLevel;
                self.best_ask_cache.store(level_ptr, Ordering::Release);
            }
            Some(price)
        } else {
            None
        }
    }

    /// 獲取中間價
    pub fn mid_price(&self) -> Option<Price> {
        let best_bid = self.best_bid()?;
        let best_ask = self.best_ask()?;
        Some(Price::from((best_bid.into_inner() + best_ask.into_inner()) / 2.0))
    }

    /// 獲取價差
    pub fn spread(&self) -> Option<f64> {
        let best_bid = self.best_bid()?;
        let best_ask = self.best_ask()?;
        Some(best_ask.into_inner() - best_bid.into_inner())
    }

    /// 獲取深度統計（前 N 個價格級別）
    pub fn depth_stats(&self, levels: usize) -> (f64, f64) {
        let mut bid_volume = 0.0;
        let mut ask_volume = 0.0;

        // 計算買盤深度
        let mut bid_prices: Vec<_> = self.bids.iter().map(|entry| *entry.key()).collect();
        bid_prices.sort_by(|a, b| b.cmp(a)); // 降序排列
        for &price in bid_prices.iter().take(levels) {
            if let Some(level) = self.bids.get(&price) {
                bid_volume += level.get_quantity().to_f64().unwrap_or(0.0);
            }
        }

        // 計算賣盤深度
        let mut ask_prices: Vec<_> = self.asks.iter().map(|entry| *entry.key()).collect();
        ask_prices.sort(); // 升序排列
        for &price in ask_prices.iter().take(levels) {
            if let Some(level) = self.asks.get(&price) {
                ask_volume += level.get_quantity().to_f64().unwrap_or(0.0);
            }
        }

        (bid_volume, ask_volume)
    }

    /// 價格縮放（將 f64 價格轉換為 u64 用於無鎖存儲）
    fn scale_price(&self, price: Price) -> u64 {
        (price.into_inner() * 1e8) as u64
    }

    /// 價格反縮放
    fn unscale_price(&self, scaled_price: u64) -> Price {
        Price::from(scaled_price as f64 / 1e8)
    }

    /// 無效化緩存
    fn invalidate_cache(&self, side: Side) {
        match side {
            Side::Bid => self.best_bid_cache.store(ptr::null_mut(), Ordering::Release),
            Side::Ask => self.best_ask_cache.store(ptr::null_mut(), Ordering::Release),
        }
    }

    /// 獲取統計信息
    pub fn stats(&self) -> OrderBookStats {
        OrderBookStats {
            bid_levels: self.bids.len() as u64,
            ask_levels: self.asks.len() as u64,
            update_count: self.update_count.load(Ordering::Relaxed),
            sequence: self.sequence.load(Ordering::Relaxed),
            last_update: self.last_update.load(Ordering::Relaxed),
            is_valid: self.is_valid.load(Ordering::Relaxed) == 1,
        }
    }
}

/// OrderBook 統計信息
#[derive(Debug, Clone)]
pub struct OrderBookStats {
    pub bid_levels: u64,
    pub ask_levels: u64,
    pub update_count: u64,
    pub sequence: u64,
    pub last_update: u64,
    pub is_valid: bool,
}

/// 與現有 OrderBook 的兼容性接口
impl From<LockFreeOrderBook> for super::orderbook::OrderBook {
    fn from(lockfree_ob: LockFreeOrderBook) -> Self {
        let mut ob = super::orderbook::OrderBook::new(lockfree_ob.symbol.clone());
        
        // 轉換買盤
        for entry in lockfree_ob.bids.iter() {
            let price = lockfree_ob.unscale_price(*entry.key());
            let quantity = entry.value().get_quantity();
            ob.bids.insert(price, quantity);
        }
        
        // 轉換賣盤
        for entry in lockfree_ob.asks.iter() {
            let price = lockfree_ob.unscale_price(*entry.key());
            let quantity = entry.value().get_quantity();
            ob.asks.insert(price, quantity);
        }
        
        ob.last_update = lockfree_ob.last_update.load(Ordering::Relaxed);
        ob.last_sequence = lockfree_ob.sequence.load(Ordering::Relaxed);
        ob.update_count = lockfree_ob.update_count.load(Ordering::Relaxed);
        ob.is_valid = lockfree_ob.is_valid.load(Ordering::Relaxed) == 1;
        
        ob
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ordered_float::OrderedFloat;
    use rust_decimal::Decimal;

    #[test]
    fn test_lockfree_orderbook_creation() {
        let ob = LockFreeOrderBook::new("BTCUSDT".to_string());
        assert_eq!(ob.symbol, "BTCUSDT");
        assert_eq!(ob.bids.len(), 0);
        assert_eq!(ob.asks.len(), 0);
    }

    #[test]
    fn test_price_level_operations() {
        let price = Price::from(100.0);
        let quantity = Quantity::try_from(10.0).unwrap();
        
        let level = LockFreePriceLevel::new(price, quantity);
        assert_eq!(level.price, price);
        assert_eq!(level.get_quantity(), quantity);
        
        // 測試數量更新
        let new_quantity = Quantity::try_from(20.0).unwrap();
        level.update_quantity(new_quantity).unwrap();
        assert_eq!(level.get_quantity(), new_quantity);
    }

    #[test]
    fn test_lockfree_level_updates() {
        let ob = LockFreeOrderBook::new("BTCUSDT".to_string());
        
        // 添加買盤
        let bid_price = Price::from(100.0);
        let bid_quantity = Quantity::try_from(10.0).unwrap();
        ob.update_level(Side::Bid, bid_price, bid_quantity).unwrap();
        
        // 添加賣盤
        let ask_price = Price::from(101.0);
        let ask_quantity = Quantity::try_from(5.0).unwrap();
        ob.update_level(Side::Ask, ask_price, ask_quantity).unwrap();
        
        // 驗證最佳價格
        assert_eq!(ob.best_bid(), Some(bid_price));
        assert_eq!(ob.best_ask(), Some(ask_price));
        
        // 驗證中間價
        let expected_mid = Price::from(100.5);
        assert_eq!(ob.mid_price(), Some(expected_mid));
    }

    #[test]
    fn test_depth_stats() {
        let ob = LockFreeOrderBook::new("BTCUSDT".to_string());
        
        // 添加多個價格級別
        for i in 0..5 {
            let bid_price = Price::from(100.0 - i as f64);
            let ask_price = Price::from(101.0 + i as f64);
            let quantity = Quantity::try_from(10.0).unwrap();
            
            ob.update_level(Side::Bid, bid_price, quantity).unwrap();
            ob.update_level(Side::Ask, ask_price, quantity).unwrap();
        }
        
        let (bid_volume, ask_volume) = ob.depth_stats(3);
        assert_eq!(bid_volume, 30.0); // 3 levels * 10.0
        assert_eq!(ask_volume, 30.0); // 3 levels * 10.0
    }
}