/*!
 * 簡化編譯測試
 */

use anyhow::Result;
use tracing::info;

use rust_hft::core::monitoring::MonitoringServer;
use rust_hft::integrations::redis_bridge::RedisBridge;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("rust_hft=info")
        .init();

    info!("🧪 編譯測試開始");
    
    // 測試監控服務
    let _monitoring = MonitoringServer::new(9090).await?;
    info!("✅ 監控服務編譯成功");
    
    // 測試Redis橋接
    let _redis_bridge = RedisBridge::new().await?;
    info!("✅ Redis橋接編譯成功");
    
    info!("🎉 所有核心組件編譯成功");
    
    Ok(())
}