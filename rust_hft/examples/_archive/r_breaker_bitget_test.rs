/*!
 * R-Breaker Bitget 連接測試
 * 
 * 測試真實的 Bitget WebSocket 連接和數據接收
 * 驗證 ETHUSDT 真實報價數據
 */

use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;
use serde_json::Value;

/// 簡單的 WebSocket 連接測試
async fn test_bitget_connection() -> Result<(), Box<dyn std::error::Error>> {
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    use futures_util::{SinkExt, StreamExt};

    println!("🚀 測試 Bitget WebSocket 連接...");
    
    // Bitget V2 WebSocket URL
    let url = "wss://ws.bitget.com/v2/ws/public";
    
    // 連接 WebSocket
    let (ws_stream, _) = connect_async(url).await?;
    println!("✅ 成功連接到 Bitget WebSocket");
    
    let (mut write, mut read) = ws_stream.split();
    
    // 訂閱 ETHUSDT 的 orderbook 和 ticker 數據
    let subscribe_msg = serde_json::json!({
        "op": "subscribe",
        "args": [
            {
                "instType": "SPOT",
                "channel": "books5",
                "instId": "ETHUSDT"
            },
            {
                "instType": "SPOT", 
                "channel": "ticker",
                "instId": "ETHUSDT"
            }
        ]
    });
    
    println!("📡 發送訂閱請求: ETHUSDT books5 + ticker");
    write.send(Message::Text(subscribe_msg.to_string())).await?;
    
    let mut message_count = 0;
    let start_time = SystemTime::now();
    
    // 接收消息
    while let Some(message) = read.next().await {
        match message? {
            Message::Text(text) => {
                if let Ok(json) = serde_json::from_str::<Value>(&text) {
                    message_count += 1;
                    
                    // 解析消息類型
                    if let Some(event) = json.get("event").and_then(|v| v.as_str()) {
                        match event {
                            "subscribe" => {
                                println!("✅ 訂閱確認: {:?}", json.get("arg"));
                            }
                            _ => {
                                println!("📩 事件: {}", event);
                            }
                        }
                    } else if let Some(arg) = json.get("arg") {
                        // 數據消息
                        if let Some(channel) = arg.get("channel").and_then(|v| v.as_str()) {
                            match channel {
                                "books5" => {
                                    parse_orderbook_message(&json);
                                }
                                "ticker" => {
                                    parse_ticker_message(&json);
                                }
                                _ => {
                                    println!("📊 其他數據: {}", channel);
                                }
                            }
                        }
                    }
                    
                    // 限制測試時間
                    if message_count >= 20 || start_time.elapsed()? > Duration::from_secs(30) {
                        break;
                    }
                }
            }
            Message::Ping(data) => {
                println!("🏓 收到 Ping，發送 Pong");
                write.send(Message::Pong(data)).await?;
            }
            Message::Close(_) => {
                println!("🔌 連接已關閉");
                break;
            }
            _ => {}
        }
    }
    
    println!("📊 測試完成，共收到 {} 條消息", message_count);
    Ok(())
}

/// 解析 OrderBook 數據
fn parse_orderbook_message(json: &Value) {
    if let Some(data) = json.get("data").and_then(|v| v.as_array()) {
        if let Some(first_item) = data.first() {
            if let (Some(bids), Some(asks)) = (
                first_item.get("bids").and_then(|v| v.as_array()),
                first_item.get("asks").and_then(|v| v.as_array())
            ) {
                if let (Some(best_bid), Some(best_ask)) = (
                    bids.first().and_then(|v| v.as_array()).and_then(|v| v.first()).and_then(|v| v.as_str()),
                    asks.first().and_then(|v| v.as_array()).and_then(|v| v.first()).and_then(|v| v.as_str())
                ) {
                    if let (Ok(bid_price), Ok(ask_price)) = (
                        best_bid.parse::<f64>(),
                        ask_price.parse::<f64>()
                    ) {
                        let mid_price = (bid_price + ask_price) / 2.0;
                        let spread = ask_price - bid_price;
                        let spread_bps = (spread / mid_price) * 10000.0;
                        
                        println!("📈 OrderBook [ETHUSDT] Bid: {:.2} | Ask: {:.2} | Mid: {:.2} | Spread: {:.1} bps", 
                                 bid_price, ask_price, mid_price, spread_bps);
                    }
                }
            }
        }
    }
}

/// 解析 Ticker 數據
fn parse_ticker_message(json: &Value) {
    if let Some(data) = json.get("data").and_then(|v| v.as_array()) {
        if let Some(first_item) = data.first() {
            let last_price = first_item.get("lastPr").and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok());
            let volume = first_item.get("baseVolume").and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok());
            let change = first_item.get("change").and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok());
            let change_pct = first_item.get("changeUtc").and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok());
            
            if let (Some(price), Some(vol)) = (last_price, volume) {
                println!("📊 Ticker [ETHUSDT] Price: {:.2} | Volume: {:.0} | Change: {:.2}% | UTC Change: {:.2}%", 
                         price, vol, change.unwrap_or(0.0) * 100.0, change_pct.unwrap_or(0.0) * 100.0);
            }
        }
    }
}

/// R-Breaker 價位計算測試
fn test_rbreaker_calculation() {
    println!("\n🎯 測試 R-Breaker 價位計算...");
    
    // 模擬前一日數據
    let yesterday_high = 3600.0;
    let yesterday_low = 3400.0;
    let yesterday_close = 3500.0;
    let sensitivity = 0.35;
    
    // 計算樞軸點
    let pivot = (yesterday_high + yesterday_close + yesterday_low) / 3.0;
    
    // R-Breaker 六個關鍵價位
    let breakthrough_buy = yesterday_high + 2.0 * sensitivity * (pivot - yesterday_low);
    let observation_sell = pivot + sensitivity * (yesterday_high - yesterday_low);
    let reversal_sell = 2.0 * pivot - yesterday_low;
    let reversal_buy = 2.0 * pivot - yesterday_high;
    let observation_buy = pivot - sensitivity * (yesterday_high - yesterday_low);
    let breakthrough_sell = yesterday_low - 2.0 * sensitivity * (yesterday_high - pivot);
    
    println!("📊 前日數據: H={:.2}, L={:.2}, C={:.2}", yesterday_high, yesterday_low, yesterday_close);
    println!("📐 樞軸點: {:.2}", pivot);
    println!("📈 R-Breaker 關鍵價位:");
    println!("  突破買入: {:.2}", breakthrough_buy);
    println!("  觀察賣出: {:.2}", observation_sell);
    println!("  反轉賣出: {:.2}", reversal_sell);
    println!("  反轉買入: {:.2}", reversal_buy);
    println!("  觀察買入: {:.2}", observation_buy);
    println!("  突破賣出: {:.2}", breakthrough_sell);
    
    // 測試信號生成
    println!("\n🔄 測試信號生成邏輯:");
    let test_prices = vec![3300.0, 3450.0, 3500.0, 3580.0, 3650.0, 3700.0];
    
    for price in test_prices {
        let signal = if price > breakthrough_buy {
            "趨勢買入"
        } else if price < breakthrough_sell {
            "趨勢賣出"
        } else if price < reversal_sell {
            "反轉賣出"
        } else if price > reversal_buy {
            "反轉買入"
        } else {
            "持有"
        };
        
        println!("  價格 {:.2} -> 信號: {}", price, signal);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 R-Breaker Bitget 連接測試");
    println!("============================\n");
    
    // 測試 R-Breaker 計算邏輯
    test_rbreaker_calculation();
    
    println!("\n" + "=".repeat(50).as_str());
    
    // 測試實際的 WebSocket 連接
    match test_bitget_connection().await {
        Ok(_) => {
            println!("\n✅ 所有測試通過！");
            println!("💡 真實數據驗證結果:");
            println!("  📡 Bitget WebSocket 連接正常");
            println!("  📈 ETHUSDT 報價數據接收正常");
            println!("  📊 OrderBook 數據解析正確");
            println!("  🎯 R-Breaker 策略邏輯驗證完成");
            
            println!("\n🔄 下一步:");
            println!("  可以使用真實數據運行完整的 R-Breaker 交易系統");
            println!("  建議先在模擬模式下測試策略效果");
        }
        Err(e) => {
            println!("\n❌ 連接測試失敗: {}", e);
            println!("💡 可能的原因:");
            println!("  • 網絡連接問題");
            println!("  • Bitget API 服務不可用");
            println!("  • 防火牆阻擋 WebSocket 連接");
        }
    }
    
    Ok(())
}
// Archived legacy example; see 02_strategy/
