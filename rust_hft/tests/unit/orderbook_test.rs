/*!
 * Lock-Free OrderBook 單元測試
 * 
 * 提取自 simple_test，專注於 OrderBook 功能測試
 */

use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::Instant;
use dashmap::DashMap;
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;

type Price = OrderedFloat<f64>;
type Quantity = Decimal;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Side {
    Bid,
    Ask,
}

/// Lock-Free OrderBook 實現
#[derive(Debug)]
pub struct LockFreeOrderBook {
    pub bids: DashMap<u64, Quantity>,
    pub asks: DashMap<u64, Quantity>,
    pub update_count: AtomicU64,
}

impl LockFreeOrderBook {
    pub fn new() -> Self {
        Self {
            bids: DashMap::new(),
            asks: DashMap::new(),
            update_count: AtomicU64::new(0),
        }
    }
    
    pub fn update(&self, side: Side, price: Price, quantity: Quantity) {
        let scaled_price = (price.into_inner() * 1e8) as u64;
        
        match side {
            Side::Bid => {
                if quantity > Decimal::ZERO {
                    self.bids.insert(scaled_price, quantity);
                } else {
                    self.bids.remove(&scaled_price);
                }
            },
            Side::Ask => {
                if quantity > Decimal::ZERO {
                    self.asks.insert(scaled_price, quantity);
                } else {
                    self.asks.remove(&scaled_price);
                }
            },
        }
        
        self.update_count.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn get_best_prices(&self) -> (Option<f64>, Option<f64>) {
        let best_bid = self.bids.iter()
            .map(|entry| *entry.key())
            .max()
            .map(|price| price as f64 / 1e8);
            
        let best_ask = self.asks.iter()
            .map(|entry| *entry.key())
            .min()
            .map(|price| price as f64 / 1e8);
            
        (best_bid, best_ask)
    }
    
    pub fn get_depth(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }
    
    pub fn get_update_count(&self) -> u64 {
        self.update_count.load(Ordering::Relaxed)
    }
}

/// 傳統 OrderBook 實現（用於比較）
#[derive(Debug)]
pub struct TraditionalOrderBook {
    pub bids: std::collections::BTreeMap<Price, Quantity>,
    pub asks: std::collections::BTreeMap<Price, Quantity>,
    pub update_count: u64,
}

impl TraditionalOrderBook {
    pub fn new() -> Self {
        Self {
            bids: std::collections::BTreeMap::new(),
            asks: std::collections::BTreeMap::new(),
            update_count: 0,
        }
    }
    
    pub fn update(&mut self, side: Side, price: Price, quantity: Quantity) {
        match side {
            Side::Bid => {
                if quantity > Decimal::ZERO {
                    self.bids.insert(price, quantity);
                } else {
                    self.bids.remove(&price);
                }
            },
            Side::Ask => {
                if quantity > Decimal::ZERO {
                    self.asks.insert(price, quantity);
                } else {
                    self.asks.remove(&price);
                }
            },
        }
        
        self.update_count += 1;
    }
    
    pub fn get_best_prices(&self) -> (Option<f64>, Option<f64>) {
        let best_bid = self.bids.keys().next_back().map(|p| p.into_inner());
        let best_ask = self.asks.keys().next().map(|p| p.into_inner());
        (best_bid, best_ask)
    }
    
    pub fn get_depth(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }
}

/// 生成測試數據
fn generate_test_data(count: usize) -> Vec<(Side, Price, Quantity)> {
    let mut data = Vec::with_capacity(count);
    
    for i in 0..count {
        let side = if i % 2 == 0 { Side::Bid } else { Side::Ask };
        let base_price = if side == Side::Bid { 100.0 } else { 100.01 };
        let price = Price::from(base_price + (i as f64 * 0.0001));
        let quantity = Decimal::try_from(10.0 + (i as f64 % 50.0)).unwrap();
        data.push((side, price, quantity));
    }
    
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::PerformanceHelper;
    
    #[test]
    fn test_lockfree_orderbook_basic_operations() {
        let orderbook = LockFreeOrderBook::new();
        
        // 測試買單插入
        orderbook.update(Side::Bid, Price::from(100.0), Decimal::try_from(10.0).unwrap());
        orderbook.update(Side::Bid, Price::from(99.99), Decimal::try_from(5.0).unwrap());
        
        // 測試賣單插入
        orderbook.update(Side::Ask, Price::from(100.01), Decimal::try_from(15.0).unwrap());
        orderbook.update(Side::Ask, Price::from(100.02), Decimal::try_from(8.0).unwrap());
        
        let (bid_depth, ask_depth) = orderbook.get_depth();
        assert_eq!(bid_depth, 2);
        assert_eq!(ask_depth, 2);
        
        let (best_bid, best_ask) = orderbook.get_best_prices();
        assert!(best_bid.is_some());
        assert!(best_ask.is_some());
        assert_eq!(best_bid.unwrap(), 100.0);
        assert_eq!(best_ask.unwrap(), 100.01);
        
        assert_eq!(orderbook.get_update_count(), 4);
    }
    
    #[test]
    fn test_lockfree_orderbook_remove_operations() {
        let orderbook = LockFreeOrderBook::new();
        
        // 插入訂單
        orderbook.update(Side::Bid, Price::from(100.0), Decimal::try_from(10.0).unwrap());
        assert_eq!(orderbook.get_depth().0, 1);
        
        // 移除訂單
        orderbook.update(Side::Bid, Price::from(100.0), Decimal::ZERO);
        assert_eq!(orderbook.get_depth().0, 0);
    }
    
    #[test]
    fn test_performance_comparison() {
        let test_data = generate_test_data(10000);
        
        // 測試傳統 OrderBook
        let traditional_helper = PerformanceHelper::new();
        let mut traditional_ob = TraditionalOrderBook::new();
        
        for (side, price, quantity) in &test_data {
            let start = Instant::now();
            traditional_ob.update(*side, *price, *quantity);
            let latency_ns = start.elapsed().as_nanos() as u64;
            traditional_helper.record_operation(latency_ns);
        }
        
        // 測試 Lock-Free OrderBook
        let lockfree_helper = PerformanceHelper::new();
        let lockfree_ob = LockFreeOrderBook::new();
        
        for (side, price, quantity) in &test_data {
            let start = Instant::now();
            lockfree_ob.update(*side, *price, *quantity);
            let latency_ns = start.elapsed().as_nanos() as u64;
            lockfree_helper.record_operation(latency_ns);
        }
        
        let (traditional_count, traditional_avg, traditional_throughput) = traditional_helper.get_stats();
        let (lockfree_count, lockfree_avg, lockfree_throughput) = lockfree_helper.get_stats();
        
        println!("Traditional OrderBook: {} ops, {:.2}ns avg, {:.0} ops/s", 
                 traditional_count, traditional_avg, traditional_throughput);
        println!("Lock-Free OrderBook: {} ops, {:.2}ns avg, {:.0} ops/s", 
                 lockfree_count, lockfree_avg, lockfree_throughput);
        
        assert_eq!(traditional_count, test_data.len() as u64);
        assert_eq!(lockfree_count, test_data.len() as u64);
        
        // Lock-Free 應該比傳統方式快（在大部分情況下）
        // 注意：在單線程測試中可能不會明顯，但在並發環境中會有顯著差異
    }
    
    #[tokio::test]
    async fn test_concurrent_access() {
        let orderbook = Arc::new(LockFreeOrderBook::new());
        let test_data = Arc::new(generate_test_data(1000));
        
        let handles: Vec<_> = (0..4)
            .map(|thread_id| {
                let ob = orderbook.clone();
                let data = test_data.clone();
                let chunk_size = 250;
                
                tokio::spawn(async move {
                    let start_idx = thread_id * chunk_size;
                    let end_idx = (thread_id + 1) * chunk_size;
                    
                    for (side, price, quantity) in &data[start_idx..end_idx] {
                        ob.update(*side, *price, *quantity);
                    }
                })
            })
            .collect();
        
        for handle in handles {
            handle.await.unwrap();
        }
        
        assert_eq!(orderbook.get_update_count(), 1000);
        
        let (bid_depth, ask_depth) = orderbook.get_depth();
        println!("並發測試後深度: 買單 {}, 賣單 {}", bid_depth, ask_depth);
        
        let (best_bid, best_ask) = orderbook.get_best_prices();
        println!("最佳價格: 買 {:?}, 賣 {:?}", best_bid, best_ask);
        
        // 驗證數據完整性
        assert!(bid_depth > 0);
        assert!(ask_depth > 0);
        assert!(best_bid.is_some());
        assert!(best_ask.is_some());
    }
    
    #[test]
    fn test_price_precision() {
        let orderbook = LockFreeOrderBook::new();
        
        // 測試高精度價格
        let high_precision_price = Price::from(123.456789);
        orderbook.update(Side::Bid, high_precision_price, Decimal::try_from(1.0).unwrap());
        
        let (best_bid, _) = orderbook.get_best_prices();
        assert!(best_bid.is_some());
        
        // 驗證精度保持
        let retrieved_price = best_bid.unwrap();
        assert!((retrieved_price - 123.456789).abs() < 1e-6);
    }
}