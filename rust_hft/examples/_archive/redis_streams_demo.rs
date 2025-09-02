//! Redis Streams 控制面演示
//!
//! 展示如何使用 Redis Streams 進行控制事件的發布和消費

use anyhow::Result;
use rust_hft::integrations::redis_streams_controller::{
    ControlEvent, RedisStreamsController, StreamChannels,
};
use tokio::time::{sleep, Duration};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::init();

    info!("🚀 Redis Streams 控制面演示開始");

    // 創建 Producer 控制器
    let producer = RedisStreamsController::new(
        "redis://localhost:6379",
        "hft-control-group".to_string(),
        "producer-demo".to_string(),
    )
    .await?;

    // 創建 Consumer 控制器
    let consumer = RedisStreamsController::new(
        "redis://localhost:6379",
        "hft-control-group".to_string(),
        "consumer-demo".to_string(),
    )
    .await?;

    let channels = StreamChannels::default();

    // 初始化 Streams
    producer.initialize_streams(&channels).await?;
    consumer.initialize_streams(&channels).await?;

    // 啟動消費者
    consumer.start_consumer(channels.clone()).await?;

    // 訂閱事件
    let mut event_receiver = consumer.subscribe_events();

    // 消費者任務
    let consumer_task = tokio::spawn(async move {
        while let Ok(event) = event_receiver.recv().await {
            match event {
                ControlEvent::MlDeploy {
                    version,
                    ic,
                    ir,
                    mdd,
                    ..
                } => {
                    info!(
                        "🤖 收到 ML 部署事件: {} (IC: {:.3}, IR: {:.2}, MDD: {:.3})",
                        version, ic, ir, mdd
                    );
                }
                ControlEvent::OpsAlert {
                    alert_type,
                    value,
                    p99,
                    ..
                } => {
                    info!(
                        "⚠️  收到運營告警: {} = {:.2} (p99: {:?})",
                        alert_type, value, p99
                    );
                }
                ControlEvent::KillSwitch { cause, .. } => {
                    info!("🛑 收到 Kill Switch: {}", cause);
                }
                ControlEvent::MlReject { reason, trials, .. } => {
                    info!("❌ ML 模型被拒絕: {} (trials: {})", reason, trials);
                }
            }
        }
    });

    // 等待一下讓消費者準備好
    sleep(Duration::from_millis(500)).await;

    // 演示發布不同類型的事件
    info!("📤 開始發布控制事件...");

    // 1. ML 部署事件
    producer
        .publish_ml_deploy(
            "supabase://models/20250815/lstm_v2.pt".to_string(),
            "sha256abc123def456".to_string(),
            "v2.1.5-ic0042-ir135".to_string(),
            0.042,
            1.35,
            0.038,
            "lstm".to_string(),
        )
        .await?;

    sleep(Duration::from_millis(100)).await;

    // 2. 延遲告警事件
    producer
        .publish_ops_alert("latency".to_string(), 45.2, Some(23.8))
        .await?;

    sleep(Duration::from_millis(100)).await;

    // 3. 回撤告警事件
    producer
        .publish_ops_alert(
            "dd".to_string(),
            0.032, // 3.2% 回撤
            None,
        )
        .await?;

    sleep(Duration::from_millis(100)).await;

    // 4. ML 拒絕事件
    let reject_event = ControlEvent::MlReject {
        reason: "IC too low: 0.015 < 0.030 threshold".to_string(),
        trials: 25,
        timestamp: chrono::Utc::now().timestamp() as u64,
    };
    producer.publish_event("ml.reject", reject_event).await?;

    sleep(Duration::from_millis(100)).await;

    // 5. Kill Switch 事件
    producer
        .publish_kill_switch("Max drawdown exceeded: 5.2% > 5.0% limit".to_string())
        .await?;

    info!("✅ 所有事件已發布，等待消費完成...");

    // 等待消費者處理事件
    sleep(Duration::from_secs(2)).await;

    // 停止消費者任務
    consumer_task.abort();

    info!("🏁 Redis Streams 控制面演示完成");

    Ok(())
}

/// 額外演示：模擬 ML Agent 的工作流程
#[allow(dead_code)]
async fn simulate_ml_workflow() -> Result<()> {
    let controller = RedisStreamsController::new(
        "redis://localhost:6379",
        "ml-workflow".to_string(),
        "ml-agent".to_string(),
    )
    .await?;

    let channels = StreamChannels::default();
    controller.initialize_streams(&channels).await?;

    // 模擬訓練失敗 → 重試 → 成功的流程
    info!("🧠 模擬 ML 訓練工作流程...");

    // 第一次嘗試失敗
    let reject_event = ControlEvent::MlReject {
        reason: "Training diverged after 15 epochs".to_string(),
        trials: 1,
        timestamp: chrono::Utc::now().timestamp() as u64,
    };
    controller.publish_event("ml.reject", reject_event).await?;

    sleep(Duration::from_millis(200)).await;

    // 第二次嘗試失敗
    let reject_event = ControlEvent::MlReject {
        reason: "Validation loss plateau, IC < threshold".to_string(),
        trials: 2,
        timestamp: chrono::Utc::now().timestamp() as u64,
    };
    controller.publish_event("ml.reject", reject_event).await?;

    sleep(Duration::from_millis(200)).await;

    // 第三次嘗試成功
    controller
        .publish_ml_deploy(
            "supabase://models/20250815/final_model.pt".to_string(),
            "sha256def789abc123".to_string(),
            "v2.1.6-ic0035-ir142".to_string(),
            0.035,
            1.42,
            0.029,
            "transformer".to_string(),
        )
        .await?;

    info!("✅ ML 工作流程模擬完成");

    Ok(())
}

/// 演示：模擬運營告警處理流程
#[allow(dead_code)]
async fn simulate_ops_workflow() -> Result<()> {
    let controller = RedisStreamsController::new(
        "redis://localhost:6379",
        "ops-workflow".to_string(),
        "ops-agent".to_string(),
    )
    .await?;

    let channels = StreamChannels::default();
    controller.initialize_streams(&channels).await?;

    info!("🔧 模擬運營告警處理流程...");

    // 延遲升高告警
    controller
        .publish_ops_alert("latency".to_string(), 35.5, Some(25.0))
        .await?;

    sleep(Duration::from_millis(100)).await;

    // 延遲進一步升高
    controller
        .publish_ops_alert("latency".to_string(), 52.3, Some(25.0))
        .await?;

    sleep(Duration::from_millis(100)).await;

    // 觸發 Kill Switch
    controller
        .publish_kill_switch("Latency p99 exceeded 2x threshold: 52.3ms > 50ms".to_string())
        .await?;

    info!("✅ 運營告警流程模擬完成");

    Ok(())
}
// Archived legacy example
