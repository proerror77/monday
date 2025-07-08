/*!
 * Core data types for Rust HFT system
 */

use ordered_float::OrderedFloat;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

/// High-precision price representation using OrderedFloat for BTreeMap keys
pub type Price = OrderedFloat<f64>;

/// High-precision quantity using Decimal for exact arithmetic
pub type Quantity = Decimal;

/// Microsecond timestamp for ultra-precise timing
pub type Timestamp = u64;

/// LOB Tensor configuration constants
pub const LOB_TENSOR_L10_SIZE: usize = 10;
pub const LOB_TENSOR_L20_SIZE: usize = 20;

/// LOB Tensor for L=10 configuration (T, 2L) format
/// Represents orderbook state as a tensor: T ticks × 2L levels (L bids + L asks)
/// Each entry contains log-normalized quantities with relative price offsets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobTensorL10 {
    /// Tensor data: [T][2L] where T is time steps, 2L is bid/ask levels
    /// First L columns are bids (best to worst), next L columns are asks (best to worst)
    pub data: Vec<Vec<f64>>,
    
    /// Relative price offsets from mid-price (basis points)
    pub price_offsets: Vec<f64>,
    
    /// Current time step index
    pub current_step: usize,
    
    /// Maximum time steps (rolling window size)
    pub max_steps: usize,
    
    /// Mid-price reference for normalization
    pub mid_price_ref: f64,
    
    /// Timestamp of last update
    pub last_update: Timestamp,
}

/// LOB Tensor for L=20 configuration (T, 2L) format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobTensorL20 {
    /// Tensor data: [T][2L] where T is time steps, 2L is bid/ask levels
    pub data: Vec<Vec<f64>>,
    
    /// Relative price offsets from mid-price (basis points)
    pub price_offsets: Vec<f64>,
    
    /// Current time step index
    pub current_step: usize,
    
    /// Maximum time steps (rolling window size)
    pub max_steps: usize,
    
    /// Mid-price reference for normalization
    pub mid_price_ref: f64,
    
    /// Timestamp of last update
    pub last_update: Timestamp,
}

impl LobTensorL10 {
    /// Create new LOB tensor for L=10 configuration
    pub fn new(max_steps: usize) -> Self {
        Self {
            data: vec![vec![0.0; 2 * LOB_TENSOR_L10_SIZE]; max_steps],
            price_offsets: vec![0.0; 2 * LOB_TENSOR_L10_SIZE],
            current_step: 0,
            max_steps,
            mid_price_ref: 0.0,
            last_update: now_micros(),
        }
    }
    
    /// Update tensor with new orderbook state
    pub fn update_from_orderbook(&mut self, orderbook: &crate::orderbook::OrderBook) -> anyhow::Result<()> {
        let mid_price = orderbook.mid_price().unwrap_or(0.0.to_price()).0;
        if mid_price <= 0.0 {
            return Err(anyhow::anyhow!("Invalid mid price"));
        }
        
        self.mid_price_ref = mid_price;
        
        // Advance to next time step
        self.current_step = (self.current_step + 1) % self.max_steps;
        
        // Extract top L bids and asks
        let mut tensor_row = vec![0.0; 2 * LOB_TENSOR_L10_SIZE];
        let mut price_offsets = vec![0.0; 2 * LOB_TENSOR_L10_SIZE];
        
        // Process bids (first L columns)
        for (i, (price, qty)) in orderbook.bids.iter().rev().take(LOB_TENSOR_L10_SIZE).enumerate() {
            let relative_price = (price.0 - mid_price) / mid_price * 10000.0; // basis points
            let log_qty = (qty.to_f64().unwrap_or(0.0) + 1.0).ln();
            
            tensor_row[i] = log_qty;
            price_offsets[i] = relative_price;
        }
        
        // Process asks (next L columns)
        for (i, (price, qty)) in orderbook.asks.iter().take(LOB_TENSOR_L10_SIZE).enumerate() {
            let relative_price = (price.0 - mid_price) / mid_price * 10000.0; // basis points
            let log_qty = (qty.to_f64().unwrap_or(0.0) + 1.0).ln();
            
            tensor_row[LOB_TENSOR_L10_SIZE + i] = log_qty;
            price_offsets[LOB_TENSOR_L10_SIZE + i] = relative_price;
        }
        
        // Update tensor data
        self.data[self.current_step] = tensor_row;
        self.price_offsets = price_offsets;
        self.last_update = now_micros();
        
        Ok(())
    }
    
    /// Get current tensor slice (last T steps)
    pub fn get_current_slice(&self, steps: usize) -> Vec<Vec<f64>> {
        let steps = steps.min(self.max_steps);
        let mut result = Vec::with_capacity(steps);
        
        for i in 0..steps {
            let idx = (self.current_step + self.max_steps - steps + i + 1) % self.max_steps;
            result.push(self.data[idx].clone());
        }
        
        result
    }
    
    /// Extract feature vector from tensor
    pub fn extract_features(&self) -> Vec<f64> {
        let mut features = Vec::new();
        
        // Current orderbook state (latest step)
        let current_state = &self.data[self.current_step];
        
        // Bid-ask imbalance features
        let bid_volume: f64 = current_state[..LOB_TENSOR_L10_SIZE].iter().sum();
        let ask_volume: f64 = current_state[LOB_TENSOR_L10_SIZE..].iter().sum();
        let total_volume = bid_volume + ask_volume;
        
        if total_volume > 0.0 {
            features.push((bid_volume - ask_volume) / total_volume); // Overall imbalance
        } else {
            features.push(0.0);
        }
        
        // Level-wise imbalances
        for i in 0..LOB_TENSOR_L10_SIZE {
            let bid_qty = current_state[i];
            let ask_qty = current_state[LOB_TENSOR_L10_SIZE + i];
            let level_total = bid_qty + ask_qty;
            
            if level_total > 0.0 {
                features.push((bid_qty - ask_qty) / level_total);
            } else {
                features.push(0.0);
            }
        }
        
        // Price gradient features
        if self.price_offsets.len() >= 2 * LOB_TENSOR_L10_SIZE {
            let bid_gradient = if LOB_TENSOR_L10_SIZE > 1 {
                self.price_offsets[LOB_TENSOR_L10_SIZE - 1] - self.price_offsets[0]
            } else { 0.0 };
            
            let ask_gradient = if LOB_TENSOR_L10_SIZE > 1 {
                self.price_offsets[2 * LOB_TENSOR_L10_SIZE - 1] - self.price_offsets[LOB_TENSOR_L10_SIZE]
            } else { 0.0 };
            
            features.push(bid_gradient);
            features.push(ask_gradient);
        }
        
        features
    }
    
    /// Get memory usage in bytes
    pub fn memory_usage(&self) -> usize {
        std::mem::size_of::<Self>() + 
        (self.data.len() * self.data.capacity() * std::mem::size_of::<f64>()) +
        (self.price_offsets.len() * std::mem::size_of::<f64>())
    }
    
    /// Compact memory by shrinking vectors to fit
    pub fn compact_memory(&mut self) {
        for row in &mut self.data {
            row.shrink_to_fit();
        }
        self.price_offsets.shrink_to_fit();
    }
    
    /// Get slice without copying (memory efficient)
    pub fn get_current_slice_ref(&self, steps: usize) -> Vec<&[f64]> {
        let steps = steps.min(self.max_steps);
        let mut result = Vec::with_capacity(steps);
        
        for i in 0..steps {
            let idx = (self.current_step + self.max_steps - steps + i + 1) % self.max_steps;
            result.push(&self.data[idx][..]);
        }
        
        result
    }
}

impl LobTensorL20 {
    /// Create new LOB tensor for L=20 configuration
    pub fn new(max_steps: usize) -> Self {
        Self {
            data: vec![vec![0.0; 2 * LOB_TENSOR_L20_SIZE]; max_steps],
            price_offsets: vec![0.0; 2 * LOB_TENSOR_L20_SIZE],
            current_step: 0,
            max_steps,
            mid_price_ref: 0.0,
            last_update: now_micros(),
        }
    }
    
    /// Update tensor with new orderbook state
    pub fn update_from_orderbook(&mut self, orderbook: &crate::orderbook::OrderBook) -> anyhow::Result<()> {
        let mid_price = orderbook.mid_price().unwrap_or(0.0.to_price()).0;
        if mid_price <= 0.0 {
            return Err(anyhow::anyhow!("Invalid mid price"));
        }
        
        self.mid_price_ref = mid_price;
        
        // Advance to next time step
        self.current_step = (self.current_step + 1) % self.max_steps;
        
        // Extract top L bids and asks
        let mut tensor_row = vec![0.0; 2 * LOB_TENSOR_L20_SIZE];
        let mut price_offsets = vec![0.0; 2 * LOB_TENSOR_L20_SIZE];
        
        // Process bids (first L columns)
        for (i, (price, qty)) in orderbook.bids.iter().rev().take(LOB_TENSOR_L20_SIZE).enumerate() {
            let relative_price = (price.0 - mid_price) / mid_price * 10000.0; // basis points
            let log_qty = (qty.to_f64().unwrap_or(0.0) + 1.0).ln();
            
            tensor_row[i] = log_qty;
            price_offsets[i] = relative_price;
        }
        
        // Process asks (next L columns)
        for (i, (price, qty)) in orderbook.asks.iter().take(LOB_TENSOR_L20_SIZE).enumerate() {
            let relative_price = (price.0 - mid_price) / mid_price * 10000.0; // basis points
            let log_qty = (qty.to_f64().unwrap_or(0.0) + 1.0).ln();
            
            tensor_row[LOB_TENSOR_L20_SIZE + i] = log_qty;
            price_offsets[LOB_TENSOR_L20_SIZE + i] = relative_price;
        }
        
        // Update tensor data
        self.data[self.current_step] = tensor_row;
        self.price_offsets = price_offsets;
        self.last_update = now_micros();
        
        Ok(())
    }
    
    /// Get current tensor slice (last T steps)
    pub fn get_current_slice(&self, steps: usize) -> Vec<Vec<f64>> {
        let steps = steps.min(self.max_steps);
        let mut result = Vec::with_capacity(steps);
        
        for i in 0..steps {
            let idx = (self.current_step + self.max_steps - steps + i + 1) % self.max_steps;
            result.push(self.data[idx].clone());
        }
        
        result
    }
    
    /// Extract feature vector from tensor
    pub fn extract_features(&self) -> Vec<f64> {
        let mut features = Vec::new();
        
        // Current orderbook state (latest step)
        let current_state = &self.data[self.current_step];
        
        // Bid-ask imbalance features
        let bid_volume: f64 = current_state[..LOB_TENSOR_L20_SIZE].iter().sum();
        let ask_volume: f64 = current_state[LOB_TENSOR_L20_SIZE..].iter().sum();
        let total_volume = bid_volume + ask_volume;
        
        if total_volume > 0.0 {
            features.push((bid_volume - ask_volume) / total_volume); // Overall imbalance
        } else {
            features.push(0.0);
        }
        
        // Level-wise imbalances (top 10 levels only to match L10)
        for i in 0..LOB_TENSOR_L10_SIZE {
            let bid_qty = current_state[i];
            let ask_qty = current_state[LOB_TENSOR_L20_SIZE + i];
            let level_total = bid_qty + ask_qty;
            
            if level_total > 0.0 {
                features.push((bid_qty - ask_qty) / level_total);
            } else {
                features.push(0.0);
            }
        }
        
        // Price gradient features
        if self.price_offsets.len() >= 2 * LOB_TENSOR_L20_SIZE {
            let bid_gradient = if LOB_TENSOR_L20_SIZE > 1 {
                self.price_offsets[LOB_TENSOR_L20_SIZE - 1] - self.price_offsets[0]
            } else { 0.0 };
            
            let ask_gradient = if LOB_TENSOR_L20_SIZE > 1 {
                self.price_offsets[2 * LOB_TENSOR_L20_SIZE - 1] - self.price_offsets[LOB_TENSOR_L20_SIZE]
            } else { 0.0 };
            
            features.push(bid_gradient);
            features.push(ask_gradient);
        }
        
        features
    }
    
    /// Get memory usage in bytes
    pub fn memory_usage(&self) -> usize {
        std::mem::size_of::<Self>() + 
        (self.data.len() * self.data.capacity() * std::mem::size_of::<f64>()) +
        (self.price_offsets.len() * std::mem::size_of::<f64>())
    }
    
    /// Compact memory by shrinking vectors to fit
    pub fn compact_memory(&mut self) {
        for row in &mut self.data {
            row.shrink_to_fit();
        }
        self.price_offsets.shrink_to_fit();
    }
    
    /// Get slice without copying (memory efficient)
    pub fn get_current_slice_ref(&self, steps: usize) -> Vec<&[f64]> {
        let steps = steps.min(self.max_steps);
        let mut result = Vec::with_capacity(steps);
        
        for i in 0..steps {
            let idx = (self.current_step + self.max_steps - steps + i + 1) % self.max_steps;
            result.push(&self.data[idx][..]);
        }
        
        result
    }
}

/// Raw message from WebSocket
#[derive(Debug, Clone)]
pub struct RawMessage {
    pub data: String,
    pub received_at: Timestamp,
    pub sequence: Option<u64>,
}

/// OrderBook side enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Bid,
    Ask,
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Side::Bid => write!(f, "BUY"),
            Side::Ask => write!(f, "SELL"),
        }
    }
}

/// Individual price level in orderbook
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PriceLevel {
    pub price: Price,
    pub quantity: Quantity,
    pub side: Side,
}

/// OrderBook update event
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrderBookUpdate {
    pub symbol: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub timestamp: Timestamp,
    pub sequence_start: u64,
    pub sequence_end: u64,
    pub is_snapshot: bool,
}

impl Default for OrderBookUpdate {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            bids: Vec::new(),
            asks: Vec::new(),
            timestamp: now_micros(),
            sequence_start: 0,
            sequence_end: 0,
            is_snapshot: false,
        }
    }
}

/// Comprehensive feature set for ML model
#[derive(Debug, Clone)]
pub struct FeatureSet {
    // Timing
    pub timestamp: Timestamp,
    pub latency_network_us: u64,
    pub latency_processing_us: u64,
    
    // Basic orderbook features
    pub best_bid: Price,
    pub best_ask: Price,
    pub mid_price: Price,
    pub spread: f64,
    pub spread_bps: f64,
    
    // Multi-level OBI (Order Book Imbalance)
    pub obi_l1: f64,   // 1-level OBI
    pub obi_l5: f64,   // 5-level OBI  
    pub obi_l10: f64,  // 10-level OBI
    pub obi_l20: f64,  // 20-level OBI
    
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
    
    // Orderbook slope (price elasticity)
    pub bid_slope: f64,
    pub ask_slope: f64,
    
    // Volume features
    pub total_bid_levels: usize,
    pub total_ask_levels: usize,
    
    // Derived features
    pub price_momentum: f64,
    pub volume_imbalance: f64,
    
    // Quality indicators
    pub is_valid: bool,
    pub data_quality_score: f64,
    
    // Enhanced features
    pub microprice: f64,              // Weighted mid price
    pub vwap: f64,                    // Volume-weighted average price
    pub realized_volatility: f64,     // Short-term volatility
    pub effective_spread: f64,        // Effective spread estimation
    pub price_acceleration: f64,      // Second derivative of price
    pub volume_acceleration: f64,     // Second derivative of volume
    pub order_flow_imbalance: f64,    // Net order flow
    pub trade_intensity: f64,         // Trade arrival intensity
    pub bid_ask_correlation: f64,     // Correlation between bid/ask movements
    pub market_impact: f64,           // Estimated market impact
    pub liquidity_score: f64,         // Overall liquidity assessment
    pub momentum_5_tick: f64,         // 5-tick momentum
    pub momentum_10_tick: f64,        // 10-tick momentum  
    pub momentum_20_tick: f64,        // 20-tick momentum
    pub volatility_5_tick: f64,       // 5-tick volatility
    pub volatility_10_tick: f64,      // 10-tick volatility
    pub volatility_20_tick: f64,      // 20-tick volatility
    pub depth_pressure_bid: f64,      // Bid side depth pressure
    pub depth_pressure_ask: f64,      // Ask side depth pressure
    pub order_arrival_rate: f64,      // Order arrival intensity
    pub cancellation_rate: f64,       // Order cancellation rate
    
    // LOB Tensor features
    pub lob_tensor_l10: Option<LobTensorL10>,  // L=10 LOB tensor
    pub lob_tensor_l20: Option<LobTensorL20>,  // L=20 LOB tensor
}

impl FeatureSet {
    /// Create a default FeatureSet with enhanced features set to zero
    pub fn default_enhanced() -> Self {
        Self {
            timestamp: now_micros(),
            latency_network_us: 0,
            latency_processing_us: 0,
            best_bid: 0.0.to_price(),
            best_ask: 0.0.to_price(),
            mid_price: 0.0.to_price(),
            spread: 0.0,
            spread_bps: 0.0,
            obi_l1: 0.0,
            obi_l5: 0.0,
            obi_l10: 0.0,
            obi_l20: 0.0,
            bid_depth_l5: 0.0,
            ask_depth_l5: 0.0,
            bid_depth_l10: 0.0,
            ask_depth_l10: 0.0,
            bid_depth_l20: 0.0,
            ask_depth_l20: 0.0,
            depth_imbalance_l5: 0.0,
            depth_imbalance_l10: 0.0,
            depth_imbalance_l20: 0.0,
            bid_slope: 0.0,
            ask_slope: 0.0,
            total_bid_levels: 0,
            total_ask_levels: 0,
            price_momentum: 0.0,
            volume_imbalance: 0.0,
            is_valid: true,
            data_quality_score: 1.0,
            
            // Enhanced features with defaults
            microprice: 0.0,
            vwap: 0.0,
            realized_volatility: 0.0,
            effective_spread: 0.0,
            price_acceleration: 0.0,
            volume_acceleration: 0.0,
            order_flow_imbalance: 0.0,
            trade_intensity: 0.0,
            bid_ask_correlation: 0.0,
            market_impact: 0.0,
            liquidity_score: 0.0,
            momentum_5_tick: 0.0,
            momentum_10_tick: 0.0,
            momentum_20_tick: 0.0,
            volatility_5_tick: 0.0,
            volatility_10_tick: 0.0,
            volatility_20_tick: 0.0,
            depth_pressure_bid: 0.0,
            depth_pressure_ask: 0.0,
            order_arrival_rate: 0.0,
            cancellation_rate: 0.0,
            
            // LOB Tensor features - initialized as None to maintain compatibility
            lob_tensor_l10: None,
            lob_tensor_l20: None,
        }
    }
}

/// Orderbook snapshot for delta calculations
#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    pub timestamp: Timestamp,
    pub best_bid: Price,
    pub best_ask: Price,
    pub total_bid_volume: f64,
    pub total_ask_volume: f64,
    pub bid_levels: usize,
    pub ask_levels: usize,
    pub spread: f64,
}

/// Technical analysis indicators
#[derive(Debug, Clone)]
pub struct TechnicalIndicators {
    pub sma_5: f64,     // Simple Moving Average 5
    pub sma_10: f64,    // Simple Moving Average 10  
    pub sma_20: f64,    // Simple Moving Average 20
    pub ema_5: f64,     // Exponential Moving Average 5
    pub ema_10: f64,    // Exponential Moving Average 10
    pub ema_20: f64,    // Exponential Moving Average 20
    pub rsi: f64,       // Relative Strength Index
    pub macd: f64,      // MACD
    pub macd_signal: f64, // MACD Signal
    pub bollinger_upper: f64,  // Bollinger Upper Band
    pub bollinger_lower: f64,  // Bollinger Lower Band
    pub atr: f64,       // Average True Range
}

/// Trading signal from strategy
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalType {
    Buy,
    Sell,
    Hold,
}

/// Trading signal with metadata
#[derive(Debug, Clone)]
pub struct TradingSignal {
    pub signal_type: SignalType,
    pub confidence: f64,
    pub suggested_price: Price,
    pub suggested_quantity: Quantity,
    pub timestamp: Timestamp,
    pub features_timestamp: Timestamp,
    pub signal_latency_us: u64,
}

/// Order types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderType {
    Market,
    Limit,
    PostOnly,
}

/// Order time in force
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimeInForce {
    GTC,  // Good Till Cancel
    IOC,  // Immediate Or Cancel
    FOK,  // Fill Or Kill
}

/// Order status
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OrderStatus {
    New,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// Trading order
#[derive(Debug, Clone)]
pub struct Order {
    pub order_id: String,
    pub symbol: String,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: Quantity,
    pub price: Option<Price>,
    pub time_in_force: TimeInForce,
    pub status: OrderStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Market statistics
#[derive(Debug, Clone, Default)]
pub struct MarketStats {
    pub messages_received: u64,
    pub messages_processed: u64,
    pub orderbook_updates: u64,
    pub features_generated: u64,
    pub signals_generated: u64,
    pub orders_placed: u64,
    
    // Latency statistics (in microseconds)
    pub avg_network_latency: f64,
    pub avg_processing_latency: f64,
    pub avg_total_latency: f64,
    pub max_latency: u64,
    pub min_latency: u64,
    
    // Quality metrics
    pub data_gaps: u64,
    pub sequence_errors: u64,
    pub validation_failures: u64,
}

/// Performance metrics
#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    pub timestamp: Timestamp,
    pub thread_name: String,
    pub cpu_usage: f64,
    pub memory_usage: u64,
    pub queue_depth: usize,
    pub processing_rate: f64,  // items per second
    pub latency_p50: u64,      // 50th percentile latency in μs
    pub latency_p95: u64,      // 95th percentile latency in μs  
    pub latency_p99: u64,      // 99th percentile latency in μs
}

/// System health status
#[derive(Debug, Clone, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Warning,
    Critical,
    Offline,
}

/// Component health information
#[derive(Debug, Clone)]
pub struct ComponentHealth {
    pub component: String,
    pub status: HealthStatus,
    pub last_update: Timestamp,
    pub error_count: u64,
    pub uptime_seconds: u64,
}

// Utility functions for timestamp handling
pub fn timestamp_from_system_time(time: std::time::SystemTime) -> Timestamp {
    time.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// Get current timestamp in microseconds
pub fn now_micros() -> Timestamp {
    timestamp_from_system_time(std::time::SystemTime::now())
}

/// Convert timestamp to human readable string
pub fn timestamp_to_string(ts: Timestamp) -> String {
    let secs = ts / 1_000_000;
    let micros = ts % 1_000_000;
    format!("{secs}.{micros:06}")
}

// Helper traits for conversions
pub trait ToPrice {
    fn to_price(self) -> Price;
}

impl ToPrice for f64 {
    fn to_price(self) -> Price {
        OrderedFloat(self)
    }
}

pub trait ToQuantity {
    fn to_quantity(self) -> Quantity;
}

impl ToQuantity for f64 {
    fn to_quantity(self) -> Quantity {
        Decimal::from_f64_retain(self).unwrap_or_default()
    }
}

impl ToQuantity for &str {
    fn to_quantity(self) -> Quantity {
        self.parse().unwrap_or_default()
    }
}