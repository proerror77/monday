/*!
 * 簡單交易監控工具
 * 
 * 終端 UI 監控實時交易活動和市場數據
 */

use rust_hft::{
    integrations::bitget_connector::*,
    core::types::*,
};
use anyhow::Result;
use tracing::{info, error};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};
use std::io::{stdout, Write};
use crossterm::{
    terminal::{enable_raw_mode, disable_raw_mode, Clear, ClearType},
    cursor::{MoveTo, Hide, Show},
    execute,
    style::{Color, SetForegroundColor, ResetColor},
};

#[derive(Debug, Clone)]
struct MarketSnapshot {
    symbol: String,
    best_bid: f64,
    best_ask: f64,
    mid_price: f64,
    spread_bps: f64,
    timestamp: u64,
}

#[derive(Debug)]
struct TradingMonitor {
    market_data: Arc<Mutex<VecDeque<MarketSnapshot>>>,
    update_count: Arc<Mutex<u64>>,
    start_time: Instant,
}

impl TradingMonitor {
    fn new() -> Self {
        Self {
            market_data: Arc::new(Mutex::new(VecDeque::with_capacity(1000))),
            update_count: Arc::new(Mutex::new(0)),
            start_time: Instant::now(),
        }
    }
    
    fn add_market_data(&self, snapshot: MarketSnapshot) {
        if let Ok(mut data) = self.market_data.lock() {
            data.push_back(snapshot);
            if data.len() > 100 {
                data.pop_front();
            }
        }
        
        if let Ok(mut count) = self.update_count.lock() {
            *count += 1;
        }
    }
    
    fn display_dashboard(&self) -> Result<()> {
        // 清屏
        execute!(stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
        
        // 標題
        execute!(stdout(), SetForegroundColor(Color::Cyan))?;
        println!("📊 === HFT TRADING MONITOR === 📊");
        execute!(stdout(), ResetColor)?;
        
        // 運行時間
        let elapsed = self.start_time.elapsed();
        let update_count = self.update_count.lock().unwrap();
        let updates_per_sec = *update_count as f64 / elapsed.as_secs_f64();
        
        println!("⏱️  Runtime: {:02}:{:02}:{:02}", 
                 elapsed.as_secs() / 3600,
                 (elapsed.as_secs() % 3600) / 60,
                 elapsed.as_secs() % 60);
        println!("📈 Updates: {} ({:.1}/sec)", *update_count, updates_per_sec);
        println!();
        
        // 市場數據
        if let Ok(data) = self.market_data.lock() {
            if let Some(latest) = data.back() {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
                println!("🎯 LATEST MARKET DATA");
                execute!(stdout(), ResetColor)?;
                
                println!("Symbol: {}", latest.symbol);
                println!("Best Bid: {:.2}", latest.best_bid);
                println!("Best Ask: {:.2}", latest.best_ask);
                println!("Mid Price: {:.2}", latest.mid_price);
                
                // 點差顏色編碼
                if latest.spread_bps < 5.0 {
                    execute!(stdout(), SetForegroundColor(Color::Green))?;
                } else if latest.spread_bps < 15.0 {
                    execute!(stdout(), SetForegroundColor(Color::Yellow))?;
                } else {
                    execute!(stdout(), SetForegroundColor(Color::Red))?;
                }
                println!("Spread: {:.1} bps", latest.spread_bps);
                execute!(stdout(), ResetColor)?;
                
                println!();
                
                // 價格歷史圖表 (簡化版)
                println!("📊 PRICE HISTORY (Last 20 updates):");
                self.display_price_chart(&data)?;
            } else {
                println!("⏳ Waiting for market data...");
            }
        }
        
        println!("\n💡 Press Ctrl+C to exit");
        stdout().flush()?;
        
        Ok(())
    }
    
    fn display_price_chart(&self, data: &VecDeque<MarketSnapshot>) -> Result<()> {
        let recent_data: Vec<_> = data.iter().rev().take(20).collect();
        
        if recent_data.len() < 2 {
            return Ok(());
        }
        
        // 找到價格範圍
        let mut min_price = f64::MAX;
        let mut max_price = f64::MIN;
        
        for snapshot in &recent_data {
            min_price = min_price.min(snapshot.mid_price);
            max_price = max_price.max(snapshot.mid_price);
        }
        
        let price_range = max_price - min_price;
        if price_range == 0.0 {
            return Ok(());
        }
        
        // 繪製簡單的 ASCII 圖表
        for snapshot in recent_data.iter().rev() {
            let normalized = ((snapshot.mid_price - min_price) / price_range * 50.0) as usize;
            let bar = "█".repeat(normalized.max(1));
            
            // 價格變化顏色
            if snapshot.mid_price > recent_data[0].mid_price {
                execute!(stdout(), SetForegroundColor(Color::Green))?;
            } else if snapshot.mid_price < recent_data[0].mid_price {
                execute!(stdout(), SetForegroundColor(Color::Red))?;
            }
            
            println!("{:8.2} |{}", snapshot.mid_price, bar);
            execute!(stdout(), ResetColor)?;
        }
        
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化終端
    enable_raw_mode()?;
    execute!(stdout(), Hide)?;
    
    let monitor = Arc::new(TradingMonitor::new());
    
    // 配置 Bitget 連接
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 5,
    };

    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    // 消息處理
    let monitor_clone = monitor.clone();
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                if let Ok(snapshot) = parse_market_snapshot(&symbol, &data, timestamp) {
                    monitor_clone.add_market_data(snapshot);
                }
            },
            _ => {}
        }
    };

    // 啟動 UI 更新任務
    let monitor_ui = monitor.clone();
    let ui_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        
        loop {
            interval.tick().await;
            if let Err(e) = monitor_ui.display_dashboard() {
                error!("UI update error: {}", e);
                break;
            }
        }
    });

    // 運行監控器
    tokio::select! {
        result = connector.connect_public(message_handler) => {
            if let Err(e) = result {
                error!("Connection failed: {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutting down monitor...");
        }
    }

    ui_task.abort();
    
    // 恢復終端
    execute!(stdout(), Show, Clear(ClearType::All), MoveTo(0, 0))?;
    disable_raw_mode()?;
    
    println!("👋 Trading monitor stopped. Goodbye!");
    
    Ok(())
}

fn parse_market_snapshot(symbol: &str, data: &serde_json::Value, timestamp: u64) -> Result<MarketSnapshot> {
    // 簡化的解析邏輯
    let data_array = data.as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
    
    let first_item = data_array.first()
        .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;
    
    let mut best_bid = 0.0;
    let mut best_ask = f64::MAX;
    
    // 解析 bids (找最高價)
    if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
        for bid in bid_data.iter().take(1) {
            if let Some(bid_array) = bid.as_array() {
                if bid_array.len() >= 2 {
                    if let Ok(price) = bid_array[0].as_str().unwrap_or("0").parse::<f64>() {
                        best_bid = f64::max(best_bid, price);
                    }
                }
            }
        }
    }
    
    // 解析 asks (找最低價)
    if let Some(ask_data) = first_item.get("asks").and_then(|v| v.as_array()) {
        for ask in ask_data.iter().take(1) {
            if let Some(ask_array) = ask.as_array() {
                if ask_array.len() >= 2 {
                    if let Ok(price) = ask_array[0].as_str().unwrap_or("0").parse::<f64>() {
                        best_ask = f64::min(best_ask, price);
                    }
                }
            }
        }
    }
    
    if best_ask == f64::MAX {
        best_ask = best_bid;
    }
    
    let mid_price = (best_bid + best_ask) / 2.0;
    let spread_bps = if mid_price > 0.0 {
        ((best_ask - best_bid) / mid_price) * 10000.0
    } else {
        0.0
    };
    
    Ok(MarketSnapshot {
        symbol: symbol.to_string(),
        best_bid,
        best_ask,
        mid_price,
        spread_bps,
        timestamp,
    })
}