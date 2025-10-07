use super::*;

#[test]
fn test_parse_system_config() {
    let yaml = r#"
schema_version: v2
engine:
  queue_capacity: 1024
  stale_us: 5000
  top_n: 10

venues:
  - name: binance
    venue_type: BINANCE
    symbol_catalog: [ "BTCUSDT@BINANCE" ]

strategies:
  - name: trend_btc
    strategy_type: Trend
    symbols: [ "BTCUSDT" ]
    params:
      kind: trend
      ema_fast: 12
      ema_slow: 26
      rsi_period: 14

risk:
  global_position_limit: 1000
  global_notional_limit: 100000
  max_daily_trades: 100
  max_orders_per_second: 10
  staleness_threshold_us: 5000
"#;

    let config = SystemConfig::from_yaml_str(yaml).expect("parse config");
    assert_eq!(config.engine.top_n, 10);
    assert_eq!(config.strategies.len(), 1);
    assert_eq!(config.venues.len(), 1);
    assert_eq!(config.risk.risk_type, "Default");
}
