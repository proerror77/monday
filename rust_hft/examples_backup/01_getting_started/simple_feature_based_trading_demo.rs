/*!
 * Simple Feature-Based Trading Demo
 * 
 * 基於特徵工程的簡化交易信號生成系統：
 * 1. 真實LOB數據接收
 * 2. 基礎特徵提取（OBI、spread、momentum）
 * 3. 規則基礎的交易信號生成
 * 4. 模擬交易執行
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
    core::{types::*, orderbook::OrderBook},
};
use anyhow::Result;
use tracing::{info, warn};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::Duration;
use serde_json::Value;

/// 特徵基礎交易系統
#[derive(Debug)]
pub struct FeatureBasedTradingSystem {
    /// 特徵提取器
    feature_extractor: FeatureExtractor,
    
    /// OrderBook歷史
    orderbook_history: Arc<Mutex<VecDeque<OrderBook>>>,
    
    /// 特徵歷史
    feature_history: Arc<Mutex<VecDeque<FeatureSet>>>,
    
    /// 系統統計
    stats: Arc<Mutex<TradingStats>>,
    
    /// 最後交易時間（用於冷卻期）
    last_trade_time: Arc<Mutex<u64>>,
}

#[derive(Debug, Default)]
pub struct TradingStats {
    pub total_updates: u64,
    pub feature_extractions: u64,
    pub trading_signals: u64,
    pub buy_signals: u64,
    pub sell_signals: u64,
    pub last_price: f64,
    pub spread_bps: f64,
    pub current_position: f64, // 當前倉位 (USDT)
    pub total_pnl: f64,        // 總P&L
    pub win_rate: f64,         // 勝率
    pub rejected_signals: u64, // 被拒絕的信號數
    pub closed_positions: u64, // 平倉次數
    pub entry_price: f64,      // 進場價格
    pub position_start_time: u64, // 持倉開始時間
}

#[derive(Debug, Clone)]
pub struct TradingSignal {
    pub timestamp: u64,
    pub direction: TradeDirection,
    pub confidence: f64,
    pub position_size: f64,
    pub entry_price: f64,
    pub reasoning: String,
}

#[derive(Debug, Clone)]
pub enum TradeDirection {
    Buy,
    Sell,
    Close,
}

impl FeatureBasedTradingSystem {
    pub fn new() -> Result<Self> {
        let feature_extractor = FeatureExtractor::new(100); // 100個窗口大小
        
        Ok(Self {
            feature_extractor,
            orderbook_history: Arc::new(Mutex::new(VecDeque::with_capacity(100))),
            feature_history: Arc::new(Mutex::new(VecDeque::with_capacity(50))),
            stats: Arc::new(Mutex::new(TradingStats::default())),
            last_trade_time: Arc::new(Mutex::new(0)),
        })
    }
    
    /// 處理Bitget LOB數據
    pub fn process_bitget_orderbook(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != "BTCUSDT" {
            return Ok(());
        }

        // 解析和創建OrderBook
        let orderbook = self.parse_bitget_lob_data(data, timestamp)?;
        
        // 計算價格統計
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        let mid_price = (best_bid + best_ask) / 2.0;
        
        // 更新統計
        {
            let mut stats = self.stats.lock().unwrap();
            stats.total_updates += 1;
            if mid_price > 0.0 {
                stats.last_price = mid_price;
                stats.spread_bps = (best_ask - best_bid) / mid_price * 10000.0;
            }
        }
        
        // 添加到歷史
        {
            let mut history = self.orderbook_history.lock().unwrap();
            history.push_back(orderbook.clone());
            if history.len() > 100 {
                history.pop_front();
            }
        }
        
        // 檢查止損/止盈條件
        self.check_stop_conditions(mid_price, timestamp);
        
        // 特徵提取和信號生成
        if self.orderbook_history.lock().unwrap().len() >= 10 {
            self.extract_features_and_generate_signals(&orderbook)?;
        }
        
        Ok(())
    }
    
    /// 解析Bitget LOB數據
    fn parse_bitget_lob_data(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // 解析bids
        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
            for bid in bid_data.iter().take(5) {
                if let Some(bid_array) = bid.as_array() {
                    if bid_array.len() >= 2 {
                        let price: f64 = bid_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = bid_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        if price > 0.0 && qty > 0.0 {
                            use ordered_float::OrderedFloat;
                            use rust_decimal::Decimal;
                            orderbook.bids.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
                        }
                    }
                }
            }
        }
        
        // 解析asks
        if let Some(ask_data) = first_item.get("asks").and_then(|v| v.as_array()) {
            for ask in ask_data.iter().take(5) {
                if let Some(ask_array) = ask.as_array() {
                    if ask_array.len() >= 2 {
                        let price: f64 = ask_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = ask_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        if price > 0.0 && qty > 0.0 {
                            use ordered_float::OrderedFloat;
                            use rust_decimal::Decimal;
                            orderbook.asks.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
                        }
                    }
                }
            }
        }
        
        orderbook.last_update = timestamp;
        
        // 標記為有效
        if !orderbook.bids.is_empty() && !orderbook.asks.is_empty() {
            orderbook.is_valid = true;
            orderbook.data_quality_score = 1.0;
        }
        
        Ok(orderbook)
    }
    
    /// 特徵提取和信號生成
    fn extract_features_and_generate_signals(&mut self, orderbook: &OrderBook) -> Result<()> {
        // 特徵提取
        match self.feature_extractor.extract_features(orderbook, 100, now_micros()) {
            Ok(features) => {
                {
                    let mut stats = self.stats.lock().unwrap();
                    stats.feature_extractions += 1;
                }
                
                // 添加到特徵歷史
                {
                    let mut history = self.feature_history.lock().unwrap();
                    history.push_back(features.clone());
                    if history.len() > 50 {
                        history.pop_front();
                    }
                }
                
                // 生成交易信號
                if let Some(signal) = self.generate_feature_based_signal(&features, orderbook) {
                    self.process_trading_signal(signal);
                }
            }
            Err(e) => {
                warn!("Feature extraction failed: {}", e);
            }
        }
        
        Ok(())
    }
    
    /// 基於特徵的信號生成
    fn generate_feature_based_signal(&self, features: &FeatureSet, orderbook: &OrderBook) -> Option<TradingSignal> {
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        let mid_price = (best_bid + best_ask) / 2.0;
        
        // 優化的交易規則 - 提高閾值降低交易頻率
        let obi_threshold = 0.05;      // OBI閾值 (從0.02提高至0.05)
        let momentum_threshold = 0.02; // 動量閾值 (從0.01提高至0.02)
        let spread_threshold = 10.0;   // 點差閾值 (從15.0降至10.0 bps)
        
        // 檢查點差是否合理
        if features.spread_bps > spread_threshold {
            return None; // 點差太大，不交易
        }
        
        // 多重信號組合策略
        let strong_obi = features.obi_l5.abs() > obi_threshold * 2.0;
        let _weak_obi = features.obi_l5.abs() > obi_threshold;
        let strong_momentum = features.price_momentum.abs() > momentum_threshold * 2.0;
        let _weak_momentum = features.price_momentum.abs() > momentum_threshold;
        
        // 基於OBI和動量的信號組合
        let buy_signal = (features.obi_l5 > obi_threshold && features.price_momentum > momentum_threshold) ||
                        (strong_obi && features.obi_l5 > 0.0) ||
                        (strong_momentum && features.price_momentum > 0.0);
        
        let sell_signal = (features.obi_l5 < -obi_threshold && features.price_momentum < -momentum_threshold) ||
                         (strong_obi && features.obi_l5 < 0.0) ||
                         (strong_momentum && features.price_momentum < 0.0);
        
        if buy_signal {
            // 改進的信心計算 - 考慮多重指標
            let obi_strength = (features.obi_l5 / 0.1).abs().min(1.0);
            let momentum_strength = (features.price_momentum / 0.05).abs().min(1.0);
            let spread_quality = (1.0 - features.spread_bps / 15.0).max(0.0);
            
            let confidence = (obi_strength * 0.4 + momentum_strength * 0.4 + spread_quality * 0.2).min(1.0);
            let position_size = self.calculate_position_size(confidence);
            
            Some(TradingSignal {
                timestamp: now_micros(),
                direction: TradeDirection::Buy,
                confidence,
                position_size,
                entry_price: mid_price,
                reasoning: format!("Buy: OBI={:.3}, Momentum={:.3}, Spread={:.1}bps, Conf={:.3}", 
                                 features.obi_l5, features.price_momentum, features.spread_bps, confidence),
            })
        } else if sell_signal {
            // 改進的信心計算 - 考慮多重指標
            let obi_strength = (features.obi_l5 / 0.1).abs().min(1.0);
            let momentum_strength = (features.price_momentum / 0.05).abs().min(1.0);
            let spread_quality = (1.0 - features.spread_bps / 15.0).max(0.0);
            
            let confidence = (obi_strength * 0.4 + momentum_strength * 0.4 + spread_quality * 0.2).min(1.0);
            let position_size = self.calculate_position_size(confidence);
            
            Some(TradingSignal {
                timestamp: now_micros(),
                direction: TradeDirection::Sell,
                confidence,
                position_size,
                entry_price: mid_price,
                reasoning: format!("Sell: OBI={:.3}, Momentum={:.3}, Spread={:.1}bps, Conf={:.3}", 
                                 features.obi_l5, features.price_momentum, features.spread_bps, confidence),
            })
        } else {
            None
        }
    }
    
    /// 計算倉位大小
    fn calculate_position_size(&self, confidence: f64) -> f64 {
        let stats = self.stats.lock().unwrap();
        let base_capital = 400.0; // 400 USDT
        let current_position_value = stats.current_position.abs();
        
        // 計算可用資金 (考慮已用資金)
        let available_capital = base_capital - current_position_value;
        let capital_utilization = current_position_value / base_capital;
        
        // 根據資金使用率調整風險
        let risk_adjustment = if capital_utilization < 0.1 {
            1.0  // 使用率低於10%，正常風險
        } else if capital_utilization < 0.2 {
            0.5  // 使用率10-20%，降低風險
        } else {
            0.2  // 使用率超過20%，大幅降低風險
        };
        
        let max_risk_per_trade = 0.02 * risk_adjustment; // 基礎2%，根據使用率調整
        let confidence_multiplier = 0.5 + confidence * 0.8; // 0.5 - 1.3倍數
        
        let position_size = available_capital * max_risk_per_trade * confidence_multiplier;
        let min_size = 3.0; // 最小交易金額
        let max_size = (available_capital * 0.05).min(12.0); // 最大不超過可用資金的5%或12 USDT
        
        position_size.max(min_size).min(max_size)
    }
    
    /// 檢查持倉限制
    fn check_position_limits(&self, signal_direction: &TradeDirection, position_size: f64) -> bool {
        let stats = self.stats.lock().unwrap();
        let current_position = stats.current_position;
        let max_position = 80.0; // 最大持倉 80 USDT (20% 資金)
        
        match signal_direction {
            TradeDirection::Buy => {
                // 檢查買入後是否會超過最大持倉
                if current_position + position_size > max_position {
                    return false;
                }
                // 如果已經有大量多頭持倉，限制繼續加倉
                if current_position > max_position * 0.6 { // 超過最大持倉的60%時限制
                    return false;
                }
            }
            TradeDirection::Sell => {
                // 檢查賣出後是否會超過最大空頭持倉
                if current_position - position_size < -max_position {
                    return false;
                }
                // 如果已經有大量空頭持倉，限制繼續加倉
                if current_position < -max_position * 0.6 { // 超過最大持倉的60%時限制
                    return false;
                }
            }
            TradeDirection::Close => {
                return true; // 平倉總是允許的
            }
        }
        
        true
    }
    
    /// 檢查止損/止盈和時間止損條件
    fn check_stop_conditions(&self, current_price: f64, current_time: u64) {
        let mut should_close = false;
        let mut close_reason = String::new();
        
        {
            let stats = self.stats.lock().unwrap();
            
            // 如果沒有持倉，直接返回
            if stats.current_position.abs() < 0.1 {
                return;
            }
            
            let _position_value = stats.current_position.abs();
            let entry_price = stats.entry_price;
            let position_duration = current_time - stats.position_start_time;
            
            // 時間止損：持倉超過5分鐘自動平倉
            if position_duration > 300_000_000 { // 5分鐘 = 300秒 = 300,000,000微秒
                should_close = true;
                close_reason = format!("Time stop: Position held for {:.1} minutes", position_duration as f64 / 60_000_000.0);
            }
            
            // 止損/止盈檢查
            if entry_price > 0.0 && current_price > 0.0 {
                let price_change_pct = if stats.current_position > 0.0 {
                    // 多頭持倉：當前價格相對於進場價格的變化
                    (current_price - entry_price) / entry_price * 100.0
                } else {
                    // 空頭持倉：進場價格相對於當前價格的變化
                    (entry_price - current_price) / entry_price * 100.0
                };
                
                // 止損：虧損超過2%
                if price_change_pct < -2.0 {
                    should_close = true;
                    close_reason = format!("Stop loss: {:.2}% loss", price_change_pct.abs());
                }
                
                // 止盈：盈利超過1.5%
                if price_change_pct > 1.5 {
                    should_close = true;
                    close_reason = format!("Take profit: {:.2}% gain", price_change_pct);
                }
            }
        }
        
        // 執行平倉
        if should_close {
            let stats = self.stats.lock().unwrap();
            let close_signal = TradingSignal {
                timestamp: current_time,
                direction: TradeDirection::Close,
                confidence: 1.0,
                position_size: stats.current_position.abs(),
                entry_price: current_price,
                reasoning: close_reason,
            };
            drop(stats); // 釋放鎖
            
            info!("🚨 Auto Close Position: {}", close_signal.reasoning);
            self.execute_single_signal(close_signal);
        }
    }
    
    /// 處理交易信號
    fn process_trading_signal(&self, signal: TradingSignal) {
        // 檢查冷卻期：避免頻繁交易
        {
            let last_trade = *self.last_trade_time.lock().unwrap();
            let cooldown_period = 10_000_000; // 10秒冷卻期
            
            if signal.timestamp - last_trade < cooldown_period {
                let mut stats = self.stats.lock().unwrap();
                stats.rejected_signals += 1;
                info!("🕐 Signal Rejected: Cooldown period ({:.1}s remaining)", 
                     (cooldown_period - (signal.timestamp - last_trade)) as f64 / 1_000_000.0);
                return;
            }
        }
        
        let mut actual_signals = Vec::new();
        
        {
            let mut stats = self.stats.lock().unwrap();
            let current_position = stats.current_position;
            
            match signal.direction {
                TradeDirection::Buy => {
                    // 如果有空頭持倉，先平倉
                    if current_position < -0.1 { // 小於-0.1 USDT視為空頭持倉
                        let close_signal = TradingSignal {
                            timestamp: signal.timestamp,
                            direction: TradeDirection::Close,
                            confidence: 1.0,
                            position_size: current_position.abs(),
                            entry_price: signal.entry_price,
                            reasoning: "Close short position before going long".to_string(),
                        };
                        actual_signals.push(close_signal);
                    }
                    
                    // 檢查持倉限制（在平倉後）
                    let new_position = if current_position < 0.0 { signal.position_size } else { current_position + signal.position_size };
                    if new_position <= 80.0 { // 最大持倉限制
                        actual_signals.push(signal.clone());
                    } else {
                        stats.rejected_signals += 1;
                        info!("🚫 Buy Signal Rejected: Size: {:.1} USDT | Would exceed position limit", signal.position_size);
                        return;
                    }
                }
                TradeDirection::Sell => {
                    // 如果有多頭持倉，先平倉
                    if current_position > 0.1 { // 大於0.1 USDT視為多頭持倉
                        let close_signal = TradingSignal {
                            timestamp: signal.timestamp,
                            direction: TradeDirection::Close,
                            confidence: 1.0,
                            position_size: current_position,
                            entry_price: signal.entry_price,
                            reasoning: "Close long position before going short".to_string(),
                        };
                        actual_signals.push(close_signal);
                    }
                    
                    // 檢查持倉限制（在平倉後）
                    let new_position = if current_position > 0.0 { -signal.position_size } else { current_position - signal.position_size };
                    if new_position >= -80.0 { // 最大空頭持倉限制
                        actual_signals.push(signal.clone());
                    } else {
                        stats.rejected_signals += 1;
                        info!("🚫 Sell Signal Rejected: Size: {:.1} USDT | Would exceed position limit", signal.position_size);
                        return;
                    }
                }
                TradeDirection::Close => {
                    actual_signals.push(signal.clone());
                }
            }
        }
        
        // 執行所有實際信號
        for actual_signal in actual_signals {
            let timestamp = actual_signal.timestamp; // 先保存時間戳
            self.execute_single_signal(actual_signal);
            
            // 更新最後交易時間
            *self.last_trade_time.lock().unwrap() = timestamp;
        }
    }
    
    /// 執行單一交易信號
    fn execute_single_signal(&self, signal: TradingSignal) {
        {
            let mut stats = self.stats.lock().unwrap();
            stats.trading_signals += 1;
            
            match signal.direction {
                TradeDirection::Buy => {
                    stats.buy_signals += 1;
                    stats.current_position += signal.position_size;
                    // 更新進場價格和時間
                    if stats.current_position > 0.1 {
                        stats.entry_price = signal.entry_price;
                        stats.position_start_time = signal.timestamp;
                    }
                }
                TradeDirection::Sell => {
                    stats.sell_signals += 1;
                    stats.current_position -= signal.position_size;
                    // 更新進場價格和時間
                    if stats.current_position < -0.1 {
                        stats.entry_price = signal.entry_price;
                        stats.position_start_time = signal.timestamp;
                    }
                }
                TradeDirection::Close => {
                    stats.closed_positions += 1;
                    // 計算P&L
                    if stats.current_position.abs() > 0.1 {
                        let pnl = if stats.current_position > 0.0 {
                            // 平多頭：(current_price - entry_price) * position
                            (signal.entry_price - stats.entry_price) * stats.current_position / stats.entry_price
                        } else {
                            // 平空頭：(entry_price - current_price) * position
                            (stats.entry_price - signal.entry_price) * stats.current_position.abs() / stats.entry_price
                        };
                        stats.total_pnl += pnl;
                    }
                    stats.current_position = 0.0;
                    stats.entry_price = 0.0;
                    stats.position_start_time = 0;
                }
            }
        }
        
        // 記錄信號
        info!("🎯 Feature Signal: {:?} | Confidence: {:.3} | Size: {:.1} USDT | Price: {:.2}", 
             signal.direction, signal.confidence, signal.position_size, signal.entry_price);
        info!("   {}", signal.reasoning);
        
        // 模擬執行
        self.simulate_execution(&signal);
    }
    
    /// 模擬交易執行
    fn simulate_execution(&self, signal: &TradingSignal) {
        let fee = signal.position_size * 0.0004; // 0.04% 手續費
        
        info!("📈 Simulated Execution:");
        info!("   Direction: {:?}", signal.direction);
        info!("   Size: {:.2} USDT", signal.position_size);
        info!("   Price: {:.2}", signal.entry_price);
        info!("   Fee: {:.4} USDT", fee);
        
        let stats = self.stats.lock().unwrap();
        info!("   Current Position: {:.2} USDT", stats.current_position);
    }
    
    /// 獲取統計信息
    pub fn get_stats(&self) -> TradingStats {
        self.stats.lock().unwrap().clone()
    }
}

impl Clone for TradingStats {
    fn clone(&self) -> Self {
        Self {
            total_updates: self.total_updates,
            feature_extractions: self.feature_extractions,
            trading_signals: self.trading_signals,
            buy_signals: self.buy_signals,
            sell_signals: self.sell_signals,
            last_price: self.last_price,
            spread_bps: self.spread_bps,
            current_position: self.current_position,
            total_pnl: self.total_pnl,
            win_rate: self.win_rate,
            rejected_signals: self.rejected_signals,
            closed_positions: self.closed_positions,
            entry_price: self.entry_price,
            position_start_time: self.position_start_time,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting Feature-Based Trading Demo");

    // 創建交易系統
    let mut trading_system = FeatureBasedTradingSystem::new()?;
    info!("✅ Trading system initialized");

    // 創建Bitget配置
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    // 創建連接器
    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    info!("📊 Subscribed to BTCUSDT books5 channel");

    // 統計計數器
    let stats_counter = Arc::new(Mutex::new(0u64));
    let start_time = std::time::Instant::now();

    // 系統引用
    let system_arc = Arc::new(Mutex::new(trading_system));
    let system_clone = system_arc.clone();
    let stats_clone = stats_counter.clone();

    // 創建消息處理器
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                // 處理LOB數據
                if let Ok(mut system) = system_clone.lock() {
                    if let Err(e) = system.process_bitget_orderbook(&symbol, &data, timestamp) {
                        warn!("Trading system processing error: {}", e);
                    }
                }

                // 更新統計
                {
                    let mut count = stats_clone.lock().unwrap();
                    *count += 1;

                    // 每25次更新顯示統計信息
                    if *count % 25 == 0 {
                        let elapsed = start_time.elapsed();
                        let rate = *count as f64 / elapsed.as_secs_f64();
                        
                        if let Ok(system) = system_clone.lock() {
                            let stats = system.get_stats();
                            info!("📊 Updates: {}, Rate: {:.1}/s, Price: {:.2}, Spread: {:.1}bps", 
                                 *count, rate, stats.last_price, stats.spread_bps);
                            let position_status = if stats.current_position.abs() < 0.1 {
                                "Flat".to_string()
                            } else if stats.current_position > 0.0 {
                                format!("Long {:.1}", stats.current_position)
                            } else {
                                format!("Short {:.1}", stats.current_position.abs())
                            };
                            info!("   Features: {}, Signals: {} (Buy:{}, Sell:{}), Closed: {}, Rejected: {}, Position: {}", 
                                 stats.feature_extractions, stats.trading_signals,
                                 stats.buy_signals, stats.sell_signals, stats.closed_positions,
                                 stats.rejected_signals, position_status);
                        }
                    }
                }
            }
            _ => {}
        }
    };

    info!("🔌 Connecting to Bitget BTCUSDT feature-based trading...");

    // 設置運行時間限制 - 24小時數據收集
    let timeout = Duration::from_secs(24 * 60 * 60); // 24小時運行
    
    info!("🕐 System will run for 24 hours to collect training data");
    info!("💡 Press Ctrl+C to stop early if needed");

    // 啟動連接
    match tokio::time::timeout(timeout, connector.connect_public(message_handler)).await {
        Ok(Ok(_)) => {
            info!("✅ Feature-based trading completed successfully");
        }
        Ok(Err(e)) => {
            warn!("❌ Connection failed: {}", e);
            return Err(e);
        }
        Err(_) => {
            info!("⏰ Demo completed after 1 minute");
        }
    }

    // 最終統計
    if let Ok(system) = system_arc.lock() {
        let stats = system.get_stats();
        let final_count = *stats_counter.lock().unwrap();
        let elapsed = start_time.elapsed();
        let rate = final_count as f64 / elapsed.as_secs_f64();

        info!("🏁 Final Trading Statistics:");
        info!("   Total LOB updates: {}", final_count);
        info!("   Average rate: {:.1} updates/sec", rate);
        info!("   Feature extractions: {}", stats.feature_extractions);
        info!("   Trading signals: {} (Buy:{}, Sell:{}), Closed: {}, Rejected: {}", 
             stats.trading_signals, stats.buy_signals, stats.sell_signals, stats.closed_positions, stats.rejected_signals);
        info!("   Final P&L: {:.2} USDT", stats.total_pnl);
        info!("   Signal rate: {:.1}%", 
             stats.trading_signals as f64 / stats.feature_extractions.max(1) as f64 * 100.0);
        info!("   Final position: {:.2} USDT", stats.current_position);
        info!("   Last price: {:.2} USDT", stats.last_price);
    }

    Ok(())
}