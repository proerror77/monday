use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("🔍 直接測試 Bitget WebSocket 連接...");
    
    // 測試 URL
    let url = "wss://ws.bitget.com/v2/ws/public";
    println!("連接到: {}", url);
    
    // 建立連接
    match connect_async(url).await {
        Ok((ws_stream, response)) => {
            println!("✅ 連接成功! Status: {:?}", response.status());
            
            let (mut write, mut read) = ws_stream.split();
            
            // 訂閱消息
            let subscribe_msg = json!({
                "op": "subscribe",
                "args": [{
                    "instType": "SPOT",
                    "channel": "books5", 
                    "instId": "BTCUSDT"
                }]
            });
            
            println!("📤 發送訂閱消息: {}", subscribe_msg);
            write.send(Message::Text(subscribe_msg.to_string())).await?;
            
            // 讀取消息
            let mut count = 0;
            while let Some(msg) = read.next().await {
                count += 1;
                match msg? {
                    Message::Text(text) => {
                        println!("📨 收到消息 #{}: {}", count, &text[..std::cmp::min(200, text.len())]);
                        if count >= 5 {
                            break;
                        }
                    }
                    Message::Ping(data) => {
                        println!("🏓 Ping: {:?}", data);
                        write.send(Message::Pong(data)).await?;
                    }
                    Message::Pong(_) => {
                        println!("🏓 Pong");
                    }
                    _ => {
                        println!("❓ 其他消息類型");
                    }
                }
            }
        }
        Err(e) => {
            println!("❌ 連接失敗: {}", e);
        }
    }
    
    Ok(())
}