//! Critical Path Unit Tests
//!
//! 關鍵路徑單元測試，覆蓋：
//! - OrderBook 維護與快照邏輯
//! - Risk Manager 限額檢查
//! - Execution Event 處理
//! - Strategy Trait 實現

use hft_core::{Price, Quantity, Side, Symbol, VenueId};
use ports::{AccountView, ExecutionEvent, OrderIntent, OrderType, Position, VenueSpec};
use risk::default_risk_manager::{DefaultRiskManager, PrecisionNormalizer, RiskConfig};
use rust_decimal::Decimal;
use std::collections::HashMap;

// ============================================================================
// OrderBook Tests
// ============================================================================

mod orderbook_tests {
    use hft_core::topn_orderbook::{DualTopNJoiner, TopNOrderBook};
    use hft_core::Symbol;

    #[test]
    fn test_empty_orderbook() {
        let book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        assert!(book.best_bid().is_none());
        assert!(book.best_ask().is_none());
        assert!(book.spread().is_none());
        assert!(book.mid_price().is_none());
        assert!(!book.is_valid());
        assert_eq!(book.bid_count, 0);
        assert_eq!(book.ask_count, 0);
    }

    #[test]
    fn test_snapshot_overwrites_previous() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("ETHUSDT"));

        // First snapshot
        let bids1 = vec![(3500.0, 10.0)];
        let asks1 = vec![(3501.0, 8.0)];
        book.apply_snapshot(&bids1, &asks1, 1000).unwrap();

        assert_eq!(book.best_bid(), Some(3500.0));
        assert_eq!(book.version, 1);

        // Second snapshot should completely overwrite
        let bids2 = vec![(4000.0, 5.0), (3999.0, 3.0)];
        let asks2 = vec![(4001.0, 4.0)];
        book.apply_snapshot(&bids2, &asks2, 2000).unwrap();

        assert_eq!(book.best_bid(), Some(4000.0));
        assert_eq!(book.bid_count, 2);
        assert_eq!(book.ask_count, 1);
        assert_eq!(book.version, 2);
    }

    #[test]
    fn test_update_removes_level_on_zero_quantity() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![(67200.0, 1.5), (67199.0, 2.0)];
        let asks = vec![(67201.0, 1.2)];
        book.apply_snapshot(&bids, &asks, 1000).unwrap();

        assert_eq!(book.bid_count, 2);

        // Remove first bid level by setting quantity to 0
        let bid_updates = vec![(67200.0, 0.0)];
        book.apply_update(&bid_updates, &[], 2000).unwrap();

        assert_eq!(book.bid_count, 1);
        assert_eq!(book.best_bid(), Some(67199.0));
    }

    #[test]
    fn test_update_inserts_new_level_in_order() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![(67200.0, 1.0), (67198.0, 1.0)];
        let asks = vec![(67201.0, 1.0)];
        book.apply_snapshot(&bids, &asks, 1000).unwrap();

        // Insert new bid in the middle
        let bid_updates = vec![(67199.0, 2.0)];
        book.apply_update(&bid_updates, &[], 2000).unwrap();

        assert_eq!(book.bid_count, 3);
        // Should be ordered: 67200, 67199, 67198
        assert_eq!(book.bid_prices[0], 67200.0);
        assert_eq!(book.bid_prices[1], 67199.0);
        assert_eq!(book.bid_prices[2], 67198.0);
    }

    #[test]
    fn test_topn_capacity_limit() {
        let mut book = TopNOrderBook::<3>::new(Symbol::new("BTCUSDT"));

        // Try to insert more than 3 levels
        let bids = vec![
            (67200.0, 1.0),
            (67199.0, 1.0),
            (67198.0, 1.0),
            (67197.0, 1.0), // Should be ignored
            (67196.0, 1.0), // Should be ignored
        ];
        book.apply_snapshot(&bids, &[], 1000).unwrap();

        assert_eq!(book.bid_count, 3);
        assert_eq!(book.bid_prices[2], 67198.0); // Lowest kept bid
    }

    #[test]
    fn test_vwap_calculation() {
        let mut book = TopNOrderBook::<5>::new(Symbol::new("BTCUSDT"));

        let bids = vec![
            (100.0, 10.0), // 10 units at 100
            (99.0, 20.0),  // 20 units at 99
            (98.0, 30.0),  // 30 units at 98
        ];
        book.apply_snapshot(&bids, &[], 1000).unwrap();

        // VWAP for 15 units: (10*100 + 5*99) / 15 = 1495/15 = 99.666...
        let vwap = book.vwap_bid(15.0).unwrap();
        assert!((vwap - 99.666666).abs() < 0.001);

        // VWAP for 5 units: all at best bid
        let vwap_small = book.vwap_bid(5.0).unwrap();
        assert_eq!(vwap_small, 100.0);
    }

    #[test]
    fn test_dual_joiner_no_arbitrage() {
        let mut joiner = DualTopNJoiner::<5>::new(
            Symbol::new("BTCUSDT_A"),
            Symbol::new("BTCUSDT_B"),
            1.0, // 1 bps minimum
        );

        // Same prices - no arbitrage
        joiner.venue_a.apply_snapshot(&[(100.0, 1.0)], &[(101.0, 1.0)], 1000).unwrap();
        joiner.venue_b.apply_snapshot(&[(100.0, 1.0)], &[(101.0, 1.0)], 1000).unwrap();

        assert!(joiner.check_arbitrage(1500).is_none());
    }

    #[test]
    fn test_dual_joiner_stale_data_rejection() {
        let mut joiner = DualTopNJoiner::<5>::new(
            Symbol::new("BTCUSDT_A"),
            Symbol::new("BTCUSDT_B"),
            0.1,
        );
        joiner.max_stale_us = 1000; // 1ms staleness limit

        // Venue A is fresh, Venue B is stale
        joiner.venue_a.apply_snapshot(&[(100.0, 1.0)], &[(101.0, 1.0)], 2000).unwrap();
        joiner.venue_b.apply_snapshot(&[(99.0, 1.0)], &[(100.5, 1.0)], 500).unwrap(); // Old timestamp

        // Current time is 2500, Venue B data is 2000us old (stale)
        assert!(joiner.check_arbitrage(2500).is_none());
    }
}

// ============================================================================
// Risk Manager Tests
// ============================================================================

mod risk_manager_tests {
    use super::*;

    fn create_test_intent(symbol: &str, side: Side, qty: f64, price: Option<f64>) -> OrderIntent {
        OrderIntent {
            symbol: Symbol::new(symbol),
            side,
            quantity: Quantity::from_f64(qty).unwrap(),
            price: price.map(|p| Price::from_f64(p).unwrap()),
            order_type: OrderType::Limit,
            time_in_force: None,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(VenueId::BITGET),
            client_order_id: None,
        }
    }

    fn create_test_account() -> AccountView {
        let mut positions = HashMap::new();
        positions.insert(
            Symbol::new("BTCUSDT"),
            Position {
                quantity: Quantity::from_f64(0.5).unwrap(),
                avg_price: Price::from_f64(67000.0).unwrap(),
                unrealized_pnl: Decimal::from(100),
            },
        );

        AccountView {
            cash_balance: Decimal::from(100000),
            positions,
            unrealized_pnl: Decimal::from(100),
            realized_pnl: Decimal::from(500),
        }
    }

    #[test]
    fn test_position_limit_exceeded() {
        let config = RiskConfig {
            max_position_per_symbol: Quantity::from_f64(1.0).unwrap(),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);
        let account = create_test_account();

        // Try to buy 0.6 BTC when already holding 0.5 (total 1.1 > 1.0 limit)
        let intent = create_test_intent("BTCUSDT", Side::Buy, 0.6, Some(67000.0));

        let result = mgr.check_position_limits(&intent, &account);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("持仓限额"));
    }

    #[test]
    fn test_position_limit_passed() {
        let config = RiskConfig {
            max_position_per_symbol: Quantity::from_f64(2.0).unwrap(),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);
        let account = create_test_account();

        // Buy 0.5 BTC when holding 0.5 (total 1.0 < 2.0 limit)
        let intent = create_test_intent("BTCUSDT", Side::Buy, 0.5, Some(67000.0));

        let result = mgr.check_position_limits(&intent, &account);
        assert!(result.is_ok());
    }

    #[test]
    fn test_daily_loss_limit() {
        let config = RiskConfig {
            max_daily_loss: Decimal::from(1000),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);

        // Account with large loss
        let mut account = create_test_account();
        account.realized_pnl = Decimal::from(-800);
        account.unrealized_pnl = Decimal::from(-300); // Total -1100

        let result = mgr.check_daily_loss(&account);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("日内最大亏损"));
    }

    #[test]
    fn test_order_rate_limit() {
        let config = RiskConfig {
            max_orders_per_second: 2,
            order_cooldown_ms: 50,
            ..Default::default()
        };
        let mut mgr = DefaultRiskManager::new(config);
        let symbol = Symbol::new("BTCUSDT");

        // First order should pass
        assert!(mgr.check_rate_limit(&symbol).is_ok());
        mgr.update_state(&symbol);

        // Immediate second order should fail (cooldown)
        assert!(mgr.check_rate_limit(&symbol).is_err());
    }

    #[test]
    fn test_global_notional_limit() {
        let config = RiskConfig {
            max_global_notional: Decimal::from(100000),
            max_position_per_symbol: Quantity::from_f64(100.0).unwrap(),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);

        // Account already has ~33500 notional (0.5 * 67000)
        let account = create_test_account();

        // Try to add 70000 notional (1 * 70000), total would be 103500 > 100000
        let intent = create_test_intent("ETHUSDT", Side::Buy, 20.0, Some(3500.0));

        let result = mgr.check_position_limits(&intent, &account);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("全局名义价值限额"));
    }

    #[test]
    fn test_aggressive_mode_bypasses_rate_limit() {
        let config = RiskConfig {
            aggressive_mode: true,
            max_orders_per_second: 1,
            order_cooldown_ms: 1000,
            ..Default::default()
        };
        let mut mgr = DefaultRiskManager::new(config);
        let account = create_test_account();
        let venue = VenueSpec::default();

        let intent1 = create_test_intent("BTCUSDT", Side::Buy, 0.01, Some(67000.0));
        let intent2 = create_test_intent("BTCUSDT", Side::Buy, 0.01, Some(67000.0));

        // Both should pass in aggressive mode
        let approved = mgr.review(vec![intent1, intent2], &account, &venue);
        assert_eq!(approved.len(), 2);
    }

    #[test]
    fn test_emergency_stop() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        mgr.emergency_stop().unwrap();

        // After emergency stop, all limits should be zero
        assert_eq!(mgr.config.max_position_per_symbol, Quantity::zero());
        assert_eq!(mgr.config.max_global_notional, Decimal::ZERO);
        assert_eq!(mgr.config.max_orders_per_second, 0);
    }
}

// ============================================================================
// Precision Normalizer Tests
// ============================================================================

mod precision_tests {
    use super::*;

    #[test]
    fn test_price_normalization_rounding() {
        let price = Price::from_f64(67123.456789).unwrap();
        let tick_size = Price::from_f64(0.01).unwrap();

        let normalized = PrecisionNormalizer::normalize_price(price, tick_size);
        // Should round to 67123.46
        let expected = Decimal::from_str_exact("67123.46").unwrap();
        assert_eq!(normalized.0, expected);
    }

    #[test]
    fn test_quantity_normalization_floor() {
        let quantity = Quantity::from_f64(1.23999).unwrap();
        let lot_size = Quantity::from_f64(0.01).unwrap();

        let normalized = PrecisionNormalizer::normalize_quantity(quantity, lot_size);
        // Should floor to 1.23
        let expected = Decimal::from_str_exact("1.23").unwrap();
        assert_eq!(normalized.0, expected);
    }

    #[test]
    fn test_zero_tick_size_passthrough() {
        let price = Price::from_f64(67123.456789).unwrap();
        let tick_size = Price::from_f64(0.0).unwrap();

        let normalized = PrecisionNormalizer::normalize_price(price, tick_size);
        assert_eq!(normalized.0, price.0);
    }

    #[test]
    fn test_order_minimum_validation() {
        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001).unwrap(),
            price: Some(Price::from_f64(67000.0).unwrap()),
            order_type: OrderType::Limit,
            time_in_force: None,
            strategy_id: "test".to_string(),
            target_venue: None,
            client_order_id: None,
        };

        // Minimum qty is 0.01, intent has 0.001
        let result = PrecisionNormalizer::validate_order_minimums(
            &intent,
            Quantity::from_f64(0.01).unwrap(),
            Decimal::from(10),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("数量低于最小值"));
    }

    #[test]
    fn test_order_minimum_notional() {
        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.0001).unwrap(), // 0.0001 * 67000 = 6.7 notional
            price: Some(Price::from_f64(67000.0).unwrap()),
            order_type: OrderType::Limit,
            time_in_force: None,
            strategy_id: "test".to_string(),
            target_venue: None,
            client_order_id: None,
        };

        // Minimum notional is 10
        let result = PrecisionNormalizer::validate_order_minimums(
            &intent,
            Quantity::from_f64(0.0001).unwrap(), // qty check passes
            Decimal::from(10),                    // notional check fails
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("名义价值低于最小值"));
    }
}

// ============================================================================
// Execution Event Handling Tests
// ============================================================================

mod execution_event_tests {
    use super::*;
    use hft_core::OrderId;

    #[test]
    fn test_fill_event_updates_daily_pnl() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);

        let fill_event = ExecutionEvent::Fill {
            order_id: OrderId::from_u64(1),
            fill_id: "fill_001".to_string(),
            price: Price::from_f64(67000.0).unwrap(),
            quantity: Quantity::from_f64(0.1).unwrap(),
            timestamp: 1000000,
            fee: Decimal::ZERO,
            side: Side::Buy,
        };

        mgr.on_execution_event(&fill_event);

        // daily_pnl should be updated (simplified: just adds notional)
        assert!(mgr.daily_pnl > Decimal::ZERO);
    }

    #[test]
    fn test_risk_metrics_tracking() {
        let config = RiskConfig::default();
        let mut mgr = DefaultRiskManager::new(config);
        let symbol = Symbol::new("BTCUSDT");

        // Simulate some orders
        for _ in 0..5 {
            mgr.update_state(&symbol);
        }

        let metrics = mgr.get_risk_metrics();
        assert_eq!(*metrics.get("total_orders_today").unwrap(), Decimal::from(5));
    }

    #[test]
    fn test_should_halt_trading() {
        let config = RiskConfig {
            max_daily_loss: Decimal::from(1000),
            ..Default::default()
        };
        let mgr = DefaultRiskManager::new(config);

        // Account with acceptable loss
        let mut account = AccountView::default();
        account.realized_pnl = Decimal::from(-500);
        account.unrealized_pnl = Decimal::from(-400);
        assert!(!mgr.should_halt_trading(&account));

        // Account with excessive loss
        account.realized_pnl = Decimal::from(-600);
        account.unrealized_pnl = Decimal::from(-500);
        assert!(mgr.should_halt_trading(&account));
    }
}

// ============================================================================
// Integration Tests
// ============================================================================

mod integration_tests {
    use super::*;

    #[test]
    fn test_full_risk_review_flow() {
        let config = RiskConfig {
            max_position_per_symbol: Quantity::from_f64(10.0).unwrap(),
            max_global_notional: Decimal::from(1000000),
            max_orders_per_second: 10,
            order_cooldown_ms: 10,
            max_daily_loss: Decimal::from(50000),
            aggressive_mode: false,
            staleness_threshold_us: 5000,
        };
        let mut mgr = DefaultRiskManager::new(config);
        let account = AccountView::default();
        let venue = VenueSpec::default();

        let intents = vec![
            OrderIntent {
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                quantity: Quantity::from_f64(0.1).unwrap(),
                price: Some(Price::from_f64(67000.0).unwrap()),
                order_type: OrderType::Limit,
                time_in_force: None,
                strategy_id: "trend_1".to_string(),
                target_venue: Some(VenueId::BITGET),
                client_order_id: None,
            },
            OrderIntent {
                symbol: Symbol::new("ETHUSDT"),
                side: Side::Sell,
                quantity: Quantity::from_f64(1.0).unwrap(),
                price: Some(Price::from_f64(3500.0).unwrap()),
                order_type: OrderType::Market,
                time_in_force: None,
                strategy_id: "arb_1".to_string(),
                target_venue: Some(VenueId::BINANCE),
                client_order_id: None,
            },
        ];

        // Wait a bit to avoid cooldown from potential previous tests
        std::thread::sleep(std::time::Duration::from_millis(20));

        let approved = mgr.review(intents, &account, &venue);

        // Both should pass initial review
        assert_eq!(approved.len(), 2);

        // Verify state was updated
        let metrics = mgr.get_risk_metrics();
        assert_eq!(*metrics.get("total_orders_today").unwrap(), Decimal::from(2));
    }
}

use rust_decimal::prelude::FromStr;

trait DecimalExt {
    fn from_str_exact(s: &str) -> Result<Decimal, rust_decimal::Error>;
}

impl DecimalExt for Decimal {
    fn from_str_exact(s: &str) -> Result<Decimal, rust_decimal::Error> {
        Decimal::from_str(s)
    }
}
