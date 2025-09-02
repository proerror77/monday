//! Trend strategy template implementing `ports::Strategy`
//! - Pure function + small state machine, triggered by bar close events
//! - Implements simple EMA crossover with RSI filter

use hft_core::{HftResult, Symbol, Price, Quantity, Side, OrderType, TimeInForce};
use ports::{Strategy, MarketEvent, ExecutionEvent, OrderIntent, AccountView, AggregatedBar, VenueScope};
use serde::{Deserialize, Serialize};
use num_traits::ToPrimitive;

/// EMA 指標計算器
#[derive(Debug, Clone)]
pub struct EmaCalculator {
    period: u32,
    multiplier: f64,
    value: Option<f64>,
}

impl EmaCalculator {
    pub fn new(period: u32) -> Self {
        let multiplier = 2.0 / (period as f64 + 1.0);
        Self {
            period,
            multiplier,
            value: None,
        }
    }
    
    pub fn update(&mut self, price: f64) -> Option<f64> {
        match self.value {
            None => {
                self.value = Some(price);
                self.value
            }
            Some(prev) => {
                let new_value = (price * self.multiplier) + (prev * (1.0 - self.multiplier));
                self.value = Some(new_value);
                self.value
            }
        }
    }
    
    pub fn current(&self) -> Option<f64> {
        self.value
    }
}

/// RSI 指標計算器
#[derive(Debug, Clone)]
pub struct RsiCalculator {
    period: u32,
    gains: Vec<f64>,
    losses: Vec<f64>,
    last_price: Option<f64>,
}

impl RsiCalculator {
    pub fn new(period: u32) -> Self {
        Self {
            period,
            gains: Vec::new(),
            losses: Vec::new(),
            last_price: None,
        }
    }
    
    pub fn update(&mut self, price: f64) -> Option<f64> {
        if let Some(last) = self.last_price {
            let change = price - last;
            if change >= 0.0 {
                self.gains.push(change);
                self.losses.push(0.0);
            } else {
                self.gains.push(0.0);
                self.losses.push(-change);
            }
            
            // 保持期間長度
            if self.gains.len() > self.period as usize {
                self.gains.remove(0);
                self.losses.remove(0);
            }
            
            // 計算 RSI
            if self.gains.len() == self.period as usize {
                let avg_gain = self.gains.iter().sum::<f64>() / self.period as f64;
                let avg_loss = self.losses.iter().sum::<f64>() / self.period as f64;
                
                if avg_loss == 0.0 {
                    return Some(100.0);
                }
                
                let rs = avg_gain / avg_loss;
                let rsi = 100.0 - (100.0 / (1.0 + rs));
                
                self.last_price = Some(price);
                return Some(rsi);
            }
        }
        
        self.last_price = Some(price);
        None
    }
    
    pub fn current(&self) -> Option<f64> {
        if self.gains.len() < self.period as usize {
            return None;
        }
        
        let avg_gain = self.gains.iter().sum::<f64>() / self.period as f64;
        let avg_loss = self.losses.iter().sum::<f64>() / self.period as f64;
        
        if avg_loss == 0.0 {
            return Some(100.0);
        }
        
        let rs = avg_gain / avg_loss;
        Some(100.0 - (100.0 / (1.0 + rs)))
    }
}

/// 趨勢策略配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendStrategyConfig {
    pub ema_fast_period: u32,
    pub ema_slow_period: u32,
    pub rsi_period: u32,
    pub rsi_oversold: f64,
    pub rsi_overbought: f64,
    pub position_size: f64,
    pub max_position: f64,
    pub min_spread_bps: f64,
}

impl Default for TrendStrategyConfig {
    fn default() -> Self {
        Self {
            ema_fast_period: 12,
            ema_slow_period: 26,
            rsi_period: 14,
            rsi_oversold: 30.0,
            rsi_overbought: 70.0,
            position_size: 0.1,
            max_position: 1.0,
            min_spread_bps: 5.0,
        }
    }
}

/// 交易信號
#[derive(Debug, Clone, PartialEq)]
pub enum TrendSignal {
    Buy,
    Sell,
    Hold,
}

/// 策略狀態
#[derive(Debug, Clone)]
pub struct StrategyState {
    pub ema_fast: EmaCalculator,
    pub ema_slow: EmaCalculator,
    pub rsi: RsiCalculator,
    pub last_signal: TrendSignal,
    pub last_bar_close: Option<f64>,
}

impl StrategyState {
    pub fn new(config: &TrendStrategyConfig) -> Self {
        Self {
            ema_fast: EmaCalculator::new(config.ema_fast_period),
            ema_slow: EmaCalculator::new(config.ema_slow_period),
            rsi: RsiCalculator::new(config.rsi_period),
            last_signal: TrendSignal::Hold,
            last_bar_close: None,
        }
    }
}

/// 趨勢策略實現 - Bar Close 觸發的 EMA 金叉死叉 + RSI 過濾
pub struct TrendStrategy {
    symbol: Symbol,
    config: TrendStrategyConfig,
    state: StrategyState,
    strategy_id: String,
}

impl TrendStrategy {
    /// 創建新的趨勢策略（舊版，保持向後兼容性）
    pub fn new(symbol: Symbol, config: TrendStrategyConfig) -> Self {
        // 使用基於符號的穩定 ID，而非動態時間戳
        let strategy_id = format!("trend_{}", symbol.0);
        Self::with_name(symbol, config, strategy_id)
    }
    
    /// 🔥 Phase 1.4: 創建帶穩定名稱的趨勢策略
    pub fn with_name(symbol: Symbol, config: TrendStrategyConfig, strategy_name: String) -> Self {
        let state = StrategyState::new(&config);
        
        Self {
            symbol,
            config,
            state,
            strategy_id: strategy_name,
        }
    }
    
    /// 處理新的 K 線並生成交易信號
    fn process_bar(&mut self, bar: &AggregatedBar) -> TrendSignal {
        let close_price = bar.close.0.to_f64().unwrap_or(0.0);
        
        // 更新技術指標
        let ema_fast = self.state.ema_fast.update(close_price);
        let ema_slow = self.state.ema_slow.update(close_price);
        let rsi = self.state.rsi.update(close_price);
        
        self.state.last_bar_close = Some(close_price);
        
        // 生成信號
        match (ema_fast, ema_slow, rsi) {
            (Some(fast), Some(slow), Some(rsi_value)) => {
                let signal = if fast > slow && rsi_value < self.config.rsi_overbought {
                    // 快線上穿慢線 且 RSI 未超買
                    TrendSignal::Buy
                } else if fast < slow && rsi_value > self.config.rsi_oversold {
                    // 快線下穿慢線 且 RSI 未超賣
                    TrendSignal::Sell
                } else {
                    TrendSignal::Hold
                };
                
                self.state.last_signal = signal.clone();
                signal
            }
            _ => {
                // 指標未就緒
                TrendSignal::Hold
            }
        }
    }
    
    /// 根據信號和當前倉位生成訂單意圖
    fn signal_to_order_intent(&self, signal: TrendSignal, current_position: f64, current_price: f64) -> Vec<OrderIntent> {
        let mut orders = Vec::new();
        
        match signal {
            TrendSignal::Buy => {
                // 檢查是否可以加倉或開多倉
                if current_position < self.config.max_position {
                    let target_quantity = (self.config.position_size).min(self.config.max_position - current_position);
                    
                    if target_quantity > 0.001 { // 最小交易量
                        if let (Ok(price), Ok(quantity)) = (
                            Price::from_f64(current_price),
                            Quantity::from_f64(target_quantity)
                        ) {
                            orders.push(OrderIntent {
                                symbol: self.symbol.clone(),
                                side: Side::Buy,
                                quantity,
                                order_type: OrderType::Limit,
                                price: Some(price),
                                time_in_force: TimeInForce::IOC, // 立即成交或取消
                                strategy_id: self.strategy_id.clone(),
                                target_venue: None, // 由 Router 決定路由
                            });
                        }
                    }
                }
            }
            TrendSignal::Sell => {
                // 檢查是否可以減倉或開空倉
                if current_position > -self.config.max_position {
                    let target_quantity = (self.config.position_size).min(current_position + self.config.max_position);
                    
                    if target_quantity > 0.001 { // 最小交易量
                        if let (Ok(price), Ok(quantity)) = (
                            Price::from_f64(current_price),
                            Quantity::from_f64(target_quantity)
                        ) {
                            orders.push(OrderIntent {
                                symbol: self.symbol.clone(),
                                side: Side::Sell,
                                quantity,
                                order_type: OrderType::Limit,
                                price: Some(price),
                                time_in_force: TimeInForce::IOC,
                                strategy_id: self.strategy_id.clone(),
                                target_venue: None, // 由 Router 決定路由
                            });
                        }
                    }
                }
            }
            TrendSignal::Hold => {
                // 無操作
            }
        }
        
        orders
    }
}

impl Strategy for TrendStrategy {
    fn id(&self) -> &str { &self.strategy_id }
    fn venue_scope(&self) -> VenueScope { VenueScope::Single }
    fn on_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Bar(bar) => {
                // 只處理目標品種的 K 線
                if bar.symbol != self.symbol {
                    tracing::debug!("TrendStrategy 忽略不匹配的品種: {} (期望: {})", bar.symbol.0, self.symbol.0);
                    return Vec::new();
                }
                
                tracing::info!("TrendStrategy 處理 Bar: symbol={}, close={:.2}, 時間: {}", 
                              bar.symbol.0, bar.close.0.to_f64().unwrap_or(0.0), bar.close_time);
                
                // 處理 K 線並生成信號
                let signal = self.process_bar(bar);
                
                // 獲取當前倉位
                let current_position = account.positions.get(&self.symbol)
                    .map(|pos| pos.quantity.0.to_f64().unwrap_or(0.0))
                    .unwrap_or(0.0);
                
                // 使用 K 線收盤價作為下單價格
                let current_price = bar.close.0.to_f64().unwrap_or(0.0);
                
                tracing::info!("TrendStrategy 信號: {:?}, 當前倉位: {:.3}, 價格: {:.2}", 
                              signal, current_position, current_price);
                
                // 根據信號生成訂單意圖
                let orders = self.signal_to_order_intent(signal, current_position, current_price);
                
                if !orders.is_empty() {
                    tracing::info!("TrendStrategy 生成 {} 個訂單意圖", orders.len());
                    for (i, order) in orders.iter().enumerate() {
                        tracing::info!("  訂單 {}: {:?} {:.3} @ {:.2} ({})", 
                                      i+1, order.side, order.quantity.0.to_f64().unwrap_or(0.0), 
                                      order.price.as_ref().map(|p| p.0.to_f64().unwrap_or(0.0)).unwrap_or(0.0),
                                      order.strategy_id);
                    }
                } else {
                    tracing::debug!("TrendStrategy 無訂單意圖生成");
                }
                
                orders
            }
            _ => {
                // 非 K 線事件不處理
                Vec::new()
            }
        }
    }
    
    fn on_execution_event(&mut self, _event: &ExecutionEvent, _account: &AccountView) -> Vec<OrderIntent> {
        // 執行事件暫不處理，可後續擴展
        Vec::new()
    }
    
    fn name(&self) -> &str {
        "TrendStrategy"
    }
    
    fn initialize(&mut self) -> HftResult<()> {
        log::info!("趨勢策略初始化: 品種={}, EMA({}/{}), RSI({})", 
                   self.symbol.0, 
                   self.config.ema_fast_period, 
                   self.config.ema_slow_period,
                   self.config.rsi_period);
        Ok(())
    }
    
    fn shutdown(&mut self) -> HftResult<()> {
        log::info!("趨勢策略關閉: {}", self.symbol.0);
        Ok(())
    }
}

/// 策略工廠函數
pub fn create_trend_strategy(symbol: Symbol, config: Option<TrendStrategyConfig>) -> TrendStrategy {
    let config = config.unwrap_or_default();
    TrendStrategy::new(symbol, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_ema_calculator() {
        let mut ema = EmaCalculator::new(3);
        
        // 第一個值
        assert_eq!(ema.update(10.0), Some(10.0));
        
        // 第二個值
        let result = ema.update(20.0).unwrap();
        assert!((result - 15.0).abs() < 0.01);
    }
    
    #[test]
    fn test_rsi_calculator() {
        let mut rsi = RsiCalculator::new(3);
        
        // 需要多個值才能計算 RSI
        rsi.update(10.0);
        rsi.update(12.0);
        rsi.update(11.0);
        rsi.update(13.0);
        
        let result = rsi.current();
        assert!(result.is_some());
        assert!(result.unwrap() > 0.0 && result.unwrap() < 100.0);
    }
    
    #[test]
    fn test_trend_strategy_creation() {
        let symbol = Symbol("BTCUSDT".to_string());
        let config = TrendStrategyConfig::default();
        let strategy = TrendStrategy::new(symbol, config);
        
        assert_eq!(strategy.name(), "TrendStrategy");
        assert_eq!(strategy.symbol.0, "BTCUSDT");
    }
}
