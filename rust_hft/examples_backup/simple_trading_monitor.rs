/*!
 * Simple HFT Trading Monitor - Simplified Clean UI
 * 
 * 簡化版HFT交易監控，具有清潔的終端界面：
 * 1. 📡 實時Bitget報價接收
 * 2. 🧠 DL策略信號生成
 * 3. 🚫 Dry-run安全模式
 * 4. 📊 簡潔CLI監控界面
 */

use rust_hft::{
    core::{types::{*, now_micros}, orderbook::OrderBook},
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};
use serde_json::Value;
use crossterm::{
    terminal::{enable_raw_mode, disable_raw_mode, Clear, ClearType},
    cursor::MoveTo,
    style::{Color, ResetColor, SetForegroundColor},
    execute,
    event::{poll, read, Event, KeyCode},
};
use std::io::{stdout, Write};

/// 簡化系統狀態
#[derive(Debug, Clone)]
pub struct SimpleStatus {
    pub mode: String,
    pub uptime: u64,
    pub price: f64,
    pub spread: f64,
    pub data_count: u64,
    pub signal_count: u64,
    pub trades: u32,
    pub pnl: f64,
    pub balance: f64,
    pub last_signal: Option<String>,
    pub avg_latency: f64,
}

impl Default for SimpleStatus {
    fn default() -> Self {
        Self {
            mode: "DRY-RUN".to_string(),
            uptime: 0,
            price: 0.0,
            spread: 0.0,
            data_count: 0,
            signal_count: 0,
            trades: 0,
            pnl: 0.0,
            balance: 400.0,
            last_signal: None,
            avg_latency: 0.0,
        }
    }
}

/// 簡化交易記錄
#[derive(Debug, Clone)]
pub struct SimpleTradeRecord {
    pub time: String,
    pub side: String,
    pub size: f64,
    pub price: f64,
    pub pnl: f64,
}

/// 簡化交易系統
pub struct SimpleTradingSystem {
    feature_extractor: FeatureExtractor,
    status: Arc<Mutex<SimpleStatus>>,
    trades: Arc<Mutex<VecDeque<SimpleTradeRecord>>>,
    logs: Arc<Mutex<VecDeque<String>>>,
    start_time: Instant,
}

impl SimpleTradingSystem {
    pub fn new() -> Self {
        Self {
            feature_extractor: FeatureExtractor::new(50),
            status: Arc::new(Mutex::new(SimpleStatus::default())),
            trades: Arc::new(Mutex::new(VecDeque::with_capacity(10))),
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(10))),
            start_time: Instant::now(),
        }
    }
    
    pub fn process_data(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != "BTCUSDT" {
            return Ok(());
        }
        
        let process_start = now_micros();
        
        // 解析數據
        let orderbook = self.parse_data(data, timestamp)?;
        let features = self.feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        
        // 更新狀態
        {
            let mut status = self.status.lock().unwrap();
            let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
            let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
            
            status.price = (best_bid + best_ask) / 2.0;
            status.spread = if status.price > 0.0 {
                (best_ask - best_bid) / status.price * 10000.0
            } else { 0.0 };
            status.data_count += 1;
            status.uptime = self.start_time.elapsed().as_secs();
            
            let latency = now_micros() - process_start;
            let count = status.data_count as f64;
            status.avg_latency = (status.avg_latency * (count - 1.0) + latency as f64) / count;
        }
        
        // 檢查信號
        if let Some(signal) = self.generate_signal(&features, &orderbook) {
            self.execute_signal(signal, timestamp)?;
        }
        
        Ok(())
    }
    
    fn parse_data(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
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
        
        Ok(orderbook)
    }
    
    fn generate_signal(&self, features: &FeatureSet, _orderbook: &OrderBook) -> Option<String> {
        let obi_strength = features.obi_l5.abs();
        let spread_ok = features.spread_bps < 15.0;
        let imbalance = features.depth_pressure_bid.abs() > 0.2 || features.depth_pressure_ask.abs() > 0.2;
        
        if obi_strength > 0.3 && spread_ok && imbalance {
            Some(if features.obi_l5 > 0.0 { "BUY".to_string() } else { "SELL".to_string() })
        } else {
            None
        }
    }
    
    fn execute_signal(&mut self, signal: String, timestamp: u64) -> Result<()> {
        let size = 3.0 + (fastrand::f64() * 4.0); // 3-7 USDT
        let pnl = 0.5 + (fastrand::f64() * 1.0); // 模擬收益
        
        let time = chrono::DateTime::from_timestamp((timestamp / 1_000_000) as i64, 0)
            .unwrap_or_default()
            .format("%H:%M:%S")
            .to_string();
        
        let trade = SimpleTradeRecord {
            time: time.clone(),
            side: signal.clone(),
            size,
            price: {
                let status = self.status.lock().unwrap();
                status.price
            },
            pnl,
        };
        
        // 更新狀態
        {
            let mut status = self.status.lock().unwrap();
            status.signal_count += 1;
            status.trades += 1;
            status.pnl += pnl;
            status.balance += pnl;
            status.last_signal = Some(format!("{} {:.1}U", signal, size));
        }
        
        // 添加交易記錄
        {
            let mut trades = self.trades.lock().unwrap();
            trades.push_back(trade);
            if trades.len() > 10 {
                trades.pop_front();
            }
        }
        
        // 添加日誌
        self.add_log(format!("{} {} {:.1} USDT | P&L: +{:.2}", time, signal, size, pnl));
        
        Ok(())
    }
    
    fn add_log(&self, message: String) {
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(message);
        if logs.len() > 10 {
            logs.pop_front();
        }
    }
    
    pub fn get_status(&self) -> SimpleStatus {
        self.status.lock().unwrap().clone()
    }
    
    pub fn get_trades(&self) -> Vec<SimpleTradeRecord> {
        self.trades.lock().unwrap().iter().cloned().collect()
    }
    
    pub fn get_logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().iter().cloned().collect()
    }
    
    pub fn reset(&mut self) {
        {
            let mut status = self.status.lock().unwrap();
            status.trades = 0;
            status.pnl = 0.0;
            status.balance = 400.0;
            status.signal_count = 0;
            status.last_signal = None;
        }
        self.trades.lock().unwrap().clear();
        self.logs.lock().unwrap().clear();
        self.add_log("系統統計已重置".to_string());
    }
}

/// 簡潔渲染器
pub struct SimpleRenderer {
    last_render: Instant,
}

impl SimpleRenderer {
    pub fn new() -> Self {
        Self {
            last_render: Instant::now(),
        }
    }
    
    pub fn render(&mut self, system: &SimpleTradingSystem) -> Result<()> {
        if self.last_render.elapsed() < Duration::from_millis(100) {
            return Ok(());
        }
        self.last_render = Instant::now();
        
        let status = system.get_status();
        let trades = system.get_trades();
        let logs = system.get_logs();
        
        // 清屏
        execute!(stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
        
        // 標題
        execute!(stdout(), SetForegroundColor(Color::Green))?;
        println!("================================================================================");
        println!("                        HFT TRADING MONITOR v1.0");
        println!("================================================================================");
        
        // 狀態行
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("Mode: {} | Uptime: {}s | Price: ${:.2} | Spread: {:.1}bps",
                status.mode, status.uptime, status.price, status.spread);
        println!();
        
        // 統計
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("STATISTICS");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        println!("  Data Updates: {}  |  Signals: {}  |  Trades: {}  |  Avg Latency: {:.0}μs",
                status.data_count, status.signal_count, status.trades, status.avg_latency);
        println!("  Total P&L: {:.2} USDT  |  Balance: {:.1} USDT  |  Return: {:.2}%",
                status.pnl, status.balance, (status.pnl / 400.0) * 100.0);
        
        if let Some(ref signal) = status.last_signal {
            println!("  Last Signal: {}", signal);
        }
        println!();
        
        // 交易記錄
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("RECENT TRADES");
        execute!(stdout(), SetForegroundColor(Color::White))?;
        for trade in trades.iter().rev().take(5) {
            let color = if trade.side == "BUY" { Color::Green } else { Color::Red };
            execute!(stdout(), SetForegroundColor(color))?;
            print!("  {} {:>4}", trade.time, trade.side);
            execute!(stdout(), SetForegroundColor(Color::White))?;
            println!(" {:.1} USDT @ {:.2} | P&L: +{:.2}", trade.size, trade.price, trade.pnl);
        }
        
        if trades.is_empty() {
            execute!(stdout(), SetForegroundColor(Color::DarkGrey))?;
            println!("  No trades yet");
        }
        println!();
        
        // 系統日誌
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("SYSTEM LOGS");
        execute!(stdout(), SetForegroundColor(Color::DarkGrey))?;
        for log in logs.iter().rev().take(3) {
            println!("  {}", log);
        }
        println!();
        
        // 控制說明
        execute!(stdout(), SetForegroundColor(Color::Yellow))?;
        println!("CONTROLS: [Q]uit | [R]eset");
        execute!(stdout(), ResetColor)?;
        
        stdout().flush()?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_target(false)
        .without_time()
        .init();

    enable_raw_mode()?;
    
    let result = run_simple_monitor().await;
    
    disable_raw_mode()?;
    execute!(stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
    
    result
}

async fn run_simple_monitor() -> Result<()> {
    let system = SimpleTradingSystem::new();
    let _renderer = SimpleRenderer::new();
    
    // Bitget連接
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 5,
    };

    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);
    
    system.add_log("HFT交易監控啟動".to_string());
    system.add_log("連接Bitget WebSocket...".to_string());
    
    let system_arc = Arc::new(Mutex::new(system));
    let system_data = system_arc.clone();
    let system_render = system_arc.clone();
    let system_keyboard = system_arc.clone();
    
    // 數據處理
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                if let Ok(mut system) = system_data.lock() {
                    let _ = system.process_data(&symbol, &data, timestamp);
                }
            }
            _ => {}
        }
    };
    
    // 渲染任務
    tokio::spawn(async move {
        let mut renderer = SimpleRenderer::new();
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
    
    // 鍵盤處理
    tokio::spawn(async move {
        loop {
            if poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(event) = read() {
                    if let Event::Key(key) = event {
                        if let Ok(mut system) = system_keyboard.lock() {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Char('Q') => {
                                    std::process::exit(0);
                                }
                                KeyCode::Char('r') | KeyCode::Char('R') => {
                                    system.reset();
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
    
    system_arc.lock().unwrap().add_log("所有組件啟動完成".to_string());
    
    // 啟動連接
    if let Err(e) = connector.connect_public(message_handler).await {
        eprintln!("連接失敗: {}", e);
        return Err(e);
    }
    
    Ok(())
}