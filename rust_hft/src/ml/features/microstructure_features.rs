/*!
 * Microstructure Features - 市场微结构特征
 * 
 * 计算订单簿失衡、深度特征、价格弹性等市场微结构指标
 * 从原 features.rs 的微结构部分提取
 */

use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use anyhow::Result;
use std::collections::VecDeque;

/// Microstructure features from orderbook
#[derive(Debug, Clone)]
pub struct MicrostructureFeatures {
    // Order Book Imbalance features
    pub obi_l1: f64,
    pub obi_l5: f64,
    pub obi_l10: f64,
    pub obi_l20: f64,
    
    // Depth features
    pub bid_depth_l5: f64,
    pub ask_depth_l5: f64,
    pub bid_depth_l10: f64,
    pub ask_depth_l10: f64,
    pub bid_depth_l20: f64,
    pub ask_depth_l20: f64,
    
    // Depth imbalances
    pub depth_imbalance_l5: f64,
    pub depth_imbalance_l10: f64,
    pub depth_imbalance_l20: f64,
    
    // Price slopes (elasticity)
    pub bid_slope: f64,
    pub ask_slope: f64,
    
    // Microprice and VWAP
    pub microprice: f64,
    pub vwap: f64,
    
    // Order flow metrics
    pub order_flow_imbalance: f64,
    pub effective_spread: f64,
    pub market_impact: f64,
    pub liquidity_score: f64,
    
    // Pressure metrics
    pub depth_pressure_bid: f64,
    pub depth_pressure_ask: f64,
}

/// Microstructure feature extractor
#[derive(Debug)]
pub struct MicrostructureExtractor {
    /// Historical OBI values for trend analysis
    obi_history: VecDeque<f64>,
    
    /// Historical microprice values
    microprice_history: VecDeque<f64>,
    
    /// Historical VWAP values
    vwap_history: VecDeque<f64>,
    
    /// Historical order flow imbalance
    order_flow_history: VecDeque<f64>,
    
    /// Previous orderbook snapshot for delta calculations
    prev_orderbook_snapshot: Option<OrderBookSnapshot>,
    
    /// Window size for moving calculations
    window_size: usize,
}

impl MicrostructureExtractor {
    /// Create new microstructure feature extractor
    pub fn new(window_size: usize) -> Self {
        Self {
            obi_history: VecDeque::with_capacity(window_size),
            microprice_history: VecDeque::with_capacity(window_size),
            vwap_history: VecDeque::with_capacity(window_size),
            order_flow_history: VecDeque::with_capacity(window_size),
            prev_orderbook_snapshot: None,
            window_size,
        }
    }

    /// Extract microstructure features from orderbook
    pub fn extract(&mut self, orderbook: &OrderBook) -> Result<MicrostructureFeatures> {
        // Calculate multi-level OBI
        let (obi_l1, obi_l5, obi_l10, obi_l20) = orderbook.calculate_multi_obi();

        // Calculate depth features
        let bid_depth_l5 = orderbook.calculate_depth(5, Side::Bid);
        let ask_depth_l5 = orderbook.calculate_depth(5, Side::Ask);
        let bid_depth_l10 = orderbook.calculate_depth(10, Side::Bid);
        let ask_depth_l10 = orderbook.calculate_depth(10, Side::Ask);
        let bid_depth_l20 = orderbook.calculate_depth(20, Side::Bid);
        let ask_depth_l20 = orderbook.calculate_depth(20, Side::Ask);

        // Calculate depth imbalances
        let depth_imbalance_l5 = orderbook.calculate_depth_imbalance(5);
        let depth_imbalance_l10 = orderbook.calculate_depth_imbalance(10);
        let depth_imbalance_l20 = orderbook.calculate_depth_imbalance(20);

        // Calculate slopes (price elasticity)
        let bid_slope = orderbook.calculate_slope(Side::Bid);
        let ask_slope = orderbook.calculate_slope(Side::Ask);

        // Calculate enhanced features
        let microprice = self.calculate_microprice(orderbook);
        let vwap = self.calculate_vwap(orderbook);
        let order_flow_imbalance = self.calculate_order_flow_imbalance(orderbook);
        let effective_spread = self.calculate_effective_spread(orderbook);
        let market_impact = self.calculate_market_impact(orderbook);
        let liquidity_score = self.calculate_liquidity_score(orderbook);

        // Calculate depth pressure
        let (depth_pressure_bid, depth_pressure_ask) = self.calculate_depth_pressure(orderbook);

        // Update historical data
        self.update_history(obi_l10, microprice, vwap, order_flow_imbalance);

        // Save current orderbook snapshot for next iteration
        self.prev_orderbook_snapshot = Some(self.create_snapshot(orderbook));

        Ok(MicrostructureFeatures {
            obi_l1,
            obi_l5,
            obi_l10,
            obi_l20,
            bid_depth_l5,
            ask_depth_l5,
            bid_depth_l10,
            ask_depth_l10,
            bid_depth_l20,
            ask_depth_l20,
            depth_imbalance_l5,
            depth_imbalance_l10,
            depth_imbalance_l20,
            bid_slope,
            ask_slope,
            microprice,
            vwap,
            order_flow_imbalance,
            effective_spread,
            market_impact,
            liquidity_score,
            depth_pressure_bid,
            depth_pressure_ask,
        })
    }

    /// Calculate microprice (volume-weighted mid price)
    fn calculate_microprice(&self, orderbook: &OrderBook) -> f64 {
        let best_bid = orderbook.best_bid().unwrap_or(0.0.to_price());
        let best_ask = orderbook.best_ask().unwrap_or(0.0.to_price());
        
        if best_bid.0 == 0.0 || best_ask.0 == 0.0 {
            return orderbook.mid_price().unwrap_or(0.0.to_price()).0;
        }

        // Get volumes at best prices
        let bid_volume = orderbook.get_volume_at_price(best_bid, Side::Bid);
        let ask_volume = orderbook.get_volume_at_price(best_ask, Side::Ask);
        
        if bid_volume + ask_volume == 0.0 {
            return orderbook.mid_price().unwrap_or(0.0.to_price()).0;
        }

        // Volume-weighted microprice
        (best_bid.0 * ask_volume + best_ask.0 * bid_volume) / (bid_volume + ask_volume)
    }

    /// Calculate Volume Weighted Average Price
    fn calculate_vwap(&self, orderbook: &OrderBook) -> f64 {
        let mut total_volume = 0.0;
        let mut weighted_price = 0.0;

        // Calculate VWAP for top 10 levels
        for level in 1..=10 {
            if let (Some(bid), Some(ask)) = (orderbook.get_level(level, Side::Bid), orderbook.get_level(level, Side::Ask)) {
                let bid_contribution = bid.price.0 * bid.size;
                let ask_contribution = ask.price.0 * ask.size;
                
                weighted_price += bid_contribution + ask_contribution;
                total_volume += bid.size + ask.size;
            }
        }

        if total_volume > 0.0 {
            weighted_price / total_volume
        } else {
            orderbook.mid_price().unwrap_or(0.0.to_price()).0
        }
    }

    /// Calculate order flow imbalance
    fn calculate_order_flow_imbalance(&self, orderbook: &OrderBook) -> f64 {
        let bid_flow = orderbook.calculate_depth(3, Side::Bid);
        let ask_flow = orderbook.calculate_depth(3, Side::Ask);
        
        if bid_flow + ask_flow > 0.0 {
            (bid_flow - ask_flow) / (bid_flow + ask_flow)
        } else {
            0.0
        }
    }

    /// Calculate effective spread
    fn calculate_effective_spread(&self, orderbook: &OrderBook) -> f64 {
        let mid_price = orderbook.mid_price().unwrap_or(0.0.to_price()).0;
        let microprice = self.calculate_microprice(orderbook);
        
        if mid_price > 0.0 {
            2.0 * (microprice - mid_price).abs() / mid_price
        } else {
            0.0
        }
    }

    /// Calculate market impact
    fn calculate_market_impact(&self, orderbook: &OrderBook) -> f64 {
        // Simplified market impact calculation
        let spread = orderbook.spread().unwrap_or(0.0);
        let mid_price = orderbook.mid_price().unwrap_or(0.0.to_price()).0;
        
        if mid_price > 0.0 {
            spread / mid_price
        } else {
            0.0
        }
    }

    /// Calculate liquidity score
    fn calculate_liquidity_score(&self, orderbook: &OrderBook) -> f64 {
        let total_depth = orderbook.calculate_depth(10, Side::Bid) + 
                         orderbook.calculate_depth(10, Side::Ask);
        let spread = orderbook.spread().unwrap_or(0.0);
        
        if spread > 0.0 {
            total_depth / spread
        } else {
            total_depth
        }
    }

    /// Calculate depth pressure
    fn calculate_depth_pressure(&self, orderbook: &OrderBook) -> (f64, f64) {
        let bid_pressure = orderbook.calculate_depth(5, Side::Bid) / 
                          orderbook.calculate_depth(20, Side::Bid).max(1.0);
        let ask_pressure = orderbook.calculate_depth(5, Side::Ask) / 
                          orderbook.calculate_depth(20, Side::Ask).max(1.0);
        
        (bid_pressure, ask_pressure)
    }

    /// Update historical data
    fn update_history(&mut self, obi_l10: f64, microprice: f64, vwap: f64, order_flow_imbalance: f64) {
        // Update OBI history
        if self.obi_history.len() >= self.window_size {
            self.obi_history.pop_front();
        }
        self.obi_history.push_back(obi_l10);

        // Update microprice history
        if self.microprice_history.len() >= self.window_size {
            self.microprice_history.pop_front();
        }
        self.microprice_history.push_back(microprice);

        // Update VWAP history
        if self.vwap_history.len() >= self.window_size {
            self.vwap_history.pop_front();
        }
        self.vwap_history.push_back(vwap);

        // Update order flow history
        if self.order_flow_history.len() >= self.window_size {
            self.order_flow_history.pop_front();
        }
        self.order_flow_history.push_back(order_flow_imbalance);
    }

    /// Create orderbook snapshot
    fn create_snapshot(&self, orderbook: &OrderBook) -> OrderBookSnapshot {
        // This would create a snapshot of current orderbook state
        // Implementation depends on OrderBook structure
        OrderBookSnapshot {
            timestamp: now_micros(),
            best_bid: orderbook.best_bid().unwrap_or(0.0.to_price()),
            best_ask: orderbook.best_ask().unwrap_or(0.0.to_price()),
            // Add other snapshot data as needed
        }
    }
}

/// Simple orderbook snapshot for comparison
#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    pub timestamp: Timestamp,
    pub best_bid: Price,
    pub best_ask: Price,
}