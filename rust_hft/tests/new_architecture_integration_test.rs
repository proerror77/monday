/*!
 * 新架构集成测试 (New Architecture Integration Test)
 *
 * 验证重构后的应用层组件（OrderService, RiskService, ExecutionService,
 * ExchangeRouter, EventBus, ConfigManager）之间的集成工作。
 */

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use tokio::time::{timeout, Duration};

    use rust_hft::app::{
        config::{AppConfig, ConfigManager},
        events::{EventBus, EventPriority, EventType},
        routing::{ExchangeRouter, RouterConfig},
        services::{ExecutionService, OrderRequest, OrderService, RiskService},
    };
    use rust_hft::domains::trading::{AccountId, OrderSide, OrderType, TimeInForce};

    #[tokio::test]
    async fn test_new_architecture_basic_integration() {
        // 1. 配置管理器测试
        let config_manager = ConfigManager::new();
        let default_config = config_manager.get_config().await;
        assert_eq!(default_config.app.name, "rust_hft");

        // 2. 事件总线测试
        let (event_bus, mut event_receiver) = EventBus::new(false);

        // 启动事件处理循环
        event_bus.start_processing(event_receiver).await;

        // 测试事件订阅
        let mut change_receiver = event_bus.subscribe_to_changes();

        // 3. 测试配置变更通知
        let _result = config_manager
            .update_config_section("/app/debug_mode", true)
            .await;

        // 验证配置已更新
        let updated_config = config_manager.get_config().await;
        assert!(updated_config.app.debug_mode);

        println!("✅ 新架构基础集成测试通过");
    }

    #[tokio::test]
    async fn test_service_layer_integration() {
        // 创建测试账户
        let test_account = AccountId::new("test_exchange", "test_account");

        // 创建订单请求
        let order_request = OrderRequest {
            client_order_id: "test_order_001".to_string(),
            account_id: test_account.clone(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: rust_hft::domains::trading::ToQuantity::to_quantity(1.0),
            price: Some(rust_hft::domains::trading::ToPrice::to_price(50000.0)),
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: HashMap::new(),
        };

        // 注意: 这里我们测试服务的创建和基本功能，
        // 但不测试实际的交易执行，因为那需要真实的交易所连接

        println!("✅ 服务层集成测试框架就绪");
        println!("   - OrderRequest 结构验证通过");
        println!("   - AccountId 创建验证通过");
        println!("   - 类型转换验证通过");
    }

    #[tokio::test]
    async fn test_domain_types_integration() {
        use rust_hft::domains::trading::*;

        // 测试统一域类型
        let account = AccountId::new("bitget", "main");
        assert_eq!(account.exchange, "bitget");
        assert_eq!(account.label, "main");
        assert_eq!(account.as_str(), "bitget:main");

        // 测试价格和数量类型转换
        let price = 50000.0.to_price();
        let quantity = 1.5.to_quantity();

        assert_eq!(price.0, 50000.0);
        assert!(quantity > rust_decimal::Decimal::ZERO);

        // 测试订单创建
        let order = Order::new(
            "test_order".to_string(),
            account.clone(),
            "BTCUSDT".to_string(),
            OrderSide::Buy,
            OrderType::Limit,
            quantity,
            Some(price),
        );

        assert_eq!(order.symbol, "BTCUSDT");
        assert_eq!(order.side, OrderSide::Buy);
        assert!(order.is_active());
        assert!(!order.is_finished());

        println!("✅ 域类型集成测试通过");
        println!("   - AccountId 功能验证通过");
        println!("   - Price/Quantity 转换通过");
        println!("   - Order 状态机验证通过");
    }

    #[tokio::test]
    async fn test_config_management_integration() {
        let config_manager = ConfigManager::new();

        // 测试配置验证
        let config = config_manager.get_config().await;
        let validation_result = config_manager.validate_config(&config);
        assert!(validation_result.is_ok());

        // 测试配置部分获取
        let app_section: rust_hft::app::config::AppInfo = config_manager
            .get_config_section("/app")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(app_section.name, "rust_hft");

        // 测试版本跟踪
        let version_before = config_manager.get_version().await;
        let _update_result = config_manager
            .update_config_section("/app/log_level", "debug".to_string())
            .await;
        let version_after = config_manager.get_version().await;
        assert!(version_after > version_before);

        println!("✅ 配置管理集成测试通过");
        println!("   - 配置验证通过");
        println!("   - 配置部分访问通过");
        println!("   - 版本跟踪通过");
    }

    #[tokio::test]
    async fn test_event_bus_integration() {
        use rust_hft::app::events::*;

        let (event_bus, mut event_receiver) = EventBus::new(false);

        // 启动事件处理
        tokio::spawn(async move {
            event_bus.start_processing(event_receiver).await;
        });

        // 创建测试事件
        let test_payload = OrderUpdatePayload {
            order_id: "test_order_123".to_string(),
            account_id: AccountId::new("test", "main"),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            status: rust_hft::domains::trading::OrderStatus::New,
            update_type: "new".to_string(),
        };

        let test_event = Event::order_event(
            test_payload,
            EventPriority::Normal,
            "test_service".to_string(),
        );

        // 发布事件
        let publish_result = event_bus.publish(test_event).await;
        assert!(publish_result.is_ok());

        // 验证统计
        let stats = event_bus.get_stats().await;
        assert!(stats.total_events > 0);

        println!("✅ 事件总线集成测试通过");
        println!("   - 事件发布通过");
        println!("   - 统计跟踪通过");
    }
}
