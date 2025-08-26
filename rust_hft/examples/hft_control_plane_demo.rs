//! HFT 控制面完整演示
//!
//! 展示 ArcSwap 兩層快照 + Redis Streams 控制面的完整集成

use anyhow::Result;
use rust_hft::exchanges::{ExchangeManager, FailoverConfig, RoutingStrategy};
use rust_hft::integrations::redis_streams_controller::{
    ControlEvent, RedisStreamsController, StreamChannels,
};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::init();

    info!("🚀 HFT 控制面完整演示開始");

    // 1. 創建 Redis Streams 控制器
    let redis_controller = Arc::new(
        RedisStreamsController::new(
            "redis://localhost:6379",
            "hft-production".to_string(),
            "hft-engine".to_string(),
        )
        .await?,
    );

    let channels = StreamChannels::default();
    redis_controller.initialize_streams(&channels).await?;
    redis_controller.start_consumer(channels.clone()).await?;

    // 2. 創建 ExchangeManager 並集成 Redis Streams
    let mut exchange_manager =
        ExchangeManager::with_config(RoutingStrategy::Composite, FailoverConfig::default());

    exchange_manager.set_redis_controller(redis_controller.clone());
    let exchange_manager = Arc::new(exchange_manager);

    info!("✅ HFT 系統初始化完成");

    // 3. 訂閱控制事件並啟動事件處理
    if let Some(mut event_receiver) = exchange_manager.subscribe_control_events() {
        let exchange_manager_clone = exchange_manager.clone();

        tokio::spawn(async move {
            while let Ok(event) = event_receiver.recv().await {
                handle_control_event(&exchange_manager_clone, event).await;
            }
        });
    }

    // 4. 模擬 HFT 系統運行過程中的各種場景
    info!("📊 開始模擬 HFT 系統運行場景...");

    // 場景 1: 正常運行，偶爾的延遲波動
    simulate_normal_operation(&exchange_manager).await?;

    // 場景 2: 延遲升高，觸發告警
    simulate_latency_spike(&exchange_manager).await?;

    // 場景 3: ML 模型更新流程
    simulate_ml_model_update().await?;

    // 場景 4: 嚴重問題，觸發 Kill Switch
    simulate_critical_failure(&exchange_manager).await?;

    info!("✅ 所有場景模擬完成，等待事件處理...");
    sleep(Duration::from_secs(3)).await;

    info!("🏁 HFT 控制面演示完成");

    Ok(())
}

/// 處理控制事件
async fn handle_control_event(exchange_manager: &Arc<ExchangeManager>, event: ControlEvent) {
    match event {
        ControlEvent::MlDeploy {
            version,
            ic,
            ir,
            mdd,
            ..
        } => {
            info!(
                "🤖 處理 ML 模型部署: {} (IC: {:.3}, IR: {:.2}, MDD: {:.3})",
                version, ic, ir, mdd
            );
            // 在實際系統中，這裡會觸發模型熱加載
            // exchange_manager.load_model(url, version).await;
        }

        ControlEvent::OpsAlert {
            alert_type,
            value,
            p99,
            ..
        } => {
            match alert_type.as_str() {
                "latency" => {
                    if let Some(p99_value) = p99 {
                        if value > p99_value * 2.0 {
                            warn!(
                                "🚨 嚴重延遲告警: {:.1}ms > {:.1}ms (2x p99)",
                                value,
                                p99_value * 2.0
                            );
                            // 觸發降頻操作
                            // exchange_manager.throttle_trading().await;
                        } else {
                            info!("⚠️  延遲告警: {:.1}ms (p99: {:.1}ms)", value, p99_value);
                        }
                    }
                }
                "dd" => {
                    if value > 0.05 {
                        // 5% 回撤
                        error!("🛑 嚴重回撤告警: {:.1}% > 5%", value * 100.0);
                        // 可能觸發 Kill Switch
                        if let Err(e) = exchange_manager
                            .publish_ops_alert("kill_switch", value, None)
                            .await
                        {
                            error!("發布 Kill Switch 失敗: {}", e);
                        }
                    } else {
                        warn!("📉 回撤告警: {:.1}%", value * 100.0);
                    }
                }
                _ => {
                    info!("ℹ️  其他告警: {} = {:.2}", alert_type, value);
                }
            }
        }

        ControlEvent::KillSwitch { cause, .. } => {
            error!("🚨 Kill Switch 激活: {}", cause);
            // 在實際系統中，這裡會停止所有交易
            // exchange_manager.emergency_stop().await;

            // 模擬等待人工干預
            info!("⏸️  系統已暫停，等待人工干預...");
        }

        ControlEvent::MlReject { reason, trials, .. } => {
            warn!("❌ ML 模型訓練失敗 (第 {} 次): {}", trials, reason);
            // 如果失敗次數過多，可能需要人工干預
            if trials >= 5 {
                error!("🚨 ML 訓練連續失敗 {} 次，需要人工檢查", trials);
            }
        }
    }
}

/// 模擬正常運行場景
async fn simulate_normal_operation(exchange_manager: &Arc<ExchangeManager>) -> Result<()> {
    info!("📈 模擬正常運行場景...");

    // 模擬正常的延遲波動
    for i in 1..=5 {
        let latency = 15.0 + (i as f64) * 2.0; // 15-25ms
        exchange_manager
            .publish_ops_alert("latency", latency, Some(20.0))
            .await?;
        sleep(Duration::from_millis(200)).await;
    }

    info!("✅ 正常運行場景完成");
    Ok(())
}

/// 模擬延遲峰值場景
async fn simulate_latency_spike(exchange_manager: &Arc<ExchangeManager>) -> Result<()> {
    info!("📊 模擬延遲峰值場景...");

    // 逐漸升高的延遲
    let latencies = [25.0, 35.0, 45.0, 60.0, 75.0]; // 超過 2x p99 (40ms)

    for latency in latencies {
        exchange_manager
            .publish_ops_alert("latency", latency, Some(20.0))
            .await?;
        sleep(Duration::from_millis(300)).await;
    }

    info!("✅ 延遲峰值場景完成");
    Ok(())
}

/// 模擬 ML 模型更新流程
async fn simulate_ml_model_update() -> Result<()> {
    info!("🧠 模擬 ML 模型更新流程...");

    let controller = RedisStreamsController::new(
        "redis://localhost:6379",
        "ml-training".to_string(),
        "ml-simulator".to_string(),
    )
    .await?;

    // 模擬訓練過程：失敗 -> 重試 -> 成功

    // 第一次失敗
    let reject_event = ControlEvent::MlReject {
        reason: "IC below threshold: 0.022 < 0.030".to_string(),
        trials: 1,
        timestamp: chrono::Utc::now().timestamp() as u64,
    };
    controller.publish_event("ml.reject", reject_event).await?;

    sleep(Duration::from_millis(500)).await;

    // 第二次失敗
    let reject_event = ControlEvent::MlReject {
        reason: "Validation loss diverged".to_string(),
        trials: 2,
        timestamp: chrono::Utc::now().timestamp() as u64,
    };
    controller.publish_event("ml.reject", reject_event).await?;

    sleep(Duration::from_millis(500)).await;

    // 第三次成功
    controller
        .publish_ml_deploy(
            "supabase://models/20250815/transformer_v3.pt".to_string(),
            "sha256:abcd1234ef567890".to_string(),
            "v3.2.1-ic0038-ir145".to_string(),
            0.038,
            1.45,
            0.025,
            "transformer".to_string(),
        )
        .await?;

    info!("✅ ML 模型更新流程完成");
    Ok(())
}

/// 模擬嚴重故障場景
async fn simulate_critical_failure(exchange_manager: &Arc<ExchangeManager>) -> Result<()> {
    info!("🚨 模擬嚴重故障場景...");

    // 嚴重回撤
    exchange_manager
        .publish_ops_alert("dd", 0.062, None)
        .await?; // 6.2% 回撤

    sleep(Duration::from_millis(200)).await;

    // 系統延遲失控
    exchange_manager
        .publish_ops_alert("latency", 150.0, Some(20.0))
        .await?; // 150ms

    sleep(Duration::from_millis(200)).await;

    // 觸發 Kill Switch
    let controller = RedisStreamsController::new(
        "redis://localhost:6379",
        "emergency".to_string(),
        "risk-manager".to_string(),
    )
    .await?;

    controller
        .publish_kill_switch(
            "Multiple critical failures: DD 6.2% > 5%, Latency 150ms > 40ms".to_string(),
        )
        .await?;

    info!("✅ 嚴重故障場景完成");
    Ok(())
}

/// 展示系統指標和統計
#[allow(dead_code)]
async fn display_system_metrics(exchange_manager: &Arc<ExchangeManager>) -> Result<()> {
    info!("📊 系統指標：");

    // 獲取健康的交易所
    let healthy_exchanges = exchange_manager.get_healthy_exchanges().await;
    info!("  健康交易所數量: {}", healthy_exchanges.len());

    // 獲取所有交易所
    let all_exchanges = exchange_manager.list_exchanges().await;
    info!("  總交易所數量: {}", all_exchanges.len());

    // 重連狀態
    let reconnect_status = exchange_manager.get_reconnect_status();
    info!(
        "  活躍重連數量: {}",
        reconnect_status.values().filter(|&&v| v).count()
    );

    // Redis 控制事件訂閱狀態
    if exchange_manager.subscribe_control_events().is_some() {
        info!("  Redis Streams 控制面: ✅ 已連接");
    } else {
        info!("  Redis Streams 控制面: ❌ 未配置");
    }

    Ok(())
}
