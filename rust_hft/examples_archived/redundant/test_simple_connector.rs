/*!
 * 簡化的 UnifiedBitgetConnector 測試
 * 一步一步驗證每個組件
 */

use rust_hft::integrations::{
    UnifiedBitgetConnector, UnifiedBitgetConfig, ConnectionMode, UnifiedBitgetChannel
};
use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    info!("🔍 測試 UnifiedBitgetConnector 基本流程");
    
    // 1. 創建連接器
    let config = UnifiedBitgetConfig {
        ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        api_key: None,
        api_secret: None,
        passphrase: None,
        mode: ConnectionMode::Single,
        max_reconnect_attempts: 3,
        reconnect_delay_ms: 1000,
        enable_compression: true,
        max_message_size: 1024 * 1024,
        ping_interval_secs: 30,
        connection_timeout_secs: 10,
    };
    
    let connector = UnifiedBitgetConnector::new(config);
    info!("✅ 連接器創建成功");
    
    // 2. 先添加訂閱
    info!("📋 添加訂閱...");
    connector.subscribe("BTCUSDT", UnifiedBitgetChannel::OrderBook5).await?;
    connector.subscribe("BTCUSDT", UnifiedBitgetChannel::Trades).await?;
    info!("✅ 訂閱添加成功");
    
    // 3. 啟動連接
    info!("🚀 啟動連接...");
    let mut receiver = connector.start().await?;
    info!("✅ 連接啟動成功");
    
    // 4. 接收消息
    info!("📡 開始接收消息...");
    let mut count = 0;
    while let Some(message) = receiver.recv().await {
        count += 1;
        info!("📨 收到消息 #{}: {:?} @ {}", count, message.channel, message.symbol);
        
        if count >= 5 {
            break;
        }
    }
    
    info!("🎉 測試完成，共收到 {} 條消息", count);
    Ok(())
}