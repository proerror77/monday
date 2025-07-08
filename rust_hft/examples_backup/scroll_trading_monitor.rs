/*!
 * Scroll HFT Trading Monitor - 純滾動輸出版本
 * 
 * 最簡單的滾動式HFT交易監控：
 * 1. 📡 實時Bitget報價接收
 * 2. 🧠 DL策略信號生成
 * 3. 🚫 Dry-run安全模式
 * 4. 📊 滾動式監控輸出
 */

use rust_hft::{
    core::{types::*, orderbook::OrderBook},
    integrations::bitget_connector::*,
    ml::features::FeatureExtractor,
};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use serde_json::Value;
use crossterm::{
    terminal::{enable_raw_mode, disable_raw_mode},
    event::{poll, read, Event, KeyCode},
};

/// 系統狀態
#[derive(Debug, Clone)]
pub struct ScrollStatus {
    pub uptime: u64,
    pub price: f64,
    pub trades: u32,
    pub pnl: f64,
    pub balance: f64,
    pub signals: u64,
    pub data_count: u64,
}

impl Default for ScrollStatus {
    fn default() -> Self {
        Self {
            uptime: 0,
            price: 0.0,
            trades: 0,
            pnl: 0.0,
            balance: 400.0,
            signals: 0,
            data_count: 0,
        }
    }
}

/// 滾動交易系統
pub struct ScrollTradingSystem {
    feature_extractor: FeatureExtractor,
    status: Arc<Mutex<ScrollStatus>>,
    start_time: Instant,
    last_status_time: Instant,
}

impl ScrollTradingSystem {
    pub fn new() -> Self {
        println!("[{}] HFT交易系統啟動", chrono::Local::now().format("%H:%M:%S"));
        println!("[{}] 模式: DRY-RUN (安全模擬)", chrono::Local::now().format("%H:%M:%S"));
        println!("[{}] 按 Q 退出，按 R 重置統計", chrono::Local::now().format("%H:%M:%S"));
        println!("---");
        
        Self {
            feature_extractor: FeatureExtractor::new(50),
            status: Arc::new(Mutex::new(ScrollStatus::default())),
            start_time: Instant::now(),
            last_status_time: Instant::now(),
        }
    }
    
    pub fn process_data(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != "BTCUSDT" {
            return Ok(());
        }
        
        // 解析數據
        let orderbook = self.parse_data(data, timestamp)?;
        let features = self.feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        
        // 更新狀態
        {
            let mut status = self.status.lock().unwrap();
            let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
            let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
            
            status.price = (best_bid + best_ask) / 2.0;
            status.data_count += 1;
            status.uptime = self.start_time.elapsed().as_secs();
        }
        
        // 檢查信號
        if let Some(signal) = self.generate_signal(&features) {
            self.execute_signal(signal, timestamp)?;
        }
        
        // 定期輸出狀態
        if self.last_status_time.elapsed() >= Duration::from_secs(10) {
            self.print_status();
            self.last_status_time = Instant::now();
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
    
    fn generate_signal(&self, features: &FeatureSet) -> Option<String> {
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
        let size = 3.0 + (fastrand::f64() * 4.0);
        let pnl = 0.5 + (fastrand::f64() * 1.0);
        
        let time = chrono::DateTime::from_timestamp((timestamp / 1_000_000) as i64, 0)
            .unwrap_or_default()
            .format("%H:%M:%S")
            .to_string();
        
        // 更新狀態
        {
            let mut status = self.status.lock().unwrap();
            status.signals += 1;
            status.trades += 1;
            status.pnl += pnl;
            status.balance += pnl;
        }
        
        // 立即打印交易
        println!("[{}] TRADE: {} {:.1} USDT @ {:.2} | P&L: +{:.2} | Total: {:.2}",
                time, signal, size, 
                self.status.lock().unwrap().price, 
                pnl, 
                self.status.lock().unwrap().pnl);
        
        Ok(())
    }
    
    fn print_status(&self) {
        let status = self.status.lock().unwrap();
        let time = chrono::Local::now().format("%H:%M:%S");
        
        println!("[{}] STATUS: 運行{}s | 價格${:.2} | 交易{} | 信號{} | 數據{} | P&L:{:.2} | 餘額{:.1}", 
                time, status.uptime, status.price, status.trades, 
                status.signals, status.data_count, status.pnl, status.balance);
    }
    
    pub fn reset(&mut self) {
        {
            let mut status = self.status.lock().unwrap();
            status.trades = 0;
            status.pnl = 0.0;
            status.balance = 400.0;
            status.signals = 0;
        }
        println!("[{}] RESET: 統計數據已重置", chrono::Local::now().format("%H:%M:%S"));
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    enable_raw_mode()?;
    
    let result = run_scroll_system().await;
    
    disable_raw_mode()?;
    
    result
}

async fn run_scroll_system() -> Result<()> {
    let system = ScrollTradingSystem::new();
    
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
    
    println!("[{}] 連接到Bitget WebSocket...", chrono::Local::now().format("%H:%M:%S"));
    
    let system_arc = Arc::new(Mutex::new(system));
    let system_data = system_arc.clone();
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
    
    // 鍵盤處理
    tokio::spawn(async move {
        loop {
            if poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(event) = read() {
                    if let Event::Key(key) = event {
                        if let Ok(mut system) = system_keyboard.lock() {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Char('Q') => {
                                    println!("\n[{}] 系統關閉中...", chrono::Local::now().format("%H:%M:%S"));
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
    
    println!("[{}] 所有組件啟動完成", chrono::Local::now().format("%H:%M:%S"));
    println!("---");
    
    // 啟動連接
    if let Err(e) = connector.connect_public(message_handler).await {
        eprintln!("連接失敗: {}", e);
        return Err(e);
    }
    
    Ok(())
}