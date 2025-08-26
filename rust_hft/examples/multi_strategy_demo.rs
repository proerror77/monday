//! 多策略引擎演示 - 展示基本功能和架构

use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

use rust_hft::core::types::AccountId;
use rust_hft::engine::{
    multi_strategy_engine::{MultiStrategyConfig, MultiStrategyEngine},
    CompleteOMS,
};
use rust_hft::exchanges::{ExchangeEventHub, ExchangeManager};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();

    info!("🚀 Multi-Strategy Engine Demo Starting");

    // 1. 创建基础设施
    let exchange_manager = Arc::new(ExchangeManager::new());
    let oms = Arc::new(CompleteOMS::new(exchange_manager));
    let event_hub = Arc::new(ExchangeEventHub::new());

    // 2. 配置多策略引擎
    let config = MultiStrategyConfig::default();
    let engine = MultiStrategyEngine::new(oms.clone(), event_hub.clone(), config);

    // 3. 添加测试账户
    oms.add_account(AccountId::new("demo", "account1"), 10000.0)
        .await?;

    // 4. 启动引擎
    engine.start().await?;
    info!("✅ Multi-Strategy Engine started");

    // 5. 等待一段时间观察运行状态
    sleep(Duration::from_secs(2)).await;

    // 6. 检查状态
    let global_stats = engine.get_global_stats().await;
    info!(
        "📊 Global Stats: {} strategies",
        global_stats.total_strategies
    );

    // 7. 停止引擎
    engine.stop().await?;
    info!("✅ Multi-Strategy Engine stopped");

    info!("🎉 Demo completed successfully");
    Ok(())
}
