/*!
 * High-Performance OrderBook implementation for Rust HFT
 * 
 * Uses BTreeMap for O(log n) operations with automatic sorting
 * Optimized for minimal latency and maximum throughput
 */

use crate::types::*;
use anyhow::Result;
use std::collections::BTreeMap;
use tracing::{debug, warn, error};
use rust_decimal::prelude::ToPrimitive;

/// High-performance OrderBook with BTreeMap backend
#[derive(Debug, Clone)]
pub struct OrderBook {
    /// Buy orders: price -> quantity (automatically sorted high to low)
    pub bids: BTreeMap<Price, Quantity>,
    
    /// Sell orders: price -> quantity (automatically sorted low to high)  
    pub asks: BTreeMap<Price, Quantity>,
    
    /// Symbol identifier
    pub symbol: String,
    
    /// Last update timestamp
    pub last_update: Timestamp,
    
    /// Sequence number for data integrity
    pub last_sequence: u64,
    
    /// Total number of updates processed
    pub update_count: u64,
    
    /// Quality metrics
    pub data_quality_score: f64,
    
    /// Validation flags
    pub is_valid: bool,
}

impl OrderBook {
    /// Create new empty OrderBook
    pub fn new(symbol: String) -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            symbol,
            last_update: now_micros(),
            last_sequence: 0,
            update_count: 0,
            data_quality_score: 1.0,
            is_valid: false,
        }
    }

    /// Initialize with full snapshot
    pub fn init_snapshot(&mut self, update: OrderBookUpdate) -> Result<()> {
        let start_time = now_micros();

        // Clear existing data
        self.bids.clear();
        self.asks.clear();

        // Load bids
        for level in &update.bids {
            if level.quantity > Quantity::ZERO {
                self.bids.insert(level.price, level.quantity);
            }
        }

        // Load asks
        for level in &update.asks {
            if level.quantity > Quantity::ZERO {
                self.asks.insert(level.price, level.quantity);
            }
        }

        // Update metadata
        self.last_update = update.timestamp;
        self.last_sequence = update.sequence_end;
        self.update_count += 1;
        self.is_valid = self.validate_orderbook();

        let latency = now_micros() - start_time;
        debug!("OrderBook snapshot initialized: {} bids, {} asks, {}μs", 
               self.bids.len(), self.asks.len(), latency);

        Ok(())
    }

    /// Apply incremental update
    pub fn apply_update(&mut self, update: OrderBookUpdate) -> Result<()> {
        let start_time = now_micros();

        // Sequence validation
        if update.sequence_start != self.last_sequence + 1 {
            warn!("Sequence gap detected: expected {}, got {}", 
                  self.last_sequence + 1, update.sequence_start);
            return Err(anyhow::anyhow!("Sequence gap: need snapshot refresh"));
        }

        // Apply bid updates
        for level in &update.bids {
            if level.quantity == Quantity::ZERO {
                // Remove price level
                self.bids.remove(&level.price);
                debug!("Removed bid level: {}", level.price);
            } else {
                // Update price level
                self.bids.insert(level.price, level.quantity);
                debug!("Updated bid level: {} @ {}", level.quantity, level.price);
            }
        }

        // Apply ask updates
        for level in &update.asks {
            if level.quantity == Quantity::ZERO {
                // Remove price level
                self.asks.remove(&level.price);
                debug!("Removed ask level: {}", level.price);
            } else {
                // Update price level
                self.asks.insert(level.price, level.quantity);
                debug!("Updated ask level: {} @ {}", level.quantity, level.price);
            }
        }

        // Update metadata
        self.last_update = update.timestamp;
        self.last_sequence = update.sequence_end;
        self.update_count += 1;
        self.is_valid = self.validate_orderbook();

        let latency = now_micros() - start_time;
        debug!("OrderBook update applied: {}μs", latency);

        Ok(())
    }

    /// Get best bid price
    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().last().copied()
    }

    /// Get best ask price
    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    /// Get mid price
    pub fn mid_price(&self) -> Option<Price> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(((bid.0 + ask.0) / 2.0).to_price()),
            _ => None,
        }
    }

    /// Get spread in price units
    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(ask.0 - bid.0),
            _ => None,
        }
    }

    /// Get spread in basis points
    pub fn spread_bps(&self) -> Option<f64> {
        match (self.spread(), self.mid_price()) {
            (Some(spread), Some(mid)) if mid.0 > 0.0 => {
                Some((spread / mid.0) * 10000.0)
            },
            _ => None,
        }
    }

    /// Calculate depth for specified number of levels
    pub fn calculate_depth(&self, levels: usize, side: Side) -> f64 {
        match side {
            Side::Bid => {
                self.bids.iter()
                    .rev()  // Start from highest bid
                    .take(levels)
                    .map(|(price, qty)| price.0 * qty.to_f64().unwrap_or(0.0))
                    .sum()
            },
            Side::Ask => {
                self.asks.iter()
                    .take(levels)  // Start from lowest ask
                    .map(|(price, qty)| price.0 * qty.to_f64().unwrap_or(0.0))
                    .sum()
            },
        }
    }

    /// Calculate Order Book Imbalance for specified levels
    pub fn calculate_obi(&self, levels: usize) -> f64 {
        let bid_depth = self.calculate_depth(levels, Side::Bid);
        let ask_depth = self.calculate_depth(levels, Side::Ask);
        let total_depth = bid_depth + ask_depth;

        if total_depth > 0.0 {
            (bid_depth - ask_depth) / total_depth
        } else {
            0.0
        }
    }

    /// Calculate multi-level OBI values
    pub fn calculate_multi_obi(&self) -> (f64, f64, f64, f64) {
        (
            self.calculate_obi(1),   // L1 OBI
            self.calculate_obi(5),   // L5 OBI
            self.calculate_obi(10),  // L10 OBI
            self.calculate_obi(20),  // L20 OBI
        )
    }

    /// Calculate depth imbalance for specified levels
    pub fn calculate_depth_imbalance(&self, levels: usize) -> f64 {
        let bid_depth = self.calculate_depth(levels, Side::Bid);
        let ask_depth = self.calculate_depth(levels, Side::Ask);
        let total_depth = bid_depth + ask_depth;

        if total_depth > 0.0 {
            (bid_depth - ask_depth) / total_depth
        } else {
            0.0
        }
    }

    /// Calculate orderbook slope (price elasticity)
    pub fn calculate_slope(&self, side: Side) -> f64 {
        match side {
            Side::Bid => {
                let levels: Vec<_> = self.bids.iter().rev().take(3).collect();
                if levels.len() >= 2 {
                    let price_diff = levels[0].0.0 - levels[levels.len()-1].0.0;
                    let qty_diff = levels[0].1.to_f64().unwrap_or(0.0) - 
                                  levels[levels.len()-1].1.to_f64().unwrap_or(0.0);
                    if price_diff.abs() > 0.01 {
                        qty_diff / price_diff.abs()
                    } else {
                        0.0
                    }
                } else {
                    0.0
                }
            },
            Side::Ask => {
                let levels: Vec<_> = self.asks.iter().take(3).collect();
                if levels.len() >= 2 {
                    let price_diff = levels[levels.len()-1].0.0 - levels[0].0.0;
                    let qty_diff = levels[levels.len()-1].1.to_f64().unwrap_or(0.0) - 
                                  levels[0].1.to_f64().unwrap_or(0.0);
                    if price_diff.abs() > 0.01 {
                        qty_diff / price_diff.abs()
                    } else {
                        0.0
                    }
                } else {
                    0.0
                }
            },
        }
    }

    /// Validate orderbook integrity
    pub fn validate_orderbook(&self) -> bool {
        // Check if we have data
        if self.bids.is_empty() || self.asks.is_empty() {
            warn!("Empty orderbook detected");
            return false;
        }

        // Check bid-ask spread
        if let (Some(best_bid), Some(best_ask)) = (self.best_bid(), self.best_ask()) {
            if best_bid >= best_ask {
                error!("Invalid spread: bid {} >= ask {}", best_bid, best_ask);
                return false;
            }
        }

        // Check for negative quantities
        for (price, qty) in &self.bids {
            if *qty <= Quantity::ZERO {
                error!("Invalid bid quantity: {} @ {}", qty, price);
                return false;
            }
        }

        for (price, qty) in &self.asks {
            if *qty <= Quantity::ZERO {
                error!("Invalid ask quantity: {} @ {}", qty, price);
                return false;
            }
        }

        true
    }

    /// Get orderbook statistics
    pub fn get_stats(&self) -> OrderBookStats {
        OrderBookStats {
            bid_levels: self.bids.len(),
            ask_levels: self.asks.len(),
            best_bid: self.best_bid(),
            best_ask: self.best_ask(),
            mid_price: self.mid_price(),
            spread: self.spread(),
            spread_bps: self.spread_bps(),
            last_update: self.last_update,
            update_count: self.update_count,
            is_valid: self.is_valid,
            data_quality_score: self.data_quality_score,
        }
    }

    /// Estimate market impact for given order size
    pub fn estimate_market_impact(&self, side: Side, quantity: Quantity) -> f64 {
        let mut remaining_qty = quantity;
        let mut total_cost = 0.0;
        let mut weighted_price = 0.0;

        let levels: Box<dyn Iterator<Item = (&Price, &Quantity)>> = match side {
            Side::Bid => {
                // For buy orders, consume ask side
                Box::new(self.asks.iter())
            },
            Side::Ask => {
                // For sell orders, consume bid side  
                Box::new(self.bids.iter().rev())
            },
        };

        for (price, available_qty) in levels {
            if remaining_qty <= Quantity::ZERO {
                break;
            }

            let consumed_qty = remaining_qty.min(*available_qty);
            let consumed_qty_f64 = consumed_qty.to_f64().unwrap_or(0.0);
            let cost = consumed_qty_f64 * price.0;
            
            total_cost += cost;
            weighted_price += cost;
            remaining_qty -= consumed_qty;
        }

        if total_cost > 0.0 {
            weighted_price / total_cost
        } else {
            0.0
        }
    }

    /// Get volume at specific price level
    pub fn get_volume_at_price(&self, price: f64, side: Side) -> f64 {
        let price_key = Price::from(price);
        match side {
            Side::Bid => self.bids.get(&price_key).map(|q| q.to_f64().unwrap_or(0.0)).unwrap_or(0.0),
            Side::Ask => self.asks.get(&price_key).map(|q| q.to_f64().unwrap_or(0.0)).unwrap_or(0.0),
        }
    }

    /// Get price and quantity at specific level (0-indexed)
    pub fn get_level(&self, level: usize, side: Side) -> Option<(f64, f64)> {
        match side {
            Side::Bid => {
                self.bids.iter()
                    .rev()
                    .nth(level)
                    .map(|(price, qty)| (price.0, qty.to_f64().unwrap_or(0.0)))
            },
            Side::Ask => {
                self.asks.iter()
                    .nth(level)
                    .map(|(price, qty)| (price.0, qty.to_f64().unwrap_or(0.0)))
            },
        }
    }
}

/// OrderBook statistics
#[derive(Debug, Clone)]
pub struct OrderBookStats {
    pub bid_levels: usize,
    pub ask_levels: usize,
    pub best_bid: Option<Price>,
    pub best_ask: Option<Price>,
    pub mid_price: Option<Price>,
    pub spread: Option<f64>,
    pub spread_bps: Option<f64>,
    pub last_update: Timestamp,
    pub update_count: u64,
    pub is_valid: bool,
    pub data_quality_score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orderbook_basic_operations() {
        let mut ob = OrderBook::new("BTCUSDT".to_string());
        
        // Create test update
        let update = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![
                PriceLevel { price: 100.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
                PriceLevel { price: 99.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Bid },
            ],
            asks: vec![
                PriceLevel { price: 101.0.to_price(), quantity: "1.5".to_quantity(), side: Side::Ask },
                PriceLevel { price: 102.0.to_price(), quantity: "2.5".to_quantity(), side: Side::Ask },
            ],
            timestamp: now_micros(),
            sequence_start: 1,
            sequence_end: 1,
            is_snapshot: true,
        };

        // Initialize with snapshot
        ob.init_snapshot(update).unwrap();

        // Test basic functions
        assert_eq!(ob.best_bid(), Some(100.0.to_price()));
        assert_eq!(ob.best_ask(), Some(101.0.to_price()));
        assert_eq!(ob.spread(), Some(1.0));
        assert!(ob.is_valid);

        // Test OBI calculation
        let obi = ob.calculate_obi(2);
        assert!(obi.abs() < 1.0); // Should be between -1 and 1
    }

    #[test]
    fn test_orderbook_validation() {
        let mut ob = OrderBook::new("BTCUSDT".to_string());
        
        // Invalid orderbook (bid >= ask)
        let invalid_update = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![
                PriceLevel { price: 102.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
            ],
            asks: vec![
                PriceLevel { price: 101.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
            ],
            timestamp: now_micros(),
            sequence_start: 1,
            sequence_end: 1,
            is_snapshot: true,
        };

        ob.init_snapshot(invalid_update).unwrap();
        assert!(!ob.is_valid); // Should be invalid due to crossed spread
    }
}