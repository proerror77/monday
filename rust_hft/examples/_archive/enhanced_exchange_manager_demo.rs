//! 增強版交易所管理器演示
//!
//! 展示智能路由、健康檢查、故障轉移和多賬戶支持功能

use rust_hft::exchanges::{
    binance::BinanceExchange, bitget::BitgetExchange, ExchangeInstanceConfig, ExchangeManager,
    FailoverConfig, RoutingStrategy,
};
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::init();

    info!("=== 增強版交易所管理器演示 ===");

    // 創建自定義配置的管理器
    let failover_config = FailoverConfig {
        health_threshold: 0.7,
        latency_threshold: 100.0,
        error_rate_threshold: 0.03,
        heartbeat_timeout: 30,
        reconnect_interval: 10,
        max_reconnect_attempts: 3,
    };

    let manager = ExchangeManager::with_config(RoutingStrategy::Composite, failover_config);

    info!(
        "✅ 創建交易所管理器，路由策略: {:?}",
        RoutingStrategy::Composite
    );

    // 添加多個 Bitget 實例 (模擬多賬戶)
    add_bitget_instances(&manager).await?;

    // 添加 Binance 實例
    add_binance_instances(&manager).await?;

    // 列出所有實例
    list_all_instances(&manager).await;

    // 啟動健康監控
    manager.start_monitoring().await?;
    info!("✅ 啟動健康監控");

    // 演示智能路由
    demo_smart_routing(&manager).await;

    // 演示健康度指標記錄
    demo_health_metrics(&manager).await;

    // 演示故障轉移
    demo_failover(&manager).await;

    // 演示重連功能
    demo_reconnection(&manager).await;

    // 獲取健康狀態
    demo_health_status(&manager).await;

    // 清理
    manager.stop_monitoring().await;
    info!("✅ 停止監控，演示完成");

    Ok(())
}

async fn add_bitget_instances(manager: &ExchangeManager) -> Result<(), Box<dyn std::error::Error>> {
    info!("添加 Bitget 實例...");

    // 主賬戶
    let config1 = ExchangeInstanceConfig {
        name: "bitget".to_string(),
        account_id: "main_account".to_string(),
        api_key: "main_api_key".to_string(),
        secret_key: "main_secret".to_string(),
        passphrase: Some("main_passphrase".to_string()),
        testnet: false,
        enabled: true,
        priority: 1, // 最高優先級
    };

    // 備用賬戶
    let config2 = ExchangeInstanceConfig {
        name: "bitget".to_string(),
        account_id: "backup_account".to_string(),
        api_key: "backup_api_key".to_string(),
        secret_key: "backup_secret".to_string(),
        passphrase: Some("backup_passphrase".to_string()),
        testnet: false,
        enabled: true,
        priority: 2,
    };

    // 測試賬戶
    let config3 = ExchangeInstanceConfig {
        name: "bitget".to_string(),
        account_id: "test_account".to_string(),
        api_key: "test_api_key".to_string(),
        secret_key: "test_secret".to_string(),
        passphrase: Some("test_passphrase".to_string()),
        testnet: true,
        enabled: true,
        priority: 3,
    };

    manager
        .add_exchange_instance(config1, Box::new(BitgetExchange::new()))
        .await?;
    manager
        .add_exchange_instance(config2, Box::new(BitgetExchange::new()))
        .await?;
    manager
        .add_exchange_instance(config3, Box::new(BitgetExchange::new()))
        .await?;

    info!("✅ 添加了 3 個 Bitget 實例");
    Ok(())
}

async fn add_binance_instances(
    manager: &ExchangeManager,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("添加 Binance 實例...");

    let config = ExchangeInstanceConfig {
        name: "binance".to_string(),
        account_id: "binance_main".to_string(),
        api_key: "binance_api_key".to_string(),
        secret_key: "binance_secret".to_string(),
        passphrase: None,
        testnet: false,
        enabled: true,
        priority: 1,
    };

    manager
        .add_exchange_instance(config, Box::new(BinanceExchange::new()))
        .await?;

    info!("✅ 添加了 1 個 Binance 實例");
    Ok(())
}

async fn list_all_instances(manager: &ExchangeManager) {
    info!("=== 實例列表 ===");

    let exchanges = manager.list_exchanges().await;
    info!("交易所: {:?}", exchanges);

    let instances = manager.list_exchange_instances().await;
    for (exchange_name, instance_list) in instances {
        info!("交易所: {}", exchange_name);
        for (account_id, enabled, health_score) in instance_list {
            info!(
                "  └─ 賬戶: {}, 啟用: {}, 健康度: {:.3}",
                account_id, enabled, health_score
            );
        }
    }
}

async fn demo_smart_routing(manager: &ExchangeManager) {
    info!("=== 智能路由演示 ===");

    // 多次選擇，觀察路由策略
    for i in 1..=5 {
        if let Some(exchange) = manager.select_best_exchange("bitget").await {
            info!("第 {} 次選擇: 獲得 Bitget 實例", i);
        } else {
            warn!("第 {} 次選擇: 無可用實例", i);
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn demo_health_metrics(manager: &ExchangeManager) {
    info!("=== 健康度指標演示 ===");

    // 模擬記錄延遲
    manager.record_latency("bitget", "main_account", 25.5).await;
    manager.record_latency("bitget", "main_account", 31.2).await;
    manager
        .record_latency("bitget", "backup_account", 45.8)
        .await;

    info!("✅ 記錄了延遲指標");

    // 模擬記錄錯誤
    manager
        .record_error("bitget", "main_account", "connection_timeout")
        .await;
    manager
        .record_error("bitget", "backup_account", "rate_limit")
        .await;

    info!("✅ 記錄了錯誤指標");

    sleep(Duration::from_secs(2)).await;
}

async fn demo_failover(manager: &ExchangeManager) {
    info!("=== 故障轉移演示 ===");

    // 禁用一個實例來模擬故障
    if let Err(e) = manager
        .set_instance_enabled("bitget", "main_account", false)
        .await
    {
        error!("禁用實例失敗: {}", e);
    } else {
        info!("✅ 禁用主賬戶，測試故障轉移");
    }

    // 再次選擇，應該路由到其他實例
    if let Some(_) = manager.select_best_exchange("bitget").await {
        info!("✅ 故障轉移成功，路由到備用實例");
    } else {
        warn!("❌ 故障轉移失敗");
    }

    // 重新啟用
    if let Err(e) = manager
        .set_instance_enabled("bitget", "main_account", true)
        .await
    {
        error!("重新啟用實例失敗: {}", e);
    } else {
        info!("✅ 重新啟用主賬戶");
    }
}

async fn demo_reconnection(manager: &ExchangeManager) {
    info!("=== 重連機制演示 ===");

    // 手動觸發重連
    match manager.reconnect_instance("bitget", "main_account").await {
        Ok(_) => info!("✅ 觸發重連任務"),
        Err(e) => warn!("觸發重連失敗: {}", e),
    }

    // 等待重連完成
    sleep(Duration::from_secs(3)).await;

    // 檢查重連狀態
    let status = manager.get_reconnect_status().await;
    for (instance_key, (attempts, is_reconnecting)) in status {
        info!(
            "實例 {}: 重連次數 {}, 正在重連: {}",
            instance_key, attempts, is_reconnecting
        );
    }
}

async fn demo_health_status(manager: &ExchangeManager) {
    info!("=== 健康狀態檢查 ===");

    let healthy_exchanges = manager.get_healthy_exchanges().await;
    info!("健康的交易所: {:?}", healthy_exchanges);

    // 訂閱健康狀態變化 (演示用，實際使用中會持續監聽)
    let mut health_receiver = manager.subscribe_health_changes();

    tokio::spawn(async move {
        // 模擬監聽健康狀態變化
        for _ in 0..3 {
            match health_receiver.recv().await {
                Ok((exchange, account, metrics)) => {
                    info!(
                        "健康狀態更新: {}:{} - 可用性: {:.3}, 延遲: {:.2}ms, 錯誤率: {:.3}%",
                        exchange,
                        account,
                        metrics.availability_score,
                        metrics.latency_ewma,
                        metrics.error_rate * 100.0
                    );
                }
                Err(_) => break,
            }
        }
    });

    sleep(Duration::from_secs(2)).await;
}
// Archived legacy example
