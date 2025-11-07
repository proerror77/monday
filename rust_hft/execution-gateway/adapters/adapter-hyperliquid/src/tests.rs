//! Hyperliquid 适配器测试
//!
//! 测试 Paper 和 Live 模式的基础功能

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::{
        Eip712Signer, ExecutionMode, HyperliquidExecutionClient, HyperliquidExecutionConfig,
    };
    use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce};
    use ports::{ExecutionClient, OrderIntent};

    #[tokio::test]
    async fn test_paper_mode_basic_operations() {
        let config = HyperliquidExecutionConfig {
            mode: ExecutionMode::Paper,
            ..Default::default()
        };

        let mut client = HyperliquidExecutionClient::new(config);

        // 测试连接
        assert!(client.connect().await.is_ok());
        assert!(client.health().await.connected);

        // 测试下单
        let order_intent = OrderIntent {
            symbol: Symbol::new("BTC-PERP"),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.1).unwrap(),
            price: Some(Price::from_f64(50000.0).unwrap()),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test".to_string(),
            target_venue: None,
        };

        let order_id = client.place_order(order_intent).await.unwrap();
        assert!(order_id.0.starts_with("HL_PAPER_"));

        // 测试撤单
        assert!(client.cancel_order(&order_id).await.is_ok());

        // 测试改单
        let modify_result = client
            .modify_order(
                &order_id,
                Some(Quantity::from_f64(0.2).unwrap()),
                Some(Price::from_f64(51000.0).unwrap()),
            )
            .await;
        assert!(modify_result.is_ok());

        // 测试查询未结订单 (Paper 模式应该返回空列表)
        let open_orders = client.list_open_orders().await.unwrap();
        assert_eq!(open_orders.len(), 0);

        // 测试断开连接
        assert!(client.disconnect().await.is_ok());
    }

    #[tokio::test]
    async fn test_live_mode_without_private_key() {
        let config = HyperliquidExecutionConfig {
            mode: ExecutionMode::Live,
            private_key: "".to_string(), // 空私钥应该失败
            ..Default::default()
        };

        let mut client = HyperliquidExecutionClient::new(config);

        // 连接时应该失败，因为没有私钥
        let connect_result = client.connect().await;
        assert!(connect_result.is_err());
        assert!(connect_result
            .unwrap_err()
            .to_string()
            .contains("Live 模式需要私钥"));
    }

    #[tokio::test]
    async fn test_symbol_mapping() {
        let config = HyperliquidExecutionConfig::default();
        let client = HyperliquidExecutionClient::new(config);

        // 测试资产索引映射
        assert_eq!(client.asset_map.get(&Symbol::new("BTC-PERP")), Some(&0));
        assert_eq!(client.asset_map.get(&Symbol::new("ETH-PERP")), Some(&1));
        assert_eq!(client.asset_map.get(&Symbol::new("SOL-PERP")), Some(&2));
        assert_eq!(client.asset_map.get(&Symbol::new("SUI-PERP")), Some(&3));
    }

    // Market stream tests are in the data adapter, not execution adapter

    #[tokio::test]
    async fn test_list_open_orders() {
        // 测试 Paper 模式
        let paper_config = HyperliquidExecutionConfig {
            mode: ExecutionMode::Paper,
            ..Default::default()
        };
        let mut paper_client = HyperliquidExecutionClient::new(paper_config);
        assert!(paper_client.connect().await.is_ok());

        // Paper 模式应该总是返回空列表
        let paper_orders = paper_client.list_open_orders().await.unwrap();
        assert_eq!(paper_orders.len(), 0);

        // 测试 Live 模式（无私钥，应该返回错误）
        let live_config = HyperliquidExecutionConfig {
            mode: ExecutionMode::Live,
            private_key: "".to_string(),
            ..Default::default()
        };
        let live_client = HyperliquidExecutionClient::new(live_config);

        // Live 模式没有私钥时，查询应该失败
        let live_orders_result = live_client.list_open_orders().await;
        assert!(live_orders_result.is_err());

        assert!(paper_client.disconnect().await.is_ok());
    }

    #[test]
    fn test_eip712_signer_creation() {
        // 测试有效私钥格式
        let valid_key = "a".repeat(64); // 32 字节的十六进制
        let signer_result = Eip712Signer::new(&valid_key);
        assert!(signer_result.is_ok());

        // 测试无效私钥格式
        let invalid_key = "invalid_key";
        let signer_result = Eip712Signer::new(invalid_key);
        assert!(signer_result.is_err());

        // 测试 0x 前缀
        let key_with_prefix = format!("0x{}", "b".repeat(64));
        let signer_result = Eip712Signer::new(&key_with_prefix);
        assert!(signer_result.is_ok());
    }

    #[test]
    fn test_config_defaults() {
        let config = HyperliquidExecutionConfig::default();

        assert_eq!(config.rest_base_url, "https://api.hyperliquid.xyz");
        assert_eq!(config.ws_private_url, "wss://api.hyperliquid.xyz/ws");
        assert_eq!(config.mode, ExecutionMode::Paper);
        assert_eq!(config.timeout_ms, 5000);
        assert!(config.private_key.is_empty());
        assert!(config.vault_address.is_none());
    }
}
