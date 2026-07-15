use ploy_feed_loaders::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::{
    FullConfig, HistoricalFeed, NullRecorder, Recorder, SimulatedExecutor, StrategyLogic,
    StrategyRuntime,
};
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;
use std::env;
use tracing::info;

use crate::RuntimeModeConfig;

pub(crate) async fn run_backtest_entry(
    config: &FullConfig,
    symbols: &[String],
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    run_backtest(config, symbols, strategy, runtime_config).await
}

async fn run_backtest(
    config: &FullConfig,
    symbols: &[String],
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    let db_url = match env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => {
            eprintln!(
                "DATABASE_URL is required for backtest mode; refusing to fall back to a local database"
            );
            std::process::exit(1);
        }
    };

    let pool = match PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("Failed to connect to configured DATABASE_URL for backtest: {error}");
            std::process::exit(1);
        }
    };

    let (from, to) = config.backtest_time_range().unwrap_or_else(|| {
        let from = chrono::DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let to = chrono::DateTime::parse_from_rfc3339("2026-04-01T13:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        (from, to)
    });

    info!(
        from = %from,
        to = %to,
        symbols = ?symbols,
        "Loading historical data from database",
    );

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

    let updates =
        match load_from_database_with_options(&pool, symbols, from, to, &backtest_options).await {
            Ok(updates) => updates,
            Err(error) => {
                eprintln!("Failed to load historical data for backtest: {error}");
                std::process::exit(1);
            }
        };

    info!(updates = updates.len(), "Historical data loaded");

    let feed = HistoricalFeed::new(updates);
    let executor = SimulatedExecutor::new(config.sim_executor_config());
    let recorder: Box<dyn Recorder> = Box::new(NullRecorder);
    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;
    let snapshot = runtime.trading().snapshot(&BTreeMap::new());
    (result, snapshot)
}
