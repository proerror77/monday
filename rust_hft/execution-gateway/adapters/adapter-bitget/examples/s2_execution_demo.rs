//! S2 Bitget 執行適配器演示
//!
//! 此示例展示：
//! 1. Bitget 執行客戶端的初始化和連接
//! 2. 模擬交易模式的下單操作
//! 3. 執行事件流的監聽

use execution_adapter_bitget::{BitgetExecutionClient, BitgetExecutionConfig, ExecutionMode};
use futures::StreamExt;
use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce};
use integration::signing::BitgetCredentials;
use ports::{ExecutionClient, OrderIntent};
use tokio::time::{sleep, Duration};
use tracing::{error, info};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌
    tracing_subscriber::fmt::init();

    info!("🚀 S2 Bitget 執行適配器演示開始");

    // 創建測試配置（模擬交易模式）
    let config = BitgetExecutionConfig {
        credentials: BitgetCredentials::new(
            "test_api_key".to_string(),
            "test_secret_key".to_string(),
            "test_passphrase".to_string(),
        ),
        mode: ExecutionMode::Paper, // 使用模擬交易模式
        timeout_ms: 3000,
        ..Default::default()
    };

    // 創建執行客戶端
    let mut client = BitgetExecutionClient::new(config)?;

    info!("✅ 執行客戶端創建成功");

    // 連接客戶端
    client.connect().await?;
    info!("✅ 客戶端連接成功");

    // 獲取執行事件流
    let mut event_stream = client.execution_stream().await?;

    // 在後台監聽執行事件
    let event_handle = tokio::spawn(async move {
        info!("📡 開始監聽執行事件...");

        while let Some(event_result) = event_stream.next().await {
            match event_result {
                Ok(event) => {
                    info!("📨 收到執行事件: {:?}", event);
                }
                Err(e) => {
                    error!("❌ 執行事件錯誤: {}", e);
                }
            }
        }

        info!("📡 執行事件流結束");
    });

    // 等待一小段時間讓事件流啟動
    sleep(Duration::from_millis(100)).await;

    // 創建測試訂單意圖
    let order_intents = vec![
        OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001)?,
            order_type: OrderType::Limit,
            price: Some(Price::from_f64(50000.0)?),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
        },
        OrderIntent {
            symbol: Symbol::new("ETHUSDT"),
            side: Side::Sell,
            quantity: Quantity::from_f64(0.01)?,
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: "test_strategy".to_string(),
        },
    ];

    info!("💼 開始測試下單流程...");

    // 執行測試訂單
    for (i, intent) in order_intents.iter().enumerate() {
        info!(
            "📋 訂單 #{}: {} {} {} @ {:?}",
            i + 1,
            intent.symbol.as_str(),
            match intent.side {
                Side::Buy => "買入",
                Side::Sell => "賣出",
            },
            intent.quantity.0,
            intent.price.map(|p| p.0)
        );

        match client.place_order(intent.clone()).await {
            Ok(order_id) => {
                info!("✅ 下單成功，訂單ID: {}", order_id.0);

                // 等待一小段時間再下一筆訂單
                sleep(Duration::from_millis(500)).await;
            }
            Err(e) => {
                error!("❌ 下單失敗: {}", e);
            }
        }
    }

    // 測試健康檢查
    let health = client.health().await;
    info!(
        "🏥 健康狀態: 連接={}, 延遲={:?}ms, 最後心跳={}",
        health.connected, health.latency_ms, health.last_heartbeat
    );

    // 等待更多事件
    info!("⏱️  等待更多事件...");
    sleep(Duration::from_secs(2)).await;

    // 斷開連接
    client.disconnect().await?;
    info!("✅ 客戶端斷開連接");

    // 等待事件處理完成
    event_handle.abort();

    info!("🎉 S2 執行適配器演示完成");

    Ok(())
}
