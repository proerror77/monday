//! Hyperliquid 集成演示程序
//!
//! 演示如何使用 Hyperliquid 适配器进行交易

use hft_core::{HftResult, Symbol, Side, Price, Quantity, OrderType, TimeInForce};
use ports::{ExecutionClient, OrderIntent};
use adapter_hyperliquid_execution::{HyperliquidExecutionClient, HyperliquidExecutionConfig, ExecutionMode};
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error};

#[tokio::main]
async fn main() -> HftResult<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Hyperliquid 演示程序启动");

    // 演示 Paper 模式
    demo_paper_mode().await?;

    // 注意：Live 模式需要真实私钥，请谨慎使用
    // demo_live_mode().await?;

    info!("✅ 演示程序完成");
    Ok(())
}

async fn demo_paper_mode() -> HftResult<()> {
    info!("📝 开始 Paper 模式演示");

    // 创建 Paper 模式配置
    let config = HyperliquidExecutionConfig {
        mode: ExecutionMode::Paper,
        rest_base_url: "https://api.hyperliquid.xyz".to_string(),
        ws_private_url: "wss://api.hyperliquid.xyz/ws".to_string(),
        private_key: "".to_string(), // Paper 模式不需要私钥
        timeout_ms: 5000,
        vault_address: None,
    };

    let mut client = HyperliquidExecutionClient::new(config);

    // 连接到 Hyperliquid
    info!("🔌 连接到 Hyperliquid...");
    client.connect().await?;

    // 检查连接状态
    let health = client.health().await;
    info!("📊 连接状态: connected={}, latency={:?}ms",
          health.connected, health.latency_ms);

    // 创建一个测试订单
    let order_intent = OrderIntent {
        symbol: Symbol::new("BTC-PERP"),
        side: Side::Buy,
        quantity: Quantity::from_f64(0.001)?, // 0.001 BTC
        price: Some(Price::from_f64(50000.0)?), // $50,000
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        strategy_id: "demo_strategy".to_string(),
        target_venue: None,
    };

    info!("📋 下单测试: Buy 0.001 BTC-PERP @ $50,000");

    // 下单
    match client.place_order(order_intent).await {
        Ok(order_id) => {
            info!("✅ 下单成功，订单ID: {:?}", order_id);

            // 等待一会儿
            sleep(Duration::from_secs(2)).await;

            // 查询未结订单
            info!("🔍 查询未结订单...");
            match client.list_open_orders().await {
                Ok(orders) => {
                    info!("📊 未结订单数量: {}", orders.len());
                    for order in &orders {
                        info!("📃 订单: {:?} {} {} @ {:?}",
                              order.order_id, order.side, order.symbol, order.price);
                    }
                },
                Err(e) => warn!("❌ 查询未结订单失败: {}", e),
            }

            // 撤单测试
            info!("❌ 撤单测试...");
            match client.cancel_order(&order_id).await {
                Ok(_) => info!("✅ 撤单成功"),
                Err(e) => warn!("❌ 撤单失败: {}", e),
            }

            // 改单测试
            info!("📝 改单测试...");
            match client.modify_order(
                &order_id,
                Some(Quantity::from_f64(0.002)?),
                Some(Price::from_f64(51000.0)?)
            ).await {
                Ok(_) => info!("✅ 改单成功"),
                Err(e) => warn!("❌ 改单失败: {}", e),
            }

        },
        Err(e) => error!("❌ 下单失败: {}", e),
    }

    // 断开连接
    info!("🔌 断开连接...");
    client.disconnect().await?;

    info!("✅ Paper 模式演示完成");
    Ok(())
}

#[allow(dead_code)]
async fn demo_live_mode() -> HftResult<()> {
    warn!("⚠️  Live 模式演示 - 这将产生真实交易！");

    // 注意：这里需要您的真实私钥
    let config = HyperliquidExecutionConfig {
        mode: ExecutionMode::Live,
        rest_base_url: "https://api.hyperliquid.xyz".to_string(),
        ws_private_url: "wss://api.hyperliquid.xyz/ws".to_string(),
        private_key: "YOUR_PRIVATE_KEY_HERE".to_string(), // 替换为您的私钥
        timeout_ms: 5000,
        vault_address: None,
    };

    let mut client = HyperliquidExecutionClient::new(config);

    // 连接（如果私钥无效会失败）
    match client.connect().await {
        Ok(_) => {
            info!("✅ Live 模式连接成功");

            // 在这里可以执行真实交易操作
            // 请确保您了解风险！

            client.disconnect().await?;
        },
        Err(e) => {
            error!("❌ Live 模式连接失败: {}", e);
            info!("💡 请确保您已正确配置私钥");
        }
    }

    Ok(())
}

/// 账户配置指南
fn print_account_setup_guide() {
    println!("\n📚 Hyperliquid 账户配置指南:");
    println!("1. 访问 https://app.hyperliquid.xyz/");
    println!("2. 使用 MetaMask 连接钱包");
    println!("3. 完成 KYC 验证（如需要）");
    println!("4. 向账户充值 USDC");
    println!("5. 从 MetaMask 导出私钥");
    println!("6. 将私钥配置到 config/hyperliquid_live.yaml");
    println!("7. 启动交易系统");
    println!("\n⚠️  重要提醒:");
    println!("- 私钥是您资金的唯一凭证，请妥善保管");
    println!("- 建议先使用 Paper 模式测试");
    println!("- 设置合理的风控参数");
    println!("- 从小额资金开始");
}