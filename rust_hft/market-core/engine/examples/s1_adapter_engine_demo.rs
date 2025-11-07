//! S1 階段演示：Adapter→Engine 有界匯流對接（Bitget 骨架）
//! - 使用 hft-data-adapter-bitget 的骨架 MarketStream
//! - 通過 AdapterBridge 建立 SPSC ring 匯流
//! - 啟動 Engine 主循環並觀察空閒運轉（因為適配器目前輸出空流）

use engine::{AdapterBridge, AdapterBridgeConfig, Engine, EngineConfig};
use hft_core::Symbol;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1) 建立引擎與橋接器
    let engine_cfg = EngineConfig::default();
    let mut engine = Engine::new(engine_cfg);

    let bridge_cfg = AdapterBridgeConfig::default();
    let mut bridge = AdapterBridge::new(bridge_cfg);

    // 2) 構建 Bitget 行情適配器（骨架）並橋接
    let bitget = data_adapter_bitget::BitgetMarketStream::new();
    let symbols = vec![Symbol::new("BTCUSDT")];
    let consumer = bridge.bridge_stream(bitget, symbols).await.unwrap();
    engine.register_event_consumer(consumer);

    // 3) 啟動引擎主循環（骨架示例：3 秒後停止）
    let mut handle_engine = engine; // move into task
    let engine_task = tokio::spawn(async move {
        // 運行 3 秒
        let stop_after = std::time::Duration::from_secs(3);
        let start = std::time::Instant::now();
        while start.elapsed() < stop_after {
            // 單次 tick（非阻塞）
            let _ = handle_engine.tick();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        handle_engine.stop();
    });

    engine_task.await?;
    Ok(())
}
