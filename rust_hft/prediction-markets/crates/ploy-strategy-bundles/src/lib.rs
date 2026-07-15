pub mod bundle;
pub mod config;
pub mod engine;
pub mod executor;
pub mod feed;
pub mod recorder;
pub mod runtime;
pub mod signals;
pub mod strategies;
pub mod traits;

pub use bundle::StrategyBundle;
pub use config::FullConfig;
pub use engine::{RuntimeConfig, RuntimeMode, RuntimeResult, StrategyRuntime};
pub use executor::{CallbackExecutor, SimulatedExecutor, SimulatedExecutorConfig};
#[cfg(feature = "parquet-feed")]
pub use feed::StreamingParquetFeed;
pub use feed::{
    HistoricalFeed, LiveFeed, RecordedFeed, RecordedFeedError, RecordedMarketUpdate, RecordingFeed,
    RecordingLimits,
};
pub use ploy_market_contracts::{Feed, InstrumentKind, MarketUpdate, PredictionFamily, VenueKind};
pub use recorder::BufferedRecorder;
pub use runtime::emit_intents;
pub use signals::{MarketSignal, SignalConfig};
pub use strategies::registry::{build_strategy, canonical_strategy_variant};
pub use strategies::BayesianDirectionalStrategy;
pub use strategies::DiffEnhancedStrategy;
pub use strategies::DiffRegularStrategy;
pub use strategies::DirectionalStrategy;
pub use strategies::MeanReversionStrategy;
pub use strategies::ProbChaseStrategy;
pub use strategies::ProbReversalStrategy;
pub use strategies::ReversalStrategy;
pub use strategies::SweepStrategy;
pub use strategies::ThreeLayerProfile;
pub use strategies::ThreeLayerStrategy;
pub use traits::{
    ExecutionPolicy, ExecutionReport, Executor, NullRecorder, Recorder, SignalRecord,
    StrategyDecision, StrategyLogic, SubmitOutcome,
};

pub const CRATE_MARKER: &str = "ploy-strategy-bundles";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
