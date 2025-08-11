/*!
 * Advanced Features - 高级特征计算器
 * 
 * 计算复杂的衍生特征：波动率、相关性、LOB张量等
 * 从原 features.rs 的高级特征部分提取
 */

use crate::core::types::*;
use crate::core::orderbook::OrderBook;
use crate::ml::features::basic_features::BasicFeatures;
use anyhow::Result;
use std::collections::VecDeque;

/// Advanced features structure
#[derive(Debug, Clone)]
pub struct AdvancedFeatures {
    // Volatility features
    pub realized_volatility: f64,
    pub volatility_5_tick: f64,
    pub volatility_10_tick: f64,
    pub volatility_20_tick: f64,
    
    // Momentum features
    pub momentum_5_tick: f64,
    pub momentum_10_tick: f64,
    pub momentum_20_tick: f64,
    
    // Acceleration features
    pub price_acceleration: f64,
    pub volume_acceleration: f64,
    
    // Correlation features
    pub bid_ask_correlation: f64,
    
    // Trade intensity
    pub trade_intensity: f64,
    pub order_arrival_rate: f64,
    pub cancellation_rate: f64,
    
    // Data quality
    pub data_quality_score: f64,
    
    // LOB tensor features
    pub lob_tensor_l10_features: Vec<f64>,
    pub lob_tensor_l20_features: Vec<f64>,
}

/// Advanced feature extractor
#[derive(Debug)]
pub struct AdvancedFeatureExtractor {
    /// Historical volatility values
    volatility_history: VecDeque<f64>,
    
    /// Historical trade intensity
    trade_intensity_history: VecDeque<f64>,
    
    /// Price change history for acceleration
    price_change_history: VecDeque<f64>,
    
    /// Volume change history for acceleration
    volume_change_history: VecDeque<f64>,
    
    /// Bid-ask correlation data
    bid_history: VecDeque<f64>,
    ask_history: VecDeque<f64>,
    
    /// LOB Tensor L10 for tensor-based features
    lob_tensor_l10: LobTensorL10,
    
    /// LOB Tensor L20 for tensor-based features
    lob_tensor_l20: LobTensorL20,
    
    /// Window size for calculations
    window_size: usize,
    
    /// Last update timestamp
    last_update: Timestamp,
}

impl AdvancedFeatureExtractor {
    /// Create new advanced feature extractor
    pub fn new(window_size: usize) -> Self {
        Self {
            volatility_history: VecDeque::with_capacity(window_size),
            trade_intensity_history: VecDeque::with_capacity(window_size),
            price_change_history: VecDeque::with_capacity(window_size),
            volume_change_history: VecDeque::with_capacity(window_size),
            bid_history: VecDeque::with_capacity(window_size),
            ask_history: VecDeque::with_capacity(window_size),
            lob_tensor_l10: LobTensorL10::new(window_size),
            lob_tensor_l20: LobTensorL20::new(window_size),
            window_size,
            last_update: now_micros(),
        }
    }

    /// Extract advanced features
    pub fn extract(
        &mut self, 
        orderbook: &OrderBook, 
        basic_features: &BasicFeatures
    ) -> Result<AdvancedFeatures> {
        let current_time = now_micros();
        
        // Calculate volatility features
        let realized_volatility = self.calculate_realized_volatility();
        let (volatility_5_tick, volatility_10_tick, volatility_20_tick) = self.calculate_multi_volatility();
        
        // Calculate momentum features
        let (momentum_5_tick, momentum_10_tick, momentum_20_tick) = self.calculate_multi_momentum();
        
        // Calculate acceleration features
        let price_acceleration = self.calculate_price_acceleration();
        let volume_acceleration = self.calculate_volume_acceleration();
        
        // Calculate correlation
        let bid_ask_correlation = self.calculate_bid_ask_correlation();
        
        // Calculate trade intensity metrics
        let trade_intensity = self.calculate_trade_intensity();
        let order_arrival_rate = self.calculate_order_arrival_rate();
        let cancellation_rate = self.calculate_cancellation_rate();
        
        // Calculate data quality
        let data_quality_score = self.calculate_data_quality_score(orderbook);
        
        // Update LOB tensors
        let _ = self.lob_tensor_l10.update_from_orderbook(orderbook);
        let _ = self.lob_tensor_l20.update_from_orderbook(orderbook);
        
        // Extract tensor features
        let lob_tensor_l10_features = self.lob_tensor_l10.get_features();
        let lob_tensor_l20_features = self.lob_tensor_l20.get_features();
        
        // Update historical data
        self.update_history(basic_features, current_time);
        
        Ok(AdvancedFeatures {
            realized_volatility,
            volatility_5_tick,
            volatility_10_tick,
            volatility_20_tick,
            momentum_5_tick,
            momentum_10_tick,
            momentum_20_tick,
            price_acceleration,
            volume_acceleration,
            bid_ask_correlation,
            trade_intensity,
            order_arrival_rate,
            cancellation_rate,
            data_quality_score,
            lob_tensor_l10_features,
            lob_tensor_l20_features,
        })
    }

    /// Calculate realized volatility
    fn calculate_realized_volatility(&self) -> f64 {
        if self.price_change_history.len() < 2 {
            return 0.0;
        }

        let variance: f64 = self.price_change_history.iter()
            .map(|&x| x * x)
            .sum::<f64>() / self.price_change_history.len() as f64;

        variance.sqrt()
    }

    /// Calculate multi-timeframe volatility
    fn calculate_multi_volatility(&self) -> (f64, f64, f64) {
        let vol_5 = self.calculate_volatility_window(5);
        let vol_10 = self.calculate_volatility_window(10);
        let vol_20 = self.calculate_volatility_window(20);
        
        (vol_5, vol_10, vol_20)
    }

    /// Calculate volatility for specific window
    fn calculate_volatility_window(&self, window: usize) -> f64 {
        if self.price_change_history.len() < window {
            return 0.0;
        }

        let changes: Vec<f64> = self.price_change_history.iter().rev().take(window).copied().collect();
        let mean = changes.iter().sum::<f64>() / changes.len() as f64;
        let variance = changes.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / changes.len() as f64;
        
        variance.sqrt()
    }

    /// Calculate multi-timeframe momentum
    fn calculate_multi_momentum(&self) -> (f64, f64, f64) {
        let mom_5 = self.calculate_momentum_window(5);
        let mom_10 = self.calculate_momentum_window(10);
        let mom_20 = self.calculate_momentum_window(20);
        
        (mom_5, mom_10, mom_20)
    }

    /// Calculate momentum for specific window
    fn calculate_momentum_window(&self, window: usize) -> f64 {
        if self.price_change_history.len() < window {
            return 0.0;
        }

        self.price_change_history.iter().rev().take(window).sum::<f64>()
    }

    /// Calculate price acceleration
    fn calculate_price_acceleration(&self) -> f64 {
        if self.price_change_history.len() < 3 {
            return 0.0;
        }

        let recent_changes: Vec<f64> = self.price_change_history.iter().rev().take(3).copied().collect();
        if recent_changes.len() < 3 {
            return 0.0;
        }

        // Simple acceleration: difference between recent and earlier momentum
        let recent_momentum = (recent_changes[0] + recent_changes[1]) / 2.0;
        let earlier_momentum = (recent_changes[1] + recent_changes[2]) / 2.0;
        
        recent_momentum - earlier_momentum
    }

    /// Calculate volume acceleration
    fn calculate_volume_acceleration(&self) -> f64 {
        if self.volume_change_history.len() < 3 {
            return 0.0;
        }

        let recent_changes: Vec<f64> = self.volume_change_history.iter().rev().take(3).copied().collect();
        if recent_changes.len() < 3 {
            return 0.0;
        }

        let recent_momentum = (recent_changes[0] + recent_changes[1]) / 2.0;
        let earlier_momentum = (recent_changes[1] + recent_changes[2]) / 2.0;
        
        recent_momentum - earlier_momentum
    }

    /// Calculate bid-ask correlation
    fn calculate_bid_ask_correlation(&self) -> f64 {
        let min_len = self.bid_history.len().min(self.ask_history.len());
        if min_len < 2 {
            return 0.0;
        }

        let bid_values: Vec<f64> = self.bid_history.iter().copied().collect();
        let ask_values: Vec<f64> = self.ask_history.iter().copied().collect();

        self.calculate_correlation(&bid_values, &ask_values)
    }

    /// Calculate trade intensity
    fn calculate_trade_intensity(&self) -> f64 {
        if self.trade_intensity_history.is_empty() {
            return 0.0;
        }

        self.trade_intensity_history.iter().sum::<f64>() / self.trade_intensity_history.len() as f64
    }

    /// Calculate order arrival rate
    fn calculate_order_arrival_rate(&self) -> f64 {
        // Simplified calculation based on historical intensity
        self.calculate_trade_intensity() * 1.5 // Assumption: orders arrive 1.5x trade rate
    }

    /// Calculate cancellation rate
    fn calculate_cancellation_rate(&self) -> f64 {
        // Simplified calculation
        self.calculate_order_arrival_rate() * 0.7 // Assumption: 70% cancellation rate
    }

    /// Calculate data quality score
    fn calculate_data_quality_score(&self, orderbook: &OrderBook) -> f64 {
        let mut score = 1.0f64;

        // Penalize for invalid orderbook
        if !orderbook.is_valid {
            score *= 0.5;
        }

        // Penalize for wide spreads
        let spread_bps = orderbook.spread_bps().unwrap_or(0.0);
        if spread_bps > 10.0 {
            score *= 0.8;
        }

        // Penalize for low depth
        let total_depth = orderbook.calculate_depth(5, Side::Bid) + 
                         orderbook.calculate_depth(5, Side::Ask);
        if total_depth < 1000.0 {
            score *= 0.9;
        }

        score.max(0.0f64).min(1.0f64)
    }

    /// Calculate correlation between two series
    fn calculate_correlation(&self, x: &[f64], y: &[f64]) -> f64 {
        if x.len() != y.len() || x.len() < 2 {
            return 0.0;
        }

        let n = x.len() as f64;
        let sum_x = x.iter().sum::<f64>();
        let sum_y = y.iter().sum::<f64>();
        let sum_xy = x.iter().zip(y.iter()).map(|(xi, yi)| xi * yi).sum::<f64>();
        let sum_x2 = x.iter().map(|xi| xi * xi).sum::<f64>();
        let sum_y2 = y.iter().map(|yi| yi * yi).sum::<f64>();

        let numerator = n * sum_xy - sum_x * sum_y;
        let denominator = ((n * sum_x2 - sum_x * sum_x) * (n * sum_y2 - sum_y * sum_y)).sqrt();

        if denominator == 0.0 {
            0.0
        } else {
            numerator / denominator
        }
    }

    /// Update historical data
    fn update_history(&mut self, basic_features: &BasicFeatures, current_time: Timestamp) {
        // Calculate price change
        if let Some(&prev_price) = self.price_change_history.back() {
            let price_change = basic_features.mid_price.0 - prev_price;
            
            if self.price_change_history.len() >= self.window_size {
                self.price_change_history.pop_front();
            }
            self.price_change_history.push_back(price_change);
        } else {
            self.price_change_history.push_back(0.0); // First entry
        }

        // Calculate volume change
        if let Some(&prev_volume) = self.volume_change_history.back() {
            let volume_change = basic_features.total_volume - prev_volume;
            
            if self.volume_change_history.len() >= self.window_size {
                self.volume_change_history.pop_front();
            }
            self.volume_change_history.push_back(volume_change);
        } else {
            self.volume_change_history.push_back(0.0); // First entry
        }

        // Update bid/ask history
        if self.bid_history.len() >= self.window_size {
            self.bid_history.pop_front();
        }
        self.bid_history.push_back(basic_features.best_bid.0);

        if self.ask_history.len() >= self.window_size {
            self.ask_history.pop_front();
        }
        self.ask_history.push_back(basic_features.best_ask.0);

        // Update trade intensity (simplified)
        let time_delta = current_time - self.last_update;
        let intensity = if time_delta > 0 { 1_000_000.0 / time_delta as f64 } else { 0.0 };
        
        if self.trade_intensity_history.len() >= self.window_size {
            self.trade_intensity_history.pop_front();
        }
        self.trade_intensity_history.push_back(intensity);

        self.last_update = current_time;
    }
}

/// LOB Tensor L10 placeholder
#[derive(Debug)]
pub struct LobTensorL10 {
    window_size: usize,
    // Add tensor data structures here
}

impl LobTensorL10 {
    pub fn new(window_size: usize) -> Self {
        Self { window_size }
    }

    pub fn update_from_orderbook(&mut self, _orderbook: &OrderBook) -> Result<()> {
        // Implementation would update tensor from orderbook
        Ok(())
    }

    pub fn get_features(&self) -> Vec<f64> {
        // Return flattened tensor features
        vec![0.0; 10] // Placeholder
    }
}

/// LOB Tensor L20 placeholder
#[derive(Debug)]
pub struct LobTensorL20 {
    window_size: usize,
    // Add tensor data structures here
}

impl LobTensorL20 {
    pub fn new(window_size: usize) -> Self {
        Self { window_size }
    }

    pub fn update_from_orderbook(&mut self, _orderbook: &OrderBook) -> Result<()> {
        // Implementation would update tensor from orderbook
        Ok(())
    }

    pub fn get_features(&self) -> Vec<f64> {
        // Return flattened tensor features
        vec![0.0; 20] // Placeholder
    }
}