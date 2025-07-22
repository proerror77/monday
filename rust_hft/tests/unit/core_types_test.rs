/*!
 * 核心類型測試 - 提升測試覆蓋率
 * 
 * 測試 core::types 模組中的重要數據結構
 */

use rust_hft::core::types::*;
use rust_decimal::Decimal;
use std::str::FromStr;

#[test]
fn test_trading_signal_creation() {
    let signal = TradingSignal {
        symbol: "BTCUSDT".to_string(),
        action: TradingAction::Buy,
        probability: 0.85,
        confidence: 0.92,
        timestamp: 1642694400000000, // 微秒時間戳
        features: vec![1.0, 2.0, 3.0],
        metadata: std::collections::HashMap::new(),
    };
    
    assert_eq!(signal.symbol, "BTCUSDT");
    assert!(matches!(signal.action, TradingAction::Buy));
    assert_eq!(signal.probability, 0.85);
    assert_eq!(signal.confidence, 0.92);
    assert_eq!(signal.features.len(), 3);
}

#[test]
fn test_trading_action_variants() {
    let buy = TradingAction::Buy;
    let sell = TradingAction::Sell;
    let hold = TradingAction::Hold;
    
    // 測試Debug trait
    assert_eq!(format!("{:?}", buy), "Buy");
    assert_eq!(format!("{:?}", sell), "Sell");
    assert_eq!(format!("{:?}", hold), "Hold");
    
    // 測試Clone trait
    let buy_clone = buy.clone();
    assert!(matches!(buy_clone, TradingAction::Buy));
}

#[test]
fn test_orderbook_snapshot_creation() {
    let snapshot = OrderBookSnapshot {
        symbol: "ETHUSDT".to_string(),
        timestamp: 1642694400000000,
        bids: vec![
            (Decimal::from_str("3000.50").unwrap(), Decimal::from_str("1.5").unwrap()),
            (Decimal::from_str("3000.00").unwrap(), Decimal::from_str("2.0").unwrap()),
        ],
        asks: vec![
            (Decimal::from_str("3001.00").unwrap(), Decimal::from_str("1.2").unwrap()),
            (Decimal::from_str("3001.50").unwrap(), Decimal::from_str("1.8").unwrap()),
        ],
    };
    
    assert_eq!(snapshot.symbol, "ETHUSDT");
    assert_eq!(snapshot.bids.len(), 2);
    assert_eq!(snapshot.asks.len(), 2);
    
    // 測試最佳買賣價
    assert_eq!(snapshot.bids[0].0, Decimal::from_str("3000.50").unwrap());
    assert_eq!(snapshot.asks[0].0, Decimal::from_str("3001.00").unwrap());
}

#[test]
fn test_feature_set_operations() {
    let mut features = FeatureSet::new();
    
    // 測試添加特徵
    features.add_feature("price".to_string(), 45000.0);
    features.add_feature("volume".to_string(), 1.5);
    features.add_feature("spread".to_string(), 0.01);
    
    assert_eq!(features.len(), 3);
    assert!(features.contains_key("price"));
    assert!(features.contains_key("volume"));
    assert!(features.contains_key("spread"));
    
    // 測試獲取特徵
    assert_eq!(features.get("price"), Some(&45000.0));
    assert_eq!(features.get("nonexistent"), None);
    
    // 測試轉換為向量
    let vector = features.to_vector();
    assert_eq!(vector.len(), 3);
}

#[test]
fn test_performance_metrics() {
    let mut metrics = PerformanceMetrics::new();
    
    // 測試延遲記錄
    metrics.record_latency("decision", 1500); // 1.5ms in microseconds
    metrics.record_latency("decision", 2000); // 2.0ms
    metrics.record_latency("database", 5000); // 5.0ms
    
    // 測試統計計算
    let decision_stats = metrics.get_latency_stats("decision");
    assert!(decision_stats.is_some());
    
    let stats = decision_stats.unwrap();
    assert_eq!(stats.count, 2);
    assert_eq!(stats.mean, 1750.0); // (1500 + 2000) / 2
    assert_eq!(stats.min, 1500);
    assert_eq!(stats.max, 2000);
}

#[test]
fn test_configuration_validation() {
    let config = TradingConfig {
        symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        max_position_size: Decimal::from_str("1000.0").unwrap(),
        stop_loss_percentage: Decimal::from_str("0.02").unwrap(), // 2%
        take_profit_percentage: Decimal::from_str("0.05").unwrap(), // 5%
        risk_free_rate: 0.02,
        enabled: true,
    };
    
    // 測試配置驗證
    assert!(config.validate().is_ok());
    
    // 測試無效配置
    let invalid_config = TradingConfig {
        symbols: vec![], // 空符號列表
        max_position_size: Decimal::from_str("1000.0").unwrap(),
        stop_loss_percentage: Decimal::from_str("-0.02").unwrap(), // 負數
        take_profit_percentage: Decimal::from_str("0.05").unwrap(),
        risk_free_rate: 0.02,
        enabled: true,
    };
    
    assert!(invalid_config.validate().is_err());
}

#[test]
fn test_order_creation_and_validation() {
    let order = Order {
        id: "order_123".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        quantity: Decimal::from_str("0.01").unwrap(),
        price: Some(Decimal::from_str("45000.0").unwrap()),
        status: OrderStatus::Pending,
        timestamp: 1642694400000000,
        filled_quantity: Decimal::ZERO,
        average_price: None,
    };
    
    assert_eq!(order.id, "order_123");
    assert!(matches!(order.side, OrderSide::Buy));
    assert!(matches!(order.order_type, OrderType::Market));
    assert!(matches!(order.status, OrderStatus::Pending));
    
    // 測試訂單剩餘數量
    let remaining = order.remaining_quantity();
    assert_eq!(remaining, order.quantity);
    
    // 測試訂單是否完全成交
    assert!(!order.is_filled());
}

#[test]
fn test_portfolio_operations() {
    let mut portfolio = Portfolio::new();
    
    // 測試添加持倉
    portfolio.add_position("BTCUSDT".to_string(), Decimal::from_str("0.5").unwrap());
    portfolio.add_position("ETHUSDT".to_string(), Decimal::from_str("2.0").unwrap());
    
    assert_eq!(portfolio.get_position("BTCUSDT"), Some(&Decimal::from_str("0.5").unwrap()));
    assert_eq!(portfolio.get_position("ETHUSDT"), Some(&Decimal::from_str("2.0").unwrap()));
    assert_eq!(portfolio.get_position("SOLUSDT"), None);
    
    // 測試更新持倉
    portfolio.update_position("BTCUSDT".to_string(), Decimal::from_str("0.8").unwrap());
    assert_eq!(portfolio.get_position("BTCUSDT"), Some(&Decimal::from_str("0.8").unwrap()));
    
    // 測試計算總價值（需要價格）
    let prices = vec![
        ("BTCUSDT".to_string(), Decimal::from_str("45000.0").unwrap()),
        ("ETHUSDT".to_string(), Decimal::from_str("3000.0").unwrap()),
    ].into_iter().collect();
    
    let total_value = portfolio.calculate_total_value(&prices);
    // 0.8 * 45000 + 2.0 * 3000 = 36000 + 6000 = 42000
    assert_eq!(total_value, Decimal::from_str("42000.0").unwrap());
}

#[test]
fn test_risk_metrics_calculation() {
    let mut metrics = RiskMetrics::new();
    
    // 添加一些歷史回報
    let returns = vec![0.02, -0.01, 0.03, -0.015, 0.025, -0.005];
    for ret in returns {
        metrics.add_return(ret);
    }
    
    // 測試風險指標計算
    let volatility = metrics.calculate_volatility();
    assert!(volatility > 0.0);
    
    let var_95 = metrics.calculate_var(0.95);
    assert!(var_95 < 0.0); // VaR 應該是負數
    
    let max_drawdown = metrics.calculate_max_drawdown();
    assert!(max_drawdown <= 0.0);
    
    let sharpe_ratio = metrics.calculate_sharpe_ratio(0.02); // 2% 無風險利率
    // Sharpe比率可以是正數或負數，取決於平均回報
}

#[test]
fn test_market_data_processing() {
    let market_data = MarketData {
        symbol: "BTCUSDT".to_string(),
        timestamp: 1642694400000000,
        price: Decimal::from_str("45000.0").unwrap(),
        volume: Decimal::from_str("1.5").unwrap(),
        bid: Some(Decimal::from_str("44999.5").unwrap()),
        ask: Some(Decimal::from_str("45000.5").unwrap()),
        high_24h: Some(Decimal::from_str("46000.0").unwrap()),
        low_24h: Some(Decimal::from_str("44000.0").unwrap()),
        volume_24h: Some(Decimal::from_str("1000.0").unwrap()),
    };
    
    // 測試價差計算
    let spread = market_data.spread();
    assert_eq!(spread, Some(Decimal::from_str("1.0").unwrap()));
    
    // 測試中間價計算
    let mid_price = market_data.mid_price();
    assert_eq!(mid_price, Some(Decimal::from_str("45000.0").unwrap()));
    
    // 測試是否在24小時範圍內
    assert!(market_data.is_within_daily_range());
}

#[cfg(test)]
mod timestamp_tests {
    use super::*;
    
    #[test]
    fn test_timestamp_utilities() {
        let now_micros = rust_hft::utils::performance::now_micros();
        assert!(now_micros > 0);
        
        let now_nanos = rust_hft::utils::performance::now_nanos();
        assert!(now_nanos > now_micros * 1000);
        
        // 測試時間戳轉換
        let timestamp = Timestamp::now();
        assert!(timestamp.as_micros() > 1600000000000000); // 2020年之後
        
        let from_micros = Timestamp::from_micros(1642694400000000);
        assert_eq!(from_micros.as_micros(), 1642694400000000);
    }
    
    #[test]
    fn test_latency_measurement() {
        use std::time::Duration;
        use std::thread;
        
        let start = rust_hft::utils::performance::now_micros();
        thread::sleep(Duration::from_millis(1)); // 1ms
        let end = rust_hft::utils::performance::now_micros();
        
        let latency = end - start;
        assert!(latency >= 1000); // 至少1ms (1000 microseconds)
        assert!(latency < 10000);  // 但不應超過10ms
    }
}