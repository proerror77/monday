/*!
 * 完整交易系統架構 - Holistic Trading System
 * 
 * 這是一個完整的 HFT 交易系統，整合了：
 * - 實時市場數據接收 (Bitget WebSocket)
 * - 機器學習特徵提取和預測
 * - 多時間範圍交易策略
 * - 風險管理和倉位控制
 * - 性能監控和統計
 * - DRY RUN 模式 (不實際下單，但連接真實數據)
 * 
 * 執行方式：
 * cargo run --example trading_system_holistic -- --mode dry-run
 * cargo run --example trading_system_holistic -- --mode live
 */

use rust_hft::{
    integrations::bitget_connector::*,
    core::{types::*, orderbook::OrderBook},
    ml::{features::FeatureExtractor, online_learning::{OnlineLearningEngine, OnlineLearningConfig}},
    utils::{performance::*, feature_store::{FeatureStore, FeatureStoreConfig}},
};
use anyhow::Result;
use tracing::{info, warn, error, debug};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use std::collections::VecDeque;
use rust_decimal::prelude::ToPrimitive;
use clap::{Parser, ValueEnum};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "holistic_trading_system")]
#[command(about = "Complete HFT Trading System with ML and Risk Management")]
struct Args {
    #[arg(short, long, value_enum, default_value_t = TradingMode::DryRun)]
    mode: TradingMode,
    
    #[arg(short, long, default_value_t = String::from("BTCUSDT"))]
    symbol: String,
    
    #[arg(long, default_value_t = 1000.0)]
    capital: f64,
    
    #[arg(long, default_value_t = 0.1)]
    max_position_pct: f64,
}

#[derive(Clone, ValueEnum, Debug)]
enum TradingMode {
    DryRun,  // 連接真實數據但不下單
    Live,    // 真實交易
}

/// 完整交易系統核心
pub struct HolisticTradingSystem {
    // 配置
    mode: TradingMode,
    symbol: String,
    total_capital: f64,
    max_position_pct: f64,
    
    // 核心組件
    feature_extractor: FeatureExtractor,
    ml_engine: OnlineLearningEngine,
    
    // 數據管理
    orderbook_history: Arc<Mutex<VecDeque<OrderBook>>>,
    feature_store: FeatureStore,
    
    // 交易狀態
    current_position: f64,
    entry_price: f64,
    unrealized_pnl: f64,
    
    // 性能統計
    stats: Arc<Mutex<TradingSystemStats>>,
    
    // 風險管理
    risk_manager: RiskManager,
    
    // 延遲監控
    latency_tracker: LatencyTracker,
}

#[derive(Debug, Clone)]
pub struct TradingSystemStats {
    // 數據統計
    pub total_lob_updates: u64,
    pub features_extracted: u64,
    pub signals_generated: u64,
    
    // 交易統計
    pub orders_placed: u64,
    pub orders_filled: u64,
    pub total_trades: u64,
    
    // P&L 統計
    pub total_pnl: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub win_rate: f64,
    pub max_drawdown: f64,
    
    // 性能統計
    pub avg_prediction_latency_us: f64,
    pub avg_order_latency_us: f64,
    pub data_quality_score: f64,
    
    // 風險統計
    pub max_position_used: f64,
    pub risk_events: u64,
    pub stop_losses_triggered: u64,
}

#[derive(Debug)]
pub struct RiskManager {
    max_position_size: f64,
    stop_loss_pct: f64,
    take_profit_pct: f64,
    max_daily_loss: f64,
    daily_pnl: f64,
}

#[derive(Debug)]
pub struct LatencyTracker {
    prediction_latencies: VecDeque<u64>,
    order_latencies: VecDeque<u64>,
    max_samples: usize,
}

impl HolisticTradingSystem {
    pub fn new(args: Args) -> Result<Self> {
        info!("🚀 Initializing Holistic Trading System");
        info!("Mode: {:?}, Symbol: {}, Capital: ${:.2}", 
              args.mode, args.symbol, args.capital);
        
        // 初始化性能優化
        let _capabilities = detect_hardware_capabilities();
        info!("Hardware capabilities detected: {:?}", _capabilities);
        
        // 創建機器學習引擎
        let ml_engine = OnlineLearningEngine::new(OnlineLearningConfig::default())?;
        
        // 風險管理配置
        let risk_manager = RiskManager {
            max_position_size: args.capital * args.max_position_pct,
            stop_loss_pct: 2.0,      // 2% 止損
            take_profit_pct: 1.5,    // 1.5% 止盈
            max_daily_loss: args.capital * 0.05, // 最大日虧損 5%
            daily_pnl: 0.0,
        };
        
        Ok(Self {
            mode: args.mode,
            symbol: args.symbol,
            total_capital: args.capital,
            max_position_pct: args.max_position_pct,
            
            feature_extractor: FeatureExtractor::new(50),
            ml_engine,
            
            orderbook_history: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
            feature_store: FeatureStore::new(FeatureStoreConfig::default())?,
            
            current_position: 0.0,
            entry_price: 0.0,
            unrealized_pnl: 0.0,
            
            stats: Arc::new(Mutex::new(TradingSystemStats::default())),
            risk_manager,
            latency_tracker: LatencyTracker::new(),
        })
    }
    
    /// 啟動完整交易系統
    pub async fn run(&mut self) -> Result<()> {
        info!("🎯 Starting Holistic Trading System for {}", self.symbol);
        
        // 配置 Bitget 連接
        let config = BitgetConfig {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 5,
        };

        let mut connector = BitgetConnector::new(config);
        connector.add_subscription(self.symbol.clone(), BitgetChannel::Books5);
        
        // 啟動統計報告任務
        let stats_clone = self.stats.clone();
        let stats_task = tokio::spawn(async move {
            Self::statistics_reporter(stats_clone).await;
        });
        
        // 创建数据处理所需的组件副本
        let symbol_clone = self.symbol.clone();
        let stats_clone = self.stats.clone();
        let orderbook_history_clone = self.orderbook_history.clone();
        let mut feature_extractor = FeatureExtractor::new(50);
        let ml_engine = OnlineLearningEngine::new(OnlineLearningConfig::default())?;
        let mut feature_store = FeatureStore::new(FeatureStoreConfig::default())?;
        
        let message_handler = move |message: BitgetMessage| {
            let process_start = now_micros();
            
            match message {
                BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                    if let Err(e) = Self::process_market_data_static(
                        &symbol_clone,
                        &symbol, 
                        &data, 
                        timestamp, 
                        process_start,
                        &stats_clone,
                        &orderbook_history_clone,
                        &mut feature_extractor,
                        &ml_engine,
                        &mut feature_store
                    ) {
                        error!("Market data processing error: {}", e);
                    }
                },
                BitgetMessage::Trade { symbol, data, timestamp } => {
                    debug!("Received trade data for {}: {:?}", symbol, data);
                },
                _ => {}
            }
        };
        
        // 啟動 WebSocket 連接
        info!("🔌 Connecting to Bitget WebSocket...");
        tokio::select! {
            result = connector.connect_public(message_handler) => {
                if let Err(e) = result {
                    error!("WebSocket connection failed: {}", e);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("📊 Shutdown signal received, generating final report...");
                
                // 生成最終報告
                self.generate_final_report();
            }
        }
        
        stats_task.abort();
        Ok(())
    }
    
    /// 静态方法处理市场数据
    fn process_market_data_static(
        _system_symbol: &str,
        symbol: &str, 
        data: &Value, 
        timestamp: u64, 
        process_start: u64,
        stats: &Arc<Mutex<TradingSystemStats>>,
        orderbook_history: &Arc<Mutex<VecDeque<OrderBook>>>,
        feature_extractor: &mut FeatureExtractor,
        ml_engine: &OnlineLearningEngine,
        feature_store: &mut FeatureStore
    ) -> Result<()> {
        // 解析 OrderBook
        let orderbook = Self::parse_bitget_orderbook_static(data, timestamp, symbol)?;
        
        // 更新統計
        {
            let mut stats_lock = stats.lock().unwrap();
            stats_lock.total_lob_updates += 1;
        }
        
        // 添加到歷史記錄
        {
            let mut history = orderbook_history.lock().unwrap();
            history.push_back(orderbook.clone());
            if history.len() > 1000 {
                history.pop_front();
            }
        }
        
        // 特徵提取
        let feature_start = now_micros();
        let features = feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        let _feature_latency = now_micros() - feature_start;
        
        // 存儲特徵到 FeatureStore
        feature_store.store_features(&features)?;
        
        // 機器學習預測
        let prediction_start = now_micros();
        let prediction_vec = ml_engine.predict(&features)?;
        let _prediction = prediction_vec.first().cloned();
        let _prediction_latency = now_micros() - prediction_start;
        
        // 更新統計
        {
            let mut stats_lock = stats.lock().unwrap();
            stats_lock.features_extracted += 1;
        }
        
        let total_latency = now_micros() - process_start;
        if total_latency > 1000 { // 超過 1ms 警告
            warn!("High processing latency: {}μs", total_latency);
        }
        
        Ok(())
    }

    /// 處理市場數據的核心邏輯
    fn process_market_data(&mut self, symbol: &str, data: &Value, timestamp: u64, process_start: u64) -> Result<()> {
        // 解析 OrderBook
        let orderbook = self.parse_bitget_orderbook(data, timestamp)?;
        
        // 更新統計
        {
            let mut stats = self.stats.lock().unwrap();
            stats.total_lob_updates += 1;
        }
        
        // 添加到歷史記錄
        {
            let mut history = self.orderbook_history.lock().unwrap();
            history.push_back(orderbook.clone());
            if history.len() > 1000 {
                history.pop_front();
            }
        }
        
        // 特徵提取
        let feature_start = now_micros();
        let features = self.feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        let feature_latency = now_micros() - feature_start;
        
        // 存儲特徵到 FeatureStore
        self.feature_store.store_features(&features)?;
        
        // 機器學習預測
        let prediction_start = now_micros();
        let prediction_vec = self.ml_engine.predict(&features)?;
        let prediction = prediction_vec.first().cloned();
        let prediction_latency = now_micros() - prediction_start;
        
        // 記錄預測延遲
        self.latency_tracker.record_prediction_latency(prediction_latency);
        
        // 交易策略決策 (簡化版)
        let signal = self.generate_simple_signal(&features, &prediction)?;
        
        // 風險管理檢查
        if let Some(validated_signal) = self.risk_manager.validate_signal(&signal, self.current_position, &features) {
            // 執行交易邏輯
            self.execute_trading_decision(validated_signal, &features)?;
        }
        
        // 更新統計
        {
            let mut stats = self.stats.lock().unwrap();
            stats.features_extracted += 1;
            stats.avg_prediction_latency_us = self.latency_tracker.avg_prediction_latency();
            
            if signal.is_some() {
                stats.signals_generated += 1;
            }
        }
        
        let total_latency = now_micros() - process_start;
        if total_latency > 1000 { // 超過 1ms 警告
            warn!("High processing latency: {}μs", total_latency);
        }
        
        Ok(())
    }
    
    /// 執行交易決策
    fn execute_trading_decision(&mut self, signal: TradingSignal, features: &FeatureSet) -> Result<()> {
        let mid_price = *features.mid_price;
        
        match self.mode {
            TradingMode::DryRun => {
                // DRY RUN 模式：模擬執行但不下實際訂單
                info!("🎯 [DRY RUN] Trading Signal: {:?} at price {:.2}", 
                      signal.signal_type, mid_price);
                
                // 模擬倉位更新
                match signal.signal_type {
                    SignalType::Buy => {
                        if self.current_position <= 0.0 {
                            let position_size = signal.suggested_quantity.to_f64().unwrap_or(0.0);
                            self.current_position = position_size;
                            self.entry_price = mid_price;
                            info!("📈 [DRY RUN] Opened LONG position: {:.4} at {:.2}", 
                                  position_size, mid_price);
                        }
                    },
                    SignalType::Sell => {
                        if self.current_position >= 0.0 {
                            let position_size = signal.suggested_quantity.to_f64().unwrap_or(0.0);
                            self.current_position = -position_size;
                            self.entry_price = mid_price;
                            info!("📉 [DRY RUN] Opened SHORT position: {:.4} at {:.2}", 
                                  position_size, mid_price);
                        }
                    },
                    SignalType::Hold => {
                        if self.current_position != 0.0 {
                            let pnl = self.calculate_pnl(mid_price);
                            self.update_pnl_stats(pnl);
                            
                            info!("💰 [DRY RUN] Closed position. PnL: {:.2}", pnl);
                            self.current_position = 0.0;
                            self.entry_price = 0.0;
                        }
                    }
                }
            },
            TradingMode::Live => {
                // 實際交易模式 (暫時簡化，只記錄)
                warn!("⚡ [LIVE] Would execute real order: {:?}", signal);
                
                // 模擬下單延遲
                let order_latency = 500; // 模擬 500μs 下單延遲
                self.latency_tracker.record_order_latency(order_latency);
                
                // 更新統計
                let mut stats = self.stats.lock().unwrap();
                stats.orders_placed += 1;
                stats.avg_order_latency_us = self.latency_tracker.avg_order_latency();
            }
        }
        
        Ok(())
    }
    
    /// 生成簡單的交易信號
    fn generate_simple_signal(&self, features: &FeatureSet, prediction: &Option<f64>) -> Result<Option<TradingSignal>> {
        // 簡單的信號生成邏輯
        let confidence_threshold = 0.6;
        let position_size = 100.0; // 固定倉位大小
        
        if let Some(pred) = prediction {
            if pred > &confidence_threshold {
                // 買入信號
                let signal = TradingSignal {
                    signal_type: SignalType::Buy,
                    confidence: *pred,
                    suggested_price: features.mid_price,
                    suggested_quantity: rust_decimal::Decimal::from_f64_retain(position_size).unwrap_or_default(),
                    timestamp: now_micros(),
                    features_timestamp: features.timestamp,
                    signal_latency_us: 0,
                };
                return Ok(Some(signal));
            } else if pred < &(-confidence_threshold) {
                // 賣出信號
                let signal = TradingSignal {
                    signal_type: SignalType::Sell,
                    confidence: (*pred).abs(),
                    suggested_price: features.mid_price,
                    suggested_quantity: rust_decimal::Decimal::from_f64_retain(position_size).unwrap_or_default(),
                    timestamp: now_micros(),
                    features_timestamp: features.timestamp,
                    signal_latency_us: 0,
                };
                return Ok(Some(signal));
            }
        }
        
        // 如果條件不滿足，檢查是否需要平倉
        if self.current_position != 0.0 {
            // 檢查止損止盈條件
            let mid_price = *features.mid_price;
            let pnl_pct = if self.current_position > 0.0 {
                (mid_price - self.entry_price) / self.entry_price
            } else {
                (self.entry_price - mid_price) / self.entry_price
            };
            
            // 止損 2% 或止盈 1.5%
            if pnl_pct < -0.02 || pnl_pct > 0.015 {
                let signal = TradingSignal {
                    signal_type: SignalType::Hold, // 使用 Hold 作為平倉信號
                    confidence: 0.9,
                    suggested_price: features.mid_price,
                    suggested_quantity: rust_decimal::Decimal::from_f64_retain(self.current_position.abs() as f64).unwrap_or_default(),
                    timestamp: now_micros(),
                    features_timestamp: features.timestamp,
                    signal_latency_us: 0,
                };
                return Ok(Some(signal));
            }
        }
        
        Ok(None)
    }
    
    /// 静态方法解析 Bitget OrderBook 數據
    fn parse_bitget_orderbook_static(data: &Value, timestamp: u64, symbol: &str) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new(symbol.to_string());
        
        // 解析 bids
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
        
        // 解析 asks
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
        orderbook.is_valid = !orderbook.bids.is_empty() && !orderbook.asks.is_empty();
        
        Ok(orderbook)
    }

    /// 解析 Bitget OrderBook 數據
    fn parse_bitget_orderbook(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new(self.symbol.clone());
        
        // 解析 bids
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
        
        // 解析 asks
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
        orderbook.is_valid = !orderbook.bids.is_empty() && !orderbook.asks.is_empty();
        
        Ok(orderbook)
    }
    
    /// 計算 PnL
    fn calculate_pnl(&self, current_price: f64) -> f64 {
        if self.current_position == 0.0 || self.entry_price == 0.0 {
            return 0.0;
        }
        
        if self.current_position > 0.0 {
            // Long position
            (current_price - self.entry_price) * self.current_position
        } else {
            // Short position
            (self.entry_price - current_price) * self.current_position.abs()
        }
    }
    
    /// 更新 PnL 統計
    fn update_pnl_stats(&mut self, pnl: f64) {
        let mut stats = self.stats.lock().unwrap();
        stats.total_trades += 1;
        stats.realized_pnl += pnl;
        stats.total_pnl = stats.realized_pnl + self.unrealized_pnl;
        
        // 更新勝率
        if pnl > 0.0 {
            // 這裡需要更複雜的勝率計算邏輯
        }
    }
    
    /// 統計報告任務
    async fn statistics_reporter(stats: Arc<Mutex<TradingSystemStats>>) {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        
        loop {
            interval.tick().await;
            
            if let Ok(stats) = stats.lock() {
                info!("📊 === Trading System Statistics ===");
                info!("LOB Updates: {}, Features: {}, Signals: {}", 
                      stats.total_lob_updates, stats.features_extracted, stats.signals_generated);
                info!("Orders: {} placed, {} filled, {} total trades", 
                      stats.orders_placed, stats.orders_filled, stats.total_trades);
                info!("PnL: Total {:.2}, Realized {:.2}, Unrealized {:.2}", 
                      stats.total_pnl, stats.realized_pnl, stats.unrealized_pnl);
                info!("Latency: Prediction {:.1}μs, Order {:.1}μs", 
                      stats.avg_prediction_latency_us, stats.avg_order_latency_us);
                info!("=====================================");
            }
        }
    }
    
    /// 生成最終報告
    fn generate_final_report(&self) {
        info!("📋 === FINAL TRADING SYSTEM REPORT ===");
        
        if let Ok(stats) = self.stats.lock() {
            info!("🔢 Data Processing:");
            info!("   └─ LOB Updates Processed: {}", stats.total_lob_updates);
            info!("   └─ Features Extracted: {}", stats.features_extracted);
            info!("   └─ Trading Signals Generated: {}", stats.signals_generated);
            
            info!("⚡ Performance Metrics:");
            info!("   └─ Avg Prediction Latency: {:.1}μs", stats.avg_prediction_latency_us);
            info!("   └─ Avg Order Latency: {:.1}μs", stats.avg_order_latency_us);
            info!("   └─ Data Quality Score: {:.3}", stats.data_quality_score);
            
            info!("💰 Trading Results:");
            info!("   └─ Total Orders Placed: {}", stats.orders_placed);
            info!("   └─ Orders Filled: {}", stats.orders_filled);
            info!("   └─ Total Trades: {}", stats.total_trades);
            info!("   └─ Total PnL: ${:.2}", stats.total_pnl);
            info!("   └─ Realized PnL: ${:.2}", stats.realized_pnl);
            info!("   └─ Win Rate: {:.1}%", stats.win_rate * 100.0);
            
            info!("🛡️  Risk Management:");
            info!("   └─ Max Position Used: {:.1}%", stats.max_position_used * 100.0);
            info!("   └─ Risk Events: {}", stats.risk_events);
            info!("   └─ Stop Losses Triggered: {}", stats.stop_losses_triggered);
        }
        
        info!("====================================");
        
        match self.mode {
            TradingMode::DryRun => {
                info!("ℹ️  This was a DRY RUN - no real trades were executed");
            },
            TradingMode::Live => {
                warn!("⚠️  This was LIVE TRADING - real money was involved");
            }
        }
    }
}

// Implement Default and other helper structs
impl Default for TradingSystemStats {
    fn default() -> Self {
        Self {
            total_lob_updates: 0,
            features_extracted: 0,
            signals_generated: 0,
            orders_placed: 0,
            orders_filled: 0,
            total_trades: 0,
            total_pnl: 0.0,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            win_rate: 0.0,
            max_drawdown: 0.0,
            avg_prediction_latency_us: 0.0,
            avg_order_latency_us: 0.0,
            data_quality_score: 1.0,
            max_position_used: 0.0,
            risk_events: 0,
            stop_losses_triggered: 0,
        }
    }
}

impl RiskManager {
    fn validate_signal(&self, signal: &Option<TradingSignal>, current_position: f64, features: &FeatureSet) -> Option<TradingSignal> {
        if let Some(signal) = signal {
            // 倉位大小檢查
            if signal.suggested_quantity.to_f64().unwrap_or(0.0) > self.max_position_size {
                warn!("Signal rejected: position size too large");
                return None;
            }
            
            // 點差檢查
            if features.spread_bps > 20.0 {
                warn!("Signal rejected: spread too wide ({:.1} bps)", features.spread_bps);
                return None;
            }
            
            // 每日損失檢查
            if self.daily_pnl < -self.max_daily_loss {
                warn!("Signal rejected: daily loss limit reached");
                return None;
            }
            
            Some(signal.clone())
        } else {
            None
        }
    }
}

impl LatencyTracker {
    fn new() -> Self {
        Self {
            prediction_latencies: VecDeque::with_capacity(1000),
            order_latencies: VecDeque::with_capacity(1000),
            max_samples: 1000,
        }
    }
    
    fn record_prediction_latency(&mut self, latency: u64) {
        self.prediction_latencies.push_back(latency);
        if self.prediction_latencies.len() > self.max_samples {
            self.prediction_latencies.pop_front();
        }
    }
    
    fn record_order_latency(&mut self, latency: u64) {
        self.order_latencies.push_back(latency);
        if self.order_latencies.len() > self.max_samples {
            self.order_latencies.pop_front();
        }
    }
    
    fn avg_prediction_latency(&self) -> f64 {
        if self.prediction_latencies.is_empty() {
            0.0
        } else {
            self.prediction_latencies.iter().sum::<u64>() as f64 / self.prediction_latencies.len() as f64
        }
    }
    
    fn avg_order_latency(&self) -> f64 {
        if self.order_latencies.is_empty() {
            0.0
        } else {
            self.order_latencies.iter().sum::<u64>() as f64 / self.order_latencies.len() as f64
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    
    info!("🚀 Starting Holistic Trading System");
    info!("⚙️  Configuration:");
    info!("   Mode: {:?}", args.mode);
    info!("   Symbol: {}", args.symbol);
    info!("   Capital: ${:.2}", args.capital);
    info!("   Max Position: {:.1}%", args.max_position_pct * 100.0);
    
    let mut system = HolisticTradingSystem::new(args)?;
    system.run().await?;
    
    Ok(())
}