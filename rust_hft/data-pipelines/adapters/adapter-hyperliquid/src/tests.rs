//! Hyperliquid 数据适配器测试
//!
//! 测试市场数据流的连接和解析功能

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        extract_coin_from_symbol, HyperliquidMarketConfig, HyperliquidMarketStream, L2BookData,
        SubscribeRequest, Subscription, TradeData,
    };
    use hft_core::{Side, Symbol, VenueId};
    use ports::{MarketEvent, MarketStream};

    #[tokio::test]
    async fn test_hyperliquid_market_stream_creation() {
        let config = HyperliquidMarketConfig::default();
        let stream = HyperliquidMarketStream::new(config);

        assert!(!stream.connected);
        assert_eq!(stream.symbol_mapping.len(), 4);
        assert!(stream.symbol_mapping.contains_key("BTC"));
        assert!(stream.symbol_mapping.contains_key("ETH"));
        assert!(stream.symbol_mapping.contains_key("SOL"));
        assert!(stream.symbol_mapping.contains_key("SUI"));
    }

    #[tokio::test]
    async fn test_extract_coin_from_symbol() {
        let btc_symbol = Symbol("BTC-PERP".to_string());
        let eth_symbol = Symbol("ETH-PERP".to_string());
        let invalid_symbol = Symbol("INVALID".to_string());

        assert_eq!(extract_coin_from_symbol(&btc_symbol).unwrap(), "BTC");
        assert_eq!(extract_coin_from_symbol(&eth_symbol).unwrap(), "ETH");
        assert!(extract_coin_from_symbol(&invalid_symbol).is_err());
    }

    #[tokio::test]
    async fn test_health_check() {
        let stream = HyperliquidMarketStream::mainnet();
        let health = stream.health().await;

        assert!(!health.connected); // 初始状态未连接
        assert!(health.latency_ms.is_some());
        assert!(health.last_heartbeat > 0);
    }

    #[test]
    fn test_subscribe_request_serialization() {
        let request = SubscribeRequest {
            method: "subscribe".to_string(),
            subscription: Subscription::L2Book {
                coin: "BTC".to_string(),
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("subscribe"));
        assert!(json.contains("l2Book"));
        assert!(json.contains("BTC"));
    }

    #[test]
    fn test_trade_data_deserialization() {
        let json = r#"{
            "coin": "BTC",
            "side": "A",
            "px": "50000.0",
            "sz": "0.001",
            "time": 1721810005345,
            "hash": "abc123"
        }"#;

        let trade: TradeData = serde_json::from_str(json).unwrap();
        assert_eq!(trade.coin, "BTC");
        assert_eq!(trade.side, "A");
        assert_eq!(trade.px, "50000.0");
        assert_eq!(trade.sz, "0.001");
        assert_eq!(trade.time, 1721810005345);
        assert_eq!(trade.hash, Some("abc123".to_string()));
    }

    #[test]
    fn test_l2_book_data_deserialization() {
        let json = r#"{
            "coin": "BTC",
            "levels": [
                [
                    {"px": "50000.0", "sz": "0.5", "n": 1},
                    {"px": "49999.0", "sz": "0.3", "n": 2}
                ],
                [
                    {"px": "50001.0", "sz": "0.4", "n": 1},
                    {"px": "50002.0", "sz": "0.6", "n": 3}
                ]
            ],
            "time": 1721810005123
        }"#;

        let book_data: L2BookData = serde_json::from_str(json).unwrap();
        assert_eq!(book_data.coin, "BTC");
        assert_eq!(book_data.levels.len(), 2);
        assert_eq!(book_data.levels[0].len(), 2); // bids
        assert_eq!(book_data.levels[1].len(), 2); // asks
        assert_eq!(book_data.time, 1721810005123);
    }

    #[tokio::test]
    async fn test_parse_message_with_trade_data() {
        let stream = HyperliquidMarketStream::mainnet();

        // 模拟交易数据消息
        let trade_message = r#"{
            "channel": "trades",
            "data": [
                {
                    "coin": "BTC",
                    "side": "A",
                    "px": "50000.0",
                    "sz": "0.001",
                    "time": 1721810005345,
                    "hash": "abc123"
                }
            ]
        }"#;

        if let Some(event) = stream.parse_message(trade_message) {
            match event {
                MarketEvent::Trade(trade) => {
                    assert_eq!(trade.symbol, Symbol("BTC-PERP".to_string()));
                    assert_eq!(trade.side, Side::Buy); // "A" = Aggressive buy
                    assert_eq!(trade.trade_id, "abc123");
                }
                _ => panic!("Expected Trade event"),
            }
        } else {
            panic!("Failed to parse trade message");
        }
    }

    #[tokio::test]
    async fn test_parse_message_with_l2_book_data() {
        let stream = HyperliquidMarketStream::mainnet();

        // 模拟 L2 订单簿数据消息
        let book_message = r#"{
            "channel": "l2Book",
            "data": {
                "coin": "BTC",
                "levels": [
                    [
                        {"px": "50000.0", "sz": "0.5", "n": 1}
                    ],
                    [
                        {"px": "50001.0", "sz": "0.4", "n": 1}
                    ]
                ],
                "time": 1721810005123
            }
        }"#;

        if let Some(event) = stream.parse_message(book_message) {
            match event {
                MarketEvent::Snapshot(snapshot) => {
                    assert_eq!(snapshot.symbol, Symbol("BTC-PERP".to_string()));
                    assert_eq!(snapshot.bids.len(), 1);
                    assert_eq!(snapshot.asks.len(), 1);
                    assert_eq!(snapshot.timestamp, 1721810005123);
                    assert_eq!(snapshot.source_venue, Some(VenueId::HYPERLIQUID));
                }
                _ => panic!("Expected Snapshot event"),
            }
        } else {
            panic!("Failed to parse book message");
        }
    }

    #[test]
    fn test_venue_id_constant() {
        assert_eq!(VenueId::HYPERLIQUID.as_str(), "HYPERLIQUID");
        assert_eq!(VenueId::HYPERLIQUID.0, 5);
    }

    #[tokio::test]
    async fn test_convenience_constructors() {
        let mainnet_stream = HyperliquidMarketStream::mainnet();
        assert_eq!(
            mainnet_stream.config.ws_base_url,
            "wss://api.hyperliquid.xyz/ws"
        );

        let testnet_stream = HyperliquidMarketStream::testnet();
        assert_eq!(
            testnet_stream.config.ws_base_url,
            "wss://api.hyperliquid-testnet.xyz/ws"
        );

        let custom_symbols = vec![Symbol("BTC-PERP".to_string())];
        let custom_stream = HyperliquidMarketStream::with_symbols(custom_symbols.clone());
        assert_eq!(custom_stream.config.symbols, custom_symbols);
    }
}
