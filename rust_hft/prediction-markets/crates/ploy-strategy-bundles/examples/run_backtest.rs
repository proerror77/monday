//! Runnable backtest example for PM5D strategy variants.
//!
//! Usage (synthetic):
//!   cargo run -p ploy-strategy-bundles --example run_backtest -- [config.toml]
//!
//! Usage (database):
//!   cargo run -p ploy-strategy-bundles --example run_backtest -- \
//!     --config config/strategies/02-pm5d.unified.toml \
//!     --db-url postgresql://... \
//!     --start-date 2026-03-28 \
//!     --end-date 2026-04-03
//!
//! If no --db-url is given, uses synthetic market data.

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::strategies::directional::DirectionalConfig;
use ploy_strategy_bundles::{
    config::FullConfig, DirectionalStrategy, HistoricalFeed, MarketUpdate, NullRecorder,
    ReversalStrategy, RuntimeConfig, RuntimeMode, SimulatedExecutor, SimulatedExecutorConfig,
    StrategyLogic, StrategyRuntime, ThreeLayerProfile, ThreeLayerStrategy,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::Instant;

/// Generate synthetic market data: 1 hour of 5-min windows for 3 symbols.
///
/// Each window: event discovered → spot ticks every 10s → quotes update → event expires.
/// Spot prices random-walk around a base with momentum bursts to trigger signals.
fn generate_synthetic_data(symbols: &[&str], duration_mins: u64) -> Vec<MarketUpdate> {
    let mut updates = Vec::new();
    let start = Utc::now() - Duration::minutes(duration_mins as i64);
    let window_secs = 300u64; // 5 min windows

    let base_prices: Vec<Decimal> = vec![dec!(100000), dec!(3500), dec!(140)]; // BTC, ETH, SOL

    for window_idx in 0..(duration_mins * 60 / window_secs) {
        let window_start = start + Duration::seconds((window_idx * window_secs) as i64);
        let window_end = window_start + Duration::seconds(window_secs as i64);

        for (sym_idx, &symbol) in symbols.iter().enumerate() {
            let base = base_prices[sym_idx % base_prices.len()];
            let sym: Arc<str> = Arc::from(symbol);
            let event_id: Arc<str> =
                Arc::from(format!("evt-{}-{}", symbol.to_lowercase(), window_idx));
            let up_token: Arc<str> =
                Arc::from(format!("up-{}-{}", symbol.to_lowercase(), window_idx));
            let dn_token: Arc<str> =
                Arc::from(format!("dn-{}-{}", symbol.to_lowercase(), window_idx));

            // Initial spot (open price)
            updates.push(MarketUpdate::SpotPrice {
                symbol: Arc::clone(&sym),
                price: base,
                ts: window_start,
            });

            // Event discovered
            updates.push(MarketUpdate::EventDiscovered {
                event_id: Arc::clone(&event_id),
                symbol: Arc::clone(&sym),
                up_token: Arc::clone(&up_token),
                down_token: Arc::clone(&dn_token),
                end_time: window_end,
                window_secs,
                price_to_beat: None,
                resolved_up_won: None,
            });

            // Simulate spot price ticks every 10s
            // Alternate: odd windows drift up, even windows drift down
            let drift: Decimal = if window_idx % 3 == 0 {
                // Strong up move (1.5%) — should trigger UP entry
                base * dec!(0.015)
            } else if window_idx % 3 == 1 {
                // Strong down move (-1.2%) — should trigger DOWN entry
                -(base * dec!(0.012))
            } else {
                // Flat (0.1%) — should NOT trigger entry
                base * dec!(0.001)
            };

            // Quote updates — price reflects market's lagging assessment
            let up_ask = if drift > Decimal::ZERO {
                dec!(0.55) // market slightly favors up
            } else if drift < -(base * dec!(0.005)) {
                dec!(0.30) // market still thinks it might go up
            } else {
                dec!(0.50) // no-trade zone
            };

            updates.push(MarketUpdate::Quote {
                token_id: Arc::clone(&up_token),
                bid: Some(up_ask - dec!(0.01)),
                ask: Some(up_ask),
                ts: window_start + Duration::seconds(5),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            });
            updates.push(MarketUpdate::Quote {
                token_id: Arc::clone(&dn_token),
                bid: Some(dec!(1) - up_ask - dec!(0.01)),
                ask: Some(dec!(1) - up_ask),
                ts: window_start + Duration::seconds(5),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            });

            // Spot ticks showing the drift
            for tick in 1..=5 {
                let t = window_start + Duration::seconds(tick * 10);
                let pct = Decimal::from(tick) / dec!(5);
                let price = base + drift * pct;
                updates.push(MarketUpdate::SpotPrice {
                    symbol: Arc::clone(&sym),
                    price,
                    ts: t,
                });
            }

            // Updated quotes after price move
            let final_price = base + drift;
            let p_up_model = if drift > Decimal::ZERO {
                dec!(0.80)
            } else {
                dec!(0.20)
            };
            let up_ask_final = if drift.abs() > base * dec!(0.005) {
                // Market catches up partially
                p_up_model - dec!(0.10)
            } else {
                dec!(0.50) // no-trade zone
            };

            updates.push(MarketUpdate::Quote {
                token_id: up_token,
                bid: Some(up_ask_final - dec!(0.01)),
                ask: Some(up_ask_final),
                ts: window_start + Duration::seconds(55),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            });
            updates.push(MarketUpdate::Quote {
                token_id: dn_token,
                bid: Some(dec!(1) - up_ask_final - dec!(0.01)),
                ask: Some(dec!(1) - up_ask_final),
                ts: window_start + Duration::seconds(55),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            });

            // Spot at window midpoint (entry zone: 60-300s remaining)
            updates.push(MarketUpdate::SpotPrice {
                symbol: sym,
                price: final_price,
                ts: window_start + Duration::seconds(120), // 180s remaining
            });

            // Event expires
            updates.push(MarketUpdate::EventExpired {
                event_id,
                end_time: window_end,
                resolved_up_won: None, // synthetic data — use spot fallback
            });
        }
    }

    // Sort by timestamp
    updates.sort_by_key(MarketUpdate::sort_ts);

    updates
}

/// Parse a named flag value from args: `--flag value`
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn force_backtest_mode(mut config: RuntimeConfig) -> RuntimeConfig {
    config.mode = RuntimeMode::Backtest;
    config.skip_settlement_exits = false;
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_backtest_forces_backtest_runtime_mode() {
        let config = force_backtest_mode(RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: Some(10),
            max_updates: Some(123),
            skip_settlement_exits: true,
        });

        assert_eq!(config.mode, RuntimeMode::Backtest);
        assert_eq!(config.throttle_hz, Some(10));
        assert_eq!(config.max_updates, Some(123));
        assert!(!config.skip_settlement_exits);
    }

    #[test]
    fn normalized_runtime_evidence_exposes_order_and_fill_rows() {
        let timestamp = Utc::now();
        let snapshot = ploy_trading::TradingRuntimeSnapshot {
            intents: vec![ploy_trading::TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "pm5d-test".to_string(),
                market_id: "event-1".to_string(),
                token_id: "token-up".to_string(),
                side: ploy_trading::TradeSide::Buy,
                quantity: dec!(10),
                limit_price: Some(dec!(0.42)),
                purpose: ploy_trading::IntentPurpose::Entry,
                created_at: timestamp,
            }],
            orders: vec![ploy_trading::OrderRecord {
                order_id: "order-1".to_string(),
                intent_id: "intent-1".to_string(),
                deployment_id: "pm5d-test".to_string(),
                token_id: "token-up".to_string(),
                requested_qty: dec!(10),
                limit_price: Some(dec!(0.42)),
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: vec![],
                revision: 0,
                idempotency_key: None,
                state: ploy_trading::OrderState::Filled,
                state_changed_at: Some(timestamp),
                filled_qty: dec!(10),
                rejection_reason: None,
                last_error: None,
            }],
            fills: vec![ploy_trading::FillRecord {
                fill_id: "fill-1".to_string(),
                order_id: "order-1".to_string(),
                token_id: "token-up".to_string(),
                side: ploy_trading::TradeSide::Buy,
                quantity: dec!(10),
                price: dec!(0.41),
                fee: Decimal::ZERO,
                timestamp,
            }],
            ..Default::default()
        };

        let evidence = normalized_runtime_evidence(&snapshot);

        assert_eq!(evidence["basis"], "trading_runtime_snapshot");
        assert_eq!(evidence["events"][0]["event_id"], "event-1");
        assert!(evidence["events"][0]["decision_ts"].as_str().is_some());
        assert_eq!(evidence["events"][0]["quote"], "0.42");
        assert_eq!(evidence["events"][0]["signal_inputs"]["purpose"], "ENTRY");
        assert_eq!(evidence["events"][0]["side"], "BUY");
        assert_eq!(evidence["events"][0]["entry_price"], "0.41");
        assert_eq!(evidence["events"][0]["fill_status"], "FILLED");
        assert_eq!(evidence["events"][0]["settlement"], "open");
        assert_eq!(evidence["events"][0]["pnl"], "-4.10");
        assert_eq!(evidence["orders"][0]["deployment_id"], "pm5d-test");
        assert_eq!(evidence["orders"][0]["intent_id"], "intent-1");
        assert_eq!(evidence["orders"][0]["status"], "FILLED");
        assert_eq!(evidence["fills"][0]["fill_id"], "fill-1");
        assert_eq!(evidence["fills"][0]["fill_side"], "BUY");
    }

    #[test]
    fn backtest_evidence_tracks_event_uniqueness_and_closed_drawdown() {
        let started = Utc::now();
        let mut snapshot = ploy_trading::TradingRuntimeSnapshot::default();
        for (index, (event_id, exit_price)) in [
            ("event-1", dec!(0.60)),
            ("event-2", dec!(0.25)),
            ("event-3", dec!(0.55)),
        ]
        .into_iter()
        .enumerate()
        {
            let entry_intent_id = format!("entry-{index}");
            let exit_intent_id = format!("exit-{index}");
            let entry_order_id = format!("entry-order-{index}");
            let exit_order_id = format!("exit-order-{index}");
            let token_id = format!("token-{index}");
            let opened_at = started + Duration::seconds(index as i64 * 10);
            let closed_at = opened_at + Duration::seconds(5);
            snapshot.intents.extend([
                ploy_trading::TradingIntent {
                    intent_id: entry_intent_id.clone(),
                    deployment_id: "pm5d-test".into(),
                    market_id: event_id.into(),
                    token_id: token_id.clone(),
                    side: ploy_trading::TradeSide::Buy,
                    quantity: dec!(100),
                    limit_price: Some(dec!(0.50)),
                    purpose: ploy_trading::IntentPurpose::Entry,
                    created_at: opened_at,
                },
                ploy_trading::TradingIntent {
                    intent_id: exit_intent_id.clone(),
                    deployment_id: "pm5d-test".into(),
                    market_id: event_id.into(),
                    token_id: token_id.clone(),
                    side: ploy_trading::TradeSide::Sell,
                    quantity: dec!(100),
                    limit_price: Some(exit_price),
                    purpose: ploy_trading::IntentPurpose::Exit,
                    created_at: closed_at,
                },
            ]);
            for (order_id, intent_id) in [
                (entry_order_id.clone(), entry_intent_id.clone()),
                (exit_order_id.clone(), exit_intent_id.clone()),
            ] {
                snapshot.orders.push(ploy_trading::OrderRecord {
                    order_id,
                    intent_id,
                    deployment_id: "pm5d-test".into(),
                    token_id: token_id.clone(),
                    requested_qty: dec!(100),
                    limit_price: Some(dec!(0.50)),
                    venue_order_id: None,
                    venue_order_history: Vec::new(),
                    revision: 0,
                    idempotency_key: None,
                    state: ploy_trading::OrderState::Filled,
                    filled_qty: dec!(100),
                    rejection_reason: None,
                    last_error: None,
                });
            }
            snapshot.fills.extend([
                ploy_trading::FillRecord {
                    fill_id: format!("entry-fill-{index}"),
                    order_id: entry_order_id,
                    token_id: token_id.clone(),
                    side: ploy_trading::TradeSide::Buy,
                    quantity: dec!(100),
                    price: dec!(0.50),
                    fee: Decimal::ZERO,
                    timestamp: opened_at,
                },
                ploy_trading::FillRecord {
                    fill_id: format!("exit-fill-{index}"),
                    order_id: exit_order_id,
                    token_id,
                    side: ploy_trading::TradeSide::Sell,
                    quantity: dec!(100),
                    price: exit_price,
                    fee: Decimal::ZERO,
                    timestamp: closed_at,
                },
            ]);
        }
        snapshot.intents.push(ploy_trading::TradingIntent {
            intent_id: "duplicate-event-1".into(),
            deployment_id: "pm5d-test".into(),
            market_id: "event-1".into(),
            token_id: "token-0".into(),
            side: ploy_trading::TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.50)),
            purpose: ploy_trading::IntentPurpose::Entry,
            created_at: started + Duration::seconds(1),
        });
        snapshot.intents.push(ploy_trading::TradingIntent {
            intent_id: "orphan-exit".into(),
            deployment_id: "pm5d-test".into(),
            market_id: "event-without-entry".into(),
            token_id: "orphan-token".into(),
            side: ploy_trading::TradeSide::Sell,
            quantity: dec!(1),
            limit_price: Some(dec!(0.75)),
            purpose: ploy_trading::IntentPurpose::Exit,
            created_at: started + Duration::seconds(40),
        });
        snapshot.orders.push(ploy_trading::OrderRecord {
            order_id: "orphan-exit-order".into(),
            intent_id: "orphan-exit".into(),
            deployment_id: "pm5d-test".into(),
            token_id: "orphan-token".into(),
            requested_qty: dec!(1),
            limit_price: Some(dec!(0.75)),
            venue_order_id: None,
            venue_order_history: Vec::new(),
            revision: 0,
            idempotency_key: None,
            state: ploy_trading::OrderState::Filled,
            filled_qty: dec!(1),
            rejection_reason: None,
            last_error: None,
        });
        snapshot.fills.push(ploy_trading::FillRecord {
            fill_id: "orphan-exit-fill".into(),
            order_id: "orphan-exit-order".into(),
            token_id: "orphan-token".into(),
            side: ploy_trading::TradeSide::Sell,
            quantity: dec!(1),
            price: dec!(0.75),
            fee: Decimal::ZERO,
            timestamp: started + Duration::seconds(40),
        });

        let metrics = backtest_evidence_metrics(&snapshot);

        assert_eq!(metrics.unique_event_count, 3);
        assert_eq!(metrics.max_event_decisions, 2);
        assert_eq!(metrics.closed_event_count, 3);
        assert_eq!(metrics.open_event_count, 0);
        assert_eq!(metrics.lifecycle_without_entry_decision_count, 1);
        assert_eq!(metrics.max_drawdown, dec!(-25));
    }
}

fn main() {
    // Parse CLI flags
    let args: Vec<String> = std::env::args().collect();
    let total_started = Instant::now();
    let config_path = flag_value(&args, "--config")
        .or_else(|| args.get(1).filter(|a| !a.starts_with('-')).cloned());
    let db_url = flag_value(&args, "--db-url");
    let data_dir = flag_value(&args, "--data-dir");
    let start_date = flag_value(&args, "--start-date");
    let end_date = flag_value(&args, "--end-date");
    let timing_json = flag_value(&args, "--timing-json");
    let output_json = flag_value(&args, "--output-json");

    let (strategy_variant, strategy_config, sim_config, runtime_config, backtest_options) =
        if let Some(ref path) = config_path {
            let config = FullConfig::from_file(path).expect("Failed to parse config");
            let sim = config.sim_executor_config();
            let rt = force_backtest_mode(config.runtime_config());
            let strategy_variant = config.runtime.canonical_strategy_variant();
            let backtest_options = HistoricalLoadOptions {
                include_reference_prices: config.backtest_data.include_reference_prices,
                reference_symbols: config
                    .backtest_data
                    .reference_symbols(&config.reference_data),
                include_sports_state: config.backtest_data.include_sports_state,
                require_official_settlement: config.backtest_data.require_official_settlement,
                include_l2: true,
                lob_sample_secs: 30,
                spot_sample_secs: 1,
                max_source_delay_secs: 30,
            };
            (strategy_variant, config.strategy, sim, rt, backtest_options)
        } else {
            eprintln!("No config file provided, using built-in defaults\n");
            (
                "directional".to_string(),
                DirectionalConfig {
                    symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
                    symbol_profiles: std::collections::HashMap::new(),
                    vol_floor: 0.001,
                    min_probability: 0.62,
                    min_z_score: 0.35,
                    min_entry_price: 0.15,
                    max_entry_price: 0.85,
                    no_trade_zone_min: 0.45,
                    no_trade_zone_max: 0.55,
                    min_edge: 0.05,
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
                    cooldown_secs: 60,
                    stake_usd: dec!(25),
                    max_positions: 3,
                    max_daily_trades: 1000,
                    max_daily_loss_usd: None,
                    allowed_window_secs: vec![300, 900],
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
                },
                SimulatedExecutorConfig {
                    use_spread: true,
                    spread_pct: dec!(0.02),
                    enable_partial_fills: false,
                    enable_market_impact: true,
                    impact_coefficient: dec!(0.1),
                    ..Default::default()
                },
                force_backtest_mode(RuntimeConfig {
                    mode: RuntimeMode::Backtest,
                    throttle_hz: None,
                    max_updates: None,
                    skip_settlement_exits: false,
                }),
                HistoricalLoadOptions::default(),
            )
        };

    eprintln!("=== PM5D Backtest ===");
    eprintln!("Mode:    {:?}", runtime_config.mode);
    eprintln!("Variant: {strategy_variant}");
    eprintln!("Symbols: {:?}", strategy_config.symbols);
    eprintln!(
        "Params:  min_edge={:.0}% min_p={:.0}% cooldown={}s",
        strategy_config.min_edge * 100.0,
        strategy_config.min_probability * 100.0,
        strategy_config.cooldown_secs,
    );
    eprintln!();

    // Build tokio runtime for async DB loading
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let stake_usd = strategy_config.stake_usd;
    let strategy: Box<dyn StrategyLogic> = match strategy_variant.as_str() {
        "directional" => Box::new(DirectionalStrategy::new(strategy_config.clone())),
        "reversal" => Box::new(ReversalStrategy::new(strategy_config.clone().into())),
        "three_layer" => Box::new(ThreeLayerStrategy::new(strategy_config.clone().into())),
        other => panic!("unsupported strategy_variant in run_backtest example: {other}"),
    };
    let executor = SimulatedExecutor::new(sim_config);
    let recorder = Box::new(NullRecorder);

    // When --data-dir is set, use StreamingParquetFeed for O(1) memory usage.
    // Otherwise fall back to Vec-backed HistoricalFeed (DB or synthetic).
    if let Some(ref dir) = data_dir {
        #[cfg(not(feature = "parquet-feed"))]
        {
            let _ = dir;
            eprintln!("--data-dir requires the `parquet-feed` feature");
            std::process::exit(1);
        }

        #[cfg(feature = "parquet-feed")]
        {
            use ploy_strategy_bundles::feed::parquet_stream::StreamingParquetFeed;

            let from = start_date.as_deref().unwrap_or("2026-03-28");
            let to = end_date.as_deref().unwrap_or("2026-04-03");
            let from_dt = Utc.from_utc_datetime(
                &NaiveDate::parse_from_str(from, "%Y-%m-%d")
                    .expect("Invalid --start-date (use YYYY-MM-DD)")
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            );
            let to_dt = Utc.from_utc_datetime(
                &NaiveDate::parse_from_str(to, "%Y-%m-%d")
                    .expect("Invalid --end-date (use YYYY-MM-DD)")
                    .and_hms_opt(23, 59, 59)
                    .unwrap(),
            );
            eprintln!("Streaming Parquet data from: {dir} ({from} → {to})");
            let source_started = Instant::now();
            let feed = StreamingParquetFeed::new(
                dir,
                &strategy_config.symbols,
                from_dt,
                to_dt,
                &backtest_options,
            );
            let source_open_secs = source_started.elapsed().as_secs_f64();
            let mut runtime =
                StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
            let run_started = Instant::now();
            let result = rt.block_on(runtime.run());
            let runtime_wall_secs = run_started.elapsed().as_secs_f64();
            let mark_prices = BTreeMap::new();
            let snapshot = runtime.trading().snapshot(&mark_prices);
            write_timing_json(
                timing_json.as_deref(),
                json!({
                    "command": "run_backtest",
                    "source": "parquet-stream",
                    "config": config_path.as_deref(),
                    "strategy_variant": &strategy_variant,
                    "symbols": &strategy_config.symbols,
                    "start_date": from,
                    "end_date": to,
                    "data_dir": dir,
                    "source_open_secs": round_secs(source_open_secs),
                    "runtime_wall_secs": round_secs(runtime_wall_secs),
                    "runtime_elapsed_secs": round_secs(result.elapsed_secs),
                    "total_wall_secs": round_secs(total_started.elapsed().as_secs_f64()),
                    "updates_processed": result.updates_processed,
                    "updates_per_sec": round_secs(throughput(result.updates_processed, runtime_wall_secs)),
                    "fills_recorded": result.fills_recorded,
                    "intents_submitted": result.intents_submitted,
                    "net_pnl": result.pnl.net_pnl().to_string(),
                    "open_positions": result.risk.open_positions,
                    "gross_exposure": result.risk.gross_exposure.to_string(),
                }),
            );
            print_results(&result, &snapshot, stake_usd);
            write_backtest_evaluation_json(
                output_json.as_deref(),
                &strategy_variant,
                config_path.as_deref(),
                "parquet_stream",
                Some(dir.as_str()),
                start_date.as_deref(),
                end_date.as_deref(),
                &backtest_options,
                &result,
                &snapshot,
            );
        }
    } else {
        let eager_source_load_secs;
        let source_kind = if db_url.is_some() {
            "db_eager"
        } else {
            "synthetic"
        };
        let data: Vec<MarketUpdate> = if let Some(ref url) = db_url {
            let from = start_date.as_deref().unwrap_or("2026-03-28");
            let to = end_date.as_deref().unwrap_or("2026-04-03");
            let from_dt = Utc.from_utc_datetime(
                &NaiveDate::parse_from_str(from, "%Y-%m-%d")
                    .expect("Invalid --start-date (use YYYY-MM-DD)")
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            );
            let to_dt = Utc.from_utc_datetime(
                &NaiveDate::parse_from_str(to, "%Y-%m-%d")
                    .expect("Invalid --end-date (use YYYY-MM-DD)")
                    .and_hms_opt(23, 59, 59)
                    .unwrap(),
            );
            eprintln!("Loading DB data: {} → {}", from, to);
            let source_started = Instant::now();
            let pool = rt
                .block_on(PgPoolOptions::new().max_connections(5).connect(url))
                .expect("DB connection failed");
            let symbols: Vec<String> = strategy_config.symbols.clone();
            let updates = rt
                .block_on(load_from_database_with_options(
                    &pool,
                    &symbols,
                    from_dt,
                    to_dt,
                    &backtest_options,
                ))
                .expect("Failed to load from database");
            let source_load_secs = source_started.elapsed().as_secs_f64();
            eager_source_load_secs = Some(source_load_secs);
            eprintln!("Loaded {} market updates from DB\n", updates.len());
            print_data_breakdown(&updates);
            updates
        } else {
            let source_started = Instant::now();
            let updates = generate_synthetic_data(&["BTCUSDT", "ETHUSDT", "SOLUSDT"], 60);
            let source_load_secs = source_started.elapsed().as_secs_f64();
            eager_source_load_secs = Some(source_load_secs);
            eprintln!(
                "Generated {} market updates (1 hour synthetic)\n",
                updates.len()
            );
            eprintln!("Synthetic generation elapsed: {:.3}s", source_load_secs);
            updates
        };

        let feed = HistoricalFeed::new(data);
        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
        let run_started = Instant::now();
        let result = rt.block_on(runtime.run());
        let runtime_wall_secs = run_started.elapsed().as_secs_f64();
        let mark_prices = BTreeMap::new();
        let snapshot = runtime.trading().snapshot(&mark_prices);
        write_timing_json(
            timing_json.as_deref(),
            json!({
                "command": "run_backtest",
                "source": source_kind,
                "config": config_path.as_deref(),
                "strategy_variant": &strategy_variant,
                "symbols": &strategy_config.symbols,
                "start_date": start_date.as_deref(),
                "end_date": end_date.as_deref(),
                "source_load_secs": eager_source_load_secs.map(round_secs),
                "updates_loaded": result.updates_processed,
                "runtime_wall_secs": round_secs(runtime_wall_secs),
                "runtime_elapsed_secs": round_secs(result.elapsed_secs),
                "total_wall_secs": round_secs(total_started.elapsed().as_secs_f64()),
                "updates_processed": result.updates_processed,
                "updates_per_sec": round_secs(throughput(result.updates_processed, runtime_wall_secs)),
                "fills_recorded": result.fills_recorded,
                "intents_submitted": result.intents_submitted,
                "net_pnl": result.pnl.net_pnl().to_string(),
                "open_positions": result.risk.open_positions,
                "gross_exposure": result.risk.gross_exposure.to_string(),
            }),
        );
        print_results(&result, &snapshot, stake_usd);
        write_backtest_evaluation_json(
            output_json.as_deref(),
            &strategy_variant,
            config_path.as_deref(),
            source_kind,
            None,
            start_date.as_deref(),
            end_date.as_deref(),
            &backtest_options,
            &result,
            &snapshot,
        );
    }
}

fn write_backtest_evaluation_json(
    path: Option<&str>,
    strategy_variant: &str,
    config_path: Option<&str>,
    source_kind: &str,
    data_dir: Option<&str>,
    start_date: Option<&str>,
    end_date: Option<&str>,
    backtest_options: &HistoricalLoadOptions,
    result: &ploy_strategy_bundles::RuntimeResult,
    snapshot: &ploy_trading::TradingRuntimeSnapshot,
) {
    let Some(path) = path else {
        return;
    };
    let artifact = build_backtest_evaluation_artifact(
        strategy_variant,
        config_path,
        source_kind,
        data_dir,
        start_date,
        end_date,
        backtest_options,
        result,
        snapshot,
    );
    if let Some(parent) = std::path::Path::new(path).parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!(
                "warning: failed to create output json dir {}: {error}",
                parent.display()
            );
            return;
        }
    }
    match serde_json::to_string_pretty(&artifact) {
        Ok(body) => {
            if let Err(error) = fs::write(path, format!("{body}\n")) {
                eprintln!("warning: failed to write output json {path}: {error}");
            }
        }
        Err(error) => eprintln!("warning: failed to encode output json {path}: {error}"),
    }
}

fn build_backtest_evaluation_artifact(
    strategy_variant: &str,
    config_path: Option<&str>,
    source_kind: &str,
    data_dir: Option<&str>,
    start_date: Option<&str>,
    end_date: Option<&str>,
    backtest_options: &HistoricalLoadOptions,
    result: &ploy_strategy_bundles::RuntimeResult,
    snapshot: &ploy_trading::TradingRuntimeSnapshot,
) -> serde_json::Value {
    let cashflow = snapshot.fill_cashflow_summary();
    let evidence_metrics = backtest_evidence_metrics(snapshot);
    let mut artifact_risk_flags = Vec::new();
    if snapshot.orders.is_empty() {
        artifact_risk_flags.push("no_order_level_rows");
    }
    if snapshot.fills.is_empty() {
        artifact_risk_flags.push("no_fill_level_rows");
    }

    let has_official_settlement_gate = backtest_options.require_official_settlement;
    let has_full_depth_clob = result.non_settlement_fills_observed > 0
        && result.full_depth_fills_observed == result.non_settlement_fills_observed;
    let has_event_level_accounting = evidence_metrics.unique_event_count > 0
        && evidence_metrics.missing_event_id_count == 0
        && evidence_metrics.lifecycle_without_entry_decision_count == 0
        && evidence_metrics.max_event_decisions <= 1
        && evidence_metrics.open_event_count == 0
        && evidence_metrics.closed_event_count == evidence_metrics.unique_event_count;
    let has_replay_dryrun_parity = false;
    let has_runtime_scorer_parity = false;
    let is_synthetic = source_kind == "synthetic";

    let mut blocking_risk_flags = Vec::new();
    if !has_full_depth_clob {
        blocking_risk_flags.push("missing_full_depth_clob_fillability");
    }
    if !has_official_settlement_gate {
        blocking_risk_flags.push("missing_official_settlement_gate");
    }
    if evidence_metrics.missing_event_id_count > 0 {
        blocking_risk_flags.push("missing_event_ids");
    }
    if evidence_metrics.lifecycle_without_entry_decision_count > 0 {
        blocking_risk_flags.push("lifecycle_without_entry_decision");
    }
    if evidence_metrics.max_event_decisions > 1 {
        blocking_risk_flags.push("multiple_entry_decisions_per_event");
    }
    if !has_event_level_accounting {
        blocking_risk_flags.push("incomplete_event_lifecycle_accounting");
    }
    if !has_replay_dryrun_parity {
        blocking_risk_flags.push("missing_replay_dryrun_parity");
    }
    if !has_runtime_scorer_parity {
        blocking_risk_flags.push("missing_runtime_scorer_parity");
    }
    if is_synthetic {
        blocking_risk_flags.push("synthetic_data_source");
    }
    blocking_risk_flags.extend(artifact_risk_flags.iter().copied());

    let mut advisory_flags = Vec::new();
    if source_kind == "parquet_stream" && result.depth_quote_updates_observed == 0 {
        advisory_flags.push("parquet_stream_uses_quote_ticks_not_full_clob_lake");
    }
    if source_kind == "db_eager" {
        advisory_flags.push("db_eager_mode_is_debug_or_small_window_only");
    }

    let evidence_stage = if !is_synthetic
        && has_official_settlement_gate
        && has_full_depth_clob
        && has_event_level_accounting
    {
        "executable_replay"
    } else {
        "diagnostic"
    };
    let canonical_result = if !artifact_risk_flags.is_empty() {
        "fix-workflow-or-data-source"
    } else if blocking_risk_flags.is_empty() {
        "continue"
    } else {
        "revise"
    };
    let full_depth_quote_coverage = if result.quote_updates_observed == 0 {
        0.0
    } else {
        result.depth_quote_updates_observed as f64 / result.quote_updates_observed as f64
    };

    json!({
        "schema_version": 1,
        "artifact_type": "strategy_backtest_evaluation",
        "producer": "run_backtest",
        "generated_at": Utc::now().to_rfc3339(),
        "replay_mode": "backtest",
        "evidence_stage": evidence_stage,
        "canonical_result": canonical_result,
        "promotion_ready": false,
        "promotion_decision": "pending replay/dry-run parity review",
        "strategy_variant": strategy_variant,
        "config_path": config_path,
        "source": {
            "kind": source_kind,
            "data_dir": data_dir,
        },
        "data_surfaces": {
            "binance_l2_requested": backtest_options.include_l2,
            "lob_sample_secs": backtest_options.lob_sample_secs,
            "spot_sample_secs": backtest_options.spot_sample_secs,
            "official_settlement_required": has_official_settlement_gate,
            "full_depth_clob_fillability": has_full_depth_clob,
            "event_level_accounting": has_event_level_accounting,
            "runtime_replay_parity": has_replay_dryrun_parity,
            "runtime_scorer_parity": has_runtime_scorer_parity,
        },
        "gate_notes": [
            "Backtest evidence alone is not promotion evidence.",
            "Promotion requires full-depth CLOB fillability, official settlement, replay/dry-run parity, and runtime scorer parity."
        ],
        "window": {
            "start_date": start_date,
            "end_date": end_date,
        },
        "metrics": {
            "updates_processed": result.updates_processed,
            "quote_updates_observed": result.quote_updates_observed,
            "depth_quote_updates_observed": result.depth_quote_updates_observed,
            "full_depth_quote_coverage": full_depth_quote_coverage,
            "intents_submitted": result.intents_submitted,
            "orders": snapshot.orders.len(),
            "fills_recorded": result.fills_recorded,
            "fills": snapshot.fills.len(),
            "non_settlement_fills_observed": result.non_settlement_fills_observed,
            "full_depth_fills_observed": result.full_depth_fills_observed,
            "unique_event_count": evidence_metrics.unique_event_count,
            "max_event_decisions": evidence_metrics.max_event_decisions,
            "closed_event_count": evidence_metrics.closed_event_count,
            "open_event_count": evidence_metrics.open_event_count,
            "missing_event_id_count": evidence_metrics.missing_event_id_count,
            "lifecycle_without_entry_decision_count": evidence_metrics.lifecycle_without_entry_decision_count,
            "max_drawdown": evidence_metrics.max_drawdown,
            "realized_pnl": result.pnl.realized_pnl,
            "unrealized_pnl": result.pnl.unrealized_pnl,
            "total_fees": result.pnl.total_fees,
            "net_pnl": result.pnl.net_pnl(),
            "deployed_capital": cashflow.deployed_capital(),
            "gross_sell_proceeds": cashflow.gross_sell_proceeds,
            "elapsed_secs": result.elapsed_secs,
        },
        "risk_flags": artifact_risk_flags,
        "blocking_risk_flags": blocking_risk_flags,
        "advisory_flags": advisory_flags,
        "runtime_evidence": normalized_runtime_evidence(snapshot),
    })
}

#[derive(Debug, Default)]
struct BacktestEvidenceMetrics {
    unique_event_count: usize,
    max_event_decisions: usize,
    missing_event_id_count: usize,
    lifecycle_without_entry_decision_count: usize,
    closed_event_count: usize,
    open_event_count: usize,
    max_drawdown: Decimal,
}

#[derive(Debug, Default)]
struct EventLifecycle {
    net_quantity_by_token: BTreeMap<String, Decimal>,
    net_pnl: Decimal,
    closed_at: Option<chrono::DateTime<Utc>>,
    has_fill: bool,
}

fn backtest_evidence_metrics(
    snapshot: &ploy_trading::TradingRuntimeSnapshot,
) -> BacktestEvidenceMetrics {
    let mut entry_decisions = BTreeMap::<String, usize>::new();
    let mut missing_event_id_count = 0usize;
    for intent in snapshot
        .intents
        .iter()
        .filter(|intent| intent.purpose == ploy_trading::IntentPurpose::Entry)
    {
        if intent.market_id.trim().is_empty() {
            missing_event_id_count += 1;
        } else {
            *entry_decisions.entry(intent.market_id.clone()).or_default() += 1;
        }
    }

    let intents_by_id = snapshot
        .intents
        .iter()
        .map(|intent| (intent.intent_id.as_str(), intent))
        .collect::<BTreeMap<_, _>>();
    let orders_by_id = snapshot
        .orders
        .iter()
        .map(|order| (order.order_id.as_str(), order))
        .collect::<BTreeMap<_, _>>();
    let mut lifecycles = BTreeMap::<String, EventLifecycle>::new();
    for fill in &snapshot.fills {
        let Some(intent) = orders_by_id
            .get(fill.order_id.as_str())
            .and_then(|order| intents_by_id.get(order.intent_id.as_str()))
            .copied()
        else {
            continue;
        };
        if intent.market_id.trim().is_empty() {
            continue;
        }
        let lifecycle = lifecycles.entry(intent.market_id.clone()).or_default();
        let signed_quantity = match fill.side {
            ploy_trading::TradeSide::Buy => fill.quantity,
            ploy_trading::TradeSide::Sell => -fill.quantity,
        };
        *lifecycle
            .net_quantity_by_token
            .entry(fill.token_id.clone())
            .or_default() += signed_quantity;
        let signed_notional = match fill.side {
            ploy_trading::TradeSide::Buy => -(fill.quantity * fill.price),
            ploy_trading::TradeSide::Sell => fill.quantity * fill.price,
        };
        lifecycle.net_pnl += signed_notional - fill.fee;
        lifecycle.closed_at = Some(
            lifecycle
                .closed_at
                .map_or(fill.timestamp, |current| current.max(fill.timestamp)),
        );
        lifecycle.has_fill = true;
    }

    let mut closed = Vec::new();
    let mut open_event_count = 0usize;
    let lifecycle_without_entry_decision_count = lifecycles
        .keys()
        .filter(|event_id| !entry_decisions.contains_key(event_id.as_str()))
        .count();
    for event_id in entry_decisions.keys() {
        match lifecycles.get(event_id) {
            Some(lifecycle)
                if lifecycle.has_fill
                    && lifecycle
                        .net_quantity_by_token
                        .values()
                        .all(Decimal::is_zero) =>
            {
                closed.push((
                    lifecycle.closed_at.unwrap_or_default(),
                    event_id.as_str(),
                    lifecycle.net_pnl,
                ));
            }
            _ => open_event_count += 1,
        }
    }
    closed.sort_by_key(|(closed_at, event_id, _)| (*closed_at, *event_id));

    let mut cumulative = Decimal::ZERO;
    let mut peak = Decimal::ZERO;
    let mut max_drawdown = Decimal::ZERO;
    for (_, _, pnl) in &closed {
        cumulative += *pnl;
        peak = peak.max(cumulative);
        max_drawdown = max_drawdown.min(cumulative - peak);
    }

    BacktestEvidenceMetrics {
        unique_event_count: entry_decisions.len(),
        max_event_decisions: entry_decisions.values().copied().max().unwrap_or(0),
        missing_event_id_count,
        lifecycle_without_entry_decision_count,
        closed_event_count: closed.len(),
        open_event_count,
        max_drawdown,
    }
}

fn normalized_runtime_evidence(
    snapshot: &ploy_trading::TradingRuntimeSnapshot,
) -> serde_json::Value {
    let intents_by_id: BTreeMap<&str, &ploy_trading::TradingIntent> = snapshot
        .intents
        .iter()
        .map(|intent| (intent.intent_id.as_str(), intent))
        .collect();
    let orders_by_id: BTreeMap<&str, &ploy_trading::OrderRecord> = snapshot
        .orders
        .iter()
        .map(|order| (order.order_id.as_str(), order))
        .collect();

    let mut fill_quantity_by_order: BTreeMap<&str, Decimal> = BTreeMap::new();
    let mut fill_notional_by_order: BTreeMap<&str, Decimal> = BTreeMap::new();
    let mut fill_pnl_by_order: BTreeMap<&str, Decimal> = BTreeMap::new();
    for fill in &snapshot.fills {
        *fill_quantity_by_order
            .entry(fill.order_id.as_str())
            .or_default() += fill.quantity;
        *fill_notional_by_order
            .entry(fill.order_id.as_str())
            .or_default() += fill.quantity * fill.price;
        let signed_notional = match fill.side {
            ploy_trading::TradeSide::Buy => -(fill.quantity * fill.price),
            ploy_trading::TradeSide::Sell => fill.quantity * fill.price,
        };
        *fill_pnl_by_order.entry(fill.order_id.as_str()).or_default() += signed_notional - fill.fee;
    }

    let events: Vec<_> = snapshot
        .orders
        .iter()
        .map(|order| {
            let intent = intents_by_id.get(order.intent_id.as_str()).copied();
            let fill_quantity = fill_quantity_by_order
                .get(order.order_id.as_str())
                .copied()
                .unwrap_or(order.filled_qty);
            let avg_fill_price = if fill_quantity == Decimal::ZERO {
                None
            } else {
                fill_notional_by_order
                    .get(order.order_id.as_str())
                    .map(|notional| *notional / fill_quantity)
            };
            let purpose = intent
                .map(|intent| intent_purpose_label(intent.purpose))
                .unwrap_or("ENTRY");

            json!({
                "deployment_id": empty_to_none(order.deployment_id.as_str())
                    .or_else(|| intent.and_then(|intent| empty_to_none(intent.deployment_id.as_str()))),
                "intent_id": order.intent_id.as_str(),
                "order_id": order.order_id.as_str(),
                "event_id": intent.map(|intent| intent.market_id.as_str()),
                "market_id": intent.map(|intent| intent.market_id.as_str()),
                "token_id": order.token_id.as_str(),
                "decision_ts": intent.map(|intent| intent.created_at),
                "quote": order.limit_price,
                "signal_inputs": {
                    "purpose": purpose,
                    "requested_qty": order.requested_qty,
                    "limit_price": order.limit_price,
                },
                "side": intent
                    .map(|intent| trade_side_label(intent.side))
                    .unwrap_or("UNKNOWN"),
                "entry_price": avg_fill_price.or(order.limit_price),
                "fill_status": order_state_label(order.state),
                "settlement": "open",
                "pnl": fill_pnl_by_order
                    .get(order.order_id.as_str())
                    .copied()
                    .unwrap_or(Decimal::ZERO),
            })
        })
        .collect();

    let orders: Vec<_> = snapshot
        .orders
        .iter()
        .map(|order| {
            let intent = intents_by_id.get(order.intent_id.as_str()).copied();
            let fill_quantity = fill_quantity_by_order
                .get(order.order_id.as_str())
                .copied()
                .unwrap_or(order.filled_qty);
            let avg_fill_price = if fill_quantity == Decimal::ZERO {
                None
            } else {
                fill_notional_by_order
                    .get(order.order_id.as_str())
                    .map(|notional| *notional / fill_quantity)
            };

            json!({
                "deployment_id": empty_to_none(order.deployment_id.as_str())
                    .or_else(|| intent.and_then(|intent| empty_to_none(intent.deployment_id.as_str()))),
                "intent_id": order.intent_id.as_str(),
                "order_id": order.order_id.as_str(),
                "venue_order_id": order.venue_order_id.as_deref(),
                "event_id": intent.map(|intent| intent.market_id.as_str()),
                "market_id": intent.map(|intent| intent.market_id.as_str()),
                "token_id": order.token_id.as_str(),
                "market_side": None::<String>,
                "order_side": intent
                    .map(|intent| trade_side_label(intent.side))
                    .unwrap_or("UNKNOWN"),
                "purpose": intent.map(|intent| intent_purpose_label(intent.purpose)),
                "quantity": order.requested_qty,
                "requested_qty": order.requested_qty,
                "limit_price": order.limit_price,
                "filled_quantity": fill_quantity,
                "avg_fill_price": avg_fill_price,
                "status": order_state_label(order.state),
                "rejection_reason": order.rejection_reason.as_deref(),
                "last_error": order.last_error.as_deref(),
                "created_at": intent.map(|intent| intent.created_at),
            })
        })
        .collect();

    let fills: Vec<_> = snapshot
        .fills
        .iter()
        .map(|fill| {
            let order = orders_by_id.get(fill.order_id.as_str()).copied();
            let intent =
                order.and_then(|order| intents_by_id.get(order.intent_id.as_str()).copied());
            json!({
                "deployment_id": order
                    .and_then(|order| empty_to_none(order.deployment_id.as_str()))
                    .or_else(|| intent.and_then(|intent| empty_to_none(intent.deployment_id.as_str()))),
                "intent_id": order.map(|order| order.intent_id.as_str()),
                "order_id": fill.order_id.as_str(),
                "fill_id": fill.fill_id.as_str(),
                "event_id": intent.map(|intent| intent.market_id.as_str()),
                "market_id": intent.map(|intent| intent.market_id.as_str()),
                "token_id": fill.token_id.as_str(),
                "market_side": None::<String>,
                "fill_side": trade_side_label(fill.side),
                "purpose": intent.map(|intent| intent_purpose_label(intent.purpose)),
                "quantity": fill.quantity,
                "price": fill.price,
                "fee": fill.fee,
                "fill_timestamp": fill.timestamp,
            })
        })
        .collect();

    json!({
        "schema_version": 1,
        "basis": "trading_runtime_snapshot",
        "comparison_contract": "Compare these normalized rows against strategy_runtime_orders and strategy_runtime_fills exported from Tango for the same deployment/config/feed window.",
        "events": events,
        "orders": orders,
        "fills": fills,
    })
}

fn empty_to_none(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn trade_side_label(side: ploy_trading::TradeSide) -> &'static str {
    match side {
        ploy_trading::TradeSide::Buy => "BUY",
        ploy_trading::TradeSide::Sell => "SELL",
    }
}

fn intent_purpose_label(purpose: ploy_trading::IntentPurpose) -> &'static str {
    match purpose {
        ploy_trading::IntentPurpose::Entry => "ENTRY",
        ploy_trading::IntentPurpose::Exit => "EXIT",
        ploy_trading::IntentPurpose::Reduce => "REDUCE",
        ploy_trading::IntentPurpose::Hedge => "HEDGE",
        ploy_trading::IntentPurpose::Cancel => "CANCEL",
    }
}

fn order_state_label(state: ploy_trading::OrderState) -> &'static str {
    match state {
        ploy_trading::OrderState::Pending => "PENDING",
        ploy_trading::OrderState::Unknown => "UNKNOWN",
        ploy_trading::OrderState::Acknowledged => "ACKNOWLEDGED",
        ploy_trading::OrderState::PartiallyFilled => "PARTIALLY_FILLED",
        ploy_trading::OrderState::Filled => "FILLED",
        ploy_trading::OrderState::Canceled => "CANCELED",
        ploy_trading::OrderState::Rejected => "REJECTED",
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn print_data_breakdown(updates: &[MarketUpdate]) {
    let mut spot_count = 0u64;
    let mut quote_count = 0u64;
    let mut event_discovered = 0u64;
    let mut event_expired = 0u64;
    let mut l2_count = 0u64;
    let mut kline_count = 0u64;
    for u in updates {
        match u {
            MarketUpdate::SpotPrice { .. } => spot_count += 1,
            MarketUpdate::AggTrade { .. } | MarketUpdate::QuoteCollectionFailure { .. } => {}
            MarketUpdate::Quote { .. } => quote_count += 1,
            MarketUpdate::EventDiscovered { .. } => event_discovered += 1,
            MarketUpdate::EventExpired { .. } => event_expired += 1,
            MarketUpdate::L2 { .. } | MarketUpdate::L2Depth { .. } => l2_count += 1,
            MarketUpdate::SportsState { .. }
            | MarketUpdate::SportsPregame { .. }
            | MarketUpdate::SportsLive { .. }
            | MarketUpdate::ReferencePrice { .. } => {}
            MarketUpdate::Kline { .. } => kline_count += 1,
        }
    }
    eprintln!(
        "Data breakdown: spot={spot_count} quote={quote_count} discovered={event_discovered} expired={event_expired} l2={l2_count} kline={kline_count}"
    );
}

fn round_secs(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn throughput(updates: u64, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        0.0
    } else {
        updates as f64 / seconds
    }
}

fn write_timing_json(path: Option<&str>, payload: serde_json::Value) {
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = std::path::Path::new(path).parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!(
                "warning: failed to create timing dir {}: {error}",
                parent.display()
            );
            return;
        }
    }
    match serde_json::to_string_pretty(&payload) {
        Ok(body) => {
            if let Err(error) = fs::write(path, format!("{body}\n")) {
                eprintln!("warning: failed to write timing json {path}: {error}");
            }
        }
        Err(error) => eprintln!("warning: failed to encode timing json {path}: {error}"),
    }
}

fn print_results(
    result: &ploy_strategy_bundles::RuntimeResult,
    snapshot: &ploy_trading::TradingRuntimeSnapshot,
    stake_usd: Decimal,
) {
    let cashflow = snapshot.fill_cashflow_summary();

    eprintln!("=== Results ===");
    eprintln!("Updates processed: {}", result.updates_processed);
    let trade_count = result.fills_recorded / 2;
    eprintln!(
        "Trades:            {} ({} fills)",
        trade_count, result.fills_recorded
    );
    eprintln!("Elapsed:           {:.2}s", result.elapsed_secs);
    eprintln!();

    let fills = &snapshot.fills;
    let mut open_positions: i64 = 0;
    let mut peak_positions: i64 = 0;
    for fill in fills {
        match fill.side {
            ploy_trading::TradeSide::Buy => {
                open_positions += 1;
                if open_positions > peak_positions {
                    peak_positions = open_positions;
                }
            }
            ploy_trading::TradeSide::Sell => {
                open_positions = (open_positions - 1).max(0);
            }
        }
    }
    let peak_capital = rust_decimal::Decimal::from(peak_positions) * stake_usd;

    if !snapshot.fills.is_empty() && std::env::args().any(|a| a == "--show-fills") {
        eprintln!("=== Fills ===");
        for fill in fills {
            eprintln!(
                "  {} {:?} {}x @ {} (fee: {})",
                &fill.token_id[..12],
                fill.side,
                fill.quantity,
                fill.price,
                fill.fee
            );
        }
        eprintln!();
    }

    if !snapshot.positions.is_empty() {
        eprintln!("=== Open Positions ===");
        for pos in &snapshot.positions {
            eprintln!(
                "  {} qty={} avg_entry={} realized_pnl={}",
                &pos.token_id[..12],
                pos.net_qty,
                pos.avg_entry_price,
                pos.realized_pnl
            );
        }
        eprintln!();
    }

    eprintln!("=== P&L ===");
    eprintln!("Realized:          {}", result.pnl.realized_pnl);
    eprintln!("Fees:              {}", result.pnl.total_fees);
    eprintln!("Net:               {}", result.pnl.net_pnl());
    eprintln!();
    eprintln!("=== Capital Usage ===");
    eprintln!("Stake per trade:   ${}", stake_usd);
    eprintln!("Total trades:      {}", trade_count);
    eprintln!(
        "Cumulative cost:   {} (all trades summed)",
        cashflow.deployed_capital()
    );
    eprintln!(
        "Peak concurrent:   {} (max simultaneous open)",
        peak_capital.round_dp(2)
    );
    eprintln!("Sell proceeds:     {}", cashflow.gross_sell_proceeds);
    if let Some(roi) = cashflow.roi_on_deployed_capital() {
        eprintln!(
            "ROI (cumulative):  {}%  ← total profit / total cost (misleading for multi-trade)",
            (roi * Decimal::from(100)).round_dp(2)
        );
    }
    if peak_capital > Decimal::ZERO {
        let roi_peak = result.pnl.net_pnl() / peak_capital * Decimal::from(100);
        eprintln!(
            "ROI (peak capital): {}%  ← net profit / max simultaneous capital",
            roi_peak.round_dp(2)
        );
    }
    eprintln!("Note: quantity is shares/contracts; capital = shares × price");
    eprintln!();
    eprintln!("=== Risk ===");
    eprintln!("Open positions:  {}", result.risk.open_positions);
    eprintln!("Active orders:   {}", result.risk.active_orders);
    eprintln!("Gross exposure:  {}", result.risk.gross_exposure);
}
