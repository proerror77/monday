use ploy_strategy_bundles::{
    FullConfig, NullRecorder, RecordedFeed, Recorder, SimulatedExecutor, StrategyLogic,
    StrategyRuntime,
};
use std::collections::BTreeMap;
use tracing::info;

use crate::RuntimeModeConfig;

pub(crate) async fn run_replay_entry(
    config: &FullConfig,
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    run_replay(config, strategy, runtime_config).await
}

async fn run_replay(
    config: &FullConfig,
    strategy: Box<dyn StrategyLogic>,
    runtime_config: RuntimeModeConfig,
) -> (
    ploy_strategy_bundles::RuntimeResult,
    ploy_trading::TradingRuntimeSnapshot,
) {
    let replay_path = config.replay_market_updates_path().unwrap_or_else(|| {
        eprintln!("Replay mode requires [runtime].replay_market_updates_from in the config");
        std::process::exit(1);
    });

    info!(
        path = %replay_path.display(),
        "Loading recorded market-update log for replay",
    );

    let feed = RecordedFeed::from_path(replay_path).unwrap_or_else(|error| {
        eprintln!(
            "Failed to load replay market updates from {}: {error}",
            replay_path.display()
        );
        std::process::exit(1);
    });
    let executor = SimulatedExecutor::new(config.sim_executor_config());
    let recorder: Box<dyn Recorder> = Box::new(NullRecorder);
    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);
    let result = runtime.run().await;
    let snapshot = runtime.trading().snapshot(&BTreeMap::new());
    (result, snapshot)
}
