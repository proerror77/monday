use ploy_market_data::binance_collectors::spawn_binance_tick_feed;
use ploy_market_data::feeds::{
    spawn_chainlink_feed, spawn_db_aggtrade_feed, spawn_db_l2_feed, spawn_db_polymarket_feed,
    spawn_db_spot_feed, spawn_pyth_reference_feed,
};
use ploy_market_data::reference_prices::new_reference_price_registry;
use ploy_market_data::scanner::spawn_market_scanner;
use ploy_market_data::sports_feed::spawn_sports_feed;
use ploy_strategy_bundles::config::MarketDataSource;
use ploy_strategy_bundles::{
    Feed, FullConfig, LiveFeed, RecordingFeed, RuntimeMode, StrategyLogic,
};
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::recording::build_signal_recorder;
use crate::{database_unavailable_is_fatal, RuntimeModeConfig};

fn uses_db_primary_ticks(source: MarketDataSource) -> bool {
    source.uses_local_db() && !source.uses_external_direct()
}

pub(crate) async fn run_live_or_dry_run_entry(
    config: &FullConfig,
    symbols: &[String],
    strategy: Option<Box<dyn StrategyLogic>>,
    runtime_config: RuntimeModeConfig,
    deployment_id: String,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    portfolio_core::prediction::TradingRuntimeSnapshot,
) {
    run_live_or_dry_run(config, symbols, strategy, runtime_config, deployment_id).await
}

async fn run_live_or_dry_run(
    config: &FullConfig,
    symbols: &[String],
    strategy: Option<Box<dyn StrategyLogic>>,
    runtime_config: RuntimeModeConfig,
    deployment_id: String,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    portfolio_core::prediction::TradingRuntimeSnapshot,
) {
    let db_url = env::var("DATABASE_URL").ok();
    let db_pool: Option<sqlx::PgPool> = match db_url.as_deref() {
        Some(url) => match PgPoolOptions::new().max_connections(5).connect(url).await {
            Ok(pool) => {
                info!("DB connected — market metadata and quotes will be persisted");
                Some(pool)
            }
            Err(error) => {
                if database_unavailable_is_fatal(runtime_config.mode, true) {
                    error!(
                        error = %error,
                        "DB connection failed for configured runtime; refusing to start without persistence"
                    );
                    std::process::exit(1);
                }

                warn!(error = %error, "DB connection failed; running without persistence");
                None
            }
        },
        None => {
            if database_unavailable_is_fatal(runtime_config.mode, false) {
                error!(
                    "DATABASE_URL not set for live runtime; refusing to start without persistence"
                );
                std::process::exit(1);
            }
            info!("DATABASE_URL not set — running without DB persistence");
            None
        }
    };

    let feed_capacity = config
        .validated_feed_broadcast_capacity()
        .unwrap_or_else(|message| {
            eprintln!("{message}");
            std::process::exit(1);
        });
    if !config.feed_lag_policy_allowed(runtime_config.mode) {
        eprintln!(
            "feed_lag_policy = \"skip_and_continue\" is only allowed for a pure noop dry-run recorder"
        );
        std::process::exit(1);
    }
    let (tx, rx) = broadcast::channel(feed_capacity);
    let tx = Arc::new(tx);
    let reference_prices = new_reference_price_registry();
    let market_data_source = config.runtime.market_data_source;

    if market_data_source.uses_local_db() && db_pool.is_none() {
        error!(
            source = ?market_data_source,
            "Local market-data source requires DATABASE_URL; refusing to open direct public feeds"
        );
        std::process::exit(1);
    }

    let mut feed_handles = Vec::new();
    if market_data_source.uses_local_db() {
        if let Some(ref db) = db_pool {
            if uses_db_primary_ticks(market_data_source) {
                feed_handles.push(spawn_db_spot_feed(tx.clone(), symbols.to_vec(), db.clone()));
                feed_handles.push(spawn_db_polymarket_feed(
                    tx.clone(),
                    symbols.to_vec(),
                    db.clone(),
                ));
                feed_handles.push(spawn_db_aggtrade_feed(
                    tx.clone(),
                    symbols.to_vec(),
                    db.clone(),
                ));
                feed_handles.push(spawn_db_l2_feed(tx.clone(), symbols.to_vec(), db.clone()));
            }
        }
    }

    if market_data_source.uses_external_direct() {
        feed_handles.push(spawn_binance_tick_feed(
            tx.clone(),
            reference_prices.clone(),
            symbols.to_vec(),
            20,
        ));
        feed_handles.push(spawn_chainlink_feed(
            tx.clone(),
            reference_prices.clone(),
            symbols.to_vec(),
            db_pool.clone(),
        ));
        feed_handles.push(spawn_pyth_reference_feed(
            tx.clone(),
            reference_prices.clone(),
            config.reference_data.pyth_symbols.clone(),
            db_pool.clone(),
        ));
        feed_handles.push(spawn_market_scanner(
            tx.clone(),
            reference_prices.clone(),
            symbols.to_vec(),
            db_pool.clone(),
            config.reference_data.capture_sports_state,
        ));

        if config.reference_data.capture_sports_state {
            feed_handles.push(spawn_sports_feed(tx.clone(), db_pool.clone()));
        }
    } else if config.reference_data.capture_sports_state {
        warn!(
            "capture_sports_state requires market_data_source = external_direct or dual; local_db runtime will not open the sports WebSocket"
        );
    }

    let feed: Box<dyn Feed> = if let Some(record_path) = config.record_market_updates_path() {
        Box::new(
            RecordingFeed::with_policy(
                LiveFeed::with_lag_policy(rx, config.feed_lag_policy()),
                record_path,
                config.record_market_updates_policy(),
            )
            .unwrap_or_else(|error| {
                eprintln!(
                    "Failed to open market-update log {}: {error}",
                    record_path.display()
                );
                std::process::exit(1);
            }),
        )
    } else {
        Box::new(LiveFeed::with_lag_policy(rx, config.feed_lag_policy()))
    };

    let result = if config.is_market_update_recorder_profile(runtime_config.mode) {
        assert!(
            strategy.is_none(),
            "pure recorder must not construct a strategy"
        );
        drain_market_update_recorder(feed, &runtime_config).await
    } else {
        let strategy = strategy.expect("trading runtime requires a strategy");
        let recorder = build_signal_recorder(db_pool.clone(), runtime_config.mode);
        if runtime_config.mode == RuntimeMode::Live {
            eprintln!(
                "Live execution is disabled; Monday runtime is the only production execution authority"
            );
            std::process::exit(1);
        } else {
            let executor =
                ploy_strategy_bundles::SimulatedExecutor::new(config.sim_executor_config());
            let mut runtime = ploy_strategy_bundles::StrategyRuntime::new(
                strategy,
                feed,
                executor,
                recorder,
                runtime_config,
            )
            .with_deployment_id(deployment_id);
            let result = runtime.run().await;
            let snapshot = runtime
                .trading()
                .snapshot(&std::collections::BTreeMap::new());
            (result, snapshot)
        }
    };

    for handle in feed_handles {
        handle.abort();
    }

    result
}

async fn drain_market_update_recorder(
    mut feed: Box<dyn Feed>,
    runtime_config: &RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    portfolio_core::prediction::TradingRuntimeSnapshot,
) {
    let start = std::time::Instant::now();
    let mut updates_processed = 0_u64;
    let mut quote_updates_observed = 0_u64;
    let mut depth_quote_updates_observed = 0_u64;

    while let Some(update) = feed.next().await {
        updates_processed += 1;
        if let ploy_strategy_bundles::MarketUpdate::Quote {
            bid_levels,
            ask_levels,
            ..
        } = update
        {
            quote_updates_observed += 1;
            if !bid_levels.is_empty() || !ask_levels.is_empty() {
                depth_quote_updates_observed += 1;
            }
        }
        if runtime_config
            .max_updates
            .is_some_and(|max| updates_processed >= max)
        {
            break;
        }
    }

    let result = ploy_strategy_bundles::RuntimeResult {
        mode: runtime_config.mode,
        updates_processed,
        quote_updates_observed,
        depth_quote_updates_observed,
        intents_submitted: 0,
        fills_recorded: 0,
        non_settlement_fills_observed: 0,
        full_depth_fills_observed: 0,
        pnl: portfolio_core::prediction::PnlSnapshot::default(),
        risk: portfolio_core::prediction::RiskSnapshot::default(),
        elapsed_secs: start.elapsed().as_secs_f64(),
        strategy_diagnostics: Vec::new(),
    };
    info!(
        updates = result.updates_processed,
        quotes = result.quote_updates_observed,
        depth_quotes = result.depth_quote_updates_observed,
        "Pure market recorder stopped",
    );
    (
        result,
        portfolio_core::prediction::TradingRuntimeSnapshot::default(),
    )
}

#[cfg(test)]
mod feed_source_tests {
    use super::{uses_db_primary_ticks, MarketDataSource};

    #[test]
    fn dual_market_data_keeps_primary_ticks_direct() {
        assert!(uses_db_primary_ticks(MarketDataSource::LocalDb));
        assert!(!uses_db_primary_ticks(MarketDataSource::Dual));
        assert!(!uses_db_primary_ticks(MarketDataSource::ExternalDirect));
    }
}

#[cfg(test)]
mod pure_recorder_tests {
    use super::{drain_market_update_recorder, RuntimeModeConfig};
    use chrono::Utc;
    use ploy_strategy_bundles::feed::{RecordingKind, RecordingPolicy};
    use ploy_strategy_bundles::{HistoricalFeed, MarketUpdate, RecordingFeed, RuntimeMode};
    use rust_decimal::Decimal;
    use std::fs;
    use std::sync::Arc;

    #[tokio::test]
    async fn pure_recorder_preserves_full_market_updates_without_trading_state() {
        let now = Utc::now();
        let updates = vec![
            MarketUpdate::Quote {
                token_id: Arc::from("up-1"),
                bid: Some(Decimal::new(49, 2)),
                ask: Some(Decimal::new(51, 2)),
                bid_size: Some(Decimal::new(12, 0)),
                ask_size: Some(Decimal::new(11, 0)),
                bid_levels: vec![
                    serde_json::from_value(serde_json::json!({"price":"0.49","size":"12"}))
                        .expect("bid level"),
                    serde_json::from_value(serde_json::json!({"price":"0.48","size":"13"}))
                        .expect("second bid level"),
                ],
                ask_levels: vec![
                    serde_json::from_value(serde_json::json!({"price":"0.51","size":"11"}))
                        .expect("ask level"),
                    serde_json::from_value(serde_json::json!({"price":"0.52","size":"14"}))
                        .expect("second ask level"),
                ],
                ts: now,
            },
            MarketUpdate::AggTrade {
                symbol: Arc::from("BTCUSDT"),
                agg_trade_id: 42,
                price: Decimal::new(100_000, 0),
                quantity: Decimal::new(25, 1),
                is_buyer_maker: false,
                ts: now,
            },
            MarketUpdate::L2 {
                symbol: Arc::from("BTCUSDT"),
                obi: 0.25,
                spread_bps: 2,
                ts: now,
            },
        ];
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .expect("current dir")
                    .join("target/test-artifacts")
            });
        fs::create_dir_all(&root).expect("create test artifact dir");
        let path = root.join(format!("polymarket-pure-recorder-{unique}.ndjson"));
        let feed = RecordingFeed::with_policy(
            HistoricalFeed::new(updates.clone()),
            &path,
            RecordingPolicy {
                include_kinds: vec![
                    RecordingKind::Quote,
                    RecordingKind::AggTrade,
                    RecordingKind::L2,
                ],
                quote_sample_ms: Some(0),
                ..RecordingPolicy::default()
            },
        )
        .expect("recording feed");
        let runtime_config = RuntimeModeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let (result, snapshot) =
            drain_market_update_recorder(Box::new(feed), &runtime_config).await;

        let recorded_bytes = fs::read(&path).expect("read recorded tape");
        let recorded = std::str::from_utf8(&recorded_bytes).expect("UTF-8 market tape");
        let records = recorded
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("recorded update"))
            .collect::<Vec<_>>();
        assert_eq!(
            records
                .iter()
                .map(|record| record["sequence"].as_u64().expect("sequence"))
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        for (record, expected) in records.into_iter().zip(&updates) {
            let mut actual = record["update"].clone();
            if let Some(object) = actual.as_object_mut() {
                object.remove("request_status");
                object.remove("collection_result");
            }
            assert_eq!(
                actual,
                serde_json::to_value(expected).expect("expected update")
            );
        }
        assert_eq!(result.updates_processed, 3);
        assert_eq!(result.quote_updates_observed, 1);
        assert_eq!(result.depth_quote_updates_observed, 1);
        assert_eq!(result.intents_submitted, 0);
        assert_eq!(result.fills_recorded, 0);
        assert!(snapshot.intents.is_empty());
        assert!(snapshot.orders.is_empty());
        assert!(snapshot.fills.is_empty());

        let _ = fs::remove_file(path);
    }
}
