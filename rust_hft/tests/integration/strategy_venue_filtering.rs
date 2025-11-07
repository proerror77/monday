//! 策略場所過濾與路由器集成測試
//!
//! 此測試驗證：
//! 1. SystemBuilder 從 YAML 配置正確構建策略實例 ID
//! 2. Engine 使用正確的策略實例 ID 進行事件過濾
//! 3. 單場策略只處理映射場所的事件
//! 4. StrategyMapRouter 正確路由訂單

use std::collections::HashMap;
use hft_core::*;
use ports::*;

/// 模擬的市場事件，帶有特定的 source_venue
fn create_test_bar_event(symbol: &str, venue: Option<VenueId>) -> MarketEvent {
    MarketEvent::Bar(AggregatedBar {
        symbol: Symbol::new(symbol),
        timestamp: 1234567890,
        open: Price::from_f64(100.0).unwrap(),
        high: Price::from_f64(102.0).unwrap(),
        low: Price::from_f64(99.0).unwrap(),
        close: Price::from_f64(101.0).unwrap(),
        volume: Quantity::from_f64(1000.0).unwrap(),
        vwap: Some(Price::from_f64(100.5).unwrap()),
        source_venue: venue,
    })
}

/// 模擬的策略，記錄收到的事件數量
struct MockStrategy {
    name: String,
    received_events: std::sync::Arc<std::sync::Mutex<u32>>,
}

impl MockStrategy {
    fn new(name: String) -> Self {
        Self {
            name,
            received_events: std::sync::Arc::new(std::sync::Mutex::new(0)),
        }
    }

    fn get_received_count(&self) -> u32 {
        *self.received_events.lock().unwrap()
    }
}

impl Strategy for MockStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_market_event(&mut self, _event: &MarketEvent, _account: &AccountView) -> Vec<OrderIntent> {
        *self.received_events.lock().unwrap() += 1;
        // 不產生任何訂單意圖
        Vec::new()
    }

    fn on_execution_event(&mut self, _event: &ExecutionEvent, _account: &AccountView) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn initialize(&mut self) -> HftResult<()> {
        Ok(())
    }

    fn shutdown(&mut self) -> HftResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn test_strategy_venue_filtering_integration() {
    println!("🎯 測試策略場所過濾集成");

    // 創建引擎配置
    let engine_config = engine::EngineConfig::default();
    let mut engine = engine::Engine::new(engine_config);

    // 創建模擬策略並註冊
    let trend_strategy = MockStrategy::new("trend_default:ETHUSDT".to_string());
    let imbalance_strategy = MockStrategy::new("imbalance_lob:BTCUSDT".to_string());
    let arbitrage_strategy = MockStrategy::new("arbitrage_cross:ETHUSDT".to_string());

    let trend_counter = trend_strategy.received_events.clone();
    let imbalance_counter = imbalance_strategy.received_events.clone();
    let arbitrage_counter = arbitrage_strategy.received_events.clone();

    engine.register_strategy(trend_strategy);
    engine.register_strategy(imbalance_strategy);
    engine.register_strategy(arbitrage_strategy);

    // 設置策略場所映射（模擬 SystemBuilder 的行為）
    let mut strategy_venue_mapping = HashMap::new();
    strategy_venue_mapping.insert("trend_default:ETHUSDT".to_string(), VenueId::BINANCE);
    strategy_venue_mapping.insert("imbalance_lob:BTCUSDT".to_string(), VenueId::BITGET);
    // arbitrage_cross 作為跨場策略，不在映射中

    engine.set_strategy_venue_mapping(strategy_venue_mapping);

    // 創建測試事件：來自不同場所的 ETHUSDT 事件
    let binance_event = create_test_bar_event("ETHUSDT", Some(VenueId::BINANCE));
    let bitget_event = create_test_bar_event("ETHUSDT", Some(VenueId::BITGET));
    let no_venue_event = create_test_bar_event("ETHUSDT", None);

    // 手動觸發策略處理這些事件
    let account_view = AccountView::default();

    // 處理來自 BINANCE 的事件
    engine.process_market_event_for_strategies(&binance_event, &account_view);

    // 處理來自 BITGET 的事件
    engine.process_market_event_for_strategies(&bitget_event, &account_view);

    // 處理沒有 venue 信息的事件
    engine.process_market_event_for_strategies(&no_venue_event, &account_view);

    // 驗證結果
    let trend_received = trend_counter.lock().unwrap();
    let imbalance_received = imbalance_counter.lock().unwrap();
    let arbitrage_received = arbitrage_counter.lock().unwrap();

    println!("trend_default:ETHUSDT 收到事件數: {}", *trend_received);
    println!("imbalance_lob:BTCUSDT 收到事件數: {}", *imbalance_received);
    println!("arbitrage_cross:ETHUSDT 收到事件數: {}", *arbitrage_received);

    // 期望結果：
    // - trend_default:ETHUSDT (單場策略，映射到 BINANCE): 應該收到 2 個事件（BINANCE + 無 venue）
    // - imbalance_lob:BTCUSDT (單場策略，映射到 BITGET，但事件是 ETHUSDT): 應該收到 1 個事件（無 venue）
    // - arbitrage_cross:ETHUSDT (跨場策略，不在映射中): 應該收到 3 個事件（全部）

    assert_eq!(*trend_received, 2, "trend_default:ETHUSDT 應該收到 2 個事件");
    assert_eq!(*imbalance_received, 1, "imbalance_lob:BTCUSDT 應該收到 1 個事件");
    assert_eq!(*arbitrage_received, 3, "arbitrage_cross:ETHUSDT 應該收到 3 個事件");

    println!("✅ 策略場所過濾集成測試通過");
}

#[tokio::test]
async fn test_strategy_map_router_integration() {
    println!("🔀 測試 StrategyMapRouter 集成");

    // 創建路由器配置
    let mut strategy_venues = HashMap::new();
    strategy_venues.insert("trend_default:ETHUSDT".to_string(), "BINANCE".to_string());
    strategy_venues.insert("imbalance_lob:BTCUSDT".to_string(), "BITGET".to_string());

    let router_config = RouterConfig::StrategyMap {
        default_venue: "BINANCE".to_string(),
        strategy_venues,
    };

    let router = router_config.build();

    // 創建場所映射
    let mut venue_to_client = HashMap::new();
    venue_to_client.insert(VenueId::BINANCE, 0);
    venue_to_client.insert(VenueId::BITGET, 1);

    // 測試訂單意圖路由
    let trend_intent = OrderIntent {
        symbol: Symbol::new("ETHUSDT"),
        side: Side::Buy,
        order_type: OrderType::Market,
        quantity: Quantity::from_f64(1.0).unwrap(),
        price: None,
        time_in_force: TimeInForce::IOC,
        strategy_id: "trend_default:ETHUSDT".to_string(),
        target_venue: None, // 讓路由器決定
    };

    let imbalance_intent = OrderIntent {
        symbol: Symbol::new("BTCUSDT"),
        side: Side::Buy,
        order_type: OrderType::Limit,
        quantity: Quantity::from_f64(0.1).unwrap(),
        price: Some(Price::from_f64(50000.0).unwrap()),
        time_in_force: TimeInForce::GTC,
        strategy_id: "imbalance_lob:BTCUSDT".to_string(),
        target_venue: None,
    };

    // 路由決策
    let trend_decision = router.route_order(&trend_intent, &venue_to_client, None);
    let imbalance_decision = router.route_order(&imbalance_intent, &venue_to_client, None);

    // 驗證路由結果
    assert!(trend_decision.is_some(), "trend 策略應該找到路由");
    assert!(imbalance_decision.is_some(), "imbalance 策略應該找到路由");

    let trend_decision = trend_decision.unwrap();
    let imbalance_decision = imbalance_decision.unwrap();

    assert_eq!(trend_decision.target_venue, VenueId::BINANCE, "trend 策略應該路由到 BINANCE");
    assert_eq!(trend_decision.client_index, 0, "trend 策略應該使用客戶端 0");

    assert_eq!(imbalance_decision.target_venue, VenueId::BITGET, "imbalance 策略應該路由到 BITGET");
    assert_eq!(imbalance_decision.client_index, 1, "imbalance 策略應該使用客戶端 1");

    println!("✅ StrategyMapRouter 集成測試通過");
}