/*!
 * Feature Engineering for ML-Enhanced HFT
 * 
 * Extracts comprehensive market microstructure features
 * Optimized for real-time computation with minimal latency
 */

use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use anyhow::Result;
use std::collections::VecDeque;
use tracing::{debug, warn};
use rust_decimal::prelude::ToPrimitive;

/// Feature extractor with historical context
#[derive(Debug)]
pub struct FeatureExtractor {
    /// Historical prices for momentum calculation
    price_history: VecDeque<f64>,
    
    /// Historical volumes for volume-based features
    volume_history: VecDeque<f64>,
    
    /// Historical OBI values for trend analysis
    obi_history: VecDeque<f64>,
    
    /// Historical spread values
    spread_history: VecDeque<f64>,
    
    /// Historical microprice values (for momentum analysis)
    microprice_history: VecDeque<f64>,
    
    /// Historical volatility values
    volatility_history: VecDeque<f64>,
    
    /// Historical VWAP values
    vwap_history: VecDeque<f64>,
    
    /// Historical order flow imbalance
    order_flow_history: VecDeque<f64>,
    
    /// Historical trade intensity
    trade_intensity_history: VecDeque<f64>,
    
    /// Previous orderbook snapshot for delta calculations
    prev_orderbook_snapshot: Option<OrderBookSnapshot>,
    
    /// Technical indicators calculator
    technical_indicators: TechnicalIndicators,
    
    /// Window size for moving calculations
    window_size: usize,
    
    /// Feature extraction count
    extraction_count: u64,
    
    /// Last feature extraction timestamp
    last_extraction: Timestamp,
    
    /// LOB Tensor L10 for tensor-based features
    lob_tensor_l10: LobTensorL10,
    
    /// LOB Tensor L20 for tensor-based features
    lob_tensor_l20: LobTensorL20,
}

impl FeatureExtractor {
    /// Create new feature extractor
    pub fn new(window_size: usize) -> Self {
        Self {
            price_history: VecDeque::with_capacity(window_size),
            volume_history: VecDeque::with_capacity(window_size),
            obi_history: VecDeque::with_capacity(window_size),
            spread_history: VecDeque::with_capacity(window_size),
            microprice_history: VecDeque::with_capacity(window_size),
            volatility_history: VecDeque::with_capacity(window_size),
            vwap_history: VecDeque::with_capacity(window_size),
            order_flow_history: VecDeque::with_capacity(window_size),
            trade_intensity_history: VecDeque::with_capacity(window_size),
            prev_orderbook_snapshot: None,
            technical_indicators: TechnicalIndicators {
                sma_5: 0.0, sma_10: 0.0, sma_20: 0.0,
                ema_5: 0.0, ema_10: 0.0, ema_20: 0.0,
                rsi: 50.0, macd: 0.0, macd_signal: 0.0,
                bollinger_upper: 0.0, bollinger_lower: 0.0,
                atr: 0.0,
            },
            window_size,
            extraction_count: 0,
            last_extraction: now_micros(),
            lob_tensor_l10: LobTensorL10::new(window_size),
            lob_tensor_l20: LobTensorL20::new(window_size),
        }
    }

    /// Extract comprehensive feature set from orderbook
    pub fn extract_features(
        &mut self,
        orderbook: &OrderBook,
        network_latency_us: u64,
        processing_start: Timestamp,
    ) -> Result<FeatureSet> {
        let start_time = now_micros();
        let processing_latency = start_time - processing_start;

        // Basic orderbook validation
        if !orderbook.is_valid {
            warn!("Attempting to extract features from invalid orderbook");
        }

        // Get basic price information
        let best_bid = orderbook.best_bid().unwrap_or(0.0.to_price());
        let best_ask = orderbook.best_ask().unwrap_or(0.0.to_price());
        let mid_price = orderbook.mid_price().unwrap_or(0.0.to_price());
        let spread = orderbook.spread().unwrap_or(0.0);
        let spread_bps = orderbook.spread_bps().unwrap_or(0.0);

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

        // Update historical data
        self.update_history(mid_price.0, bid_depth_l5 + ask_depth_l5, obi_l10, spread);

        // Calculate derived features
        let price_momentum = self.calculate_price_momentum();
        let volume_imbalance = self.calculate_volume_imbalance();
        let data_quality_score = self.calculate_data_quality_score(orderbook);
        
        // Calculate enhanced features
        let microprice = self.calculate_microprice(orderbook);
        let vwap = self.calculate_vwap(orderbook);
        let realized_volatility = self.calculate_realized_volatility();
        let effective_spread = self.calculate_effective_spread(orderbook);
        let price_acceleration = self.calculate_price_acceleration();
        let volume_acceleration = self.calculate_volume_acceleration();
        let order_flow_imbalance = self.calculate_order_flow_imbalance(orderbook);
        let trade_intensity = self.calculate_trade_intensity();
        let bid_ask_correlation = self.calculate_bid_ask_correlation();
        let market_impact = self.calculate_market_impact(orderbook);
        let liquidity_score = self.calculate_liquidity_score(orderbook);
        
        // Calculate multi-timeframe momentum and volatility
        let (momentum_5_tick, momentum_10_tick, momentum_20_tick) = self.calculate_multi_momentum();
        let (volatility_5_tick, volatility_10_tick, volatility_20_tick) = self.calculate_multi_volatility();
        
        // Calculate depth pressure
        let (depth_pressure_bid, depth_pressure_ask) = self.calculate_depth_pressure(orderbook);
        
        // Calculate order flow metrics
        let order_arrival_rate = self.calculate_order_arrival_rate();
        let cancellation_rate = self.calculate_cancellation_rate();
        
        // Update technical indicators
        self.update_technical_indicators(mid_price.0);

        // Update LOB tensors
        let _ = self.lob_tensor_l10.update_from_orderbook(orderbook);
        let _ = self.lob_tensor_l20.update_from_orderbook(orderbook);

        // Create feature set
        let mut features = FeatureSet::default_enhanced();
        features.timestamp = start_time;
        features.latency_network_us = network_latency_us;
        features.latency_processing_us = processing_latency;
        
        // Basic features
        features.best_bid = best_bid;
        features.best_ask = best_ask;
        features.mid_price = mid_price;
        features.spread = spread;
        features.spread_bps = spread_bps;
        
        // OBI features
        features.obi_l1 = obi_l1;
        features.obi_l5 = obi_l5;
        features.obi_l10 = obi_l10;
        features.obi_l20 = obi_l20;
        
        // Depth features
        features.bid_depth_l5 = bid_depth_l5;
        features.ask_depth_l5 = ask_depth_l5;
        features.bid_depth_l10 = bid_depth_l10;
        features.ask_depth_l10 = ask_depth_l10;
        features.bid_depth_l20 = bid_depth_l20;
        features.ask_depth_l20 = ask_depth_l20;
        
        // Imbalance features
        features.depth_imbalance_l5 = depth_imbalance_l5;
        features.depth_imbalance_l10 = depth_imbalance_l10;
        features.depth_imbalance_l20 = depth_imbalance_l20;
        
        // Slope features
        features.bid_slope = bid_slope;
        features.ask_slope = ask_slope;
        
        // Volume features
        features.total_bid_levels = orderbook.bids.len();
        features.total_ask_levels = orderbook.asks.len();
        
        // Derived features
        features.price_momentum = price_momentum;
        features.volume_imbalance = volume_imbalance;
        
        // Quality indicators
        features.is_valid = orderbook.is_valid;
        features.data_quality_score = data_quality_score;
        
        // Enhanced features
        features.microprice = microprice;
        features.vwap = vwap;
        features.realized_volatility = realized_volatility;
        features.effective_spread = effective_spread;
        features.price_acceleration = price_acceleration;
        features.volume_acceleration = volume_acceleration;
        features.order_flow_imbalance = order_flow_imbalance;
        features.trade_intensity = trade_intensity;
        features.bid_ask_correlation = bid_ask_correlation;
        features.market_impact = market_impact;
        features.liquidity_score = liquidity_score;
        features.momentum_5_tick = momentum_5_tick;
        features.momentum_10_tick = momentum_10_tick;
        features.momentum_20_tick = momentum_20_tick;
        features.volatility_5_tick = volatility_5_tick;
        features.volatility_10_tick = volatility_10_tick;
        features.volatility_20_tick = volatility_20_tick;
        features.depth_pressure_bid = depth_pressure_bid;
        features.depth_pressure_ask = depth_pressure_ask;
        features.order_arrival_rate = order_arrival_rate;
        features.cancellation_rate = cancellation_rate;
        
        // Add tensor features
        features.lob_tensor_l10 = Some(self.lob_tensor_l10.clone());
        features.lob_tensor_l20 = Some(self.lob_tensor_l20.clone());

        self.extraction_count += 1;
        self.last_extraction = start_time;

        let extraction_latency = now_micros() - start_time;
        debug!("Feature extraction completed in {}μs", extraction_latency);

        Ok(features)
    }

    /// Update historical data with sliding window
    fn update_history(&mut self, price: f64, volume: f64, obi: f64, spread: f64) {
        // Calculate additional values
        let microprice = price; // Will be updated with proper calculation
        let volatility = if self.price_history.len() > 1 {
            self.calculate_volatility_n_tick(5)
        } else { 0.0 };
        let order_flow = obi; // Use OBI as proxy for order flow
        let trade_intensity = 1.0 / ((now_micros() - self.last_extraction) as f64 / 1_000_000.0).max(0.001);
        
        // Add new values
        self.price_history.push_back(price);
        self.volume_history.push_back(volume);
        self.obi_history.push_back(obi);
        self.spread_history.push_back(spread);
        self.microprice_history.push_back(microprice);
        self.volatility_history.push_back(volatility);
        self.vwap_history.push_back(price); // Simplified - will be updated with proper VWAP
        self.order_flow_history.push_back(order_flow);
        self.trade_intensity_history.push_back(trade_intensity);

        // Remove old values if window is full
        let histories = vec![
            &mut self.price_history,
            &mut self.volume_history,
            &mut self.obi_history,
            &mut self.spread_history,
            &mut self.microprice_history,
            &mut self.volatility_history,
            &mut self.vwap_history,
            &mut self.order_flow_history,
            &mut self.trade_intensity_history,
        ];
        
        for history in histories {
            if history.len() > self.window_size {
                history.pop_front();
            }
        }
    }

    /// Calculate price momentum using historical prices
    fn calculate_price_momentum(&self) -> f64 {
        if self.price_history.len() < 2 {
            return 0.0;
        }

        let recent_price = self.price_history.back().unwrap();
        let old_price = self.price_history.front().unwrap();

        if *old_price > 0.0 {
            (recent_price - old_price) / old_price
        } else {
            0.0
        }
    }

    /// Calculate volume imbalance using historical volumes
    fn calculate_volume_imbalance(&self) -> f64 {
        if self.volume_history.len() < 2 {
            return 0.0;
        }

        let recent_volume = self.volume_history.back().unwrap();
        let avg_volume = self.volume_history.iter().sum::<f64>() / self.volume_history.len() as f64;

        if avg_volume > 0.0 {
            (recent_volume - avg_volume) / avg_volume
        } else {
            0.0
        }
    }

    /// Calculate data quality score
    fn calculate_data_quality_score(&self, orderbook: &OrderBook) -> f64 {
        let mut score: f64 = 1.0;

        // Penalize invalid orderbooks
        if !orderbook.is_valid {
            score *= 0.5;
        }

        // Penalize thin orderbooks
        let total_levels = orderbook.bids.len() + orderbook.asks.len();
        if total_levels < 10 {
            score *= 0.8;
        }

        // Penalize wide spreads
        if let Some(spread_bps) = orderbook.spread_bps() {
            if spread_bps > 50.0 {  // > 0.5%
                score *= 0.7;
            }
        }

        // Penalize stale data
        let age_ms = (now_micros() - orderbook.last_update) / 1000;
        if age_ms > 1000 {  // > 1 second
            score *= 0.6;
        }

        score.clamp(0.0, 1.0)
    }

    /// Get feature extractor statistics
    pub fn get_stats(&self) -> FeatureExtractorStats {
        FeatureExtractorStats {
            extraction_count: self.extraction_count,
            last_extraction: self.last_extraction,
            window_size: self.window_size,
            price_history_size: self.price_history.len(),
            volume_history_size: self.volume_history.len(),
            obi_history_size: self.obi_history.len(),
            spread_history_size: self.spread_history.len(),
        }
    }

    /// Reset historical data
    pub fn reset(&mut self) {
        self.price_history.clear();
        self.volume_history.clear();
        self.obi_history.clear();
        self.spread_history.clear();
        self.microprice_history.clear();
        self.volatility_history.clear();
        self.vwap_history.clear();
        self.order_flow_history.clear();
        self.trade_intensity_history.clear();
        self.prev_orderbook_snapshot = None;
        self.extraction_count = 0;
        
        // Reset tensors
        self.lob_tensor_l10 = LobTensorL10::new(self.window_size);
        self.lob_tensor_l20 = LobTensorL20::new(self.window_size);
    }
    
    // ===== ENHANCED FEATURE CALCULATION METHODS =====
    
    /// Calculate microprice (weighted mid price)
    fn calculate_microprice(&self, orderbook: &OrderBook) -> f64 {
        if let (Some(best_bid), Some(best_ask)) = (orderbook.best_bid(), orderbook.best_ask()) {
            let bid_vol = orderbook.bids.iter().next().map_or(0.0, |(_, qty)| qty.to_f64().unwrap_or(0.0));
            let ask_vol = orderbook.asks.iter().next().map_or(0.0, |(_, qty)| qty.to_f64().unwrap_or(0.0));
            
            if bid_vol + ask_vol > 0.0 {
                (best_bid.0 * ask_vol + best_ask.0 * bid_vol) / (bid_vol + ask_vol)
            } else {
                (best_bid.0 + best_ask.0) / 2.0
            }
        } else {
            0.0
        }
    }
    
    /// Calculate volume-weighted average price (VWAP)
    fn calculate_vwap(&self, orderbook: &OrderBook) -> f64 {
        let mut total_value = 0.0;
        let mut total_volume = 0.0;
        
        // Calculate VWAP from top 5 levels
        for (price, qty) in orderbook.bids.iter().take(5) {
            let vol = qty.to_f64().unwrap_or(0.0);
            total_value += price.0 * vol;
            total_volume += vol;
        }
        
        for (price, qty) in orderbook.asks.iter().take(5) {
            let vol = qty.to_f64().unwrap_or(0.0);
            total_value += price.0 * vol;
            total_volume += vol;
        }
        
        if total_volume > 0.0 {
            total_value / total_volume
        } else {
            orderbook.mid_price().unwrap_or(0.0.to_price()).0
        }
    }
    
    /// Calculate realized volatility using price returns
    fn calculate_realized_volatility(&self) -> f64 {
        if self.price_history.len() < 5 {
            return 0.0;
        }
        
        let returns: Vec<f64> = self.price_history
            .iter()
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1] / w[0]).ln())
            .collect();
        
        if returns.is_empty() {
            return 0.0;
        }
        
        let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter()
            .map(|r| (r - mean_return).powi(2))
            .sum::<f64>() / (returns.len() - 1) as f64;
        
        variance.sqrt() * (60.0 * 60.0 * 1000000.0_f64).sqrt() // Annualized volatility
    }
    
    /// Calculate effective spread estimation
    fn calculate_effective_spread(&self, orderbook: &OrderBook) -> f64 {
        let mid = orderbook.mid_price().unwrap_or(0.0.to_price()).0;
        if mid > 0.0 {
            // Use microprice to estimate effective spread
            let microprice = self.calculate_microprice(orderbook);
            2.0 * (microprice - mid).abs() / mid
        } else {
            0.0
        }
    }
    
    /// Calculate price acceleration (second derivative)
    fn calculate_price_acceleration(&self) -> f64 {
        if self.price_history.len() < 3 {
            return 0.0;
        }
        
        let prices: Vec<f64> = self.price_history.iter().cloned().collect();
        let n = prices.len();
        
        // Calculate second difference
        let first_diff_1 = prices[n-1] - prices[n-2];
        let first_diff_2 = prices[n-2] - prices[n-3];
        
        first_diff_1 - first_diff_2
    }
    
    /// Calculate volume acceleration
    fn calculate_volume_acceleration(&self) -> f64 {
        if self.volume_history.len() < 3 {
            return 0.0;
        }
        
        let volumes: Vec<f64> = self.volume_history.iter().cloned().collect();
        let n = volumes.len();
        
        let first_diff_1 = volumes[n-1] - volumes[n-2];
        let first_diff_2 = volumes[n-2] - volumes[n-3];
        
        first_diff_1 - first_diff_2
    }
    
    /// Calculate order flow imbalance
    fn calculate_order_flow_imbalance(&self, orderbook: &OrderBook) -> f64 {
        let bid_volume = orderbook.calculate_depth(5, Side::Bid);
        let ask_volume = orderbook.calculate_depth(5, Side::Ask);
        
        if bid_volume + ask_volume > 0.0 {
            (bid_volume - ask_volume) / (bid_volume + ask_volume)
        } else {
            0.0
        }
    }
    
    /// Calculate trade intensity (arrival rate estimation)
    fn calculate_trade_intensity(&self) -> f64 {
        if self.trade_intensity_history.len() < 2 {
            return 0.0;
        }
        
        // Simple exponential moving average
        let alpha = 0.1;
        self.trade_intensity_history
            .iter()
            .fold(0.0, |acc, &x| alpha * x + (1.0 - alpha) * acc)
    }
    
    /// Calculate bid-ask correlation
    fn calculate_bid_ask_correlation(&self) -> f64 {
        if self.price_history.len() < 10 {
            return 0.0;
        }
        
        // Use spread changes as proxy for bid-ask correlation
        let spread_changes: Vec<f64> = self.spread_history
            .iter()
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect();
        
        if spread_changes.len() < 2 {
            return 0.0;
        }
        
        // Calculate correlation with price changes
        let price_changes: Vec<f64> = self.price_history
            .iter()
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| w[1] - w[0])
            .take(spread_changes.len())
            .collect();
        
        if price_changes.len() != spread_changes.len() {
            return 0.0;
        }
        
        let n = price_changes.len() as f64;
        let sum_x: f64 = price_changes.iter().sum();
        let sum_y: f64 = spread_changes.iter().sum();
        let sum_xy: f64 = price_changes.iter().zip(&spread_changes).map(|(x, y)| x * y).sum();
        let sum_x2: f64 = price_changes.iter().map(|x| x * x).sum();
        let sum_y2: f64 = spread_changes.iter().map(|y| y * y).sum();
        
        let numerator = n * sum_xy - sum_x * sum_y;
        let denominator = ((n * sum_x2 - sum_x * sum_x) * (n * sum_y2 - sum_y * sum_y)).sqrt();
        
        if denominator != 0.0 {
            numerator / denominator
        } else {
            0.0
        }
    }
    
    /// Calculate market impact estimation
    fn calculate_market_impact(&self, orderbook: &OrderBook) -> f64 {
        let spread = orderbook.spread().unwrap_or(0.0);
        let depth_5 = orderbook.calculate_depth(5, Side::Bid) + orderbook.calculate_depth(5, Side::Ask);
        
        if depth_5 > 0.0 {
            // Kyle's lambda approximation
            spread / (2.0 * depth_5.sqrt())
        } else {
            0.0
        }
    }
    
    /// Calculate overall liquidity score
    fn calculate_liquidity_score(&self, orderbook: &OrderBook) -> f64 {
        let spread_bps = orderbook.spread_bps().unwrap_or(f64::INFINITY);
        let depth_10 = orderbook.calculate_depth(10, Side::Bid) + orderbook.calculate_depth(10, Side::Ask);
        let levels = (orderbook.bids.len() + orderbook.asks.len()) as f64;
        
        // Composite liquidity score (normalized 0-1)
        let spread_score = (100.0 / (1.0 + spread_bps)).min(1.0);
        let depth_score = (depth_10 / 10000.0).min(1.0); // Normalize by $10k
        let level_score = (levels / 20.0).min(1.0); // Normalize by 20 levels
        
        (spread_score + depth_score + level_score) / 3.0
    }
    
    /// Calculate multi-timeframe momentum
    fn calculate_multi_momentum(&self) -> (f64, f64, f64) {
        let momentum_5 = self.calculate_momentum_n_tick(5);
        let momentum_10 = self.calculate_momentum_n_tick(10);
        let momentum_20 = self.calculate_momentum_n_tick(20);
        
        (momentum_5, momentum_10, momentum_20)
    }
    
    /// Calculate momentum for n ticks
    fn calculate_momentum_n_tick(&self, n: usize) -> f64 {
        if self.price_history.len() < n + 1 {
            return 0.0;
        }
        
        let prices: Vec<f64> = self.price_history.iter().cloned().collect();
        let current = prices[prices.len() - 1];
        let past = prices[prices.len() - 1 - n];
        
        if past > 0.0 {
            (current - past) / past
        } else {
            0.0
        }
    }
    
    /// Calculate multi-timeframe volatility
    fn calculate_multi_volatility(&self) -> (f64, f64, f64) {
        let vol_5 = self.calculate_volatility_n_tick(5);
        let vol_10 = self.calculate_volatility_n_tick(10);
        let vol_20 = self.calculate_volatility_n_tick(20);
        
        (vol_5, vol_10, vol_20)
    }
    
    /// Calculate volatility for n ticks
    fn calculate_volatility_n_tick(&self, n: usize) -> f64 {
        if self.price_history.len() < n + 1 {
            return 0.0;
        }
        
        let prices: Vec<f64> = self.price_history.iter().rev().take(n + 1).cloned().collect();
        let returns: Vec<f64> = prices.windows(2).map(|w| (w[0] / w[1]).ln()).collect();
        
        if returns.is_empty() {
            return 0.0;
        }
        
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
        
        variance.sqrt()
    }
    
    /// Calculate depth pressure
    fn calculate_depth_pressure(&self, orderbook: &OrderBook) -> (f64, f64) {
        let bid_pressure = self.calculate_side_pressure(orderbook, Side::Bid);
        let ask_pressure = self.calculate_side_pressure(orderbook, Side::Ask);
        
        (bid_pressure, ask_pressure)
    }
    
    /// Calculate pressure on one side
    fn calculate_side_pressure(&self, orderbook: &OrderBook, side: Side) -> f64 {
        let levels = match side {
            Side::Bid => &orderbook.bids,
            Side::Ask => &orderbook.asks,
        };
        
        if levels.len() < 3 {
            return 0.0;
        }
        
        // Calculate weighted depth pressure
        let mut pressure = 0.0;
        let mut total_weight = 0.0;
        
        for (i, (_, qty)) in levels.iter().take(10).enumerate() {
            let weight = 1.0 / (i + 1) as f64; // Higher weight for closer levels
            let volume = qty.to_f64().unwrap_or(0.0);
            pressure += volume * weight;
            total_weight += weight;
        }
        
        if total_weight > 0.0 {
            pressure / total_weight
        } else {
            0.0
        }
    }
    
    /// Calculate order arrival rate
    fn calculate_order_arrival_rate(&self) -> f64 {
        // Simple estimation based on extraction frequency
        if self.extraction_count > 1 {
            let time_diff = (now_micros() - self.last_extraction) as f64 / 1_000_000.0; // seconds
            if time_diff > 0.0 {
                1.0 / time_diff // orders per second
            } else {
                0.0
            }
        } else {
            0.0
        }
    }
    
    /// Calculate cancellation rate estimation
    fn calculate_cancellation_rate(&self) -> f64 {
        // Proxy using spread volatility
        if self.spread_history.len() < 5 {
            return 0.0;
        }
        
        let spread_std = {
            let mean = self.spread_history.iter().sum::<f64>() / self.spread_history.len() as f64;
            let variance = self.spread_history.iter()
                .map(|s| (s - mean).powi(2))
                .sum::<f64>() / self.spread_history.len() as f64;
            variance.sqrt()
        };
        
        // Higher spread volatility suggests more cancellations
        spread_std.min(1.0)
    }
    
    /// Update technical indicators
    fn update_technical_indicators(&mut self, price: f64) {
        if self.price_history.is_empty() {
            return;
        }
        
        // Update simple moving averages
        self.technical_indicators.sma_5 = self.calculate_sma(5);
        self.technical_indicators.sma_10 = self.calculate_sma(10);
        self.technical_indicators.sma_20 = self.calculate_sma(20);
        
        // Update exponential moving averages
        Self::update_ema(&mut self.technical_indicators.ema_5, price, 5);
        Self::update_ema(&mut self.technical_indicators.ema_10, price, 10);
        Self::update_ema(&mut self.technical_indicators.ema_20, price, 20);
        
        // Update RSI
        self.technical_indicators.rsi = self.calculate_rsi();
        
        // Update ATR
        self.technical_indicators.atr = self.calculate_atr();
    }
    
    /// Calculate Simple Moving Average
    fn calculate_sma(&self, period: usize) -> f64 {
        if self.price_history.len() < period {
            return 0.0;
        }
        
        self.price_history.iter().rev().take(period).sum::<f64>() / period as f64
    }
    
    /// Update Exponential Moving Average
    fn update_ema(ema: &mut f64, price: f64, period: usize) {
        let alpha = 2.0 / (period as f64 + 1.0);
        if *ema == 0.0 {
            *ema = price;
        } else {
            *ema = alpha * price + (1.0 - alpha) * *ema;
        }
    }
    
    /// Calculate Relative Strength Index
    fn calculate_rsi(&self) -> f64 {
        if self.price_history.len() < 15 {
            return 50.0; // Neutral RSI
        }
        
        let prices: Vec<f64> = self.price_history.iter().rev().take(14).cloned().collect();
        let mut gains = 0.0;
        let mut losses = 0.0;
        
        for window in prices.windows(2) {
            let change = window[0] - window[1];
            if change > 0.0 {
                gains += change;
            } else {
                losses -= change;
            }
        }
        
        if losses == 0.0 {
            return 100.0;
        }
        
        let rs = gains / losses;
        100.0 - (100.0 / (1.0 + rs))
    }
    
    /// Calculate Average True Range
    fn calculate_atr(&self) -> f64 {
        if self.price_history.len() < 14 {
            return 0.0;
        }
        
        // Simplified ATR using price range
        let prices: Vec<f64> = self.price_history.iter().rev().take(14).cloned().collect();
        let ranges: Vec<f64> = prices.windows(2).map(|w| (w[0] - w[1]).abs()).collect();
        
        if ranges.is_empty() {
            0.0
        } else {
            ranges.iter().sum::<f64>() / ranges.len() as f64
        }
    }
}

/// Feature extractor statistics
#[derive(Debug, Clone)]
pub struct FeatureExtractorStats {
    pub extraction_count: u64,
    pub last_extraction: Timestamp,
    pub window_size: usize,
    pub price_history_size: usize,
    pub volume_history_size: usize,
    pub obi_history_size: usize,
    pub spread_history_size: usize,
}

/// Utility functions for feature validation
pub fn validate_features(features: &FeatureSet) -> bool {
    // Check for NaN or infinite values
    if !features.best_bid.is_finite() ||
       !features.best_ask.is_finite() ||
       !features.mid_price.is_finite() {
        return false;
    }

    // Check for reasonable price values
    if features.best_bid.0 <= 0.0 || features.best_ask.0 <= 0.0 {
        return false;
    }

    // Check spread sanity
    if features.spread < 0.0 || features.spread_bps < 0.0 {
        return false;
    }

    // Check OBI values are in valid range [-1, 1]
    if features.obi_l1.abs() > 1.0 ||
       features.obi_l5.abs() > 1.0 ||
       features.obi_l10.abs() > 1.0 ||
       features.obi_l20.abs() > 1.0 {
        return false;
    }

    // Check depth values are non-negative
    if features.bid_depth_l5 < 0.0 ||
       features.ask_depth_l5 < 0.0 ||
       features.bid_depth_l10 < 0.0 ||
       features.ask_depth_l10 < 0.0 {
        return false;
    }

    true
}

/// Convert FeatureSet to ML model input vector
pub fn features_to_vector(features: &FeatureSet) -> Vec<f64> {
    let mut vector = vec![
        // Basic price features
        features.spread_bps,
        features.effective_spread,
        
        // OBI features
        features.obi_l1,
        features.obi_l5,
        features.obi_l10,
        features.obi_l20,
        
        // Depth imbalance features
        features.depth_imbalance_l5,
        features.depth_imbalance_l10,
        features.depth_imbalance_l20,
        
        // Slope features
        features.bid_slope,
        features.ask_slope,
        
        // Basic derived features
        features.price_momentum,
        features.volume_imbalance,
        
        // Enhanced price features
        features.microprice / features.mid_price.0.max(1.0), // Normalized microprice
        features.vwap / features.mid_price.0.max(1.0),       // Normalized VWAP
        features.realized_volatility,
        features.price_acceleration,
        features.volume_acceleration,
        
        // Order flow features
        features.order_flow_imbalance,
        features.trade_intensity.min(100.0), // Cap at 100 to prevent outliers
        features.bid_ask_correlation,
        
        // Market microstructure features
        features.market_impact.min(1.0), // Cap at 1.0
        features.liquidity_score,
        
        // Multi-timeframe momentum
        features.momentum_5_tick,
        features.momentum_10_tick,
        features.momentum_20_tick,
        
        // Multi-timeframe volatility
        features.volatility_5_tick,
        features.volatility_10_tick,
        features.volatility_20_tick,
        
        // Depth pressure features
        normalize_depth(features.depth_pressure_bid),
        normalize_depth(features.depth_pressure_ask),
        
        // Order flow metrics
        features.order_arrival_rate.min(1000.0) / 1000.0, // Normalize to [0,1]
        features.cancellation_rate,
        
        // Normalized depth features
        normalize_depth(features.bid_depth_l5),
        normalize_depth(features.ask_depth_l5),
        normalize_depth(features.bid_depth_l10),
        normalize_depth(features.ask_depth_l10),
        
        // Quality indicators
        features.data_quality_score,
        if features.is_valid { 1.0 } else { 0.0 },
    ];
    
    // Add tensor-derived features
    if let Some(ref tensor_l10) = features.lob_tensor_l10 {
        let tensor_features = tensor_l10.extract_features();
        vector.extend(tensor_features);
    } else {
        // Add zeros for missing tensor features (maintain compatibility)
        vector.extend(vec![0.0; 12]); // 1 overall + 10 level + 2 gradient features
    }
    
    if let Some(ref tensor_l20) = features.lob_tensor_l20 {
        let tensor_features = tensor_l20.extract_features();
        // Only add additional features that L20 provides beyond L10
        if tensor_features.len() > 12 {
            vector.extend(&tensor_features[12..]);
        }
    }
    
    vector
}

/// Normalize depth values for ML model
fn normalize_depth(depth: f64) -> f64 {
    // Log normalization for depth values
    if depth > 0.0 {
        (1.0 + depth).ln()
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::orderbook::OrderBook;

    #[test]
    fn test_feature_extraction() {
        let mut extractor = FeatureExtractor::new(10);
        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // Initialize orderbook with test data
        let update = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![
                PriceLevel { price: 100.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
            ],
            asks: vec![
                PriceLevel { price: 101.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
            ],
            timestamp: now_micros(),
            sequence_start: 1,
            sequence_end: 1,
            is_snapshot: true,
        };
        
        orderbook.init_snapshot(update).unwrap();
        
        // Extract features
        let features = extractor.extract_features(&orderbook, 10, now_micros()).unwrap();
        
        // Validate features
        assert!(validate_features(&features));
        assert_eq!(features.best_bid, 100.0.to_price());
        assert_eq!(features.best_ask, 101.0.to_price());
        assert_eq!(features.spread, 1.0);
    }

    #[test] 
    fn test_lob_tensor_functionality() {
        let mut extractor = FeatureExtractor::new(10);
        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // Initialize orderbook with test data
        let update = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![
                PriceLevel { price: 100.0.to_price(), quantity: "5.0".to_quantity(), side: Side::Bid },
                PriceLevel { price: 99.5.to_price(), quantity: "3.0".to_quantity(), side: Side::Bid },
                PriceLevel { price: 99.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Bid },
            ],
            asks: vec![
                PriceLevel { price: 101.0.to_price(), quantity: "4.0".to_quantity(), side: Side::Ask },
                PriceLevel { price: 101.5.to_price(), quantity: "6.0".to_quantity(), side: Side::Ask },
                PriceLevel { price: 102.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
            ],
            timestamp: now_micros(),
            sequence_start: 1,
            sequence_end: 1,
            is_snapshot: true,
        };
        
        orderbook.init_snapshot(update).unwrap();
        
        // Extract features with tensors
        let features = extractor.extract_features(&orderbook, 10, now_micros()).unwrap();
        
        // Verify tensor features are present
        assert!(features.lob_tensor_l10.is_some());
        assert!(features.lob_tensor_l20.is_some());
        
        // Verify tensor extracts features
        let tensor_features = if let Some(ref tensor_l10) = features.lob_tensor_l10 {
            tensor_l10.extract_features()
        } else {
            panic!("Expected tensor L10 to be present");
        };
        assert!(!tensor_features.is_empty());
        assert!(tensor_features.len() >= 12); // 1 overall + 10 level + 2 gradient features
        
        // Verify features_to_vector includes tensor features
        let vector = features_to_vector(&features);
        println!("Tensor L10 features: {:?}", tensor_features);
        println!("Feature vector length: {}", vector.len());
        
        // The actual count might be different, let's verify it's at least the expected minimum
        assert!(vector.len() >= 51); // At least 39 original + 12 tensor features
    }

    #[test]
    fn test_features_to_vector() {
        let mut features = FeatureSet::default_enhanced();
        features.timestamp = now_micros();
        features.latency_network_us = 10;
        features.latency_processing_us = 20;
        features.best_bid = 100.0.to_price();
        features.best_ask = 101.0.to_price();
        features.mid_price = 100.5.to_price();
        features.spread = 1.0;
        features.spread_bps = 100.0;
        features.obi_l1 = 0.1;
        features.obi_l5 = 0.05;
        features.obi_l10 = 0.02;
        features.obi_l20 = 0.01;
        features.bid_depth_l5 = 1000.0;
        features.ask_depth_l5 = 1100.0;
        features.bid_depth_l10 = 2000.0;
        features.ask_depth_l10 = 2200.0;
        features.bid_depth_l20 = 4000.0;
        features.ask_depth_l20 = 4400.0;
        features.depth_imbalance_l5 = -0.05;
        features.depth_imbalance_l10 = -0.05;
        features.depth_imbalance_l20 = -0.05;
        features.bid_slope = 0.1;
        features.ask_slope = 0.1;
        features.total_bid_levels = 10;
        features.total_ask_levels = 12;
        features.price_momentum = 0.001;
        features.volume_imbalance = 0.05;
        features.is_valid = true;
        features.data_quality_score = 0.95;
        
        // Enhanced features
        features.microprice = 100.3;
        features.vwap = 100.4;
        features.realized_volatility = 0.02;
        features.effective_spread = 0.01;
        features.price_acceleration = 0.001;
        features.volume_acceleration = 10.0;
        features.order_flow_imbalance = 0.03;
        features.trade_intensity = 5.0;
        features.bid_ask_correlation = 0.2;
        features.market_impact = 0.05;
        features.liquidity_score = 0.8;
        features.momentum_5_tick = 0.001;
        features.momentum_10_tick = 0.002;
        features.momentum_20_tick = 0.003;
        features.volatility_5_tick = 0.01;
        features.volatility_10_tick = 0.015;
        features.volatility_20_tick = 0.02;
        features.depth_pressure_bid = 500.0;
        features.depth_pressure_ask = 600.0;
        features.order_arrival_rate = 10.0;
        features.cancellation_rate = 0.1;

        let vector = features_to_vector(&features);
        assert_eq!(vector.len(), 51); // Updated expected number of features (39 original + 12 tensor features)
        assert!(validate_features(&features));
        
        // Check that all features are finite
        for (i, &value) in vector.iter().enumerate() {
            assert!(value.is_finite(), "Feature {} is not finite: {}", i, value);
        }
    }
}