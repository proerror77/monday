/*!
 * HFT CLI Real-time Dashboard
 * 
 * 終端實時監控界面：
 * 1. 真實 LOB 數據流
 * 2. 實時交易執行狀態
 * 3. P&L 和風險監控
 * 4. 訂單成交記錄
 * 5. 性能延遲統計
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
    core::{types::*, orderbook::OrderBook},
    utils::performance::*,
};
use anyhow::Result;
use tracing::{info, warn};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};
use serde_json::Value;
use crossterm::{
    terminal::{enable_raw_mode, disable_raw_mode, Clear, ClearType},
    cursor::{MoveTo, Hide, Show},
    style::{Color, Print, ResetColor, SetForegroundColor},
    execute,
    event::{poll, read, Event, KeyCode},
};
use std::io::{stdout, Write};

/// CLI交易狀態
#[derive(Debug, Clone)]
pub struct CliTradingStatus {
    // 基礎狀態
    pub total_orders: u64,
    pub filled_orders: u64,
    pub pending_orders: u64,
    pub rejected_orders: u64,
    pub is_live_trading: bool,
    
    // P&L 狀態
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub total_fees: f64,
    pub current_balance: f64,
    pub win_rate: f64,
    
    // 風險指標
    pub current_position: f64,
    pub max_drawdown: f64,
    pub daily_volume: f64,
    
    // 性能指標
    pub avg_execution_latency_us: f64,
    pub orders_per_second: f64,
    pub signal_accuracy: f64,
    
    // 市場數據
    pub last_price: f64,
    pub spread_bps: f64,
    pub obi_l5: f64,
    pub volume_24h: f64,
    
    // 時間戳
    pub last_update: u64,
    pub uptime_seconds: u64,
}

impl Default for CliTradingStatus {
    fn default() -> Self {
        Self {
            total_orders: 0,
            filled_orders: 0,
            pending_orders: 0,
            rejected_orders: 0,
            is_live_trading: false,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            total_fees: 0.0,
            current_balance: 400.0,
            win_rate: 0.0,
            current_position: 0.0,
            max_drawdown: 0.0,
            daily_volume: 0.0,
            avg_execution_latency_us: 0.0,
            orders_per_second: 0.0,
            signal_accuracy: 0.0,
            last_price: 0.0,
            spread_bps: 0.0,
            obi_l5: 0.0,
            volume_24h: 0.0,
            last_update: now_micros(),
            uptime_seconds: 0,
        }
    }
}

/// 訂單記錄
#[derive(Debug, Clone)]
pub struct CliOrderRecord {
    pub order_id: String,
    pub side: String,
    pub size: f64,
    pub price: f64,
    pub status: String,
    pub fill_time: u64,
    pub latency_us: u64,
    pub strategy: String,
    pub pnl: f64,
}

/// HFT CLI 系統
pub struct HftCliSystem {
    /// 特徵提取器
    feature_extractor: FeatureExtractor,
    
    /// 交易狀態
    status: Arc<Mutex<CliTradingStatus>>,
    
    /// 訂單記錄
    recent_orders: Arc<Mutex<VecDeque<CliOrderRecord>>>,
    
    /// 系統日誌
    system_logs: Arc<Mutex<VecDeque<String>>>,
    
    /// 開始時間
    start_time: Instant,
    
    /// 配置
    is_live_trading: bool,
}

impl HftCliSystem {
    pub fn new(live_trading: bool) -> Result<Self> {
        let feature_extractor = FeatureExtractor::new(50);
        
        Ok(Self {
            feature_extractor,
            status: Arc::new(Mutex::new(CliTradingStatus {
                is_live_trading: live_trading,
                ..Default::default()
            })),
            recent_orders: Arc::new(Mutex::new(VecDeque::with_capacity(100))),
            system_logs: Arc::new(Mutex::new(VecDeque::with_capacity(50))),
            start_time: Instant::now(),
            is_live_trading: live_trading,
        })
    }
    
    /// 處理LOB數據並生成交易信號
    pub fn process_lob_data(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != "BTCUSDT" {
            return Ok(());
        }
        
        // 解析LOB數據
        let orderbook = self.parse_lob_data(data, timestamp)?;
        
        // 提取特徵
        let features = self.feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        
        // 更新市場數據
        {
            let mut status = self.status.lock().unwrap();
            let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
            let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
            
            status.last_price = (best_bid + best_ask) / 2.0;
            status.spread_bps = if status.last_price > 0.0 {
                (best_ask - best_bid) / status.last_price * 10000.0
            } else { 0.0 };
            status.obi_l5 = features.obi_l5;
            status.last_update = timestamp;
            status.uptime_seconds = self.start_time.elapsed().as_secs();
        }
        
        // 生成交易信號
        if let Some(signal) = self.generate_hft_signal(&features, &orderbook) {
            self.execute_trading_signal(signal, timestamp)?;
        }
        
        Ok(())
    }
    
    /// 生成HFT信號
    fn generate_hft_signal(&self, features: &FeatureSet, orderbook: &OrderBook) -> Option<HftSignal> {
        // 強OBI不平衡檢測
        let strong_obi = features.obi_l5.abs() > 0.3;
        let reasonable_spread = features.spread_bps < 15.0;
        let depth_pressure = features.depth_pressure_bid.abs() > 0.2 || 
                           features.depth_pressure_ask.abs() > 0.2;
        
        if strong_obi && reasonable_spread && depth_pressure {
            let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
            let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
            
            let direction = if features.obi_l5 > 0.0 {
                TradeDirection::Buy
            } else {
                TradeDirection::Sell
            };
            
            let entry_price = match direction {
                TradeDirection::Buy => best_ask,
                TradeDirection::Sell => best_bid,
                _ => (best_bid + best_ask) / 2.0,
            };
            
            let position_size = self.calculate_position_size(features.obi_l5.abs());
            
            return Some(HftSignal {
                direction,
                signal_type: HftTradeType::Scalping,
                entry_price,
                position_size,
                confidence: features.obi_l5.abs(),
                expected_hold_time_ms: 2000,
                reasoning: format!("Strong OBI: {:.3}, Spread: {:.1}bps", 
                                 features.obi_l5, features.spread_bps),
            });
        }
        
        None
    }
    
    /// 計算倉位大小
    fn calculate_position_size(&self, signal_strength: f64) -> f64 {
        let base_size = 2.0; // 基礎 2 USDT
        let strength_multiplier = (signal_strength * 3.0).min(5.0).max(0.5);
        (base_size * strength_multiplier).min(10.0) // 最大 10 USDT
    }
    
    /// 執行交易信號
    fn execute_trading_signal(&self, signal: HftSignal, timestamp: u64) -> Result<()> {
        let order_id = format!("hft_{}", timestamp);
        let execution_start = now_micros();
        
        // 模擬執行延遲
        std::thread::sleep(Duration::from_micros(50)); // 50μs 模擬延遲
        
        let execution_latency = now_micros() - execution_start;
        
        // 計算模擬P&L
        let fee = signal.position_size * 0.0004; // 0.04% 手續費
        let expected_pnl = match signal.signal_type {
            HftTradeType::Scalping => signal.confidence * 0.5 - fee,
            HftTradeType::SpreadCapture => signal.confidence * 0.3 - fee,
            _ => signal.confidence * 0.4 - fee,
        };
        
        // 創建訂單記錄
        let order = CliOrderRecord {
            order_id: order_id.clone(),
            side: format!("{:?}", signal.direction),
            size: signal.position_size,
            price: signal.entry_price,
            status: if self.is_live_trading { "FILLED".to_string() } else { "SIMULATED".to_string() },
            fill_time: timestamp,
            latency_us: execution_latency,
            strategy: format!("{:?}", signal.signal_type),
            pnl: expected_pnl,
        };
        
        // 更新統計
        {
            let mut status = self.status.lock().unwrap();
            status.total_orders += 1;
            status.filled_orders += 1;
            status.realized_pnl += expected_pnl;
            status.total_fees += fee;
            status.current_balance += expected_pnl;
            
            // 更新倉位
            match signal.direction {
                TradeDirection::Buy => status.current_position += signal.position_size,
                TradeDirection::Sell => status.current_position -= signal.position_size,
                _ => {}
            }
            
            // 更新延遲統計
            let count = status.filled_orders as f64;
            status.avg_execution_latency_us = (status.avg_execution_latency_us * (count - 1.0) + execution_latency as f64) / count;
            
            // 更新勝率
            if status.total_orders > 0 {
                let winning_orders = if expected_pnl > 0.0 { 1 } else { 0 };
                status.win_rate = winning_orders as f64 / status.total_orders as f64 * 100.0;
            }
        }
        
        // 添加到訂單記錄
        {
            let mut orders = self.recent_orders.lock().unwrap();
            orders.push_back(order);
            if orders.len() > 100 {
                orders.pop_front();
            }
        }
        
        // 添加日誌
        self.add_log(format!("🎯 {} {:.1} USDT @ {:.2} | P&L: {:.4} | {}μs", 
                            signal.direction_str(), signal.position_size, signal.entry_price, 
                            expected_pnl, execution_latency));
        
        Ok(())
    }
    
    /// 添加系統日誌
    fn add_log(&self, message: String) {
        let mut logs = self.system_logs.lock().unwrap();
        let timestamp = chrono::Local::now().format("%H:%M:%S");
        logs.push_back(format!("[{}] {}", timestamp, message));
        if logs.len() > 50 {
            logs.pop_front();
        }
    }
    
    /// 解析LOB數據
    fn parse_lob_data(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // 解析 bids
        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
            for bid in bid_data.iter().take(10) {
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
            for ask in ask_data.iter().take(10) {
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
        orderbook.data_quality_score = 1.0;
        
        Ok(orderbook)
    }
    
    /// 獲取狀態
    pub fn get_status(&self) -> CliTradingStatus {
        self.status.lock().unwrap().clone()
    }
    
    /// 獲取最近訂單
    pub fn get_recent_orders(&self, limit: usize) -> Vec<CliOrderRecord> {
        let orders = self.recent_orders.lock().unwrap();
        orders.iter().rev().take(limit).cloned().collect()
    }
    
    /// 獲取系統日誌
    pub fn get_logs(&self, limit: usize) -> Vec<String> {
        let logs = self.system_logs.lock().unwrap();
        logs.iter().rev().take(limit).cloned().collect()
    }
}

/// CLI 渲染器
pub struct CliRenderer {
    last_render: Instant,
}

impl CliRenderer {
    pub fn new() -> Self {
        Self {
            last_render: Instant::now(),
        }
    }
    
    /// 渲染完整界面
    pub fn render(&mut self, system: &HftCliSystem) -> Result<()> {
        // 限制渲染頻率
        if self.last_render.elapsed() < Duration::from_millis(100) {
            return Ok(());
        }
        self.last_render = Instant::now();
        
        let status = system.get_status();
        let orders = system.get_recent_orders(8);
        let logs = system.get_logs(6);
        
        // 清屏並移動到頂部
        execute!(stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
        
        // 標題
        self.print_title(&status)?;
        
        // 主要指標
        self.print_main_metrics(&status)?;
        
        // P&L 和風險
        self.print_pnl_risk(&status)?;
        
        // 性能指標
        self.print_performance(&status)?;
        
        // 最近訂單
        self.print_recent_orders(&orders)?;
        
        // 系統日誌
        self.print_system_logs(&logs)?;
        
        // 控制說明
        self.print_controls()?;
        
        stdout().flush()?;
        
        Ok(())
    }
    
    fn print_title(&self, status: &CliTradingStatus) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Green))?;
        println!("╔═══════════════════════════════════════════════════════════════════════════════╗");
        println!("║                          ⚡ HFT TRADING DASHBOARD ⚡                          ║");
        execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        print!("║  Mode: ");
        if status.is_live_trading {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
            print!("🔴 LIVE TRADING");
        } else {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
            print!("🟡 SIMULATION");
        }
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("  │  Uptime: {}s  │  Price: ${:.2}     ║", 
                status.uptime_seconds, status.last_price);
        execute!(stdout(), SetForegroundColor(Color::Green))?;
        println!("╚═══════════════════════════════════════════════════════════════════════════════╝");
        
        Ok(())
    }
    
    fn print_main_metrics(&self, status: &CliTradingStatus) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\n📊 TRADING STATISTICS");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("┌─────────────┬─────────────┬─────────────┬─────────────┬─────────────┐");
        println!("│ Total Orders│ Filled      │ Pending     │ Rejected    │ Win Rate    │");
        println!("├─────────────┼─────────────┼─────────────┼─────────────┼─────────────┤");
        execute!(stdout(), SetForegroundColor(Color::Green))?;
        print!("│     {:>7} ", status.total_orders);
        execute!(stdout(), SetForegroundColor(Color::Green))?;
        print!("│     {:>7} ", status.filled_orders);
        execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        print!("│     {:>7} ", status.pending_orders);
        execute!(stdout(), SetForegroundColor(Color::Red))?;
        print!("│     {:>7} ", status.rejected_orders);
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        print!("│    {:>6.1}% ", status.win_rate);
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("│");
        println!("└─────────────┴─────────────┴─────────────┴─────────────┴─────────────┘");
        
        Ok(())
    }
    
    fn print_pnl_risk(&self, status: &CliTradingStatus) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\n💰 P&L & RISK MANAGEMENT");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("┌────────────────┬────────────────┬────────────────┬────────────────┐");
        println!("│ Realized P&L   │ Balance        │ Position       │ Fees Paid      │");
        println!("├────────────────┼────────────────┼────────────────┼────────────────┤");
        
        // Realized P&L 顏色
        if status.realized_pnl >= 0.0 {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
            print!("│ +{:>11.4} ", status.realized_pnl);
        } else {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
            print!("│ {:>12.4} ", status.realized_pnl);
        }
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        print!("│ {:>11.2} USDT", status.current_balance);
        
        // Position 顏色
        if status.current_position > 0.0 {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
            print!("│ Long {:>8.1} ", status.current_position);
        } else if status.current_position < 0.0 {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
            print!("│ Short{:>8.1} ", status.current_position.abs());
        } else {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
            print!("│ Flat {:>8.1} ", 0.0);
        }
        
        execute!(stdout(), SetForegroundColor(Color::Red))?;
        print!("│ {:>11.4} USDT", status.total_fees);
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("│");
        println!("└────────────────┴────────────────┴────────────────┴────────────────┘");
        
        Ok(())
    }
    
    fn print_performance(&self, status: &CliTradingStatus) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\n⚡ PERFORMANCE METRICS");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("┌─────────────────┬─────────────────┬─────────────────┬─────────────────┐");
        println!("│ Avg Latency     │ Orders/Sec      │ Spread          │ OBI L5          │");
        println!("├─────────────────┼─────────────────┼─────────────────┼─────────────────┤");
        
        // 延遲顏色編碼
        if status.avg_execution_latency_us < 100.0 {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
        } else if status.avg_execution_latency_us < 500.0 {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        } else {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
        }
        print!("│     {:>8.0}μs ", status.avg_execution_latency_us);
        
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        print!("│     {:>8.2} ", status.orders_per_second);
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        print!("│     {:>8.1}bps ", status.spread_bps);
        
        // OBI 顏色編碼
        if status.obi_l5.abs() > 0.3 {
            execute!(stdout(), SetForegroundColor(Color::Red))?;
        } else if status.obi_l5.abs() > 0.1 {
            execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        } else {
            execute!(stdout(), SetForegroundColor(Color::Green))?;
        }
        print!("│     {:>+8.3} ", status.obi_l5);
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("│");
        println!("└─────────────────┴─────────────────┴─────────────────┴─────────────────┘");
        
        Ok(())
    }
    
    fn print_recent_orders(&self, orders: &[CliOrderRecord]) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\n📋 RECENT ORDERS");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("┌──────────┬──────┬─────────┬──────────┬─────────┬─────────┬──────────┐");
        println!("│   Time   │ Side │  Size   │  Price   │ Status  │ Latency │   P&L    │");
        println!("├──────────┼──────┼─────────┼──────────┼─────────┼─────────┼──────────┤");
        
        for order in orders.iter().take(8) {
            let time = chrono::DateTime::from_timestamp((order.fill_time / 1_000_000) as i64, 0)
                .unwrap_or_default()
                .format("%H:%M:%S");
            
            print!("│ {} ", time);
            
            // Side 顏色
            if order.side == "Buy" {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
            } else {
                execute!(stdout(), SetForegroundColor(Color::Red))?;
            }
            print!("│ {:>4} ", order.side);
            
            execute!(stdout(), SetForegroundColor(Color::White))?;
            print!("│ {:>7.1} │ {:>8.2} ", order.size, order.price);
            
            // Status 顏色
            if order.status == "FILLED" {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
            } else {
                execute!(stdout(), SetForegroundColor(Color::Yellow))?;
            }
            print!("│ {:>7} ", order.status);
            
            execute!(stdout(), SetForegroundColor(Color::Cyan))?;
            print!("│ {:>5}μs ", order.latency_us);
            
            // P&L 顏色
            if order.pnl >= 0.0 {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
                print!("│ +{:>7.4} ", order.pnl);
            } else {
                execute!(stdout(), SetForegroundColor(Color::Red))?;
                print!("│ {:>8.4} ", order.pnl);
            }
            
            execute!(stdout(), SetForegroundColor(Color::White))?;
            println!("│");
        }
        
        if orders.is_empty() {
            execute!(stdout(), SetForegroundColor(Color::DarkGrey))?;
            println!("│                            No orders yet                             │");
        }
        
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("└──────────┴──────┴─────────┴──────────┴─────────┴─────────┴──────────┘");
        
        Ok(())
    }
    
    fn print_system_logs(&self, logs: &[String]) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("\n📜 SYSTEM LOGS");
        execute!(stdout(), SetForegroundColor(Color::DarkGrey))?;
        
        for log in logs.iter().take(6) {
            println!("  {}", log);
        }
        
        if logs.is_empty() {
            println!("  No logs available");
        }
        
        Ok(())
    }
    
    fn print_controls(&self) -> Result<()> {
        execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        println!("\n🎮 CONTROLS: [Q]uit | [R]eset | [L]ive Toggle | [SPACE]Pause");
        execute!(stdout(), ResetColor)?;
        
        Ok(())
    }
}

// Signal 和相關類型定義
#[derive(Debug, Clone)]
pub struct HftSignal {
    pub direction: TradeDirection,
    pub signal_type: HftTradeType,
    pub entry_price: f64,
    pub position_size: f64,
    pub confidence: f64,
    pub expected_hold_time_ms: u64,
    pub reasoning: String,
}

impl HftSignal {
    pub fn direction_str(&self) -> &str {
        match self.direction {
            TradeDirection::Buy => "BUY",
            TradeDirection::Sell => "SELL",
            TradeDirection::Close => "CLOSE",
        }
    }
}

#[derive(Debug, Clone)]
pub enum TradeDirection {
    Buy,
    Sell,
    Close,
}

#[derive(Debug, Clone)]
pub enum HftTradeType {
    Scalping,
    SpreadCapture,
    MicrostructureArbitrage,
    LiquidityProvision,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌（靜默模式）
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_target(false)
        .without_time()
        .init();

    // 啟用終端原始模式
    enable_raw_mode()?;
    execute!(stdout(), Hide)?;
    
    let result = run_hft_cli().await;
    
    // 恢復終端
    disable_raw_mode()?;
    execute!(stdout(), Show, Clear(ClearType::All), MoveTo(0, 0))?;
    
    result
}

async fn run_hft_cli() -> Result<()> {
    // 創建系統
    let mut system = HftCliSystem::new(false)?; // 默認模擬模式
    let mut renderer = CliRenderer::new();
    
    // 創建Bitget連接
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);
    
    system.add_log("🚀 HFT CLI Dashboard starting...".to_string());
    system.add_log("📡 Connecting to Bitget BTCUSDT...".to_string());
    
    // 統計計數器
    let mut update_counter = 0u64;
    let start_time = std::time::Instant::now();
    
    // 創建消息處理器
    let system_arc = Arc::new(Mutex::new(system));
    let system_clone = system_arc.clone();
    
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                if let Ok(mut system) = system_clone.lock() {
                    if let Err(e) = system.process_lob_data(&symbol, &data, timestamp) {
                        system.add_log(format!("❌ Error processing LOB: {}", e));
                    }
                }
            }
            _ => {}
        }
    };
    
    // 渲染任務
    let system_render = system_arc.clone();
    tokio::spawn(async move {
        let mut renderer = CliRenderer::new();
        loop {
            if let Ok(system) = system_render.lock() {
                if let Err(e) = renderer.render(&*system) {
                    eprintln!("Render error: {}", e);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    
    // 鍵盤輸入處理
    tokio::spawn(async move {
        loop {
            if poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(event) = read() {
                    if let Event::Key(key) = event {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                std::process::exit(0);
                            }
                            KeyCode::Char('l') | KeyCode::Char('L') => {
                                // Toggle live trading mode
                                println!("Live trading toggle not implemented in this demo");
                            }
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                // Reset statistics
                                println!("Reset not implemented in this demo");
                            }
                            _ => {}
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    
    // 啟動連接
    system_arc.lock().unwrap().add_log("✅ Starting market data stream".to_string());
    
    if let Err(e) = connector.connect_public(message_handler).await {
        eprintln!("Connection failed: {}", e);
        return Err(e);
    }
    
    Ok(())
}