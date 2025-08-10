/*!
 * Technical Indicators - 技术指标计算器
 * 
 * 计算各种技术指标：SMA, EMA, RSI, MACD, Bollinger Bands等
 * 从原 features.rs 的技术指标部分提取
 */

use crate::ml::features::basic_features::BasicFeatures;
use anyhow::Result;
use std::collections::VecDeque;

/// Technical indicators structure
#[derive(Debug, Clone)]
pub struct TechnicalIndicators {
    pub sma_5: f64,
    pub sma_10: f64,
    pub sma_20: f64,
    pub ema_5: f64,
    pub ema_10: f64,
    pub ema_20: f64,
    pub rsi: f64,
    pub macd: f64,
    pub macd_signal: f64,
    pub bollinger_upper: f64,
    pub bollinger_lower: f64,
    pub atr: f64,
}

impl Default for TechnicalIndicators {
    fn default() -> Self {
        Self {
            sma_5: 0.0,
            sma_10: 0.0,
            sma_20: 0.0,
            ema_5: 0.0,
            ema_10: 0.0,
            ema_20: 0.0,
            rsi: 50.0,
            macd: 0.0,
            macd_signal: 0.0,
            bollinger_upper: 0.0,
            bollinger_lower: 0.0,
            atr: 0.0,
        }
    }
}

/// Technical indicator calculator
#[derive(Debug)]
pub struct TechnicalIndicatorCalculator {
    price_history: VecDeque<f64>,
    gains: VecDeque<f64>,
    losses: VecDeque<f64>,
    ema_5_prev: Option<f64>,
    ema_10_prev: Option<f64>,
    ema_20_prev: Option<f64>,
    macd_prev: Option<f64>,
    macd_signal_prev: Option<f64>,
    window_size: usize,
}

impl TechnicalIndicatorCalculator {
    /// Create new technical indicator calculator
    pub fn new(window_size: usize) -> Self {
        Self {
            price_history: VecDeque::with_capacity(window_size.max(20)),
            gains: VecDeque::with_capacity(14), // RSI period
            losses: VecDeque::with_capacity(14), // RSI period
            ema_5_prev: None,
            ema_10_prev: None,
            ema_20_prev: None,
            macd_prev: None,
            macd_signal_prev: None,
            window_size,
        }
    }

    /// Calculate all technical indicators
    pub fn calculate(&mut self, basic_features: &BasicFeatures) -> Result<TechnicalIndicators> {
        let current_price = basic_features.mid_price.0;
        
        // Update price history
        if self.price_history.len() >= 20 {
            self.price_history.pop_front();
        }
        self.price_history.push_back(current_price);

        // Update gain/loss for RSI
        self.update_gain_loss();

        Ok(TechnicalIndicators {
            sma_5: self.calculate_sma(5),
            sma_10: self.calculate_sma(10),
            sma_20: self.calculate_sma(20),
            ema_5: self.calculate_ema(5),
            ema_10: self.calculate_ema(10),
            ema_20: self.calculate_ema(20),
            rsi: self.calculate_rsi(),
            macd: self.calculate_macd().0,
            macd_signal: self.calculate_macd().1,
            bollinger_upper: self.calculate_bollinger_bands().0,
            bollinger_lower: self.calculate_bollinger_bands().1,
            atr: self.calculate_atr(),
        })
    }

    /// Calculate Simple Moving Average
    fn calculate_sma(&self, period: usize) -> f64 {
        if self.price_history.len() < period {
            return self.price_history.back().copied().unwrap_or(0.0);
        }

        let sum: f64 = self.price_history.iter().rev().take(period).sum();
        sum / period as f64
    }

    /// Calculate Exponential Moving Average
    fn calculate_ema(&mut self, period: usize) -> f64 {
        let current_price = self.price_history.back().copied().unwrap_or(0.0);
        let alpha = 2.0 / (period as f64 + 1.0);

        let prev_ema = match period {
            5 => self.ema_5_prev,
            10 => self.ema_10_prev,
            20 => self.ema_20_prev,
            _ => return current_price,
        };

        let ema = match prev_ema {
            Some(prev) => alpha * current_price + (1.0 - alpha) * prev,
            None => current_price, // First calculation
        };

        // Update stored EMA
        match period {
            5 => self.ema_5_prev = Some(ema),
            10 => self.ema_10_prev = Some(ema),
            20 => self.ema_20_prev = Some(ema),
            _ => {}
        }

        ema
    }

    /// Calculate RSI (Relative Strength Index)
    fn calculate_rsi(&self) -> f64 {
        if self.gains.len() < 14 || self.losses.len() < 14 {
            return 50.0; // Default RSI
        }

        let avg_gain: f64 = self.gains.iter().sum::<f64>() / 14.0;
        let avg_loss: f64 = self.losses.iter().sum::<f64>() / 14.0;

        if avg_loss == 0.0 {
            return 100.0;
        }

        let rs = avg_gain / avg_loss;
        100.0 - (100.0 / (1.0 + rs))
    }

    /// Calculate MACD (Moving Average Convergence Divergence)
    fn calculate_macd(&mut self) -> (f64, f64) {
        let ema_12 = self.calculate_ema(12);
        let ema_26 = self.calculate_ema(26);
        let macd = ema_12 - ema_26;

        // MACD signal line (9-period EMA of MACD)
        let signal = match self.macd_signal_prev {
            Some(prev_signal) => {
                let alpha = 2.0 / (9.0 + 1.0);
                alpha * macd + (1.0 - alpha) * prev_signal
            }
            None => macd, // First calculation
        };

        self.macd_prev = Some(macd);
        self.macd_signal_prev = Some(signal);

        (macd, signal)
    }

    /// Calculate Bollinger Bands
    fn calculate_bollinger_bands(&self) -> (f64, f64) {
        let sma_20 = self.calculate_sma(20);
        let std_dev = self.calculate_standard_deviation(20);

        let upper = sma_20 + 2.0 * std_dev;
        let lower = sma_20 - 2.0 * std_dev;

        (upper, lower)
    }

    /// Calculate Average True Range (ATR)
    fn calculate_atr(&self) -> f64 {
        if self.price_history.len() < 2 {
            return 0.0;
        }

        // Simplified ATR calculation based on price changes
        let price_changes: Vec<f64> = self.price_history
            .iter()
            .zip(self.price_history.iter().skip(1))
            .map(|(prev, curr)| (curr - prev).abs())
            .collect();

        if price_changes.is_empty() {
            return 0.0;
        }

        price_changes.iter().sum::<f64>() / price_changes.len() as f64
    }

    /// Calculate standard deviation
    fn calculate_standard_deviation(&self, period: usize) -> f64 {
        if self.price_history.len() < period {
            return 0.0;
        }

        let prices: Vec<f64> = self.price_history.iter().rev().take(period).copied().collect();
        let mean = prices.iter().sum::<f64>() / period as f64;
        
        let variance = prices.iter()
            .map(|price| (price - mean).powi(2))
            .sum::<f64>() / period as f64;

        variance.sqrt()
    }

    /// Update gain/loss for RSI calculation
    fn update_gain_loss(&mut self) {
        if self.price_history.len() < 2 {
            return;
        }

        let current = self.price_history.back().copied().unwrap_or(0.0);
        let previous = self.price_history.iter().nth_back(1).copied().unwrap_or(0.0);
        let change = current - previous;

        let (gain, loss) = if change > 0.0 {
            (change, 0.0)
        } else {
            (0.0, -change)
        };

        // Update gains history
        if self.gains.len() >= 14 {
            self.gains.pop_front();
        }
        self.gains.push_back(gain);

        // Update losses history
        if self.losses.len() >= 14 {
            self.losses.pop_front();
        }
        self.losses.push_back(loss);
    }
}