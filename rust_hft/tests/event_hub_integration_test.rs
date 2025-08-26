//! 事件分發中心集成測試

use rust_hft::exchanges::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_bitget_multi_consumer_events() {
    // 創建 Bitget 交易所實例
    let exchange = BitgetExchange::new();

    // 獲取事件分發中心
    let event_hub = exchange
        .get_event_hub()
        .expect("Event hub should be available");

    // 創建三個不同的訂閱者
    let mut strategy_receiver = event_hub
        .subscribe_market_events(
            "strategy_test".to_string(),
            100,
            EventFilter::Symbol("BTCUSDT".to_string()),
        )
        .await;

    let mut backtest_receiver = event_hub
        .subscribe_market_events("backtest_test".to_string(), 100, EventFilter::All)
        .await;

    let mut execution_receiver = event_hub
        .subscribe_execution_reports("execution_test".to_string(), 100, EventFilter::All)
        .await;

    // 模擬市場事件
    let btc_event = MarketEvent::Ticker {
        symbol: "BTCUSDT".to_string(),
        exchange: "bitget".to_string(),
        last_price: 50000.0,
        bid_price: 49999.0,
        ask_price: 50001.0,
        bid_size: 1.0,
        ask_size: 1.0,
        volume_24h: 1000.0,
        change_24h: 0.05,
        timestamp: 1234567890,
    };

    let eth_event = MarketEvent::Ticker {
        symbol: "ETHUSDT".to_string(),
        exchange: "bitget".to_string(),
        last_price: 3000.0,
        bid_price: 2999.0,
        ask_price: 3001.0,
        bid_size: 1.0,
        ask_size: 1.0,
        volume_24h: 500.0,
        change_24h: 0.03,
        timestamp: 1234567891,
    };

    // 廣播事件
    event_hub.broadcast_market_event(btc_event).await;
    event_hub.broadcast_market_event(eth_event).await;

    // 等待事件分發
    sleep(Duration::from_millis(10)).await;

    // 驗證策略訂閱者只收到 BTC 事件
    let received_btc = strategy_receiver.try_recv();
    assert!(received_btc.is_ok(), "Strategy should receive BTC event");
    assert_eq!(received_btc.unwrap().symbol(), "BTCUSDT");

    // 策略訂閱者不應該收到 ETH 事件
    let received_eth = strategy_receiver.try_recv();
    assert!(
        received_eth.is_err(),
        "Strategy should not receive ETH event"
    );

    // 回測訂閱者應該收到兩個事件
    let backtest_event1 = backtest_receiver.try_recv();
    let backtest_event2 = backtest_receiver.try_recv();
    assert!(
        backtest_event1.is_ok(),
        "Backtest should receive first event"
    );
    assert!(
        backtest_event2.is_ok(),
        "Backtest should receive second event"
    );

    // 測試執行回報
    let execution_report = ExecutionReport {
        order_id: "test_order".to_string(),
        client_order_id: Some("client_test".to_string()),
        exchange: "bitget".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: rust_hft::core::types::OrderSide::Buy,
        order_type: rust_hft::core::types::OrderType::Limit,
        status: rust_hft::core::types::OrderStatus::Filled,
        original_quantity: 1.0,
        executed_quantity: 1.0,
        remaining_quantity: 0.0,
        price: 50000.0,
        avg_price: 50000.0,
        last_executed_price: 50000.0,
        last_executed_quantity: 1.0,
        commission: 0.1,
        commission_asset: "USDT".to_string(),
        create_time: 1234567890,
        update_time: 1234567890,
        transaction_time: 1234567890,
        reject_reason: None,
    };

    event_hub.broadcast_execution_report(execution_report).await;

    // 等待執行回報分發
    sleep(Duration::from_millis(10)).await;

    // 驗證執行回報接收
    let received_execution = execution_receiver.try_recv();
    assert!(
        received_execution.is_ok(),
        "Should receive execution report"
    );
    assert_eq!(received_execution.unwrap().order_id, "test_order");
}

#[tokio::test]
async fn test_event_hub_statistics() {
    let event_hub = Arc::new(ExchangeEventHub::new());

    // 創建訂閱者
    let _receiver1 = event_hub
        .subscribe_market_events("stats_test_1".to_string(), 100, EventFilter::All)
        .await;

    let _receiver2 = event_hub
        .subscribe_market_events(
            "stats_test_2".to_string(),
            100,
            EventFilter::Symbol("BTCUSDT".to_string()),
        )
        .await;

    let _exec_receiver = event_hub
        .subscribe_execution_reports("exec_stats_test".to_string(), 100, EventFilter::All)
        .await;

    // 發送一些事件
    for i in 0..5 {
        let event = MarketEvent::Heartbeat {
            exchange: "test".to_string(),
            timestamp: i as u64,
        };
        event_hub.broadcast_market_event(event).await;
    }

    // 檢查統計信息
    let hub_stats = event_hub.get_hub_stats().await;
    assert!(hub_stats.total_events_sent > 0, "Should have sent events");
    assert_eq!(
        hub_stats.market_event_subscribers, 2,
        "Should have 2 market subscribers"
    );
    assert_eq!(
        hub_stats.execution_report_subscribers, 1,
        "Should have 1 execution subscriber"
    );
    assert_eq!(
        hub_stats.active_subscribers, 3,
        "Should have 3 total subscribers"
    );

    let market_stats = event_hub.get_market_subscriber_stats().await;
    assert_eq!(
        market_stats.len(),
        2,
        "Should have 2 market subscriber stats"
    );

    let execution_stats = event_hub.get_execution_subscriber_stats().await;
    assert_eq!(
        execution_stats.len(),
        1,
        "Should have 1 execution subscriber stats"
    );
}

#[tokio::test]
async fn test_event_filtering() {
    let event_hub = Arc::new(ExchangeEventHub::new());

    // 創建不同過濾器的訂閱者
    let mut symbol_receiver = event_hub
        .subscribe_market_events(
            "symbol_filter".to_string(),
            100,
            EventFilter::Symbol("BTCUSDT".to_string()),
        )
        .await;

    let mut exchange_receiver = event_hub
        .subscribe_market_events(
            "exchange_filter".to_string(),
            100,
            EventFilter::Exchange("bitget".to_string()),
        )
        .await;

    let mut combined_receiver = event_hub
        .subscribe_market_events(
            "combined_filter".to_string(),
            100,
            EventFilter::And(vec![
                EventFilter::Symbol("BTCUSDT".to_string()),
                EventFilter::Exchange("bitget".to_string()),
            ]),
        )
        .await;

    let mut type_receiver = event_hub
        .subscribe_market_events(
            "type_filter".to_string(),
            100,
            EventFilter::EventType(EventType::Trade),
        )
        .await;

    // 發送匹配的事件
    let btc_bitget_trade = MarketEvent::Trade {
        symbol: "BTCUSDT".to_string(),
        exchange: "bitget".to_string(),
        trade_id: "trade1".to_string(),
        price: 50000.0,
        quantity: 1.0,
        side: rust_hft::core::types::OrderSide::Buy,
        timestamp: 1234567890,
        buyer_maker: false,
    };

    event_hub.broadcast_market_event(btc_bitget_trade).await;
    sleep(Duration::from_millis(10)).await;

    // 所有訂閱者都應該收到這個事件
    assert!(
        symbol_receiver.try_recv().is_ok(),
        "Symbol filter should match"
    );
    assert!(
        exchange_receiver.try_recv().is_ok(),
        "Exchange filter should match"
    );
    assert!(
        combined_receiver.try_recv().is_ok(),
        "Combined filter should match"
    );
    assert!(type_receiver.try_recv().is_ok(), "Type filter should match");

    // 發送不匹配的事件
    let eth_binance_ticker = MarketEvent::Ticker {
        symbol: "ETHUSDT".to_string(),
        exchange: "binance".to_string(),
        last_price: 3000.0,
        bid_price: 2999.0,
        ask_price: 3001.0,
        bid_size: 1.0,
        ask_size: 1.0,
        volume_24h: 500.0,
        change_24h: 0.03,
        timestamp: 1234567891,
    };

    event_hub.broadcast_market_event(eth_binance_ticker).await;
    sleep(Duration::from_millis(10)).await;

    // 只有交易所過濾器不應該匹配
    assert!(
        symbol_receiver.try_recv().is_err(),
        "Symbol filter should not match ETH"
    );
    assert!(
        exchange_receiver.try_recv().is_err(),
        "Exchange filter should not match Binance"
    );
    assert!(
        combined_receiver.try_recv().is_err(),
        "Combined filter should not match"
    );
    assert!(
        type_receiver.try_recv().is_err(),
        "Type filter should not match Ticker"
    );
}

#[tokio::test]
async fn test_subscription_management() {
    let event_hub = Arc::new(ExchangeEventHub::new());

    // 創建訂閱
    let _receiver = event_hub
        .subscribe_market_events("test_unsub".to_string(), 100, EventFilter::All)
        .await;

    // 檢查訂閱者數量
    let stats_before = event_hub.get_hub_stats().await;
    assert_eq!(stats_before.market_event_subscribers, 1);

    // 取消訂閱
    let removed = event_hub
        .unsubscribe_market_events(&"test_unsub".to_string())
        .await;
    assert!(removed, "Should successfully remove subscriber");

    // 檢查訂閱者數量
    let stats_after = event_hub.get_hub_stats().await;
    assert_eq!(stats_after.market_event_subscribers, 0);

    // 嘗試取消不存在的訂閱
    let not_removed = event_hub
        .unsubscribe_market_events(&"non_existent".to_string())
        .await;
    assert!(!not_removed, "Should not remove non-existent subscriber");
}
