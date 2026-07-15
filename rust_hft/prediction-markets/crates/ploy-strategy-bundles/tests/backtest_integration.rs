//! Integration test: full backtest loop through StrategyRuntime.
//!
//! Simulates a complete pm_5m_directional backtest with:
//! - Event discovered → spot price moves → quotes update → entry signal
//! - SimulatedExecutor produces realistic fill
//! - TradingRuntime tracks position and PnL

use chrono::{Duration, Utc};
use ploy_strategy_bundles::config::FullConfig;
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use ploy_strategy_bundles::{
    DirectionalStrategy, HistoricalFeed, MarketUpdate, NullRecorder, RecordedFeed, RecordingFeed,
    RuntimeConfig, RuntimeMode, SimulatedExecutor, SimulatedExecutorConfig, StrategyRuntime,
    ThreeLayerProfile,
};
use rust_decimal_macros::dec;
use std::fs;

/// Helper: build a sequence of market updates simulating one 5-min window.
fn build_scenario() -> Vec<MarketUpdate> {
    let now = Utc::now();
    let event_end = now + Duration::seconds(180); // 3 min from now
    let mut updates = Vec::new();

    // 1. Initial spot price FIRST (so event picks it up as open_price)
    updates.push(MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".into(),
        price: dec!(100000),
        ts: now,
    });

    // 2. Event discovered (captures current spot as open_price)
    updates.push(MarketUpdate::EventDiscovered {
        event_id: "evt-btc-001".into(),
        symbol: "BTCUSDT".into(),
        up_token: "up-btc-001".into(),
        down_token: "dn-btc-001".into(),
        end_time: event_end,
        window_secs: 300,
        price_to_beat: None,
        resolved_up_won: Some(true),
    });

    // 3. Polymarket quotes — UP token cheap at 0.30
    updates.push(MarketUpdate::Quote {
        token_id: "up-btc-001".into(),
        bid: Some(dec!(0.29)),
        ask: Some(dec!(0.30)),
        ts: now,
        bid_size: None,
        ask_size: None,
        bid_levels: Vec::new(),
        ask_levels: Vec::new(),
    });
    updates.push(MarketUpdate::Quote {
        token_id: "dn-btc-001".into(),
        bid: Some(dec!(0.69)),
        ask: Some(dec!(0.70)),
        ts: now,
        bid_size: None,
        ask_size: None,
        bid_levels: Vec::new(),
        ask_levels: Vec::new(),
    });

    // 4. BTC trends up over several updates so realized vol has a usable estimate.
    updates.push(MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".into(),
        price: dec!(100400),
        ts: now + Duration::seconds(20),
    });

    updates.push(MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".into(),
        price: dec!(100900),
        ts: now + Duration::seconds(40),
    });

    updates.push(MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".into(),
        price: dec!(101500),
        ts: now + Duration::seconds(60),
    });

    // 6. Event expires (settlement)
    updates.push(MarketUpdate::EventExpired {
        event_id: "evt-btc-001".into(),
        end_time: event_end,
        resolved_up_won: None, // synthetic data — use spot fallback
    });

    updates
}

fn build_scenario_with_sports() -> Vec<MarketUpdate> {
    let mut updates = build_scenario();
    let now = Utc::now();

    updates.insert(
        1,
        MarketUpdate::SportsState {
            game_id: "19439".into(),
            league: "nfl".into(),
            slug: "nfl-lac-buf-2025-01-26".into(),
            home_team: "LAC".into(),
            away_team: "BUF".into(),
            status: "Scheduled".into(),
            period: Some("1Q".into()),
            score: Some("0-0".into()),
            elapsed: Some("0:00".into()),
            live: false,
            ended: false,
            finished_at: None,
            ts: now + Duration::seconds(1),
        },
    );
    updates.push(MarketUpdate::SportsState {
        game_id: "19439".into(),
        league: "nfl".into(),
        slug: "nfl-lac-buf-2025-01-26".into(),
        home_team: "LAC".into(),
        away_team: "BUF".into(),
        status: "Final".into(),
        period: Some("FT".into()),
        score: Some("17-24".into()),
        elapsed: None,
        live: false,
        ended: true,
        finished_at: Some(now + Duration::minutes(5)),
        ts: now + Duration::minutes(5),
    });

    updates
}

fn build_scenario_with_reference_data() -> Vec<MarketUpdate> {
    let mut updates = build_scenario();
    let now = Utc::now();

    updates.insert(
        1,
        MarketUpdate::ReferencePrice {
            symbol: "aapl".into(),
            source: "pyth".into(),
            asset_class: "equity".into(),
            price: dec!(212.45),
            full_accuracy_value: Some("212.450000".into()),
            is_carried_forward: false,
            received_at: Some(now + Duration::seconds(1)),
            ts: now + Duration::seconds(1),
        },
    );
    updates.push(MarketUpdate::ReferencePrice {
        symbol: "xauusd".into(),
        source: "pyth".into(),
        asset_class: "precious_metal".into(),
        price: dec!(3098.20),
        full_accuracy_value: None,
        is_carried_forward: true,
        received_at: Some(now + Duration::minutes(5)),
        ts: now + Duration::minutes(5),
    });

    updates
}

#[tokio::test]
async fn backtest_full_loop_produces_entry() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.02,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: 0.015,
        reversal_max_drift_flip_age_secs: 20,
        reversal_min_post_flip_drift: 0.0001,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: 1.3,
        reversal_max_ask_for_reversal: 0.25,
        reversal_max_pm_lag_secs: 30,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        three_layer_strategy_profile: ThreeLayerProfile::Mixed,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 0,
        stake_usd: dec!(25),
        max_positions: 1000,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![],
        three_layer_min_direction_prob: 0.56,
        three_layer_min_distance_over_sigma: 0.3,
        three_layer_min_confirmation_score: 0.10,
        three_layer_require_confirmation: false,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.03,
        three_layer_min_reward_risk: 1.2,
        three_layer_alpha_contrarian: false,
        three_layer_cex_contrarian: false,
        three_layer_probability_shrink: 1.0,
        three_layer_probability_haircut: 0.0,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
        three_layer_autofactor_runtime_score: None,
        three_layer_event_ml_model_path: None,
    };

    let strategy = DirectionalStrategy::new(config);
    let feed = HistoricalFeed::new(build_scenario());
    let executor = SimulatedExecutor::new(SimulatedExecutorConfig {
        use_spread: true,
        spread_pct: dec!(0.02),
        enable_partial_fills: false,
        enable_market_impact: false,
        ..Default::default()
    });
    let recorder = Box::new(NullRecorder);
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::Backtest,
        throttle_hz: None,
        max_updates: None,
        skip_settlement_exits: false,
    };

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;

    assert_eq!(result.mode, RuntimeMode::Backtest);
    assert_eq!(result.updates_processed, 8);
    assert!(
        result.intents_submitted >= 1,
        "Expected at least 1 intent, got {}",
        result.intents_submitted
    );
    assert!(
        result.fills_recorded >= 1,
        "Expected at least 1 fill, got {}",
        result.fills_recorded
    );

    // Position should exist
    let trading = runtime.trading();
    let snapshot = trading.snapshot(&std::collections::BTreeMap::new());
    assert!(
        !snapshot.fills.is_empty(),
        "Expected fills in trading runtime"
    );
}

#[tokio::test]
async fn toml_config_drives_backtest() {
    let toml = r#"
[runtime]
mode = "backtest"

[strategy]
symbols = ["BTCUSDT"]
min_edge = 0.02
min_time_remaining_secs = 60
max_time_remaining_secs = 300
cooldown_secs = 0
stake_usd = 25.0
max_positions = 1000
max_daily_trades = 1000

[execution]
use_spread = false
enable_partial_fills = false
enable_market_impact = false
"#;

    let config = FullConfig::from_toml(toml).unwrap();
    let sim_config = config.sim_executor_config();
    let runtime_config = config.runtime_config();

    let strategy = DirectionalStrategy::new(config.strategy);
    let feed = HistoricalFeed::new(build_scenario());
    let executor = SimulatedExecutor::new(sim_config);
    let recorder = Box::new(NullRecorder);

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;

    assert_eq!(result.mode, RuntimeMode::Backtest);
    // No spread/impact → fill at signal price
    assert!(result.fills_recorded >= 1);
}

#[tokio::test]
async fn empty_feed_produces_zero_trades() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.02,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: 0.015,
        reversal_max_drift_flip_age_secs: 20,
        reversal_min_post_flip_drift: 0.0001,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: 1.3,
        reversal_max_ask_for_reversal: 0.25,
        reversal_max_pm_lag_secs: 30,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        three_layer_strategy_profile: ThreeLayerProfile::Mixed,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 0,
        stake_usd: dec!(25),
        max_positions: 1000,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![],
        three_layer_min_direction_prob: 0.56,
        three_layer_min_distance_over_sigma: 0.3,
        three_layer_min_confirmation_score: 0.10,
        three_layer_require_confirmation: false,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.03,
        three_layer_min_reward_risk: 1.2,
        three_layer_alpha_contrarian: false,
        three_layer_cex_contrarian: false,
        three_layer_probability_shrink: 1.0,
        three_layer_probability_haircut: 0.0,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
        three_layer_autofactor_runtime_score: None,
        three_layer_event_ml_model_path: None,
    };

    let strategy = DirectionalStrategy::new(config);
    let feed = HistoricalFeed::new(vec![]); // empty
    let executor = SimulatedExecutor::new(SimulatedExecutorConfig::default());
    let recorder = Box::new(NullRecorder);
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::Backtest,
        throttle_hz: None,
        max_updates: None,
        skip_settlement_exits: false,
    };

    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;

    assert_eq!(result.updates_processed, 0);
    assert_eq!(result.intents_submitted, 0);
    assert_eq!(result.fills_recorded, 0);
}

#[tokio::test]
async fn recorded_updates_replay_to_the_same_runtime_result() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.02,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: 0.015,
        reversal_max_drift_flip_age_secs: 20,
        reversal_min_post_flip_drift: 0.0001,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: 1.3,
        reversal_max_ask_for_reversal: 0.25,
        reversal_max_pm_lag_secs: 30,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        three_layer_strategy_profile: ThreeLayerProfile::Mixed,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 0,
        stake_usd: dec!(25),
        max_positions: 1000,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![],
        three_layer_min_direction_prob: 0.56,
        three_layer_min_distance_over_sigma: 0.3,
        three_layer_min_confirmation_score: 0.10,
        three_layer_require_confirmation: false,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.03,
        three_layer_min_reward_risk: 1.2,
        three_layer_alpha_contrarian: false,
        three_layer_cex_contrarian: false,
        three_layer_probability_shrink: 1.0,
        three_layer_probability_haircut: 0.0,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
        three_layer_autofactor_runtime_score: None,
        three_layer_event_ml_model_path: None,
    };
    let sim_config = SimulatedExecutorConfig {
        use_spread: true,
        spread_pct: dec!(0.02),
        enable_partial_fills: false,
        enable_market_impact: false,
        ..Default::default()
    };
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::DryRun,
        throttle_hz: None,
        max_updates: None,
        skip_settlement_exits: false,
    };

    let mut record_path = std::env::temp_dir();
    record_path.push(format!(
        "ploy-replay-parity-{}.ndjson",
        uuid::Uuid::new_v4()
    ));

    let record_feed =
        RecordingFeed::new(HistoricalFeed::new(build_scenario()), &record_path).unwrap();
    let mut recorded_runtime = StrategyRuntime::new(
        DirectionalStrategy::new(config.clone()),
        record_feed,
        SimulatedExecutor::new(sim_config.clone()),
        Box::new(NullRecorder),
        runtime_config.clone(),
    )
    .with_deployment_id("test.recorded.dryrun");
    let recorded_result = recorded_runtime.run().await;
    let recorded_snapshot = recorded_runtime
        .trading()
        .snapshot(&std::collections::BTreeMap::new());

    let replay_feed = RecordedFeed::from_path(&record_path).unwrap();
    let mut replay_runtime = StrategyRuntime::new(
        DirectionalStrategy::new(config),
        replay_feed,
        SimulatedExecutor::new(sim_config),
        Box::new(NullRecorder),
        RuntimeConfig {
            mode: RuntimeMode::Replay,
            ..runtime_config
        },
    );
    let replay_result = replay_runtime.run().await;
    let replay_snapshot = replay_runtime
        .trading()
        .snapshot(&std::collections::BTreeMap::new());

    assert_eq!(
        recorded_result.updates_processed,
        replay_result.updates_processed
    );
    assert_eq!(
        recorded_result.intents_submitted,
        replay_result.intents_submitted
    );
    assert_eq!(recorded_result.fills_recorded, replay_result.fills_recorded);
    assert_eq!(recorded_result.pnl.net_pnl(), replay_result.pnl.net_pnl());
    assert_eq!(
        recorded_snapshot.fill_cashflow_summary(),
        replay_snapshot.fill_cashflow_summary()
    );
    assert_eq!(recorded_snapshot.fills.len(), replay_snapshot.fills.len());

    let _ = fs::remove_file(record_path);
}

#[tokio::test]
async fn sports_updates_round_trip_without_changing_crypto_runtime_behavior() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.02,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: 0.015,
        reversal_max_drift_flip_age_secs: 20,
        reversal_min_post_flip_drift: 0.0001,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: 1.3,
        reversal_max_ask_for_reversal: 0.25,
        reversal_max_pm_lag_secs: 30,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        three_layer_strategy_profile: ThreeLayerProfile::Mixed,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 0,
        stake_usd: dec!(25),
        max_positions: 1000,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![],
        three_layer_min_direction_prob: 0.56,
        three_layer_min_distance_over_sigma: 0.3,
        three_layer_min_confirmation_score: 0.10,
        three_layer_require_confirmation: false,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.03,
        three_layer_min_reward_risk: 1.2,
        three_layer_alpha_contrarian: false,
        three_layer_cex_contrarian: false,
        three_layer_probability_shrink: 1.0,
        three_layer_probability_haircut: 0.0,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
        three_layer_autofactor_runtime_score: None,
        three_layer_event_ml_model_path: None,
    };
    let sim_config = SimulatedExecutorConfig {
        use_spread: true,
        spread_pct: dec!(0.02),
        enable_partial_fills: false,
        enable_market_impact: false,
        ..Default::default()
    };
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::DryRun,
        throttle_hz: None,
        max_updates: None,
        skip_settlement_exits: false,
    };

    let mut record_path = std::env::temp_dir();
    record_path.push(format!(
        "ploy-sports-replay-parity-{}.ndjson",
        uuid::Uuid::new_v4()
    ));

    let record_feed = RecordingFeed::new(
        HistoricalFeed::new(build_scenario_with_sports()),
        &record_path,
    )
    .unwrap();
    let mut recorded_runtime = StrategyRuntime::new(
        DirectionalStrategy::new(config.clone()),
        record_feed,
        SimulatedExecutor::new(sim_config.clone()),
        Box::new(NullRecorder),
        runtime_config.clone(),
    )
    .with_deployment_id("test.recorded.dryrun");
    let recorded_result = recorded_runtime.run().await;
    drop(recorded_runtime);

    let replay_feed = RecordedFeed::from_path(&record_path).unwrap();
    let mut replay_runtime = StrategyRuntime::new(
        DirectionalStrategy::new(config),
        replay_feed,
        SimulatedExecutor::new(sim_config),
        Box::new(NullRecorder),
        RuntimeConfig {
            mode: RuntimeMode::Replay,
            ..runtime_config
        },
    );
    let replay_result = replay_runtime.run().await;

    assert_eq!(
        recorded_result.updates_processed,
        replay_result.updates_processed
    );
    assert_eq!(
        recorded_result.intents_submitted,
        replay_result.intents_submitted
    );
    assert_eq!(recorded_result.fills_recorded, replay_result.fills_recorded);
    assert_eq!(recorded_result.pnl.net_pnl(), replay_result.pnl.net_pnl());

    let _ = fs::remove_file(record_path);
}

#[tokio::test]
async fn reference_updates_round_trip_without_changing_crypto_runtime_behavior() {
    let config = DirectionalConfig {
        symbols: vec!["BTCUSDT".into()],
        symbol_profiles: std::collections::HashMap::new(),
        vol_floor: 0.001,
        min_probability: 0.55,
        min_z_score: 0.35,
        min_entry_price: 0.15,
        max_entry_price: 0.85,
        no_trade_zone_min: 0.45,
        no_trade_zone_max: 0.55,
        min_edge: 0.02,
        min_deviation_pct: 0.005,
        min_reversal_consistency: 0.55,
        min_trend_consistency: 0.50,
        min_trend_persistence_secs: 0,
        take_profit_price_delta: 0.10,
        stop_loss_price_delta: 0.05,
        max_hold_secs: 120,
        reversal_bonus_cap: 0.20,
        use_multiscale_volatility: true,
        use_price_structure_adjustment: true,
        reversal_max_distance_pct: 0.015,
        reversal_max_drift_flip_age_secs: 20,
        reversal_min_post_flip_drift: 0.0001,
        reversal_lob_depth_pct: 0.001,
        reversal_min_lob_depth_ratio: 1.3,
        reversal_max_ask_for_reversal: 0.25,
        reversal_max_pm_lag_secs: 30,
        reversal_take_profit_ask: 0.65,
        reversal_stop_distance_pct: 0.025,
        three_layer_strategy_profile: ThreeLayerProfile::Mixed,
        min_time_remaining_secs: 60,
        max_time_remaining_secs: 300,
        cooldown_secs: 0,
        stake_usd: dec!(25),
        max_positions: 1000,
        max_daily_trades: 1000,
        max_daily_loss_usd: None,
        allowed_window_secs: vec![],
        three_layer_min_direction_prob: 0.56,
        three_layer_min_distance_over_sigma: 0.3,
        three_layer_min_confirmation_score: 0.10,
        three_layer_require_confirmation: false,
        three_layer_min_drift_confirmation: 0.0002,
        three_layer_min_edge: 0.03,
        three_layer_min_reward_risk: 1.2,
        three_layer_alpha_contrarian: false,
        three_layer_cex_contrarian: false,
        three_layer_probability_shrink: 1.0,
        three_layer_probability_haircut: 0.0,
        three_layer_take_profit_ask: 0.70,
        three_layer_stop_distance_pct: 0.020,
        three_layer_max_pm_lag_secs: 15,
        three_layer_min_entry_score: 0.30,
        three_layer_autofactor_runtime_score: None,
        three_layer_event_ml_model_path: None,
    };
    let sim_config = SimulatedExecutorConfig {
        use_spread: true,
        spread_pct: dec!(0.02),
        enable_partial_fills: false,
        enable_market_impact: false,
        ..Default::default()
    };
    let runtime_config = RuntimeConfig {
        mode: RuntimeMode::DryRun,
        throttle_hz: None,
        max_updates: None,
        skip_settlement_exits: false,
    };

    let mut record_path = std::env::temp_dir();
    record_path.push(format!(
        "ploy-reference-replay-parity-{}.ndjson",
        uuid::Uuid::new_v4()
    ));

    let record_feed = RecordingFeed::new(
        HistoricalFeed::new(build_scenario_with_reference_data()),
        &record_path,
    )
    .unwrap();
    let mut recorded_runtime = StrategyRuntime::new(
        DirectionalStrategy::new(config.clone()),
        record_feed,
        SimulatedExecutor::new(sim_config.clone()),
        Box::new(NullRecorder),
        runtime_config.clone(),
    )
    .with_deployment_id("test.recorded.dryrun");
    let recorded_result = recorded_runtime.run().await;
    drop(recorded_runtime);

    let replay_feed = RecordedFeed::from_path(&record_path).unwrap();
    let mut replay_runtime = StrategyRuntime::new(
        DirectionalStrategy::new(config),
        replay_feed,
        SimulatedExecutor::new(sim_config),
        Box::new(NullRecorder),
        RuntimeConfig {
            mode: RuntimeMode::Replay,
            ..runtime_config
        },
    );
    let replay_result = replay_runtime.run().await;

    assert_eq!(
        recorded_result.updates_processed,
        replay_result.updates_processed
    );
    assert_eq!(
        recorded_result.intents_submitted,
        replay_result.intents_submitted
    );
    assert_eq!(recorded_result.fills_recorded, replay_result.fills_recorded);
    assert_eq!(recorded_result.pnl.net_pnl(), replay_result.pnl.net_pnl());

    let _ = fs::remove_file(record_path);
}
