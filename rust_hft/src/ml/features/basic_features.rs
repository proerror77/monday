/*!
 * Basic Features Extractor - 基础特征提取器
 * 
 * 提取基本的订单簿特征：价格、价差、深度等
 * 从原 features.rs 的前200行提取
 */

use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use anyhow::Result;
use std::collections::VecDeque;
use tracing::warn;
use rust_decimal::prelude::ToPrimitive;

/// Basic features from orderbook
#[derive(Debug, Clone)]
pub struct BasicFeatures {
    pub best_bid: Price,
    pub best_ask: Price,
    pub mid_price: Price,
    pub spread: f64,
    pub spread_bps: f64,
    pub bid_volume: f64,
    pub ask_volume: f64,
    pub total_volume: f64,
    pub timestamp: Timestamp,
}

impl BasicFeatures {
    /// Create new basic features
    pub fn new(orderbook: &OrderBook) -> Result<Self> {
        let best_bid = orderbook.best_bid().unwrap_or(0.0.to_price());
        let best_ask = orderbook.best_ask().unwrap_or(0.0.to_price());
        let mid_price = orderbook.mid_price().unwrap_or(0.0.to_price());
        let spread = orderbook.spread().unwrap_or(0.0);
        let spread_bps = orderbook.spread_bps().unwrap_or(0.0);
        
        // Calculate basic volumes
        let bid_volume = orderbook.calculate_depth(5, Side::Bid);
        let ask_volume = orderbook.calculate_depth(5, Side::Ask);
        let total_volume = bid_volume + ask_volume;

        Ok(Self {
            best_bid,
            best_ask,
            mid_price,
            spread,
            spread_bps,
            bid_volume,
            ask_volume,
            total_volume,
            timestamp: now_micros(),
        })
    }
}

/// Basic feature extractor with historical context
#[derive(Debug)]
pub struct BasicFeatureExtractor {
    /// Historical prices for momentum calculation
    price_history: VecDeque<f64>,
    
    /// Historical volumes for volume-based features
    volume_history: VecDeque<f64>,
    
    /// Historical spread values
    spread_history: VecDeque<f64>,
    
    /// Window size for moving calculations
    window_size: usize,
}

impl BasicFeatureExtractor {
    /// Create new basic feature extractor
    pub fn new(window_size: usize) -> Self {
        Self {
            price_history: VecDeque::with_capacity(window_size),
            volume_history: VecDeque::with_capacity(window_size),
            spread_history: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    /// Extract basic features from orderbook
    pub fn extract(&mut self, orderbook: &OrderBook) -> Result<BasicFeatures> {
        // Basic orderbook validation
        if !orderbook.is_valid {
            warn!("Attempting to extract features from invalid orderbook");
        }

        let features = BasicFeatures::new(orderbook)?;
        
        // Update historical data
        self.update_history(&features);
        
        Ok(features)
    }

    /// Update historical data with new features
    fn update_history(&mut self, features: &BasicFeatures) {
        // Update price history
        if self.price_history.len() >= self.window_size {
            self.price_history.pop_front();
        }
        self.price_history.push_back(features.mid_price.0);

        // Update volume history
        if self.volume_history.len() >= self.window_size {
            self.volume_history.pop_front();
        }
        self.volume_history.push_back(features.total_volume);

        // Update spread history
        if self.spread_history.len() >= self.window_size {
            self.spread_history.pop_front();
        }
        self.spread_history.push_back(features.spread);
    }

    /// Calculate price momentum
    pub fn calculate_price_momentum(&self) -> f64 {
        if self.price_history.len() < 2 {
            return 0.0;
        }
        
        let current = self.price_history.back().copied().unwrap_or(0.0);
        let prev = self.price_history.front().copied().unwrap_or(0.0);
        
        if prev > 0.0 {
            (current - prev) / prev
        } else {
            0.0
        }
    }

    /// Calculate volume momentum
    pub fn calculate_volume_momentum(&self) -> f64 {
        if self.volume_history.len() < 2 {
            return 0.0;
        }
        
        let current = self.volume_history.back().copied().unwrap_or(0.0);
        let prev = self.volume_history.front().copied().unwrap_or(0.0);
        
        if prev > 0.0 {
            (current - prev) / prev
        } else {
            0.0
        }
    }

    /// Calculate spread momentum
    pub fn calculate_spread_momentum(&self) -> f64 {
        if self.spread_history.len() < 2 {
            return 0.0;
        }
        
        let current = self.spread_history.back().copied().unwrap_or(0.0);
        let prev = self.spread_history.front().copied().unwrap_or(0.0);
        
        if prev > 0.0 {
            (current - prev) / prev
        } else {
            0.0
        }
    }

    /// Get historical statistics
    pub fn get_stats(&self) -> BasicFeatureStats {
        BasicFeatureStats {
            price_history_len: self.price_history.len(),
            volume_history_len: self.volume_history.len(),
            spread_history_len: self.spread_history.len(),
            window_size: self.window_size,
        }
    }
}

/// Basic feature extraction statistics
#[derive(Debug, Clone)]
pub struct BasicFeatureStats {
    pub price_history_len: usize,
    pub volume_history_len: usize,
    pub spread_history_len: usize,
    pub window_size: usize,
}