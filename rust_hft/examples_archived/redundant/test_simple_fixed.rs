/*!
 * 測試修復後的簡化連接器
 */

use rust_hft::integrations::{SimpleBitgetConnector, SimpleChannel};
use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    info!("🔍 測試簡化 Bitget 連接器");
    
    // 創建連接器
    let connector = SimpleBitgetConnector::new(None);
    info!("✅ 連接器創建成功");
    
    // 添加訂閱
    connector.subscribe("BTCUSDT", SimpleChannel::OrderBook5).await?;
    connector.subscribe("BTCUSDT", SimpleChannel::Trade).await?;
    info!("✅ 訂閱添加成功");
    
    // 啟動連接
    let mut receiver = connector.start().await?;
    info!("✅ 連接啟動成功");
    
    // 接收消息
    info!("📡 開始接收消息...");
    let mut count = 0;
    let start_time = std::time::Instant::now();
    
    while let Some(message) = receiver.recv().await {
        count += 1;
        info!("📨 #{}: {:?} @ {} (數據長度: {})", 
              count, message.channel, message.symbol, 
              message.data.to_string().len());
        
        if count >= 10 {
            break;
        }
        
        // 10秒超時
        if start_time.elapsed().as_secs() > 10 {
            info!("⏰ 10秒超時，結束測試");
            break;
        }
    }
    
    info!("🎉 測試完成，共收到 {} 條消息", count);
    
    // 停止連接器
    connector.stop().await?;
    
    Ok(())
}