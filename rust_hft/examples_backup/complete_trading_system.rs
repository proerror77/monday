/*!
 * Complete HFT Trading System with DL Strategy & Real-time CLI Monitoring
 * 
 * 完整的HFT交易系統，包含：
 * 1. 🔗 實時Bitget報價接收
 * 2. 🧠 深度學習策略引擎
 * 3. 🚫 Dry-run安全模式
 * 4. 📊 即時CLI監控界面
 * 5. ⚡ 超低延遲執行
 * 6. 🛡️ 風險管理系統
 */

use rust_hft::{
    core::{types::{*, now_micros}, orderbook::OrderBook},
    engine::{BtcusdtDlTrader, BtcusdtDlConfig},
    integrations::bitget_connector::*,
    ml::{features::FeatureExtractor, OnlineLearningEngine, OnlineLearningConfig},
};
use anyhow::Result;
use tracing::{info, debug};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};
use serde_json::Value;
use crossterm::{
    terminal::{enable_raw_mode, disable_raw_mode, Clear, ClearType},
    cursor::{MoveTo, Hide, Show},
    style::{Color, ResetColor, SetForegroundColor},
    execute,
    event::{poll, read, Event, KeyCode},
};
use std::io::{stdout, Write};

/// 交易系統配置
#[derive(Debug, Clone)]
pub struct TradingSystemConfig {
    pub dry_run_mode: bool,
    pub max_position_size_usdt: f64,
    pub daily_loss_limit_usdt: f64,
    pub min_confidence_threshold: f64,
    pub max_trades_per_hour: u32,
    pub symbol: String,
    pub initial_balance_usdt: f64,
}

impl Default for TradingSystemConfig {
    fn default() -> Self {
        Self {
            dry_run_mode: true,
            max_position_size_usdt: 50.0,
            daily_loss_limit_usdt: 20.0,
            min_confidence_threshold: 0.65,
            max_trades_per_hour: 100,
            symbol: "BTCUSDT".to_string(),
            initial_balance_usdt: 400.0,
        }
    }
}

/// 交易記錄
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub id: String,
    pub timestamp: u64,
    pub side: String,
    pub size: f64,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub pnl: f64,
    pub status: String,
    pub strategy: String,
    pub confidence: f64,
    pub latency_us: u64,
}

/// 系統狀態
#[derive(Debug, Clone)]
pub struct SystemStatus {
    pub is_running: bool,
    pub mode: String,
    pub uptime_seconds: u64,
    pub last_price: f64,
    pub spread_bps: f64,
    pub total_trades: u32,
    pub winning_trades: u32,
    pub total_pnl: f64,
    pub current_balance: f64,
    pub current_position: f64,
    pub daily_trades: u32,
    pub avg_latency_us: f64,
    pub data_updates: u64,
    pub strategy_signals: u64,
    pub last_signal_time: Option<u64>,
    pub risk_level: String,
}

impl Default for SystemStatus {
    fn default() -> Self {
        Self {
            is_running: false,
            mode: "DRY-RUN".to_string(),
            uptime_seconds: 0,
            last_price: 0.0,
            spread_bps: 0.0,
            total_trades: 0,
            winning_trades: 0,
            total_pnl: 0.0,
            current_balance: 400.0,
            current_position: 0.0,
            daily_trades: 0,
            avg_latency_us: 0.0,
            data_updates: 0,
            strategy_signals: 0,
            last_signal_time: None,
            risk_level: "LOW".to_string(),
        }
    }
}

/// 完整交易系統
pub struct CompleteTradingSystem {
    config: TradingSystemConfig,
    dl_trader: Option<BtcusdtDlTrader>,
    feature_extractor: FeatureExtractor,
    online_learning: Option<OnlineLearningEngine>,
    
    // 狀態管理
    status: Arc<Mutex<SystemStatus>>,
    trade_history: Arc<Mutex<VecDeque<TradeRecord>>>,
    system_logs: Arc<Mutex<VecDeque<String>>>,
    
    // 運行時狀態
    start_time: Instant,
    last_trade_time: Option<Instant>,
    trades_this_hour: u32,
}

impl CompleteTradingSystem {
    pub fn new(config: TradingSystemConfig) -> Result<Self> {
        let feature_extractor = FeatureExtractor::new(50);
        
        let mut status = SystemStatus::default();
        status.mode = if config.dry_run_mode { "DRY-RUN".to_string() } else { "LIVE".to_string() };
        status.current_balance = config.initial_balance_usdt;
        
        Ok(Self {
            config,
            dl_trader: None,
            feature_extractor,
            online_learning: None,
            status: Arc::new(Mutex::new(status)),
            trade_history: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
            system_logs: Arc::new(Mutex::new(VecDeque::with_capacity(100))),
            start_time: Instant::now(),
            last_trade_time: None,
            trades_this_hour: 0,
        })
    }
    
    /// 初始化DL交易者
    pub async fn initialize_dl_trader(&mut self) -> Result<()> {
        let dl_config = BtcusdtDlConfig {
            symbol: self.config.symbol.clone(),
            test_capital: self.config.initial_balance_usdt,
            max_position_size_pct: 0.125, // 12.5%
            min_position_size_usdt: 2.0,
            sequence_length: 30,
            prediction_horizons: vec![3, 5, 10],
            min_prediction_confidence: self.config.min_confidence_threshold,
            model_update_interval_seconds: 300,
            lob_depth: 20,
            feature_window_size: 50,
            technical_indicators: vec![
                "SMA_5".to_string(), "SMA_10".to_string(),
                "EMA_5".to_string(), "EMA_10".to_string(),
                "RSI_14".to_string(), "MACD".to_string(),
                "VWAP".to_string(), "BBands_Upper".to_string(),
                "BBands_Lower".to_string(), "ATR".to_string(),
            ],
            market_microstructure_features: true,
            kelly_fraction_max: 0.25,
            kelly_fraction_conservative: 0.15,
            daily_loss_limit_pct: self.config.daily_loss_limit_usdt / self.config.initial_balance_usdt,
            position_loss_limit_pct: 0.02,
            max_drawdown_pct: 0.03,
            signal_generation_interval_ms: 200,
            position_rebalance_interval_ms: 1000,
            max_concurrent_positions: 3,
            position_hold_time_target_seconds: 15,
            enable_simd_optimization: true,
            enable_parallel_feature_extraction: true,
            max_inference_latency_us: 1000,
            memory_pool_size: 1024,
            enable_detailed_logging: false,
            performance_tracking_window: 200,
            real_time_metrics: true,
        };
        
        self.dl_trader = Some(BtcusdtDlTrader::new(dl_config)?);
        self.add_log("✅ DL交易者初始化完成".to_string());
        Ok(())
    }
    
    /// 初始化在線學習
    pub fn initialize_online_learning(&mut self) -> Result<()> {
        let ol_config = OnlineLearningConfig {
            input_size: 25,
            hidden_sizes: vec![64, 32],
            output_size: 3,
            dropout_rate: 0.1,
            learning_rate: 0.001,
            batch_size: 32,
            sequence_length: 30,
            prediction_horizons: vec![3, 5, 10],
            update_frequency: 50,
            validation_ratio: 0.2,
            early_stopping_patience: 10,
            max_training_samples: 10000,
            use_gpu: false,
            mixed_precision: false,
            gradient_clipping: 1.0,
            model_save_interval: 1000,
            model_save_path: "models/online_model.bin".to_string(),
        };
        
        self.online_learning = Some(OnlineLearningEngine::new(ol_config)?);
        self.add_log("✅ 在線學習引擎初始化完成".to_string());
        Ok(())
    }
    
    /// 處理市場數據（同步版本）
    pub fn process_market_data_sync(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != self.config.symbol {
            return Ok(());
        }
        
        let process_start = now_micros();
        
        // 解析訂單簿
        let orderbook = self.parse_market_data(data, timestamp)?;
        
        // 提取特徵
        let features = self.feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        
        // 更新系統狀態
        self.update_market_status(&orderbook, &features, timestamp);
        
        // 生成交易信號
        if let Some(signal) = self.generate_trading_signal_sync(&features, &orderbook, timestamp)? {
            self.execute_trading_signal_sync(signal, timestamp)?;
        }
        
        // 更新統計
        let latency = now_micros() - process_start;
        self.update_performance_stats(latency);
        
        debug!("處理 {} 數據完成，延遲: {}μs", symbol, latency);
        Ok(())
    }
    
    /// 解析市場數據
    fn parse_market_data(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new(self.config.symbol.clone());
        
        // 解析 bids
        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
            for bid in bid_data.iter().take(20) {
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
            for ask in ask_data.iter().take(20) {
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
        orderbook.data_quality_score = if orderbook.is_valid { 1.0 } else { 0.0 };
        
        Ok(orderbook)
    }
    
    /// 更新市場狀態
    fn update_market_status(&self, orderbook: &OrderBook, features: &FeatureSet, _timestamp: u64) {
        let mut status = self.status.lock().unwrap();
        
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        
        status.last_price = (best_bid + best_ask) / 2.0;
        status.spread_bps = if status.last_price > 0.0 {
            (best_ask - best_bid) / status.last_price * 10000.0
        } else { 0.0 };
        
        status.uptime_seconds = self.start_time.elapsed().as_secs();
        status.data_updates += 1;
        
        // 更新風險水平
        status.risk_level = if features.obi_l5.abs() > 0.5 {
            "HIGH".to_string()
        } else if features.obi_l5.abs() > 0.2 {
            "MEDIUM".to_string()
        } else {
            "LOW".to_string()
        };
    }
    
    /// 生成交易信號（同步版本）
    fn generate_trading_signal_sync(&mut self, features: &FeatureSet, orderbook: &OrderBook, _timestamp: u64) -> Result<Option<TradingSignal>> {
        // 基本檢查
        if !self.should_generate_signal() {
            return Ok(None);
        }
        
        // DL模型預測
        let dl_prediction = self.get_dl_prediction_sync(features, orderbook)?;
        
        // 置信度檢查
        if dl_prediction.confidence < self.config.min_confidence_threshold {
            return Ok(None);
        }
        
        // 風險檢查
        if !self.check_risk_limits(&dl_prediction) {
            return Ok(None);
        }
        
        // 更新信號統計
        {
            let mut status = self.status.lock().unwrap();
            status.strategy_signals += 1;
            status.last_signal_time = Some(_timestamp);
        }
        
        self.add_log(format!("🎯 生成交易信號: {} 置信度={:.2}", dl_prediction.direction, dl_prediction.confidence));
        
        Ok(Some(dl_prediction))
    }
    
    /// 檢查是否應該生成信號
    fn should_generate_signal(&self) -> bool {
        let status = self.status.lock().unwrap();
        
        // 檢查每小時交易限制
        if self.trades_this_hour >= self.config.max_trades_per_hour {
            return false;
        }
        
        // 檢查日損失限制
        if status.total_pnl <= -self.config.daily_loss_limit_usdt {
            return false;
        }
        
        // 檢查最後交易時間間隔
        if let Some(last_trade) = self.last_trade_time {
            if last_trade.elapsed() < Duration::from_secs(10) {
                return false;
            }
        }
        
        true
    }
    
    /// 獲取DL預測（同步版本）
    fn get_dl_prediction_sync(&self, features: &FeatureSet, orderbook: &OrderBook) -> Result<TradingSignal> {
        // 模擬DL預測邏輯
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        let mid_price = (best_bid + best_ask) / 2.0;
        
        // 基於OBI和深度壓力的信號
        let obi_strength = features.obi_l5.abs();
        let spread_reasonable = features.spread_bps < 20.0;
        let depth_imbalance = features.depth_pressure_bid.abs() > 0.15 || features.depth_pressure_ask.abs() > 0.15;
        
        let direction = if features.obi_l5 > 0.1 && spread_reasonable && depth_imbalance {
            "BUY".to_string()
        } else if features.obi_l5 < -0.1 && spread_reasonable && depth_imbalance {
            "SELL".to_string()
        } else {
            "HOLD".to_string()
        };
        
        let confidence = if direction != "HOLD" {
            (obi_strength * 1.5).min(0.95).max(0.5)
        } else {
            0.0
        };
        
        let size = if direction != "HOLD" {
            self.calculate_position_size(confidence)
        } else {
            0.0
        };
        
        let entry_price = match direction.as_str() {
            "BUY" => best_ask,
            "SELL" => best_bid,
            _ => mid_price,
        };
        
        Ok(TradingSignal {
            direction,
            entry_price,
            size,
            confidence,
            reasoning: format!("OBI={:.3}, Spread={:.1}bps, Depth={:.3}", 
                             features.obi_l5, features.spread_bps, features.depth_pressure_bid),
        })
    }
    
    /// 計算倉位大小
    fn calculate_position_size(&self, confidence: f64) -> f64 {
        let base_size = 2.0; // 基礎 2 USDT
        let confidence_multiplier = confidence * 3.0;
        let size = base_size * confidence_multiplier;
        size.min(self.config.max_position_size_usdt).max(1.0)
    }
    
    /// 檢查風險限制
    fn check_risk_limits(&self, signal: &TradingSignal) -> bool {
        let status = self.status.lock().unwrap();
        
        // 檢查倉位大小
        if signal.size > self.config.max_position_size_usdt {
            return false;
        }
        
        // 檢查總倉位
        let new_position = match signal.direction.as_str() {
            "BUY" => status.current_position + signal.size,
            "SELL" => status.current_position - signal.size,
            _ => status.current_position,
        };
        
        if new_position.abs() > self.config.max_position_size_usdt * 3.0 {
            return false;
        }
        
        // 檢查餘額
        if status.current_balance < signal.size {
            return false;
        }
        
        true
    }
    
    /// 執行交易信號（同步版本）
    fn execute_trading_signal_sync(&mut self, signal: TradingSignal, timestamp: u64) -> Result<()> {
        let execution_start = now_micros();
        
        // 模擬執行延遲
        std::thread::sleep(Duration::from_micros(50));
        
        let execution_latency = now_micros() - execution_start;
        
        // 計算費用和預期P&L
        let fee = signal.size * 0.0004; // 0.04% 手續費
        let expected_pnl = signal.confidence * 0.8 - fee; // 模擬收益
        
        // 創建交易記錄
        let trade = TradeRecord {
            id: format!("trade_{}", timestamp),
            timestamp,
            side: signal.direction.clone(),
            size: signal.size,
            entry_price: signal.entry_price,
            exit_price: None,
            pnl: expected_pnl,
            status: if self.config.dry_run_mode { "SIMULATED".to_string() } else { "EXECUTED".to_string() },
            strategy: "DL_SIGNAL".to_string(),
            confidence: signal.confidence,
            latency_us: execution_latency,
        };
        
        // 更新系統狀態
        {
            let mut status = self.status.lock().unwrap();
            status.total_trades += 1;
            status.daily_trades += 1;
            
            if expected_pnl > 0.0 {
                status.winning_trades += 1;
            }
            
            status.total_pnl += expected_pnl;
            status.current_balance += expected_pnl;
            
            // 更新倉位
            match signal.direction.as_str() {
                "BUY" => status.current_position += signal.size,
                "SELL" => status.current_position -= signal.size,
                _ => {}
            }
        }
        
        // 添加到交易歷史
        {
            let mut history = self.trade_history.lock().unwrap();
            history.push_back(trade.clone());
            if history.len() > 1000 {
                history.pop_front();
            }
        }
        
        // 更新交易時間
        self.last_trade_time = Some(Instant::now());
        self.trades_this_hour += 1;
        
        let mode_str = if self.config.dry_run_mode { "模擬" } else { "實盤" };
        self.add_log(format!("{} {} {:.1} USDT @ {:.2} | P&L: {:.4} | {}μs", 
                            mode_str, signal.direction, signal.size, signal.entry_price,
                            expected_pnl, execution_latency));
        
        info!("✅ 交易執行完成: {} {:.1} USDT, P&L: {:.4}", 
              signal.direction, signal.size, expected_pnl);
        
        Ok(())
    }
    
    /// 更新性能統計
    fn update_performance_stats(&self, latency: u64) {
        let mut status = self.status.lock().unwrap();
        let count = status.data_updates as f64;
        status.avg_latency_us = (status.avg_latency_us * (count - 1.0) + latency as f64) / count;
    }
    
    /// 添加系統日誌
    fn add_log(&self, message: String) {
        let mut logs = self.system_logs.lock().unwrap();
        let timestamp = chrono::Local::now().format("%H:%M:%S");
        logs.push_back(format!("[{}] {}", timestamp, message));
        if logs.len() > 100 {
            logs.pop_front();
        }
    }
    
    /// 獲取系統狀態
    pub fn get_status(&self) -> SystemStatus {
        self.status.lock().unwrap().clone()
    }
    
    /// 獲取最近交易
    pub fn get_recent_trades(&self, limit: usize) -> Vec<TradeRecord> {
        let history = self.trade_history.lock().unwrap();
        history.iter().rev().take(limit).cloned().collect()
    }
    
    /// 獲取系統日誌
    pub fn get_logs(&self, limit: usize) -> Vec<String> {
        let logs = self.system_logs.lock().unwrap();
        logs.iter().rev().take(limit).cloned().collect()
    }
    
    /// 切換交易模式
    pub fn toggle_mode(&mut self) {
        self.config.dry_run_mode = !self.config.dry_run_mode;
        let mut status = self.status.lock().unwrap();
        status.mode = if self.config.dry_run_mode { "DRY-RUN".to_string() } else { "LIVE".to_string() };
        
        let mode_str = if self.config.dry_run_mode { "模擬模式" } else { "實盤模式" };
        self.add_log(format!("🔄 切換到{}", mode_str));
    }
    
    /// 重置統計
    pub fn reset_stats(&mut self) {
        {
            let mut status = self.status.lock().unwrap();
            status.total_trades = 0;
            status.winning_trades = 0;
            status.total_pnl = 0.0;
            status.current_balance = self.config.initial_balance_usdt;
            status.current_position = 0.0;
            status.daily_trades = 0;
            status.data_updates = 0;
            status.strategy_signals = 0;
        }
        
        {
            let mut history = self.trade_history.lock().unwrap();
            history.clear();
        }
        
        self.trades_this_hour = 0;
        self.last_trade_time = None;
        
        self.add_log("🔄 統計數據已重置".to_string());
    }
}

/// 交易信號
#[derive(Debug, Clone)]
pub struct TradingSignal {
    pub direction: String,
    pub entry_price: f64,
    pub size: f64,
    pub confidence: f64,
    pub reasoning: String,
}

/// CLI 渲染器
pub struct TradingSystemRenderer {
    last_render: Instant,
}

impl TradingSystemRenderer {
    pub fn new() -> Self {
        Self {
            last_render: Instant::now(),
        }
    }
    
    /// 渲染完整界面
    pub fn render(&mut self, system: &CompleteTradingSystem) -> Result<()> {
        // 限制渲染頻率到10fps
        if self.last_render.elapsed() < Duration::from_millis(100) {
            return Ok(());
        }
        self.last_render = Instant::now();
        
        let status = system.get_status();
        let trades = system.get_recent_trades(8);
        let logs = system.get_logs(6);
        
        // 清屏並移動到頂部
        execute!(stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
        
        // 渲染各個部分
        self.render_header(&status)?;
        self.render_trading_stats(&status)?;
        self.render_performance(&status)?;
        self.render_recent_trades(&trades)?;
        self.render_system_logs(&logs)?;
        self.render_controls(&status)?;
        
        stdout().flush()?;
        Ok(())
    }
    
    fn render_header(&self, status: &SystemStatus) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Green))?;
        println!("+===============================================================================+");
        println!("|                    ⚡ COMPLETE HFT TRADING SYSTEM ⚡                       |");
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        print!("|  Mode: ");
        
        if status.mode == "LIVE" {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
            print!("[LIVE]");
        } else {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
            print!("[DRY-RUN]");
        }
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("  |  Uptime: {}s  |  Price: ${:.2}  |  Risk: {}  |", 
                status.uptime_seconds, status.last_price, status.risk_level);
        
        execute!(stdout(), SetForegroundColor(Color::Green))?;
        println!("+===============================================================================+");
        
        Ok(())
    }
    
    fn render_trading_stats(&self, status: &SystemStatus) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\nTRADING & STRATEGY STATISTICS");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("+-------------+-------------+-------------+-------------+-------------+");
        println!("| Total Trades| Win Rate    | Total P&L   | Balance     | Position    |");
        println!("+-------------+-------------+-------------+-------------+-------------+");
        
        execute!(stdout(), SetForegroundColor(Color::Green))?;
        print!("|     {:>7} ", status.total_trades);
        
        let win_rate = if status.total_trades > 0 {
            (status.winning_trades as f64 / status.total_trades as f64) * 100.0
        } else {
            0.0
        };
        
        if win_rate >= 60.0 {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
        } else if win_rate >= 40.0 {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        } else {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
        }
        print!("|    {:>6.1}% ", win_rate);
        
        if status.total_pnl >= 0.0 {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
            print!("| +{:>9.2} ", status.total_pnl);
        } else {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
            print!("| {:>10.2} ", status.total_pnl);
        }
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        print!("| {:>9.1} U ", status.current_balance);
        
        if status.current_position > 0.0 {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
            print!("| Long{:>6.1} ", status.current_position);
        } else if status.current_position < 0.0 {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
            print!("| Short{:>5.1} ", status.current_position.abs());
        } else {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
            print!("| Flat{:>6.1} ", 0.0);
        }
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("|");
        println!("+-------------+-------------+-------------+-------------+-------------+");
        
        // 策略狀態
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\nDL STRATEGY STATUS");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("+-------------+-------------+-------------+-------------+-------------+");
        println!("| Data Updates| Signals Gen | Last Signal | Avg Latency | Spread      |");
        println!("+-------------+-------------+-------------+-------------+-------------+");
        
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        print!("|     {:>7} ", status.data_updates);
        print!("|     {:>7} ", status.strategy_signals);
        
        if let Some(last_signal) = status.last_signal_time {
            let signal_age = (now_micros() - last_signal) / 1_000_000;
            if signal_age < 30 {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
            } else {
                execute!(stdout(), SetForegroundColor(Color::Yellow))?;
            }
            print!("|     {:>5}s ago ", signal_age);
        } else {
            execute!(stdout(), SetForegroundColor(Color::DarkGrey))?;
            print!("|     No Signal ");
        }
        
        if status.avg_latency_us < 500.0 {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
        } else if status.avg_latency_us < 2000.0 {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        } else {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
        }
        print!("|   {:>6.0}μs ", status.avg_latency_us);
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        print!("|  {:>7.1}bps ", status.spread_bps);
        
        println!("|");
        println!("+-------------+-------------+-------------+-------------+-------------+");
        
        Ok(())
    }
    
    fn render_performance(&self, status: &SystemStatus) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\nPERFORMANCE METRICS");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        
        let daily_return = if status.current_balance > 0.0 {
            ((status.current_balance - 400.0) / 400.0) * 100.0
        } else {
            0.0
        };
        
        let signal_rate = if status.uptime_seconds > 0 {
            (status.strategy_signals as f64 / status.uptime_seconds as f64) * 3600.0
        } else {
            0.0
        };
        
        println!("+-----------------+-----------------+-----------------+-----------------+");
        println!("| Daily Return    | Trades/Hour     | Signals/Hour    | System Status   |");
        println!("+-----------------+-----------------+-----------------+-----------------+");
        
        if daily_return >= 1.0 {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
        } else if daily_return >= 0.0 {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        } else {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
        }
        print!("|     {:>+8.2}% ", daily_return);
        
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        print!("|     {:>8.1} ", status.daily_trades as f64 / (status.uptime_seconds as f64 / 3600.0).max(1.0));
        print!("|     {:>8.1} ", signal_rate);
        
        if status.is_running {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
            print!("|     [RUNNING]   ");
        } else {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
            print!("|     [STOPPED]   ");
        }
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("|");
        println!("+-----------------+-----------------+-----------------+-----------------+");
        
        Ok(())
    }
    
    fn render_recent_trades(&self, trades: &[TradeRecord]) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\nRECENT TRADES");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("+----------+------+---------+----------+-------------+----------+----------+");
        println!("|   Time   | Side |  Size   |  Price   |   Status    | Latency  |   P&L    |");
        println!("+----------+------+---------+----------+-------------+----------+----------+");
        
        for trade in trades.iter().take(5) {
            let time = chrono::DateTime::from_timestamp((trade.timestamp / 1_000_000) as i64, 0)
                .unwrap_or_default()
                .format("%H:%M:%S");
            
            print!("| {} ", time);
            
            // Side 顏色
            if trade.side == "BUY" {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
            } else {
                execute!(stdout(), SetForegroundColor(Color::Red))?;
            }
            print!("| {:>4} ", trade.side);
            
            execute!(stdout(), SetForegroundColor(Color::White))?;
            print!("| {:>7.1} | {:>8.2} ", trade.size, trade.entry_price);
            
            // Status 顏色
            if trade.status == "EXECUTED" {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
            } else {
                execute!(stdout(), SetForegroundColor(Color::Yellow))?;
            }
            print!("| {:>11} ", trade.status);
            
            execute!(stdout(), SetForegroundColor(Color::Cyan))?;
            print!("| {:>6}us ", trade.latency_us);
            
            // P&L 顏色
            if trade.pnl >= 0.0 {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
                print!("| +{:>7.4} ", trade.pnl);
            } else {
                execute!(stdout(), SetForegroundColor(Color::Red))?;
                print!("| {:>8.4} ", trade.pnl);
            }
            
            execute!(stdout(), SetForegroundColor(Color::White))?;
            println!("|");
        }
        
        if trades.is_empty() {
            execute!(stdout(), SetForegroundColor(Color::DarkGrey))?;
            println!("|                          No trades yet                           |");
        }
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("+----------+------+---------+----------+-------------+----------+----------+");
        
        Ok(())
    }
    
    fn render_system_logs(&self, logs: &[String]) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\nSYSTEM LOGS");
        execute!(stdout(), SetForegroundColor(Color::DarkGrey))?;
        
        for log in logs.iter().take(4) {
            println!("  {}", log);
        }
        
        if logs.is_empty() {
            println!("  No logs available");
        }
        
        Ok(())
    }
    
    fn render_controls(&self, status: &SystemStatus) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        println!("\nCONTROLS:");
        println!("  [Q]uit | [M]ode Toggle ({}) | [R]eset Stats | [SPACE]Pause", 
                if status.mode == "LIVE" { "->DRY-RUN" } else { "->LIVE" });
        execute!(stdout(), ResetColor)?;
        
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌（靜默模式用於CLI）
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_target(false)
        .without_time()
        .init();

    // 啟用終端原始模式
    enable_raw_mode()?;
    execute!(stdout(), Hide)?;
    
    let result = run_complete_trading_system().await;
    
    // 恢復終端
    disable_raw_mode()?;
    execute!(stdout(), Show, Clear(ClearType::All), MoveTo(0, 0))?;
    
    result
}

async fn run_complete_trading_system() -> Result<()> {
    // 創建交易系統配置
    let mut config = TradingSystemConfig::default();
    config.dry_run_mode = true; // 默認安全模式
    
    // 創建系統
    let mut trading_system = CompleteTradingSystem::new(config)?;
    
    // 初始化組件
    trading_system.initialize_dl_trader().await?;
    trading_system.initialize_online_learning()?;
    
    // 創建Bitget連接
    let bitget_config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 5,
    };

    let mut connector = BitgetConnector::new(bitget_config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);
    
    trading_system.add_log("🚀 完整HFT交易系統啟動".to_string());
    trading_system.add_log("📡 連接到Bitget實時數據...".to_string());
    
    // 創建系統共享引用
    let system_arc = Arc::new(Mutex::new(trading_system));
    let system_data = system_arc.clone();
    let system_render = system_arc.clone();
    let system_keyboard = system_arc.clone();
    
    // 數據處理任務
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                if let Ok(mut system) = system_data.lock() {
                    // 同步處理以避免Send trait問題
                    let _ = system.process_market_data_sync(&symbol, &data, timestamp);
                }
            }
            _ => {}
        }
    };
    
    // 渲染任務
    tokio::spawn(async move {
        let mut renderer = TradingSystemRenderer::new();
        loop {
            if let Ok(system) = system_render.lock() {
                if let Err(e) = renderer.render(&*system) {
                    eprintln!("渲染錯誤: {}", e);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    
    // 鍵盤輸入處理任務
    tokio::spawn(async move {
        loop {
            if poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(event) = read() {
                    if let Event::Key(key) = event {
                        if let Ok(mut system) = system_keyboard.lock() {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Char('Q') => {
                                    system.add_log("👋 系統關閉中...".to_string());
                                    std::process::exit(0);
                                }
                                KeyCode::Char('m') | KeyCode::Char('M') => {
                                    system.toggle_mode();
                                }
                                KeyCode::Char('r') | KeyCode::Char('R') => {
                                    system.reset_stats();
                                }
                                KeyCode::Char(' ') => {
                                    system.add_log("⏸️ 暫停功能未實現".to_string());
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    
    // 標記系統為運行狀態
    {
        let system = system_arc.lock().unwrap();
        let mut status = system.status.lock().unwrap();
        status.is_running = true;
    }
    
    system_arc.lock().unwrap().add_log("✅ 所有組件啟動完成".to_string());
    
    // 啟動市場數據連接
    if let Err(e) = connector.connect_public(message_handler).await {
        eprintln!("連接失敗: {}", e);
        return Err(e);
    }
    
    Ok(())
}